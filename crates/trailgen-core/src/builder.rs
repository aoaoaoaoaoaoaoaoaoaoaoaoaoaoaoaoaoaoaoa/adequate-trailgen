use crate::difficulty::DifficultyWeights;
use crate::enrich::{EmbeddedElevation, EnrichmentConfig, enrich_graph};
use crate::geo::{Coord, LineString};
use crate::model::{
    Access, Edge, EdgeAttr, EdgeId, EdgeTravel, GradeDistribution, Provenance, Terrain, TrailClass,
    TrailGraph, TurnBan, Vertex, VertexId,
};
use crate::{Result, TrailgenError};
use rstar::{AABB, RTree, RTreeObject};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, btree_map::Entry};

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
    pub terrain: Terrain,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain_confidence: Option<f64>,
    pub surface: Option<String>,
    pub access: Access,
    #[serde(default)]
    pub travel: EdgeTravel,
    pub road_exposure: f64,
    pub confidence: f64,
    pub provenance: Provenance,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JunctionPolicy {
    #[default]
    Planar,
    ExplicitNodes,
    ExplicitEndpoints,
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
            snap_tolerance_m: 8.0,
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
}

impl GraphBuilder {
    pub fn build(self, drafts: &[SegmentDraft]) -> Result<TrailGraph> {
        if drafts.is_empty() {
            return Err(TrailgenError::InvalidData(
                "cannot build graph from zero segments".to_owned(),
            ));
        }

        let primitives = draft_primitives(drafts);

        let mut cuts = primitives
            .iter()
            .map(|p| vec![Cut::exact(0.0, p.a), Cut::exact(1.0, p.b)])
            .collect::<Vec<_>>();

        let index = primitive_index(&primitives);
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

        let deg_tol = (self.snap_tolerance_m / 111_320.0).max(1.0e-9);
        for snap in near_miss_snaps(drafts, &primitives, &index, deg_tol) {
            cuts[snap.src_primitive].push(Cut::snapped(snap.src_t, snap.coord));
            cuts[snap.target_primitive].push(Cut::snapped(snap.target_t, snap.coord));
        }

        let mut vertices = Vec::<Vertex>::new();
        let mut vertex_by_cell = BTreeMap::<(String, String), VertexId>::new();
        let mut edges = Vec::<Edge>::new();
        let snap_provenance = Provenance {
            source: "graph-builder".to_owned(),
            layer: Some("near-miss-snap".to_owned()),
            source_id: Some(format!("tolerance {:.1} m", self.snap_tolerance_m)),
            license: None,
        };

        let mut edges_by_draft = vec![Vec::<EdgeId>::new(); drafts.len()];
        for (primitive, xs) in primitives.iter().copied().zip(cuts) {
            let xs = normalize_cuts(xs);
            for pair in xs.windows(2) {
                let a = pair[0].coord;
                let b = pair[1].coord;
                if a.planar_distance2(b) < 1.0e-18 {
                    continue;
                }
                let va = vertex_id(a, deg_tol, &mut vertices, &mut vertex_by_cell);
                let vb = vertex_id(b, deg_tol, &mut vertices, &mut vertex_by_cell);
                if va == vb {
                    continue;
                }
                let draft = &drafts[primitive.src];
                let geometry = edge_geometry(draft, vertices[va.0].coord, vertices[vb.0].coord);
                let id = EdgeId(edges.len());
                let snapped = pair[0].snapped || pair[1].snapped;
                let attr = edge_attr(draft, &geometry, snapped.then_some(snap_provenance.clone()));
                let mut edge = Edge {
                    id,
                    a: va,
                    b: vb,
                    geometry,
                    attr,
                };
                self.weights.apply_edge(&mut edge);
                edges.push(edge);
                edges_by_draft[primitive.src].push(id);
            }
        }

        let mut graph = TrailGraph::new(vertices, edges);
        graph.turn_bans = turn_bans(drafts, &edges_by_draft, &graph, self.snap_tolerance_m);
        enrich_graph(
            &mut graph,
            &EmbeddedElevation,
            self.enrichment,
            self.weights,
        )?;
        Ok(graph)
    }
}

