use crate::RouteShape;
use crate::constraints::LoopConstraints;
use crate::model::{EdgeId, TrailGraph, VertexId};
use crate::route::{Route, rank_routes};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchParams {
    pub max_hops: usize,
    pub max_frontier: usize,
    pub keep: usize,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            max_hops: 36,
            max_frontier: 200_000,
            keep: 12,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LoopHunter {
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
            if state.edges.len() >= self.params.max_hops {
                continue;
            }
            let mut fanout = graph.adjacency[state.at.0].clone();
            fanout.sort_by(|a, b| {
                graph.edges[a.0]
                    .attr
                    .difficulty
                    .total_cmp(&graph.edges[b.0].attr.difficulty)
            });

            for edge_id in fanout {
                if state.used.contains(&edge_id) {
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
                if next == start && edges.len() >= 3 {
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

        let mut seen = BTreeSet::new();
        routes.retain(|route| seen.insert(route.edges.clone()));
        rank_routes(&mut routes, constraints);
        routes.truncate(count.max(1).min(self.params.keep));
        for (i, route) in routes.iter_mut().enumerate() {
            route.name = format!("candidate-{}", i + 1);
        }
        routes
    }
}

fn push_allowed_route(
    routes: &mut Vec<Route>,
    graph: &TrailGraph,
    start: VertexId,
    edges: Vec<EdgeId>,
    constraints: &LoopConstraints,
) {
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

fn route_distance(graph: &TrailGraph, edges: &[EdgeId]) -> f64 {
    edges
        .iter()
        .map(|edge_id| graph.edges[edge_id.0].attr.length_m)
        .sum()
}

fn mirrored_route(edges: &[EdgeId]) -> Vec<EdgeId> {
    edges
        .iter()
        .copied()
        .chain(edges.iter().rev().copied())
        .collect()
}
