use crate::{EdgeId, EdgeIndex, RoutingLaw, VertexId, WalkGraph};
use std::{
    cmp::Ordering,
    collections::{BTreeSet, BinaryHeap},
};

/// Immutable graph-side indices shared by every realization against one
/// corpus. Search scratch lives separately so a worker can reuse it without
/// synchronization or per-leg maps.
#[derive(Clone, Debug)]
pub struct WalkRouter {
    forbidden_turns: BTreeSet<(EdgeId, VertexId, EdgeId)>,
}

/// A materialized induced graph projection. Keeping its CSR separate prevents
/// urban degree from taxing Finder at every expansion.
#[derive(Clone)]
pub struct WalkRealmIndex {
    allowed: Vec<bool>,
    adjacency: Vec<Vec<EdgeId>>,
    edges: EdgeIndex,
}

impl WalkRealmIndex {
    #[must_use]
    pub fn finder(graph: &WalkGraph) -> Self {
        Self::forge(
            graph,
            graph
                .edges
                .iter()
                .map(|edge| edge.attr.realm.admitted_by_finder())
                .collect(),
        )
    }

    fn forge(graph: &WalkGraph, allowed: Vec<bool>) -> Self {
        let mut adjacency = vec![Vec::new(); graph.vertices.len()];
        for (vertex, fanout) in graph.adjacency.iter().enumerate() {
            adjacency[vertex].extend(fanout.iter().copied().filter(|edge| allowed[edge.0]));
        }
        let edges = EdgeIndex::forge_allowed(graph, &allowed);
        Self {
            allowed,
            adjacency,
            edges,
        }
    }

    #[must_use]
    pub fn allowed(&self) -> &[bool] {
        &self.allowed
    }

    #[must_use]
    pub fn adjacency(&self) -> &[Vec<EdgeId>] {
        &self.adjacency
    }

    #[must_use]
    pub const fn edges(&self) -> &EdgeIndex {
        &self.edges
    }
}

impl WalkRouter {
    #[must_use]
    pub fn forge(graph: &WalkGraph) -> Self {
        Self {
            forbidden_turns: graph
                .turn_bans
                .iter()
                .map(|ban| (ban.from, ban.via, ban.to))
                .collect(),
        }
    }

    #[must_use]
    pub fn workspace(&self, graph: &WalkGraph) -> RoutingWorkspace {
        RoutingWorkspace::forge(graph.edges.len())
    }

    #[must_use]
    pub fn shortest_path(
        &self,
        graph: &WalkGraph,
        workspace: &mut RoutingWorkspace,
        request: RouteRequest<'_>,
    ) -> Option<Vec<EdgeId>> {
        if request.from == request.target {
            return Some(Vec::new());
        }
        workspace.begin(graph.edges.len());
        let origin = workspace.origin_slot();
        workspace.set(origin, 0.0, None);
        let mut frontier = BinaryHeap::from([Frontier {
            estimate: heuristic(graph, request.from, request.target),
            cost: 0.0,
            slot: origin,
            at: request.from,
            previous: request.previous,
        }]);

        while let Some(here) = frontier.pop() {
            if request
                .cost_ceiling
                .is_some_and(|ceiling| here.cost > ceiling)
                || workspace
                    .cost(here.slot)
                    .is_none_or(|best| here.cost.total_cmp(&best).is_gt())
            {
                continue;
            }
            if here.at == request.target {
                return workspace.reconstruct(origin, here.slot);
            }
            for edge_id in graph.adjacency[here.at.0].iter().copied() {
                if request
                    .forbidden_edges
                    .is_some_and(|edges| edges.contains(&edge_id))
                    || !self.turn_allowed(here.previous, here.at, edge_id)
                {
                    continue;
                }
                let Some(step_cost) = request.law.edge_cost(graph, edge_id) else {
                    continue;
                };
                let cost = here.cost + step_cost;
                if request.cost_ceiling.is_some_and(|ceiling| cost > ceiling) {
                    continue;
                }
                let edge = &graph.edges[edge_id.0];
                let Some(at) = edge.traverse(here.at) else {
                    continue;
                };
                if at != request.target
                    && request
                        .forbidden_vertices
                        .is_some_and(|vertices| vertices.contains(&at))
                {
                    continue;
                }
                let slot = state_slot(edge_id, at, edge.a);
                if workspace
                    .cost(slot)
                    .is_some_and(|best| !cost.total_cmp(&best).is_lt())
                {
                    continue;
                }
                workspace.set(
                    slot,
                    cost,
                    Some(Predecessor {
                        slot: here.slot,
                        edge: edge_id,
                    }),
                );
                frontier.push(Frontier {
                    estimate: cost + heuristic(graph, at, request.target),
                    cost,
                    slot,
                    at,
                    previous: Some(edge_id),
                });
            }
        }
        None
    }