fn draft_primitives(drafts: &[SegmentDraft]) -> Vec<Primitive> {
    drafts
        .iter()
        .enumerate()
        .flat_map(|(src, draft)| {
            if draft.junctions == JunctionPolicy::ExplicitEndpoints {
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

fn edge_geometry(draft: &SegmentDraft, a: Coord, b: Coord) -> LineString {
    if draft.junctions != JunctionPolicy::ExplicitEndpoints {
        return LineString::unchecked(vec![a, b]);
    }
    let mut geometry = draft.geometry.clone();
    let last = geometry.points.len() - 1;
    geometry.points[0] = a;
    geometry.points[last] = b;
    geometry
}

fn junctions_may_be_inferred(drafts: &[SegmentDraft], a: Primitive, b: Primitive) -> bool {
    drafts[a.src].junctions == JunctionPolicy::Planar
        && drafts[b.src].junctions == JunctionPolicy::Planar
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
    let mut provenance = vec![draft.provenance.clone()];
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
    index: &RTree<PrimitiveEnvelope>,
    deg_tol: f64,
) -> Vec<SnapCandidate> {
    let mut best = BTreeMap::<(usize, u8), SnapCandidate>::new();
    let tolerance2 = deg_tol * deg_tol;
    for (src_idx, primitive) in primitives.iter().copied().enumerate() {
        for (endpoint_ix, endpoint_t, endpoint) in [(0, 0.0, primitive.a), (1, 1.0, primitive.b)] {
            let neighborhood = AABB::from_corners(
                [endpoint.lon - deg_tol, endpoint.lat - deg_tol],
                [endpoint.lon + deg_tol, endpoint.lat + deg_tol],
            );
            for candidate in index.locate_in_envelope_intersecting(&neighborhood) {
                let target_idx = candidate.index;
                let target = primitives[target_idx];
                if src_idx == target_idx || primitive.src == target.src {
                    continue;
                }
                if !junctions_may_be_inferred(drafts, primitive, target) {
                    continue;
                }
                let Some((target_t, coord, distance2)) =
                    projected_snap(endpoint, target, tolerance2)
                else {
                    continue;
                };
                let candidate = SnapCandidate {
                    src_primitive: src_idx,
                    src_t: endpoint_t,
                    target_primitive: target_idx,
                    target_t,
                    coord,
                    distance2,
                };
                match best.entry((src_idx, endpoint_ix)) {
                    Entry::Occupied(mut entry)
                        if (candidate.distance2, candidate.target_primitive)
                            < (entry.get().distance2, entry.get().target_primitive) =>
                    {
                        entry.insert(candidate);
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(candidate);
                    }
                    Entry::Occupied(_) => {}
                }
            }
        }
    }
    best.into_values().collect()
}

fn projected_snap(
    endpoint: Coord,
    target: Primitive,
    tolerance2: f64,
) -> Option<(f64, Coord, f64)> {
    let vx = target.b.lon - target.a.lon;
    let vy = target.b.lat - target.a.lat;
    let len2 = vx.mul_add(vx, vy * vy);
    if len2 <= f64::EPSILON {
        return None;
    }
    let wx = endpoint.lon - target.a.lon;
    let wy = endpoint.lat - target.a.lat;
    let t = (wx.mul_add(vx, wy * vy) / len2).clamp(0.0, 1.0);
    if !(1.0e-9..=1.0 - 1.0e-9).contains(&t) {
        return None;
    }
    let coord = target.a.lerp(target.b, t);
    let distance2 = endpoint.planar_distance2(coord);
    (distance2 <= tolerance2).then_some((t, coord, distance2))
}

fn vertex_id(
    coord: Coord,
    deg_tol: f64,
    vertices: &mut Vec<Vertex>,
    vertex_by_cell: &mut BTreeMap<(String, String), VertexId>,
) -> VertexId {
    let key = (
        format!("{:.0}", coord.lon / deg_tol),
        format!("{:.0}", coord.lat / deg_tol),
    );
    match vertex_by_cell.entry(key) {
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
