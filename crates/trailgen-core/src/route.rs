use crate::constraints::{ConstraintVerdict, LoopConstraints};
use crate::difficulty::DifficultyBreakdown;
use crate::geo::LineString;
use crate::model::{Access, CrossingKind, EdgeId, Terrain, TrailGraph, VertexId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const LOW_CONFIDENCE_THRESHOLD: f64 = 0.6;

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum RouteShape {
    #[default]
    Loop,
    FigureEight,
    OutAndBack,
    Open,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Route {
    pub name: String,
    pub start: VertexId,
    pub edges: Vec<EdgeId>,
    #[serde(default)]
    pub pareto_rank: u32,
    pub metrics: RouteMetrics,
    pub verdict: ConstraintVerdict,
    #[serde(default)]
    pub score: f64,
}

impl Route {
    #[must_use]
    pub fn from_edges(
        name: impl Into<String>,
        graph: &TrailGraph,
        start: VertexId,
        edges: Vec<EdgeId>,
        constraints: &LoopConstraints,
    ) -> Self {
        let metrics = RouteMetrics::measure(graph, start, &edges);
        let verdict = constraints.judge(&metrics);
        let score = route_score(&metrics, &verdict);
        Self {
            name: name.into(),
            start,
            edges,
            pareto_rank: 0,
            metrics,
            verdict,
            score,
        }
    }

    #[must_use]
    pub fn computed_score(&self) -> f64 {
        route_score(&self.metrics, &self.verdict)
    }

    #[must_use]
    pub fn geometry(&self, graph: &TrailGraph) -> LineString {
        let mut at = self.start;
        let mut points = Vec::new();
        for edge_id in &self.edges {
            let edge = &graph.edges[edge_id.0];
            let line = edge.oriented_geometry(at);
            if points.is_empty() {
                points.extend(line.points.iter().copied());
            } else {
                points.extend(line.points.iter().skip(1).copied());
            }
            at = edge.traverse(at).expect("route edge must be traversable");
        }
        LineString::unchecked(points)
    }
}

#[must_use]
pub fn route_score(metrics: &RouteMetrics, verdict: &ConstraintVerdict) -> f64 {
    metrics
        .low_confidence_fraction
        .mul_add(10.0, metrics.difficulty.mul_add(0.05, verdict.penalty))
}

pub fn rank_routes(routes: &mut [Route], constraints: &LoopConstraints) {
    for route in routes.iter_mut() {
        route.score = route.computed_score();
    }
    let points = routes
        .iter()
        .map(|route| ParetoPoint::from_route(route, constraints))
        .collect::<Vec<_>>();
    let mut unranked = (0..routes.len()).collect::<Vec<_>>();
    let mut rank = 1u32;
    while !unranked.is_empty() {
        let front = unranked
            .iter()
            .copied()
            .filter(|&i| {
                !unranked
                    .iter()
                    .any(|&j| i != j && points[j].dominates(points[i]))
            })
            .collect::<Vec<_>>();
        for i in &front {
            routes[*i].pareto_rank = rank;
        }
        unranked.retain(|i| !front.contains(i));
        rank += 1;
    }
    routes.sort_by(|a, b| {
        a.pareto_rank
            .cmp(&b.pareto_rank)
            .then_with(|| a.computed_score().total_cmp(&b.computed_score()))
    });
}

#[derive(Clone, Copy, Debug)]
struct ParetoPoint {
    constraint_penalty: f64,
    distance_deviation_m: f64,
    ascent_deviation_m: f64,
    descent_deviation_m: f64,
    difficulty: f64,
    road_fraction: f64,
    low_confidence_fraction: f64,
    restricted_access_fraction: f64,
    repeated_edge_fraction: f64,
}

impl ParetoPoint {
    fn from_route(route: &Route, constraints: &LoopConstraints) -> Self {
        let m = &route.metrics;
        Self {
            constraint_penalty: route.verdict.penalty,
            distance_deviation_m: range_deviation(
                m.distance_m,
                constraints.min_distance_m,
                constraints.max_distance_m,
            ),
            ascent_deviation_m: range_deviation(
                m.ascent_m,
                constraints.min_ascent_m,
                constraints.max_ascent_m,
            ),
            descent_deviation_m: range_deviation(
                m.descent_m,
                constraints.min_descent_m,
                constraints.max_descent_m,
            ),
            difficulty: m.difficulty,
            road_fraction: m.road_fraction,
            low_confidence_fraction: m.low_confidence_fraction,
            restricted_access_fraction: m.restricted_access_fraction,
            repeated_edge_fraction: m.repeated_edge_fraction,
        }
    }

    fn dominates(self, rhs: Self) -> bool {
        self.objectives()
            .into_iter()
            .zip(rhs.objectives())
            .all(|(a, b)| a <= b)
            && self
                .objectives()
                .into_iter()
                .zip(rhs.objectives())
                .any(|(a, b)| a < b)
    }

    const fn objectives(self) -> [f64; 9] {
        [
            self.constraint_penalty,
            self.distance_deviation_m,
            self.ascent_deviation_m,
            self.descent_deviation_m,
            self.difficulty,
            self.road_fraction,
            self.low_confidence_fraction,
            self.restricted_access_fraction,
            self.repeated_edge_fraction,
        ]
    }
}

fn range_deviation(value: f64, min: f64, max: f64) -> f64 {
    if value < min {
        min - value
    } else if value > max {
        value - max
    } else {
        0.0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteMetrics {
    #[serde(default)]
    pub shape: RouteShape,
    pub distance_m: f64,
    pub ascent_m: f64,
    pub descent_m: f64,
    pub difficulty: f64,
    #[serde(default)]
    pub difficulty_breakdown: DifficultyBreakdown,
    pub road_fraction: f64,
    pub low_confidence_fraction: f64,
    #[serde(default)]
    pub restricted_access_fraction: f64,
    pub repeated_edge_fraction: f64,
    #[serde(default)]
    pub crossings: BTreeMap<CrossingKind, u32>,
    #[serde(default)]
    pub access_m: BTreeMap<Access, f64>,
    pub terrain_m: BTreeMap<Terrain, f64>,
}

impl RouteMetrics {
    #[must_use]
    pub fn measure(graph: &TrailGraph, start: VertexId, edges: &[EdgeId]) -> Self {
        let mut m = Self::default();
        let mut seen = BTreeMap::<EdgeId, usize>::new();
        let mut vertex_visits = BTreeMap::<VertexId, usize>::from([(start, 1)]);
        let mut at = start;
        let mut road_m = 0.0;
        let mut low_conf_m = 0.0;
        let mut restricted_access_m = 0.0;
        let mut repeated_edge_m = 0.0;
        for edge_id in edges {
            let edge = &graph.edges[edge_id.0];
            let from = at;
            at = edge.traverse(from).expect("route edge must be traversable");
            *vertex_visits.entry(at).or_default() += 1;
            let a = &edge.attr;
            m.distance_m += a.length_m;
            let (ascent_m, descent_m) = if from == edge.a {
                (a.ascent_m, a.descent_m)
            } else {
                (a.descent_m, a.ascent_m)
            };
            m.ascent_m += ascent_m;
            m.descent_m += descent_m;
            m.difficulty += a.difficulty;
            m.difficulty_breakdown += a.difficulty_breakdown;
            road_m = a
                .length_m
                .mul_add(road_pavement_exposure(a.terrain, a.road_exposure), road_m);
            if a.confidence < LOW_CONFIDENCE_THRESHOLD {
                low_conf_m += a.length_m;
            }
            if is_restricted_access(a.access) {
                restricted_access_m += a.length_m;
            }
            for crossing in &a.crossings {
                *m.crossings.entry(crossing.kind).or_default() += crossing.count;
            }
            *m.access_m.entry(a.access).or_default() += a.length_m;
            *m.terrain_m.entry(a.terrain).or_default() += a.length_m;
            let n = seen.entry(*edge_id).or_default();
            if *n > 0 {
                repeated_edge_m += a.length_m;
            }
            *n += 1;
        }
        if m.distance_m > 0.0 {
            m.road_fraction = road_m / m.distance_m;
            m.low_confidence_fraction = low_conf_m / m.distance_m;
            m.restricted_access_fraction = restricted_access_m / m.distance_m;
            m.repeated_edge_fraction = repeated_edge_m / m.distance_m;
        }
        m.shape = classify_shape(start, at, repeated_edge_m, &vertex_visits);
        m
    }

    #[must_use]
    pub fn terrain_percentages(&self) -> BTreeMap<Terrain, f64> {
        self.terrain_m
            .iter()
            .map(|(terrain, meters)| (*terrain, meters / self.distance_m.max(1.0)))
            .collect()
    }

    #[must_use]
    pub fn access_percentages(&self) -> BTreeMap<Access, f64> {
        self.access_m
            .iter()
            .map(|(access, meters)| (*access, meters / self.distance_m.max(1.0)))
            .collect()
    }
}

#[must_use]
pub const fn is_restricted_access(access: Access) -> bool {
    matches!(
        access,
        Access::Restricted | Access::Closed | Access::Private
    )
}

const fn road_pavement_exposure(terrain: Terrain, road_exposure: f64) -> f64 {
    road_exposure
        .clamp(0.0, 1.0)
        .max(if matches!(terrain, Terrain::Pavement | Terrain::Road) {
            1.0
        } else {
            0.0
        })
}

fn classify_shape(
    start: VertexId,
    end: VertexId,
    repeated_edge_m: f64,
    vertex_visits: &BTreeMap<VertexId, usize>,
) -> RouteShape {
    if start != end {
        return RouteShape::Open;
    }
    if repeated_edge_m > 0.0 {
        return RouteShape::OutAndBack;
    }
    if vertex_visits
        .iter()
        .any(|(vertex, visits)| *visits > usize::from(*vertex == start) + 1)
    {
        return RouteShape::FigureEight;
    }
    RouteShape::Loop
}