    fn turn_allowed(&self, from: Option<EdgeId>, via: VertexId, to: EdgeId) -> bool {
        from.is_none_or(|from| !self.forbidden_turns.contains(&(from, via, to)))
    }
}

#[derive(Clone, Copy)]
pub struct RouteRequest<'a> {
    pub from: VertexId,
    pub target: VertexId,
    pub previous: Option<EdgeId>,
    pub law: RoutingLaw,
    pub cost_ceiling: Option<f64>,
    pub forbidden_edges: Option<&'a BTreeSet<EdgeId>>,
    pub forbidden_vertices: Option<&'a BTreeSet<VertexId>>,
}

#[derive(Clone, Debug)]
pub struct RoutingWorkspace {
    generation: u32,
    labels: Vec<Label>,
}

impl RoutingWorkspace {
    fn forge(edge_count: usize) -> Self {
        Self {
            generation: 0,
            labels: vec![Label::default(); edge_count.saturating_mul(2).saturating_add(1)],
        }
    }

    fn begin(&mut self, edge_count: usize) {
        let slots = edge_count.saturating_mul(2).saturating_add(1);
        if self.labels.len() != slots {
            self.labels.resize(slots, Label::default());
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.labels.fill(Label::default());
            self.generation = 1;
        }
    }

    const fn origin_slot(&self) -> usize {
        self.labels.len() - 1
    }

    fn cost(&self, slot: usize) -> Option<f64> {
        let label = &self.labels[slot];
        (label.generation == self.generation).then_some(label.cost)
    }

    fn predecessor(&self, slot: usize) -> Option<Predecessor> {
        let label = &self.labels[slot];
        (label.generation == self.generation)
            .then_some(label.predecessor)
            .flatten()
    }

    fn set(&mut self, slot: usize, cost: f64, predecessor: Option<Predecessor>) {
        self.labels[slot] = Label {
            generation: self.generation,
            cost,
            predecessor,
        };
    }

