use crate::{
    basemap::{self, TileKey},
    cadence::Pattern,
    map::{
        CadenceLineage, MapFramePlan, TrailMark, TrailSalience, WorldEdge, trail_class_color,
        trail_core, trail_pattern,
    },
};
use bytemuck::{Pod, Zeroable};
use egui::Painter;
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor, wgpu};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use wgpu::util::DeviceExt as _;

const BASE_TILE_ZOOM: u8 = 12;
const FIRST_BAND: u8 = 9;
const LAST_BAND: u8 = 14;
const BAND_COUNT: usize = (LAST_BAND - FIRST_BAND + 1) as usize;
const SIMPLIFICATION_ERROR_POINTS: f64 = 0.42;
const TUBE_ONSET_ZOOM: f32 = 9.33;
const CORE_ONSET_ZOOM: f32 = 9.77;
const PATTERN_ONSET_ZOOM: f32 = 10.19;
const DISCLOSURE_SPAN_ZOOM: f32 = 0.72;
const _: () = assert!(TUBE_ONSET_ZOOM >= FIRST_BAND as f32);
const _: () = assert!(TUBE_ONSET_ZOOM < CORE_ONSET_ZOOM);
const _: () = assert!(CORE_ONSET_ZOOM < PATTERN_ONSET_ZOOM);
const GPU_CEILING: usize = 256 * 1_048_576;
const MAX_WRAP_RADIUS: u32 = 2;
const MAX_WRAP_INSTANCES: usize = (MAX_WRAP_RADIUS * 2 + 1) as usize;
const LAYER_COUNT: usize = 2;
const MAX_LAYER_INSTANCES: usize = MAX_WRAP_INSTANCES * LAYER_COUNT;
const PATTERN_COUNT: usize = 3;
static NEXT_CORPUS: AtomicU64 = AtomicU64::new(1);

pub struct TrailField {
    corpus: TrailCorpus,
    laws: Arc<[CadenceDatum]>,
    tiles: HashMap<TileKey, Arc<TrailTile>>,
    visibility: Option<Visibility>,
    phase_transport: PhaseTransport,
}

struct Visibility {
    band: DetailBand,
    keys: Vec<TileKey>,
    tiles: Arc<[Arc<TrailTile>]>,
}

impl TrailField {
    pub fn forge(edges: &[WorldEdge]) -> Self {
        let begun = Instant::now();
        let (law_ids, laws) = cadence_laws(edges);
        let mut tiles = HashMap::<TileKey, Vec<TrailMeshBuilder>>::new();
        for (edge_id, edge) in edges.iter().enumerate() {
            let samples = samples(edge);
            for band in 0..BAND_COUNT {
                let band = DetailBand::from_index(band);
                let simplified = simplify(&samples, band.tolerance_world());
                for fragment in cleave(&simplified, band.spatial_zoom()) {
                    tiles.entry(fragment.key).or_insert_with(|| {
                        (0..BAND_COUNT)
                            .map(|_| TrailMeshBuilder::default())
                            .collect()
                    })[band.index()]
                    .push(
                        &fragment.points,
                        fragment.key,
                        edge,
                        law_ids[edge_id],
                        laws[law_ids[edge_id] as usize],
                    );
                }
            }
        }
        let tiles = tiles
            .into_iter()
            .map(|(key, bands)| {
                (
                    key,
                    Arc::new(TrailTile {
                        key,
                        bands: bands.into_iter().map(TrailMeshBuilder::seal).collect(),
                    }),
                )
            })
            .collect::<HashMap<_, _>>();
        if std::env::var_os("TRAILGEN_PROFILE_TRAILS").is_some() {
            let bytes = tiles
                .values()
                .map(|tile| tile.resident_bytes())
                .sum::<usize>();
            eprintln!(
                "trail-atlas forge_us={} edges={} tiles={} cpu_bytes={bytes}",
                begun.elapsed().as_micros(),
                edges.len(),
                tiles.len(),
            );
        }
        Self {
            corpus: TrailCorpus::mint(),
            laws: laws.into(),
            tiles,
            visibility: None,
            phase_transport: PhaseTransport::default(),
        }
    }

