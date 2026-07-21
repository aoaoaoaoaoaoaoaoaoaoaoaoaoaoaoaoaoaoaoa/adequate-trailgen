use crate::{
    Access, Coord, EdgeId, LoopConstraints, Route, RouteShape, Terrain, TrailGraph, TrailgenError,
    VertexId,
};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap},
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

    /// Recovers a compact support design when shortest lawful legs reproduce
    /// the candidate exactly. An irreducible parallel-edge ambiguity returns
    /// `None` rather than attaching dishonest controls to the route.
    #[must_use]
    pub fn infer(graph: &TrailGraph, route: &Route, routing: RoutingLaw) -> Option<Self> {
        routing.validate().ok()?;
        let vertices = route_vertices(graph, route)?;
        let n = route.edges.len();
        if n == 0 {
            return None;
        }
        let indices = match route.metrics.shape {
            RouteShape::OutAndBack => {
                let split = n / 2;
                (n.is_multiple_of(2)
                    && route.edges[..split]
                        == route.edges[split..]
                            .iter()
                            .rev()
                            .copied()
                            .collect::<Vec<_>>()
                    && shortest_path(graph, route.start, vertices[split], None, routing, None)?
                        == route.edges[..split])
                    .then_some(vec![0, split])?
            }
            RouteShape::Loop => {
                if vertices[n] != route.start || n < 2 {
                    return None;
                }
                let mut indices = Vec::new();
                let split = n / 2;
                compress_arc(graph, route, &vertices, routing, 0, split, &mut indices)?;
                compress_arc(graph, route, &vertices, routing, split, n, &mut indices)?;
                indices
            }
            RouteShape::Open => {
                let mut indices = Vec::new();
                compress_arc(graph, route, &vertices, routing, 0, n, &mut indices)?;
                indices.push(n);
                indices
            }
            RouteShape::FigureEight => return None,
        };
        let points = indices
            .into_iter()
            .map(|index| SupportPoint::forge(graph.vertices[vertices[index].0].coord))
            .collect::<Option<Vec<_>>>()?;
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
        let bindings = self
            .support_points
            .iter()
            .map(|point| bind(graph, *point, max_snap_m))
            .collect::<crate::Result<Vec<_>>>()?;
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
            trail: self.clone(),
            bindings,
            route,
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
    supports: &mut Vec<usize>,
) -> Option<()> {
    let previous = lo.checked_sub(1).map(|index| route.edges[index]);
    if shortest_path(graph, vertices[lo], vertices[hi], previous, routing, None)?
        == route.edges[lo..hi]
    {
        supports.push(lo);
        return Some(());
    }
    if hi - lo <= 1 {
        return None;
    }
    let split = lo + (hi - lo) / 2;
    compress_arc(graph, route, vertices, routing, lo, split, supports)?;
    compress_arc(graph, route, vertices, routing, split, hi, supports)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportBinding {
    pub requested: SupportPoint,
    pub vertex: VertexId,
    pub snap_m: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrailRealization {
    pub trail: Trail,
    pub bindings: Vec<SupportBinding>,
    pub route: Route,
}

fn bind(graph: &TrailGraph, point: SupportPoint, max_snap_m: f64) -> crate::Result<SupportBinding> {
    let (vertex, snap_m) = graph
        .nearest_vertex_with_distance(point.coord())
        .ok_or_else(|| {
            TrailgenError::InvalidData("cannot bind a support point to an empty network".to_owned())
        })?;
    if snap_m > max_snap_m {
        return Err(TrailgenError::InvalidData(format!(
            "support point lies {snap_m:.0} m from the trail network"
        )));
    }
    Ok(SupportBinding {
        requested: point,
        vertex,
        snap_m,
    })
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
