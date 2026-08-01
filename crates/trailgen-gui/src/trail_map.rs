use crate::{
    basemap::{self, TileKey},
    cadence::WorldLevel,
    map::{
        CadenceLineage, MapFramePlan, TrailColoring, TrailMark, TrailSalience, WorldEdge,
        coloring_shader_code, formality_color, terrain_color, trail_core_width,
    },
};
use bytemuck::{Pod, Zeroable};
use egui::{Color32, Painter};
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor, wgpu};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use trailgen_core::{Access, Terrain, TrailStanding};
use wgpu::util::DeviceExt as _;

const BASE_TILE_ZOOM: u8 = 12;
const FIRST_BAND: u8 = 10;
const LAST_BAND: u8 = 14;
const BAND_COUNT: usize = (LAST_BAND - FIRST_BAND + 1) as usize;
const SIMPLIFICATION_ERROR_POINTS: f64 = 0.42;
const TUBE_ONSET_ZOOM: f32 = 10.80;
const CORE_ONSET_ZOOM: f32 = 11.05;
const PATTERN_ONSET_ZOOM: f32 = 11.48;
const DISCLOSURE_SPAN_ZOOM: f32 = 0.58;
const OVERLAY_ONSET_ZOOM: f32 = -100.0;
const URBAN_TUBE_ONSET_ZOOM: f32 = 14.35;
const URBAN_CORE_ONSET_ZOOM: f32 = 14.85;
const URBAN_PATTERN_ONSET_ZOOM: f32 = 15.35;
const PEDESTRIAN_DIAGNOSTIC_TUBE_ONSET_ZOOM: f32 = 17.70;
const PEDESTRIAN_DIAGNOSTIC_CORE_ONSET_ZOOM: f32 = 18.00;
const PEDESTRIAN_DIAGNOSTIC_PATTERN_ONSET_ZOOM: f32 = 18.35;
const SELECTED_MITER_LIMIT: f32 = std::f32::consts::SQRT_2;
const ROUND_CAP_STEPS: usize = 8;
const DETAIL_HYSTERESIS_ZOOM: f64 = 0.08;
const DETAIL_TRANSITION: std::time::Duration = std::time::Duration::from_millis(160);
const _: () = assert!(TUBE_ONSET_ZOOM >= FIRST_BAND as f32);
const _: () = assert!(TUBE_ONSET_ZOOM < CORE_ONSET_ZOOM);
const _: () = assert!(CORE_ONSET_ZOOM < PATTERN_ONSET_ZOOM);
const GPU_CEILING: usize = 256 * 1_048_576;
const GPU_UPLOAD_BUDGET: Duration = Duration::from_millis(3);
const GPU_UPLOAD_BYTES: usize = 8 * 1_048_576;
const MAX_WRAP_RADIUS: u32 = 2;
const MAX_WRAP_INSTANCES: usize = (MAX_WRAP_RADIUS * 2 + 1) as usize;
const LAYER_COUNT: usize = 2;
const MAX_LAYER_INSTANCES: usize = MAX_WRAP_INSTANCES * LAYER_COUNT;
static NEXT_CORPUS: AtomicU64 = AtomicU64::new(1);

pub struct TrailField {
    corpus: TrailCorpus,
    laws: Arc<[CadenceDatum]>,
    tiles: HashMap<TileKey, Arc<TrailTile>>,
    dialect: TrailDialect,
    visibility: Option<Visibility>,
    transition: Option<DetailTransition>,
    cadence: Option<WorldLevel>,
}

#[derive(Clone, Copy)]
struct TrailDialect {
    salience: TrailSalience,
    disclosure: [f32; 4],
    core: bool,
    hue: HueAuthority,
}

#[derive(Clone, Copy)]
enum HueAuthority {
    Projected,
    Intrinsic,
}

impl TrailDialect {
    const fn projected(salience: TrailSalience, disclosure: [f32; 4]) -> Self {
        Self {
            salience,
            disclosure,
            core: true,
            hue: HueAuthority::Projected,
        }
    }

    const fn monolith(disclosure: [f32; 4]) -> Self {
        Self {
            salience: TrailSalience::Context,
            disclosure,
            core: false,
            hue: HueAuthority::Intrinsic,
        }
    }

    const fn coloring(self, requested: TrailColoring) -> TrailColoring {
        match self.hue {
            HueAuthority::Projected => requested,
            HueAuthority::Intrinsic => TrailColoring::Class,
        }
    }
}

#[derive(Clone)]
struct Visibility {
    band: DetailBand,
    keys: Vec<TileKey>,
    tiles: Arc<[Arc<TrailTile>]>,
}

struct DetailTransition {
    prior: Visibility,
    begun: Instant,
}

impl TrailField {
    pub fn forge(edges: &[WorldEdge]) -> Self {
        Self::forge_as(
            edges,
            TrailDialect::projected(
                TrailSalience::Context,
                [
                    TUBE_ONSET_ZOOM,
                    CORE_ONSET_ZOOM,
                    PATTERN_ONSET_ZOOM,
                    DISCLOSURE_SPAN_ZOOM,
                ],
            ),
        )
    }

    pub fn overlay(edges: &[WorldEdge]) -> Self {
        Self::forge_as(
            edges,
            TrailDialect::projected(
                TrailSalience::Selected,
                [
                    OVERLAY_ONSET_ZOOM,
                    CORE_ONSET_ZOOM,
                    PATTERN_ONSET_ZOOM,
                    DISCLOSURE_SPAN_ZOOM,
                ],
            ),
        )
    }

    pub fn urban(edges: &[WorldEdge]) -> Self {
        Self::forge_as(
            edges,
            TrailDialect::projected(
                TrailSalience::Context,
                [
                    URBAN_TUBE_ONSET_ZOOM,
                    URBAN_CORE_ONSET_ZOOM,
                    URBAN_PATTERN_ONSET_ZOOM,
                    DISCLOSURE_SPAN_ZOOM,
                ],
            ),
        )
    }

    pub fn crossing_diagnostics(edges: &[WorldEdge]) -> Self {
        Self::forge_as(
            edges,
            TrailDialect::projected(
                TrailSalience::Context,
                [
                    PEDESTRIAN_DIAGNOSTIC_TUBE_ONSET_ZOOM,
                    PEDESTRIAN_DIAGNOSTIC_CORE_ONSET_ZOOM,
                    PEDESTRIAN_DIAGNOSTIC_PATTERN_ONSET_ZOOM,
                    DISCLOSURE_SPAN_ZOOM,
                ],
            ),
        )
    }

