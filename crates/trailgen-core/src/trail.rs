use crate::{
    Access, Coord, DifficultyBreakdown, Edge, EdgeAttr, EdgeId, GradeDistribution, LineString,
    LoopConstraints, Route, RouteShape, Terrain, TrailGraph, TrailgenError, TurnBan, Vertex,
    VertexId,
};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap},
    sync::Arc,
};

pub const DEFAULT_ROAD_AVERSION: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SupportPoint(Coord);

impl SupportPoint {
    pub fn forge(coord: Coord) -> Option<Self> {
        (coord.lon.is_finite()
            && coord.lat.is_finite()
            && (-180.0..=180.0).contains(&coord.lon)
            && (-85.0..=85.0).contains(&coord.lat)
            && coord.ele.is_none_or(f64::is_finite))
        .then_some(Self(coord))
    }

    #[must_use]
    pub const fn coord(self) -> Coord {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingLaw {
    #[serde(default = "default_road_aversion", alias = "road_penalty")]
    pub road_aversion: f64,
}

impl Default for RoutingLaw {
    fn default() -> Self {
        Self {
            road_aversion: DEFAULT_ROAD_AVERSION,
        }
    }
}

impl RoutingLaw {
    pub fn validate(self) -> crate::Result<()> {
        if !self.road_aversion.is_finite() || self.road_aversion < 0.0 {
            return Err(TrailgenError::InvalidData(
                "road aversion must be finite and nonnegative".to_owned(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn edge_cost(self, graph: &TrailGraph, edge: EdgeId) -> Option<f64> {
        let attr = &graph.edges[edge.0].attr;
        if matches!(attr.access, Access::Closed | Access::Private) {
            return None;
        }
        let road = attr.road_exposure.clamp(0.0, 1.0).max(
            if matches!(attr.terrain, Terrain::Road | Terrain::Pavement) {
                1.0
            } else {
                0.0
            },
        );
        Some(attr.length_m * self.road_aversion.mul_add(road, 1.0))
    }
}

const fn default_road_aversion() -> f64 {
    DEFAULT_ROAD_AVERSION
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trail {
    pub shape: RouteShape,
    pub support_points: Vec<SupportPoint>,
    #[serde(default)]
    pub routing: RoutingLaw,
}

impl Trail {
    pub fn forge(
        shape: RouteShape,
        support_points: Vec<SupportPoint>,
        routing: RoutingLaw,
    ) -> crate::Result<Self> {
        let trail = Self {
            shape,
            support_points,
            routing,
        };
        trail.validate()?;
        Ok(trail)
    }

    pub fn validate(&self) -> crate::Result<()> {
        self.routing.validate()?;
        if self.shape == RouteShape::FigureEight {
            return Err(TrailgenError::InvalidData(
                "figure-eight support topology is not defined".to_owned(),
            ));
        }
        if self.support_points.len() < 2 {
            return Err(TrailgenError::InvalidData(
                "a trail needs a trailhead and at least one further support point".to_owned(),
            ));
        }
        if self
            .support_points
            .iter()
            .any(|point| SupportPoint::forge(point.coord()).is_none())
        {
            return Err(TrailgenError::InvalidData(
                "trail contains an invalid support point".to_owned(),
            ));
        }
        Ok(())
    }

    /// Recovers a compact, lossless support design. Globally shortest spans
    /// remain single legs; an irreducible non-shortest edge receives an
    /// interior support so incision compels that physical segment.
    #[must_use]
    pub fn infer(graph: &TrailGraph, route: &Route, routing: RoutingLaw) -> Option<Self> {
        routing.validate().ok()?;
        let vertices = route_vertices(graph, route)?;
        let n = route.edges.len();
        if n == 0 {
            return None;
        }
        let points = match route.metrics.shape {
            RouteShape::OutAndBack => {
                let split = n / 2;
                if !n.is_multiple_of(2)
                    || route.edges[..split]
                        != route.edges[split..]
                            .iter()
                            .rev()
                            .copied()
                            .collect::<Vec<_>>()
                {
                    return None;
                }
                let mut points = Vec::new();
                compress_arc(graph, route, &vertices, routing, 0, split, &mut points)?;
                points.push(vertex_support(graph, vertices[split])?);
                points
            }
            RouteShape::Loop => {
                if vertices[n] != route.start || n < 2 {
                    return None;
                }
                let mut points = Vec::new();
                let split = n / 2;
                compress_arc(graph, route, &vertices, routing, 0, split, &mut points)?;
                compress_arc(graph, route, &vertices, routing, split, n, &mut points)?;
                points
            }
            RouteShape::Open => {
                let mut points = Vec::new();
                compress_arc(graph, route, &vertices, routing, 0, n, &mut points)?;
                points.push(vertex_support(graph, vertices[n])?);
                points
            }
            RouteShape::FigureEight => return None,
        };
        Self::forge(route.metrics.shape, points, routing).ok()
    }

    pub fn realize(
        &self,
        name: impl Into<String>,
        graph: &TrailGraph,
        constraints: &LoopConstraints,
        max_snap_m: f64,
    ) -> crate::Result<TrailRealization> {
        self.validate()?;
        if !max_snap_m.is_finite() || max_snap_m <= 0.0 {
            return Err(TrailgenError::InvalidData(
                "support-point snap distance must be positive".to_owned(),
            ));
        }
        let (incised, bindings) = bind(graph, &self.support_points, max_snap_m)?;
        let graph = incised.as_deref().unwrap_or(graph);
        let start = bindings[0].vertex;
        let mut edges = Vec::new();
        let mut previous = None;
        for targets in bindings.windows(2) {
            let leg = shortest_path(
                graph,
                targets[0].vertex,
                targets[1].vertex,
                previous,
                self.routing,
                None,
            )
            .ok_or_else(|| {
                TrailgenError::InvalidData(
                    "no lawful trail connects consecutive support points".to_owned(),
                )
            })?;
            previous = leg.last().copied().or(previous);
            edges.extend(leg);
        }
        match self.shape {
            RouteShape::Open => {}
            RouteShape::OutAndBack => {
                let spine = edges.len();
                edges.extend_from_within(..);
                edges[spine..].reverse();
            }
            RouteShape::Loop => {
                let end = bindings
                    .last()
                    .expect("validated support points are nonempty")
                    .vertex;
                edges.extend(
                    shortest_path(graph, end, start, previous, self.routing, None).ok_or_else(
                        || {
                            TrailgenError::InvalidData(
                                "no lawful return connects the final support point".to_owned(),
                            )
                        },
                    )?,
                );
            }
            RouteShape::FigureEight => unreachable!("figure-eight rejected by validation"),
        }
        if graph.walk_edges(start, &edges).is_none() {
            return Err(TrailgenError::InvalidData(
                "support points induce an illegal directed trail".to_owned(),
            ));
        }
        let route = Route::from_edges(name, graph, start, edges, constraints);
        if route.metrics.shape != self.shape {
            return Err(TrailgenError::InvalidData(format!(
                "support points realize {:?}, not {:?}",
                route.metrics.shape, self.shape
            )));
        }
        Ok(TrailRealization {
            trail: Self {
                shape: self.shape,
                support_points: bindings.iter().map(|binding| binding.anchor).collect(),
                routing: self.routing,
            },
            bindings,
            route,
            incised,
        })
    }
}

fn route_vertices(graph: &TrailGraph, route: &Route) -> Option<Vec<VertexId>> {
    let mut vertices = Vec::with_capacity(route.edges.len() + 1);
    let mut at = route.start;
    let mut previous = None;
    vertices.push(at);
    for edge in &route.edges {
        let segment = graph.edges.get(edge.0)?;
        if !graph.turn_allowed(previous, at, *edge) {
            return None;
        }
        at = segment.traverse(at)?;
        previous = Some(*edge);
        vertices.push(at);
    }
    Some(vertices)
}

fn compress_arc(
    graph: &TrailGraph,
    route: &Route,
    vertices: &[VertexId],
    routing: RoutingLaw,
    lo: usize,
    hi: usize,
    supports: &mut Vec<SupportPoint>,
) -> Option<()> {
    let previous = lo.checked_sub(1).map(|index| route.edges[index]);
    if shortest_path(graph, vertices[lo], vertices[hi], previous, routing, None)
        .is_some_and(|shortest| shortest == route.edges[lo..hi])
    {
        supports.push(vertex_support(graph, vertices[lo])?);
        return Some(());
    }
    if hi - lo <= 1 {
        supports.push(vertex_support(graph, vertices[lo])?);
        let edge = &graph.edges[route.edges[lo].0];
        supports.push(SupportPoint::forge(line_coord_at(
            &edge.geometry,
            edge.geometry.length_m() * 0.5,
        ))?);
        return Some(());
    }
    let split = lo + (hi - lo) / 2;
    compress_arc(graph, route, vertices, routing, lo, split, supports)?;
    compress_arc(graph, route, vertices, routing, split, hi, supports)
}

fn vertex_support(graph: &TrailGraph, vertex: VertexId) -> Option<SupportPoint> {
    SupportPoint::forge(graph.vertices[vertex.0].coord)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportBinding {
    pub requested: SupportPoint,
    pub anchor: SupportPoint,
    pub vertex: VertexId,
    pub snap_m: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrailRealization {
    pub trail: Trail,
    pub bindings: Vec<SupportBinding>,
    pub route: Route,
    incised: Option<Arc<TrailGraph>>,
}

impl TrailRealization {
    #[must_use]
    pub fn graph<'a>(&'a self, source: &'a TrailGraph) -> &'a TrailGraph {
        self.incised.as_deref().unwrap_or(source)
    }
}

const INCISION_EPSILON_M: f64 = 0.05;

struct Incision {
    progress_m: f64,
    vertex: VertexId,
    supports: Vec<usize>,
}

type PendingBindings = Vec<Option<SupportBinding>>;
type IncisionMap = BTreeMap<EdgeId, Vec<Incision>>;

fn bind(
    graph: &TrailGraph,
    supports: &[SupportPoint],
    max_snap_m: f64,
) -> crate::Result<(Option<Arc<TrailGraph>>, Vec<SupportBinding>)> {
    let (mut bindings, mut incisions) = locate_supports(graph, supports, max_snap_m)?;
    if incisions.is_empty() {
        return Ok((None, complete_bindings(bindings)?));
    }
    let incised = incise(graph, supports, &mut bindings, &mut incisions)?;
    Ok((Some(Arc::new(incised)), complete_bindings(bindings)?))
}

fn locate_supports(
    graph: &TrailGraph,
    supports: &[SupportPoint],
    max_snap_m: f64,
) -> crate::Result<(PendingBindings, IncisionMap)> {
    let mut bindings = vec![None; supports.len()];
    let mut incisions = IncisionMap::new();
    for (slot, requested) in supports.iter().copied().enumerate() {
        let projection = graph.project_onto_edge(requested.coord()).ok_or_else(|| {
            TrailgenError::InvalidData("cannot bind a support point to an empty network".to_owned())
        })?;
        if projection.distance_m > max_snap_m {
            return Err(TrailgenError::InvalidData(format!(
                "support point lies {:.0} m from the trail network",
                projection.distance_m
            )));
        }
        let edge = &graph.edges[projection.edge.0];
        let span_m = edge.geometry.length_m();
        let endpoint = if projection.progress_m <= INCISION_EPSILON_M {
            Some(edge.a)
        } else if span_m - projection.progress_m <= INCISION_EPSILON_M {
            Some(edge.b)
        } else {
            None
        };
        if let Some(vertex) = endpoint {
            bindings[slot] = Some(SupportBinding {
                requested,
                anchor: SupportPoint(graph.vertices[vertex.0].coord),
                vertex,
                snap_m: projection.distance_m,
            });
            continue;
        }
        let cuts = incisions.entry(projection.edge).or_default();
        if let Some(cut) = cuts
            .iter_mut()
            .find(|cut| (cut.progress_m - projection.progress_m).abs() <= INCISION_EPSILON_M)
        {
            cut.supports.push(slot);
        } else {
            cuts.push(Incision {
                progress_m: projection.progress_m,
                vertex: VertexId(usize::MAX),
                supports: vec![slot],
            });
        }
    }
    Ok((bindings, incisions))
}

fn incise(
    graph: &TrailGraph,
    supports: &[SupportPoint],
    bindings: &mut [Option<SupportBinding>],
    incisions: &mut IncisionMap,
) -> crate::Result<TrailGraph> {
    let mut vertices = graph.vertices.clone();
    for (edge, cuts) in incisions.iter_mut() {
        cuts.sort_by(|left, right| left.progress_m.total_cmp(&right.progress_m));
        for cut in cuts {
            cut.vertex = VertexId(vertices.len());
            let anchor = SupportPoint(line_coord_at(&graph.edges[edge.0].geometry, cut.progress_m));
            vertices.push(Vertex {
                id: cut.vertex,
                coord: anchor.coord(),
            });
            for slot in &cut.supports {
                bindings[*slot] = Some(SupportBinding {
                    requested: supports[*slot],
                    anchor,
                    vertex: cut.vertex,
                    snap_m: supports[*slot].coord().haversine_m(anchor.coord()),
                });
            }
        }
    }

    let mut edges = Vec::with_capacity(graph.edges.len() + vertices.len() - graph.vertices.len());
    let mut heirs = vec![Vec::new(); graph.edges.len()];
    for source in &graph.edges {
        let Some(cuts) = incisions.get(&source.id) else {
            let mut edge = source.clone();
            edge.id = EdgeId(edges.len());
            heirs[source.id.0].push(edge.id);
            edges.push(edge);
            continue;
        };
        let span_m = source.geometry.length_m();
        let mut marks = Vec::with_capacity(cuts.len() + 2);
        marks.push((0.0, source.a));
        marks.extend(cuts.iter().map(|cut| (cut.progress_m, cut.vertex)));
        marks.push((span_m, source.b));
        for (segment, marks) in marks.windows(2).enumerate() {
            let geometry = line_slice(&source.geometry, marks[0].0, marks[1].0);
            // Crossings have no along-edge coordinate; charge them once to
            // the median heir so subdivision preserves the known total.
            let carries_crossings = segment == cuts.len() / 2;
            let mut edge = Edge {
                id: EdgeId(edges.len()),
                a: marks[0].1,
                b: marks[1].1,
                attr: cleaved_attr(source, &geometry, span_m, carries_crossings),
                geometry,
            };
            edge.attr.length_m = source.attr.length_m * (marks[1].0 - marks[0].0) / span_m;
            heirs[source.id.0].push(edge.id);
            edges.push(edge);
        }
    }
    let mut incised = TrailGraph::new(vertices, edges);
    incised.turn_bans = graph
        .turn_bans
        .iter()
        .map(|ban| inherit_ban(graph, &heirs, ban))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| TrailgenError::InvalidData("turn-ban inheritance failed".to_owned()))?;
    incised.validate()?;
    Ok(incised)
}

fn complete_bindings(bindings: Vec<Option<SupportBinding>>) -> crate::Result<Vec<SupportBinding>> {
    bindings
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            TrailgenError::InvalidData("support-point binding was incomplete".to_owned())
        })
}

fn inherit_ban(graph: &TrailGraph, heirs: &[Vec<EdgeId>], ban: &TurnBan) -> Option<TurnBan> {
    let incident = |edge: EdgeId| {
        let source = &graph.edges[edge.0];
        if ban.via == source.a {
            heirs[edge.0].first().copied()
        } else if ban.via == source.b {
            heirs[edge.0].last().copied()
        } else {
            None
        }
    };
    Some(TurnBan {
        via: ban.via,
        from: incident(ban.from)?,
        to: incident(ban.to)?,
        provenance: ban.provenance.clone(),
    })
}

fn line_coord_at(line: &LineString, progress_m: f64) -> Coord {
    let mut traversed_m = 0.0;
    for segment in line.points.windows(2) {
        let length_m = segment[0].haversine_m(segment[1]);
        if progress_m <= traversed_m + length_m {
            let t = if length_m <= f64::EPSILON {
                0.0
            } else {
                (progress_m - traversed_m) / length_m
            };
            return segment[0].lerp(segment[1], t.clamp(0.0, 1.0));
        }
        traversed_m += length_m;
    }
    line.end()
}

fn line_slice(line: &LineString, start_m: f64, end_m: f64) -> LineString {
    let mut points = vec![line_coord_at(line, start_m)];
    let mut traversed_m = 0.0;
    for segment in line.points.windows(2) {
        traversed_m += segment[0].haversine_m(segment[1]);
        if traversed_m > start_m && traversed_m < end_m {
            points.push(segment[1]);
        }
    }
    let end = line_coord_at(line, end_m);
    if points.last() != Some(&end) {
        points.push(end);
    }
    if points.len() == 1 {
        points.push(end);
    }
    LineString::unchecked(points)
}

fn cleaved_attr(
    source: &Edge,
    geometry: &LineString,
    span_m: f64,
    carries_crossings: bool,
) -> EdgeAttr {
    let ratio = geometry.length_m() / span_m;
    let (measured_ascent, measured_descent) = geometry.ascent_descent_m();
    let measured = geometry.points.iter().all(|point| point.ele.is_some());
    let ascent_m = if measured {
        measured_ascent
    } else {
        source.attr.ascent_m * ratio
    };
    let descent_m = if measured {
        measured_descent
    } else {
        source.attr.descent_m * ratio
    };
    let mut attr = source.attr.clone();
    attr.length_m *= ratio;
    attr.ascent_m = ascent_m;
    attr.descent_m = descent_m;
    attr.sustained_steep_m *= ratio;
    attr.grade_distribution = scale_grades(attr.grade_distribution, ratio);
    attr.difficulty_breakdown = scale_difficulty(
        attr.difficulty_breakdown,
        ratio,
        ascent_m / source.attr.ascent_m.max(f64::EPSILON),
        descent_m / source.attr.descent_m.max(f64::EPSILON),
    );
    attr.difficulty = attr.difficulty_breakdown.total();
    if !carries_crossings {
        attr.crossings.clear();
    }
    attr
}

const fn scale_grades(grades: GradeDistribution, ratio: f64) -> GradeDistribution {
    GradeDistribution {
        flat_m: grades.flat_m * ratio,
        rolling_m: grades.rolling_m * ratio,
        steep_m: grades.steep_m * ratio,
        savage_m: grades.savage_m * ratio,
    }
}

const fn scale_difficulty(
    difficulty: DifficultyBreakdown,
    ratio: f64,
    ascent_ratio: f64,
    descent_ratio: f64,
) -> DifficultyBreakdown {
    DifficultyBreakdown {
        distance: difficulty.distance * ratio,
        ascent: difficulty.ascent * ascent_ratio,
        descent: difficulty.descent * descent_ratio,
        grade: difficulty.grade * ratio,
        terrain: difficulty.terrain * ratio,
        road: difficulty.road * ratio,
        technical: difficulty.technical * ratio,
        navigation: difficulty.navigation * ratio,
        bushwhack: difficulty.bushwhack * ratio,
        confidence: difficulty.confidence * ratio,
        access: difficulty.access * ratio,
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Walk {
    at: VertexId,
    previous: Option<EdgeId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Frontier {
    cost: f64,
    walk: Walk,
}

impl Eq for Frontier {}

impl Ord for Frontier {
    fn cmp(&self, rhs: &Self) -> Ordering {
        rhs.cost
            .total_cmp(&self.cost)
            .then_with(|| rhs.walk.cmp(&self.walk))
    }
}

impl PartialOrd for Frontier {
    fn partial_cmp(&self, rhs: &Self) -> Option<Ordering> {
        Some(self.cmp(rhs))
    }
}

pub(crate) fn shortest_path(
    graph: &TrailGraph,
    from: VertexId,
    target: VertexId,
    previous: Option<EdgeId>,
    law: RoutingLaw,
    max_cost: Option<f64>,
) -> Option<Vec<EdgeId>> {
    if from == target {
        return Some(Vec::new());
    }
    let origin = Walk { at: from, previous };
    let mut frontier = BinaryHeap::from([Frontier {
        cost: 0.0,
        walk: origin,
    }]);
    let mut distance = BTreeMap::from([(origin, 0.0)]);
    let mut predecessor = BTreeMap::<Walk, (Walk, EdgeId)>::new();
    while let Some(Frontier { cost, walk }) = frontier.pop() {
        if max_cost.is_some_and(|maximum| cost > maximum)
            || distance
                .get(&walk)
                .is_some_and(|best| cost > *best + f64::EPSILON)
        {
            continue;
        }
        if walk.at == target {
            let mut edges = Vec::new();
            let mut cursor = walk;
            while cursor != origin {
                let (prior, edge) = predecessor.get(&cursor).copied()?;
                edges.push(edge);
                cursor = prior;
            }
            edges.reverse();
            return Some(edges);
        }
        for edge in graph.adjacency[walk.at.0].iter().copied() {
            if !graph.turn_allowed(walk.previous, walk.at, edge) {
                continue;
            }
            let Some(edge_cost) = law.edge_cost(graph, edge) else {
                continue;
            };
            let next_cost = cost + edge_cost;
            if max_cost.is_some_and(|maximum| next_cost > maximum) {
                continue;
            }
            let next = Walk {
                at: graph.edges[edge.0].traverse(walk.at)?,
                previous: Some(edge),
            };
            if distance
                .get(&next)
                .is_none_or(|best| next_cost < *best - f64::EPSILON)
            {
                distance.insert(next, next_cost);
                predecessor.insert(next, (walk, edge));
                frontier.push(Frontier {
                    cost: next_cost,
                    walk: next,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EdgeTravel, GraphBuilder, JunctionPolicy, LineString, Provenance, SegmentDraft, TrailClass,
        TrailStanding, io::geojson,
    };

    fn graph() -> TrailGraph {
        GraphBuilder::default()
            .build(
                &geojson::network_from_str(include_str!("../tests/fixtures/mini_network.geojson"))
                    .expect("parse fixture"),
            )
            .expect("build fixture")
    }

    #[test]
    fn out_and_back_is_a_shortest_spine_reversed_by_construction() {
        let graph = graph();
        let start = SupportPoint::forge(graph.vertices[0].coord).expect("valid start");
        let end = SupportPoint::forge(graph.vertices[2].coord).expect("valid end");
        let trail = Trail::forge(
            RouteShape::OutAndBack,
            vec![start, end],
            RoutingLaw::default(),
        )
        .expect("valid trail");
        let constraints = LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: f64::MAX,
            max_repeated_edge_fraction: 1.0,
            allowed_shapes: vec![RouteShape::OutAndBack],
            ..LoopConstraints::default()
        };
        let realized = trail
            .realize("manual", &graph, &constraints, 1.0)
            .expect("realize support points");
        let split = realized.route.edges.len() / 2;
        assert_eq!(
            realized.route.edges[..split],
            realized.route.edges[split..]
                .iter()
                .rev()
                .copied()
                .collect::<Vec<_>>()
        );
        let inferred = Trail::infer(&graph, &realized.route, RoutingLaw::default())
            .expect("canonical out-and-back is inferable");
        assert_eq!(inferred.support_points.len(), 2);
    }

    #[test]
    fn out_and_back_may_turn_inside_an_edge() {
        let start_coord = Coord::with_ele(0.0, 0.0, 0.0);
        let turn_coord = Coord::new(0.001, 0.000_004);
        let graph = GraphBuilder::default()
            .build(&[SegmentDraft {
                geometry: LineString::new(vec![start_coord, Coord::with_ele(0.004, 0.0, 40.0)])
                    .expect("valid line"),
                junctions: JunctionPolicy::Planar,
                turn_ref: None,
                turn_restrictions: Vec::new(),
                trail_class: TrailClass::Path,
                standing: TrailStanding::Established,
                marking: crate::TrailMarking::default(),
                terrain: Terrain::Trail,
                terrain_confidence: Some(1.0),
                surface: Some("dirt".to_owned()),
                access: Access::Open,
                travel: EdgeTravel::Both,
                road_exposure: 0.0,
                confidence: 1.0,
                provenance: vec![Provenance::fixture("long-edge")],
            }])
            .expect("build line");
        let source_length_m = graph.edges[0].attr.length_m;
        let trail = Trail::forge(
            RouteShape::OutAndBack,
            vec![
                SupportPoint::forge(start_coord).expect("valid start"),
                SupportPoint::forge(turn_coord).expect("valid turn"),
            ],
            RoutingLaw::default(),
        )
        .expect("valid trail");
        let constraints = LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: f64::MAX,
            max_repeated_edge_fraction: 1.0,
            allowed_shapes: vec![RouteShape::OutAndBack],
            ..LoopConstraints::default()
        };

        let realized = trail
            .realize("partial", &graph, &constraints, 2.0)
            .expect("realize partial edge");
        let incised = realized.graph(&graph);

        assert_eq!(graph.vertices.len(), 2, "the corpus remains immutable");
        assert_eq!(graph.edges.len(), 1, "the corpus remains immutable");
        assert_eq!(incised.vertices.len(), 3);
        assert_eq!(incised.edges.len(), 2);
        assert_eq!(realized.route.edges.len(), 2);
        assert_eq!(realized.route.edges[0], realized.route.edges[1]);
        assert!((realized.route.metrics.distance_m - source_length_m / 2.0).abs() < 0.5);
        assert!((realized.route.metrics.ascent_m - 10.0).abs() < 0.1);
        assert!((realized.route.metrics.descent_m - 10.0).abs() < 0.1);
        let anchor = realized.trail.support_points[1].coord();
        assert!(anchor.lat.abs() < 1.0e-10);
        assert!((anchor.lon - 0.001).abs() < 1.0e-8);
        assert!(realized.bindings[1].vertex.0 >= graph.vertices.len());
    }

    #[test]
    fn loop_candidates_recover_exact_support_designs() {
        let graph = graph();
        let constraints = LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: f64::MAX,
            max_repeated_edge_fraction: 0.0,
            allowed_shapes: vec![RouteShape::Loop],
            ..LoopConstraints::default()
        };
        let route = crate::ExactLoopSolver::default()
            .enumerate(&graph, VertexId(0), &constraints, 1)
            .into_iter()
            .next()
            .expect("fixture has a loop");
        let trail = Trail::infer(&graph, &route, RoutingLaw::default())
            .expect("loop has an exact support design");
        let realized = trail
            .realize("recovered", &graph, &constraints, 1.0)
            .expect("realize recovered loop");
        assert_eq!(realized.route.edges, route.edges);
        assert!(trail.support_points.len() >= 2);
    }

    #[test]
    fn non_shortest_candidate_edges_recover_lossless_interior_supports() {
        let a = Coord::new(0.0, 0.0);
        let b = Coord::new(0.002, 0.0);
        let c = Coord::new(0.001, 0.000_4);
        let draft = |name: &str, from: Coord, to: Coord, road_exposure| SegmentDraft {
            geometry: LineString::new(vec![from, to]).expect("valid segment"),
            junctions: JunctionPolicy::Planar,
            turn_ref: None,
            turn_restrictions: Vec::new(),
            trail_class: TrailClass::Path,
            standing: TrailStanding::Established,
            marking: crate::TrailMarking::default(),
            terrain: Terrain::Trail,
            terrain_confidence: Some(1.0),
            surface: Some("dirt".to_owned()),
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure,
            confidence: 1.0,
            provenance: vec![Provenance::fixture(name)],
        };
        let graph = GraphBuilder::default()
            .build(&[
                draft("direct", a, b, 1.0),
                draft("detour-a", a, c, 0.0),
                draft("detour-b", c, b, 0.0),
            ])
            .expect("build triangle");
        let vertex = |coord: Coord| {
            graph
                .vertices
                .iter()
                .find(|vertex| vertex.coord.planar_distance2(coord) < 1.0e-16)
                .expect("coordinate vertex")
                .id
        };
        let edge = |from: VertexId, to: VertexId| {
            graph.adjacency[from.0]
                .iter()
                .copied()
                .find(|edge| graph.edges[edge.0].other(from) == Some(to))
                .expect("adjacent vertices")
        };
        let va = vertex(a);
        let vb = vertex(b);
        let vc = vertex(c);
        let original = vec![edge(va, vb), edge(vb, vc), edge(vc, va)];
        let constraints = LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: f64::MAX,
            max_repeated_edge_fraction: 0.0,
            allowed_shapes: vec![RouteShape::Loop],
            ..LoopConstraints::default()
        };
        let route = Route::from_edges("non-shortest", &graph, va, original, &constraints);
        let trail = Trail::infer(&graph, &route, RoutingLaw::default())
            .expect("non-shortest edge gains an interior support");
        assert!(
            trail.support_points.len() >= 3,
            "interior support should make the direct road edge compulsory"
        );
        let realized = trail
            .realize("recovered", &graph, &constraints, 1.0)
            .expect("realize inferred controls");
        assert_eq!(realized.route.metrics.shape, RouteShape::Loop);
        assert!((realized.route.metrics.distance_m - route.metrics.distance_m).abs() < 0.01);
    }

    #[test]
    fn closed_and_private_edges_are_not_routing_penalties() {
        let mut graph = graph();
        for edge in &mut graph.edges {
            edge.attr.access = Access::Closed;
        }
        assert!(
            shortest_path(
                &graph,
                VertexId(0),
                VertexId(1),
                None,
                RoutingLaw::default(),
                None
            )
            .is_none()
        );
    }

    #[test]
    fn road_aversion_prefers_a_modest_trail_detour_without_banning_roads() {
        let draft = |points, terrain, road_exposure, name| SegmentDraft {
            geometry: LineString::new(points).expect("valid line"),
            junctions: JunctionPolicy::Planar,
            turn_ref: None,
            turn_restrictions: Vec::new(),
            trail_class: TrailClass::Path,
            standing: TrailStanding::Established,
            marking: crate::TrailMarking::default(),
            terrain,
            terrain_confidence: Some(1.0),
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure,
            confidence: 1.0,
            provenance: vec![Provenance::fixture(name)],
        };
        let start_coord = Coord::new(0.0, 0.0);
        let end_coord = Coord::new(0.002, 0.0);
        let graph = GraphBuilder::default()
            .build(&[
                draft(
                    vec![start_coord, end_coord],
                    Terrain::Road,
                    1.0,
                    "short-road",
                ),
                draft(
                    vec![start_coord, Coord::new(0.001, 0.001), end_coord],
                    Terrain::Trail,
                    0.0,
                    "trail-detour",
                ),
            ])
            .expect("build fork");
        let start = graph.nearest_vertex(start_coord).expect("start vertex");
        let end = graph.nearest_vertex(end_coord).expect("end vertex");
        let trail = shortest_path(&graph, start, end, None, RoutingLaw::default(), None)
            .expect("trail detour");
        let road = shortest_path(
            &graph,
            start,
            end,
            None,
            RoutingLaw { road_aversion: 0.0 },
            None,
        )
        .expect("road route");
        assert!(trail.len() > road.len());
        assert_eq!(graph.edges[road[0].0].attr.terrain, Terrain::Road);
        assert!(
            trail
                .iter()
                .all(|edge| graph.edges[edge.0].attr.terrain == Terrain::Trail)
        );
    }
}
