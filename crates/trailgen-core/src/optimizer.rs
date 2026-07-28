use crate::Coord;
use crate::RouteShape;
use crate::constraints::LoopConstraints;
use crate::model::{EdgeId, EdgeTravel, TrailGraph, VertexId};
use crate::route::{Route, rank_routes};
use crate::trail::RoutingLaw;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchStage {
    Preparing,
    Exploring,
    Ranking,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchProgress {
    pub stage: SearchStage,
    pub explored: usize,
    pub limit: usize,
    pub candidates: usize,
}

pub trait SearchMonitor {
    fn cancelled(&self) -> bool;
    fn report(&self, progress: SearchProgress);
}

impl SearchMonitor for () {
    fn cancelled(&self) -> bool {
        false
    }

    fn report(&self, _progress: SearchProgress) {}
}

#[derive(Clone, Copy)]
pub struct SearchScope<'a> {
    graph: &'a TrailGraph,
    allowed: Option<&'a [bool]>,
    edge_count: usize,
}

impl<'a> SearchScope<'a> {
    #[must_use]
    pub const fn all(graph: &'a TrailGraph) -> Self {
        Self {
            graph,
            allowed: None,
            edge_count: graph.edges.len(),
        }
    }

    #[must_use]
    pub fn restricted(graph: &'a TrailGraph, allowed: &'a [bool]) -> Self {
        assert_eq!(
            allowed.len(),
            graph.edges.len(),
            "search mask must cover every graph edge"
        );
        Self {
            graph,
            allowed: Some(allowed),
            edge_count: allowed.iter().filter(|allowed| **allowed).count(),
        }
    }

    fn fanout(self, vertex: VertexId) -> Vec<EdgeId> {
        self.graph.adjacency[vertex.0]
            .iter()
            .copied()
            .filter(|edge| self.allowed.is_none_or(|allowed| allowed[edge.0]))
            .collect()
    }

