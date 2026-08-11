use crate::persistence;
use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
use trailgen_core::{
    Access, Coord, Edge, HikingModel, LineString, LoopConstraints, Route, RouteMetrics, RouteShape,
    RoutingLaw, SupportPoint, Terrain, Trail, TrailMarking, TrailRealization, TrailStanding,
    WalkGraph, WayKind,
};

const SCHEMA: u32 = 7;
const INDEX: &str = "library/index.json";

pub fn validate_trail_name(name: &str) -> Result<&str> {
    let name = name.trim();
    ensure!(!name.is_empty(), "trail name must not be empty");
    ensure!(
        name.chars().count() <= 80,
        "trail name must not exceed 80 characters"
    );
    ensure!(
        name.chars().all(|character| !character.is_control()),
        "trail name must not contain control characters"
    );
    Ok(name)
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TrailId(String);

impl TrailId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Trailhead(Coord);

impl Trailhead {
    pub fn forge(coord: Coord) -> Option<Self> {
        (coord.lon.is_finite()
            && coord.lat.is_finite()
            && (-180.0..=180.0).contains(&coord.lon)
            && (-85.0..=85.0).contains(&coord.lat))
        .then_some(Self(coord))
    }

    pub const fn coord(self) -> Coord {
        self.0
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SearchBoundary(Vec<Coord>);

impl SearchBoundary {
    pub fn forge(mut points: Vec<Coord>) -> Result<Self> {
        ensure!(
            points.iter().all(|point| {
                point.lon.is_finite()
                    && point.lat.is_finite()
                    && (-180.0..=180.0).contains(&point.lon)
                    && (-85.0..=85.0).contains(&point.lat)
            }),
            "search boundary contains an invalid coordinate"
        );
        points.dedup_by(|left, right| same_coord(*left, *right));
        if points.len() >= 2
            && points
                .first()
                .zip(points.last())
                .is_some_and(|(first, last)| same_coord(*first, *last))
        {
            let _closing_duplicate = points.pop();
        }
        let boundary = Self(points);
        boundary.validate()?;
        Ok(boundary)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.0.len() >= 3,
            "search boundary needs at least three points"
        );
        ensure!(
            self.0.iter().all(|point| {
                point.lon.is_finite()
                    && point.lat.is_finite()
                    && (-180.0..=180.0).contains(&point.lon)
                    && (-85.0..=85.0).contains(&point.lat)
            }),
            "search boundary contains an invalid coordinate"
        );
        ensure!(
            polygon_area2(&self.0).abs() > 1.0e-12,
            "search boundary has no area"
        );
        Ok(())
    }

    pub fn points(&self) -> &[Coord] {
        &self.0
    }

    pub fn contains(&self, point: Coord) -> bool {
        ring_segments(&self.0).any(|(a, b)| point_on_segment(point, a, b))
            || point_in_ring(point, &self.0)
    }

    pub(crate) fn edge_mask(
        &self,
        graph: &WalkGraph,
        mut pulse: impl FnMut(usize, usize) -> bool,
    ) -> Option<Vec<bool>> {
        const PULSE_STRIDE: usize = 128;
        let total = graph.edges.len();
        let mut allowed = Vec::with_capacity(total);
        for (index, edge) in graph.edges.iter().enumerate() {
            if index.is_multiple_of(PULSE_STRIDE) && !pulse(index, total) {
                return None;
            }
            allowed.push(self.allows_edge(edge));
        }
        pulse(total, total).then_some(allowed)
    }

    pub(crate) fn allows_edge(&self, edge: &Edge) -> bool {
        edge.geometry
            .points
            .iter()
            .all(|point| self.contains(*point))
            && edge
                .geometry
                .points
                .windows(2)
                .all(|segment| self.contains_segment(segment[0], segment[1]))
    }

    fn contains_segment(&self, a: Coord, b: Coord) -> bool {
        let mut cuts = vec![0.0, 1.0];
        cuts.extend(
            ring_segments(&self.0).filter_map(|(c, d)| segment_intersection_parameter(a, b, c, d)),
        );
        cuts.sort_by(f64::total_cmp);
        cuts.dedup_by(|left, right| (*left - *right).abs() <= 1.0e-10);
        cuts.windows(2)
            .map(|span| a.lerp(b, (span[0] + span[1]) * 0.5))
            .all(|point| self.contains(point))
    }
}

const fn same_coord(left: Coord, right: Coord) -> bool {
    left.lon.to_bits() == right.lon.to_bits() && left.lat.to_bits() == right.lat.to_bits()
}

fn polygon_area2(ring: &[Coord]) -> f64 {
    ring_segments(ring)
        .map(|(a, b)| a.lon.mul_add(b.lat, -(b.lon * a.lat)))
        .sum()
}

fn point_in_ring(point: Coord, ring: &[Coord]) -> bool {
    ring_segments(ring).fold(false, |inside, (a, b)| {
        let crosses = (a.lat > point.lat) != (b.lat > point.lat);
        if !crosses {
            return inside;
        }
        let longitude = (b.lon - a.lon).mul_add((point.lat - a.lat) / (b.lat - a.lat), a.lon);
        inside ^ (point.lon < longitude)
    })
}

fn point_on_segment(point: Coord, a: Coord, b: Coord) -> bool {
    const EPSILON: f64 = 1.0e-10;
    let cross =
        (b.lon - a.lon).mul_add(point.lat - a.lat, -((b.lat - a.lat) * (point.lon - a.lon)));
    cross.abs() <= EPSILON
        && (a.lon.min(b.lon) - EPSILON..=a.lon.max(b.lon) + EPSILON).contains(&point.lon)
        && (a.lat.min(b.lat) - EPSILON..=a.lat.max(b.lat) + EPSILON).contains(&point.lat)
}

fn ring_segments(ring: &[Coord]) -> impl Iterator<Item = (Coord, Coord)> + '_ {
    ring.windows(2)
        .map(|segment| (segment[0], segment[1]))
        .chain((ring.len() >= 2).then(|| (ring[ring.len() - 1], ring[0])))
}

fn segment_intersection_parameter(
    start: Coord,
    end: Coord,
    boundary_start: Coord,
    boundary_end: Coord,
) -> Option<f64> {
    const EPSILON: f64 = 1.0e-12;
    let course = [end.lon - start.lon, end.lat - start.lat];
    let boundary_course = [
        boundary_end.lon - boundary_start.lon,
        boundary_end.lat - boundary_start.lat,
    ];
    let cross = |left: [f64; 2], right: [f64; 2]| left[1].mul_add(-right[0], left[0] * right[1]);
    let denominator = cross(course, boundary_course);
    if denominator.abs() <= EPSILON {
        return None;
    }
    let offset = [
        boundary_start.lon - start.lon,
        boundary_start.lat - start.lat,
    ];
    let progress = cross(offset, boundary_course) / denominator;
    let boundary_progress = cross(offset, course) / denominator;
    ((-EPSILON..=1.0 + EPSILON).contains(&progress)
        && (-EPSILON..=1.0 + EPSILON).contains(&boundary_progress))
    .then(|| progress.clamp(0.0, 1.0))
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MeasureRange {
    pub min: f64,
    pub max: f64,
}

impl MeasureRange {
    fn validate(self, noun: &str) -> Result<()> {
        ensure!(
            self.min.is_finite() && self.max.is_finite(),
            "{noun} range must be finite"
        );
        ensure!(self.min >= 0.0, "minimum {noun} must be nonnegative");
        ensure!(self.min <= self.max, "minimum {noun} exceeds maximum");
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRecipe {
    pub trailhead: Option<Trailhead>,
    #[serde(default)]
    pub boundary: Option<SearchBoundary>,
    pub distance_m: MeasureRange,
    pub climb_m: MeasureRange,
    #[serde(default = "default_moving_time_range")]
    pub moving_time_s: MeasureRange,
    #[serde(default = "default_lower_limb_load_target", alias = "difficulty")]
    pub lower_limb_load_km: f64,
    pub shape: RouteShape,
}

const fn default_lower_limb_load_target() -> f64 {
    30.0
}

const fn default_moving_time_range() -> MeasureRange {
    MeasureRange {
        min: 0.0,
        max: 48.0 * 3_600.0,
    }
}

impl SearchRecipe {
    pub fn from_defaults(defaults: &LoopConstraints) -> Self {
        Self {
            trailhead: None,
            boundary: None,
            distance_m: MeasureRange {
                min: defaults.min_distance_m,
                max: defaults.max_distance_m,
            },
            climb_m: MeasureRange {
                min: defaults.min_ascent_m,
                max: defaults.max_ascent_m,
            },
            moving_time_s: MeasureRange {
                min: defaults.min_moving_time_s,
                max: if defaults.max_moving_time_s > 1.0e300 {
                    default_moving_time_range().max
                } else {
                    defaults.max_moving_time_s
                },
            },
            lower_limb_load_km: defaults.target_lower_limb_load_km.unwrap_or_else(|| {
                default_lower_limb_load_target()
                    .max(defaults.min_lower_limb_load_km)
                    .min(defaults.max_lower_limb_load_km)
            }),
            shape: defaults
                .allowed_shapes
                .first()
                .copied()
                .unwrap_or(RouteShape::Loop),
        }
    }

    pub fn validate(&self) -> Result<()> {
        self.distance_m.validate("distance")?;
        self.climb_m.validate("climb")?;
        self.moving_time_s.validate("moving time")?;
        ensure!(
            self.lower_limb_load_km.is_finite() && self.lower_limb_load_km >= 0.0,
            "lower-limb load target must be finite and nonnegative"
        );
        ensure!(
            self.trailhead.is_none_or(|trailhead| {
                let coord = trailhead.coord();
                coord.lon.is_finite()
                    && coord.lat.is_finite()
                    && (-180.0..=180.0).contains(&coord.lon)
                    && (-85.0..=85.0).contains(&coord.lat)
            }),
            "trailhead coordinate is invalid"
        );
        if let Some(boundary) = &self.boundary {
            boundary.validate()?;
        }
        Ok(())
    }

    pub fn constraints(&self, defaults: &LoopConstraints) -> Result<LoopConstraints> {
        self.validate()?;
        let mut constraints = defaults.clone();
        constraints.min_distance_m = self.distance_m.min;
        constraints.max_distance_m = self.distance_m.max;
        constraints.min_ascent_m = self.climb_m.min;
        constraints.max_ascent_m = self.climb_m.max;
        constraints.min_moving_time_s = self.moving_time_s.min;
        constraints.max_moving_time_s = self.moving_time_s.max;
        constraints.allowed_shapes = vec![self.shape];
        constraints.target_lower_limb_load_km = Some(self.lower_limb_load_km);
        // Roads are undesirable rather than intrinsically unlawful. Their
        // finite routing/quality penalty may yield when they are the only way
        // to hit an easier target; closed and private edges remain excluded.
        constraints.max_road_fraction = 1.0;
        if self.shape == RouteShape::Open {
            constraints.min_descent_m = 0.0;
        } else {
            constraints.min_descent_m = self.climb_m.min;
            constraints.max_descent_m = self.climb_m.max;
        }
        constraints.max_repeated_edge_fraction = if self.shape == RouteShape::OutAndBack {
            1.0
        } else {
            defaults.max_repeated_edge_fraction
        };
        Ok(constraints)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrailLeg {
    pub geometry: LineString,
    #[serde(alias = "trail_class")]
    pub way_kind: WayKind,
    #[serde(default)]
    pub standing: TrailStanding,
    #[serde(default)]
    pub marking: TrailMarking,
    pub terrain: Terrain,
    pub surface: Option<String>,
    pub access: Access,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SavedTrail {
    pub id: TrailId,
    pub name: String,
    pub legs: Vec<TrailLeg>,
    pub metrics: RouteMetrics,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub support_points: Vec<SupportPoint>,
    /// Authored topology is distinct from measured walk morphology: a manual
    /// loop may lawfully retrace a bridge and therefore measure `OutAndBack`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design_shape: Option<RouteShape>,
    #[serde(default)]
    pub routing: RoutingLaw,
}

impl SavedTrail {
    pub fn capture(graph: &WalkGraph, route: &Route) -> Result<Self> {
        Self::capture_design(graph, route, None)
    }

    fn capture_design(graph: &WalkGraph, route: &Route, trail: Option<&Trail>) -> Result<Self> {
        ensure!(
            graph.walk_edges(route.start, &route.edges).is_some(),
            "candidate `{}` is not a legal graph walk",
            route.name
        );
        let mut at = route.start;
        let legs = route
            .edges
            .iter()
            .map(|edge_id| {
                let edge = &graph.edges[edge_id.0];
                let leg = TrailLeg {
                    geometry: edge.oriented_geometry(at),
                    way_kind: edge.attr.way_kind,
                    standing: edge.attr.standing,
                    marking: edge.attr.marking,
                    terrain: edge.attr.terrain,
                    surface: edge.attr.surface.clone(),
                    access: edge.attr.access,
                };
                at = edge
                    .traverse(at)
                    .expect("validated route edge must be traversable");
                leg
            })
            .collect::<Vec<_>>();
        ensure!(!legs.is_empty(), "cannot save an empty candidate");
        Ok(Self {
            id: trail_id(&legs),
            name: route.name.clone(),
            legs,
            metrics: route.metrics.clone(),
            support_points: trail.map_or_else(Vec::new, |trail| trail.support_points.clone()),
            design_shape: trail.map(|trail| trail.shape),
            routing: trail.map_or_else(RoutingLaw::default, |trail| trail.routing),
        })
    }

    pub fn design(&self) -> Option<Trail> {
        Trail::forge(
            self.design_shape.unwrap_or(self.metrics.shape),
            self.support_points.clone(),
            self.routing,
        )
        .ok()
    }

    pub fn geometry(&self) -> LineString {
        let mut points = Vec::new();
        for leg in &self.legs {
            points.extend(
                leg.geometry
                    .points
                    .iter()
                    .skip(usize::from(!points.is_empty()))
                    .copied(),
            );
        }
        LineString::unchecked(points)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Library {
    schema: u32,
    trails: Vec<SavedTrail>,
    search: SearchRecipe,
    #[serde(default = "legacy_imported")]
    legacy_imported: bool,
}

impl Default for Library {
    fn default() -> Self {
        Self::forge(&LoopConstraints::default())
    }
}

#[derive(Deserialize)]
struct LegacyLibrary {
    trails: Vec<SavedTrail>,
    #[serde(rename = "families")]
    searches: Vec<LegacySearch>,
}

#[derive(Deserialize)]
struct LegacySearch {
    search: SearchRecipe,
}

#[derive(Deserialize)]
struct LibrarySchema {
    schema: u32,
}

const fn legacy_imported() -> bool {
    true
}

impl Library {
    fn forge(defaults: &LoopConstraints) -> Self {
        Self {
            schema: SCHEMA,
            trails: Vec::new(),
            search: SearchRecipe::from_defaults(defaults),
            legacy_imported: true,
        }
    }

    pub fn open(project: &Path, defaults: &LoopConstraints) -> Result<Self> {
        let path = index_path(project);
        match fs::read(&path) {
            Ok(bytes) => {
                let schema = serde_json::from_slice::<LibrarySchema>(&bytes)
                    .with_context(|| format!("parse {}", path.display()))?
                    .schema;
                let (mut library, migrated) = if schema == SCHEMA {
                    (
                        serde_json::from_slice::<Self>(&bytes)
                            .with_context(|| format!("parse {}", path.display()))?,
                        false,
                    )
                } else if matches!(schema, 3..=6) {
                    let mut library = serde_json::from_slice::<Self>(&bytes)
                        .with_context(|| format!("parse {}", path.display()))?;
                    library.schema = SCHEMA;
                    (library, true)
                } else if (1..SCHEMA).contains(&schema) {
                    let legacy = serde_json::from_slice::<LegacyLibrary>(&bytes)
                        .with_context(|| format!("parse {}", path.display()))?;
                    let search = legacy.searches.into_iter().next().map_or_else(
                        || SearchRecipe::from_defaults(defaults),
                        |scope| scope.search,
                    );
                    (
                        Self {
                            schema: SCHEMA,
                            trails: legacy.trails,
                            search,
                            legacy_imported: true,
                        },
                        true,
                    )
                } else {
                    anyhow::bail!("unsupported trail library schema {schema}");
                };
                let repaired = library.recover_metrics(schema) || migrated;
                library.validate()?;
                if repaired {
                    library.save(project)?;
                }
                Ok(library)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let mut library = Self::forge(defaults);
                library.legacy_imported = false;
                library.save(project)?;
                Ok(library)
            }
            Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn save(&self, project: &Path) -> Result<()> {
        self.validate()?;
        let path = index_path(project);
        persistence::replace(&path, &serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("replace library index {}", path.display()))
    }

    pub fn trails(&self) -> &[SavedTrail] {
        &self.trails
    }

    pub(crate) const fn legacy_routes_pending(&self) -> bool {
        !self.legacy_imported
    }

    pub fn trail(&self, id: &TrailId) -> Option<&SavedTrail> {
        self.trails.iter().find(|trail| &trail.id == id)
    }

    pub const fn search(&self) -> &SearchRecipe {
        &self.search
    }

    pub const fn search_mut(&mut self) -> &mut SearchRecipe {
        &mut self.search
    }

    pub fn remove_trail(&mut self, id: &TrailId) -> bool {
        let before = self.trails.len();
        self.trails.retain(|trail| &trail.id != id);
        if self.trails.len() == before {
            return false;
        }
        true
    }

    pub fn rename_trail(&mut self, id: &TrailId, name: &str) -> Result<bool> {
        let name = validate_trail_name(name)?;
        let trail = self
            .trails
            .iter_mut()
            .find(|trail| &trail.id == id)
            .context("trail no longer exists")?;
        if trail.name == name {
            return Ok(false);
        }
        name.clone_into(&mut trail.name);
        Ok(true)
    }

    pub fn promote(&mut self, graph: &WalkGraph, route: &Route) -> Result<TrailId> {
        let trail = SavedTrail::capture(graph, route)?;
        Ok(self.admit(trail))
    }

    pub fn promote_design(
        &mut self,
        graph: &WalkGraph,
        route: &Route,
        design: &Trail,
    ) -> Result<TrailId> {
        let trail = SavedTrail::capture_design(graph, route, Some(design))?;
        Ok(self.admit(trail))
    }

    fn admit(&mut self, trail: SavedTrail) -> TrailId {
        let id = trail.id.clone();
        if self.trail(&id).is_none() {
            self.trails.push(trail);
        }
        id
    }

    pub fn promote_realization(&mut self, realization: &TrailRealization) -> Result<TrailId> {
        let trail = SavedTrail::capture_design(
            realization.graph(),
            &realization.route,
            Some(&realization.trail),
        )?;
        Ok(self.admit(trail))
    }

    pub fn replace_realization(
        &mut self,
        old: &TrailId,
        realization: &TrailRealization,
    ) -> Result<TrailId> {
        ensure!(self.trail(old).is_some(), "trail no longer exists");
        let replacement = SavedTrail::capture_design(
            realization.graph(),
            &realization.route,
            Some(&realization.trail),
        )?;
        let id = replacement.id.clone();
        if &id != old {
            self.trails.retain(|trail| &trail.id != old);
            if self.trail(&id).is_none() {
                self.trails.push(replacement);
            }
        } else if let Some(stored) = self.trails.iter_mut().find(|trail| &trail.id == old) {
            *stored = replacement;
        }
        Ok(id)
    }

    fn validate(&self) -> Result<()> {
        ensure!(self.schema == SCHEMA, "unsupported trail library schema");
        let mut trail_ids = HashSet::new();
        for trail in &self.trails {
            ensure!(
                trail_ids.insert(trail.id.clone()),
                "duplicate trail identity"
            );
            ensure!(!trail.legs.is_empty(), "saved trail has no legs");
            ensure!(
                [
                    trail.metrics.distance_m,
                    trail.metrics.ascent_m,
                    trail.metrics.descent_m,
                    trail.metrics.lower_limb_load_km,
                    trail.metrics.moving_time_s,
                ]
                .into_iter()
                .all(|value| value.is_finite() && value >= 0.0),
                "saved trail contains invalid measurements"
            );
            for leg in &trail.legs {
                ensure!(
                    leg.geometry.points.len() >= 2
                        && leg.geometry.points.iter().all(|point| {
                            point.lon.is_finite()
                                && point.lat.is_finite()
                                && point.ele.is_none_or(f64::is_finite)
                        }),
                    "saved trail contains invalid geometry"
                );
            }
            trail.routing.validate().map_err(anyhow::Error::from)?;
            if !trail.support_points.is_empty() {
                Trail::forge(
                    trail.design_shape.unwrap_or(trail.metrics.shape),
                    trail.support_points.clone(),
                    trail.routing,
                )
                .map_err(anyhow::Error::from)?;
            }
        }
        self.search.validate()?;
        Ok(())
    }

    fn recover_metrics(&mut self, schema: u32) -> bool {
        let mut changed = false;
        for trail in &mut self.trails {
            if schema == 1 {
                let disutility = 0.70_f64.mul_add(
                    trail.metrics.road_fraction,
                    0.25_f64.mul_add(
                        trail.metrics.low_confidence_fraction,
                        0.50 * trail.metrics.restricted_access_fraction,
                    ),
                );
                trail.metrics.quality = 100.0 * (1.0 - disutility.clamp(0.0, 1.0));
                changed = true;
            }
            if schema < 5 {
                let traversal = trail
                    .legs
                    .iter()
                    .map(|leg| {
                        HikingModel
                            .estimate_leg(
                                &leg.geometry,
                                leg.way_kind,
                                leg.terrain,
                                leg.surface.as_deref(),
                                None,
                            )
                            .forward
                    })
                    .fold(
                        trailgen_core::TraversalEstimate::default(),
                        |mut total, estimate| {
                            total.lower_limb_load_km += estimate.lower_limb_load_km;
                            total.moving_time_s += estimate.moving_time_s;
                            total
                        },
                    );
                trail.metrics.lower_limb_load_km = traversal.lower_limb_load_km;
                trail.metrics.moving_time_s = traversal.moving_time_s;
                changed = true;
            }
            if trail.metrics.elevation_fraction <= f64::EPSILON {
                let (covered, total) = trail
                    .legs
                    .iter()
                    .flat_map(|leg| {
                        leg.geometry.points.windows(2).map(|segment| {
                            let length = segment[0].haversine_m(segment[1]);
                            (
                                if segment[0].ele.is_some() && segment[1].ele.is_some() {
                                    length
                                } else {
                                    0.0
                                },
                                length,
                            )
                        })
                    })
                    .fold((0.0, 0.0), |(covered, total), (c, t)| {
                        (covered + c, total + t)
                    });
                let elevation_fraction = covered / total.max(1.0);
                if elevation_fraction > 0.0 {
                    trail.metrics.elevation_fraction = elevation_fraction.clamp(0.0, 1.0);
                    changed = true;
                }
            }
        }
        changed
    }

    pub(crate) fn read_legacy_routes(project: &Path, graph: &WalkGraph) -> Result<Vec<SavedTrail>> {
        let generated_graph =
            read_optional::<WalkGraph>(&project.join("routes/generated.graph.json"))?;
        if generated_graph.as_ref() != Some(graph) {
            return Ok(Vec::new());
        }
        let routes = read_optional::<Vec<Route>>(&project.join("routes/generated.routes.json"))?
            .unwrap_or_default();
        routes
            .into_iter()
            .map(|route| {
                let design = Trail::infer(graph, &route, RoutingLaw::default());
                SavedTrail::capture_design(graph, &route, design.as_ref())
            })
            .collect()
    }

    pub(crate) fn absorb_legacy_routes(&mut self, trails: Vec<SavedTrail>) -> bool {
        let mut changed = false;
        for trail in trails {
            if self.trail(&trail.id).is_none() {
                self.trails.push(trail);
                changed = true;
            }
        }
        self.legacy_imported = true;
        changed
    }
}

fn trail_id(legs: &[TrailLeg]) -> TrailId {
    let mut hash = Sha256::new();
    for leg in legs {
        hash.update(leg.geometry.points.len().to_le_bytes());
        for point in &leg.geometry.points {
            hash.update(point.lon.to_bits().to_le_bytes());
            hash.update(point.lat.to_bits().to_le_bytes());
            hash.update(point.ele.map_or(u64::MAX, f64::to_bits).to_le_bytes());
        }
    }
    TrailId(format!("{:x}", hash.finalize())[..24].to_owned())
}

fn read_optional<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {}", path.display()))
            .map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

fn index_path(project: &Path) -> PathBuf {
    project.join(INDEX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trailgen_core::{
        GraphBuilder, RoutingLaw, SearchParams, SolverKind, SupportPoint, Trail, io::geojson,
    };

    fn fixture() -> Result<(WalkGraph, Route)> {
        let graph = GraphBuilder::default().build(&geojson::network_from_str(include_str!(
            "../../trailgen-core/tests/fixtures/mini_network.geojson"
        ))?)?;
        let constraints = LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 20_000.0,
            ..LoopConstraints::default()
        };
        let route = SolverKind::Exact
            .solve(
                SearchParams::default(),
                &graph,
                trailgen_core::VertexId(0),
                &constraints,
                1,
            )
            .into_iter()
            .next()
            .context("fixture must contain a loop")?;
        Ok((graph, route))
    }

    #[test]
    fn default_recipe_targets_lower_limb_load_without_capping_it() -> Result<()> {
        const FOUR_HOURS_S: f64 = 14_400.0;
        const NINE_HOURS_S: f64 = 32_400.0;
        const FORTY_EIGHT_HOURS_S: f64 = 172_800.0;
        let defaults = LoopConstraints::default();
        let recipe = SearchRecipe::from_defaults(&defaults);

        assert!(
            (recipe.lower_limb_load_km - default_lower_limb_load_target()).abs() <= f64::EPSILON
        );
        let projected = recipe.constraints(&defaults)?;
        assert!(projected.max_lower_limb_load_km > 1.0e300);
        assert_eq!(
            recipe.moving_time_s,
            MeasureRange {
                min: 0.0,
                max: 48.0 * 3_600.0,
            }
        );
        assert!((projected.max_moving_time_s - FORTY_EIGHT_HOURS_S).abs() <= f64::EPSILON);
        let bounded = LoopConstraints {
            min_lower_limb_load_km: 50.0,
            max_lower_limb_load_km: 90.0,
            min_moving_time_s: FOUR_HOURS_S,
            max_moving_time_s: NINE_HOURS_S,
            ..defaults
        };
        let recipe = SearchRecipe::from_defaults(&bounded);
        assert!((recipe.lower_limb_load_km - 50.0).abs() <= f64::EPSILON);
        let projected = recipe.constraints(&bounded)?;
        assert!((projected.max_lower_limb_load_km - 90.0).abs() <= f64::EPSILON);
        assert!((recipe.moving_time_s.min - FOUR_HOURS_S).abs() <= f64::EPSILON);
        assert!((recipe.moving_time_s.max - NINE_HOURS_S).abs() <= f64::EPSILON);
        assert!((projected.min_moving_time_s - FOUR_HOURS_S).abs() <= f64::EPSILON);
        assert!((projected.max_moving_time_s - NINE_HOURS_S).abs() <= f64::EPSILON);
        Ok(())
    }

    #[test]
    fn trails_belong_directly_to_the_project() -> Result<()> {
        let (graph, route) = fixture()?;
        let mut library = Library::default();
        let trail = library.promote(&graph, &route)?;
        assert_eq!(library.trails().len(), 1);
        assert_eq!(library.trails()[0].id, trail);
        assert!(library.remove_trail(&trail));
        assert!(library.trails().is_empty());
        Ok(())
    }

    #[test]
    fn trail_renaming_preserves_geometry_identity_and_rejects_void_names() -> Result<()> {
        let (graph, route) = fixture()?;
        let mut library = Library::default();
        let id = library.promote(&graph, &route)?;
        assert!(library.rename_trail(&id, "  Seven Hills  ")?);
        let renamed = library.trail(&id).context("renamed trail")?;
        assert_eq!(renamed.id, id);
        assert_eq!(renamed.name, "Seven Hills");
        assert!(!library.rename_trail(&id, "Seven Hills")?);
        assert!(library.rename_trail(&id, " \n ").is_err());
        Ok(())
    }

    #[test]
    fn schema_two_organizers_collapse_without_losing_trails() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let (graph, route) = fixture()?;
        let saved = SavedTrail::capture(&graph, &route)?;
        let id = saved.id.clone();
        let mut first = SearchRecipe::from_defaults(&LoopConstraints::default());
        first.lower_limb_load_km = 73.0;
        let second = SearchRecipe::from_defaults(&LoopConstraints::default());
        let path = index_path(temp.path());
        fs::create_dir_all(path.parent().context("index parent")?)?;
        fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": 2,
                "next_family": 3,
                "trails": [saved],
                "families": [
                    {"id": 1, "name": "long", "search": first, "trails": [id]},
                    {"id": 2, "name": "climby", "search": second, "trails": []}
                ]
            }))?,
        )?;

        let library = Library::open(temp.path(), &LoopConstraints::default())?;
        assert_eq!(library.trails().len(), 1);
        assert!((library.search().lower_limb_load_km - 73.0).abs() < f64::EPSILON);
        let stored: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
        assert_eq!(stored["schema"], SCHEMA);
        assert!(stored.get("families").is_none());
        Ok(())
    }

    #[test]
    fn missing_library_migrates_legacy_routes_once() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let (graph, route) = fixture()?;
        fs::create_dir(temp.path().join("routes"))?;
        fs::write(
            temp.path().join("routes/generated.graph.json"),
            serde_json::to_vec_pretty(&graph)?,
        )?;
        fs::write(
            temp.path().join("routes/generated.routes.json"),
            serde_json::to_vec_pretty(&vec![route])?,
        )?;
        let mut library = Library::open(temp.path(), &LoopConstraints::default())?;
        assert!(library.legacy_routes_pending());
        assert!(
            Library::open(temp.path(), &LoopConstraints::default())?.legacy_routes_pending(),
            "an interrupted first launch must not suppress legacy migration"
        );
        library.search_mut().lower_limb_load_km = 47.0;
        let legacy = Library::read_legacy_routes(temp.path(), &graph)?;
        assert!(library.absorb_legacy_routes(legacy));
        assert_eq!(library.trails.len(), 1);
        assert!((library.search().lower_limb_load_km - 47.0).abs() < f64::EPSILON);
        assert!(
            !library.trails[0].support_points.is_empty(),
            "migrated generated routes must remain editable"
        );
        library.trails.clear();
        library.save(temp.path())?;
        let reopened = Library::open(temp.path(), &LoopConstraints::default())?;
        assert!(reopened.trails.is_empty());
        assert!(!reopened.legacy_routes_pending());
        Ok(())
    }

    #[test]
    fn recipes_store_intent_without_a_graph_vertex() -> Result<()> {
        let defaults = LoopConstraints::default();
        let mut recipe = SearchRecipe::from_defaults(&defaults);
        assert!(recipe.trailhead.is_none());
        recipe.shape = RouteShape::OutAndBack;
        recipe.climb_m = MeasureRange {
            min: 100.0,
            max: 400.0,
        };
        let constraints = recipe.constraints(&defaults)?;
        assert_eq!(constraints.allowed_shapes, [RouteShape::OutAndBack]);
        assert!((constraints.max_repeated_edge_fraction - 1.0).abs() < f64::EPSILON);
        assert!((constraints.min_descent_m - 100.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn recipes_reject_corrupt_persisted_trailheads() {
        let mut recipe = SearchRecipe::from_defaults(&LoopConstraints::default());
        recipe.trailhead = Some(Trailhead(Coord::new(f64::NAN, 41.0)));
        assert!(recipe.validate().is_err());
    }

    #[test]
    fn concave_search_boundary_rejects_an_excursion_between_interior_endpoints() -> Result<()> {
        let boundary = SearchBoundary::forge(vec![
            Coord::new(0.0, 0.0),
            Coord::new(3.0, 0.0),
            Coord::new(3.0, 3.0),
            Coord::new(2.0, 3.0),
            Coord::new(2.0, 1.0),
            Coord::new(1.0, 1.0),
            Coord::new(1.0, 3.0),
            Coord::new(0.0, 3.0),
        ])?;
        let west = Coord::new(0.5, 2.0);
        let east = Coord::new(2.5, 2.0);

        assert!(boundary.contains(west));
        assert!(boundary.contains(east));
        assert!(!boundary.contains_segment(west, east));
        assert!(boundary.contains_segment(west, Coord::new(0.5, 0.5)));
        Ok(())
    }

    #[test]
    fn schema_three_search_recipe_gains_an_empty_boundary() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = index_path(temp.path());
        fs::create_dir_all(path.parent().context("index parent")?)?;
        let mut value = serde_json::to_value(Library::default())?;
        value["schema"] = serde_json::json!(3);
        let _boundary = value["search"]
            .as_object_mut()
            .context("search recipe object")?
            .remove("boundary");
        fs::write(&path, serde_json::to_vec_pretty(&value)?)?;

        let library = Library::open(temp.path(), &LoopConstraints::default())?;

        assert!(library.search().boundary.is_none());
        let stored: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
        assert_eq!(stored["schema"], SCHEMA);
        Ok(())
    }

    #[test]
    fn schema_four_difficulty_migrates_to_load_and_time() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let (graph, route) = fixture()?;
        let path = index_path(temp.path());
        fs::create_dir_all(path.parent().context("index parent")?)?;
        let mut library = Library::default();
        library.search.lower_limb_load_km = 73.0;
        library.promote(&graph, &route)?;
        let mut value = serde_json::to_value(library)?;
        value["schema"] = serde_json::json!(4);
        let search = value["search"]
            .as_object_mut()
            .context("search recipe object")?;
        search.insert("difficulty".to_owned(), serde_json::json!(73.0));
        search.remove("lower_limb_load_km");
        search.remove("moving_time_s");
        for trail in value["trails"].as_array_mut().context("saved trails")? {
            let metrics = trail["metrics"].as_object_mut().context("route metrics")?;
            metrics.insert("difficulty".to_owned(), serde_json::json!(12_345.0));
            metrics.remove("lower_limb_load_km");
            metrics.remove("moving_time_s");
        }
        fs::write(&path, serde_json::to_vec_pretty(&value)?)?;

        let library = Library::open(temp.path(), &LoopConstraints::default())?;

        assert!((library.search().lower_limb_load_km - 73.0).abs() <= f64::EPSILON);
        assert_eq!(library.search().moving_time_s, default_moving_time_range());
        assert!(library.trails()[0].metrics.lower_limb_load_km > 0.0);
        assert!(library.trails()[0].metrics.moving_time_s > 0.0);
        let stored: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
        assert_eq!(stored["schema"], SCHEMA);
        assert!(stored["search"].get("difficulty").is_none());
        Ok(())
    }

    #[test]
    fn schema_five_trail_class_is_rectified_to_way_kind() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let (graph, route) = fixture()?;
        let path = index_path(temp.path());
        fs::create_dir_all(path.parent().context("index parent")?)?;
        let mut library = Library::default();
        library.promote(&graph, &route)?;
        let mut value = serde_json::to_value(library)?;
        value["schema"] = serde_json::json!(5);
        for trail in value["trails"].as_array_mut().context("saved trails")? {
            for leg in trail["legs"].as_array_mut().context("saved trail legs")? {
                let leg = leg.as_object_mut().context("saved trail leg")?;
                let kind = leg.remove("way_kind").context("way kind")?;
                leg.insert("trail_class".to_owned(), kind);
            }
        }
        value["trails"][0]["legs"][0]["trail_class"] = serde_json::json!("road");
        value["trails"][0]["legs"][1]["trail_class"] = serde_json::json!("service");
        fs::write(&path, serde_json::to_vec_pretty(&value)?)?;

        let library = Library::open(temp.path(), &LoopConstraints::default())?;

        assert!(!library.trails().is_empty());
        assert_eq!(library.trails()[0].legs[0].way_kind, WayKind::Roadway);
        assert_eq!(library.trails()[0].legs[1].way_kind, WayKind::ServiceRoad);
        let stored: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
        assert_eq!(stored["schema"], SCHEMA);
        let leg = &stored["trails"][0]["legs"][0];
        assert!(leg.get("way_kind").is_some());
        assert!(leg.get("trail_class").is_none());
        Ok(())
    }

    #[test]
    fn support_design_survives_library_round_trip_and_replacement() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let (graph, _) = fixture()?;
        let realize = |end: usize| -> Result<TrailRealization> {
            let trail = Trail::forge(
                RouteShape::OutAndBack,
                vec![
                    SupportPoint::forge(graph.vertices[0].coord).context("valid start")?,
                    SupportPoint::forge(graph.vertices[end].coord).context("valid end")?,
                ],
                RoutingLaw::default(),
            )?;
            let constraints = LoopConstraints {
                min_distance_m: 0.0,
                max_distance_m: f64::MAX,
                max_lower_limb_load_km: f64::MAX,
                max_repeated_edge_fraction: 1.0,
                allowed_shapes: vec![RouteShape::OutAndBack],
                ..LoopConstraints::default()
            };
            Ok(trail.realize("manual", &graph, &constraints, 1.0)?)
        };

        let mut library = Library::default();
        let original = library.promote_realization(&realize(2)?)?;
        library.save(temp.path())?;

        let mut reopened = Library::open(temp.path(), &LoopConstraints::default())?;
        assert!(
            reopened
                .trail(&original)
                .and_then(SavedTrail::design)
                .is_some()
        );
        let replacement = reopened.replace_realization(&original, &realize(1)?)?;
        assert!(
            reopened
                .trail(&replacement)
                .and_then(SavedTrail::design)
                .is_some()
        );
        Ok(())
    }
}