    fn reconstruct(&self, origin: usize, mut cursor: usize) -> Option<Vec<EdgeId>> {
        let mut edges = Vec::new();
        while cursor != origin {
            let predecessor = self.predecessor(cursor)?;
            edges.push(predecessor.edge);
            cursor = predecessor.slot;
        }
        edges.reverse();
        Some(edges)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Label {
    generation: u32,
    cost: f64,
    predecessor: Option<Predecessor>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Predecessor {
    slot: usize,
    edge: EdgeId,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Frontier {
    estimate: f64,
    cost: f64,
    slot: usize,
    at: VertexId,
    previous: Option<EdgeId>,
}

impl Eq for Frontier {}

impl Ord for Frontier {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .estimate
            .total_cmp(&self.estimate)
            .then_with(|| other.cost.total_cmp(&self.cost))
            .then_with(|| other.slot.cmp(&self.slot))
    }
}

impl PartialOrd for Frontier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn state_slot(edge: EdgeId, at: VertexId, edge_a: VertexId) -> usize {
    edge.0 * 2 + usize::from(at != edge_a)
}

fn heuristic(graph: &WalkGraph, from: VertexId, target: VertexId) -> f64 {
    graph.vertices[from.0]
        .coord
        .haversine_m(graph.vertices[target.0].coord)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GraphBuilder, io::geojson};
    use std::collections::BTreeMap;

    #[test]
    fn astar_matches_reference_dijkstra_across_vetoes() {
        let drafts =
            geojson::network_from_str(include_str!("../tests/fixtures/mini_network.geojson"))
                .unwrap();
        let graph = GraphBuilder::default().build(&drafts).unwrap();
        let router = WalkRouter::forge(&graph);
        let mut workspace = router.workspace(&graph);
        let mut entropy = 0x9e37_79b9_7f4a_7c15_u64;

        for trial in 0..256 {
            entropy = entropy
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let from = VertexId(
                usize::from(u16::try_from(entropy & u64::from(u16::MAX)).expect("masked to u16"))
                    % graph.vertices.len(),
            );
            let target = VertexId(
                usize::from(
                    u16::try_from((entropy >> 16) & u64::from(u16::MAX)).expect("masked to u16"),
                ) % graph.vertices.len(),
            );
            let law = RoutingLaw {
                road_aversion: [0.0, 1.0, 3.0][trial % 3],
            };
            let forbidden_edges = graph
                .edges
                .iter()
                .filter_map(|edge| {
                    let rotation = u32::try_from(edge.id.0 % 64).expect("rotation is below 64");
                    (entropy.rotate_left(rotation).trailing_zeros() >= 5).then_some(edge.id)
                })
                .collect::<BTreeSet<_>>();
            let forbidden_vertices = graph
                .vertices
                .iter()
                .filter_map(|vertex| {
                    (vertex.id != from
                        && vertex.id != target
                        && entropy
                            .rotate_right(
                                u32::try_from(vertex.id.0 % 64).expect("rotation is below 64"),
                            )
                            .trailing_zeros()
                            >= 6)
                        .then_some(vertex.id)
                })
                .collect::<BTreeSet<_>>();
            let request = RouteRequest {
                from,
                target,
                previous: None,
                law,
                cost_ceiling: (trial % 5 == 0).then_some(2_000.0),
                forbidden_edges: Some(&forbidden_edges),
                forbidden_vertices: Some(&forbidden_vertices),
            };

            let expected = reference_cost(&graph, request);
            let actual = router
                .shortest_path(&graph, &mut workspace, request)
                .map(|path| path_cost(&graph, request, &path));
            assert_eq!(actual.is_some(), expected.is_some(), "trial {trial}");
            if let (Some(actual), Some(expected)) = (actual, expected) {
                assert!((actual - expected).abs() < 1.0e-9, "trial {trial}");
            }
        }
    }

    #[test]
    fn equal_cost_routing_is_reproducible() {
        let drafts =
            geojson::network_from_str(include_str!("../tests/fixtures/mini_network.geojson"))
                .unwrap();
        let graph = GraphBuilder::default().build(&drafts).unwrap();
        let router = WalkRouter::forge(&graph);
        let mut workspace = router.workspace(&graph);
        let request = RouteRequest {
            from: VertexId(0),
            target: VertexId(graph.vertices.len() - 1),
            previous: None,
            law: RoutingLaw::default(),
            cost_ceiling: None,
            forbidden_edges: None,
            forbidden_vertices: None,
        };
        let expected = router
            .shortest_path(&graph, &mut workspace, request)
            .unwrap();

        for _ in 0..64 {
            assert_eq!(
                router.shortest_path(&graph, &mut workspace, request),
                Some(expected.clone())
            );
        }
    }

    fn path_cost(graph: &WalkGraph, request: RouteRequest<'_>, path: &[EdgeId]) -> f64 {
        path.iter()
            .map(|edge| request.law.edge_cost(graph, *edge).unwrap())
            .sum()
    }

    fn reference_cost(graph: &WalkGraph, request: RouteRequest<'_>) -> Option<f64> {
        if request.from == request.target {
            return Some(0.0);
        }
        let origin = (request.from, request.previous);
        let mut labels = BTreeMap::<(VertexId, Option<EdgeId>), f64>::from([(origin, 0.0)]);
        let mut settled = BTreeSet::new();
        loop {
            let (&state, &cost) = labels
                .iter()
                .filter(|(state, _)| !settled.contains(*state))
                .min_by(|(left_state, left), (right_state, right)| {
                    (**left)
                        .total_cmp(right)
                        .then_with(|| left_state.cmp(right_state))
                })?;
            settled.insert(state);
            let (at, previous) = state;
            if at == request.target {
                return Some(cost);
            }
            for edge in graph.adjacency[at.0].iter().copied() {
                if request
                    .forbidden_edges
                    .is_some_and(|forbidden| forbidden.contains(&edge))
                    || !graph.turn_allowed(previous, at, edge)
                {
                    continue;
                }
                let Some(next) = graph.edges[edge.0].traverse(at) else {
                    continue;
                };
                if next != request.target
                    && request
                        .forbidden_vertices
                        .is_some_and(|forbidden| forbidden.contains(&next))
                {
                    continue;
                }
                let Some(step_cost) = request.law.edge_cost(graph, edge) else {
                    continue;
                };
                let next_cost = cost + step_cost;
                if request
                    .cost_ceiling
                    .is_some_and(|ceiling| next_cost > ceiling)
                {
                    continue;
                }
                let next_state = (next, Some(edge));
                if labels
                    .get(&next_state)
                    .is_none_or(|known| next_cost < *known)
                {
                    labels.insert(next_state, next_cost);
                }
            }
        }
    }
}