    pub fn paint(&mut self, painter: &Painter, frame: MapFramePlan) {
        let Some(band) = DetailBand::for_zoom(frame.zoom.get()) else {
            return;
        };
        let keys = visible_keys(frame, &self.tiles, band.spatial_zoom());
        let changed = self
            .visibility
            .as_ref()
            .is_none_or(|visibility| visibility.band != band || visibility.keys != keys);
        if changed {
            let tiles = keys
                .iter()
                .filter_map(|key| self.tiles.get(key).cloned())
                .collect::<Arc<[_]>>();
            self.visibility = Some(Visibility { band, keys, tiles });
        }
        let visibility = self
            .visibility
            .as_ref()
            .expect("trail visibility was just established");
        if visibility.tiles.is_empty() {
            return;
        }
        let tiles = Arc::clone(&visibility.tiles);
        let moments = tiles.iter().fold(
            [CadenceMoment::default(); PATTERN_COUNT],
            |mut sum, tile| {
                for (whole, part) in sum.iter_mut().zip(tile.bands[band.index()].moments.iter()) {
                    *whole += *part;
                }
                sum
            },
        );
        let world_points = frame.world_points as f32;
        let phase_offsets = self
            .phase_transport
            .advance(f64::from(world_points), moments);
        painter.add(egui_wgpu::Callback::new_paint_callback(
            frame.rect,
            TrailPaint {
                corpus: self.corpus,
                laws: Arc::clone(&self.laws),
                band,
                tiles,
                center_world: frame.viewport.center,
                world_points,
                viewport_points: [frame.rect.width(), frame.rect.height()],
                view_zoom: frame.zoom.get() as f32,
                phase_offsets,
            },
        ));
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct DetailBand(u8);

impl DetailBand {
    fn for_zoom(zoom: f64) -> Option<Self> {
        (zoom > f64::from(TUBE_ONSET_ZOOM)).then(|| {
            Self(
                (zoom.floor() as u8)
                    .clamp(FIRST_BAND, LAST_BAND)
                    .saturating_sub(FIRST_BAND),
            )
        })
    }

    const fn from_index(index: usize) -> Self {
        assert!(index < BAND_COUNT, "trail detail band is out of range");
        Self(index as u8)
    }

    const fn index(self) -> usize {
        self.0 as usize
    }

    const fn zoom(self) -> f32 {
        (FIRST_BAND + self.0) as f32
    }

    fn tolerance_world(self) -> f64 {
        if self.zoom() as u8 == LAST_BAND {
            0.0
        } else {
            SIMPLIFICATION_ERROR_POINTS / (256.0 * f64::from(self.zoom() + 1.0).exp2())
        }
    }

    const fn spatial_zoom(self) -> u8 {
        let zoom = FIRST_BAND + self.0;
        if zoom > BASE_TILE_ZOOM {
            zoom
        } else {
            BASE_TILE_ZOOM
        }
    }
}

#[derive(Clone, Copy)]
struct Sample {
    world: [f64; 2],
    arc_world: f64,
}

fn samples(edge: &WorldEdge) -> Vec<Sample> {
    let mut arc_world = 0.0;
    edge.points
        .iter()
        .copied()
        .enumerate()
        .map(|(slot, world)| {
            if slot > 0 {
                let prior = edge.points[slot - 1];
                arc_world += (world[0] - prior[0]).hypot(world[1] - prior[1]);
            }
            Sample { world, arc_world }
        })
        .collect()
}

fn simplify(points: &[Sample], tolerance: f64) -> Vec<Sample> {
    if points.len() <= 2 || tolerance <= 0.0 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut frontier = vec![(0, points.len() - 1)];
    while let Some((start, end)) = frontier.pop() {
        if end <= start + 1 {
            continue;
        }
        let mut champion = None;
        for slot in start + 1..end {
            let error =
                segment_distance(points[slot].world, points[start].world, points[end].world);
            if champion.is_none_or(|(_, prior)| error > prior) {
                champion = Some((slot, error));
            }
        }
        if let Some((slot, error)) = champion
            && error > tolerance
        {
            keep[slot] = true;
            frontier.extend([(start, slot), (slot, end)]);
        }
    }
    points
        .iter()
        .copied()
        .zip(keep)
        .filter_map(|(point, keep)| keep.then_some(point))
        .collect()
}

fn segment_distance(point: [f64; 2], start: [f64; 2], end: [f64; 2]) -> f64 {
    let edge = [end[0] - start[0], end[1] - start[1]];
    let length_squared = edge[0].mul_add(edge[0], edge[1] * edge[1]);
    if length_squared <= f64::EPSILON {
        return (point[0] - start[0]).hypot(point[1] - start[1]);
    }
    let offset = [point[0] - start[0], point[1] - start[1]];
    let progress = offset[0].mul_add(edge[0], offset[1] * edge[1]) / length_squared;
    let progress = progress.clamp(0.0, 1.0);
    (point[0] - edge[0].mul_add(progress, start[0]))
        .hypot(point[1] - edge[1].mul_add(progress, start[1]))
}

struct Fragment {
    key: TileKey,
    points: Vec<Sample>,
}

fn cleave(points: &[Sample], tile_zoom: u8) -> Vec<Fragment> {
    let scale = f64::from(1_u32 << tile_zoom);
    let mut fragments = Vec::<Fragment>::new();
    for pair in points.windows(2) {
        let [start, end] = [pair[0], pair[1]];
        let mut cuts = vec![0.0, 1.0];
        cut_axis(start.world[0], end.world[0], scale, &mut cuts);
        cut_axis(start.world[1], end.world[1], scale, &mut cuts);
        cuts.sort_unstable_by(f64::total_cmp);
        cuts.dedup_by(|left, right| (*left - *right).abs() <= 1.0e-12);
        for interval in cuts.windows(2) {
            let [enter, exit] = [interval[0], interval[1]];
            if exit - enter <= 1.0e-12 {
                continue;
            }
            let midpoint = sample_between(start, end, (enter + exit) * 0.5);
            let x = (midpoint.world[0] * scale).floor() as i64;
            let y = (midpoint.world[1] * scale).floor().clamp(0.0, scale - 1.0) as u32;
            let key = TileKey {
                zoom: tile_zoom,
                x: x.rem_euclid(scale as i64) as u32,
                y,
            };
            let a = sample_between(start, end, enter);
            let b = sample_between(start, end, exit);
            if let Some(fragment) = fragments.last_mut()
                && fragment.key == key
                && fragment
                    .points
                    .last()
                    .is_some_and(|prior| same_world(prior.world, a.world))
            {
                fragment.points.push(b);
            } else {
                fragments.push(Fragment {
                    key,
                    points: vec![a, b],
                });
            }
        }
    }
    fragments
}

fn cut_axis(start: f64, end: f64, scale: f64, cuts: &mut Vec<f64>) {
    let delta = end - start;
    if delta.abs() <= f64::EPSILON {
        return;
    }
    let low = (start.min(end) * scale).floor() as i64 + 1;
    let high = (start.max(end) * scale).floor() as i64;
    for boundary in low..=high {
        let progress = (boundary as f64 / scale - start) / delta;
        if (1.0e-12..1.0 - 1.0e-12).contains(&progress) {
            cuts.push(progress);
        }
    }
}

fn sample_between(start: Sample, end: Sample, progress: f64) -> Sample {
    Sample {
        world: [
            (end.world[0] - start.world[0]).mul_add(progress, start.world[0]),
            (end.world[1] - start.world[1]).mul_add(progress, start.world[1]),
        ],
        arc_world: (end.arc_world - start.arc_world).mul_add(progress, start.arc_world),
    }
}

fn same_world(left: [f64; 2], right: [f64; 2]) -> bool {
    (left[0] - right[0]).abs() <= 1.0e-12 && (left[1] - right[1]).abs() <= 1.0e-12
}

#[derive(Default)]
struct TrailMeshBuilder {
    vertices: Vec<TrailPoint>,
    indices: Vec<u32>,
    moments: [CadenceMoment; PATTERN_COUNT],
}

impl TrailMeshBuilder {
    fn push(
        &mut self,
        samples: &[Sample],
        key: TileKey,
        edge: &WorldEdge,
        law_id: u32,
        law: CadenceDatum,
    ) {
        if samples.len() < 2 {
            return;
        }
        if let Some((pattern, moment)) = law.moment(
            samples
                .first()
                .expect("trail fragment is nonempty")
                .arc_world,
            samples
                .last()
                .expect("trail fragment is nonempty")
                .arc_world,
        ) {
            self.moments[pattern] += moment;
        }
        let scale = f64::from(1_u32 << key.zoom);
        let local = samples
            .iter()
            .map(|sample| {
                [
                    sample.world[0].mul_add(scale, -f64::from(key.x)) as f32,
                    sample.world[1].mul_add(scale, -f64::from(key.y)) as f32,
                ]
            })
            .collect::<Vec<_>>();
        let color = TrailSalience::Context
            .access_color(trail_class_color(edge.trail_class), edge.access)
            .to_array();
        let pattern = pattern_code(edge.mark);
        let base = u32::try_from(self.vertices.len()).expect("trail tile vertex count fits u32");
        for (slot, point) in local.iter().copied().enumerate() {
            let extrusion = basemap::join_normal(&local, slot);
            self.vertices.extend([
                TrailPoint {
                    local: point,
                    extrusion: [-extrusion[0], -extrusion[1]],
                    srgb: color,
                    arc_world: samples[slot].arc_world as f32,
                    cadence: cadence_word(law_id, pattern, false),
                },
                TrailPoint {
                    local: point,
                    extrusion,
                    srgb: color,
                    arc_world: samples[slot].arc_world as f32,
                    cadence: cadence_word(law_id, pattern, true),
                },
            ]);
        }
        for slot in 0..local.len() - 1 {
            let offset = u32::try_from(slot)
                .expect("trail fragment length fits u32")
                .checked_mul(2)
                .expect("trail fragment index fits u32");
            let a = base + offset;
            self.indices.extend([a, a + 1, a + 2, a + 1, a + 3, a + 2]);
        }
    }

    fn seal(mut self) -> TrailMesh {
        let mut global_ids = self
            .vertices
            .iter()
            .filter(|vertex| vertex.pattern() != 0)
            .map(TrailPoint::law)
            .collect::<Vec<_>>();
        global_ids.sort_unstable();
        global_ids.dedup();
        let remap = global_ids
            .iter()
            .enumerate()
            .map(|(slot, global)| {
                (
                    *global,
                    u32::try_from(slot + 1).expect("tile-local cadence law count fits u32"),
                )
            })
            .collect::<HashMap<_, _>>();
        for vertex in &mut self.vertices {
            let local = if vertex.pattern() == 0 {
                0
            } else {
                remap[&vertex.law()]
            };
            vertex.set_law(local);
        }
        TrailMesh {
            vertices: self.vertices.into(),
            indices: self.indices.into(),
            law_ids: std::iter::once(0).chain(global_ids).collect(),
            moments: self.moments,
        }
    }
}

const fn pattern_code(mark: TrailMark) -> u32 {
    match mark {
        TrailMark::Solid => 0,
        TrailMark::Dashed => 1,
        TrailMark::DashDot => 2,
        TrailMark::Unmarked => 3,
    }
}

const fn cadence_word(law: u32, pattern: u32, positive_side: bool) -> u32 {
    assert!(
        law <= u32::MAX >> 3,
        "cadence law exceeds packed vertex range"
    );
    assert!(pattern < 4, "trail pattern exceeds packed vertex range");
    (law << 3) | (pattern << 1) | positive_side as u32
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TrailPoint {
    local: [f32; 2],
    extrusion: [f32; 2],
    srgb: [u8; 4],
    arc_world: f32,
    cadence: u32,
}
const _: () = assert!(size_of::<TrailPoint>() == 28);

impl TrailPoint {
    const fn law(&self) -> u32 {
        self.cadence >> 3
    }

    const fn pattern(&self) -> u32 {
        (self.cadence >> 1) & 3
    }

    const fn set_law(&mut self, law: u32) {
        self.cadence = cadence_word(law, self.pattern(), self.cadence & 1 != 0);
    }
}

struct TrailMesh {
    vertices: Arc<[TrailPoint]>,
    indices: Arc<[u32]>,
    law_ids: Arc<[u32]>,
    moments: [CadenceMoment; PATTERN_COUNT],
}

struct TrailTile {
    key: TileKey,
    bands: Vec<TrailMesh>,
}

impl TrailTile {
    fn resident_bytes(&self) -> usize {
        self.bands
            .iter()
            .map(|mesh| {
                mesh.vertices
                    .len()
                    .saturating_mul(size_of::<TrailPoint>())
                    .saturating_add(mesh.indices.len().saturating_mul(size_of::<u32>()))
                    .saturating_add(mesh.law_ids.len().saturating_mul(size_of::<u32>()))
            })
            .sum()
    }
}

#[derive(Clone, Copy)]
enum CadenceDatum {
    Solid,
    Stem {
        pattern: Pattern,
        datum_world: f64,
        reverse: bool,
        length_world: f64,
    },
    Chord {
        pattern: Pattern,
        endpoint_datums_world: [f64; 2],
        length_world: f64,
    },
}

impl CadenceDatum {
    fn moment(self, arc_start: f64, arc_end: f64) -> Option<(usize, CadenceMoment)> {
        let mass = arc_end - arc_start;
        if mass <= f64::EPSILON {
            return None;
        }
        let (pattern, first) = match self {
            Self::Solid => return None,
            Self::Stem {
                pattern,
                datum_world,
                reverse,
                length_world,
            } => {
                let arc_mean = (arc_start + arc_end) * 0.5;
                let datum_mean = if reverse {
                    datum_world + length_world - arc_mean
                } else {
                    datum_world + arc_mean
                };
                (pattern, mass * datum_mean)
            }
            Self::Chord {
                pattern,
                endpoint_datums_world,
                length_world,
            } => (
                pattern,
                chord_datum_integral(endpoint_datums_world, length_world, arc_start, arc_end),
            ),
        };
        Some((
            pattern_slot(pattern),
            CadenceMoment {
                mass_world: mass,
                first_world: first,
            },
        ))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CadenceMoment {
    mass_world: f64,
    first_world: f64,
}

impl CadenceMoment {
    fn mean(self) -> Option<f64> {
        (self.mass_world > f64::EPSILON).then(|| self.first_world / self.mass_world)
    }
}

impl std::ops::AddAssign for CadenceMoment {
    fn add_assign(&mut self, rhs: Self) {
        self.mass_world += rhs.mass_world;
        self.first_world += rhs.first_world;
    }
}

fn chord_datum_integral(
    endpoint_datums_world: [f64; 2],
    length_world: f64,
    arc_start: f64,
    arc_end: f64,
) -> f64 {
    let midpoint = length_world * 0.5;
    let primitive = |arc: f64| {
        if arc <= midpoint {
            endpoint_datums_world[0].mul_add(arc, arc * arc * 0.5)
        } else {
            let first_half = endpoint_datums_world[0].mul_add(midpoint, midpoint * midpoint * 0.5);
            let tail_area =
                (endpoint_datums_world[1] + length_world).mul_add(arc - midpoint, first_half);
            let square_delta = arc.mul_add(arc, -(midpoint * midpoint));
            (-0.5_f64).mul_add(square_delta, tail_area)
        }
    };
    primitive(arc_end) - primitive(arc_start)
}

const fn pattern_slot(pattern: Pattern) -> usize {
    match pattern {
        Pattern::Dash { .. } => 0,
        Pattern::DashDot { .. } => 1,
        Pattern::Dots { .. } => 2,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PhaseGauge {
    prior: Option<PhaseFrame>,
    offset_points: f64,
}

#[derive(Clone, Copy, Debug)]
struct PhaseFrame {
    world_points: f64,
    moment: CadenceMoment,
}

#[derive(Debug, Default)]
struct PhaseTransport {
    gauges: [PhaseGauge; PATTERN_COUNT],
}

impl PhaseTransport {
    fn advance(
        &mut self,
        world_points: f64,
        moments: [CadenceMoment; PATTERN_COUNT],
    ) -> [f64; PATTERN_COUNT] {
        let periods = context_patterns().map(|pattern| f64::from(pattern.period()));
        for ((gauge, moment), period) in self.gauges.iter_mut().zip(moments).zip(periods) {
            if moment.mean().is_none() {
                gauge.prior = None;
                continue;
            }
            if let Some(prior) = gauge.prior {
                let mut bridge = prior.moment;
                bridge += moment;
                let anchor_world = bridge
                    .mean()
                    .expect("two positive cadence moments own a mean");
                // Zoom changes phase at topological datum d by Δscale·d. The
                // symmetric old/new arc centroid gives the reversible L² gauge.
                gauge.offset_points = (prior.world_points - world_points)
                    .mul_add(anchor_world, gauge.offset_points)
                    .rem_euclid(period);
            }
            gauge.prior = Some(PhaseFrame {
                world_points,
                moment,
            });
        }
        self.gauges.map(|gauge| gauge.offset_points)
    }
}

fn context_patterns() -> [Pattern; PATTERN_COUNT] {
    let width = TrailSalience::Context.width();
    let core_width = trail_core(width).width;
    [TrailMark::Dashed, TrailMark::DashDot, TrailMark::Unmarked].map(|mark| {
        trail_pattern(mark, width, core_width).expect("patterned trail marks own a cadence")
    })
}

fn cadence_laws(edges: &[WorldEdge]) -> (Vec<u32>, Vec<CadenceDatum>) {
    let mut laws = vec![CadenceDatum::Solid];
    let law_ids = edges
        .iter()
        .map(|edge| {
            let Some(pattern) = trail_pattern(
                edge.mark,
                TrailSalience::Context.width(),
                trail_core(TrailSalience::Context.width()).width,
            ) else {
                return 0;
            };
            let law = match edge
                .lineage
                .expect("patterned trail edge owns a cadence lineage")
            {
                CadenceLineage::Stem {
                    datum_world,
                    reverse,
                } => CadenceDatum::Stem {
                    pattern,
                    datum_world,
                    reverse,
                    length_world: edge.length_world,
                },
                CadenceLineage::Chord {
                    endpoint_datums_world,
                } => CadenceDatum::Chord {
                    pattern,
                    endpoint_datums_world,
                    length_world: edge.length_world,
                },
            };
            laws.push(law);
            u32::try_from(laws.len() - 1).expect("cadence law count fits u32")
        })
        .collect();
    (law_ids, laws)
}

fn visible_keys(
    frame: MapFramePlan,
    tiles: &HashMap<TileKey, Arc<TrailTile>>,
    tile_zoom: u8,
) -> Vec<TileKey> {
    let bounds = frame.world_bounds();
    let scale = f64::from(1_u32 << tile_zoom);
    let left = (bounds[0] * scale).floor() as i64;
    let right = (bounds[2] * scale).floor() as i64;
    let top = (bounds[1] * scale).floor().max(0.0) as i64;
    let bottom = (bounds[3] * scale).floor().min(scale - 1.0).max(0.0) as i64;
    let width = right.saturating_sub(left).saturating_add(1);
    let height = bottom.saturating_sub(top).saturating_add(1);
    let cells = usize::try_from(width.saturating_mul(height)).unwrap_or(usize::MAX);
    let mut visible = if cells <= tiles.len().saturating_mul(2).max(64) {
        let mut visible = Vec::new();
        for raw_y in top..=bottom {
            for raw_x in left..=right {
                let key = TileKey {
                    zoom: tile_zoom,
                    x: raw_x.rem_euclid(scale as i64) as u32,
                    y: raw_y as u32,
                };
                if tiles.contains_key(&key) {
                    visible.push(key);
                }
            }
        }
        visible
    } else {
        tiles
            .keys()
            .copied()
            .filter(|key| key.zoom == tile_zoom && tile_intersects(bounds, *key))
            .collect()
    };
    visible.sort_unstable();
    visible.dedup();
    visible
}

fn tile_intersects(bounds: [f64; 4], key: TileKey) -> bool {
    let scale = f64::from(1_u32 << key.zoom);
    let west = f64::from(key.x) / scale;
    let east = f64::from(key.x + 1) / scale;
    let north = f64::from(key.y) / scale;
    let south = f64::from(key.y + 1) / scale;
    [-1.0, 0.0, 1.0].into_iter().any(|wrap| {
        let west = west + wrap;
        let east = east + wrap;
        west <= bounds[2] && east >= bounds[0] && north <= bounds[3] && south >= bounds[1]
    })
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct TrailCorpus(u64);

impl TrailCorpus {
    fn mint() -> Self {
        Self(NEXT_CORPUS.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Clone)]
struct TrailPaint {
    corpus: TrailCorpus,
    laws: Arc<[CadenceDatum]>,
    band: DetailBand,
    tiles: Arc<[Arc<TrailTile>]>,
    center_world: [f64; 2],
    world_points: f32,
    viewport_points: [f32; 2],
    view_zoom: f32,
    phase_offsets: [f64; PATTERN_COUNT],
}

impl CallbackTrait for TrailPaint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(gpu) = resources.get_mut::<TrailMapGpu>() {
            gpu.prepare(device, queue, self);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let Some(gpu) = resources.get::<TrailMapGpu>() else {
            return;
        };
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, &gpu.camera_bind, &[]);
        for tile in self.tiles.iter() {
            let key = GpuKey {
                corpus: self.corpus,
                tile: tile.key,
                band: self.band,
            };
            if let Some(tile) = gpu.tiles.get(&key) {
                pass.set_bind_group(1, &tile.law_bind, &[]);
                tile.draw
                    .paint(pass, &tile.buffer, &tile.transform, 0..gpu.instances);
                let core = MAX_WRAP_INSTANCES as u32;
                tile.draw.paint(
                    pass,
                    &tile.buffer,
                    &tile.transform,
                    core..core + gpu.instances,
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct GpuKey {
    corpus: TrailCorpus,
    tile: TileKey,
    band: DetailBand,
}

struct GpuTrailTile {
    draw: Draw,
    buffer: wgpu::Buffer,
    transform: Range<u64>,
    law_ids: Arc<[u32]>,
    laws: Arc<[CadenceDatum]>,
    law_buffer: wgpu::Buffer,
    law_bind: wgpu::BindGroup,
    law_world_points: f32,
    law_phase_offsets: [f64; PATTERN_COUNT],
    bytes: usize,
    touched: u64,
}

impl GpuTrailTile {
    fn raise(
        device: &wgpu::Device,
        law_layout: &wgpu::BindGroupLayout,
        tile: &TrailTile,
        paint: &TrailPaint,
        touched: u64,
    ) -> Option<Self> {
        let mesh = &tile.bands[paint.band.index()];
        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            return None;
        }
        let mut blade = Vec::with_capacity(
            mesh.vertices
                .len()
                .saturating_mul(size_of::<TrailPoint>())
                .saturating_add(mesh.indices.len().saturating_mul(size_of::<u32>()))
                .saturating_add(size_of::<TileInstance>() * MAX_LAYER_INSTANCES),
        );
        let draw = Draw::pack(&mut blade, &mesh.vertices, &mesh.indices)?;
        let transforms: [TileInstance; MAX_LAYER_INSTANCES] = std::array::from_fn(|slot| {
            TileInstance::forge(
                tile.key,
                (slot % MAX_WRAP_INSTANCES) as u32,
                (slot / MAX_WRAP_INSTANCES) as u32,
            )
        });
        let transform = append(&mut blade, &transforms);
        let bytes = blade.len();
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("trail-tile"),
            contents: &blade,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::INDEX,
        });
        let gpu_laws = mesh
            .law_ids
            .iter()
            .map(|law| {
                GpuLaw::forge(
                    paint.laws[*law as usize],
                    f64::from(paint.world_points),
                    paint.phase_offsets,
                )
            })
            .collect::<Vec<_>>();
        let law_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("trail-tile-cadence-laws"),
            contents: bytemuck::cast_slice(&gpu_laws),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let law_bind = law_bind(device, law_layout, &law_buffer);
        let bytes = bytes.saturating_add(gpu_laws.len().saturating_mul(size_of::<GpuLaw>()));
        Some(Self {
            draw,
            buffer,
            transform,
            law_ids: Arc::clone(&mesh.law_ids),
            laws: Arc::clone(&paint.laws),
            law_buffer,
            law_bind,
            law_world_points: paint.world_points,
            law_phase_offsets: paint.phase_offsets,
            bytes,
            touched,
        })
    }

    fn refresh_laws(
        &mut self,
        queue: &wgpu::Queue,
        world_points: f32,
        phase_offsets: [f64; PATTERN_COUNT],
        scratch: &mut Vec<GpuLaw>,
    ) -> usize {
        if self.law_world_points.to_bits() == world_points.to_bits()
            && self
                .law_phase_offsets
                .iter()
                .zip(phase_offsets)
                .all(|(prior, next)| prior.to_bits() == next.to_bits())
        {
            return 0;
        }
        scratch.clear();
        scratch.extend(self.law_ids.iter().map(|law| {
            GpuLaw::forge(
                self.laws[*law as usize],
                f64::from(world_points),
                phase_offsets,
            )
        }));
        let bytes = scratch.len().saturating_mul(size_of::<GpuLaw>());
        queue.write_buffer(&self.law_buffer, 0, bytemuck::cast_slice(scratch));
        self.law_world_points = world_points;
        self.law_phase_offsets = phase_offsets;
        bytes
    }
}

struct Draw {
    vertices: Range<u64>,
    indices: Range<u64>,
    index_count: u32,
}

impl Draw {
    fn pack<V: Pod>(blade: &mut Vec<u8>, vertices: &[V], indices: &[u32]) -> Option<Self> {
        let index_count = u32::try_from(indices.len()).ok()?;
        Some(Self {
            vertices: append(blade, vertices),
            indices: append(blade, indices),
            index_count,
        })
    }

    fn paint(
        &self,
        pass: &mut wgpu::RenderPass<'static>,
        buffer: &wgpu::Buffer,
        transform: &Range<u64>,
        instances: Range<u32>,
    ) {
        pass.set_vertex_buffer(0, buffer.slice(self.vertices.clone()));
        pass.set_vertex_buffer(1, buffer.slice(transform.clone()));
        pass.set_index_buffer(
            buffer.slice(self.indices.clone()),
            wgpu::IndexFormat::Uint32,
        );
        pass.draw_indexed(0..self.index_count, 0, instances);
    }
}

fn append<T: Pod>(blade: &mut Vec<u8>, values: &[T]) -> Range<u64> {
    let start = blade.len() as u64;
    blade.extend_from_slice(bytemuck::cast_slice(values));
    start..blade.len() as u64
}

pub struct TrailMapGpu {
    pipeline: wgpu::RenderPipeline,
    law_layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    camera_bind: wgpu::BindGroup,
    tiles: HashMap<GpuKey, GpuTrailTile>,
    active: HashSet<GpuKey>,
    order: VecDeque<(GpuKey, u64)>,
    epoch: u64,
    bytes: usize,
    instances: u32,
    law_scratch: Vec<GpuLaw>,
    profile: bool,
}

impl TrailMapGpu {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("trail-map-uniform"),
            size: size_of::<Uniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("trail-map-camera"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(size_of::<Uniform>() as u64),
                },
                count: None,
            }],
        });
        let law_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("trail-map-cadence"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(size_of::<GpuLaw>() as u64),
                },
                count: None,
            }],
        });
        let camera_bind = camera_bind(device, &camera_layout, &uniform);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("trail-map"),
            bind_group_layouts: &[Some(&camera_layout), Some(&law_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("trail-map"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });
        let buffers = [trail_layout(), tile_layout()];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("trail-map"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("trail_vertex"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &buffers,
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(if format.is_srgb() {
                    "fragment_linear"
                } else {
                    "fragment_gamma"
                }),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            law_layout,
            uniform,
            camera_bind,
            tiles: HashMap::new(),
            active: HashSet::new(),
            order: VecDeque::new(),
            epoch: 0,
            bytes: 0,
            instances: 1,
            law_scratch: Vec::new(),
            profile: std::env::var_os("TRAILGEN_PROFILE_TRAILS").is_some(),
        }
    }

    fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, paint: &TrailPaint) {
        let begun = Instant::now();
        let active = paint
            .tiles
            .iter()
            .map(|tile| GpuKey {
                corpus: paint.corpus,
                tile: tile.key,
                band: paint.band,
            })
            .collect::<HashSet<_>>();
        let active_changed = self.active != active;
        if active_changed {
            self.epoch = self.epoch.saturating_add(1);
        }
        self.active = active;
        let mut uploaded = 0;
        let mut cadence_uploaded = 0;
        for tile in paint.tiles.iter() {
            let key = GpuKey {
                corpus: paint.corpus,
                tile: tile.key,
                band: paint.band,
            };
            if let Some(resident) = self.tiles.get_mut(&key) {
                cadence_uploaded += resident.refresh_laws(
                    queue,
                    paint.world_points,
                    paint.phase_offsets,
                    &mut self.law_scratch,
                );
                if active_changed {
                    resident.touched = self.epoch;
                    self.order.push_back((key, self.epoch));
                }
                continue;
            }
            let Some(resident) =
                GpuTrailTile::raise(device, &self.law_layout, tile, paint, self.epoch)
            else {
                continue;
            };
            uploaded += resident.bytes;
            self.bytes = self.bytes.saturating_add(resident.bytes);
            self.order.push_back((key, self.epoch));
            self.tiles.insert(key, resident);
        }
        let uniform = Uniform::forge(paint);
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&uniform));
        self.instances = uniform.wrap_radius.saturating_mul(2).saturating_add(1);
        self.reap();
        if self.profile {
            eprintln!(
                "trail-gpu prepare_us={} upload_bytes={uploaded} active_tiles={} band={}",
                begun.elapsed().as_micros(),
                paint.tiles.len(),
                paint.band.zoom(),
            );
            if cadence_uploaded != 0 {
                eprintln!("trail-gpu cadence_upload_bytes={cadence_uploaded}");
            }
        }
    }

    fn reap(&mut self) {
        while self.bytes > GPU_CEILING {
            let Some((key, epoch)) = self.order.pop_front() else {
                break;
            };
            let Some(resident) = self.tiles.get(&key) else {
                continue;
            };
            if resident.touched != epoch || self.active.contains(&key) {
                continue;
            }
            let resident = self
                .tiles
                .remove(&key)
                .expect("resident trail tile survived candidate inspection");
            self.bytes = self.bytes.saturating_sub(resident.bytes);
        }
    }
}

