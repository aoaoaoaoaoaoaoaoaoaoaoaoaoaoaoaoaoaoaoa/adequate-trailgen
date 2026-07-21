use crate::RouteShape;
use crate::constraints::LoopConstraints;
use crate::model::{EdgeId, TrailGraph, VertexId};
use crate::route::{Route, rank_routes};
use crate::trail::RoutingLaw;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchParams {
    pub max_hops: usize,
    pub max_frontier: usize,
    pub keep: usize,
    #[serde(default = "default_closure_paths")]
    pub closure_paths: usize,
    #[serde(default)]
    pub seed: u64,
    #[serde(default)]
    pub routing: RoutingLaw,
}

const fn default_closure_paths() -> usize {
    4
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            max_hops: 256,
            max_frontier: 200_000,
            keep: 12,
            closure_paths: default_closure_paths(),
            seed: 0,
            routing: RoutingLaw::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SolverKind {
    #[default]
    Auto,
    Heuristic,
    Exact,
}

impl SolverKind {
    const AUTO_EXACT_EDGE_LIMIT: usize = 32;

    #[must_use]
    pub const fn resolve(self, graph: &TrailGraph) -> Self {
        match self {
            Self::Auto if graph.edges.len() <= Self::AUTO_EXACT_EDGE_LIMIT => Self::Exact,
            Self::Auto => Self::Heuristic,
            resolved => resolved,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Heuristic => "loop-hunter",
            Self::Exact => "exact-enumerator",
        }
    }

    #[must_use]
    pub fn solve(
        self,
        params: SearchParams,
        graph: &TrailGraph,
        start: VertexId,
        constraints: &LoopConstraints,
        count: usize,
    ) -> Vec<Route> {
        if constraints.allowed_shapes.as_slice() == [RouteShape::OutAndBack] {
            return support_out_and_backs(params, graph, start, constraints, count);
        }
        match self.resolve(graph) {
            Self::Auto => unreachable!("auto solver must resolve to a concrete backend"),
            Self::Heuristic => LoopHunter { params }.solve(graph, start, constraints, count),
            Self::Exact => ExactLoopSolver { params }.solve(graph, start, constraints, count),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LoopHunter {
    pub params: SearchParams,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExactLoopSolver {
    pub params: SearchParams,
}

#[derive(Clone)]
struct State {
    at: VertexId,
    edges: Vec<EdgeId>,
    used: BTreeSet<EdgeId>,
    distance_m: f64,
}

pub trait RouteSolver {
    fn solve(
        &self,
        graph: &TrailGraph,
        start: VertexId,
        constraints: &LoopConstraints,
        count: usize,
    ) -> Vec<Route>;
}

impl LoopHunter {
    #[must_use]
    pub fn hunt(
        self,
        graph: &TrailGraph,
        start: VertexId,
        constraints: &LoopConstraints,
        count: usize,
    ) -> Vec<Route> {
        self.solve(graph, start, constraints, count)
    }
}

impl RouteSolver for LoopHunter {
    fn solve(
        &self,
        graph: &TrailGraph,
        start: VertexId,
        constraints: &LoopConstraints,
        count: usize,
    ) -> Vec<Route> {
        let mut stack = vec![State {
            at: start,
            edges: Vec::new(),
            used: BTreeSet::new(),
            distance_m: 0.0,
        }];
        let mut routes = Vec::<Route>::new();
        let mut expanded = 0usize;

        while let Some(state) = stack.pop() {
            expanded += 1;
            if expanded > self.params.max_frontier {
                break;
            }
            if closes_allowed(constraints) && state.at != start && !state.edges.is_empty() {
                let max_return_m = constraints.max_distance_m.mul_add(1.35, -state.distance_m);
                for return_edges in shortest_return_paths(ReturnHunt {
                    graph,
                    from: state.at,
                    target: start,
                    previous: state.edges.last().copied(),
                    barred: &state.used,
                    max_distance_m: max_return_m,
                    keep: self.params.closure_paths,
                    law: self.params.routing,
                }) {
                    let mut route_edges = state.edges.clone();
                    route_edges.extend(return_edges);
                    push_allowed_route(&mut routes, graph, start, route_edges, constraints);
                }
            }
            if state.edges.len() >= self.params.max_hops {
                continue;
            }
            let mut fanout = graph.adjacency[state.at.0].clone();
            sort_heuristic_fanout(
                graph,
                &mut fanout,
                self.params.seed,
                state.edges.len(),
                state.at,
                self.params.routing,
            );

            for edge_id in fanout {
                if state.used.contains(&edge_id) {
                    continue;
                }
                if self.params.routing.edge_cost(graph, edge_id).is_none() {
                    continue;
                }
                if !graph.turn_allowed(state.edges.last().copied(), state.at, edge_id) {
                    continue;
                }
                let edge = &graph.edges[edge_id.0];
                let Some(next) = edge.traverse(state.at) else {
                    continue;
                };
                let distance_m = state.distance_m + edge.attr.length_m;
                if distance_m > constraints.max_distance_m * 1.35 {
                    continue;
                }
                let mut edges = state.edges.clone();
                edges.push(edge_id);
                if constraints.allows_shape(RouteShape::OutAndBack) {
                    let out_and_back = mirrored_route(&edges);
                    if route_distance(graph, &out_and_back) <= constraints.max_distance_m * 1.35 {
                        push_allowed_route(&mut routes, graph, start, out_and_back, constraints);
                    }
                }

                let mut used = state.used.clone();
                used.insert(edge_id);
                if next == start && edges.len() >= 2 {
                    push_allowed_route(&mut routes, graph, start, edges.clone(), constraints);
                    if constraints.allows_shape(RouteShape::FigureEight) {
                        stack.push(State {
                            at: next,
                            edges,
                            used,
                            distance_m,
                        });
                    }
                    continue;
                }

                stack.push(State {
                    at: next,
                    edges,
                    used,
                    distance_m,
                });
            }
        }

        finish_routes(routes, graph, constraints, count, self.params.keep)
    }
}

impl ExactLoopSolver {
    #[must_use]
    pub fn enumerate(
        self,
        graph: &TrailGraph,
        start: VertexId,
        constraints: &LoopConstraints,
        count: usize,
    ) -> Vec<Route> {
        self.solve(graph, start, constraints, count)
    }
}

impl RouteSolver for ExactLoopSolver {
    fn solve(
        &self,
        graph: &TrailGraph,
        start: VertexId,
        constraints: &LoopConstraints,
        count: usize,
    ) -> Vec<Route> {
        let mut stack = vec![State {
            at: start,
            edges: Vec::new(),
            used: BTreeSet::new(),
            distance_m: 0.0,
        }];
        let mut routes = Vec::<Route>::new();
        let mut expanded = 0usize;

        while let Some(state) = stack.pop() {
            expanded += 1;
            if expanded > self.params.max_frontier {
                break;
            }
            if state.edges.len() >= self.params.max_hops {
                continue;
            }

            let mut fanout = graph.adjacency[state.at.0].clone();
            fanout.sort();
            fanout.reverse();

            for edge_id in fanout {
                if state.used.contains(&edge_id) {
                    continue;
                }
                if self.params.routing.edge_cost(graph, edge_id).is_none() {
                    continue;
                }
                if !graph.turn_allowed(state.edges.last().copied(), state.at, edge_id) {
                    continue;
                }
                let edge = &graph.edges[edge_id.0];
                let Some(next) = edge.traverse(state.at) else {
                    continue;
                };
                let distance_m = state.distance_m + edge.attr.length_m;
                if distance_m > constraints.max_distance_m * 1.35 {
                    continue;
                }

                let mut edges = state.edges.clone();
                edges.push(edge_id);
                if constraints.allows_shape(RouteShape::OutAndBack) {
                    let out_and_back = mirrored_route(&edges);
                    if route_distance(graph, &out_and_back) <= constraints.max_distance_m * 1.35 {
                        push_allowed_route(&mut routes, graph, start, out_and_back, constraints);
                    }
                }

                let mut used = state.used.clone();
                used.insert(edge_id);
                if next == start && edges.len() >= 2 {
                    push_allowed_route(&mut routes, graph, start, edges.clone(), constraints);
                    if !constraints.allows_shape(RouteShape::FigureEight) {
                        continue;
                    }
                }

                stack.push(State {
                    at: next,
                    edges,
                    used,
                    distance_m,
                });
            }
        }

        finish_routes(routes, graph, constraints, count, self.params.keep)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SupportWalk {
    at: VertexId,
    previous: Option<EdgeId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SupportFrontier {
    cost: f64,
    walk: SupportWalk,
}

impl Eq for SupportFrontier {}

impl Ord for SupportFrontier {
    fn cmp(&self, rhs: &Self) -> Ordering {
        rhs.cost
            .total_cmp(&self.cost)
            .then_with(|| rhs.walk.cmp(&self.walk))
    }
}

impl PartialOrd for SupportFrontier {
    fn partial_cmp(&self, rhs: &Self) -> Option<Ordering> {
        Some(self.cmp(rhs))
    }
}

fn support_out_and_backs(
    params: SearchParams,
    graph: &TrailGraph,
    start: VertexId,
    constraints: &LoopConstraints,
    count: usize,
) -> Vec<Route> {
    let law = params.routing;
    let origin = SupportWalk {
        at: start,
        previous: None,
    };
    let mut frontier = BinaryHeap::from([SupportFrontier {
        cost: 0.0,
        walk: origin,
    }]);
    let mut distance = BTreeMap::from([(origin, 0.0)]);
    let mut predecessor = BTreeMap::<SupportWalk, (SupportWalk, EdgeId)>::new();
    let mut emitted = BTreeSet::new();
    let mut routes = Vec::new();
    let mut expanded = 0usize;
    let maximum_outward_m = constraints.max_distance_m * 0.675;
    let maximum_cost = maximum_outward_m * (1.0 + law.road_aversion);

    while let Some(SupportFrontier { cost, walk }) = frontier.pop() {
        if expanded >= params.max_frontier || cost > maximum_cost {
            break;
        }
        if distance
            .get(&walk)
            .is_some_and(|best| cost > *best + f64::EPSILON)
        {
            continue;
        }
        expanded += 1;
        let path = support_path(origin, walk, &predecessor);
        let outward_m = route_distance(graph, &path);
        if walk.at != start
            && emitted.insert(walk.at)
            && !path.is_empty()
            && path.len() <= params.max_hops
            && outward_m <= maximum_outward_m
            && path.iter().copied().collect::<BTreeSet<_>>().len() == path.len()
        {
            push_allowed_route(
                &mut routes,
                graph,
                start,
                mirrored_route(&path),
                constraints,
            );
        }
        if path.len() >= params.max_hops || outward_m > maximum_outward_m {
            continue;
        }
        for edge in graph.adjacency[walk.at.0].iter().copied() {
            if !graph.turn_allowed(walk.previous, walk.at, edge) {
                continue;
            }
            let Some(edge_cost) = law.edge_cost(graph, edge) else {
                continue;
            };
            let next_cost = cost + edge_cost;
            if next_cost > maximum_cost {
                continue;
            }
            let Some(at) = graph.edges[edge.0].traverse(walk.at) else {
                continue;
            };
            let next = SupportWalk {
                at,
                previous: Some(edge),
            };
            if distance
                .get(&next)
                .is_none_or(|best| next_cost < *best - f64::EPSILON)
            {
                distance.insert(next, next_cost);
                predecessor.insert(next, (walk, edge));
                frontier.push(SupportFrontier {
                    cost: next_cost,
                    walk: next,
                });
            }
        }
    }
    finish_routes(routes, graph, constraints, count, params.keep)
}

fn support_path(
    origin: SupportWalk,
    mut cursor: SupportWalk,
    predecessor: &BTreeMap<SupportWalk, (SupportWalk, EdgeId)>,
) -> Vec<EdgeId> {
    let mut edges = Vec::new();
    while cursor != origin {
        let Some((prior, edge)) = predecessor.get(&cursor).copied() else {
            return Vec::new();
        };
        edges.push(edge);
        cursor = prior;
    }
    edges.reverse();
    edges
}

fn closes_allowed(constraints: &LoopConstraints) -> bool {
    constraints.allows_shape(RouteShape::Loop) || constraints.allows_shape(RouteShape::FigureEight)
}

fn push_allowed_route(
    routes: &mut Vec<Route>,
    graph: &TrailGraph,
    start: VertexId,
    edges: Vec<EdgeId>,
    constraints: &LoopConstraints,
) {
    if graph.walk_edges(start, &edges).is_none() {
        return;
    }
    let route = Route::from_edges(
        format!("candidate-{}", routes.len() + 1),
        graph,
        start,
        edges,
        constraints,
    );
    if constraints.allows_shape(route.metrics.shape) {
        routes.push(route);
    }
}

fn finish_routes(
    mut routes: Vec<Route>,
    graph: &TrailGraph,
    constraints: &LoopConstraints,
    count: usize,
    keep: usize,
) -> Vec<Route> {
    let mut seen = BTreeSet::new();
    routes.retain(|route| seen.insert(route_signature(route)));
    rank_routes(&mut routes, constraints);
    routes = diverse_portfolio(routes, graph, graphless_limit(count, keep));
    for (i, route) in routes.iter_mut().enumerate() {
        route.name = format!("candidate-{}", i + 1);
    }
    routes
}

const fn graphless_limit(count: usize, keep: usize) -> usize {
    if count < 1 {
        1
    } else if count < keep {
        count
    } else {
        keep
    }
}

fn diverse_portfolio(routes: Vec<Route>, graph: &TrailGraph, limit: usize) -> Vec<Route> {
    if routes.len() <= limit {
        return routes;
    }
    let tier = routes
        .iter()
        .position(|route| !route.verdict.satisfied)
        .unwrap_or(routes.len());
    let mut misses = routes;
    let near = misses.split_off(tier);
    let mut chosen = Vec::with_capacity(limit);
    admit_diverse_tier(misses, graph, limit, &mut chosen);
    admit_diverse_tier(near, graph, limit, &mut chosen);
    chosen
}

fn admit_diverse_tier(
    routes: Vec<Route>,
    graph: &TrailGraph,
    limit: usize,
    chosen: &mut Vec<Route>,
) {
    let mut pool = routes.into_iter().map(Some).collect::<Vec<_>>();
    for exclusion_radius in [0.35, 0.20, 0.08, 0.0] {
        for candidate in &mut pool {
            if chosen.len() >= limit {
                break;
            }
            let admit = candidate.as_ref().is_some_and(|route| {
                chosen
                    .iter()
                    .all(|known| route_distance_between(graph, route, known) >= exclusion_radius)
            });
            if admit {
                chosen.push(candidate.take().expect("admitted route exists"));
            }
        }
    }
}

fn route_distance_between(graph: &TrailGraph, left: &Route, right: &Route) -> f64 {
    let counts = |route: &Route| {
        let mut counts = BTreeMap::<EdgeId, u32>::new();
        for edge in &route.edges {
            let count = counts.entry(*edge).or_default();
            *count = count
                .checked_add(1)
                .expect("a route has fewer than 2³² legs");
        }
        counts
    };
    let left_counts = counts(left);
    let right_counts = counts(right);
    let shared_m = left_counts
        .iter()
        .map(|(edge, count)| {
            graph.edges[edge.0].attr.length_m
                * f64::from(*count.min(right_counts.get(edge).unwrap_or(&0)))
        })
        .sum::<f64>();
    let basis_m = left
        .metrics
        .distance_m
        .min(right.metrics.distance_m)
        .max(1.0);
    1.0 - shared_m / basis_m
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct RouteSignature {
    shape: RouteShape,
    edge_counts: Vec<(EdgeId, usize)>,
}

fn route_signature(route: &Route) -> RouteSignature {
    let mut edge_counts = BTreeMap::<EdgeId, usize>::new();
    for edge in &route.edges {
        *edge_counts.entry(*edge).or_default() += 1;
    }
    RouteSignature {
        shape: route.metrics.shape,
        edge_counts: edge_counts.into_iter().collect(),
    }
}

fn route_distance(graph: &TrailGraph, edges: &[EdgeId]) -> f64 {
    edges
        .iter()
        .map(|edge_id| graph.edges[edge_id.0].attr.length_m)
        .sum()
}

#[derive(Clone, Copy)]
struct ReturnHunt<'a> {
    graph: &'a TrailGraph,
    from: VertexId,
    target: VertexId,
    previous: Option<EdgeId>,
    barred: &'a BTreeSet<EdgeId>,
    max_distance_m: f64,
    keep: usize,
    law: RoutingLaw,
}

fn shortest_return_paths(hunt: ReturnHunt<'_>) -> Vec<Vec<EdgeId>> {
    if hunt.max_distance_m < 0.0 {
        return Vec::new();
    }
    let keep = hunt.keep.max(1);
    let expansion_cap = keep
        .saturating_mul(hunt.graph.edges.len().max(1))
        .saturating_mul(8)
        .max(64);
    let mut heap = BinaryHeap::new();
    heap.push(ReturnPathState {
        routing_cost_m: 0.0,
        distance_m: 0.0,
        at: hunt.from,
        previous: hunt.previous,
        edges: Vec::new(),
        used: hunt.barred.clone(),
    });
    let mut expanded = 0usize;
    let mut paths = Vec::new();

    while let Some(state) = heap.pop() {
        if paths.len() >= keep || expanded >= expansion_cap {
            break;
        }
        expanded += 1;
        if state.at == hunt.target && !state.edges.is_empty() {
            paths.push(state.edges);
            continue;
        }
        for edge_id in &hunt.graph.adjacency[state.at.0] {
            if state.used.contains(edge_id) {
                continue;
            }
            if !hunt.graph.turn_allowed(state.previous, state.at, *edge_id) {
                continue;
            }
            let Some(edge_cost) = hunt.law.edge_cost(hunt.graph, *edge_id) else {
                continue;
            };
            let edge = &hunt.graph.edges[edge_id.0];
            let Some(next) = edge.traverse(state.at) else {
                continue;
            };
            let distance_m = state.distance_m + edge.attr.length_m;
            if distance_m > hunt.max_distance_m {
                continue;
            }
            let mut edges = state.edges.clone();
            edges.push(*edge_id);
            let mut used = state.used.clone();
            used.insert(*edge_id);
            heap.push(ReturnPathState {
                routing_cost_m: state.routing_cost_m + edge_cost,
                distance_m,
                at: next,
                previous: Some(*edge_id),
                edges,
                used,
            });
        }
    }
    paths
}

fn sort_heuristic_fanout(
    graph: &TrailGraph,
    fanout: &mut [EdgeId],
    seed: u64,
    depth: usize,
    at: VertexId,
    law: RoutingLaw,
) {
    fanout.sort_by(|a, b| {
        branch_score(graph, *b, seed, depth, at, law)
            .total_cmp(&branch_score(graph, *a, seed, depth, at, law))
            .then_with(|| b.cmp(a))
    });
}

fn branch_score(
    graph: &TrailGraph,
    edge_id: EdgeId,
    seed: u64,
    depth: usize,
    at: VertexId,
    law: RoutingLaw,
) -> f64 {
    let edge = &graph.edges[edge_id.0];
    let road_detour_km = law
        .edge_cost(graph, edge_id)
        .map_or(f64::MAX, |cost| (cost - edge.attr.length_m) / 1_000.0);
    (edge.attr.difficulty + road_detour_km)
        .mul_add(1_024.0, seeded_unit(seed, depth, at, edge_id) * 128.0)
}

fn seeded_unit(seed: u64, depth: usize, at: VertexId, edge_id: EdgeId) -> f64 {
    let hash = splitmix64(seed ^ ((depth as u64) << 48) ^ ((at.0 as u64) << 24) ^ edge_id.0 as u64);
    let bits = u32::try_from(hash >> 32).expect("shifted splitmix output fits in u32");
    f64::from(bits) * (1.0 / f64::from(u32::MAX))
}

const fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

#[derive(Clone, Debug, PartialEq)]
struct ReturnPathState {
    routing_cost_m: f64,
    distance_m: f64,
    at: VertexId,
    previous: Option<EdgeId>,
    edges: Vec<EdgeId>,
    used: BTreeSet<EdgeId>,
}

impl Eq for ReturnPathState {}

impl Ord for ReturnPathState {
    fn cmp(&self, rhs: &Self) -> Ordering {
        rhs.routing_cost_m
            .total_cmp(&self.routing_cost_m)
            .then_with(|| rhs.distance_m.total_cmp(&self.distance_m))
            .then_with(|| rhs.at.cmp(&self.at))
            .then_with(|| rhs.edges.cmp(&self.edges))
    }
}

impl PartialOrd for ReturnPathState {
    fn partial_cmp(&self, rhs: &Self) -> Option<Ordering> {
        Some(self.cmp(rhs))
    }
}

fn mirrored_route(edges: &[EdgeId]) -> Vec<EdgeId> {
    edges
        .iter()
        .copied()
        .chain(edges.iter().rev().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Access, Coord, EdgeTravel, GraphBuilder, JunctionPolicy, LineString, Provenance,
        SegmentDraft, Terrain, TrailClass, TrailStanding,
    };

    fn branch(points: Vec<Coord>, name: &str) -> SegmentDraft {
        SegmentDraft {
            geometry: LineString::new(points).expect("valid branch"),
            junctions: JunctionPolicy::Planar,
            turn_ref: None,
            turn_restrictions: Vec::new(),
            trail_class: TrailClass::Path,
            standing: TrailStanding::Established,
            terrain: Terrain::Trail,
            terrain_confidence: Some(1.0),
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: vec![Provenance::fixture(name)],
        }
    }

    #[test]
    fn out_and_back_portfolio_spends_slots_on_distinct_spines_first() {
        let origin = Coord::new(0.0, 0.0);
        let graph = GraphBuilder::default()
            .build(&[
                branch(
                    vec![
                        origin,
                        Coord::new(0.001, 0.0),
                        Coord::new(0.002, 0.0),
                        Coord::new(0.003, 0.0),
                    ],
                    "east",
                ),
                branch(
                    vec![
                        origin,
                        Coord::new(0.0, 0.001),
                        Coord::new(0.0, 0.002),
                        Coord::new(0.0, 0.003),
                    ],
                    "north",
                ),
                branch(
                    vec![
                        origin,
                        Coord::new(-0.001, 0.0),
                        Coord::new(-0.002, 0.0),
                        Coord::new(-0.003, 0.0),
                    ],
                    "west",
                ),
            ])
            .expect("build branching graph");
        let start = graph.nearest_vertex(origin).expect("origin vertex");
        let constraints = LoopConstraints {
            min_distance_m: 200.0,
            max_distance_m: 800.0,
            max_difficulty: f64::MAX,
            max_repeated_edge_fraction: 1.0,
            allowed_shapes: vec![RouteShape::OutAndBack],
            ..LoopConstraints::default()
        };
        let routes = SolverKind::Auto.solve(
            SearchParams {
                max_hops: 4,
                max_frontier: 100,
                keep: 12,
                closure_paths: 1,
                seed: 0,
                routing: RoutingLaw::default(),
            },
            &graph,
            start,
            &constraints,
            3,
        );
        assert_eq!(routes.len(), 3);
        assert_eq!(
            routes
                .iter()
                .map(|route| route.edges[0])
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn diversity_never_spends_a_slot_on_a_near_miss_while_matches_remain() {
        let origin = Coord::new(0.0, 0.0);
        let graph = GraphBuilder::default()
            .build(&[
                branch(
                    vec![
                        origin,
                        Coord::new(0.001, 0.0),
                        Coord::new(0.002, 0.0),
                        Coord::new(0.003, 0.0),
                    ],
                    "exact-spine",
                ),
                branch(vec![origin, Coord::new(0.0, 0.0004)], "short-near-miss"),
            ])
            .expect("build tiered graph");
        let start = graph.nearest_vertex(origin).expect("origin vertex");
        let constraints = LoopConstraints {
            min_distance_m: 200.0,
            max_distance_m: 800.0,
            max_difficulty: f64::MAX,
            max_repeated_edge_fraction: 1.0,
            allowed_shapes: vec![RouteShape::OutAndBack],
            ..LoopConstraints::default()
        };
        let routes = SolverKind::Auto.solve(
            SearchParams {
                keep: 12,
                max_frontier: 100,
                ..SearchParams::default()
            },
            &graph,
            start,
            &constraints,
            3,
        );
        assert_eq!(routes.len(), 3);
        assert!(routes.iter().all(|route| route.verdict.satisfied));
    }
}
