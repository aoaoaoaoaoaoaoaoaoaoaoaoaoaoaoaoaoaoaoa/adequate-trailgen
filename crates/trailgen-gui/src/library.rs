use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::HashSet,
    fmt::{Display, Formatter},
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};
use trailgen_core::{
    Access, Coord, LineString, LoopConstraints, Route, RouteMetrics, RouteShape, Terrain,
    TrailClass, TrailGraph, TrailStanding,
};

const SCHEMA: u32 = 1;
const INDEX: &str = "library/index.json";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct FamilyId(u64);

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TrailId(String);

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct FamilyName(String);

impl FamilyName {
    pub fn forge(raw: &str) -> Option<Self> {
        let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        (!name.is_empty() && name.chars().count() <= 64).then_some(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for FamilyName {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl TryFrom<String> for FamilyName {
    type Error = &'static str;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::forge(&raw).ok_or("family name must contain 1–64 non-whitespace characters")
    }
}

impl From<FamilyName> for String {
    fn from(name: FamilyName) -> Self {
        name.0
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
    pub distance_m: MeasureRange,
    pub climb_m: MeasureRange,
    pub shape: RouteShape,
}

impl SearchRecipe {
    pub fn from_defaults(defaults: &LoopConstraints) -> Self {
        Self {
            trailhead: None,
            distance_m: MeasureRange {
                min: defaults.min_distance_m,
                max: defaults.max_distance_m,
            },
            climb_m: MeasureRange {
                min: defaults.min_ascent_m,
                max: defaults.max_ascent_m,
            },
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
        Ok(())
    }

    pub fn constraints(&self, defaults: &LoopConstraints) -> Result<LoopConstraints> {
        self.validate()?;
        let mut constraints = defaults.clone();
        constraints.min_distance_m = self.distance_m.min;
        constraints.max_distance_m = self.distance_m.max;
        constraints.min_ascent_m = self.climb_m.min;
        constraints.max_ascent_m = self.climb_m.max;
        constraints.allowed_shapes = vec![self.shape];
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
    pub trail_class: TrailClass,
    #[serde(default)]
    pub standing: TrailStanding,
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
}

impl SavedTrail {
    pub fn capture(graph: &TrailGraph, route: &Route) -> Result<Self> {
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
                    trail_class: edge.attr.trail_class,
                    standing: edge.attr.standing,
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
        })
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
pub struct Family {
    pub id: FamilyId,
    pub name: FamilyName,
    pub search: SearchRecipe,
    pub trails: Vec<TrailId>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Library {
    schema: u32,
    next_family: u64,
    trails: Vec<SavedTrail>,
    families: Vec<Family>,
}

impl Default for Library {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            next_family: 1,
            trails: Vec::new(),
            families: Vec::new(),
        }
    }
}

impl Library {
    pub fn open(project: &Path, graph: &TrailGraph) -> Result<Self> {
        let path = index_path(project);
        match fs::read(&path) {
            Ok(bytes) => {
                let library = serde_json::from_slice::<Self>(&bytes)
                    .with_context(|| format!("parse {}", path.display()))?;
                library.validate()?;
                Ok(library)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let library = Self::migrate_legacy(project, graph)?;
                library.save(project)?;
                Ok(library)
            }
            Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn save(&self, project: &Path) -> Result<()> {
        self.validate()?;
        let path = index_path(project);
        let parent = path.parent().context("library index has no parent")?;
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        let temporary = path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&temporary)
                .with_context(|| format!("create {}", temporary.display()))?;
            file.write_all(&serde_json::to_vec_pretty(self)?)
                .with_context(|| format!("write {}", temporary.display()))?;
            file.sync_all()
                .with_context(|| format!("sync {}", temporary.display()))?;
        }
        fs::rename(&temporary, &path)
            .with_context(|| format!("replace {} with {}", temporary.display(), path.display()))
    }

    pub fn families(&self) -> &[Family] {
        &self.families
    }

    pub fn family(&self, id: FamilyId) -> Option<&Family> {
        self.families.iter().find(|family| family.id == id)
    }

    pub fn family_mut(&mut self, id: FamilyId) -> Option<&mut Family> {
        self.families.iter_mut().find(|family| family.id == id)
    }

    pub fn trail(&self, id: &TrailId) -> Option<&SavedTrail> {
        self.trails.iter().find(|trail| &trail.id == id)
    }

    pub fn family_trails(&self, id: FamilyId) -> impl Iterator<Item = &SavedTrail> {
        self.family(id)
            .into_iter()
            .flat_map(|family| family.trails.iter())
            .filter_map(|id| self.trail(id))
    }

    pub fn loose_trails(&self) -> impl Iterator<Item = &SavedTrail> {
        self.trails.iter().filter(|trail| {
            self.families
                .iter()
                .all(|family| !family.trails.contains(&trail.id))
        })
    }

    pub fn add_family(&mut self, defaults: &LoopConstraints) -> FamilyId {
        let id = FamilyId(self.next_family);
        self.next_family = self.next_family.saturating_add(1);
        let name = self.spare_family_name();
        self.families.push(Family {
            id,
            name,
            search: SearchRecipe::from_defaults(defaults),
            trails: Vec::new(),
        });
        id
    }

    pub fn rename_family(&mut self, id: FamilyId, raw: &str) -> bool {
        let Some(name) = FamilyName::forge(raw) else {
            return false;
        };
        if self
            .families
            .iter()
            .any(|family| family.id != id && family.name == name)
        {
            return false;
        }
        let Some(family) = self.family_mut(id) else {
            return false;
        };
        family.name = name;
        true
    }

    pub fn remove_family(&mut self, id: FamilyId) -> bool {
        let before = self.families.len();
        self.families.retain(|family| family.id != id);
        self.families.len() != before
    }

    pub fn remove_trail(&mut self, id: &TrailId) -> bool {
        let before = self.trails.len();
        self.trails.retain(|trail| &trail.id != id);
        if self.trails.len() == before {
            return false;
        }
        for family in &mut self.families {
            family.trails.retain(|trail| trail != id);
        }
        true
    }

    pub fn promote(
        &mut self,
        family: FamilyId,
        graph: &TrailGraph,
        route: &Route,
    ) -> Result<TrailId> {
        ensure!(
            self.family(family).is_some(),
            "trail family no longer exists"
        );
        let trail = SavedTrail::capture(graph, route)?;
        let id = trail.id.clone();
        if self.trail(&id).is_none() {
            self.trails.push(trail);
        }
        let members = &mut self
            .family_mut(family)
            .expect("family existence checked")
            .trails;
        if !members.contains(&id) {
            members.push(id.clone());
        }
        Ok(id)
    }

    pub fn toggle_membership(&mut self, family: FamilyId, trail: &TrailId) -> bool {
        if self.trail(trail).is_none() {
            return false;
        }
        let Some(family) = self.family_mut(family) else {
            return false;
        };
        if let Some(slot) = family.trails.iter().position(|known| known == trail) {
            let _removed = family.trails.remove(slot);
        } else {
            family.trails.push(trail.clone());
        }
        true
    }

    pub fn contains(&self, family: FamilyId, trail: &TrailId) -> bool {
        self.family(family)
            .is_some_and(|family| family.trails.contains(trail))
    }

    fn validate(&self) -> Result<()> {
        ensure!(self.schema == SCHEMA, "unsupported trail library schema");
        let mut family_ids = HashSet::new();
        let mut family_names = HashSet::new();
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
                    trail.metrics.difficulty,
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
        }
        for family in &self.families {
            ensure!(family_ids.insert(family.id), "duplicate family identity");
            ensure!(
                family_names.insert(family.name.clone()),
                "duplicate family name"
            );
            family.search.validate()?;
            let mut members = HashSet::new();
            for trail in &family.trails {
                ensure!(
                    trail_ids.contains(trail),
                    "family references a missing trail"
                );
                ensure!(members.insert(trail), "family contains a trail twice");
            }
        }
        let next = self
            .families
            .iter()
            .map(|family| family.id.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        ensure!(
            self.next_family >= next,
            "family identity counter regressed"
        );
        Ok(())
    }

    fn spare_family_name(&self) -> FamilyName {
        let mut raw = "new family".to_owned();
        let mut suffix = 2_u64;
        while self
            .families
            .iter()
            .any(|family| family.name.as_str() == raw)
        {
            raw = format!("new family {suffix}");
            suffix = suffix.saturating_add(1);
        }
        FamilyName::forge(&raw).expect("compiled family name is valid")
    }

    fn migrate_legacy(project: &Path, graph: &TrailGraph) -> Result<Self> {
        let generated_graph =
            read_optional::<TrailGraph>(&project.join("routes/generated.graph.json"))?;
        if generated_graph.as_ref() != Some(graph) {
            return Ok(Self::default());
        }
        let routes = read_optional::<Vec<Route>>(&project.join("routes/generated.routes.json"))?
            .unwrap_or_default();
        let mut library = Self::default();
        for route in routes {
            let trail = SavedTrail::capture(graph, &route)?;
            if library.trail(&trail.id).is_none() {
                library.trails.push(trail);
            }
        }
        Ok(library)
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

pub fn index_path(project: &Path) -> PathBuf {
    project.join(INDEX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trailgen_core::{GraphBuilder, SearchParams, SolverKind, io::geojson};

    fn fixture() -> Result<(TrailGraph, Route)> {
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
    fn families_are_flat_membership_sets_and_deletion_spills() -> Result<()> {
        let (graph, route) = fixture()?;
        let mut library = Library::default();
        let a = library.add_family(&LoopConstraints::default());
        let b = library.add_family(&LoopConstraints::default());
        let trail = library.promote(a, &graph, &route)?;
        assert!(library.toggle_membership(b, &trail));
        assert!(library.contains(a, &trail) && library.contains(b, &trail));
        assert!(library.remove_family(a));
        assert_eq!(library.loose_trails().count(), 0);
        assert!(library.remove_family(b));
        assert_eq!(library.loose_trails().count(), 1);
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
        let mut library = Library::open(temp.path(), &graph)?;
        assert_eq!(library.trails.len(), 1);
        library.trails.clear();
        library.save(temp.path())?;
        assert!(Library::open(temp.path(), &graph)?.trails.is_empty());
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
}