fn camera_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("trail-map-camera"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform.as_entire_binding(),
        }],
    })
}

fn law_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    laws: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("trail-map-cadence"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: laws.as_entire_binding(),
        }],
    })
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuLaw {
    metrics: [f32; 4],
}
const _: () = assert!(size_of::<GpuLaw>() == 16);

impl GpuLaw {
    fn forge(law: CadenceDatum, world_points: f64, phase_offsets: [f64; PATTERN_COUNT]) -> Self {
        match law {
            CadenceDatum::Solid => Self::zeroed(),
            CadenceDatum::Stem {
                pattern,
                datum_world,
                reverse,
                length_world,
            } => Self {
                metrics: [
                    phase(
                        datum_world,
                        world_points,
                        pattern.period(),
                        phase_offsets[pattern_slot(pattern)],
                    ),
                    0.0,
                    if reverse { -2.0 } else { -1.0 },
                    (length_world * world_points) as f32,
                ],
            },
            CadenceDatum::Chord {
                pattern,
                endpoint_datums_world,
                length_world,
            } => {
                let length = (length_world * world_points) as f32;
                let offset = phase_offsets[pattern_slot(pattern)];
                let a = phase(
                    endpoint_datums_world[0],
                    world_points,
                    pattern.period(),
                    offset,
                );
                let b = phase(
                    endpoint_datums_world[1],
                    world_points,
                    pattern.period(),
                    offset,
                );
                Self {
                    metrics: [a, b, pattern.splice(a, b, length), length],
                }
            }
        }
    }
}

