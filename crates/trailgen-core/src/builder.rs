use crate::difficulty::DifficultyWeights;
use crate::enrich::{EmbeddedElevation, EnrichmentConfig, enrich_graph};
use crate::geo::{Coord, LineString};
use crate::model::{
    Access, Edge, EdgeAttr, EdgeId, GradeDistribution, Provenance, Terrain, TrailGraph, Vertex,
    VertexId,
};
use crate::{Result, TrailgenError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, btree_map::Entry};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SegmentDraft {
    pub geometry: LineString,
    pub terrain: Terrain,
    pub access: Access,
    pub road_exposure: f64,
    pub confidence: f64,
    pub provenance: Provenance,
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

        let primitives = drafts
            .iter()
            .enumerate()
            .flat_map(|(src, draft)| {
                draft.geometry.points.windows(2).map(move |w| Primitive {
                    a: w[0],
                    b: w[1],
                    src,
                })
            })
            .collect::<Vec<_>>();

        let mut cuts = primitives
            .iter()
            .map(|p| vec![Cut::exact(0.0, p.a), Cut::exact(1.0, p.b)])
            .collect::<Vec<_>>();

        for i in 0..primitives.len() {
            for j in (i + 1)..primitives.len() {
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
        for snap in near_miss_snaps(&primitives, deg_tol) {
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
                let geometry =
                    LineString::unchecked(vec![vertices[va.0].coord, vertices[vb.0].coord]);
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
            }
        }

        let mut graph = TrailGraph::new(vertices, edges);
        enrich_graph(
            &mut graph,
            &EmbeddedElevation,
            self.enrichment,
            self.weights,
        )?;
        Ok(graph)
    }
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
        terrain: draft.terrain,
        terrain_confidence: if draft.terrain == Terrain::Unknown {
            0.0
        } else {
            0.90
        },
        terrain_evidence: Vec::new(),
        access: draft.access,
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

fn near_miss_snaps(primitives: &[Primitive], deg_tol: f64) -> Vec<SnapCandidate> {
    let mut best = BTreeMap::<(usize, u8), SnapCandidate>::new();
    let tolerance2 = deg_tol * deg_tol;
    for (src_idx, primitive) in primitives.iter().copied().enumerate() {
        for (endpoint_ix, endpoint_t, endpoint) in [(0, 0.0, primitive.a), (1, 1.0, primitive.b)] {
            for (target_idx, target) in primitives.iter().copied().enumerate() {
                if src_idx == target_idx || primitive.src == target.src {
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
                    Entry::Occupied(mut entry) if candidate.distance2 < entry.get().distance2 => {
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
