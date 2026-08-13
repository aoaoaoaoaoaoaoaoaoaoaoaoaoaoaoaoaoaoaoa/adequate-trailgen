use crate::{
    Access, Coord, Edge, EdgeAttr, EdgeId, EdgeIndex, EdgeProjection, GradeDistribution,
    HikingModel, LineString, LoopConstraints, Route, RouteShape, Terrain, TrailgenError, TurnBan,
    Vertex, VertexId, WalkGraph,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
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
    pub fn edge_cost(self, graph: &WalkGraph, edge: EdgeId) -> Option<f64> {
        let attr = &graph.edges[edge.0].attr;
        if matches!(attr.access, Access::Closed | Access::Private) {
            return None;
        }
        let road =
            attr.road_exposure
                .clamp(0.0, 1.0)
                .max(if matches!(attr.terrain, Terrain::Road) {
                    1.0
                } else {
                    0.0
                });
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
    /// interior support so the design compels that physical segment.
    #[must_use]
    pub fn infer(graph: &WalkGraph, route: &Route, routing: RoutingLaw) -> Option<Self> {
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
        graph: &WalkGraph,
        constraints: &LoopConstraints,
        max_snap_m: f64,
    ) -> crate::Result<TrailRealization> {
        let index = EdgeIndex::forge(graph);
        let router = crate::WalkRouter::forge(graph);
        self.realize_indexed(name, graph, &index, &router, constraints, max_snap_m)
    }

    pub fn realize_indexed(
        &self,
        name: impl Into<String>,
        graph: &WalkGraph,
        index: &EdgeIndex,
        router: &crate::WalkRouter,
        constraints: &LoopConstraints,
        max_snap_m: f64,
    ) -> crate::Result<TrailRealization> {
        self.validate()?;
        if !max_snap_m.is_finite() || max_snap_m <= 0.0 {
            return Err(TrailgenError::InvalidData(
                "support-point snap distance must be positive".to_owned(),
            ));
        }
        let anchors = bind_projections(graph, index, &self.support_points, max_snap_m)?;
        let RoutedDesign {
            spans,
            support_span_offsets,
        } = self.strike_spans(graph, router, &anchors)?;
        let realized = materialize_walk(graph, &anchors, &spans, &support_span_offsets)?;
        let start = realized.bindings[0].vertex;
        let end = realized
            .graph
            .walk_edges(start, &realized.edges)
            .ok_or_else(|| {
                TrailgenError::InvalidData(
                    "support points induce an illegal directed trail".to_owned(),
                )
            })?;
        let route = Route::from_edges(name, &realized.graph, start, realized.edges, constraints);
        if !walk_fulfills_design(self.shape, route.metrics.shape, start == end) {
            return Err(TrailgenError::ShapeMismatch {
                actual: route.metrics.shape,
                expected: self.shape,
            });
        }
        Ok(TrailRealization {
            trail: Self {
                shape: self.shape,
                support_points: realized
                    .bindings
                    .iter()
                    .map(|binding| binding.anchor)
                    .collect(),
                routing: self.routing,
            },
            bindings: realized.bindings,
            route,
            walk: Arc::new(realized.graph),
            support_offsets: realized.support_offsets,
            source_spans: spans,
            support_span_offsets,
        })
    }

    fn strike_spans(
        &self,
        graph: &WalkGraph,
        router: &crate::WalkRouter,
        anchors: &[BoundProjection],
    ) -> crate::Result<RoutedDesign> {
        let mut workspace = router.workspace(graph);
        let mut forge = PathForge {
            router,
            workspace: &mut workspace,
            graph,
            law: self.routing,
        };
        let mut spans = Vec::new();
        let mut support_span_offsets = vec![0];
        let mut previous = None;
        for targets in anchors.windows(2) {
            let leg = forge
                .route(
                    targets[0].projection,
                    targets[1].projection,
                    previous,
                    PathVeto::default(),
                )
                .ok_or_else(|| {
                    TrailgenError::InvalidData(
                        "no lawful trail connects consecutive support points".to_owned(),
                    )
                })?;
            previous = leg.last().map(|span| span.edge).or(previous);
            spans.extend(leg);
            support_span_offsets.push(spans.len());
        }
        match self.shape {
            RouteShape::Open => {}
            RouteShape::OutAndBack => {
                let mut reverse = spans
                    .iter()
                    .rev()
                    .map(|span| span.reversed())
                    .collect::<Vec<_>>();
                spans.append(&mut reverse);
            }
            RouteShape::Loop => {
                let forbidden_edges = spans.iter().map(|span| span.edge).collect::<BTreeSet<_>>();
                let forbidden_vertices = walked_span_vertices(graph, &spans);
                let return_path = forge
                    .route(
                        anchors.last().expect("validated anchors").projection,
                        anchors[0].projection,
                        previous,
                        PathVeto {
                            edges: Some(&forbidden_edges),
                            vertices: Some(&forbidden_vertices),
                            ..PathVeto::default()
                        },
                    )
                    // Prefer a simple closure, then admit the shortest lawful
                    // closed walk when the network topology compels a retrace.
                    .or_else(|| {
                        forge.route(
                            anchors.last().expect("validated anchors").projection,
                            anchors[0].projection,
                            previous,
                            PathVeto::default(),
                        )
                    })
                    .ok_or_else(|| {
                        TrailgenError::InvalidData(
                            "no lawful return connects the final support point".to_owned(),
                        )
                    })?;
                spans.extend(return_path);
            }
            RouteShape::FigureEight => unreachable!("figure-eight rejected by validation"),
        }
        Ok(RoutedDesign {
            spans,
            support_span_offsets,
        })
    }
}

struct RoutedDesign {
    spans: Vec<PartialSpan>,
    support_span_offsets: Vec<usize>,
}

/// A trail shape is design intent; route shape is observed walk morphology.
/// In particular, a manually designed loop may revisit a junction or retrace
/// an edge while still fulfilling its sole topological promise: returning to
/// its trailhead.
fn walk_fulfills_design(design: RouteShape, morphology: RouteShape, closed: bool) -> bool {
    match design {
        RouteShape::Loop => closed,
        RouteShape::Open | RouteShape::OutAndBack => morphology == design,
        RouteShape::FigureEight => false,
    }
}

fn route_vertices(graph: &WalkGraph, route: &Route) -> Option<Vec<VertexId>> {
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
    graph: &WalkGraph,
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

fn compress_span_arc(
    forge: &mut PathForge<'_>,
    spans: &[PartialSpan],
    lo: usize,
    hi: usize,
    supports: &mut Vec<SupportPoint>,
) -> Option<()> {
    let start = span_projection(forge.graph, spans[lo], true);
    let target = span_projection(forge.graph, spans[hi - 1], false);
    let previous = lo.checked_sub(1).map(|slot| spans[slot].edge);
    if forge
        .route(start, target, previous, PathVeto::default())
        .is_some_and(|shortest| same_spans(&shortest, &spans[lo..hi]))
    {
        supports.push(SupportPoint::forge(start.coord)?);
        return Some(());
    }
    if hi - lo <= 1 {
        supports.push(SupportPoint::forge(start.coord)?);
        let span = spans[lo];
        supports.push(SupportPoint::forge(line_coord_at(
            &forge.graph.edges[span.edge.0].geometry,
            (span.from_m + span.to_m) * 0.5,
        ))?);
        return Some(());
    }
    let split = lo + (hi - lo) / 2;
    compress_span_arc(forge, spans, lo, split, supports)?;
    compress_span_arc(forge, spans, split, hi, supports)
}

fn span_projection(graph: &WalkGraph, span: PartialSpan, start: bool) -> EdgeProjection {
    let progress_m = if start { span.from_m } else { span.to_m };
    EdgeProjection {
        edge: span.edge,
        coord: line_coord_at(&graph.edges[span.edge.0].geometry, progress_m),
        progress_m,
        distance_m: 0.0,
    }
}

fn same_spans(left: &[PartialSpan], right: &[PartialSpan]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.edge == right.edge
                && (left.from_m - right.from_m).abs() <= SUPPORT_EPSILON_M
                && (left.to_m - right.to_m).abs() <= SUPPORT_EPSILON_M
        })
}