fn phase(datum_world: f64, world_points: f64, period: f32, offset_points: f64) -> f32 {
    datum_world
        .mul_add(world_points, offset_points)
        .rem_euclid(f64::from(period)) as f32
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniform {
    center_high: [f32; 2],
    center_low: [f32; 2],
    viewport: [f32; 2],
    world_points: f32,
    wrap_radius: u32,
    view_zoom: f32,
    _pad: f32,
    radii: [f32; 2],
    disclosure: [f32; 4],
}

impl Uniform {
    fn forge(paint: &TrailPaint) -> Self {
        let [x_high, x_low] = split(paint.center_world[0]);
        let [y_high, y_low] = split(paint.center_world[1]);
        let wrap_radius = wrap_radius(
            paint.viewport_points[0] / paint.world_points,
            paint.center_world[0] as f32,
        );
        Self {
            center_high: [x_high, y_high],
            center_low: [x_low, y_low],
            viewport: paint.viewport_points,
            world_points: paint.world_points,
            wrap_radius,
            view_zoom: paint.view_zoom,
            _pad: 0.0,
            radii: [
                TrailSalience::Context.width() * 0.5,
                trail_core(TrailSalience::Context.width()).width * 0.5,
            ],
            disclosure: [
                TUBE_ONSET_ZOOM,
                CORE_ONSET_ZOOM,
                PATTERN_ONSET_ZOOM,
                DISCLOSURE_SPAN_ZOOM,
            ],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TileInstance {
    origin_high: [f32; 2],
    origin_low: [f32; 2],
    span: f32,
    wrap: u32,
    layer: u32,
    _pad: f32,
}

impl TileInstance {
    fn forge(key: TileKey, wrap: u32, layer: u32) -> Self {
        let divisions = f64::from(1_u32 << key.zoom);
        let [x_high, x_low] = split(f64::from(key.x) / divisions);
        let [y_high, y_low] = split(f64::from(key.y) / divisions);
        Self {
            origin_high: [x_high, y_high],
            origin_low: [x_low, y_low],
            span: (1.0 / divisions) as f32,
            wrap,
            layer,
            _pad: 0.0,
        }
    }
}

fn split(value: f64) -> [f32; 2] {
    let high = value as f32;
    [high, (value - f64::from(high)) as f32]
}

fn wrap_radius(world_width: f32, center_x: f32) -> u32 {
    let half = world_width * 0.5;
    let west = (half - center_x).max(0.0);
    let east = (center_x + half - 1.0).max(0.0);
    (west.max(east).ceil() as u32).min(MAX_WRAP_RADIUS)
}

const fn trail_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Unorm8x4,
        3 => Float32,
        4 => Uint32
    ];
    wgpu::VertexBufferLayout {
        array_stride: size_of::<TrailPoint>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

const fn tile_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
        7 => Float32x2,
        8 => Float32x2,
        9 => Float32,
        10 => Uint32,
        11 => Uint32
    ];
    wgpu::VertexBufferLayout {
        array_stride: size_of::<TileInstance>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRIBUTES,
    }
}

const WGSL: &str = r"
struct Uniform {
    center_high: vec2f,
    center_low: vec2f,
    viewport: vec2f,
    world_points: f32,
    wrap_radius: u32,
    view_zoom: f32,
    pad: f32,
    radii: vec2f,
    disclosure: vec4f,
};

struct CadenceLaw {
    metrics: vec4f,
};

@group(0) @binding(0) var<uniform> u: Uniform;
@group(1) @binding(0) var<storage, read> laws: array<CadenceLaw>;

struct VertexOut {
    @builtin(position) position: vec4f,
    @location(0) color: vec4f,
    @location(1) edge_distance: f32,
    @location(2) solid_radius: f32,
    @location(3) tile_local: vec2f,
    @location(4) arc_world: f32,
    @location(5) @interpolate(flat) law: u32,
    @location(6) @interpolate(flat) pattern: u32,
};

fn apparition(onset_zoom: f32) -> f32 {
    let phase = clamp(
        (u.view_zoom - onset_zoom) / u.disclosure.w,
        0.0,
        1.0,
    );
    return phase * phase * (3.0 - 2.0 * phase);
}

fn clip_at(
    local: vec2f,
    origin_high: vec2f,
    origin_low: vec2f,
    tile_span: f32,
    wrap: u32,
) -> vec2f {
    let origin_delta = (origin_high - u.center_high) + (origin_low - u.center_low);
    var delta = origin_delta + local * tile_span;
    delta.x -= round(origin_delta.x + tile_span * 0.5);
    delta.x += f32(wrap) - f32(u.wrap_radius);
    let points = delta * u.world_points;
    return vec2f(points.x * 2.0 / u.viewport.x, -points.y * 2.0 / u.viewport.y);
}

@vertex
fn trail_vertex(
    @location(0) local: vec2f,
    @location(1) extrusion: vec2f,
    @location(2) color: vec4f,
    @location(3) arc_world: f32,
    @location(4) cadence: u32,
    @location(7) origin_high: vec2f,
    @location(8) origin_low: vec2f,
    @location(9) tile_span: f32,
    @location(10) wrap: u32,
    @location(11) layer: u32,
) -> VertexOut {
    var out: VertexOut;
    let side = select(-1.0, 1.0, (cadence & 1u) != 0u);
    let pattern = (cadence >> 1u) & 3u;
    let law = cadence >> 3u;
    let core = layer == 1u;
    let core_onset = select(u.disclosure.y, u.disclosure.z, pattern != 0u);
    let onset_zoom = select(u.disclosure.x, core_onset, core);
    let maturity = apparition(onset_zoom);
    let radius = select(u.radii.x, u.radii.y, core);
    let visible_radius = radius * mix(0.12, 1.0, maturity);
    let expanded_radius = visible_radius + 0.8;
    let offset = extrusion * expanded_radius * 2.0 / u.viewport;
    let clip = clip_at(local, origin_high, origin_low, tile_span, wrap)
        + vec2f(offset.x, -offset.y);
    out.position = vec4f(clip, 0.0, 1.0);
    let ink = select(color, vec4f(20.0 / 255.0, 19.0 / 255.0, 17.0 / 255.0, 1.0), core);
    out.color = vec4f(ink.rgb, ink.a * maturity);
    out.edge_distance = side * expanded_radius;
    out.solid_radius = visible_radius;
    out.tile_local = local + extrusion * expanded_radius / (u.world_points * tile_span);
    out.arc_world = arc_world;
    out.law = select(0u, law, core);
    out.pattern = select(0u, pattern, core);
    return out;
}

fn cadence_distance(in: VertexOut) -> f32 {
    let law = laws[in.law];
    let arc = in.arc_world * u.world_points;
    if law.metrics.z == -2.0 {
        return law.metrics.x + law.metrics.w - arc;
    }
    if law.metrics.z >= 0.0 && arc > law.metrics.z {
        return law.metrics.y + law.metrics.w - arc;
    }
    return law.metrics.x + arc;
}

fn interval_ink(value: f32, start: f32, end: f32) -> f32 {
    let feather = max(fwidth(value), 0.55);
    return smoothstep(start - feather, start + feather, value)
        * (1.0 - smoothstep(end - feather, end + feather, value));
}

fn cadence_ink(in: VertexOut) -> f32 {
    if in.pattern == 0u {
        return 1.0;
    }
    let distance = cadence_distance(in);
    if in.pattern == 1u {
        let phase = distance - floor(distance / 9.982) * 9.982;
        return 1.0 - smoothstep(5.66, 6.76, phase);
    }
    if in.pattern == 2u {
        let phase = distance - floor(distance / 13.0824) * 13.0824;
        return max(
            1.0 - smoothstep(5.66, 6.76, phase),
            interval_ink(phase, 9.522, 9.7704),
        );
    }
    let phase = distance - floor(distance / 9.43) * 9.43;
    let axial = min(phase, 9.43 - phase);
    let radius = 0.6624;
    let distance_to_center = length(vec2f(axial, in.edge_distance));
    let feather = max(fwidth(distance_to_center), 0.55);
    return 1.0 - smoothstep(radius - feather, radius + feather, distance_to_center);
}

fn painted(in: VertexOut) -> vec4f {
    if any(in.tile_local < vec2f(0.0)) || any(in.tile_local >= vec2f(1.0)) {
        discard;
    }
    let feather = max(fwidth(in.edge_distance), 0.65);
    let edge = clamp(
        (in.solid_radius + feather * 0.5 - abs(in.edge_distance)) / feather,
        0.0,
        1.0,
    );
    let alpha = in.color.a * edge * cadence_ink(in);
    if alpha <= 0.001 {
        discard;
    }
    return vec4f(in.color.rgb, alpha);
}

@fragment
fn fragment_gamma(in: VertexOut) -> @location(0) vec4f {
    return painted(in);
}

fn linear_channel(encoded: f32) -> f32 {
    if encoded <= 0.04045 { return encoded / 12.92; }
    return pow((encoded + 0.055) / 1.055, 2.4);
}

@fragment
fn fragment_linear(in: VertexOut) -> @location(0) vec4f {
    let color = painted(in);
    return vec4f(
        linear_channel(color.r),
        linear_channel(color.g),
        linear_channel(color.b),
        color.a,
    );
}
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::EARTH_CIRCUMFERENCE_M;
    use trailgen_core::TrailClass;

    #[test]
    fn detail_admission_is_monotone_and_exact_at_fourteen() {
        let onset = f64::from(TUBE_ONSET_ZOOM);
        let bands = [onset + 0.01, 9.9, 10.0, 11.0, 12.0, 13.0, 14.0, 23.0]
            .into_iter()
            .filter_map(DetailBand::for_zoom)
            .collect::<Vec<_>>();
        assert!(DetailBand::for_zoom(onset).is_none());
        assert_eq!(bands.first().copied(), Some(DetailBand(0)));
        assert!(bands.windows(2).all(|pair| pair[0].0 <= pair[1].0));
        assert_eq!(
            bands.last().copied(),
            Some(DetailBand(LAST_BAND - FIRST_BAND))
        );
        assert!(DetailBand(LAST_BAND - FIRST_BAND).tolerance_world() <= f64::EPSILON);
    }

    #[test]
    fn simplification_error_never_exceeds_its_screen_budget() {
        for index in 0..BAND_COUNT - 1 {
            let band = DetailBand::from_index(index);
            let upper_world_points = 256.0 * f64::from(band.zoom() + 1.0).exp2();
            assert!(
                band.tolerance_world() * upper_world_points
                    <= SIMPLIFICATION_ERROR_POINTS + f64::EPSILON
            );
        }
    }

    #[test]
    fn disclosure_schedule_is_fixed_to_harriman_cartographic_scale() {
        const HARRIMAN_LATITUDE_DEG: f64 = 41.25;
        const HARRIMAN_FILAMENT_P95_M: f64 = 182.9;
        let apparent = |meters: f64, zoom: f32| {
            let parallel = EARTH_CIRCUMFERENCE_M * HARRIMAN_LATITUDE_DEG.to_radians().cos();
            meters * 256.0 * f64::from(zoom).exp2() / parallel
        };
        let longest_period = [TrailMark::Dashed, TrailMark::DashDot, TrailMark::Unmarked]
            .into_iter()
            .filter_map(|mark| {
                trail_pattern(
                    mark,
                    TrailSalience::Context.width(),
                    trail_core(TrailSalience::Context.width()).width,
                )
            })
            .map(Pattern::period)
            .max_by(f32::total_cmp)
            .expect("patterned trail marks are nonempty");

        assert!((apparent(HARRIMAN_FILAMENT_P95_M, TUBE_ONSET_ZOOM) - 1.0).abs() < 0.01);
        assert!((apparent(HARRIMAN_FILAMENT_P95_M, CORE_ONSET_ZOOM) - 1.36).abs() < 0.02);
        assert!(
            f64::from(longest_period)
                .mul_add(
                    -1.25,
                    apparent(1_000.0, PATTERN_ONSET_ZOOM + DISCLOSURE_SPAN_ZOOM),
                )
                .abs()
                < 0.05
        );
    }

    #[test]
    fn deep_detail_owns_deep_spatial_tiles() {
        assert_eq!(DetailBand::for_zoom(11.9).unwrap().spatial_zoom(), 12);
        assert_eq!(DetailBand::for_zoom(13.2).unwrap().spatial_zoom(), 13);
        assert_eq!(DetailBand::for_zoom(23.0).unwrap().spatial_zoom(), 14);
    }

    #[test]
    fn simplification_is_a_nested_subsequence() {
        let points = [[0.0, 0.0], [1.0, 0.1], [2.0, 0.0], [3.0, 0.5], [4.0, 0.0]]
            .into_iter()
            .enumerate()
            .map(|(slot, world)| Sample {
                world,
                arc_world: slot as f64,
            })
            .collect::<Vec<_>>();
        let coarse = simplify(&points, 0.2);
        let fine = simplify(&points, 0.05);
        assert!(coarse.iter().all(|point| {
            fine.iter()
                .any(|candidate| same_world(candidate.world, point.world))
        }));
    }

    #[test]
    fn tile_cleaving_preserves_arc_and_coverage() {
        let scale = f64::from(1_u32 << BASE_TILE_ZOOM);
        let points = [
            Sample {
                world: [1200.75 / scale, 1532.5 / scale],
                arc_world: 0.0,
            },
            Sample {
                world: [1202.25 / scale, 1532.5 / scale],
                arc_world: 1.5 / scale,
            },
        ];
        let fragments = cleave(&points, BASE_TILE_ZOOM);
        assert_eq!(fragments.len(), 3);
        assert_eq!(fragments[0].key.x, 1200);
        assert_eq!(fragments[2].key.x, 1202);
        assert_eq!(
            fragments
                .last()
                .and_then(|fragment| fragment.points.last())
                .map(|point| point.arc_world),
            Some(points[1].arc_world)
        );
    }

    #[test]
    fn cadence_phase_is_evaluated_in_double_precision() {
        let pattern = Pattern::Dash {
            dash: 6.21,
            gap: 3.772,
        };
        let datum = 0.031_415_926_535_897_934;
        let world_points = 256.0 * 23.5_f64.exp2();
        let expected = (datum * world_points).rem_euclid(f64::from(pattern.period())) as f32;
        assert!(
            (phase(datum, world_points, pattern.period(), 0.0) - expected).abs() <= f32::EPSILON
        );
    }

    #[test]
    fn cadence_moments_follow_stems_and_both_sides_of_chords() {
        let pattern = context_patterns()[0];
        let mean = |law: CadenceDatum, start, end| {
            law.moment(start, end)
                .expect("patterned arc owns a moment")
                .1
                .mean()
                .expect("positive arc owns a mean")
        };
        let forward = CadenceDatum::Stem {
            pattern,
            datum_world: 0.2,
            reverse: false,
            length_world: 0.1,
        };
        let reverse = CadenceDatum::Stem {
            pattern,
            datum_world: 0.2,
            reverse: true,
            length_world: 0.1,
        };
        let chord = CadenceDatum::Chord {
            pattern,
            endpoint_datums_world: [0.2, 0.5],
            length_world: 0.1,
        };

        assert!((mean(forward, 0.02, 0.06) - 0.24).abs() < f64::EPSILON);
        assert!((mean(reverse, 0.02, 0.06) - 0.26).abs() < f64::EPSILON);
        assert!((mean(chord, 0.0, 0.1) - 0.375).abs() < 8.0 * f64::EPSILON);
    }

    #[test]
    fn phase_transport_is_the_visible_arc_least_squares_gauge() {
        let moment = CadenceMoment {
            mass_world: 3.0,
            first_world: 0.375,
        };
        let moments = [moment, CadenceMoment::default(), CadenceMoment::default()];
        let mut transport = PhaseTransport::default();
        let initial = transport.advance(100.0, moments)[0];
        let transported = transport.advance(110.0, moments)[0];
        let period = f64::from(context_patterns()[0].period());
        let centered = period
            .mul_add(0.5, transported - initial)
            .rem_euclid(period);
        let shift = period.mul_add(-0.5, centered);
        let cost = |candidate: f64| {
            [0.1_f64, 0.125, 0.15]
                .into_iter()
                .map(|datum| 10.0_f64.mul_add(datum, candidate).powi(2))
                .sum::<f64>()
        };

        assert!((shift + 1.25).abs() < 8.0 * f64::EPSILON);
        assert!(cost(shift) < cost(shift - 0.25));
        assert!(cost(shift) < cost(shift + 0.25));
    }

    #[test]
    fn phase_transport_is_reversible_across_a_zoom_step() {
        let moment = |mean| CadenceMoment {
            mass_world: 1.0,
            first_world: mean,
        };
        let visible = |mean| {
            [
                moment(mean),
                CadenceMoment::default(),
                CadenceMoment::default(),
            ]
        };
        let mut transport = PhaseTransport::default();
        let initial = transport.advance(100.0, visible(0.1));
        let _nearer = transport.advance(110.0, visible(0.2));
        let returned = transport.advance(100.0, visible(0.1));
        let period = f64::from(context_patterns()[0].period());
        let drift = (returned[0] - initial[0]).rem_euclid(period);

        assert!(drift.min(period - drift) < 8.0 * f64::EPSILON);
    }

    #[test]
    fn panning_cannot_move_the_transported_cadence() {
        let moment = |mean| CadenceMoment {
            mass_world: 1.0,
            first_world: mean,
        };
        let mut transport = PhaseTransport::default();
        let before = transport.advance(
            100.0,
            [
                moment(0.1),
                CadenceMoment::default(),
                CadenceMoment::default(),
            ],
        );
        let after = transport.advance(
            100.0,
            [
                moment(0.9),
                CadenceMoment::default(),
                CadenceMoment::default(),
            ],
        );

        assert_eq!(before.map(f64::to_bits), after.map(f64::to_bits));
    }

    #[test]
    fn tube_and_core_share_one_centerline_mesh() {
        let key = TileKey {
            zoom: BASE_TILE_ZOOM,
            x: 1_200,
            y: 1_532,
        };
        let scale = f64::from(1_u32 << BASE_TILE_ZOOM);
        let samples = [
            Sample {
                world: [1_200.2 / scale, 1_532.3 / scale],
                arc_world: 0.0,
            },
            Sample {
                world: [1_200.5 / scale, 1_532.5 / scale],
                arc_world: 0.4 / scale,
            },
            Sample {
                world: [1_200.8 / scale, 1_532.7 / scale],
                arc_world: 0.8 / scale,
            },
        ];
        let edge = WorldEdge {
            endpoints: [0, 1],
            points: samples.iter().map(|sample| sample.world).collect(),
            length_world: samples[2].arc_world,
            lineage: None,
            trail_class: trailgen_core::TrailClass::Path,
            mark: TrailMark::Solid,
            access: trailgen_core::Access::Open,
        };
        let mut builder = TrailMeshBuilder::default();
        builder.push(&samples, key, &edge, 0, CadenceDatum::Solid);
        let mesh = builder.seal();

        assert_eq!(mesh.vertices.len(), samples.len() * 2);
        assert_eq!(mesh.indices.len(), (samples.len() - 1) * 6);
    }

    #[test]
    fn every_trail_class_survives_into_the_coarsest_band() {
        let scale = f64::from(1_u32 << BASE_TILE_ZOOM);
        let edge = |row: f64, class| {
            let points = vec![
                [1_200.2 / scale, (1_532.2 + row) / scale],
                [1_200.8 / scale, (1_532.2 + row) / scale],
            ];
            WorldEdge {
                endpoints: [0, 1],
                length_world: 0.6 / scale,
                points,
                lineage: None,
                trail_class: class,
                mark: TrailMark::Solid,
                access: trailgen_core::Access::Open,
            }
        };
        let classes = [
            TrailClass::Unknown,
            TrailClass::Path,
            TrailClass::Footway,
            TrailClass::Track,
            TrailClass::Service,
            TrailClass::Pedestrian,
            TrailClass::Steps,
            TrailClass::Bridleway,
            TrailClass::Bushwhack,
            TrailClass::Road,
        ];
        let edges = classes
            .into_iter()
            .enumerate()
            .map(|(slot, class)| edge(slot as f64 * 0.05, class))
            .collect::<Vec<_>>();
        let field = TrailField::forge(&edges);
        let first_band_indices = field
            .tiles
            .values()
            .map(|tile| tile.bands[0].indices.len())
            .sum::<usize>();

        assert_eq!(first_band_indices, classes.len() * 6);
    }
}