    pub fn sidewalks(edges: &[WorldEdge]) -> Self {
        Self::forge_as(
            edges,
            TrailDialect::monolith([
                PEDESTRIAN_DIAGNOSTIC_TUBE_ONSET_ZOOM,
                PEDESTRIAN_DIAGNOSTIC_TUBE_ONSET_ZOOM,
                PEDESTRIAN_DIAGNOSTIC_TUBE_ONSET_ZOOM,
                DISCLOSURE_SPAN_ZOOM,
            ]),
        )
    }

    fn forge_as(edges: &[WorldEdge], dialect: TrailDialect) -> Self {
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
                        dialect.salience,
                        dialect.salience == TrailSalience::Selected,
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
            dialect,
            visibility: None,
            transition: None,
            cadence: None,
        }
    }

    pub fn paint_colored(
        &mut self,
        painter: &Painter,
        frame: MapFramePlan,
        coloring: TrailColoring,
    ) {
        let Some(band) = DetailBand::resolve(
            self.visibility.as_ref().map(|visibility| visibility.band),
            frame.zoom.get(),
            self.dialect.disclosure[0],
        ) else {
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
            let next = Visibility { band, keys, tiles };
            if self
                .visibility
                .as_ref()
                .is_some_and(|visibility| visibility.band != band)
            {
                self.transition = self.visibility.take().map(|prior| DetailTransition {
                    prior,
                    begun: Instant::now(),
                });
            }
            self.visibility = Some(next);
        }
        let visibility = self
            .visibility
            .as_ref()
            .expect("trail visibility was just established");
        if visibility.tiles.is_empty() {
            return;
        }
        let tiles = Arc::clone(&visibility.tiles);
        let cadence = WorldLevel::resolve(self.cadence, frame.zoom.get(), DETAIL_HYSTERESIS_ZOOM);
        self.cadence = Some(cadence);
        let world_points = frame.world_points as f32;
        let now = Instant::now();
        let transition = self.transition.as_ref().map(|transition| {
            let elapsed = now.saturating_duration_since(transition.begun);
            (
                transition.prior.clone(),
                smooth_transition(elapsed.as_secs_f32() / DETAIL_TRANSITION.as_secs_f32()),
            )
        });
        if transition
            .as_ref()
            .is_some_and(|(_, maturity)| *maturity >= 1.0)
        {
            self.transition = None;
        }
        let mut layers = Vec::with_capacity(2);
        if let Some((prior, maturity)) = &transition
            && *maturity < 1.0
        {
            layers.push(TrailLayer {
                band: prior.band,
                tiles: Arc::clone(&prior.tiles),
                opacity: 1.0 - *maturity,
            });
            painter.ctx().request_repaint();
        }
        layers.push(TrailLayer {
            band,
            tiles,
            opacity: transition.map_or(1.0, |(_, maturity)| maturity.min(1.0)),
        });
        painter.add(egui_wgpu::Callback::new_paint_callback(
            frame.rect,
            TrailPaint {
                corpus: self.corpus,
                laws: Arc::clone(&self.laws),
                layers: layers.into(),
                repaint: painter.ctx().clone(),
                center_world: frame.viewport.center,
                world_points,
                viewport_points: [frame.rect.width(), frame.rect.height()],
                view_zoom: frame.zoom.get() as f32,
                cadence_cells_per_world: cadence.cells_per_world() as f32,
                dialect: self.dialect,
                coloring: self.dialect.coloring(coloring),
            },
        ));
    }
}

