use crate::{Access, Coord, JunctionKey, LineString, Provenance, SegmentDraft, Terrain, WayKind};
use rstar::{AABB, RTree, RTreeObject};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const METRES_PER_LATITUDE_DEGREE: f64 = 111_320.0;
const SEAT_EPSILON_M: f64 = 1.0e-6;

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

#[derive(Clone, Copy)]
struct LineProjection {
    along_m: f64,
    coord: Coord,
    distance_m: f64,
}

#[derive(Clone)]
struct IncidenceTransfer {
    draft: usize,
    key: JunctionKey,
    projection: LineProjection,
}

#[derive(Clone)]
struct JunctionSeat {
    draft: usize,
    projection: LineProjection,
    key: JunctionKey,
}

#[must_use]
pub fn conflate(mut strata: Vec<NetworkStratum>, policy: ConflationPolicy) -> ConflatedNetwork {
    strata.sort_by_key(|stratum| stratum.precedence);
    let mut canonical = Vec::<SegmentDraft>::new();
    let mut index = RTree::<IndexedPrimitive>::new();
    let mut incidence = Vec::<IncidenceTransfer>::new();
    let mut report = ConflationReport {
        strata: strata.len(),
        input_drafts: strata.iter().map(|stratum| stratum.drafts.len()).sum(),
        ..ConflationReport::default()
    };

    for mut stratum in strata {
        stratum.drafts.sort_by(draft_order);
        let source_incidence = source_incidence_keys(&stratum.drafts);
        let higher_precedence_count = canonical.len();
        let mut admitted = Vec::new();
        for draft in stratum.drafts {
            let mut run = Vec::<Coord>::new();
            for (segment_index, segment) in draft.geometry.points.windows(2).enumerate() {
                let [a, b] = [segment[0], segment[1]];
                if let Some(parallel) = nearest_parallel(&draft, a, b, &canonical, &index, policy) {
                    let Some(transfers) = project_source_incidence(
                        &draft,
                        segment_index,
                        parallel.draft,
                        &canonical[parallel.draft],
                        policy.parallel_tolerance_m,
                        &source_incidence,
                    ) else {
                        append_segment(&mut run, a, b);
                        continue;
                    };
                    seal_run(&draft, &mut run, &mut admitted);
                    report.suppressed_parallel_segments += 1;
                    corroborate(&mut canonical[parallel.draft], &draft);
                    incidence.extend(transfers);
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
    canonical = seat_source_incidence(canonical, incidence);
    report.output_drafts = canonical.len();
    ConflatedNetwork {
        drafts: canonical,
        report,
    }
}

fn source_incidence_keys(drafts: &[SegmentDraft]) -> BTreeSet<JunctionKey> {
    let mut occurrences = BTreeMap::<JunctionKey, usize>::new();
    for key in drafts
        .iter()
        .filter_map(|draft| draft.junction_keys.as_ref())
        .flatten()
        .filter(|key| key.is_source_owned())
    {
        *occurrences.entry(key.clone()).or_default() += 1;
    }
    occurrences
        .into_iter()
        .filter_map(|(key, occurrences)| (occurrences > 1).then_some(key))
        .collect()
}

fn project_source_incidence(
    draft: &SegmentDraft,
    segment: usize,
    preferred_index: usize,
    preferred: &SegmentDraft,
    tolerance_m: f64,
    source_incidence: &BTreeSet<JunctionKey>,
) -> Option<Vec<IncidenceTransfer>> {
    let Some(keys) = draft.junction_keys.as_ref() else {
        return Some(Vec::new());
    };
    let last_segment = draft.geometry.points.len() - 2;
    let endpoints = [
        (segment == 0).then_some((&keys[0], draft.geometry.start())),
        (segment == last_segment).then_some((&keys[1], draft.geometry.end())),
    ];
    endpoints
        .into_iter()
        .flatten()
        .filter(|(key, _)| source_incidence.contains(*key))
        .map(|(key, coord)| {
            let projection = project_onto_line(coord, &preferred.geometry);
            (projection.distance_m <= tolerance_m).then(|| IncidenceTransfer {
                draft: preferred_index,
                key: key.clone(),
                projection,
            })
        })
        .collect()
}

fn project_onto_line(point: Coord, line: &LineString) -> LineProjection {
    let mut traversed_m = 0.0;
    line.points
        .windows(2)
        .map(|points| {
            let [a, b] = [points[0], points[1]];
            let (distance_m, t) = point_segment_distance(point, a, b);
            let t = t.clamp(0.0, 1.0);
            let length_m = a.haversine_m(b);
            let projection = LineProjection {
                along_m: length_m.mul_add(t, traversed_m),
                coord: a.lerp(b, t),
                distance_m,
            };
            traversed_m += length_m;
            projection
        })
        .min_by(|left, right| {
            left.distance_m
                .total_cmp(&right.distance_m)
                .then_with(|| left.along_m.total_cmp(&right.along_m))
        })
        .expect("line strings contain at least one segment")
}

fn seat_source_incidence(
    drafts: Vec<SegmentDraft>,
    transfers: Vec<IncidenceTransfer>,
) -> Vec<SegmentDraft> {
    if transfers.is_empty() {
        return drafts;
    }
    let mut transfers_by_draft = BTreeMap::<usize, Vec<IncidenceTransfer>>::new();
    for transfer in transfers {
        transfers_by_draft
            .entry(transfer.draft)
            .or_default()
            .push(transfer);
    }

    let mut seats = Vec::<JunctionSeat>::new();
    for (draft_index, mut transfers) in transfers_by_draft {
        transfers.sort_by(|left, right| {
            left.projection
                .along_m
                .total_cmp(&right.projection.along_m)
                .then_with(|| left.key.cmp(&right.key))
        });
        for transfer in transfers {
            if let Some(seat) = seats.last_mut()
                && seat.draft == draft_index
                && seat.key == transfer.key
                && (seat.projection.along_m - transfer.projection.along_m).abs() <= SEAT_EPSILON_M
            {
                if transfer.projection.distance_m < seat.projection.distance_m {
                    seat.projection = transfer.projection;
                }
                continue;
            }
            seats.push(JunctionSeat {
                draft: draft_index,
                projection: transfer.projection,
                key: transfer.key,
            });
        }
    }

    let canonical_coordinates = canonical_seat_coordinates(&seats);
    for seat in &mut seats {
        seat.projection.coord = canonical_coordinates[&seat.key].coord;
    }

    let mut seats_by_draft = BTreeMap::<usize, Vec<JunctionSeat>>::new();
    for seat in seats {
        seats_by_draft.entry(seat.draft).or_default().push(seat);
    }
    drafts
        .into_iter()
        .enumerate()
        .flat_map(|(index, draft)| match seats_by_draft.remove(&index) {
            Some(seats) => split_at_seats(&draft, index, seats),
            None => vec![draft],
        })
        .collect()
}

fn canonical_seat_coordinates(seats: &[JunctionSeat]) -> BTreeMap<JunctionKey, LineProjection> {
    let mut canonical = BTreeMap::<JunctionKey, LineProjection>::new();
    for seat in seats {
        if canonical
            .get(&seat.key)
            .is_none_or(|known| seat.projection.distance_m < known.distance_m)
        {
            canonical.insert(seat.key.clone(), seat.projection);
        }
    }
    canonical
}

#[derive(Clone)]
struct Breakpoint {
    along_m: f64,
    coord: Coord,
    key: JunctionKey,
}

fn split_at_seats(
    draft: &SegmentDraft,
    draft_index: usize,
    seats: Vec<JunctionSeat>,
) -> Vec<SegmentDraft> {
    let length_m = draft.geometry.length_m();
    let mut breaks = seats
        .into_iter()
        .map(|seat| Breakpoint {
            along_m: seat.projection.along_m,
            coord: seat.projection.coord,
            key: seat.key,
        })
        .collect::<Vec<_>>();
    let outer_key = |side: usize, coord: Coord| {
        draft.junction_keys.as_ref().map_or_else(
            || {
                JunctionKey(format!(
                    "seam:incidence:{draft_index}:{side}:{:016x}:{:016x}",
                    coord.lon.to_bits(),
                    coord.lat.to_bits()
                ))
            },
            |keys| keys[side].clone(),
        )
    };
    if breaks
        .first()
        .is_none_or(|seat| seat.along_m > SEAT_EPSILON_M)
    {
        breaks.insert(
            0,
            Breakpoint {
                along_m: 0.0,
                coord: draft.geometry.start(),
                key: outer_key(0, draft.geometry.start()),
            },
        );
    }
    if breaks
        .last()
        .is_none_or(|seat| length_m - seat.along_m > SEAT_EPSILON_M)
    {
        breaks.push(Breakpoint {
            along_m: length_m,
            coord: draft.geometry.end(),
            key: outer_key(1, draft.geometry.end()),
        });
    }
    let mut vertex_along = Vec::with_capacity(draft.geometry.points.len());
    vertex_along.push(0.0);
    for points in draft.geometry.points.windows(2) {
        vertex_along.push(
            vertex_along.last().copied().unwrap_or_default() + points[0].haversine_m(points[1]),
        );
    }
    breaks
        .windows(2)
        .filter_map(|pair| {
            let geometry = geometry_between(&draft.geometry, &vertex_along, &pair[0], &pair[1])?;
            let mut fragment = draft.seam_fragment(geometry);
            fragment.junction_keys = Some([pair[0].key.clone(), pair[1].key.clone()]);
            Some(fragment)
        })
        .collect()
}

fn geometry_between(
    line: &LineString,
    vertex_along: &[f64],
    start: &Breakpoint,
    end: &Breakpoint,
) -> Option<LineString> {
    if end.along_m - start.along_m <= SEAT_EPSILON_M {
        return None;
    }
    let mut points = vec![start.coord];
    for (coord, along_m) in line
        .points
        .iter()
        .copied()
        .zip(vertex_along.iter().copied())
    {
        if along_m > start.along_m + SEAT_EPSILON_M
            && along_m < end.along_m - SEAT_EPSILON_M
            && points
                .last()
                .is_none_or(|last| !same_location(*last, coord))
        {
            points.push(coord);
        }
    }
    if points
        .last()
        .is_none_or(|last| !same_location(*last, end.coord))
    {
        points.push(end.coord);
    }
    (points.len() >= 2).then(|| LineString::unchecked(points))
}

fn nearest_parallel(
    draft: &SegmentDraft,
    a: Coord,
    b: Coord,
    canonical: &[SegmentDraft],
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
        .locate_in_envelope_intersecting(envelope)
        .filter_map(|candidate| {
            if !same_facility(draft, &canonical[candidate.draft]) {
                return None;
            }
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

fn same_facility(left: &SegmentDraft, right: &SegmentDraft) -> bool {
    if left.geometry_claim != right.geometry_claim {
        return false;
    }
    let protected = |kind| {
        matches!(
            kind,
            WayKind::Sidewalk
                | WayKind::Crossing
                | WayKind::PedestrianStreet
                | WayKind::Roadway
                | WayKind::ServiceRoad
                | WayKind::Cycleway
                | WayKind::Bushwhack
        )
    };
    !(protected(left.way_kind) || protected(right.way_kind)) || left.way_kind == right.way_kind
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
    admitted.push(draft.seam_fragment(LineString::unchecked(std::mem::take(run))));
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
    for provenance in &suppressed.provenance {
        if !preferred.provenance.contains(provenance) {
            preferred.provenance.push(provenance.clone());
        }
    }
    preferred.confidence = preferred.confidence.max(suppressed.confidence);
    if preferred.way_kind == WayKind::Unknown {
        preferred.way_kind = suppressed.way_kind;
    }
    if preferred.standing == crate::TrailStanding::Unknown {
        preferred.standing = suppressed.standing;
    }
    if preferred.marking == crate::TrailMarking::Unknown {
        preferred.marking = suppressed.marking;
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
    use crate::{
        CrossingControl, EdgeTravel, GeometryClaim, GraphBuilder, JunctionKey, JunctionPolicy,
        TrailMarking, TrailStanding, WayRealm,
    };

    fn draft(name: &str, latitude: f64, standing: TrailStanding) -> SegmentDraft {
        SegmentDraft {
            geometry: LineString::unchecked(vec![
                Coord::new(-74.1, latitude),
                Coord::new(-74.0, latitude),
            ]),
            junctions: JunctionPolicy::Planar,
            turn_ref: None,
            junction_keys: None,
            turn_restrictions: Vec::new(),
            way_kind: WayKind::Path,
            realm: WayRealm::default(),
            geometry_claim: GeometryClaim::default(),
            crossing_control: CrossingControl::default(),
            standing,
            marking: TrailMarking::Unknown,
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
        let mut primary = draft("osm", 41.2, TrailStanding::Informal);
        primary.geometry =
            LineString::unchecked(vec![Coord::new(-74.08, 41.2), Coord::new(-74.02, 41.2)]);
        primary.provenance[0].source = "z-authority".to_owned();
        let mut secondary = draft("usgs", 41.200_03, TrailStanding::Established);
        secondary.geometry = LineString::unchecked(vec![
            Coord::new(-74.1, 41.200_03),
            Coord::new(-74.08, 41.200_03),
            Coord::new(-74.02, 41.200_03),
            Coord::new(-74.0, 41.200_03),
        ]);
        secondary.junctions = JunctionPolicy::ExplicitNodes;
        secondary.junction_keys = Some([
            JunctionKey("osm:start".to_owned()),
            JunctionKey("osm:end".to_owned()),
        ]);
        secondary.provenance[0].source = "a-corroborator".to_owned();
        secondary.marking = TrailMarking::Marked;
        let trace = secondary.geometry.clone();
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
        assert_eq!(network.drafts.len(), 3);
        assert_eq!(network.drafts[0].standing, TrailStanding::Informal);
        assert_eq!(network.drafts[0].marking, TrailMarking::Marked);
        assert_eq!(network.drafts[0].provenance.len(), 2);
        assert_eq!(network.drafts[0].provenance[0].source, "z-authority");
        assert_eq!(network.drafts[0].provenance[1].source, "a-corroborator");
        assert_eq!(network.report.suppressed_parallel_segments, 1);
        let graph = GraphBuilder::default()
            .build(&network.drafts)
            .expect("conflated network is valid");
        let coverage = graph.trace_coverage(&trace, 10.0);
        assert_eq!(coverage.stats.disconnected_transition_count, 0);
        assert_eq!(coverage.stats.rejected_segment_count, 0);
    }

    #[test]
    fn replacement_geometry_inherits_source_incidence() {
        let mut preferred = draft("authority", 41.2, TrailStanding::Established);
        preferred.geometry = LineString::unchecked(vec![
            Coord::new(-74.1, 41.2),
            Coord::new(-74.07, 41.2),
            Coord::new(-74.04, 41.2),
            Coord::new(-74.0, 41.2),
        ]);
        let mut corroborator = draft("osm-trunk", 41.200_03, TrailStanding::Established);
        corroborator.geometry = LineString::unchecked(vec![
            Coord::new(-74.08, 41.200_03),
            Coord::new(-74.02, 41.200_03),
        ]);
        corroborator.junctions = JunctionPolicy::ExplicitEndpoints;
        corroborator.junction_keys = Some([
            JunctionKey("osm:west".to_owned()),
            JunctionKey("osm:fork".to_owned()),
        ]);
        let mut branch = draft("osm-branch", 41.200_03, TrailStanding::Established);
        branch.geometry = LineString::unchecked(vec![
            Coord::new(-74.02, 41.200_03),
            Coord::new(-74.02, 41.21),
        ]);
        branch.junctions = JunctionPolicy::ExplicitEndpoints;
        branch.junction_keys = Some([
            JunctionKey("osm:fork".to_owned()),
            JunctionKey("osm:north".to_owned()),
        ]);

        let network = conflate(
            vec![
                NetworkStratum {
                    precedence: 0,
                    drafts: vec![preferred],
                },
                NetworkStratum {
                    precedence: 10,
                    drafts: vec![corroborator, branch],
                },
            ],
            ConflationPolicy::default(),
        );
        let graph = GraphBuilder::default()
            .build(&network.drafts)
            .expect("conflated network is valid");
        let junction = graph
            .vertices
            .iter()
            .find(|vertex| {
                vertex
                    .junction
                    .as_ref()
                    .is_some_and(|key| key.0 == "osm:fork")
            })
            .expect("source junction survives on its preferred replacement");

        // Conflation used to transfer attributes but discard this exact source
        // incidence, stranding the branch a few metres from the preferred line.
        assert_eq!(graph.adjacency[junction.id.0].len(), 3);
        assert!(junction.coord.haversine_m(Coord::new(-74.02, 41.2)) < 0.01);
    }

    #[test]
    fn conflation_cannot_erase_distinct_pedestrian_facilities() {
        let mut road = draft("road", 41.2, TrailStanding::Established);
        road.way_kind = WayKind::Roadway;
        road.realm = WayRealm::Urban;
        road.terrain = Terrain::Road;
        let mut sidewalk = draft("sidewalk", 41.2, TrailStanding::Established);
        sidewalk.way_kind = WayKind::Sidewalk;
        sidewalk.realm = WayRealm::Urban;
        sidewalk.terrain = Terrain::Pavement;

        let network = conflate(
            vec![
                NetworkStratum {
                    precedence: 0,
                    drafts: vec![road],
                },
                NetworkStratum {
                    precedence: 10,
                    drafts: vec![sidewalk],
                },
            ],
            ConflationPolicy::default(),
        );

        assert_eq!(network.drafts.len(), 2);
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
