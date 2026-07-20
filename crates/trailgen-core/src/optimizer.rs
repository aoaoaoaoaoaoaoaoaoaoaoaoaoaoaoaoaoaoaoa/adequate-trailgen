use crate::RouteShape;
use crate::constraints::LoopConstraints;
use crate::model::{EdgeId, TrailGraph, VertexId};
use crate::route::{Route, rank_routes};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchParams {
    pub max_hops: usize,
    pub max_frontier: usize,
    pub keep: usize,
    #[serde(default = "default_closure_paths")]
    pub closure_paths: usize,
    #[serde(default)]
    pub seed: u64,
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
        match self.resolve(graph) {
            Self::Auto => unreachable!("auto solver must resolve to a concrete backend"),
            Self::Heuristic => LoopHunter { params }.solve(graph, start, constraints, count),
            Self::Exact => ExactLoopSolver { params }.solve(graph, start, constraints, count),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LoopHunter {
    pub params: SearchParams,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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
                for return_edges in shortest_return_paths(
                    graph,
                    state.at,
                    start,
                    state.edges.last().copied(),
                    &state.used,
                    max_return_m,
                    self.params.closure_paths,
                ) {
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
            );

            for edge_id in fanout {
                if state.used.contains(&edge_id) {
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

        finish_routes(routes, constraints, count, self.params.keep)
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

        finish_routes(routes, constraints, count, self.params.keep)
    }
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
    constraints: &LoopConstraints,
    count: usize,
    keep: usize,
) -> Vec<Route> {
    let mut seen = BTreeSet::new();
    routes.retain(|route| seen.insert(route_signature(route)));
    rank_routes(&mut routes, constraints);
    routes.truncate(count.max(1).min(keep));
    for (i, route) in routes.iter_mut().enumerate() {
        route.name = format!("candidate-{}", i + 1);
    }
    routes
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

fn shortest_return_paths(
    graph: &TrailGraph,
    from: VertexId,
    target: VertexId,
    previous: Option<EdgeId>,
    banned_edges: &BTreeSet<EdgeId>,
    max_distance_m: f64,
    keep: usize,
) -> Vec<Vec<EdgeId>> {
    if max_distance_m < 0.0 {
        return Vec::new();
    }
    let keep = keep.max(1);
    let expansion_cap = keep
        .saturating_mul(graph.edges.len().max(1))
        .saturating_mul(8)
        .max(64);
    let mut heap = BinaryHeap::new();
    heap.push(ReturnPathState {
        cost_m: 0.0,
        at: from,
        previous,
        edges: Vec::new(),
        used: banned_edges.clone(),
    });
    let mut expanded = 0usize;
    let mut paths = Vec::new();

    while let Some(state) = heap.pop() {
        if paths.len() >= keep || expanded >= expansion_cap {
            break;
        }
        expanded += 1;
        if state.at == target && !state.edges.is_empty() {
            paths.push(state.edges);
            continue;
        }
        for edge_id in &graph.adjacency[state.at.0] {
            if state.used.contains(edge_id) {
                continue;
            }
            if !graph.turn_allowed(state.previous, state.at, *edge_id) {
                continue;
            }
            let edge = &graph.edges[edge_id.0];
            let Some(next) = edge.traverse(state.at) else {
                continue;
            };
            let next_cost_m = state.cost_m + edge.attr.length_m;
            if next_cost_m > max_distance_m {
                continue;
            }
            let mut edges = state.edges.clone();
            edges.push(*edge_id);
            let mut used = state.used.clone();
            used.insert(*edge_id);
            heap.push(ReturnPathState {
                cost_m: next_cost_m,
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
) {
    fanout.sort_by(|a, b| {
        branch_score(graph, *b, seed, depth, at)
            .total_cmp(&branch_score(graph, *a, seed, depth, at))
            .then_with(|| b.cmp(a))
    });
}

fn branch_score(graph: &TrailGraph, edge_id: EdgeId, seed: u64, depth: usize, at: VertexId) -> f64 {
    graph.edges[edge_id.0]
        .attr
        .difficulty
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
    cost_m: f64,
    at: VertexId,
    previous: Option<EdgeId>,
    edges: Vec<EdgeId>,
    used: BTreeSet<EdgeId>,
}

impl Eq for ReturnPathState {}

impl Ord for ReturnPathState {
    fn cmp(&self, rhs: &Self) -> Ordering {
        rhs.cost_m
            .total_cmp(&self.cost_m)
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
