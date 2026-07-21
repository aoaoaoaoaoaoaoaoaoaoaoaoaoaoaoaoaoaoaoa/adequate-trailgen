use crate::{
    Access, Coord, JunctionPolicy, LineString, Provenance, SegmentDraft, Terrain, TrailClass,
};
use rstar::{AABB, RTree, RTreeObject};
use serde::{Deserialize, Serialize};

const METRES_PER_LATITUDE_DEGREE: f64 = 111_320.0;

/// One already-normalized provider stratum. Lower precedence wins attribute
/// disputes; lower strata may still contribute geometry absent above them.
#[derive(Clone, Debug, PartialEq)]
pub struct NetworkStratum {
    pub precedence: u16,
    pub drafts: Vec<SegmentDraft>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConflationPolicy {
    pub parallel_tolerance_m: f64,
    pub min_parallel_cosine: f64,
    pub max_reported_decisions: usize,
}

impl Default for ConflationPolicy {
    fn default() -> Self {
        Self {
            parallel_tolerance_m: 8.0,
            min_parallel_cosine: 0.94,
            max_reported_decisions: 2_048,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConflationDecision {
    pub preferred: Option<Provenance>,
    pub suppressed: Option<Provenance>,
    pub separation_m: f64,
    pub geometry: LineString,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConflationReport {
    pub strata: usize,
    pub input_drafts: usize,
    pub output_drafts: usize,
    pub suppressed_parallel_segments: usize,
    pub decisions: Vec<ConflationDecision>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConflationStats {
    pub strata: usize,
    pub input_drafts: usize,
    pub output_drafts: usize,
    pub suppressed_parallel_segments: usize,
}

impl From<&ConflationReport> for ConflationStats {
    fn from(report: &ConflationReport) -> Self {
        Self {
            strata: report.strata,
            input_drafts: report.input_drafts,
            output_drafts: report.output_drafts,
            suppressed_parallel_segments: report.suppressed_parallel_segments,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConflatedNetwork {
    pub drafts: Vec<SegmentDraft>,
    pub report: ConflationReport,
}

#[derive(Clone, Copy)]
struct IndexedPrimitive {
    draft: usize,
    a: Coord,
    b: Coord,
}

impl RTreeObject for IndexedPrimitive {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        AABB::from_corners(
            [self.a.lon.min(self.b.lon), self.a.lat.min(self.b.lat)],
            [self.a.lon.max(self.b.lon), self.a.lat.max(self.b.lat)],
        )
    }
}

#[derive(Clone, Copy)]
struct ParallelMatch {
    draft: usize,
    separation_m: f64,
}

#[must_use]
pub fn conflate(mut strata: Vec<NetworkStratum>, policy: ConflationPolicy) -> ConflatedNetwork {
    strata.sort_by_key(|stratum| stratum.precedence);
    let mut canonical = Vec::<SegmentDraft>::new();
    let mut index = RTree::<IndexedPrimitive>::new();
    let mut report = ConflationReport {
        strata: strata.len(),
        input_drafts: strata.iter().map(|stratum| stratum.drafts.len()).sum(),
        ..ConflationReport::default()
    };

    for mut stratum in strata {
        stratum.drafts.sort_by(draft_order);
        let higher_precedence_count = canonical.len();
        let mut admitted = Vec::new();
        for draft in stratum.drafts {
            let mut run = Vec::<Coord>::new();
            for segment in draft.geometry.points.windows(2) {
                let [a, b] = [segment[0], segment[1]];
                if let Some(parallel) = nearest_parallel(a, b, &index, policy) {
                    seal_run(&draft, &mut run, &mut admitted);
                    report.suppressed_parallel_segments += 1;
                    corroborate(&mut canonical[parallel.draft], &draft);
                    if report.decisions.len() < policy.max_reported_decisions {
                        report.decisions.push(ConflationDecision {
                            preferred: canonical[parallel.draft].provenance.first().cloned(),
                            suppressed: draft.provenance.first().cloned(),
                            separation_m: parallel.separation_m,
                            geometry: LineString::unchecked(vec![a, b]),
                        });
                    }
                } else {
                    append_segment(&mut run, a, b);
                }
            }
            seal_run(&draft, &mut run, &mut admitted);
        }

        canonical.extend(admitted);
        for (draft, line) in canonical.iter().enumerate().skip(higher_precedence_count) {
            for points in line.geometry.points.windows(2) {
                index.insert(IndexedPrimitive {
                    draft,
                    a: points[0],
                    b: points[1],
                });
            }
        }
    }
    report.output_drafts = canonical.len();
    ConflatedNetwork {
        drafts: canonical,
        report,
    }
}

fn nearest_parallel(
    a: Coord,
    b: Coord,
    index: &RTree<IndexedPrimitive>,
    policy: ConflationPolicy,
) -> Option<ParallelMatch> {
    if policy.parallel_tolerance_m <= 0.0 {
        return None;
    }
    let midpoint = a.lerp(b, 0.5);
    let latitude_radius = policy.parallel_tolerance_m / METRES_PER_LATITUDE_DEGREE;
    let longitude_radius = latitude_radius / midpoint.lat.to_radians().cos().abs().max(0.05);
    let envelope = AABB::from_corners(
        [
            midpoint.lon - longitude_radius,
            midpoint.lat - latitude_radius,
        ],
        [
            midpoint.lon + longitude_radius,
            midpoint.lat + latitude_radius,
        ],
    );
    index
        .locate_in_envelope_intersecting(&envelope)
        .filter_map(|candidate| {
            let (separation_m, projection) =
                point_segment_distance(midpoint, candidate.a, candidate.b);
            (separation_m <= policy.parallel_tolerance_m
                && (0.0..=1.0).contains(&projection)
                && parallel_cosine(a, b, candidate.a, candidate.b) >= policy.min_parallel_cosine)
                .then_some(ParallelMatch {
                    draft: candidate.draft,
                    separation_m,
                })
        })
        .min_by(|left, right| left.separation_m.total_cmp(&right.separation_m))
}

fn point_segment_distance(point: Coord, a: Coord, b: Coord) -> (f64, f64) {
    let latitude = point.lat.to_radians().cos();
    let scale_x = METRES_PER_LATITUDE_DEGREE * latitude;
    let scale_y = METRES_PER_LATITUDE_DEGREE;
    let vx = (b.lon - a.lon) * scale_x;
    let vy = (b.lat - a.lat) * scale_y;
    let wx = (point.lon - a.lon) * scale_x;
    let wy = (point.lat - a.lat) * scale_y;
    let length2 = vx.mul_add(vx, vy * vy);
    if length2 <= f64::EPSILON {
        return (point.haversine_m(a), 0.0);
    }
    let projection = vx.mul_add(wx, vy * wy) / length2;
    let t = projection.clamp(0.0, 1.0);
    let dx = wx - t * vx;
    let dy = wy - t * vy;
    (dx.mul_add(dx, dy * dy).sqrt(), projection)
}

fn parallel_cosine(a: Coord, b: Coord, c: Coord, d: Coord) -> f64 {
    let latitude = ((a.lat + b.lat + c.lat + d.lat) * 0.25).to_radians().cos();
    let ab = ((b.lon - a.lon) * latitude, b.lat - a.lat);
    let cd = ((d.lon - c.lon) * latitude, d.lat - c.lat);
    let denominator = ab.0.hypot(ab.1) * cd.0.hypot(cd.1);
    if denominator <= f64::EPSILON {
        0.0
    } else {
        ab.0.mul_add(cd.0, ab.1 * cd.1).abs() / denominator
    }
}

fn append_segment(run: &mut Vec<Coord>, a: Coord, b: Coord) {
    if run.last().is_none_or(|last| !same_location(*last, a)) {
        run.clear();
        run.push(a);
    }
    run.push(b);
}

fn seal_run(draft: &SegmentDraft, run: &mut Vec<Coord>, admitted: &mut Vec<SegmentDraft>) {
    if run.len() < 2 {
        run.clear();
        return;
    }
    let mut fragment = draft.clone();
    fragment.geometry = LineString::unchecked(std::mem::take(run));
    let whole = fragment.geometry.points.len() == draft.geometry.points.len()
        && same_location(fragment.geometry.start(), draft.geometry.start())
        && same_location(fragment.geometry.end(), draft.geometry.end());
    fragment.junctions = if !whole && fragment.junctions == JunctionPolicy::ExplicitNodes {
        JunctionPolicy::ExplicitEndpoints
    } else {
        fragment.junctions
    };
    if !whole {
        fragment.turn_restrictions.clear();
    }
    admitted.push(fragment);
}

fn draft_order(left: &SegmentDraft, right: &SegmentDraft) -> std::cmp::Ordering {
    left.provenance
        .cmp(&right.provenance)
        .then_with(|| {
            left.geometry
                .start()
                .lon
                .total_cmp(&right.geometry.start().lon)
        })
        .then_with(|| {
            left.geometry
                .start()
                .lat
                .total_cmp(&right.geometry.start().lat)
        })
        .then_with(|| left.geometry.end().lon.total_cmp(&right.geometry.end().lon))
        .then_with(|| left.geometry.end().lat.total_cmp(&right.geometry.end().lat))
}

fn corroborate(preferred: &mut SegmentDraft, suppressed: &SegmentDraft) {
    preferred
        .provenance
        .extend(suppressed.provenance.iter().cloned());
    preferred.provenance.sort();
    preferred.provenance.dedup();
    preferred.confidence = preferred.confidence.max(suppressed.confidence);
    if preferred.trail_class == TrailClass::Unknown {
        preferred.trail_class = suppressed.trail_class;
    }
    if preferred.standing == crate::TrailStanding::Unknown {
        preferred.standing = suppressed.standing;
    }
    if preferred.terrain == Terrain::Unknown {
        preferred.terrain = suppressed.terrain;
        preferred.terrain_confidence = suppressed.terrain_confidence;
    }
    if preferred.surface.is_none() {
        preferred.surface.clone_from(&suppressed.surface);
    }
    if preferred.access == Access::Unknown {
        preferred.access = suppressed.access;
    }
}

const fn same_location(left: Coord, right: Coord) -> bool {
    left.lon.to_bits() == right.lon.to_bits() && left.lat.to_bits() == right.lat.to_bits()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EdgeTravel, TrailStanding};

    fn draft(name: &str, latitude: f64, standing: TrailStanding) -> SegmentDraft {
        SegmentDraft {
            geometry: LineString::unchecked(vec![
                Coord::new(-74.1, latitude),
                Coord::new(-74.0, latitude),
            ]),
            junctions: JunctionPolicy::Planar,
            turn_ref: None,
            turn_restrictions: Vec::new(),
            trail_class: TrailClass::Path,
            standing,
            terrain: Terrain::Trail,
            terrain_confidence: Some(0.8),
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 0.8,
            provenance: vec![Provenance::fixture(name)],
        }
    }

    #[test]
    fn higher_strata_suppress_parallel_duplicates_and_keep_provenance() {
        let primary = draft("osm", 41.2, TrailStanding::Informal);
        let secondary = draft("usgs", 41.200_03, TrailStanding::Established);
        let network = conflate(
            vec![
                NetworkStratum {
                    precedence: 0,
                    drafts: vec![primary],
                },
                NetworkStratum {
                    precedence: 10,
                    drafts: vec![secondary],
                },
            ],
            ConflationPolicy::default(),
        );
        assert_eq!(network.drafts.len(), 1);
        assert_eq!(network.drafts[0].standing, TrailStanding::Informal);
        assert_eq!(network.drafts[0].provenance.len(), 2);
        assert_eq!(network.report.suppressed_parallel_segments, 1);
    }

    #[test]
    fn nonparallel_and_distant_segments_survive() {
        let primary = draft("osm", 41.2, TrailStanding::Established);
        let mut crossing = draft("usgs-crossing", 41.2, TrailStanding::Established);
        crossing.geometry =
            LineString::unchecked(vec![Coord::new(-74.05, 41.19), Coord::new(-74.05, 41.21)]);
        let distant = draft("usgs-distant", 41.201, TrailStanding::Established);
        let network = conflate(
            vec![
                NetworkStratum {
                    precedence: 0,
                    drafts: vec![primary],
                },
                NetworkStratum {
                    precedence: 10,
                    drafts: vec![crossing, distant],
                },
            ],
            ConflationPolicy::default(),
        );
        assert_eq!(network.drafts.len(), 3);
        assert_eq!(network.report.suppressed_parallel_segments, 0);
    }

    #[test]
    fn output_is_stable_under_equal_stratum_permutation() {
        let a = draft("a", 41.2, TrailStanding::Established);
        let b = draft("b", 41.3, TrailStanding::Established);
        let forward = conflate(
            vec![NetworkStratum {
                precedence: 0,
                drafts: vec![a.clone(), b.clone()],
            }],
            ConflationPolicy::default(),
        );
        let reverse = conflate(
            vec![NetworkStratum {
                precedence: 0,
                drafts: vec![b, a],
            }],
            ConflationPolicy::default(),
        );
        let mut forward_ids = forward
            .drafts
            .iter()
            .filter_map(|draft| draft.provenance[0].source_id.clone())
            .collect::<Vec<_>>();
        let mut reverse_ids = reverse
            .drafts
            .iter()
            .filter_map(|draft| draft.provenance[0].source_id.clone())
            .collect::<Vec<_>>();
        forward_ids.sort();
        reverse_ids.sort();
        assert_eq!(forward_ids, reverse_ids);
    }
}
