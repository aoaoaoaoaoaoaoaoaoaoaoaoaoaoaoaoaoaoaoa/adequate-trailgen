use crate::difficulty::DifficultyWeights;
use crate::enrich::{EmbeddedElevation, EnrichmentConfig, enrich_graph};
use crate::geo::{Coord, LineString};
use crate::model::{
    Access, Edge, EdgeAttr, EdgeId, EdgeTravel, GradeDistribution, Provenance, Terrain, TrailClass,
    TrailGraph, TrailMarking, TrailStanding, TurnBan, Vertex, VertexId,
};
use crate::{Result, TrailgenError};
use rstar::{AABB, RTree, RTreeObject};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, btree_map::Entry};

pub const DEFAULT_SNAP_TOLERANCE_M: f64 = 15.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SegmentDraft {
    pub geometry: LineString,
    #[serde(default)]
    pub junctions: JunctionPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turn_restrictions: Vec<TurnRestrictionDraft>,
    #[serde(default)]
    pub trail_class: TrailClass,
    #[serde(default)]
    pub standing: TrailStanding,
    #[serde(default)]
    pub marking: TrailMarking,
    pub terrain: Terrain,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain_confidence: Option<f64>,
    pub surface: Option<String>,
    pub access: Access,
    #[serde(default)]
    pub travel: EdgeTravel,
    pub road_exposure: f64,
    pub confidence: f64,
    pub provenance: Vec<Provenance>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JunctionPolicy {
    #[default]
    Planar,
    ExplicitNodes,
    ExplicitEndpoints,
    GradeSeparatedEndpoints,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnRestrictionDraft {
    pub from: String,
    pub via: Coord,
    pub to: String,
    pub rule: TurnRestrictionRule,
    pub provenance: Provenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TurnRestrictionRule {
    No,
    Only,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphBuilder {
    pub snap_tolerance_m: f64,
    pub enrichment: EnrichmentConfig,
    pub weights: DifficultyWeights,
}

impl Default for GraphBuilder {
    fn default() -> Self {
        Self {
            snap_tolerance_m: DEFAULT_SNAP_TOLERANCE_M,
            enrichment: EnrichmentConfig::default(),
            weights: DifficultyWeights::default(),
        }
    }
}

#[derive(Clone, Copy)]
struct Primitive {
    a: Coord,
    b: Coord,
    src: usize,
}

#[derive(Clone, Copy)]
struct SnapPrimitive {
    a: Coord,
    b: Coord,
    primitive: usize,
    start_t: f64,
    end_t: f64,
}

#[derive(Clone, Copy)]
struct PrimitiveEnvelope {
    index: usize,
    envelope: AABB<[f64; 2]>,
}

impl RTreeObject for PrimitiveEnvelope {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        self.envelope
    }
}

#[derive(Clone, Copy)]
struct Cut {
    t: f64,
    coord: Coord,
    snapped: bool,
}

impl Cut {
    const fn exact(t: f64, coord: Coord) -> Self {
        Self {
            t,
            coord,
            snapped: false,
        }
    }

    const fn snapped(t: f64, coord: Coord) -> Self {
        Self {
            t,
            coord,
            snapped: true,
        }
    }
}

#[derive(Clone, Copy)]
struct SnapCandidate {
    src_primitive: usize,
    src_t: f64,
    target_primitive: usize,
    target_t: f64,
    coord: Coord,
    distance2: f64,
    src_endpoint: usize,
    target_endpoint: Option<usize>,
}

impl GraphBuilder {
    pub fn build(self, drafts: &[SegmentDraft]) -> Result<TrailGraph> {
        if drafts.is_empty() {
            return Err(TrailgenError::InvalidData(
                "cannot build graph from zero segments".to_owned(),
            ));
        }

        let primitives = draft_primitives(drafts);
        let snap_primitives = snap_primitives(drafts, &primitives);

        let mut cuts = primitives
            .iter()
            .map(|p| vec![Cut::exact(0.0, p.a), Cut::exact(1.0, p.b)])
            .collect::<Vec<_>>();

        let index = primitive_index(&primitives);
        let snap_index = snap_primitive_index(&snap_primitives);
        for (i, primitive) in primitives.iter().copied().enumerate() {
            for candidate in index.locate_in_envelope_intersecting(&primitive_envelope(primitive)) {
                let j = candidate.index;
                if j <= i {
                    continue;
                }
                if !junctions_may_be_inferred(drafts, primitives[i], primitives[j]) {
                    continue;
                }
                if let Some((t, u, c)) = segment_intersection(primitives[i], primitives[j])
                    && (0.0..=1.0).contains(&t)
                    && (0.0..=1.0).contains(&u)
                {
                    cuts[i].push(Cut::exact(t, c));
                    cuts[j].push(Cut::exact(u, c));
                }
            }
        }

        for snap in near_miss_snaps(
            drafts,
            &primitives,
            &snap_primitives,
            &snap_index,
            self.snap_tolerance_m,
        ) {
            cuts[snap.src_primitive].push(Cut::snapped(snap.src_t, snap.coord));
            cuts[snap.target_primitive].push(Cut::snapped(snap.target_t, snap.coord));
        }

        let assembly = assemble_edges(
            drafts,
            &primitives,
            cuts,
            self.weights,
            self.snap_tolerance_m,
        );
        let mut graph = TrailGraph::new(assembly.vertices, assembly.edges);
        graph.turn_bans = turn_bans(
            drafts,
            &assembly.edges_by_draft,
            &graph,
            self.snap_tolerance_m,
        );
        enrich_graph(
            &mut graph,
            &EmbeddedElevation,
            self.enrichment,
            self.weights,
        )?;
        Ok(graph)
    }
}

struct EdgeAssembly {
    vertices: Vec<Vertex>,
    edges: Vec<Edge>,
    edges_by_draft: Vec<Vec<EdgeId>>,
}

fn assemble_edges(
    drafts: &[SegmentDraft],
    primitives: &[Primitive],
    cuts: Vec<Vec<Cut>>,
    weights: DifficultyWeights,
    snap_tolerance_m: f64,
) -> EdgeAssembly {
    let mut vertices = Vec::<Vertex>::new();
    let mut vertex_by_coord = BTreeMap::<(u64, u64), VertexId>::new();
    let mut edges = Vec::<Edge>::new();
    let mut edge_by_support = BTreeMap::<Vec<(u64, u64)>, EdgeId>::new();
    let mut edges_by_draft = vec![Vec::<EdgeId>::new(); drafts.len()];
    let snap_provenance = Provenance {
        source: "graph-builder".to_owned(),
        layer: Some("near-miss-snap".to_owned()),
        source_id: Some(format!("tolerance {snap_tolerance_m:.1} m")),
        license: None,
    };

    for (primitive, xs) in primitives.iter().copied().zip(cuts) {
        let xs = normalize_cuts(xs);
        for pair in xs.windows(2) {
            let a = pair[0].coord;
            let b = pair[1].coord;
            if a.planar_distance2(b) < 1.0e-18 {
                continue;
            }
            let va = vertex_id(a, &mut vertices, &mut vertex_by_coord);
            let vb = vertex_id(b, &mut vertices, &mut vertex_by_coord);
            if va == vb {
                continue;
            }
            let draft = &drafts[primitive.src];
            let geometry = edge_geometry(
                draft,
                vertices[va.0].coord,
                vertices[vb.0].coord,
                pair[0].t,
                pair[1].t,
            );
            let snapped = pair[0].snapped || pair[1].snapped;
            let attr = edge_attr(draft, &geometry, snapped.then_some(snap_provenance.clone()));
            let id = EdgeId(edges.len());
            let mut edge = Edge {
                id,
                a: va,
                b: vb,
                geometry,
                attr,
            };
            weights.apply_edge(&mut edge);
            let support = support_key(&edge.geometry);
            let id = if let Some(id) = edge_by_support.get(&support).copied() {
                corroborate_edge(&mut edges[id.0], &edge);
                weights.apply_edge(&mut edges[id.0]);
                id
            } else {
                edge_by_support.insert(support, id);
                edges.push(edge);
                id
            };
            edges_by_draft[primitive.src].push(id);
        }
    }

    EdgeAssembly {
        vertices,
        edges,
        edges_by_draft,
    }
}

fn support_key(geometry: &LineString) -> Vec<(u64, u64)> {
    let forward = geometry
        .points
        .iter()
        .map(|point| (point.lon.to_bits(), point.lat.to_bits()));
    let reverse = geometry
        .points
        .iter()
        .rev()
        .map(|point| (point.lon.to_bits(), point.lat.to_bits()));
    let forward = forward.collect::<Vec<_>>();
    let reverse = reverse.collect::<Vec<_>>();
    forward.min(reverse)
}

fn corroborate_edge(preferred: &mut Edge, suppressed: &Edge) {
    for provenance in &suppressed.attr.provenance {
        if !preferred.attr.provenance.contains(provenance) {
            preferred.attr.provenance.push(provenance.clone());
        }
    }
    preferred.attr.confidence = preferred.attr.confidence.max(suppressed.attr.confidence);
    if preferred.attr.trail_class == TrailClass::Unknown {
        preferred.attr.trail_class = suppressed.attr.trail_class;
    }
    if preferred.attr.standing == TrailStanding::Unknown {
        preferred.attr.standing = suppressed.attr.standing;
    }
    if preferred.attr.marking == TrailMarking::Unknown {
        preferred.attr.marking = suppressed.attr.marking;
    }
    if preferred.attr.terrain == Terrain::Unknown {
        preferred.attr.terrain = suppressed.attr.terrain;
        preferred.attr.terrain_confidence = suppressed.attr.terrain_confidence;
    }
    if preferred.attr.surface.is_none() {
        preferred.attr.surface.clone_from(&suppressed.attr.surface);
    }
    if preferred.attr.access == Access::Unknown {
        preferred.attr.access = suppressed.attr.access;
    }
}

fn draft_primitives(drafts: &[SegmentDraft]) -> Vec<Primitive> {
    drafts
        .iter()
        .enumerate()
        .flat_map(|(src, draft)| {
            if matches!(
                draft.junctions,
                JunctionPolicy::ExplicitEndpoints | JunctionPolicy::GradeSeparatedEndpoints
            ) {
                let points = &draft.geometry.points;
                vec![Primitive {
                    a: points[0],
                    b: points[points.len() - 1],
                    src,
                }]
            } else {
                draft
                    .geometry
                    .points
                    .windows(2)
                    .map(|points| Primitive {
                        a: points[0],
                        b: points[1],
                        src,
                    })
                    .collect()
            }
        })
        .collect()
}

fn snap_primitives(drafts: &[SegmentDraft], primitives: &[Primitive]) -> Vec<SnapPrimitive> {
    primitives
        .iter()
        .copied()
        .enumerate()
        .flat_map(|(primitive, topology)| {
            let draft = &drafts[topology.src];
            if !matches!(
                draft.junctions,
                JunctionPolicy::ExplicitEndpoints | JunctionPolicy::GradeSeparatedEndpoints
            ) {
                return vec![SnapPrimitive {
                    a: topology.a,
                    b: topology.b,
                    primitive,
                    start_t: 0.0,
                    end_t: 1.0,
                }];
            }
            let lengths = draft
                .geometry
                .points
                .windows(2)
                .map(|segment| segment[0].haversine_m(segment[1]))
                .collect::<Vec<_>>();
            let total_m = lengths.iter().sum::<f64>().max(f64::EPSILON);
            let mut traversed_m = 0.0;
            draft
                .geometry
                .points
                .windows(2)
                .zip(lengths)
                .map(|(segment, length_m)| {
                    let start_t = traversed_m / total_m;
                    traversed_m += length_m;
                    SnapPrimitive {
                        a: segment[0],
                        b: segment[1],
                        primitive,
                        start_t,
                        end_t: traversed_m / total_m,
                    }
                })
                .collect()
        })
        .collect()
}

fn edge_geometry(draft: &SegmentDraft, a: Coord, b: Coord, start_t: f64, end_t: f64) -> LineString {
    if !matches!(
        draft.junctions,
        JunctionPolicy::ExplicitEndpoints | JunctionPolicy::GradeSeparatedEndpoints
    ) {
        return LineString::unchecked(vec![a, b]);
    }
    let lengths = draft
        .geometry
        .points
        .windows(2)
        .map(|segment| segment[0].haversine_m(segment[1]))
        .collect::<Vec<_>>();
    let total_m = lengths.iter().sum::<f64>();
    let start_m = total_m * start_t;
    let end_m = total_m * end_t;
    let mut traversed_m = 0.0;
    let mut points = vec![a];
    for (index, length_m) in lengths.into_iter().enumerate() {
        traversed_m += length_m;
        if traversed_m > start_m + 1.0e-7 && traversed_m < end_m - 1.0e-7 {
            points.push(draft.geometry.points[index + 1]);
        }
    }
    points.push(b);
    LineString::unchecked(points)
}

fn junctions_may_be_inferred(drafts: &[SegmentDraft], a: Primitive, b: Primitive) -> bool {
    drafts[a.src].junctions == JunctionPolicy::Planar
        && drafts[b.src].junctions == JunctionPolicy::Planar
}

fn snaps_may_be_inferred(drafts: &[SegmentDraft], a: Primitive, b: Primitive) -> bool {
    let inferable = |policy| {
        matches!(
            policy,
            JunctionPolicy::Planar | JunctionPolicy::ExplicitEndpoints
        )
    };
    inferable(drafts[a.src].junctions) && inferable(drafts[b.src].junctions)
}

fn primitive_index(primitives: &[Primitive]) -> RTree<PrimitiveEnvelope> {
    RTree::bulk_load(
        primitives
            .iter()
            .copied()
            .enumerate()
            .map(|(index, primitive)| PrimitiveEnvelope {
                index,
                envelope: primitive_envelope(primitive),
            })
            .collect(),
    )
}

fn snap_primitive_index(primitives: &[SnapPrimitive]) -> RTree<PrimitiveEnvelope> {
    RTree::bulk_load(
        primitives
            .iter()
            .enumerate()
            .map(|(index, primitive)| PrimitiveEnvelope {
                index,
                envelope: AABB::from_corners(
                    [
                        primitive.a.lon.min(primitive.b.lon),
                        primitive.a.lat.min(primitive.b.lat),
                    ],
                    [
                        primitive.a.lon.max(primitive.b.lon),
                        primitive.a.lat.max(primitive.b.lat),
                    ],
                ),
            })
            .collect(),
    )
}

fn primitive_envelope(p: Primitive) -> AABB<[f64; 2]> {
    AABB::from_corners(
        [p.a.lon.min(p.b.lon), p.a.lat.min(p.b.lat)],
        [p.a.lon.max(p.b.lon), p.a.lat.max(p.b.lat)],
    )
}

fn turn_bans(
    drafts: &[SegmentDraft],
    edges_by_draft: &[Vec<EdgeId>],
    graph: &TrailGraph,
    snap_tolerance_m: f64,
) -> Vec<TurnBan> {
    let mut edges_by_ref = BTreeMap::<&str, Vec<EdgeId>>::new();
    for (draft, edges) in drafts.iter().zip(edges_by_draft) {
        if let Some(turn_ref) = draft.turn_ref.as_deref() {
            edges_by_ref.entry(turn_ref).or_default().extend(edges);
        }
    }
    let restrictions = drafts
        .iter()
        .flat_map(|draft| draft.turn_restrictions.iter())
        .collect::<Vec<_>>();
    let mut seen = std::collections::BTreeSet::<(VertexId, EdgeId, EdgeId)>::new();
    let mut bans = Vec::new();
    for restriction in restrictions {
        let Some((via, distance_m)) = graph.nearest_vertex_with_distance(restriction.via) else {
            continue;
        };
        if distance_m > snap_tolerance_m.max(1.0) {
            continue;
        }
        let Some(from_edges) = edges_by_ref.get(restriction.from.as_str()) else {
            continue;
        };
        let Some(to_edges) = edges_by_ref.get(restriction.to.as_str()) else {
            continue;
        };
        let from_edges = from_edges
            .iter()
            .copied()
            .filter(|edge| arrives_at(graph, *edge, via))
            .collect::<Vec<_>>();
        let allowed = to_edges
            .iter()
            .copied()
            .filter(|edge| departs_from(graph, *edge, via))
            .collect::<std::collections::BTreeSet<_>>();
        let banned = match restriction.rule {
            TurnRestrictionRule::No => allowed.iter().copied().collect::<Vec<_>>(),
            TurnRestrictionRule::Only => graph.adjacency[via.0]
                .iter()
                .copied()
                .filter(|edge| !allowed.contains(edge))
                .collect(),
        };
        for from in &from_edges {
            for to in &banned {
                if seen.insert((via, *from, *to)) {
                    bans.push(TurnBan {
                        via,
                        from: *from,
                        to: *to,
                        provenance: restriction.provenance.clone(),
                    });
                }
            }
        }
    }
    bans
}

fn arrives_at(graph: &TrailGraph, edge: EdgeId, via: VertexId) -> bool {
    let edge = &graph.edges[edge.0];
    edge.other(via)
        .is_some_and(|other| edge.traverse(other) == Some(via))
}

fn departs_from(graph: &TrailGraph, edge: EdgeId, via: VertexId) -> bool {
    graph.edges[edge.0].traverse(via).is_some()
}

fn edge_attr(
    draft: &SegmentDraft,
    geometry: &LineString,
    snap_provenance: Option<Provenance>,
) -> EdgeAttr {
    let (ascent_m, descent_m) = geometry.ascent_descent_m();
    let length_m = geometry.length_m();
    let grade_abs_mean = if length_m > 0.0 {
        (ascent_m + descent_m) / length_m
    } else {
        0.0
    };
    let mut provenance = draft.provenance.clone();
    let mut confidence = draft.confidence.clamp(0.0, 1.0);
    if let Some(snap_provenance) = snap_provenance {
        provenance.push(snap_provenance);
        confidence = confidence.min(0.74);
    }
    EdgeAttr {
        length_m,
        ascent_m,
        descent_m,
        grade_abs_mean,
        grade_abs_max: grade_abs_mean,
        sustained_steep_m: 0.0,
        grade_distribution: GradeDistribution::default().add_segment(length_m, grade_abs_mean),
        trail_class: draft.trail_class,
        standing: draft.standing,
        marking: draft.marking,
        terrain: draft.terrain,
        surface: draft.surface.clone(),
        terrain_confidence: draft
            .terrain_confidence
            .unwrap_or_else(|| legacy_terrain_confidence(draft.terrain))
            .clamp(0.0, 1.0),
        terrain_evidence: Vec::new(),
        access: draft.access,
        travel: draft.travel,
        access_confidence: if draft.access == Access::Unknown {
            0.0
        } else {
            0.90
        },
        access_provenance: Vec::new(),
        crossings: Vec::new(),
        road_exposure: draft.road_exposure.clamp(0.0, 1.0),
        confidence,
        difficulty_breakdown: crate::difficulty::DifficultyBreakdown::default(),
        difficulty: 0.0,
        seed_count: 0,
        popularity: 0.0,
        seed_provenance: Vec::new(),
        elevation_provenance: Vec::new(),
        provenance,
    }
}

const fn legacy_terrain_confidence(terrain: Terrain) -> f64 {
    if matches!(terrain, Terrain::Unknown) {
        0.0
    } else {
        0.90
    }
}

fn normalize_cuts(mut cuts: Vec<Cut>) -> Vec<Cut> {
    cuts.sort_by(|a, b| a.t.total_cmp(&b.t));
    let mut normalized = Vec::<Cut>::new();
    for cut in cuts {
        let Some(last) = normalized.last_mut() else {
            normalized.push(cut);
            continue;
        };
        if (last.t - cut.t).abs() < 1.0e-9 {
            if cut.snapped {
                *last = cut;
            }
        } else {
            normalized.push(cut);
        }
    }
    normalized
}

fn near_miss_snaps(
    drafts: &[SegmentDraft],
    primitives: &[Primitive],
    snap_primitives: &[SnapPrimitive],
    index: &RTree<PrimitiveEnvelope>,
    tolerance_m: f64,
) -> Vec<SnapCandidate> {
    let candidates = near_miss_candidates(drafts, primitives, snap_primitives, index, tolerance_m);
    let (mut snaps, clustered) = cluster_endpoint_snaps(&candidates, primitives, tolerance_m);
    let mut best_interior = BTreeMap::<usize, SnapCandidate>::new();
    for candidate in candidates.into_iter().filter(|candidate| {
        candidate.target_endpoint.is_none() && !clustered[candidate.src_endpoint]
    }) {
        match best_interior.entry(candidate.src_endpoint) {
            Entry::Occupied(mut entry) if snap_rank(candidate) < snap_rank(*entry.get()) => {
                entry.insert(candidate);
            }
            Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
            Entry::Occupied(_) => {}
        }
    }
    snaps.extend(best_interior.into_values());
    snaps
}

fn near_miss_candidates(
    drafts: &[SegmentDraft],
    primitives: &[Primitive],
    snap_primitives: &[SnapPrimitive],
    index: &RTree<PrimitiveEnvelope>,
    tolerance_m: f64,
) -> Vec<SnapCandidate> {
    let mut candidates = Vec::new();
    for (src_idx, primitive) in primitives.iter().copied().enumerate() {
        for (endpoint_ix, endpoint_t, endpoint) in [(0, 0.0, primitive.a), (1, 1.0, primitive.b)] {
            let src_endpoint = src_idx * 2 + endpoint_ix;
            let latitude_radius = tolerance_m / 110_540.0;
            let longitude_radius =
                tolerance_m / (111_320.0 * endpoint.lat.to_radians().cos().abs().max(0.05));
            let neighborhood = AABB::from_corners(
                [
                    endpoint.lon - longitude_radius,
                    endpoint.lat - latitude_radius,
                ],
                [
                    endpoint.lon + longitude_radius,
                    endpoint.lat + latitude_radius,
                ],
            );
            for candidate in index.locate_in_envelope_intersecting(&neighborhood) {
                let target_segment = snap_primitives[candidate.index];
                let target_idx = target_segment.primitive;
                let target = primitives[target_idx];
                if src_idx == target_idx || primitive.src == target.src {
                    continue;
                }
                if !snaps_may_be_inferred(drafts, primitive, target) {
                    continue;
                }
                let Some((segment_t, coord, distance2)) = projected_snap(
                    endpoint,
                    target_segment.a,
                    target_segment.b,
                    tolerance_m * tolerance_m,
                ) else {
                    continue;
                };
                let target_t = (target_segment.end_t - target_segment.start_t)
                    .mul_add(segment_t, target_segment.start_t);
                let target_endpoint = if target_t <= 1.0e-9 {
                    Some(target_idx * 2)
                } else if target_t >= 1.0 - 1.0e-9 {
                    Some(target_idx * 2 + 1)
                } else {
                    None
                };
                if target_endpoint.is_some_and(|target| src_endpoint > target) {
                    continue;
                }
                candidates.push(SnapCandidate {
                    src_primitive: src_idx,
                    src_t: endpoint_t,
                    target_primitive: target_idx,
                    target_t,
                    coord,
                    distance2,
                    src_endpoint,
                    target_endpoint,
                });
            }
        }
    }
    candidates
}

fn cluster_endpoint_snaps(
    candidates: &[SnapCandidate],
    primitives: &[Primitive],
    tolerance_m: f64,
) -> (Vec<SnapCandidate>, Vec<bool>) {
    let endpoint_count = primitives.len() * 2;
    let mut parent = (0..endpoint_count).collect::<Vec<_>>();
    let mut members = (0..endpoint_count)
        .map(|endpoint| vec![endpoint])
        .collect::<Vec<_>>();
    let mut endpoint_candidates = candidates
        .iter()
        .filter(|candidate| candidate.target_endpoint.is_some())
        .copied()
        .collect::<Vec<_>>();
    endpoint_candidates.sort_by_key(|candidate| {
        (
            candidate.distance2.to_bits(),
            candidate.src_endpoint,
            candidate.target_endpoint,
        )
    });
    for candidate in endpoint_candidates {
        let target = candidate.target_endpoint.expect("filtered endpoint snap");
        let a = endpoint_root(&parent, candidate.src_endpoint);
        let b = endpoint_root(&parent, target);
        if a == b || !clusters_fit(&members[a], &members[b], primitives, tolerance_m) {
            continue;
        }
        let (keep, discard) = if a < b { (a, b) } else { (b, a) };
        parent[discard] = keep;
        let displaced = std::mem::take(&mut members[discard]);
        members[keep].extend(displaced);
    }

    let mut snaps = Vec::new();
    let mut clustered = vec![false; endpoint_count];
    for cluster in members.iter().filter(|cluster| cluster.len() > 1) {
        let canonical = endpoint_medoid(cluster, primitives);
        let coord = endpoint_coord(primitives, canonical);
        for &endpoint in cluster {
            clustered[endpoint] = true;
            if endpoint == canonical {
                continue;
            }
            snaps.push(SnapCandidate {
                src_primitive: endpoint / 2,
                src_t: endpoint_t(endpoint),
                target_primitive: canonical / 2,
                target_t: endpoint_t(canonical),
                coord,
                distance2: endpoint_coord(primitives, endpoint)
                    .haversine_m(coord)
                    .powi(2),
                src_endpoint: endpoint,
                target_endpoint: Some(canonical),
            });
        }
    }
    (snaps, clustered)
}

const fn snap_rank(candidate: SnapCandidate) -> (u64, usize, u64) {
    (
        candidate.distance2.to_bits(),
        candidate.target_primitive,
        candidate.target_t.to_bits(),
    )
}

const fn endpoint_t(endpoint: usize) -> f64 {
    if endpoint.is_multiple_of(2) { 0.0 } else { 1.0 }
}

fn endpoint_root(parent: &[usize], mut endpoint: usize) -> usize {
    while parent[endpoint] != endpoint {
        endpoint = parent[endpoint];
    }
    endpoint
}

fn endpoint_coord(primitives: &[Primitive], endpoint: usize) -> Coord {
    let primitive = primitives[endpoint / 2];
    if endpoint.is_multiple_of(2) {
        primitive.a
    } else {
        primitive.b
    }
}

fn clusters_fit(a: &[usize], b: &[usize], primitives: &[Primitive], tolerance_m: f64) -> bool {
    a.iter().all(|&lhs| {
        b.iter().all(|&rhs| {
            endpoint_coord(primitives, lhs).haversine_m(endpoint_coord(primitives, rhs))
                <= tolerance_m
        })
    })
}

fn endpoint_medoid(cluster: &[usize], primitives: &[Primitive]) -> usize {
    cluster
        .iter()
        .copied()
        .min_by(|&lhs, &rhs| {
            let score = |candidate| {
                cluster
                    .iter()
                    .map(|&other| {
                        endpoint_coord(primitives, candidate)
                            .haversine_m(endpoint_coord(primitives, other))
                    })
                    .sum::<f64>()
            };
            score(lhs).total_cmp(&score(rhs)).then(lhs.cmp(&rhs))
        })
        .expect("endpoint cluster is nonempty")
}

fn projected_snap(
    endpoint: Coord,
    target_a: Coord,
    target_b: Coord,
    tolerance_m2: f64,
) -> Option<(f64, Coord, f64)> {
    let longitude_scale = 111_320.0 * endpoint.lat.to_radians().cos();
    let latitude_scale = 110_540.0;
    let vx = (target_b.lon - target_a.lon) * longitude_scale;
    let vy = (target_b.lat - target_a.lat) * latitude_scale;
    let len2 = vx.mul_add(vx, vy * vy);
    if len2 <= f64::EPSILON {
        return None;
    }
    let wx = (endpoint.lon - target_a.lon) * longitude_scale;
    let wy = (endpoint.lat - target_a.lat) * latitude_scale;
    let t = (wx.mul_add(vx, wy * vy) / len2).clamp(0.0, 1.0);
    let coord = target_a.lerp(target_b, t);
    let dx = wx - t * vx;
    let dy = wy - t * vy;
    let distance_m2 = dx.mul_add(dx, dy * dy);
    (distance_m2 <= tolerance_m2).then_some((t, coord, distance_m2))
}

fn vertex_id(
    coord: Coord,
    vertices: &mut Vec<Vertex>,
    vertex_by_coord: &mut BTreeMap<(u64, u64), VertexId>,
) -> VertexId {
    match vertex_by_coord.entry((coord.lon.to_bits(), coord.lat.to_bits())) {
        Entry::Occupied(entry) => *entry.get(),
        Entry::Vacant(entry) => {
            let id = VertexId(vertices.len());
            entry.insert(id);
            vertices.push(Vertex { id, coord });
            id
        }
    }
}

fn segment_intersection(lhs: Primitive, rhs: Primitive) -> Option<(f64, f64, Coord)> {
    let origin = (lhs.a.lon, lhs.a.lat);
    let ray = (lhs.b.lon - lhs.a.lon, lhs.b.lat - lhs.a.lat);
    let obstacle = (rhs.a.lon, rhs.a.lat);
    let sweep = (rhs.b.lon - rhs.a.lon, rhs.b.lat - rhs.a.lat);
    let determinant = cross(ray, sweep);
    if determinant.abs() < 1.0e-14 {
        return None;
    }
    let delta = (obstacle.0 - origin.0, obstacle.1 - origin.1);
    let t = cross(delta, sweep) / determinant;
    let u = cross(delta, ray) / determinant;
    if !(-1.0e-9..=1.0 + 1.0e-9).contains(&t) || !(-1.0e-9..=1.0 + 1.0e-9).contains(&u) {
        return None;
    }
    let coord = lhs.a.lerp(lhs.b, t.clamp(0.0, 1.0));
    Some((t.clamp(0.0, 1.0), u.clamp(0.0, 1.0), coord))
}

fn cross(a: (f64, f64), b: (f64, f64)) -> f64 {
    a.0.mul_add(b.1, -a.1 * b.0)
}