fn vertex_support(graph: &WalkGraph, vertex: VertexId) -> Option<SupportPoint> {
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
    walk: Arc<WalkGraph>,
    support_offsets: Vec<usize>,
    source_spans: Vec<PartialSpan>,
    support_span_offsets: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrailReversal {
    pub trail: Trail,
    pub added_supports: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SupportInsertion {
    pub slot: usize,
    pub distance_m: f64,
}

impl TrailRealization {
    #[must_use]
    pub fn graph(&self) -> &WalkGraph {
        &self.walk
    }

    /// Direction-sensitive identity for this realization's ordered physical
    /// walk. The value is an ephemeral comparison key, not durable project
    /// identity or a cryptographic digest.
    #[must_use]
    pub fn walk_fingerprint(&self) -> u64 {
        self.source_spans.iter().fold(
            (self.source_spans.len() as u64) ^ 0xa076_1d64_78bd_642f,
            |state, span| {
                [
                    span.edge.0 as u64,
                    span.from_m.to_bits(),
                    span.to_m.to_bits(),
                ]
                .into_iter()
                .fold(state, |state, word| {
                    (state ^ word.wrapping_mul(0xe703_7ed1_a0b4_28db))
                        .rotate_left(23)
                        .wrapping_mul(0x8ebc_6af0_9c88_c6e3)
                })
            },
        )
    }

    /// Inverts a loop's exact physical walk while retaining every existing
    /// support. The support tail is reversed; compact repairs are inserted
    /// only where that program would otherwise select another path.
    pub fn reverse_loop(&self, source: &WalkGraph) -> crate::Result<TrailReversal> {
        if self.trail.shape != RouteShape::Loop {
            return Err(TrailgenError::ShapeMismatch {
                actual: self.trail.shape,
                expected: RouteShape::Loop,
            });
        }
        if self.source_spans.iter().any(|span| {
            source
                .edges
                .get(span.edge.0)
                .is_none_or(|edge| edge.attr.travel != crate::EdgeTravel::Both)
        }) {
            return Err(TrailgenError::OneWayReversal);
        }

        let reversed = self
            .source_spans
            .iter()
            .rev()
            .map(|span| span.reversed())
            .collect::<Vec<_>>();
        let span_count = reversed.len();
        debug_assert_eq!(
            self.support_span_offsets.len(),
            self.trail.support_points.len()
        );

        let fixed = std::iter::once((0, self.trail.support_points[0]))
            .chain((1..self.trail.support_points.len()).rev().map(|slot| {
                (
                    span_count - self.support_span_offsets[slot],
                    self.trail.support_points[slot],
                )
            }))
            .collect::<Vec<_>>();
        let mut support_points = Vec::with_capacity(fixed.len());
        support_points.push(fixed[0].1);
        let mut added_supports = 0;
        let router = crate::WalkRouter::forge(source);
        let mut workspace = router.workspace(source);
        let mut forge = PathForge {
            router: &router,
            workspace: &mut workspace,
            graph: source,
            law: self.trail.routing,
        };
        for slot in 0..fixed.len() {
            let lo = fixed[slot].0;
            let hi = fixed.get(slot + 1).map_or(span_count, |next| next.0);
            if lo < hi {
                let mut repaired = Vec::new();
                compress_span_arc(&mut forge, &reversed, lo, hi, &mut repaired)
                    .ok_or(TrailgenError::UnrepresentableReversal)?;
                debug_assert_eq!(repaired.first(), Some(&fixed[slot].1));
                added_supports += repaired.len() - 1;
                support_points.extend(repaired.into_iter().skip(1));
            }
            if let Some(next) = fixed.get(slot + 1) {
                support_points.push(next.1);
            }
        }

        Ok(TrailReversal {
            trail: Trail::forge(RouteShape::Loop, support_points, self.trail.routing)?,
            added_supports,
        })
    }

    /// Locates a new support in the design order of the realized walk. Loops
    /// admit their closing arc after the final explicit support; out-and-backs
    /// index only their outward spine because the return is its exact reverse.
    #[must_use]
    pub fn support_insertion(&self, requested: Coord) -> Option<SupportInsertion> {
        let graph = self.graph();
        let routed = if self.trail.shape == RouteShape::OutAndBack {
            *self.support_offsets.last()?
        } else {
            self.route.edges.len()
        };
        let (edge_slot, distance_m) = self
            .route
            .edges
            .iter()
            .take(routed)
            .enumerate()
            .filter_map(|(slot, edge)| {
                crate::model::line_projection(&graph.edges[edge.0].geometry, requested)
                    .map(|(distance_m, _, _)| (slot, distance_m))
            })
            .min_by(|left, right| left.1.total_cmp(&right.1))?;
        let slot = self.support_offsets[1..].partition_point(|offset| *offset <= edge_slot) + 1;
        Some(SupportInsertion { slot, distance_m })
    }
}

const SUPPORT_EPSILON_M: f64 = 0.05;

#[derive(Clone, Copy)]
struct BoundProjection {
    requested: SupportPoint,
    projection: EdgeProjection,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PartialSpan {
    edge: EdgeId,
    from_m: f64,
    to_m: f64,
}

impl PartialSpan {
    const fn reversed(self) -> Self {
        Self {
            edge: self.edge,
            from_m: self.to_m,
            to_m: self.from_m,
        }
    }
}

fn bind_projections(
    graph: &WalkGraph,
    index: &EdgeIndex,
    supports: &[SupportPoint],
    max_snap_m: f64,
) -> crate::Result<Vec<BoundProjection>> {
    supports
        .iter()
        .copied()
        .map(|requested| {
            let projection = index.project(graph, requested.coord()).ok_or_else(|| {
                TrailgenError::InvalidData(
                    "cannot bind a support point to an empty network".to_owned(),
                )
            })?;
            if projection.distance_m > max_snap_m {
                return Err(TrailgenError::InvalidData(format!(
                    "support point lies {:.0} m from the walking network",
                    projection.distance_m
                )));
            }
            Ok(BoundProjection {
                requested,
                projection,
            })
        })
        .collect()
}

#[derive(Clone, Copy)]
struct EndpointRoute {
    vertex: VertexId,
    span: Option<PartialSpan>,
    cost: f64,
    previous: Option<EdgeId>,
}

struct PathForge<'a> {
    router: &'a crate::WalkRouter,
    workspace: &'a mut crate::RoutingWorkspace,
    graph: &'a WalkGraph,
    law: RoutingLaw,
}

impl PathForge<'_> {
    fn route(
        &mut self,
        from: EdgeProjection,
        target: EdgeProjection,
        previous: Option<EdgeId>,
        veto: PathVeto<'_>,
    ) -> Option<Vec<PartialSpan>> {
        let mut alternatives = Vec::<(f64, Vec<PartialSpan>)>::new();
        if from.edge == target.edge
            && span_allowed(self.graph, from.edge, from.progress_m, target.progress_m)
            && veto.edges.is_none_or(|edges| !edges.contains(&from.edge))
        {
            let edge = &self.graph.edges[from.edge.0];
            let at_endpoint = endpoint_at(edge, from.progress_m);
            if at_endpoint.is_none_or(|via| self.graph.turn_allowed(previous, via, from.edge)) {
                let span = PartialSpan {
                    edge: from.edge,
                    from_m: from.progress_m,
                    to_m: target.progress_m,
                };
                if let Some(cost) = partial_cost(self.graph, self.law, span)
                    && veto.cost_ceiling.is_none_or(|ceiling| cost <= ceiling)
                {
                    alternatives.push((cost, vec![span]));
                }
            }
        }

        for departure in departure_routes(self.graph, from, previous, self.law) {
            if departure
                .span
                .is_some_and(|span| veto.edges.is_some_and(|edges| edges.contains(&span.edge)))
            {
                continue;
            }
            for arrival in arrival_routes(self.graph, target, self.law) {
                if arrival
                    .span
                    .is_some_and(|span| veto.edges.is_some_and(|edges| edges.contains(&span.edge)))
                {
                    continue;
                }
                let fixed_cost = departure.cost + arrival.cost;
                let ceiling = veto.cost_ceiling.map(|ceiling| ceiling - fixed_cost);
                if ceiling.is_some_and(|ceiling| ceiling < 0.0) {
                    continue;
                }
                let Some(path) = self.router.shortest_path(
                    self.graph,
                    self.workspace,
                    crate::RouteRequest {
                        from: departure.vertex,
                        target: arrival.vertex,
                        previous: departure.previous,
                        law: self.law,
                        cost_ceiling: ceiling,
                        forbidden_edges: veto.edges,
                        forbidden_vertices: veto.vertices,
                    },
                ) else {
                    continue;
                };
                if let Some(span) = arrival.span
                    && !self.graph.turn_allowed(
                        path.last().copied().or(departure.previous),
                        arrival.vertex,
                        span.edge,
                    )
                {
                    continue;
                }
                let mut spans = Vec::with_capacity(path.len() + 2);
                if let Some(prefix) = departure.span {
                    spans.push(prefix);
                }
                append_full_spans(self.graph, departure.vertex, &path, &mut spans)?;
                if let Some(suffix) = arrival.span {
                    spans.push(suffix);
                }
                let cost = fixed_cost
                    + path
                        .iter()
                        .map(|edge| self.law.edge_cost(self.graph, *edge))
                        .sum::<Option<f64>>()?;
                alternatives.push((cost, spans));
            }
        }
        alternatives
            .into_iter()
            .min_by(|left, right| {
                left.0
                    .total_cmp(&right.0)
                    .then_with(|| span_key(&left.1).cmp(&span_key(&right.1)))
            })
            .map(|(_, spans)| spans)
    }
}

fn departure_routes(
    graph: &WalkGraph,
    projection: EdgeProjection,
    previous: Option<EdgeId>,
    law: RoutingLaw,
) -> Vec<EndpointRoute> {
    let edge = &graph.edges[projection.edge.0];
    if let Some(vertex) = endpoint_at(edge, projection.progress_m) {
        return vec![EndpointRoute {
            vertex,
            span: None,
            cost: 0.0,
            previous,
        }];
    }
    [(edge.a, 0.0), (edge.b, edge.geometry.length_m())]
        .into_iter()
        .filter_map(|(vertex, to_m)| {
            let span = PartialSpan {
                edge: projection.edge,
                from_m: projection.progress_m,
                to_m,
            };
            if !span_allowed(graph, span.edge, span.from_m, span.to_m) {
                return None;
            }
            Some(EndpointRoute {
                vertex,
                span: Some(span),
                cost: partial_cost(graph, law, span)?,
                previous: Some(span.edge),
            })
        })
        .collect()
}

fn arrival_routes(
    graph: &WalkGraph,
    projection: EdgeProjection,
    law: RoutingLaw,
) -> Vec<EndpointRoute> {
    let edge = &graph.edges[projection.edge.0];
    if let Some(vertex) = endpoint_at(edge, projection.progress_m) {
        return vec![EndpointRoute {
            vertex,
            span: None,
            cost: 0.0,
            previous: None,
        }];
    }
    [(edge.a, 0.0), (edge.b, edge.geometry.length_m())]
        .into_iter()
        .filter_map(|(vertex, from_m)| {
            let span = PartialSpan {
                edge: projection.edge,
                from_m,
                to_m: projection.progress_m,
            };
            if !span_allowed(graph, span.edge, span.from_m, span.to_m) {
                return None;
            }
            Some(EndpointRoute {
                vertex,
                span: Some(span),
                cost: partial_cost(graph, law, span)?,
                previous: None,
            })
        })
        .collect()
}

fn span_allowed(graph: &WalkGraph, edge: EdgeId, from_m: f64, to_m: f64) -> bool {
    use crate::EdgeTravel;
    match graph.edges[edge.0].attr.travel {
        EdgeTravel::Both => true,
        EdgeTravel::Forward => to_m >= from_m,
        EdgeTravel::Backward => to_m <= from_m,
    }
}

fn partial_cost(graph: &WalkGraph, law: RoutingLaw, span: PartialSpan) -> Option<f64> {
    let full_m = graph.edges[span.edge.0].geometry.length_m();
    let ratio = (span.to_m - span.from_m).abs() / full_m.max(f64::EPSILON);
    Some(law.edge_cost(graph, span.edge)? * ratio)
}

fn endpoint_at(edge: &Edge, progress_m: f64) -> Option<VertexId> {
    if progress_m <= SUPPORT_EPSILON_M {
        Some(edge.a)
    } else if edge.geometry.length_m() - progress_m <= SUPPORT_EPSILON_M {
        Some(edge.b)
    } else {
        None
    }
}

fn append_full_spans(
    graph: &WalkGraph,
    mut at: VertexId,
    path: &[EdgeId],
    spans: &mut Vec<PartialSpan>,
) -> Option<()> {
    for edge_id in path {
        let edge = &graph.edges[edge_id.0];
        let to = edge.traverse(at)?;
        spans.push(PartialSpan {
            edge: *edge_id,
            from_m: if at == edge.a {
                0.0
            } else {
                edge.geometry.length_m()
            },
            to_m: if to == edge.b {
                edge.geometry.length_m()
            } else {
                0.0
            },
        });
        at = to;
    }
    Some(())
}

fn span_key(spans: &[PartialSpan]) -> Vec<(EdgeId, u64, u64)> {
    spans
        .iter()
        .map(|span| (span.edge, span.from_m.to_bits(), span.to_m.to_bits()))
        .collect()
}

fn walked_span_vertices(graph: &WalkGraph, spans: &[PartialSpan]) -> BTreeSet<VertexId> {
    spans
        .iter()
        .flat_map(|span| {
            let edge = &graph.edges[span.edge.0];
            [endpoint_at(edge, span.from_m), endpoint_at(edge, span.to_m)]
        })
        .flatten()
        .collect()
}

struct MaterializedWalk {
    graph: WalkGraph,
    bindings: Vec<SupportBinding>,
    edges: Vec<EdgeId>,
    support_offsets: Vec<usize>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum LocalVertex {
    Base(VertexId),
    Interior(EdgeId, u64),
}

fn materialize_walk(
    source: &WalkGraph,
    anchors: &[BoundProjection],
    spans: &[PartialSpan],
    support_span_offsets: &[usize],
) -> crate::Result<MaterializedWalk> {
    let mut cuts = BTreeMap::<EdgeId, Vec<f64>>::new();
    for span in spans {
        cuts.entry(span.edge)
            .or_default()
            .extend([span.from_m, span.to_m]);
    }
    for anchor in anchors {
        cuts.entry(anchor.projection.edge)
            .or_default()
            .push(anchor.projection.progress_m);
    }
    for (edge, marks) in &mut cuts {
        let length_m = source.edges[edge.0].geometry.length_m();
        marks.extend([0.0, length_m]);
        marks.sort_by(f64::total_cmp);
        marks.dedup_by(|left, right| (*left - *right).abs() <= SUPPORT_EPSILON_M);
    }

    let mut vertices = Vec::new();
    let mut vertex_ids = BTreeMap::<LocalVertex, VertexId>::new();
    let mut edges = Vec::new();
    let mut edge_ids = BTreeMap::<(EdgeId, u64, u64), EdgeId>::new();
    let mut lineage = Vec::new();
    let mut route = Vec::new();
    let mut span_offsets = Vec::with_capacity(spans.len() + 1);
    span_offsets.push(0);
    for span in spans {
        let marks = &cuts[&span.edge];
        let from = canonical_mark(marks, span.from_m);
        let to = canonical_mark(marks, span.to_m);
        let lo = from.min(to);
        let hi = from.max(to);
        let mut intervals = marks
            .windows(2)
            .filter(|pair| pair[0] >= lo - SUPPORT_EPSILON_M && pair[1] <= hi + SUPPORT_EPSILON_M)
            .map(|pair| (pair[0], pair[1]))
            .collect::<Vec<_>>();
        if to < from {
            intervals.reverse();
        }
        for (lo, hi) in intervals {
            let edge = materialized_edge(
                source,
                span.edge,
                lo,
                hi,
                &mut vertices,
                &mut vertex_ids,
                &mut edges,
                &mut edge_ids,
                &mut lineage,
            );
            route.push(edge);
        }
        span_offsets.push(route.len());
    }

    let bindings = anchors
        .iter()
        .map(|bound| {
            let marks = &cuts[&bound.projection.edge];
            let progress_m = canonical_mark(marks, bound.projection.progress_m);
            let vertex = materialized_vertex(
                source,
                bound.projection.edge,
                progress_m,
                &mut vertices,
                &mut vertex_ids,
            );
            let anchor = SupportPoint(line_coord_at(
                &source.edges[bound.projection.edge.0].geometry,
                progress_m,
            ));
            SupportBinding {
                requested: bound.requested,
                anchor,
                vertex,
                snap_m: bound.requested.coord().haversine_m(anchor.coord()),
            }
        })
        .collect::<Vec<_>>();
    let support_offsets = support_span_offsets
        .iter()
        .map(|offset| span_offsets[*offset])
        .collect();
    let mut graph = WalkGraph::new(vertices, edges);
    graph.turn_bans = inherited_turn_bans(source, &graph, &lineage);
    graph.validate()?;
    Ok(MaterializedWalk {
        graph,
        bindings,
        edges: route,
        support_offsets,
    })
}

#[allow(clippy::too_many_arguments)]
fn materialized_edge(
    source: &WalkGraph,
    source_edge: EdgeId,
    lo: f64,
    hi: f64,
    vertices: &mut Vec<Vertex>,
    vertex_ids: &mut BTreeMap<LocalVertex, VertexId>,
    edges: &mut Vec<Edge>,
    edge_ids: &mut BTreeMap<(EdgeId, u64, u64), EdgeId>,
    lineage: &mut Vec<EdgeId>,
) -> EdgeId {
    let key = (source_edge, lo.to_bits(), hi.to_bits());
    if let Some(edge) = edge_ids.get(&key) {
        return *edge;
    }
    let a = materialized_vertex(source, source_edge, lo, vertices, vertex_ids);
    let b = materialized_vertex(source, source_edge, hi, vertices, vertex_ids);
    let original = &source.edges[source_edge.0];
    let span_m = original.geometry.length_m();
    let geometry = line_slice(&original.geometry, lo, hi);
    let carries_crossings = lo <= span_m * 0.5 && span_m * 0.5 < hi;
    let id = EdgeId(edges.len());
    edges.push(Edge {
        id,
        a,
        b,
        attr: cleaved_attr(original, &geometry, span_m, carries_crossings),
        geometry,
    });
    lineage.push(source_edge);
    edge_ids.insert(key, id);
    id
}

fn materialized_vertex(
    source: &WalkGraph,
    source_edge: EdgeId,
    progress_m: f64,
    vertices: &mut Vec<Vertex>,
    vertex_ids: &mut BTreeMap<LocalVertex, VertexId>,
) -> VertexId {
    let edge = &source.edges[source_edge.0];
    let key = if progress_m <= SUPPORT_EPSILON_M {
        LocalVertex::Base(edge.a)
    } else if edge.geometry.length_m() - progress_m <= SUPPORT_EPSILON_M {
        LocalVertex::Base(edge.b)
    } else {
        LocalVertex::Interior(source_edge, progress_m.to_bits())
    };
    if let Some(vertex) = vertex_ids.get(&key) {
        return *vertex;
    }
    let id = VertexId(vertices.len());
    let (coord, junction) = match key {
        LocalVertex::Base(base) => {
            let base = &source.vertices[base.0];
            (base.coord, base.junction.clone())
        }
        LocalVertex::Interior(_, _) => (line_coord_at(&edge.geometry, progress_m), None),
    };
    vertices.push(Vertex {
        id,
        coord,
        junction,
    });
    vertex_ids.insert(key, id);
    id
}

fn canonical_mark(marks: &[f64], progress_m: f64) -> f64 {
    marks
        .iter()
        .copied()
        .find(|mark| (*mark - progress_m).abs() <= SUPPORT_EPSILON_M)
        .unwrap_or(progress_m)
}

fn inherited_turn_bans(
    source: &WalkGraph,
    realized: &WalkGraph,
    lineage: &[EdgeId],
) -> Vec<TurnBan> {
    source
        .turn_bans
        .iter()
        .filter_map(|ban| {
            let via = realized
                .vertices
                .iter()
                .find(|vertex| {
                    vertex.coord == source.vertices[ban.via.0].coord
                        && vertex.junction == source.vertices[ban.via.0].junction
                })?
                .id;
            let incident = |source_edge: EdgeId| {
                lineage.iter().enumerate().find_map(|(slot, lineage)| {
                    (*lineage == source_edge
                        && [realized.edges[slot].a, realized.edges[slot].b].contains(&via))
                    .then_some(EdgeId(slot))
                })
            };
            Some(TurnBan {
                via,
                from: incident(ban.from)?,
                to: incident(ban.to)?,
                provenance: ban.provenance.clone(),
            })
        })
        .collect()
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
    attr.traversal = HikingModel.estimate(geometry, &attr);
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

#[derive(Clone, Copy, Default)]
struct PathVeto<'a> {
    cost_ceiling: Option<f64>,
    edges: Option<&'a BTreeSet<EdgeId>>,
    vertices: Option<&'a BTreeSet<VertexId>>,
}

pub(crate) fn shortest_path(
    graph: &WalkGraph,
    from: VertexId,
    target: VertexId,
    previous: Option<EdgeId>,
    law: RoutingLaw,
    max_cost: Option<f64>,
) -> Option<Vec<EdgeId>> {
    shortest_path_excluding(
        graph,
        from,
        target,
        previous,
        law,
        PathVeto {
            cost_ceiling: max_cost,
            ..PathVeto::default()
        },
    )
}

fn shortest_path_excluding(
    graph: &WalkGraph,
    from: VertexId,
    target: VertexId,
    previous: Option<EdgeId>,
    law: RoutingLaw,
    veto: PathVeto<'_>,
) -> Option<Vec<EdgeId>> {
    let router = crate::WalkRouter::forge(graph);
    let mut workspace = router.workspace(graph);
    route_path_excluding(
        &router,
        &mut workspace,
        graph,
        from,
        target,
        previous,
        law,
        veto.edges,
        veto.vertices,
        veto.cost_ceiling,
    )
}

#[allow(clippy::too_many_arguments)]
fn route_path_excluding(
    router: &crate::WalkRouter,
    workspace: &mut crate::RoutingWorkspace,
    graph: &WalkGraph,
    from: VertexId,
    target: VertexId,
    previous: Option<EdgeId>,
    law: RoutingLaw,
    forbidden_edges: Option<&BTreeSet<EdgeId>>,
    forbidden_vertices: Option<&BTreeSet<VertexId>>,
    cost_ceiling: Option<f64>,
) -> Option<Vec<EdgeId>> {
    router.shortest_path(
        graph,
        workspace,
        crate::RouteRequest {
            from,
            target,
            previous,
            law,
            cost_ceiling,
            forbidden_edges,
            forbidden_vertices,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CrossingControl, EdgeTravel, GeometryClaim, GraphBuilder, JunctionPolicy, LineString,
        Provenance, SegmentDraft, TrailStanding, WayKind, WayRealm, io::geojson,
    };

    fn graph() -> WalkGraph {
        GraphBuilder::default()
            .build(
                &geojson::network_from_str(include_str!("../tests/fixtures/mini_network.geojson"))
                    .expect("parse fixture"),
            )
            .expect("build fixture")
    }

    fn draft(name: &str, from: Coord, to: Coord) -> SegmentDraft {
        SegmentDraft {
            geometry: LineString::new(vec![from, to]).expect("valid line"),
            junctions: JunctionPolicy::Planar,
            turn_ref: None,
            junction_keys: None,
            turn_restrictions: Vec::new(),
            way_kind: WayKind::Path,
            realm: WayRealm::default(),
            geometry_claim: GeometryClaim::default(),
            crossing_control: CrossingControl::default(),
            standing: TrailStanding::Established,
            marking: crate::TrailMarking::default(),
            terrain: Terrain::Trail,
            terrain_confidence: Some(1.0),
            surface: Some("dirt".to_owned()),
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: vec![Provenance::fixture(name)],
        }
    }

    fn vertex_at(graph: &WalkGraph, coord: Coord) -> VertexId {
        graph
            .vertices
            .iter()
            .find(|vertex| vertex.coord.planar_distance2(coord) < 1.0e-16)
            .expect("coordinate vertex")
            .id
    }

    fn edge_between(graph: &WalkGraph, from: VertexId, to: VertexId) -> EdgeId {
        graph.adjacency[from.0]
            .iter()
            .copied()
            .find(|edge| graph.edges[edge.0].other(from) == Some(to))
            .expect("adjacent vertices")
    }

    fn loop_constraints() -> LoopConstraints {
        LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: f64::MAX,
            max_repeated_edge_fraction: 0.0,
            allowed_shapes: vec![RouteShape::Loop],
            ..LoopConstraints::default()
        }
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
                junction_keys: None,
                turn_restrictions: Vec::new(),
                way_kind: WayKind::Path,
                realm: WayRealm::default(),
                geometry_claim: GeometryClaim::default(),
                crossing_control: CrossingControl::default(),
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
        let local = realized.graph();

        assert_eq!(graph.vertices.len(), 2, "the corpus remains immutable");
        assert_eq!(graph.edges.len(), 1, "the corpus remains immutable");
        assert_eq!(local.vertices.len(), 2, "the realized graph is route-local");
        assert_eq!(
            local.edges.len(),
            1,
            "only the walked half-edge is materialized"
        );
        assert_eq!(realized.route.edges.len(), 2);
        assert_eq!(realized.route.edges[0], realized.route.edges[1]);
        assert!((realized.route.metrics.distance_m - source_length_m / 2.0).abs() < 0.5);
        assert!((realized.route.metrics.ascent_m - 10.0).abs() < 0.1);
        assert!((realized.route.metrics.descent_m - 10.0).abs() < 0.1);
        let anchor = realized.trail.support_points[1].coord();
        assert!(anchor.lat.abs() < 1.0e-10);
        assert!((anchor.lon - 0.001).abs() < 1.0e-8);
        assert_eq!(local.vertices[realized.bindings[1].vertex.0].coord, anchor);
    }

    #[test]
    fn loop_candidates_recover_exact_support_designs() {
        let graph = graph();
        let constraints = loop_constraints();
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
    fn loop_design_admits_and_reverses_a_retraced_bridge() {
        let trailhead = Coord::new(0.0, 0.0);
        let junction = Coord::new(0.001, 0.0);
        let east = Coord::new(0.002, 0.0);
        let north = Coord::new(0.001, 0.001);
        let graph = GraphBuilder::default()
            .build(&[
                draft("bridge", trailhead, junction),
                draft("lower", junction, east),
                draft("upper", east, north),
                draft("west", north, junction),
            ])
            .expect("build lollipop network");
        let trail = Trail::forge(
            RouteShape::Loop,
            [trailhead, east, north]
                .map(|coord| SupportPoint::forge(coord).expect("valid support"))
                .to_vec(),
            RoutingLaw::default(),
        )
        .expect("valid loop design");
        let constraints = LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: f64::MAX,
            max_repeated_edge_fraction: 1.0,
            allowed_shapes: vec![
                RouteShape::Loop,
                RouteShape::FigureEight,
                RouteShape::OutAndBack,
            ],
            ..LoopConstraints::default()
        };

        let realized = trail
            .realize("lollipop", &graph, &constraints, 1.0)
            .expect("a retraced bridge is a lawful manual loop");

        assert_eq!(realized.trail.shape, RouteShape::Loop);
        assert_eq!(realized.route.metrics.shape, RouteShape::OutAndBack);
        assert!(realized.route.metrics.repeated_edge_fraction > 0.0);
        assert!(realized.route.verdict.satisfied);
        assert_eq!(
            graph.walk_edges(realized.route.start, &realized.route.edges),
            Some(realized.route.start)
        );

        let reversal = realized
            .reverse_loop(&graph)
            .expect("closed design remains reversible despite its morphology");
        let reversed = reversal
            .trail
            .realize("lollipop", &graph, &constraints, 1.0)
            .expect("reversed supports reproduce the reversed closed walk");
        assert_eq!(reversed.trail.shape, RouteShape::Loop);
        assert_eq!(reversed.route.metrics.shape, RouteShape::OutAndBack);
        assert_ne!(realized.walk_fingerprint(), reversed.walk_fingerprint());
        let expected = realized
            .source_spans
            .iter()
            .rev()
            .map(|span| span.reversed())
            .collect::<Vec<_>>();
        assert!(same_spans(&reversed.source_spans, &expected));
    }

    #[test]
    fn reversal_retains_pins_and_repairs_only_ambiguous_spans() {
        let trailhead = Coord::new(0.0, 0.0);
        let east = Coord::new(0.001, 0.0);
        let far_east = Coord::new(0.002, 0.0);
        let turn = Coord::new(0.002, 0.001);
        let detour = Coord::new(0.001, 0.001);
        let mut graph = GraphBuilder::default()
            .build(&[
                draft("head-east", trailhead, east),
                draft("east-far", east, far_east),
                draft("far-turn", far_east, turn),
                draft("turn-head", turn, trailhead),
                draft("turn-detour", turn, detour),
                draft("detour-head", detour, trailhead),
            ])
            .expect("build pentagonal network");
        let head_vertex = vertex_at(&graph, trailhead);
        let far_vertex = vertex_at(&graph, far_east);
        let turn_vertex = vertex_at(&graph, turn);
        graph.turn_bans.push(TurnBan {
            via: turn_vertex,
            from: edge_between(&graph, far_vertex, turn_vertex),
            to: edge_between(&graph, turn_vertex, head_vertex),
            provenance: Provenance::fixture("force-detour"),
        });
        graph.validate().expect("valid turn ban");

        let supports = [trailhead, far_east, turn]
            .map(|coord| SupportPoint::forge(coord).expect("valid support"))
            .to_vec();
        let trail = Trail::forge(RouteShape::Loop, supports.clone(), RoutingLaw::default())
            .expect("valid loop design");
        let constraints = loop_constraints();
        let realized = trail
            .realize("detour", &graph, &constraints, 1.0)
            .expect("turn ban creates a lawful loop");
        let reversal = realized
            .reverse_loop(&graph)
            .expect("bidirectional loop is reversible");

        assert_eq!(reversal.added_supports, 1);
        assert!(
            supports
                .iter()
                .all(|support| reversal.trail.support_points.contains(support))
        );
        let reversed = reversal
            .trail
            .realize("detour", &graph, &constraints, 1.0)
            .expect("repaired controls realize");
        let expected = realized
            .source_spans
            .iter()
            .rev()
            .map(|span| span.reversed())
            .collect::<Vec<_>>();
        assert!(same_spans(&reversed.source_spans, &expected));
    }

    #[test]
    fn reversal_rejects_a_loop_containing_a_one_way_segment() {
        let a = Coord::new(0.0, 0.0);
        let b = Coord::new(0.001, 0.0);
        let c = Coord::new(0.0005, 0.001);
        let mut graph = GraphBuilder::default()
            .build(&[draft("ab", a, b), draft("bc", b, c), draft("ca", c, a)])
            .expect("build triangle");
        let va = vertex_at(&graph, a);
        let vb = vertex_at(&graph, b);
        let ab = edge_between(&graph, va, vb);
        graph.edges[ab.0].attr.travel = if graph.edges[ab.0].a == va {
            EdgeTravel::Forward
        } else {
            EdgeTravel::Backward
        };
        graph.rebuild_adjacency();

        let supports = [a, b, c]
            .map(|coord| SupportPoint::forge(coord).expect("valid support"))
            .to_vec();
        let trail = Trail::forge(RouteShape::Loop, supports, RoutingLaw::default())
            .expect("valid loop design");
        let constraints = loop_constraints();
        let realized = trail
            .realize("one-way", &graph, &constraints, 1.0)
            .expect("forward direction is lawful");

        assert!(matches!(
            realized.reverse_loop(&graph),
            Err(TrailgenError::OneWayReversal)
        ));
    }

    #[test]
    fn support_insertion_follows_realized_leg_order() {
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
            .realize("editable", &graph, &constraints, 1.0)
            .expect("realize recovered loop");
        let routed = realized.graph();

        for slot in 1..realized.support_offsets.len() {
            let first = realized.support_offsets[slot - 1];
            let limit = realized.support_offsets[slot];
            if first == limit {
                continue;
            }
            let edge = &routed.edges[realized.route.edges[first].0];
            let insertion = realized
                .support_insertion(line_coord_at(
                    &edge.geometry,
                    edge.geometry.length_m() / 2.0,
                ))
                .expect("route edge admits insertion");
            assert_eq!(insertion.slot, slot);
            assert!(insertion.distance_m < 0.01);
        }

        let closure = *realized.support_offsets.last().expect("support offset");
        if closure < realized.route.edges.len() {
            let edge = &routed.edges[realized.route.edges[closure].0];
            let insertion = realized
                .support_insertion(line_coord_at(
                    &edge.geometry,
                    edge.geometry.length_m() / 2.0,
                ))
                .expect("closure edge admits insertion");
            assert_eq!(insertion.slot, realized.bindings.len());
        }
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
            junction_keys: None,
            turn_restrictions: Vec::new(),
            way_kind: WayKind::Path,
            realm: WayRealm::default(),
            geometry_claim: GeometryClaim::default(),
            crossing_control: CrossingControl::default(),
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
}