fn smooth_transition(phase: f32) -> f32 {
    let phase = phase.clamp(0.0, 1.0);
    phase * phase * 2.0_f32.mul_add(-phase, 3.0)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct DetailBand(u8);

impl DetailBand {
    fn resolve(prior: Option<Self>, zoom: f64, onset_zoom: f32) -> Option<Self> {
        let target = Self::for_zoom(zoom, onset_zoom)?;
        let Some(prior) = prior else {
            return Some(target);
        };
        let retain = (target.0 > prior.0
            && zoom < f64::from(FIRST_BAND + prior.0 + 1) + DETAIL_HYSTERESIS_ZOOM)
            || (target.0 < prior.0
                && zoom >= f64::from(FIRST_BAND + prior.0) - DETAIL_HYSTERESIS_ZOOM);
        if retain { Some(prior) } else { Some(target) }
    }

    fn for_zoom(zoom: f64, onset_zoom: f32) -> Option<Self> {
        (zoom > f64::from(onset_zoom)).then(|| {
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
}

impl TrailMeshBuilder {
    fn push(
        &mut self,
        samples: &[Sample],
        key: TileKey,
        edge: &WorldEdge,
        law_id: u32,
        salience: TrailSalience,
        round_caps: bool,
    ) {
        if samples.len() < 2 {
            return;
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
        let color = salience.access_color(edge.color, edge.access).to_array();
        let pattern = pattern_code(edge.mark);
        let informal = edge.standing == TrailStanding::Informal;
        let terrain = terrain_code(edge.terrain);
        let blocked = matches!(edge.access, Access::Closed | Access::Private);
        let base = u32::try_from(self.vertices.len()).expect("trail tile vertex count fits u32");
        for (slot, point) in local.iter().copied().enumerate() {
            let [negative, positive] = ribbon_extrusions(&local, slot, salience);
            self.vertices.extend([
                TrailPoint {
                    local: point,
                    extrusion: negative,
                    srgb: color,
                    arc_world: samples[slot].arc_world as f32,
                    cadence: cadence_word(law_id, pattern, false, informal, terrain, blocked),
                    edge_factor: -1.0,
                },
                TrailPoint {
                    local: point,
                    extrusion: positive,
                    srgb: color,
                    arc_world: samples[slot].arc_world as f32,
                    cadence: cadence_word(law_id, pattern, true, informal, terrain, blocked),
                    edge_factor: 1.0,
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
        if round_caps {
            self.push_round_cap(
                local[0],
                ribbon_direction(local[0], local[1], -1.0),
                color,
                samples[0].arc_world as f32,
                cadence_word(law_id, pattern, false, informal, terrain, blocked),
            );
            self.push_round_cap(
                local[local.len() - 1],
                ribbon_direction(local[local.len() - 2], local[local.len() - 1], 1.0),
                color,
                samples[samples.len() - 1].arc_world as f32,
                cadence_word(law_id, pattern, false, informal, terrain, blocked),
            );
        }
    }

    fn push_round_cap(
        &mut self,
        local: [f32; 2],
        outward: [f32; 2],
        srgb: [u8; 4],
        arc_world: f32,
        cadence: u32,
    ) {
        let base = u32::try_from(self.vertices.len()).expect("trail tile vertex count fits u32");
        self.vertices.push(TrailPoint {
            local,
            extrusion: [0.0, 0.0],
            srgb,
            arc_world,
            cadence,
            edge_factor: 0.0,
        });
        let normal = [-outward[1], outward[0]];
        for slot in 0..=ROUND_CAP_STEPS {
            let angle = -std::f32::consts::FRAC_PI_2
                + std::f32::consts::PI * slot as f32 / ROUND_CAP_STEPS as f32;
            let (sin, cos) = angle.sin_cos();
            self.vertices.push(TrailPoint {
                local,
                extrusion: [
                    outward[0].mul_add(cos, normal[0] * sin),
                    outward[1].mul_add(cos, normal[1] * sin),
                ],
                srgb,
                arc_world,
                cadence,
                edge_factor: 1.0,
            });
        }
        for slot in 0..ROUND_CAP_STEPS {
            let rim = base + 1 + u32::try_from(slot).expect("round-cap subdivision count fits u32");
            self.indices.extend([base, rim, rim + 1]);
        }
    }

    fn seal(self) -> TrailMesh {
        TrailMesh {
            vertices: self.vertices.into(),
            indices: self.indices.into(),
        }
    }
}

fn ribbon_extrusions(points: &[[f32; 2]], slot: usize, salience: TrailSalience) -> [[f32; 2]; 2] {
    let mut join = basemap::join_normal(points, slot);
    if salience == TrailSalience::Selected {
        let reach = join[0].hypot(join[1]);
        if reach > SELECTED_MITER_LIMIT {
            let scale = SELECTED_MITER_LIMIT / reach;
            join = [join[0] * scale, join[1] * scale];
        }
    }
    [[-join[0], -join[1]], join]
}

fn ribbon_direction(from: [f32; 2], to: [f32; 2], sign: f32) -> [f32; 2] {
    let delta = [to[0] - from[0], to[1] - from[1]];
    let length = delta[0].hypot(delta[1]);
    if length <= f32::EPSILON {
        [0.0, 0.0]
    } else {
        [delta[0] * sign / length, delta[1] * sign / length]
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

const CADENCE_LAW_SHIFT: u32 = 9;
#[cfg(test)]
const CADENCE_STYLE_MASK: u32 = (1 << CADENCE_LAW_SHIFT) - 1;

const fn cadence_word(
    law: u32,
    pattern: u32,
    positive_side: bool,
    informal: bool,
    terrain: u32,
    blocked: bool,
) -> u32 {
    assert!(
        law <= u32::MAX >> CADENCE_LAW_SHIFT,
        "cadence law exceeds packed vertex range"
    );
    assert!(pattern < 4, "trail pattern exceeds packed vertex range");
    assert!(terrain < 16, "trail terrain exceeds packed vertex range");
    (law << CADENCE_LAW_SHIFT)
        | ((blocked as u32) << 8)
        | (terrain << 4)
        | ((informal as u32) << 3)
        | (pattern << 1)
        | positive_side as u32
}

const fn terrain_code(terrain: Terrain) -> u32 {
    match terrain {
        Terrain::Unknown => 0,
        Terrain::Trail => 1,
        Terrain::Forest => 2,
        Terrain::Alpine => 3,
        Terrain::Talus => 4,
        Terrain::Scramble => 5,
        Terrain::Pavement => 6,
        Terrain::Road => 7,
        Terrain::Water => 8,
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TrailPoint {
    local: [f32; 2],
    extrusion: [f32; 2],
    srgb: [u8; 4],
    arc_world: f32,
    cadence: u32,
    edge_factor: f32,
}
const _: () = assert!(size_of::<TrailPoint>() == 32);

#[cfg(test)]
impl TrailPoint {
    const fn law(&self) -> u32 {
        self.cadence >> CADENCE_LAW_SHIFT
    }

    const fn pattern(&self) -> u32 {
        (self.cadence >> 1) & 3
    }

    const fn set_law(&mut self, law: u32) {
        assert!(
            law <= u32::MAX >> CADENCE_LAW_SHIFT,
            "cadence law exceeds packed vertex range"
        );
        self.cadence = (law << CADENCE_LAW_SHIFT) | (self.cadence & CADENCE_STYLE_MASK);
    }
}

struct TrailMesh {
    vertices: Arc<[TrailPoint]>,
    indices: Arc<[u32]>,
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
            })
            .sum()
    }
}

#[derive(Clone, Copy)]
enum CadenceDatum {
    Solid,
    Stem {
        datum_world: f64,
        reverse: bool,
        length_world: f64,
    },
    Chord {
        endpoint_datums_world: [f64; 2],
        length_world: f64,
    },
}

fn cadence_laws(edges: &[WorldEdge]) -> (Vec<u32>, Vec<CadenceDatum>) {
    let mut laws = vec![CadenceDatum::Solid];
    let law_ids = edges
        .iter()
        .map(|edge| {
            let Some(lineage) = edge.lineage else {
                return 0;
            };
            let law = match lineage {
                CadenceLineage::Stem {
                    datum_world,
                    reverse,
                } => CadenceDatum::Stem {
                    datum_world,
                    reverse,
                    length_world: edge.length_world,
                },
                CadenceLineage::Chord {
                    endpoint_datums_world,
                } => CadenceDatum::Chord {
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
    layers: Arc<[TrailLayer]>,
    repaint: egui::Context,
    center_world: [f64; 2],
    world_points: f32,
    viewport_points: [f32; 2],
    view_zoom: f32,
    cadence_cells_per_world: f32,
    dialect: TrailDialect,
    coloring: TrailColoring,
}

impl TrailPaint {
    fn visible_tile_count(&self) -> usize {
        self.layers.iter().map(|layer| layer.tiles.len()).sum()
    }
}

#[derive(Clone)]
struct TrailLayer {
    band: DetailBand,
    tiles: Arc<[Arc<TrailTile>]>,
    opacity: f32,
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

    fn finish_prepare(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(gpu) = resources.get_mut::<TrailMapGpu>() {
            gpu.finish_prepare();
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
        let Some(view) = gpu.views.get(&self.corpus) else {
            return;
        };
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, &view.bind, &[]);
        pass.set_bind_group(1, &view.law_bind, &[]);
        for (layer_slot, layer) in self.layers.iter().enumerate() {
            pass.set_bind_group(2, &view.opacities[layer_slot].bind, &[]);
            for tile in layer.tiles.iter() {
                let key = GpuKey {
                    corpus: self.corpus,
                    tile: tile.key,
                    band: layer.band,
                };
                if let Some(tile) = gpu.tiles.get(&key) {
                    tile.draw
                        .paint(pass, &tile.buffer, &tile.transform, 0..view.instances);
                    if self.dialect.core {
                        let core = MAX_WRAP_INSTANCES as u32;
                        tile.draw.paint(
                            pass,
                            &tile.buffer,
                            &tile.transform,
                            core..core + view.instances,
                        );
                    }
                }
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
    bytes: usize,
    touched: u64,
}

impl GpuTrailTile {
    fn raise(
        device: &wgpu::Device,
        tile: &TrailTile,
        band: DetailBand,
        touched: u64,
    ) -> Option<Self> {
        let mesh = &tile.bands[band.index()];
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
            usage: wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::INDEX
                | wgpu::BufferUsages::COPY_DST,
        });
        Some(Self {
            draw,
            buffer,
            transform,
            bytes,
            touched,
        })
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
    camera_layout: wgpu::BindGroupLayout,
    law_layout: wgpu::BindGroupLayout,
    opacity_layout: wgpu::BindGroupLayout,
    views: HashMap<TrailCorpus, GpuView>,
    tiles: HashMap<GpuKey, GpuTrailTile>,
    visible: HashMap<TrailCorpus, HashSet<GpuKey>>,
    prepared: HashSet<GpuKey>,
    finish_pending: bool,
    order: VecDeque<(GpuKey, u64)>,
    epoch: u64,
    bytes: usize,
    law_scratch: Vec<GpuLaw>,
    profile: bool,
}

struct GpuView {
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    law_buffer: wgpu::Buffer,
    law_bind: wgpu::BindGroup,
    law_cells_per_world: f32,
    opacities: [GpuOpacity; 2],
    instances: u32,
    bytes: usize,
}

impl GpuView {
    fn raise(
        device: &wgpu::Device,
        camera_layout: &wgpu::BindGroupLayout,
        law_layout: &wgpu::BindGroupLayout,
        opacity_layout: &wgpu::BindGroupLayout,
        paint: &TrailPaint,
    ) -> Self {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("trail-map-uniform"),
            size: size_of::<Uniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = camera_bind(device, camera_layout, &uniform);
        let gpu_laws = paint
            .laws
            .iter()
            .copied()
            .map(|law| GpuLaw::forge(law, f64::from(paint.cadence_cells_per_world)))
            .collect::<Vec<_>>();
        let law_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("trail-corpus-cadence-laws"),
            contents: bytemuck::cast_slice(&gpu_laws),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let law_bind = law_bind(device, law_layout, &law_buffer);
        let opacities = std::array::from_fn(|_| GpuOpacity::raise(device, opacity_layout));
        let bytes = size_of::<Uniform>()
            .saturating_add(gpu_laws.len().saturating_mul(size_of::<GpuLaw>()))
            .saturating_add(opacities.len().saturating_mul(size_of::<OpacityUniform>()));
        Self {
            uniform,
            bind,
            law_buffer,
            law_bind,
            law_cells_per_world: paint.cadence_cells_per_world,
            opacities,
            instances: 1,
            bytes,
        }
    }

    fn refresh_laws(
        &mut self,
        queue: &wgpu::Queue,
        paint: &TrailPaint,
        scratch: &mut Vec<GpuLaw>,
    ) -> usize {
        if self.law_cells_per_world.to_bits() == paint.cadence_cells_per_world.to_bits() {
            return 0;
        }
        scratch.clear();
        scratch.extend(
            paint
                .laws
                .iter()
                .copied()
                .map(|law| GpuLaw::forge(law, f64::from(paint.cadence_cells_per_world))),
        );
        let bytes = scratch.len().saturating_mul(size_of::<GpuLaw>());
        queue.write_buffer(&self.law_buffer, 0, bytemuck::cast_slice(scratch));
        self.law_cells_per_world = paint.cadence_cells_per_world;
        bytes
    }

    fn refresh_opacities(&mut self, queue: &wgpu::Queue, layers: &[TrailLayer]) -> usize {
        layers
            .iter()
            .enumerate()
            .map(|(slot, layer)| self.opacities[slot].refresh(queue, layer.opacity))
            .sum()
    }
}

struct GpuOpacity {
    buffer: wgpu::Buffer,
    bind: wgpu::BindGroup,
    value: f32,
}

impl GpuOpacity {
    fn raise(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> Self {
        let value = 1.0;
        let uniform = OpacityUniform::forge(value);
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("trail-detail-opacity"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind = opacity_bind(device, layout, &buffer);
        Self {
            buffer,
            bind,
            value,
        }
    }

    fn refresh(&mut self, queue: &wgpu::Queue, value: f32) -> usize {
        if self.value.to_bits() == value.to_bits() {
            return 0;
        }
        queue.write_buffer(
            &self.buffer,
            0,
            bytemuck::bytes_of(&OpacityUniform::forge(value)),
        );
        self.value = value;
        size_of::<OpacityUniform>()
    }
}

impl TrailMapGpu {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
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
        let opacity_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("trail-map-detail-opacity"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(size_of::<OpacityUniform>() as u64),
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("trail-map"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&law_layout),
                Some(&opacity_layout),
            ],
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
            camera_layout,
            law_layout,
            opacity_layout,
            views: HashMap::new(),
            tiles: HashMap::new(),
            visible: HashMap::new(),
            prepared: HashSet::new(),
            finish_pending: false,
            order: VecDeque::new(),
            epoch: 0,
            bytes: 0,
            law_scratch: Vec::new(),
            profile: std::env::var_os("TRAILGEN_PROFILE_TRAILS").is_some(),
        }
    }

    fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, paint: &TrailPaint) {
        let visible_tiles = paint.visible_tile_count();
        let _phase = tracing::info_span!(
            target: "eternalist::main",
            "gpu.trail_prepare",
            layers = paint.layers.len(),
            tiles = visible_tiles,
        )
        .entered();
        let begun = Instant::now();
        let visible = paint
            .layers
            .iter()
            .flat_map(|layer| {
                layer.tiles.iter().map(|tile| GpuKey {
                    corpus: paint.corpus,
                    tile: tile.key,
                    band: layer.band,
                })
            })
            .collect::<HashSet<_>>();
        let visible_changed = self.visible.get(&paint.corpus) != Some(&visible);
        if visible_changed {
            self.epoch = self.epoch.saturating_add(1);
        }
        self.prepared.extend(visible.iter().copied());
        self.finish_pending = true;
        self.visible.insert(paint.corpus, visible);

        if !self.views.contains_key(&paint.corpus) {
            let view = GpuView::raise(
                device,
                &self.camera_layout,
                &self.law_layout,
                &self.opacity_layout,
                paint,
            );
            self.bytes = self.bytes.saturating_add(view.bytes);
            let _prior = self.views.insert(paint.corpus, view);
        }
        let view = self
            .views
            .get_mut(&paint.corpus)
            .expect("trail corpus view was just established");
        let cadence_uploaded = view.refresh_laws(queue, paint, &mut self.law_scratch);
        let opacity_uploaded = view.refresh_opacities(queue, &paint.layers);
        let uniform = Uniform::forge(paint);
        queue.write_buffer(&view.uniform, 0, bytemuck::bytes_of(&uniform));
        view.instances = uniform.wrap_radius.saturating_mul(2).saturating_add(1);

        let mut missing = Vec::new();
        for layer in paint.layers.iter() {
            for tile in layer.tiles.iter() {
                let key = GpuKey {
                    corpus: paint.corpus,
                    tile: tile.key,
                    band: layer.band,
                };
                if let Some(resident) = self.tiles.get_mut(&key) {
                    if visible_changed {
                        resident.touched = self.epoch;
                        self.order.push_back((key, self.epoch));
                    }
                    continue;
                }
                missing.push((key, tile.as_ref()));
            }
        }
        missing.sort_unstable_by(|(left, _), (right, _)| {
            tile_distance2(left.tile, paint.center_world)
                .total_cmp(&tile_distance2(right.tile, paint.center_world))
        });
        let mut uploaded = 0_usize;
        let mut deferred = false;
        for (key, tile) in missing {
            if uploaded > 0
                && (uploaded >= GPU_UPLOAD_BYTES || begun.elapsed() >= GPU_UPLOAD_BUDGET)
            {
                deferred = true;
                break;
            }
            let Some(resident) = GpuTrailTile::raise(device, tile, key.band, self.epoch) else {
                continue;
            };
            uploaded = uploaded.saturating_add(resident.bytes);
            self.bytes = self.bytes.saturating_add(resident.bytes);
            self.order.push_back((key, self.epoch));
            let _prior = self.tiles.insert(key, resident);
        }
        if deferred {
            paint.repaint.request_repaint();
        }
        self.report_prepare(
            paint,
            begun.elapsed(),
            uploaded,
            deferred,
            cadence_uploaded,
            opacity_uploaded,
        );
    }

    fn report_prepare(
        &self,
        paint: &TrailPaint,
        elapsed: Duration,
        uploaded: usize,
        deferred: bool,
        cadence_uploaded: usize,
        opacity_uploaded: usize,
    ) {
        if !self.profile {
            return;
        }
        eprintln!(
            "trail-gpu prepare_us={} upload_bytes={uploaded} active_tiles={} layers={} deferred={deferred}",
            elapsed.as_micros(),
            self.visible[&paint.corpus].len(),
            paint.layers.len(),
        );
        if cadence_uploaded != 0 {
            eprintln!("trail-gpu cadence_upload_bytes={cadence_uploaded}");
        }
        if opacity_uploaded != 0 {
            eprintln!("trail-gpu opacity_upload_bytes={opacity_uploaded}");
        }
    }

    fn finish_prepare(&mut self) {
        if !std::mem::take(&mut self.finish_pending) {
            return;
        }
        self.reap();
        self.prepared.clear();
    }

    fn reap(&mut self) {
        let candidates = self.order.len();
        for _ in 0..candidates {
            if self.bytes <= GPU_CEILING {
                break;
            }
            let Some((key, epoch)) = self.order.pop_front() else {
                break;
            };
            let Some(resident) = self.tiles.get(&key) else {
                continue;
            };
            if resident.touched != epoch {
                continue;
            }
            if self.prepared.contains(&key) {
                self.order.push_back((key, epoch));
                continue;
            }
            let resident = self
                .tiles
                .remove(&key)
                .expect("resident trail tile survived candidate inspection");
            self.bytes = self.bytes.saturating_sub(resident.bytes);
        }
        let dead = self
            .views
            .keys()
            .copied()
            .filter(|corpus| {
                !self.prepared.iter().any(|key| key.corpus == *corpus)
                    && !self.tiles.keys().any(|key| key.corpus == *corpus)
            })
            .collect::<Vec<_>>();
        for corpus in dead {
            if let Some(view) = self.views.remove(&corpus) {
                self.bytes = self.bytes.saturating_sub(view.bytes);
            }
        }
        self.visible
            .retain(|corpus, _| self.views.contains_key(corpus));
    }
}

fn tile_distance2(key: TileKey, center: [f64; 2]) -> f64 {
    let scale = f64::from(1_u32 << key.zoom);
    let tile = [
        (f64::from(key.x) + 0.5) / scale,
        (f64::from(key.y) + 0.5) / scale,
    ];
    let dx = (tile[0] - center[0]).abs();
    dx.min(1.0 - dx)
        .mul_add(dx.min(1.0 - dx), (tile[1] - center[1]).powi(2))
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

fn opacity_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("trail-map-detail-opacity"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform.as_entire_binding(),
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
    fn forge(law: CadenceDatum, cells_per_world: f64) -> Self {
        match law {
            CadenceDatum::Solid => Self::zeroed(),
            CadenceDatum::Stem {
                datum_world,
                reverse,
                length_world,
            } => Self {
                metrics: [
                    lattice_phase(
                        if reverse {
                            datum_world + length_world
                        } else {
                            datum_world
                        },
                        cells_per_world,
                    ),
                    0.0,
                    if reverse { -2.0 } else { -1.0 },
                    length_world as f32,
                ],
            },
            CadenceDatum::Chord {
                endpoint_datums_world,
                length_world,
            } => Self {
                metrics: [
                    lattice_phase(endpoint_datums_world[0], cells_per_world),
                    lattice_phase(endpoint_datums_world[1], cells_per_world),
                    (length_world * 0.5) as f32,
                    length_world as f32,
                ],
            },
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct OpacityUniform {
    opacity: f32,
    _padding: [f32; 3],
}

impl OpacityUniform {
    const fn forge(opacity: f32) -> Self {
        Self {
            opacity,
            _padding: [0.0; 3],
        }
    }
}
const _: () = assert!(size_of::<OpacityUniform>() == 16);

fn lattice_phase(datum_world: f64, cells_per_world: f64) -> f32 {
    (datum_world * cells_per_world).rem_euclid(8.0) as f32
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
    cadence_cells_per_world: f32,
    radii: [f32; 2],
    disclosure: [f32; 4],
    projection: [u32; 4],
    palette: [[f32; 4]; 11],
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
            cadence_cells_per_world: paint.cadence_cells_per_world,
            radii: [
                paint.dialect.salience.width() * 0.5,
                trail_core_width(paint.dialect.salience.width()) * 0.5,
            ],
            disclosure: paint.dialect.disclosure,
            projection: [coloring_shader_code(paint.coloring), 0, 0, 0],
            palette: trail_palette(paint.dialect.salience),
        }
    }
}

fn trail_palette(salience: TrailSalience) -> [[f32; 4]; 11] {
    const TERRAINS: [Terrain; 9] = [
        Terrain::Unknown,
        Terrain::Trail,
        Terrain::Forest,
        Terrain::Alpine,
        Terrain::Talus,
        Terrain::Scramble,
        Terrain::Pavement,
        Terrain::Road,
        Terrain::Water,
    ];
    let mut palette = [[0.0; 4]; 11];
    palette[0] = normalized(formality_color(false, salience));
    palette[1] = normalized(formality_color(true, salience));
    for (slot, terrain) in TERRAINS.into_iter().enumerate() {
        palette[slot + 2] = normalized(terrain_color(terrain, salience));
    }
    palette
}

fn normalized(color: Color32) -> [f32; 4] {
    color.to_array().map(|channel| f32::from(channel) / 255.0)
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TileInstance {
    origin_high: [f32; 2],
    origin_low: [f32; 2],
    span: f32,
    wrap: u32,
    layer: u32,
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
    const ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Unorm8x4,
        3 => Float32,
        4 => Uint32,
        5 => Float32
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
    cadence_cells_per_world: f32,
    radii: vec2f,
    disclosure: vec4f,
    projection: vec4u,
    palette: array<vec4f, 11>,
};

struct CadenceLaw {
    metrics: vec4f,
};

struct DetailOpacity {
    opacity: f32,
};

@group(0) @binding(0) var<uniform> u: Uniform;
@group(1) @binding(0) var<storage, read> laws: array<CadenceLaw>;
@group(2) @binding(0) var<uniform> detail: DetailOpacity;

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
    @location(5) edge_factor: f32,
    @location(7) origin_high: vec2f,
    @location(8) origin_low: vec2f,
    @location(9) tile_span: f32,
    @location(10) wrap: u32,
    @location(11) layer: u32,
) -> VertexOut {
    var out: VertexOut;
    let pattern = (cadence >> 1u) & 3u;
    let informal = (cadence >> 3u) & 1u;
    let terrain = (cadence >> 4u) & 15u;
    let blocked = (cadence >> 8u) & 1u;
    let law = cadence >> 9u;
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
    var tube = color;
    if blocked == 0u && u.projection.x == 1u {
        tube = u.palette[informal];
    }
    if blocked == 0u && u.projection.x == 2u {
        tube = u.palette[2u + min(terrain, 8u)];
    }
    let core_alpha = select(0.0, 0.5, pattern != 0u);
    let ink = select(
        tube,
        vec4f(20.0 / 255.0, 19.0 / 255.0, 17.0 / 255.0, core_alpha),
        core,
    );
    out.color = vec4f(ink.rgb, ink.a * maturity * detail.opacity);
    out.edge_distance = edge_factor * expanded_radius;
    out.solid_radius = visible_radius;
    out.tile_local = local + extrusion * expanded_radius / (u.world_points * tile_span);
    out.arc_world = arc_world;
    out.law = select(0u, law, core);
    out.pattern = select(0u, pattern, core);
    return out;
}

fn cadence_coordinate(in: VertexOut) -> f32 {
    let law = laws[in.law];
    let arc = in.arc_world;
    if law.metrics.z == -2.0 {
        return law.metrics.x - arc * u.cadence_cells_per_world;
    }
    if law.metrics.z >= 0.0 && arc > law.metrics.z {
        return law.metrics.y
            + (law.metrics.w - arc) * u.cadence_cells_per_world;
    }
    return law.metrics.x + arc * u.cadence_cells_per_world;
}

fn alternating_ink(coordinate: f32) -> f32 {
    let wave = sin(3.14159265359 * coordinate);
    let feather = max(fwidth(wave), 0.01);
    return smoothstep(-feather, feather, wave);
}

fn cyclic_interval_ink(phase: f32, period: f32, end: f32) -> f32 {
    let distance_to_start = min(phase, period - phase);
    let distance_to_end = abs(phase - end);
    let distance = min(distance_to_start, distance_to_end);
    let signed = select(-distance, distance, phase <= end);
    let feather = max(fwidth(signed), 0.01);
    return smoothstep(-feather, feather, signed);
}

fn cadence_ink(in: VertexOut) -> f32 {
    if in.pattern == 0u {
        return 1.0;
    }
    let coordinate = cadence_coordinate(in);
    let cell_points = u.world_points / u.cadence_cells_per_world;
    if in.pattern == 1u {
        return alternating_ink(coordinate);
    }
    if in.pattern == 2u {
        let micro_coordinate = coordinate * 4.0;
        let phase = micro_coordinate - floor(micro_coordinate / 8.0) * 8.0;
        let axial = abs(phase - 5.0) * cell_points * 0.25;
        let dot_distance = length(vec2f(axial, in.edge_distance));
        let dot_feather = max(fwidth(dot_distance), 0.55);
        let dot = 1.0 - smoothstep(0.62 - dot_feather, 0.62 + dot_feather, dot_distance);
        return max(
            cyclic_interval_ink(phase, 8.0, 3.0),
            dot,
        );
    }
    let phase = coordinate - floor(coordinate / 2.0) * 2.0;
    let axial = min(phase, 2.0 - phase) * cell_points;
    let radius = 0.6624;
    let distance_to_center = length(vec2f(axial, in.edge_distance));
    let feather = max(fwidth(distance_to_center), 0.55);
    return 1.0 - smoothstep(radius - feather, radius + feather, distance_to_center);
}

fn painted(in: VertexOut) -> vec4f {
    if any(in.tile_local < vec2f(0.0)) || any(in.tile_local >= vec2f(1.0)) {
        discard;
    }
    let distant_feather = mix(1.05, 0.65, apparition(u.disclosure.x));
    let feather = max(fwidth(in.edge_distance), distant_feather);
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
    use crate::map::{EARTH_CIRCUMFERENCE_M, way_kind_color};
    use trailgen_core::WayKind;

    #[test]
    fn detail_admission_is_monotone_and_exact_at_fourteen() {
        let onset = f64::from(TUBE_ONSET_ZOOM);
        let bands = [onset + 0.01, 9.9, 10.0, 11.0, 12.0, 13.0, 14.0, 23.0]
            .into_iter()
            .filter_map(|zoom| DetailBand::for_zoom(zoom, TUBE_ONSET_ZOOM))
            .collect::<Vec<_>>();
        assert!(DetailBand::for_zoom(onset, TUBE_ONSET_ZOOM).is_none());
        assert_eq!(bands.first().copied(), Some(DetailBand(0)));
        assert!(bands.windows(2).all(|pair| pair[0].0 <= pair[1].0));
        assert_eq!(
            bands.last().copied(),
            Some(DetailBand(LAST_BAND - FIRST_BAND))
        );
        assert!(DetailBand(LAST_BAND - FIRST_BAND).tolerance_world() <= f64::EPSILON);
    }

    #[test]
    fn detail_band_resists_boundary_chatter_in_both_directions() {
        let twelve = DetailBand::for_zoom(12.5, TUBE_ONSET_ZOOM).expect("z12 owns trail detail");
        let thirteen = DetailBand::for_zoom(13.5, TUBE_ONSET_ZOOM).expect("z13 owns trail detail");

        assert_eq!(
            DetailBand::resolve(Some(twelve), 13.05, TUBE_ONSET_ZOOM),
            Some(twelve)
        );
        assert_eq!(
            DetailBand::resolve(Some(twelve), 13.10, TUBE_ONSET_ZOOM),
            Some(thirteen)
        );
        assert_eq!(
            DetailBand::resolve(Some(thirteen), 12.95, TUBE_ONSET_ZOOM),
            Some(thirteen)
        );
        assert_eq!(
            DetailBand::resolve(Some(thirteen), 12.90, TUBE_ONSET_ZOOM),
            Some(twelve)
        );
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
        const REGIONAL_FRAME_ZOOM: f32 = 11.14;
        let apparent = |meters: f64, zoom: f32| {
            let parallel = EARTH_CIRCUMFERENCE_M * HARRIMAN_LATITUDE_DEG.to_radians().cos();
            meters * 256.0 * f64::from(zoom).exp2() / parallel
        };
        let pattern_zoom = f64::from(PATTERN_ONSET_ZOOM + DISCLOSURE_SPAN_ZOOM);
        let cadence = WorldLevel::at_zoom(pattern_zoom);
        let cadence_period = 2.0 * 256.0 * pattern_zoom.exp2() / cadence.cells_per_world();

        assert!((2.65..=2.90).contains(&apparent(HARRIMAN_FILAMENT_P95_M, TUBE_ONSET_ZOOM)));
        assert!((3.15..=3.45).contains(&apparent(HARRIMAN_FILAMENT_P95_M, CORE_ONSET_ZOOM)));
        assert!((8.0..=16.0).contains(&cadence_period));
        assert!((1.9..=4.50).contains(
            &(apparent(1_000.0, PATTERN_ONSET_ZOOM + DISCLOSURE_SPAN_ZOOM) / cadence_period)
        ));

        let apparition = |onset: f32, zoom: f32| {
            let phase = ((zoom - onset) / DISCLOSURE_SPAN_ZOOM).clamp(0.0, 1.0);
            phase * phase * 2.0_f32.mul_add(-phase, 3.0)
        };
        assert!((0.55..=0.70).contains(&apparition(TUBE_ONSET_ZOOM, REGIONAL_FRAME_ZOOM)));
        assert!(apparition(CORE_ONSET_ZOOM, REGIONAL_FRAME_ZOOM) < 0.10);
        assert!(apparition(PATTERN_ONSET_ZOOM, REGIONAL_FRAME_ZOOM).abs() < f32::EPSILON);
    }

    #[test]
    fn sidewalks_are_one_intrinsic_grey_monolith() {
        let sidewalks = TrailField::sidewalks(&[]);
        assert!(!sidewalks.dialect.core);
        assert!(matches!(sidewalks.dialect.hue, HueAuthority::Intrinsic));
        assert_eq!(
            sidewalks.dialect.coloring(TrailColoring::Terrain),
            TrailColoring::Class
        );
        assert!(
            sidewalks.dialect.disclosure[..3]
                .iter()
                .all(|onset| *onset == PEDESTRIAN_DIAGNOSTIC_TUBE_ONSET_ZOOM)
        );

        let crossings = TrailField::crossing_diagnostics(&[]);
        assert!(crossings.dialect.core);
        assert!(matches!(crossings.dialect.hue, HueAuthority::Projected));
    }

    #[test]
    fn deep_detail_owns_deep_spatial_tiles() {
        assert_eq!(
            DetailBand::for_zoom(11.9, TUBE_ONSET_ZOOM)
                .unwrap()
                .spatial_zoom(),
            12
        );
        assert_eq!(
            DetailBand::for_zoom(13.2, TUBE_ONSET_ZOOM)
                .unwrap()
                .spatial_zoom(),
            13
        );
        assert_eq!(
            DetailBand::for_zoom(23.0, TUBE_ONSET_ZOOM)
                .unwrap()
                .spatial_zoom(),
            14
        );
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
        let datum = 0.031_415_926_535_897_934;
        let cells_per_world = 2.0_f64.powi(29);
        let expected = (datum * cells_per_world).rem_euclid(8.0) as f32;

        assert_eq!(
            lattice_phase(datum, cells_per_world).to_bits(),
            expected.to_bits()
        );
    }

    #[test]
    fn cadence_lattice_edges_are_nested_world_coordinates() {
        let coarse = WorldLevel::at_zoom(12.0);
        let fine = WorldLevel::at_zoom(13.0);
        for edge in -32..=32 {
            let world = f64::from(edge) / coarse.cells_per_world();
            let fine_coordinate = world * fine.cells_per_world();
            assert!((fine_coordinate - f64::from(edge * 2)).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn cadence_level_tracks_live_zoom_without_boundary_chatter() {
        let level = WorldLevel::resolve(None, 13.4, DETAIL_HYSTERESIS_ZOOM);
        assert_eq!(level, WorldLevel::at_zoom(13.4));
        assert_eq!(
            WorldLevel::resolve(Some(level), 14.04, DETAIL_HYSTERESIS_ZOOM),
            level
        );
        assert_eq!(
            WorldLevel::resolve(Some(level), 14.09, DETAIL_HYSTERESIS_ZOOM),
            WorldLevel::at_zoom(14.09)
        );
        assert_eq!(
            WorldLevel::resolve(
                Some(WorldLevel::at_zoom(14.1)),
                13.93,
                DETAIL_HYSTERESIS_ZOOM
            ),
            WorldLevel::at_zoom(14.1)
        );
        assert_eq!(
            WorldLevel::resolve(
                Some(WorldLevel::at_zoom(14.1)),
                13.91,
                DETAIL_HYSTERESIS_ZOOM
            ),
            WorldLevel::at_zoom(13.91)
        );
    }

    #[test]
    fn reverse_stems_and_chords_own_world_anchored_endpoint_phases() {
        let lattice = 256.0;
        let reverse = GpuLaw::forge(
            CadenceDatum::Stem {
                datum_world: 0.2,
                reverse: true,
                length_world: 0.1,
            },
            lattice,
        );
        let chord = GpuLaw::forge(
            CadenceDatum::Chord {
                endpoint_datums_world: [0.2, 0.5],
                length_world: 0.1,
            },
            lattice,
        );

        assert_eq!(
            reverse.metrics[0].to_bits(),
            lattice_phase(0.3, lattice).to_bits()
        );
        assert_eq!(reverse.metrics[2].to_bits(), (-2.0_f32).to_bits());
        assert_eq!(
            chord.metrics[0].to_bits(),
            lattice_phase(0.2, lattice).to_bits()
        );
        assert_eq!(
            chord.metrics[1].to_bits(),
            lattice_phase(0.5, lattice).to_bits()
        );
        assert!((chord.metrics[2] - 0.05).abs() <= f32::EPSILON);
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
            color: way_kind_color(WayKind::Path),
            way_kind: trailgen_core::WayKind::Path,
            standing: TrailStanding::Established,
            terrain: Terrain::Trail,
            mark: TrailMark::Solid,
            access: trailgen_core::Access::Open,
        };
        let mut builder = TrailMeshBuilder::default();
        builder.push(&samples, key, &edge, 0, TrailSalience::Context, false);
        let mesh = builder.seal();

        assert_eq!(mesh.vertices.len(), samples.len() * 2);
        assert_eq!(mesh.indices.len(), (samples.len() - 1) * 6);
    }

    #[test]
    fn semantic_hue_bits_cannot_alias_cadence_bits() {
        for pattern in 0..4 {
            let plain = cadence_word(37, pattern, false, false, 0, false);
            let adorned = cadence_word(37, pattern, true, true, terrain_code(Terrain::Water), true);
            for word in [plain, adorned] {
                let mut point = TrailPoint {
                    local: [0.0; 2],
                    extrusion: [0.0; 2],
                    srgb: [0; 4],
                    arc_world: 0.0,
                    cadence: word,
                    edge_factor: 0.0,
                };
                assert_eq!(point.law(), 37);
                assert_eq!(point.pattern(), pattern);
                let style = point.cadence & CADENCE_STYLE_MASK;
                point.set_law(91);
                assert_eq!(point.law(), 91);
                assert_eq!(point.pattern(), pattern);
                assert_eq!(point.cadence & CADENCE_STYLE_MASK, style);
            }
        }
    }

    #[test]
    fn selected_ribbons_own_semicircular_support_caps() {
        let mut builder = TrailMeshBuilder::default();
        builder.push_round_cap(
            [0.5, 0.5],
            [1.0, 0.0],
            [255; 4],
            0.25,
            cadence_word(0, 0, false, false, terrain_code(Terrain::Trail), false),
        );
        assert_eq!(builder.vertices.len(), ROUND_CAP_STEPS + 2);
        assert_eq!(builder.indices.len(), ROUND_CAP_STEPS * 3);
        assert!(builder.vertices[0].edge_factor.abs() < f32::EPSILON);
        assert!(builder.vertices[1..].iter().all(|vertex| {
            (vertex.extrusion[0].hypot(vertex.extrusion[1]) - 1.0).abs() < 1.0e-5
                && vertex.extrusion[0] >= -f32::EPSILON
                && (vertex.edge_factor - 1.0).abs() < f32::EPSILON
        }));
    }

    #[test]
    fn selected_ribbons_repel_miter_spikes() {
        let corner = [[0.0, 0.0], [1.0, 0.0], [0.5, 0.866_025_4]];
        let context = ribbon_extrusions(&corner, 1, TrailSalience::Context);
        let selected = ribbon_extrusions(&corner, 1, TrailSalience::Selected);
        assert!(context[1][0].hypot(context[1][1]) > SELECTED_MITER_LIMIT);
        assert!(selected.iter().all(
            |extrusion| extrusion[0].hypot(extrusion[1]) <= SELECTED_MITER_LIMIT + f32::EPSILON
        ));
    }

    #[test]
    fn every_way_kind_survives_into_the_coarsest_band() {
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
                color: way_kind_color(class),
                way_kind: class,
                standing: TrailStanding::Established,
                terrain: Terrain::Trail,
                mark: TrailMark::Solid,
                access: trailgen_core::Access::Open,
            }
        };
        let classes = [
            WayKind::Unknown,
            WayKind::Path,
            WayKind::Footway,
            WayKind::Track,
            WayKind::ServiceRoad,
            WayKind::PedestrianStreet,
            WayKind::Steps,
            WayKind::Bridleway,
            WayKind::Bushwhack,
            WayKind::Roadway,
            WayKind::Cycleway,
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