    fn allows(self, edge: EdgeId) -> bool {
        self.allowed.is_none_or(|allowed| allowed[edge.0])
    }
}

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
        self.resolve_edge_count(graph.edges.len())
    }

    const fn resolve_edge_count(self, edge_count: usize) -> Self {
        match self {
            Self::Auto if edge_count <= Self::AUTO_EXACT_EDGE_LIMIT => Self::Exact,
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
        self.solve_monitored(params, graph, start, constraints, count, &())
    }

    #[must_use]
    pub fn solve_monitored(
        self,
        params: SearchParams,
        graph: &TrailGraph,
        start: VertexId,
        constraints: &LoopConstraints,
        count: usize,
        monitor: &dyn SearchMonitor,
    ) -> Vec<Route> {
        self.solve_scoped(
            params,
            SearchScope::all(graph),
            start,
            constraints,
            count,
            monitor,
        )
    }

    #[must_use]
    pub fn solve_scoped(
        self,
        params: SearchParams,
        scope: SearchScope<'_>,
        start: VertexId,
        constraints: &LoopConstraints,
        count: usize,
        monitor: &dyn SearchMonitor,
    ) -> Vec<Route> {
        if constraints.allowed_shapes.as_slice() == [RouteShape::OutAndBack] {
            return support_out_and_backs(params, scope, start, constraints, count, monitor);
        }
        match self.resolve_edge_count(scope.edge_count) {
            Self::Auto => unreachable!("auto solver must resolve to a concrete backend"),
            Self::Heuristic => {
                LoopHunter { params }.solve_monitored(scope, start, constraints, count, monitor)
            }
            Self::Exact => ExactLoopSolver { params }.solve_monitored(
                scope,
                start,
                constraints,
                count,
                monitor,
            ),
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

struct SearchMeter<'a> {
    monitor: &'a dyn SearchMonitor,
    explored: usize,
    limit: usize,
    reported: usize,
}

impl<'a> SearchMeter<'a> {
    const REPORT_STRIDE: usize = 32;

    fn new(monitor: &'a dyn SearchMonitor, limit: usize) -> Self {
        Self {
            monitor,
            explored: 0,
            limit,
            reported: 0,
        }
    }

    fn advance(&mut self, candidates: usize) -> bool {
        if self.explored >= self.limit || self.monitor.cancelled() {
            return false;
        }
        self.explored += 1;
        if self.explored == 1 || self.explored - self.reported >= Self::REPORT_STRIDE {
            self.emit(SearchStage::Exploring, candidates);
        }
        true
    }

    fn finish(&mut self, candidates: usize) {
        self.emit(SearchStage::Ranking, candidates);
    }

    fn emit(&mut self, stage: SearchStage, candidates: usize) {
        self.reported = self.explored;
        self.monitor.report(SearchProgress {
            stage,
            explored: self.explored,
            limit: self.limit,
            candidates,
        });
    }
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

    fn solve_monitored(
        &self,
        scope: SearchScope<'_>,
        start: VertexId,
        constraints: &LoopConstraints,
        count: usize,
        monitor: &dyn SearchMonitor,
    ) -> Vec<Route> {
        if constraints.allowed_shapes.as_slice() == [RouteShape::Loop] {
            return support_loop_portfolio(self.params, scope, start, constraints, count, monitor);
        }
        let graph = scope.graph;
        let mut stack = vec![State {
            at: start,
            edges: Vec::new(),
            used: BTreeSet::new(),
            distance_m: 0.0,
        }];
        let mut routes = Vec::<Route>::new();
        let mut meter = SearchMeter::new(monitor, self.params.max_frontier);
        let Some(mut closer) = LoopCloser::forge(scope, start, self.params, constraints, monitor)
        else {
            return Vec::new();
        };

        while let Some(state) = stack.pop() {
            if !meter.advance(routes.len()) {
                break;
            }
            closer.strike(&state, constraints, &mut routes);
            if state.edges.len() >= self.params.max_hops {
                continue;
            }
            let mut fanout = scope.fanout(state.at);
            sort_heuristic_fanout(
                graph,
                &mut fanout,
                self.params.seed,
                state.edges.len(),
                state.at,
                self.params.routing,
            );

            for edge_id in fanout {
                if monitor.cancelled() {
                    return Vec::new();
                }
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

        if monitor.cancelled() {
            return Vec::new();
        }
        meter.finish(routes.len());
        finish_routes(routes, graph, constraints, count, self.params.keep, monitor)
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
        self.solve_monitored(SearchScope::all(graph), start, constraints, count, &())
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

    fn solve_monitored(
        &self,
        scope: SearchScope<'_>,
        start: VertexId,
        constraints: &LoopConstraints,
        count: usize,
        monitor: &dyn SearchMonitor,
    ) -> Vec<Route> {
        let graph = scope.graph;
        let mut stack = vec![State {
            at: start,
            edges: Vec::new(),
            used: BTreeSet::new(),
            distance_m: 0.0,
        }];
        let mut routes = Vec::<Route>::new();
        let mut meter = SearchMeter::new(monitor, self.params.max_frontier);

        while let Some(state) = stack.pop() {
            if !meter.advance(routes.len()) {
                break;
            }
            if state.edges.len() >= self.params.max_hops {
                continue;
            }

            let mut fanout = scope.fanout(state.at);
            fanout.sort();
            fanout.reverse();

            for edge_id in fanout {
                if monitor.cancelled() {
                    return Vec::new();
                }
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

        if monitor.cancelled() {
            return Vec::new();
        }
        meter.finish(routes.len());
        finish_routes(routes, graph, constraints, count, self.params.keep, monitor)
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
        self.solve_monitored(SearchScope::all(graph), start, constraints, count, &())
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
    scope: SearchScope<'_>,
    start: VertexId,
    constraints: &LoopConstraints,
    count: usize,
    monitor: &dyn SearchMonitor,
) -> Vec<Route> {
    let graph = scope.graph;
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
    let mut meter = SearchMeter::new(monitor, params.max_frontier);
    let maximum_outward_m = constraints.max_distance_m * 0.675;
    let maximum_cost = maximum_outward_m * (1.0 + law.road_aversion);

    while let Some(SupportFrontier { cost, walk }) = frontier.pop() {
        if cost > maximum_cost || !meter.advance(routes.len()) {
            break;
        }
        if distance
            .get(&walk)
            .is_some_and(|best| cost > *best + f64::EPSILON)
        {
            continue;
        }
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
        for edge in scope.fanout(walk.at) {
            if monitor.cancelled() {
                return Vec::new();
            }
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
    if monitor.cancelled() {
        return Vec::new();
    }
    meter.finish(routes.len());
    finish_routes(routes, graph, constraints, count, params.keep, monitor)
}

const SUPPORT_RINGS: u32 = 12;
const SUPPORT_SECTORS: u32 = 16;

#[derive(Clone, Debug)]
struct SupportDesign {
    promise_m: f64,
    supports: Vec<VertexId>,
}

fn support_loop_portfolio(
    params: SearchParams,
    scope: SearchScope<'_>,
    start: VertexId,
    constraints: &LoopConstraints,
    count: usize,
    monitor: &dyn SearchMonitor,
) -> Vec<Route> {
    let graph = scope.graph;
    let skeleton = RoutingSkeleton::forge(scope, start, params.routing);
    let radial = radial_distances(&skeleton, start, constraints.max_distance_m * 0.55, monitor);
    if monitor.cancelled() {
        return Vec::new();
    }
    let landmarks = support_landmarks(graph, start, &radial, constraints);
    let designs = support_designs(graph, &radial, &landmarks, constraints, params);
    let limit = designs.len().min(params.max_frontier);
    let mut meter = SearchMeter::new(monitor, limit);
    let mut workspace = ArcWorkspace::new(skeleton.arcs.len());
    let mut banned = vec![false; graph.vertices.len()];
    let mut barred = vec![false; skeleton.arcs.len()];
    let mut routes = Vec::new();
    let mut forge = SupportForge {
        skeleton: &skeleton,
        start,
        constraints,
        monitor,
        outbound: vec![None; graph.vertices.len()],
    };
    for design in designs.into_iter().take(limit) {
        if !meter.advance(routes.len()) {
            break;
        }
        if let Some(edges) =
            forge.loop_through(&design.supports, &mut workspace, &mut banned, &mut barred)
        {
            push_allowed_route(&mut routes, graph, start, edges, constraints);
        }
    }
    if monitor.cancelled() {
        return Vec::new();
    }
    meter.finish(routes.len());
    finish_routes(routes, graph, constraints, count, params.keep, monitor)
}

fn support_designs(
    graph: &TrailGraph,
    radial: &[f64],
    landmarks: &[VertexId],
    constraints: &LoopConstraints,
    params: SearchParams,
) -> Vec<SupportDesign> {
    let pool = params.keep.max(1).saturating_mul(8);
    let target_m = (constraints.min_distance_m + constraints.max_distance_m) * 0.5;
    let mut designs = landmarks
        .iter()
        .copied()
        .map(|pivot| SupportDesign {
            promise_m: radial[pivot.0] * 2.0,
            supports: vec![pivot],
        })
        .collect::<Vec<_>>();

    let mut pairs = Vec::new();
    for first in landmarks.iter().copied() {
        for second in landmarks.iter().copied().filter(|second| *second != first) {
            pairs.push((
                radial[first.0]
                    + graph.vertices[first.0]
                        .coord
                        .haversine_m(graph.vertices[second.0].coord)
                    + radial[second.0],
                [first, second],
            ));
        }
    }
    pairs.sort_by(|left, right| {
        support_rank(left.0, &left.1, target_m, params.seed).cmp(&support_rank(
            right.0,
            &right.1,
            target_m,
            params.seed,
        ))
    });
    designs.extend(
        pairs
            .into_iter()
            .take(pool)
            .map(|(promise_m, supports)| SupportDesign {
                promise_m,
                supports: supports.into(),
            }),
    );

    let mut triples = Vec::new();
    for first in landmarks.iter().copied() {
        for second in landmarks.iter().copied().filter(|second| *second != first) {
            for third in landmarks
                .iter()
                .copied()
                .filter(|third| *third != first && *third != second)
            {
                triples.push((
                    radial[first.0]
                        + graph.vertices[first.0]
                            .coord
                            .haversine_m(graph.vertices[second.0].coord)
                        + graph.vertices[second.0]
                            .coord
                            .haversine_m(graph.vertices[third.0].coord)
                        + radial[third.0],
                    [first, second, third],
                ));
            }
        }
    }
    triples.sort_by(|left, right| {
        support_rank(left.0, &left.1, target_m, params.seed).cmp(&support_rank(
            right.0,
            &right.1,
            target_m,
            params.seed,
        ))
    });
    designs.extend(triples.into_iter().take(pool.saturating_mul(2)).map(
        |(promise_m, supports)| SupportDesign {
            promise_m,
            supports: supports.into(),
        },
    ));
    designs.sort_by(|left, right| {
        support_rank(left.promise_m, &left.supports, target_m, params.seed).cmp(&support_rank(
            right.promise_m,
            &right.supports,
            target_m,
            params.seed,
        ))
    });
    designs
}

fn support_rank(promise_m: f64, supports: &[VertexId], target_m: f64, seed: u64) -> (u64, u64) {
    let deviation = (promise_m - target_m).abs().to_bits();
    let hash = supports
        .iter()
        .fold(seed, |hash, vertex| splitmix64(hash ^ vertex.0 as u64));
    (deviation, hash)
}

fn support_landmarks(
    graph: &TrailGraph,
    start: VertexId,
    radial: &[f64],
    constraints: &LoopConstraints,
) -> Vec<VertexId> {
    let ceiling_m = constraints.max_distance_m * 0.5;
    if ceiling_m <= 0.0 || !ceiling_m.is_finite() {
        return Vec::new();
    }
    let floor_m = ceiling_m / 16.0;
    let origin = graph.vertices[start.0].coord;
    let reachable = graph
        .vertices
        .iter()
        .filter(|vertex| vertex.id != start && radial[vertex.id.0].is_finite())
        .collect::<Vec<_>>();
    let mut landmarks = BTreeSet::new();
    for ring in 0..SUPPORT_RINGS {
        let target_m =
            floor_m + (ceiling_m - floor_m) * (f64::from(ring) + 0.5) / f64::from(SUPPORT_RINGS);
        for sector in 0..SUPPORT_SECTORS {
            landmarks.extend(
                reachable
                    .iter()
                    .copied()
                    .filter(|vertex| bearing_sector(origin, vertex.coord) == sector)
                    .min_by(|left, right| {
                        (radial[left.id.0] - target_m)
                            .abs()
                            .total_cmp(&(radial[right.id.0] - target_m).abs())
                            .then_with(|| left.id.cmp(&right.id))
                    })
                    .map(|vertex| vertex.id),
            );
        }
    }
    landmarks.into_iter().collect()
}

fn bearing_sector(origin: Coord, point: Coord) -> u32 {
    let x = (point.lon - origin.lon) * origin.lat.to_radians().cos();
    let y = point.lat - origin.lat;
    let turn = (y.atan2(x) + std::f64::consts::PI) / std::f64::consts::TAU;
    (0..SUPPORT_SECTORS - 1)
        .find(|sector| turn < f64::from(*sector + 1) / f64::from(SUPPORT_SECTORS))
        .unwrap_or(SUPPORT_SECTORS - 1)
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ArcId(usize);

struct RoutingArc {
    id: ArcId,
    a: VertexId,
    b: VertexId,
    edges: Vec<EdgeId>,
    distance_m: f64,
    routing_cost_m: f64,
    forward: bool,
    backward: bool,
}

impl RoutingArc {
    fn traverse(&self, from: VertexId) -> Option<VertexId> {
        if from == self.a && self.forward {
            Some(self.b)
        } else if from == self.b && self.backward {
            Some(self.a)
        } else {
            None
        }
    }

    fn first_edge_from(&self, from: VertexId) -> Option<EdgeId> {
        self.traverse(from)?;
        if from == self.a {
            self.edges.first().copied()
        } else {
            self.edges.last().copied()
        }
    }

    fn last_edge_at(&self, at: VertexId) -> Option<EdgeId> {
        if at == self.b && self.forward {
            self.edges.last().copied()
        } else if at == self.a && self.backward {
            self.edges.first().copied()
        } else {
            None
        }
    }

    fn append_edges_from(&self, from: VertexId, output: &mut Vec<EdgeId>) -> Option<VertexId> {
        let at = self.traverse(from)?;
        if from == self.a {
            output.extend(self.edges.iter().copied());
        } else {
            output.extend(self.edges.iter().rev().copied());
        }
        Some(at)
    }
}

struct RoutingSkeleton<'graph> {
    graph: &'graph TrailGraph,
    arcs: Vec<RoutingArc>,
    adjacency: Vec<Vec<ArcId>>,
}

impl<'graph> RoutingSkeleton<'graph> {
    fn forge(scope: SearchScope<'graph>, start: VertexId, law: RoutingLaw) -> Self {
        let graph = scope.graph;
        let incidence = routing_incidence(scope, law);
        let preserved = preserved_vertices(graph, &incidence, start);
        let arcs = skeleton_arcs(graph, &incidence, &preserved, law);
        let adjacency = arc_adjacency(graph.vertices.len(), &arcs);
        Self {
            graph,
            arcs,
            adjacency,
        }
    }

    fn turn_allowed(&self, previous: Option<ArcId>, via: VertexId, next: ArcId) -> bool {
        let prior = previous.and_then(|arc| self.arcs[arc.0].last_edge_at(via));
        self.arcs[next.0]
            .first_edge_from(via)
            .is_some_and(|edge| self.graph.turn_allowed(prior, via, edge))
    }

    fn expand(&self, start: VertexId, arcs: &[ArcId]) -> Option<Vec<EdgeId>> {
        let mut at = start;
        let mut edges = Vec::new();
        for arc in arcs {
            at = self.arcs[arc.0].append_edges_from(at, &mut edges)?;
        }
        Some(edges)
    }
}

fn routing_incidence(scope: SearchScope<'_>, law: RoutingLaw) -> Vec<Vec<EdgeId>> {
    let graph = scope.graph;
    let mut incidence = vec![Vec::new(); graph.vertices.len()];
    for edge in graph
        .edges
        .iter()
        .filter(|edge| scope.allows(edge.id) && law.edge_cost(graph, edge.id).is_some())
    {
        incidence[edge.a.0].push(edge.id);
        incidence[edge.b.0].push(edge.id);
    }
    incidence
}

fn preserved_vertices(graph: &TrailGraph, incidence: &[Vec<EdgeId>], start: VertexId) -> Vec<bool> {
    let mut barred_turn = vec![false; graph.vertices.len()];
    for ban in &graph.turn_bans {
        barred_turn[ban.via.0] = true;
    }
    let mut preserved = graph
        .vertices
        .iter()
        .map(|vertex| {
            let edges = &incidence[vertex.id.0];
            let distinct_neighbours = edges.len() == 2
                && graph.edges[edges[0].0].other(vertex.id)
                    != graph.edges[edges[1].0].other(vertex.id);
            vertex.id == start
                || barred_turn[vertex.id.0]
                || !distinct_neighbours
                || edges
                    .iter()
                    .any(|edge| graph.edges[edge.0].attr.travel != EdgeTravel::Both)
        })
        .collect::<Vec<_>>();
    let roots = preserved.clone();
    for vertex in graph.vertices.iter().filter(|vertex| roots[vertex.id.0]) {
        for edge in &incidence[vertex.id.0] {
            let neighbour = graph.edges[edge.0]
                .other(vertex.id)
                .expect("an incident edge contains its vertex");
            preserved[neighbour.0] = true;
        }
    }
    preserved
}

fn skeleton_arcs(
    graph: &TrailGraph,
    incidence: &[Vec<EdgeId>],
    preserved: &[bool],
    law: RoutingLaw,
) -> Vec<RoutingArc> {
    let mut visited = vec![false; graph.edges.len()];
    let mut arcs = Vec::new();
    for vertex in graph
        .vertices
        .iter()
        .filter(|vertex| preserved[vertex.id.0])
    {
        for first in incidence[vertex.id.0].iter().copied() {
            if visited[first.0] {
                continue;
            }
            let mut edges = Vec::new();
            let mut at = vertex.id;
            let mut edge_id = first;
            let endpoint = loop {
                visited[edge_id.0] = true;
                edges.push(edge_id);
                let next = graph.edges[edge_id.0]
                    .other(at)
                    .expect("an incident edge contains its vertex");
                if preserved[next.0] {
                    break next;
                }
                edge_id = incidence[next.0]
                    .iter()
                    .copied()
                    .find(|candidate| *candidate != edge_id)
                    .expect("an elided vertex has exactly two incident edges");
                at = next;
            };
            let id = ArcId(arcs.len());
            let distance_m = edges
                .iter()
                .map(|edge| graph.edges[edge.0].attr.length_m)
                .sum();
            let routing_cost_m = edges
                .iter()
                .map(|edge| {
                    law.edge_cost(graph, *edge)
                        .expect("a skeleton contains only lawful edges")
                })
                .sum();
            arcs.push(RoutingArc {
                id,
                a: vertex.id,
                b: endpoint,
                forward: chain_traversable(graph, vertex.id, edges.iter().copied()),
                backward: chain_traversable(graph, endpoint, edges.iter().rev().copied()),
                edges,
                distance_m,
                routing_cost_m,
            });
        }
    }
    arcs
}

fn arc_adjacency(vertex_count: usize, arcs: &[RoutingArc]) -> Vec<Vec<ArcId>> {
    let mut adjacency = vec![Vec::new(); vertex_count];
    for arc in arcs {
        if arc.forward {
            adjacency[arc.a.0].push(arc.id);
        }
        if arc.backward {
            adjacency[arc.b.0].push(arc.id);
        }
    }
    adjacency
}

fn chain_traversable(
    graph: &TrailGraph,
    mut at: VertexId,
    edges: impl IntoIterator<Item = EdgeId>,
) -> bool {
    for edge in edges {
        let Some(next) = graph.edges[edge.0].traverse(at) else {
            return false;
        };
        at = next;
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RadialFrontier {
    distance_m: f64,
    at: VertexId,
}

impl Eq for RadialFrontier {}

impl Ord for RadialFrontier {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .distance_m
            .total_cmp(&self.distance_m)
            .then_with(|| other.at.cmp(&self.at))
    }
}

impl PartialOrd for RadialFrontier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn radial_distances(
    skeleton: &RoutingSkeleton<'_>,
    start: VertexId,
    maximum_m: f64,
    monitor: &dyn SearchMonitor,
) -> Vec<f64> {
    let graph = skeleton.graph;
    let mut distance = vec![f64::INFINITY; graph.vertices.len()];
    distance[start.0] = 0.0;
    let mut heap = BinaryHeap::from([RadialFrontier {
        distance_m: 0.0,
        at: start,
    }]);
    while let Some(frontier) = heap.pop() {
        if monitor.cancelled() {
            break;
        }
        if frontier.distance_m > distance[frontier.at.0] {
            continue;
        }
        for arc_id in &skeleton.adjacency[frontier.at.0] {
            let arc = &skeleton.arcs[arc_id.0];
            let Some(at) = arc.traverse(frontier.at) else {
                continue;
            };
            let next_m = frontier.distance_m + arc.distance_m;
            if next_m <= maximum_m && next_m < distance[at.0] {
                distance[at.0] = next_m;
                heap.push(RadialFrontier {
                    distance_m: next_m,
                    at,
                });
            }
        }
    }
    distance
}

struct SupportForge<'graph, 'constraint, 'monitor> {
    skeleton: &'graph RoutingSkeleton<'graph>,
    start: VertexId,
    constraints: &'constraint LoopConstraints,
    monitor: &'monitor dyn SearchMonitor,
    outbound: Vec<Option<MeasuredPath>>,
}

impl SupportForge<'_, '_, '_> {
    fn loop_through(
        &mut self,
        supports: &[VertexId],
        workspace: &mut ArcWorkspace,
        banned: &mut [bool],
        barred: &mut [bool],
    ) -> Option<Vec<EdgeId>> {
        banned.fill(false);
        barred.fill(false);
        banned[self.start.0] = true;
        let maximum_m = self.constraints.max_distance_m;
        let mut arcs = Vec::new();
        let mut at = self.start;
        let mut spent_m = 0.0;
        for target in supports.iter().copied().chain(std::iter::once(self.start)) {
            let hunt = AvoidanceHunt {
                skeleton: self.skeleton,
                from: at,
                target,
                previous: arcs.last().copied(),
                banned,
                barred,
                max_distance_m: maximum_m - spent_m,
                monitor: self.monitor,
            };
            let path = if at == self.start && arcs.is_empty() {
                if self.outbound[target.0].is_none() {
                    self.outbound[target.0] = shortest_path_avoiding(hunt, workspace);
                }
                self.outbound[target.0].clone()?
            } else {
                shortest_path_avoiding(hunt, workspace)?
            };
            ban_internal_vertices(self.skeleton, at, &path.arcs, banned);
            for arc in &path.arcs {
                barred[arc.0] = true;
            }
            if at != self.start {
                banned[at.0] = true;
            }
            spent_m += path.distance_m;
            arcs.extend(path.arcs);
            at = target;
        }
        let graph = self.skeleton.graph;
        let edges = self.skeleton.expand(self.start, &arcs)?;
        (edge_simple(&edges) && graph.walk_edges(self.start, &edges) == Some(self.start))
            .then_some(edges)
    }
}

fn ban_internal_vertices(
    skeleton: &RoutingSkeleton<'_>,
    mut at: VertexId,
    arcs: &[ArcId],
    banned: &mut [bool],
) {
    for arc in arcs.iter().copied().take(arcs.len().saturating_sub(1)) {
        at = skeleton.arcs[arc.0]
            .traverse(at)
            .expect("a recovered path is a legal walk");
        banned[at.0] = true;
    }
}

#[derive(Clone, Copy)]
struct AvoidanceHunt<'a> {
    skeleton: &'a RoutingSkeleton<'a>,
    from: VertexId,
    target: VertexId,
    previous: Option<ArcId>,
    banned: &'a [bool],
    barred: &'a [bool],
    max_distance_m: f64,
    monitor: &'a dyn SearchMonitor,
}

#[derive(Clone)]
struct MeasuredPath {
    arcs: Vec<ArcId>,
    distance_m: f64,
}

fn shortest_path_avoiding(
    hunt: AvoidanceHunt<'_>,
    workspace: &mut ArcWorkspace,
) -> Option<MeasuredPath> {
    if hunt.max_distance_m < 0.0 {
        return None;
    }
    let skeleton = hunt.skeleton;
    let target = skeleton.graph.vertices[hunt.target.0].coord;
    let origin_bound_m = skeleton.graph.vertices[hunt.from.0]
        .coord
        .haversine_m(target);
    if origin_bound_m > hunt.max_distance_m {
        return None;
    }
    workspace.begin();
    let origin = ArcWalk {
        at: hunt.from,
        previous: hunt.previous,
    };
    let origin_label = workspace.admit(
        arc_slot(skeleton, origin),
        ArcLabel {
            routing_cost_m: 0.0,
            distance_m: 0.0,
            predecessor: None,
            arc: None,
            live: true,
        },
    )?;
    workspace.heap.push(ArcFrontier {
        rank_m: origin_bound_m,
        routing_cost_m: 0.0,
        distance_m: 0.0,
        walk: origin,
        label: origin_label,
    });
    let mut expanded = 0usize;
    let expansion_cap = return_expansion_cap(1, skeleton.graph.edges.len());
    while let Some(frontier) = workspace.heap.pop() {
        if hunt.monitor.cancelled() || expanded >= expansion_cap {
            return None;
        }
        if !workspace.labels[frontier.label].live {
            continue;
        }
        expanded += 1;
        if frontier.walk.at == hunt.target {
            let arcs = recover_arc_path(&workspace.labels, frontier.label);
            return arc_simple(&arcs).then_some(MeasuredPath {
                arcs,
                distance_m: frontier.distance_m,
            });
        }
        for arc_id in &skeleton.adjacency[frontier.walk.at.0] {
            if hunt.barred[arc_id.0]
                || frontier.walk.previous == Some(*arc_id)
                || !skeleton.turn_allowed(frontier.walk.previous, frontier.walk.at, *arc_id)
            {
                continue;
            }
            let arc = &skeleton.arcs[arc_id.0];
            let Some(at) = arc.traverse(frontier.walk.at) else {
                continue;
            };
            if at != hunt.target && hunt.banned[at.0] {
                continue;
            }
            let distance_m = frontier.distance_m + arc.distance_m;
            let remaining_bound_m = skeleton.graph.vertices[at.0].coord.haversine_m(target);
            if distance_m + remaining_bound_m > hunt.max_distance_m {
                continue;
            }
            let routing_cost_m = frontier.routing_cost_m + arc.routing_cost_m;
            let walk = ArcWalk {
                at,
                previous: Some(*arc_id),
            };
            let Some(label) = workspace.admit(
                arc_slot(skeleton, walk),
                ArcLabel {
                    routing_cost_m,
                    distance_m,
                    predecessor: Some(frontier.label),
                    arc: Some(*arc_id),
                    live: true,
                },
            ) else {
                continue;
            };
            workspace.heap.push(ArcFrontier {
                rank_m: routing_cost_m + remaining_bound_m,
                routing_cost_m,
                distance_m,
                walk,
                label,
            });
        }
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ArcWalk {
    at: VertexId,
    previous: Option<ArcId>,
}

#[derive(Clone, Debug)]
struct ArcLabel {
    routing_cost_m: f64,
    distance_m: f64,
    predecessor: Option<usize>,
    arc: Option<ArcId>,
    live: bool,
}

impl ArcLabel {
    fn dominates(&self, routing_cost_m: f64, distance_m: f64) -> bool {
        self.live && self.routing_cost_m <= routing_cost_m && self.distance_m <= distance_m
    }

    fn is_dominated_by(&self, routing_cost_m: f64, distance_m: f64) -> bool {
        routing_cost_m <= self.routing_cost_m && distance_m <= self.distance_m
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ArcFrontier {
    rank_m: f64,
    routing_cost_m: f64,
    distance_m: f64,
    walk: ArcWalk,
    label: usize,
}

impl Eq for ArcFrontier {}

impl Ord for ArcFrontier {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .rank_m
            .total_cmp(&self.rank_m)
            .then_with(|| other.routing_cost_m.total_cmp(&self.routing_cost_m))
            .then_with(|| other.distance_m.total_cmp(&self.distance_m))
            .then_with(|| other.walk.cmp(&self.walk))
            .then_with(|| other.label.cmp(&self.label))
    }
}

impl PartialOrd for ArcFrontier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct ArcWorkspace {
    skylines: Vec<Vec<usize>>,
    touched: Vec<usize>,
    labels: Vec<ArcLabel>,
    heap: BinaryHeap<ArcFrontier>,
}

impl ArcWorkspace {
    fn new(arc_count: usize) -> Self {
        Self {
            skylines: vec![Vec::new(); closure_slot_count(arc_count)],
            touched: Vec::new(),
            labels: Vec::new(),
            heap: BinaryHeap::new(),
        }
    }

    fn begin(&mut self) {
        for slot in self.touched.drain(..) {
            self.skylines[slot].clear();
        }
        self.labels.clear();
        self.heap.clear();
    }

    fn admit(&mut self, slot: usize, label: ArcLabel) -> Option<usize> {
        let peers = &mut self.skylines[slot];
        let pristine = peers.is_empty();
        if peers
            .iter()
            .any(|id| self.labels[*id].dominates(label.routing_cost_m, label.distance_m))
        {
            return None;
        }
        for id in peers.iter().copied() {
            let incumbent = &mut self.labels[id];
            if incumbent.is_dominated_by(label.routing_cost_m, label.distance_m) {
                incumbent.live = false;
            }
        }
        peers.retain(|id| self.labels[*id].live);
        let id = self.labels.len();
        self.labels.push(label);
        if pristine {
            self.touched.push(slot);
        }
        peers.push(id);
        Some(id)
    }
}

fn arc_slot(skeleton: &RoutingSkeleton<'_>, walk: ArcWalk) -> usize {
    let Some(arc_id) = walk.previous else {
        return skeleton.arcs.len() * 2;
    };
    let arc = &skeleton.arcs[arc_id.0];
    arc_id.0 * 2
        + if walk.at == arc.a {
            0
        } else {
            debug_assert_eq!(walk.at, arc.b);
            1
        }
}

fn recover_arc_path(labels: &[ArcLabel], mut label: usize) -> Vec<ArcId> {
    let mut arcs = Vec::new();
    while let Some(predecessor) = labels[label].predecessor {
        arcs.push(
            labels[label]
                .arc
                .expect("a non-origin arc label records its arc"),
        );
        label = predecessor;
    }
    arcs.reverse();
    arcs
}

fn arc_simple(arcs: &[ArcId]) -> bool {
    let mut seen = BTreeSet::new();
    arcs.iter().all(|arc| seen.insert(*arc))
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
    monitor: &dyn SearchMonitor,
) -> Vec<Route> {
    if monitor.cancelled() {
        return Vec::new();
    }
    let mut seen = BTreeSet::new();
    routes.retain(|route| seen.insert(route_signature(route)));
    rank_routes(&mut routes, constraints);
    if monitor.cancelled() {
        return Vec::new();
    }
    routes = diverse_portfolio(routes, graph, graphless_limit(count, keep), monitor);
    if monitor.cancelled() {
        return Vec::new();
    }
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

fn diverse_portfolio(
    routes: Vec<Route>,
    graph: &TrailGraph,
    limit: usize,
    monitor: &dyn SearchMonitor,
) -> Vec<Route> {
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
    admit_diverse_tier(misses, graph, limit, &mut chosen, monitor);
    admit_diverse_tier(near, graph, limit, &mut chosen, monitor);
    chosen
}

fn admit_diverse_tier(
    routes: Vec<Route>,
    graph: &TrailGraph,
    limit: usize,
    chosen: &mut Vec<Route>,
    monitor: &dyn SearchMonitor,
) {
    let mut pool = routes.into_iter().map(Some).collect::<Vec<_>>();
    for exclusion_radius in [0.35, 0.20, 0.08, 0.0] {
        for candidate in &mut pool {
            if chosen.len() >= limit || monitor.cancelled() {
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
    monitor: &'a dyn SearchMonitor,
    allowed: Option<&'a [bool]>,
}

struct LoopCloser<'graph, 'monitor> {
    graph: &'graph TrailGraph,
    target: VertexId,
    keep: usize,
    law: RoutingLaw,
    monitor: &'monitor dyn SearchMonitor,
    allowed: Option<&'graph [bool]>,
    active: bool,
    workspace: ClosureWorkspace,
    oracle: Option<ClosureOracle>,
}

impl<'graph, 'monitor> LoopCloser<'graph, 'monitor> {
    fn forge(
        scope: SearchScope<'graph>,
        target: VertexId,
        params: SearchParams,
        constraints: &LoopConstraints,
        monitor: &'monitor dyn SearchMonitor,
    ) -> Option<Self> {
        let active = closes_allowed(constraints);
        let oracle = if active && params.closure_paths <= 1 {
            Some(ClosureOracle::forge(
                scope.graph,
                target,
                scope.allowed,
                params.routing,
                monitor,
            )?)
        } else {
            None
        };
        Some(Self {
            graph: scope.graph,
            target,
            keep: params.closure_paths,
            law: params.routing,
            monitor,
            allowed: scope.allowed,
            active,
            workspace: ClosureWorkspace::new(scope.graph.edges.len()),
            oracle,
        })
    }

    fn strike(&mut self, state: &State, constraints: &LoopConstraints, routes: &mut Vec<Route>) {
        if !self.active || state.at == self.target || state.edges.is_empty() {
            return;
        }
        let max_distance_m = constraints.max_distance_m.mul_add(1.35, -state.distance_m);
        let hunt = ReturnHunt {
            graph: self.graph,
            from: state.at,
            target: self.target,
            previous: state.edges.last().copied(),
            barred: &state.used,
            max_distance_m,
            keep: self.keep,
            law: self.law,
            monitor: self.monitor,
            allowed: self.allowed,
        };
        for return_edges in shortest_return_paths(hunt, &mut self.workspace, self.oracle.as_ref()) {
            let mut edges = state.edges.clone();
            edges.extend(return_edges);
            push_allowed_route(routes, self.graph, self.target, edges, constraints);
        }
    }
}

fn shortest_return_paths(
    hunt: ReturnHunt<'_>,
    workspace: &mut ClosureWorkspace,
    oracle: Option<&ClosureOracle>,
) -> Vec<Vec<EdgeId>> {
    if hunt.max_distance_m < 0.0 {
        return Vec::new();
    }
    if hunt.keep <= 1 {
        return shortest_return_path(
            hunt,
            workspace,
            oracle.expect("single-path closure search has an oracle"),
        )
        .into_iter()
        .collect();
    }
    enumerate_return_paths(hunt)
}

fn shortest_return_path(
    hunt: ReturnHunt<'_>,
    workspace: &mut ClosureWorkspace,
    oracle: &ClosureOracle,
) -> Option<Vec<EdgeId>> {
    // A* ranks by the shared reverse potential, while each turn-state keeps
    // the (routing cost, physical distance) skyline required by the separate
    // hard distance budget.
    workspace.begin();
    let origin = SupportWalk {
        at: hunt.from,
        previous: hunt.previous,
    };
    let origin_label = workspace
        .admit(
            closure_slot(hunt.graph, origin),
            ClosureLabel {
                routing_cost_m: 0.0,
                distance_m: 0.0,
                predecessor: None,
                edge: None,
                live: true,
            },
        )
        .expect("an empty closure skyline admits its origin");
    let rank_m = oracle.cost(hunt.graph, origin);
    if !rank_m.is_finite() {
        return None;
    }
    workspace.heap.push(ClosureFrontier {
        rank_m,
        routing_cost_m: 0.0,
        distance_m: 0.0,
        walk: origin,
        label: origin_label,
    });
    let expansion_cap = return_expansion_cap(1, hunt.graph.edges.len());
    let mut expanded = 0usize;

    while let Some(frontier) = workspace.heap.pop() {
        if hunt.monitor.cancelled() {
            return None;
        }
        if expanded >= expansion_cap {
            break;
        }
        if !workspace.labels[frontier.label].live {
            continue;
        }
        expanded += 1;
        if frontier.walk.at == hunt.target && frontier.label != origin_label {
            let path = recover_closure_path(&workspace.labels, frontier.label);
            if edge_simple(&path) {
                return Some(path);
            }
            continue;
        }
        for edge_id in &hunt.graph.adjacency[frontier.walk.at.0] {
            if hunt.allowed.is_some_and(|allowed| !allowed[edge_id.0])
                || hunt.barred.contains(edge_id)
                || frontier.walk.previous == Some(*edge_id)
                || !hunt
                    .graph
                    .turn_allowed(frontier.walk.previous, frontier.walk.at, *edge_id)
            {
                continue;
            }
            let Some(edge_cost) = hunt.law.edge_cost(hunt.graph, *edge_id) else {
                continue;
            };
            let edge = &hunt.graph.edges[edge_id.0];
            let Some(at) = edge.traverse(frontier.walk.at) else {
                continue;
            };
            let distance_m = frontier.distance_m + edge.attr.length_m;
            if distance_m > hunt.max_distance_m {
                continue;
            }
            let routing_cost_m = frontier.routing_cost_m + edge_cost;
            let walk = SupportWalk {
                at,
                previous: Some(*edge_id),
            };
            let remaining_cost_m = oracle.cost(hunt.graph, walk);
            if !remaining_cost_m.is_finite() {
                continue;
            }
            let Some(label) = workspace.admit(
                closure_slot(hunt.graph, walk),
                ClosureLabel {
                    routing_cost_m,
                    distance_m,
                    predecessor: Some(frontier.label),
                    edge: Some(*edge_id),
                    live: true,
                },
            ) else {
                continue;
            };
            workspace.heap.push(ClosureFrontier {
                rank_m: routing_cost_m + remaining_cost_m,
                routing_cost_m,
                distance_m,
                walk,
                label,
            });
        }
    }
    None
}

fn enumerate_return_paths(hunt: ReturnHunt<'_>) -> Vec<Vec<EdgeId>> {
    let keep = hunt.keep.max(1);
    let expansion_cap = return_expansion_cap(keep, hunt.graph.edges.len());
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
        if hunt.monitor.cancelled() {
            return Vec::new();
        }
        if paths.len() >= keep || expanded >= expansion_cap {
            break;
        }
        expanded += 1;
        if state.at == hunt.target && !state.edges.is_empty() {
            paths.push(state.edges);
            continue;
        }
        for edge_id in &hunt.graph.adjacency[state.at.0] {
            if hunt.allowed.is_some_and(|allowed| !allowed[edge_id.0]) {
                continue;
            }
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

fn return_expansion_cap(keep: usize, edge_count: usize) -> usize {
    keep.saturating_mul(edge_count.max(1))
        .saturating_mul(8)
        .max(64)
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
struct ClosureLabel {
    routing_cost_m: f64,
    distance_m: f64,
    predecessor: Option<usize>,
    edge: Option<EdgeId>,
    live: bool,
}

impl ClosureLabel {
    fn dominates(&self, routing_cost_m: f64, distance_m: f64) -> bool {
        self.live && self.routing_cost_m <= routing_cost_m && self.distance_m <= distance_m
    }

    fn is_dominated_by(&self, routing_cost_m: f64, distance_m: f64) -> bool {
        routing_cost_m <= self.routing_cost_m && distance_m <= self.distance_m
    }
}

struct ClosureWorkspace {
    skylines: Vec<Vec<usize>>,
    touched: Vec<usize>,
    labels: Vec<ClosureLabel>,
    heap: BinaryHeap<ClosureFrontier>,
}

impl ClosureWorkspace {
    fn new(edge_count: usize) -> Self {
        Self {
            skylines: vec![Vec::new(); closure_slot_count(edge_count)],
            touched: Vec::new(),
            labels: Vec::new(),
            heap: BinaryHeap::new(),
        }
    }

    fn begin(&mut self) {
        for slot in self.touched.drain(..) {
            self.skylines[slot].clear();
        }
        self.labels.clear();
        self.heap.clear();
    }

    fn admit(&mut self, slot: usize, label: ClosureLabel) -> Option<usize> {
        let peers = &mut self.skylines[slot];
        let pristine = peers.is_empty();
        if peers
            .iter()
            .any(|id| self.labels[*id].dominates(label.routing_cost_m, label.distance_m))
        {
            return None;
        }
        for id in peers.iter().copied() {
            let incumbent = &mut self.labels[id];
            if incumbent.is_dominated_by(label.routing_cost_m, label.distance_m) {
                incumbent.live = false;
            }
        }
        peers.retain(|id| self.labels[*id].live);
        let id = self.labels.len();
        self.labels.push(label);
        if pristine {
            self.touched.push(slot);
        }
        peers.push(id);
        Some(id)
    }
}

/// Exact shortest-cost potentials on the directed turn-state graph. Per-path
/// outbound-edge bans are deliberately relaxed, so these shared values remain
/// admissible for every closure attempted by one loop search.
struct ClosureOracle {
    cost_m: Vec<f64>,
}

impl ClosureOracle {
    fn forge(
        graph: &TrailGraph,
        target: VertexId,
        allowed: Option<&[bool]>,
        law: RoutingLaw,
        monitor: &dyn SearchMonitor,
    ) -> Option<Self> {
        let mut cost_m = vec![f64::INFINITY; closure_slot_count(graph.edges.len())];
        let mut heap = BinaryHeap::new();
        let mut incoming = vec![Vec::new(); graph.vertices.len()];
        for edge in &graph.edges {
            if allowed.is_some_and(|allowed| !allowed[edge.id.0])
                || law.edge_cost(graph, edge.id).is_none()
            {
                continue;
            }
            if edge.traverse(edge.a) == Some(edge.b) {
                incoming[edge.b.0].push(edge.id);
            }
            if edge.traverse(edge.b) == Some(edge.a) {
                incoming[edge.a.0].push(edge.id);
            }
        }
        for edge_id in &incoming[target.0] {
            let walk = SupportWalk {
                at: target,
                previous: Some(*edge_id),
            };
            let slot = closure_slot(graph, walk);
            cost_m[slot] = 0.0;
            heap.push(SupportFrontier { cost: 0.0, walk });
        }

        while let Some(SupportFrontier { cost, walk }) = heap.pop() {
            if monitor.cancelled() {
                return None;
            }
            if cost > cost_m[closure_slot(graph, walk)] {
                continue;
            }
            let edge_id = walk
                .previous
                .expect("an oracle frontier always follows an edge");
            let edge = &graph.edges[edge_id.0];
            let Some(via) = edge.other(walk.at) else {
                continue;
            };
            if edge.traverse(via) != Some(walk.at) {
                continue;
            }
            let edge_cost_m = law
                .edge_cost(graph, edge_id)
                .expect("an oracle frontier only contains lawful edges");
            for prior in &incoming[via.0] {
                if *prior == edge_id {
                    continue;
                }
                if !graph.turn_allowed(Some(*prior), via, edge_id) {
                    continue;
                }
                let predecessor = SupportWalk {
                    at: via,
                    previous: Some(*prior),
                };
                let slot = closure_slot(graph, predecessor);
                let candidate_m = cost + edge_cost_m;
                if candidate_m < cost_m[slot] {
                    cost_m[slot] = candidate_m;
                    heap.push(SupportFrontier {
                        cost: candidate_m,
                        walk: predecessor,
                    });
                }
            }
        }
        Some(Self { cost_m })
    }

    fn cost(&self, graph: &TrailGraph, walk: SupportWalk) -> f64 {
        self.cost_m[closure_slot(graph, walk)]
    }
}

fn closure_slot(graph: &TrailGraph, walk: SupportWalk) -> usize {
    let Some(edge_id) = walk.previous else {
        return graph.edges.len() * 2;
    };
    let edge = &graph.edges[edge_id.0];
    edge_id.0 * 2
        + if walk.at == edge.a {
            0
        } else {
            debug_assert_eq!(walk.at, edge.b);
            1
        }
}

fn closure_slot_count(edge_count: usize) -> usize {
    edge_count
        .checked_mul(2)
        .and_then(|slots| slots.checked_add(1))
        .expect("a trail graph has fewer than usize::MAX / 2 edges")
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ClosureFrontier {
    rank_m: f64,
    routing_cost_m: f64,
    distance_m: f64,
    walk: SupportWalk,
    label: usize,
}

impl Eq for ClosureFrontier {}

impl Ord for ClosureFrontier {
    fn cmp(&self, rhs: &Self) -> Ordering {
        rhs.rank_m
            .total_cmp(&self.rank_m)
            .then_with(|| rhs.routing_cost_m.total_cmp(&self.routing_cost_m))
            .then_with(|| rhs.distance_m.total_cmp(&self.distance_m))
            .then_with(|| rhs.walk.cmp(&self.walk))
            .then_with(|| rhs.label.cmp(&self.label))
    }
}

impl PartialOrd for ClosureFrontier {
    fn partial_cmp(&self, rhs: &Self) -> Option<Ordering> {
        Some(self.cmp(rhs))
    }
}

fn recover_closure_path(labels: &[ClosureLabel], mut label: usize) -> Vec<EdgeId> {
    let mut path = Vec::new();
    loop {
        let current = &labels[label];
        let Some(edge) = current.edge else {
            break;
        };
        path.push(edge);
        label = current
            .predecessor
            .expect("every closure edge has a predecessor");
    }
    path.reverse();
    path
}

fn edge_simple(path: &[EdgeId]) -> bool {
    let mut seen = BTreeSet::new();
    path.iter().all(|edge| seen.insert(*edge))
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
    use std::cell::{Cell, RefCell};

    struct RecordingMonitor {
        cancel_after: usize,
        checks: Cell<usize>,
        progress: RefCell<Vec<SearchProgress>>,
    }

    impl RecordingMonitor {
        fn patient() -> Self {
            Self {
                cancel_after: usize::MAX,
                checks: Cell::new(0),
                progress: RefCell::new(Vec::new()),
            }
        }
    }

    impl SearchMonitor for RecordingMonitor {
        fn cancelled(&self) -> bool {
            let checks = self.checks.get().saturating_add(1);
            self.checks.set(checks);
            checks >= self.cancel_after
        }

        fn report(&self, progress: SearchProgress) {
            self.progress.borrow_mut().push(progress);
        }
    }

    fn branch(points: Vec<Coord>, name: &str) -> SegmentDraft {
        SegmentDraft {
            geometry: LineString::new(points).expect("valid branch"),
            junctions: JunctionPolicy::Planar,
            turn_ref: None,
            turn_restrictions: Vec::new(),
            trail_class: TrailClass::Path,
            standing: TrailStanding::Established,
            marking: crate::TrailMarking::default(),
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

    fn named_edge(graph: &TrailGraph, name: &str) -> EdgeId {
        graph
            .edges
            .iter()
            .find(|edge| {
                edge.attr
                    .provenance
                    .iter()
                    .any(|source| source.source_id.as_deref() == Some(name))
            })
            .map(|edge| edge.id)
            .expect("fixture edge exists")
    }

    #[test]
    fn routing_skeleton_crushes_shape_points_without_losing_support() {
        let corners = [
            Coord::new(0.0, 0.0),
            Coord::new(0.01, 0.0),
            Coord::new(0.01, 0.01),
            Coord::new(0.0, 0.01),
            Coord::new(0.0, 0.0),
        ];
        let mut points = Vec::new();
        for side in corners.windows(2) {
            for step in 0..40_u32 {
                let t = f64::from(step) / 40.0;
                points.push(Coord::new(
                    (side[1].lon - side[0].lon).mul_add(t, side[0].lon),
                    (side[1].lat - side[0].lat).mul_add(t, side[0].lat),
                ));
            }
        }
        points.push(corners[4]);
        let graph = GraphBuilder::default()
            .build(&[branch(points, "fine-ring")])
            .expect("build fine ring");
        let start = graph.nearest_vertex(corners[0]).expect("ring origin");
        let skeleton =
            RoutingSkeleton::forge(SearchScope::all(&graph), start, RoutingLaw::default());

        assert_eq!(graph.edges.len(), 160);
        assert_eq!(skeleton.arcs.len(), 3);
        assert_eq!(
            skeleton
                .arcs
                .iter()
                .map(|arc| arc.edges.len())
                .sum::<usize>(),
            graph.edges.len()
        );
    }

    #[test]
    fn closure_oracle_preserves_cost_distance_pareto_labels() {
        let approach = Coord::new(-0.001, 0.0);
        let from = Coord::new(0.0, 0.0);
        let bend = Coord::new(0.002, 0.003);
        let junction = Coord::new(0.004, 0.0);
        let merge = Coord::new(0.005, 0.0);
        let target = Coord::new(0.008, 0.0);
        let mut road = branch(vec![from, junction], "short-road");
        road.trail_class = TrailClass::Road;
        road.terrain = Terrain::Road;
        road.road_exposure = 1.0;
        let graph = GraphBuilder::default()
            .build(&[
                branch(vec![approach, from], "approach"),
                branch(vec![from, bend, junction], "long-clean"),
                road,
                branch(vec![junction, merge], "common"),
                branch(vec![merge, target], "tail"),
            ])
            .expect("build cost-distance closure fixture");
        let from = graph.nearest_vertex(from).expect("closure origin");
        let target = graph.nearest_vertex(target).expect("closure target");
        let approach = named_edge(&graph, "approach");
        let road = named_edge(&graph, "short-road");
        let barred = BTreeSet::from([approach]);
        let law = RoutingLaw { road_aversion: 2.0 };
        let oracle =
            ClosureOracle::forge(&graph, target, None, law, &()).expect("forge closure oracle");
        let mut workspace = ClosureWorkspace::new(graph.edges.len());
        let paths = shortest_return_paths(
            ReturnHunt {
                graph: &graph,
                from,
                target,
                previous: Some(approach),
                barred: &barred,
                max_distance_m: 1_000.0,
                keep: 1,
                law,
                monitor: &(),
                allowed: None,
            },
            &mut workspace,
            Some(&oracle),
        );

        assert_eq!(paths.len(), 1);
        let path = &paths[0];
        assert_eq!(graph.walk_edges(from, path), Some(target));
        assert!(edge_simple(path));
        assert!(path.contains(&road));
        assert!(route_distance(&graph, path) <= 1_000.0);
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
    fn restricted_scope_never_leaks_a_forbidden_edge() {
        let origin = Coord::new(0.0, 0.0);
        let graph = GraphBuilder::default()
            .build(&[
                branch(
                    vec![origin, Coord::new(0.001, 0.0), Coord::new(0.002, 0.0)],
                    "allowed",
                ),
                branch(
                    vec![origin, Coord::new(0.0, 0.001), Coord::new(0.0, 0.002)],
                    "forbidden",
                ),
            ])
            .expect("build scoped graph");
        let allowed = graph
            .edges
            .iter()
            .map(|edge| {
                edge.geometry
                    .points
                    .iter()
                    .all(|point| point.lat.abs() < f64::EPSILON)
            })
            .collect::<Vec<_>>();
        let constraints = LoopConstraints {
            min_distance_m: 100.0,
            max_distance_m: 1_000.0,
            max_difficulty: f64::MAX,
            max_repeated_edge_fraction: 1.0,
            allowed_shapes: vec![RouteShape::OutAndBack],
            ..LoopConstraints::default()
        };
        let routes = SolverKind::Auto.solve_scoped(
            SearchParams::default(),
            SearchScope::restricted(&graph, &allowed),
            graph.nearest_vertex(origin).expect("origin vertex"),
            &constraints,
            3,
            &(),
        );

        assert!(!routes.is_empty());
        assert!(
            routes
                .iter()
                .flat_map(|route| &route.edges)
                .all(|edge| allowed[edge.0])
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

    #[test]
    fn monitored_search_reports_monotone_effort_and_ranking() {
        let origin = Coord::new(0.0, 0.0);
        let graph = GraphBuilder::default()
            .build(&[branch(
                vec![
                    origin,
                    Coord::new(0.001, 0.0),
                    Coord::new(0.002, 0.0),
                    Coord::new(0.003, 0.0),
                ],
                "monitored",
            )])
            .expect("build monitored graph");
        let start = graph.nearest_vertex(origin).expect("origin vertex");
        let constraints = LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 1_000.0,
            max_difficulty: f64::MAX,
            max_repeated_edge_fraction: 1.0,
            allowed_shapes: vec![RouteShape::OutAndBack],
            ..LoopConstraints::default()
        };
        let monitor = RecordingMonitor::patient();

        let _routes = SolverKind::Auto.solve_monitored(
            SearchParams {
                max_frontier: 100,
                ..SearchParams::default()
            },
            &graph,
            start,
            &constraints,
            3,
            &monitor,
        );

        let progress = monitor.progress.borrow();
        assert_eq!(
            progress.first().map(|progress| progress.stage),
            Some(SearchStage::Exploring)
        );
        assert_eq!(
            progress.last().map(|progress| progress.stage),
            Some(SearchStage::Ranking)
        );
        assert!(
            progress
                .windows(2)
                .all(|pair| pair[0].explored <= pair[1].explored)
        );
        assert!(progress.iter().all(|progress| progress.explored <= 100));
    }

    #[test]
    fn monitored_search_obeys_cooperative_cancellation() {
        let origin = Coord::new(0.0, 0.0);
        let graph = GraphBuilder::default()
            .build(&[branch(
                vec![
                    origin,
                    Coord::new(0.001, 0.0),
                    Coord::new(0.002, 0.0),
                    Coord::new(0.003, 0.0),
                ],
                "cancelled",
            )])
            .expect("build cancellable graph");
        let start = graph.nearest_vertex(origin).expect("origin vertex");
        let constraints = LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 1_000.0,
            max_difficulty: f64::MAX,
            max_repeated_edge_fraction: 1.0,
            allowed_shapes: vec![RouteShape::OutAndBack],
            ..LoopConstraints::default()
        };
        let monitor = RecordingMonitor {
            cancel_after: 3,
            checks: Cell::new(0),
            progress: RefCell::new(Vec::new()),
        };

        let routes = SolverKind::Auto.solve_monitored(
            SearchParams::default(),
            &graph,
            start,
            &constraints,
            3,
            &monitor,
        );

        assert!(routes.is_empty());
        assert!(
            monitor
                .progress
                .borrow()
                .iter()
                .all(|progress| progress.stage != SearchStage::Ranking)
        );
    }
}
