use crate::{
    annotation,
    basemap::{self, Mesh, StrokePoint, TileKey, VectorTile},
    map::{self, CartographicPlan, MapFramePlan},
    persistence,
    vector_map::{GeometryPass, VectorCorpus, VectorGap, VectorLayer, VectorPaint, VectorPatch},
};
use anyhow::{Context as _, Result, ensure};
use crossbeam_channel::{Receiver, bounded};
#[cfg(test)]
use egui::Rect;
use egui::{Color32, Painter};
use eternalist_apps::NativeWake;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{Cursor, Read as _},
    path::Path,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use trailgen_data::{TopographicTile, Topography};

const CACHE: &str = "cache/isohypses-v4.bin";
const MAGIC: &[u8; 8] = b"TRLISO04";
const INTERVAL_M: i32 = 10;
const INDEX_INTERVAL_M: i32 = 50;
const POINT_CEILING: usize = 16_000_000;
const LABEL_INDEX_ZOOM: u8 = 14;
const LABEL_CELL_CEILING: usize = 64;
const DETAIL_HYSTERESIS_ZOOM: f64 = 0.08;
const DETAIL_TRANSITION: Duration = Duration::from_millis(160);
const RELIEF_LAWS: [ReliefLaw; 4] = [
    ReliefLaw {
        onset_zoom: 10.5,
        reference_zoom: 11.75,
        error_points: 0.65,
        minors: false,
    },
    ReliefLaw {
        onset_zoom: 11.75,
        reference_zoom: 13.5,
        error_points: 0.60,
        minors: true,
    },
    ReliefLaw {
        onset_zoom: 13.5,
        reference_zoom: 15.75,
        error_points: 0.50,
        minors: true,
    },
    ReliefLaw {
        onset_zoom: 15.75,
        reference_zoom: 0.0,
        error_points: 0.0,
        minors: true,
    },
];

pub struct Relief {
    corpus: VectorCorpus,
    field: Arc<Field>,
    revision: u64,
    visibility: Option<ReliefVisibility>,
    transition: Option<ReliefTransition>,
    events: Receiver<Result<Field>>,
    worker: thread::JoinHandle<()>,
}

#[derive(Clone)]
struct ReliefVisibility {
    frame: MapFramePlan,
    detail: u8,
    patches: Arc<[VectorPatch]>,
}

struct ReliefTransition {
    prior: ReliefVisibility,
    begun: Instant,
}

struct Field {
    isohypses: Vec<Isohypse>,
    tiles: Vec<Arc<[Arc<VectorTile>]>>,
    label_index: LabelIndex,
}

struct Isohypse {
    key: TileKey,
    elevation_m: i32,
    label: Option<Arc<str>>,
    points: Arc<[[f64; 2]]>,
    bounds: [f64; 4],
}

impl Default for Field {
    fn default() -> Self {
        Self::seal(Vec::new())
    }
}

impl Field {
    fn seal(mut isohypses: Vec<Isohypse>) -> Self {
        isohypses.retain(|isohypse| isohypse.elevation_m != 0);
        let tiles = RELIEF_LAWS
            .into_iter()
            .map(|law| raise_tiles(&isohypses, law))
            .collect::<Vec<_>>();
        if std::env::var_os("TRAILGEN_PROFILE_RELIEF").is_some() {
            let bytes = tiles
                .iter()
                .map(|band| band.iter().map(|tile| tile.resident_bytes()).sum::<usize>())
                .collect::<Vec<_>>();
            eprintln!(
                "relief-atlas chunks={} tiles={} band_bytes={bytes:?}",
                isohypses.len(),
                tiles.first().map_or(0, |band| band.len()),
            );
        }
        let label_index = index_labels(&isohypses);
        Self {
            isohypses,
            tiles,
            label_index,
        }
    }
}

#[derive(Clone, Copy)]
struct ReliefLaw {
    onset_zoom: f32,
    reference_zoom: f32,
    error_points: f64,
    minors: bool,
}

impl ReliefLaw {
    fn for_zoom(zoom: f64) -> Option<(u8, Self)> {
        let slot = RELIEF_LAWS
            .iter()
            .rposition(|law| zoom >= f64::from(law.onset_zoom))?;
        Some((slot as u8, RELIEF_LAWS[slot]))
    }

    fn resolve(prior: Option<u8>, zoom: f64) -> Option<(u8, Self)> {
        let target = Self::for_zoom(zoom)?;
        let Some(prior) = prior else {
            return Some(target);
        };
        let prior_law = RELIEF_LAWS[usize::from(prior)];
        let retain = (target.0 > prior
            && zoom < f64::from(target.1.onset_zoom) + DETAIL_HYSTERESIS_ZOOM)
            || (target.0 < prior
                && zoom >= f64::from(prior_law.onset_zoom) - DETAIL_HYSTERESIS_ZOOM);
        if retain {
            Some((prior, prior_law))
        } else {
            Some(target)
        }
    }

    fn tolerance_world(self) -> f64 {
        if self.error_points <= 0.0 {
            0.0
        } else {
            self.error_points / (256.0 * f64::from(self.reference_zoom).exp2())
        }
    }
}

impl Relief {
    pub fn raise(ctx: &egui::Context, root: &Path) -> Result<Self> {
        let (events, worker) = spawn_forge(ctx, root)?;
        Ok(Self {
            corpus: VectorCorpus::mint(),
            field: Arc::new(Field::default()),
            revision: 0,
            visibility: None,
            transition: None,
            events,
            worker,
        })
    }

    /// Keep the last field visible while the replacement corpus is forged.
    pub fn retarget(&mut self, ctx: &egui::Context, root: &Path) -> Result<()> {
        let (events, worker) = spawn_forge(ctx, root)?;
        self.events = events;
        self.worker = worker;
        Ok(())
    }

    pub fn absorb(&mut self) {
        if let Ok(event) = self.events.try_recv() {
            match event {
                Ok(field) => {
                    self.corpus = VectorCorpus::mint();
                    self.field = Arc::new(field);
                    self.revision = self.revision.saturating_add(1);
                    self.visibility = None;
                    self.transition = None;
                }
                Err(err) => eprintln!("topography unavailable: {err:#}"),
            }
        }
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn paint(&mut self, painter: &Painter, frame: MapFramePlan, gaps: Arc<[VectorGap]>) {
        let Some((detail, _law)) = ReliefLaw::resolve(
            self.visibility.as_ref().map(|visibility| visibility.detail),
            frame.zoom.get(),
        ) else {
            return;
        };
        let unchanged = self
            .visibility
            .as_ref()
            .is_some_and(|visibility| visibility.frame == frame && visibility.detail == detail);
        if !unchanged {
            let patches = relief_patches(&self.field, detail, frame);
            let next = ReliefVisibility {
                frame,
                detail,
                patches,
            };
            if self
                .visibility
                .as_ref()
                .is_some_and(|visibility| visibility.detail != detail)
            {
                self.transition = self.visibility.clone().map(|prior| ReliefTransition {
                    prior,
                    begun: Instant::now(),
                });
            }
            self.visibility = Some(next);
        }
        if let Some(transition) = &mut self.transition
            && transition.prior.frame != frame
        {
            transition.prior.frame = frame;
            transition.prior.patches = relief_patches(&self.field, transition.prior.detail, frame);
        }
        let now = Instant::now();
        let maturity = self.transition.as_ref().map(|transition| {
            smooth_transition(
                now.saturating_duration_since(transition.begun)
                    .as_secs_f32()
                    / DETAIL_TRANSITION.as_secs_f32(),
            )
        });
        if maturity.is_some_and(|maturity| maturity >= 1.0) {
            self.transition = None;
        }
        let current = self
            .visibility
            .as_ref()
            .expect("relief visibility was just established");
        let mut patches = Vec::with_capacity(
            current.patches.len()
                + self
                    .transition
                    .as_ref()
                    .map_or(0, |transition| transition.prior.patches.len()),
        );
        if let Some(transition) = &self.transition {
            let opacity = 1.0 - maturity.unwrap_or(1.0);
            patches.extend(
                transition
                    .prior
                    .patches
                    .iter()
                    .cloned()
                    .map(|patch| patch.with_opacity(opacity)),
            );
            painter.ctx().request_repaint();
        }
        let opacity = maturity.unwrap_or(1.0);
        patches.extend(
            current
                .patches
                .iter()
                .cloned()
                .map(|patch| patch.with_opacity(opacity)),
        );
        if patches.is_empty() {
            return;
        }
        painter.add(egui_wgpu::Callback::new_paint_callback(
            frame.rect,
            VectorPaint {
                layer: VectorLayer::Relief,
                corpus: self.corpus,
                geometry: GeometryPass::Strokes,
                gaps,
                patches: patches.into(),
                prewarm: Arc::from([]),
                repaint: painter.ctx().clone(),
                center_world: frame.viewport.center,
                world_points: frame.world_points as f32,
                viewport_points: [frame.rect.width(), frame.rect.height()],
                view_zoom: frame.zoom.get() as f32,
                apparition_span: basemap::APPARITION_SPAN,
            },
        ));
    }

    pub fn annotations(
        &self,
        frame: MapFramePlan,
        cartography: CartographicPlan,
    ) -> Vec<annotation::LineLabel<'_>> {
        if cartography.zoom.get() < 12.25 {
            return Vec::new();
        }
        let visible = frame.world_bounds();
        visible_label_slots(&self.field.label_index, visible)
            .into_iter()
            .map(|slot| &self.field.isohypses[slot])
            .filter(|isohypse| intersects(visible, isohypse.bounds))
            .filter_map(|isohypse| {
                let label = isohypse.label.as_deref()?;
                Some(annotation::LineLabel {
                    path: &isohypse.points,
                    text: label,
                    rank: 1_100,
                    size: 11.4,
                    onset_zoom: 12.25,
                    ink: Color32::BLACK,
                    halo: None,
                    repeatable: true,
                    break_line: true,
                })
            })
            .collect()
    }
}

fn spawn_forge(
    ctx: &egui::Context,
    root: &Path,
) -> Result<(Receiver<Result<Field>>, thread::JoinHandle<()>)> {
    let (events_tx, events) = bounded(1);
    let root = root.to_owned();
    let wake = NativeWake::from_context(ctx);
    let worker = thread::Builder::new()
        .name("isohypse-forge".to_owned())
        .spawn(move || {
            let _sent = events_tx.send(load_or_forge(&root));
            let _woken = wake.request_foreground_repaint();
        })
        .context("spawn isohypse forge")?;
    Ok((events, worker))
}

fn relief_patches(field: &Field, detail: u8, frame: MapFramePlan) -> Arc<[VectorPatch]> {
    field.tiles.get(usize::from(detail)).map_or_else(
        || Arc::from([]),
        |tiles| visible_tiles(tiles, frame, detail),
    )
}

fn smooth_transition(phase: f32) -> f32 {
    let phase = phase.clamp(0.0, 1.0);
    phase * phase * 2.0_f32.mul_add(-phase, 3.0)
}

fn visible_tiles(tiles: &[Arc<VectorTile>], frame: MapFramePlan, detail: u8) -> Arc<[VectorPatch]> {
    let expansion = 2.0 / frame.world_points;
    let mut bounds = frame.world_bounds();
    bounds[0] -= expansion;
    bounds[1] -= expansion;
    bounds[2] += expansion;
    bounds[3] += expansion;
    tiles
        .iter()
        .filter(|tile| tile_intersects(bounds, tile.key))
        .cloned()
        .map(|tile| VectorPatch::whole(tile).stratum(detail))
        .collect()
}

fn tile_intersects(bounds: [f64; 4], key: TileKey) -> bool {
    let scale = f64::from(1_u32 << key.zoom);
    let west = f64::from(key.x) / scale;
    let east = f64::from(key.x + 1) / scale;
    let north = f64::from(key.y) / scale;
    let south = f64::from(key.y + 1) / scale;
    [-1.0, 0.0, 1.0]
        .into_iter()
        .any(|shift| intersects(bounds, [west + shift, north, east + shift, south]))
}

fn load_or_forge(root: &Path) -> Result<Field> {
    let Some(identity) = trailgen_data::indexed_topography_identity(root)? else {
        return Ok(Field::default());
    };
    let cache = root.join(CACHE);
    if let Ok(bytes) = fs::read(&cache)
        && let Ok(field) = decode(&bytes, &identity)
    {
        profile("decoded isohypse cache");
        return Ok(field);
    }
    let topography = trailgen_data::indexed_topography(root)?
        .context("topography disappeared while forging isohypses")?;
    profile("decoded topography");
    let field = forge(&topography)?;
    profile("forged isohypses");
    write_cache(&cache, &encode(&field, &identity)?)?;
    profile("cached isohypses");
    Ok(field)
}

struct Mosaic {
    zoom: u8,
    width: u32,
    height: u32,
    tiles: BTreeMap<(u32, u32), TopographicTile>,
}

impl Mosaic {
    fn raise(topography: &Topography) -> Result<Self> {
        let first = topography
            .tiles
            .first()
            .context("topography has no tiles")?;
        ensure!(
            topography.tiles.iter().all(|tile| {
                tile.id.z == first.id.z && tile.width == first.width && tile.height == first.height
            }),
            "topographic corpus mixes raster grids"
        );
        Ok(Self {
            zoom: first.id.z,
            width: first.width,
            height: first.height,
            tiles: topography
                .tiles
                .iter()
                .cloned()
                .map(|tile| ((tile.id.x, tile.id.y), tile))
                .collect(),
        })
    }

    fn elevation(&self, x: i64, y: i64) -> Option<f32> {
        let width = i64::from(self.width);
        let height = i64::from(self.height);
        let tile_x = u32::try_from(x.div_euclid(width)).ok()?;
        let tile_y = u32::try_from(y.div_euclid(height)).ok()?;
        self.tiles.get(&(tile_x, tile_y))?.elevation(
            u32::try_from(x.rem_euclid(width)).ok()?,
            u32::try_from(y.rem_euclid(height)).ok()?,
        )
    }

    fn world(&self, point: GridPoint) -> [f64; 2] {
        let divisions = f64::from(1_u32 << self.zoom);
        [
            (point.x + 0.5) / (divisions * f64::from(self.width)),
            (point.y + 0.5) / (divisions * f64::from(self.height)),
        ]
    }
}

#[derive(Clone, Copy, Debug)]
struct GridPoint {
    x: f64,
    y: f64,
}

#[derive(Clone, Copy)]
struct Segment {
    a: GridPoint,
    b: GridPoint,
}

fn forge(topography: &Topography) -> Result<Field> {
    let mosaic = Mosaic::raise(topography)?;
    let mut isohypses = Vec::new();
    for tile in mosaic.tiles.values() {
        let mut strata = BTreeMap::<i32, Vec<Segment>>::new();
        let origin_x = i64::from(tile.id.x) * i64::from(mosaic.width);
        let origin_y = i64::from(tile.id.y) * i64::from(mosaic.height);
        for local_y in 0..mosaic.height {
            for local_x in 0..mosaic.width {
                cut_cell(
                    &mosaic,
                    origin_x + i64::from(local_x),
                    origin_y + i64::from(local_y),
                    &mut strata,
                );
            }
        }
        reap_strata(
            &mosaic,
            TileKey {
                zoom: tile.id.z,
                x: tile.id.x,
                y: tile.id.y,
            },
            strata,
            &mut isohypses,
        );
        profile("reaped topographic tile");
    }
    Ok(Field::seal(isohypses))
}

fn profile(stage: &str) {
    if std::env::var_os("TRAILGEN_PROFILE_RELIEF").is_some() {
        let memory = fs::read_to_string("/proc/self/status")
            .ok()
            .map(|status| {
                status
                    .lines()
                    .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        eprintln!("relief {stage}: {memory}");
    }
}

fn reap_strata(
    mosaic: &Mosaic,
    key: TileKey,
    strata: BTreeMap<i32, Vec<Segment>>,
    isohypses: &mut Vec<Isohypse>,
) {
    for (elevation_m, segments) in strata {
        for mut chain in stitch(&segments) {
            smooth(&mut chain);
            let points = chain
                .into_iter()
                .map(|point| mosaic.world(point))
                .collect::<Arc<[_]>>();
            isohypses.push(Isohypse {
                key,
                elevation_m,
                label: (elevation_m % INDEX_INTERVAL_M == 0)
                    .then(|| Arc::from(format!("{elevation_m} m"))),
                bounds: bounds(&points),
                points,
            });
        }
    }
}

fn cut_cell(mosaic: &Mosaic, column: i64, row: i64, strata: &mut BTreeMap<i32, Vec<Segment>>) {
    let (Some(north_west), Some(north_east), Some(south_east), Some(south_west)) = (
        mosaic.elevation(column, row),
        mosaic.elevation(column + 1, row),
        mosaic.elevation(column + 1, row + 1),
        mosaic.elevation(column, row + 1),
    ) else {
        return;
    };
    let values = [north_west, north_east, south_east, south_west];
    if values
        .iter()
        .any(|value| !value.is_finite() || !(-150.0..=9_000.0).contains(value))
    {
        return;
    }
    let minimum = values.iter().copied().reduce(f32::min).unwrap_or_default();
    let maximum = values.iter().copied().reduce(f32::max).unwrap_or_default();
    let first = (minimum / INTERVAL_M as f32).ceil() as i32 * INTERVAL_M;
    let last = (maximum / INTERVAL_M as f32).floor() as i32 * INTERVAL_M;
    for elevation_m in (first..=last).step_by(INTERVAL_M as usize) {
        let level = elevation_m as f32;
        let case = values.iter().enumerate().fold(0_u8, |case, (slot, value)| {
            case | u8::from(*value >= level) << slot
        });
        if case == 0 || case == 15 {
            continue;
        }
        let corners = [
            GridPoint {
                x: column as f64,
                y: row as f64,
            },
            GridPoint {
                x: (column + 1) as f64,
                y: row as f64,
            },
            GridPoint {
                x: (column + 1) as f64,
                y: (row + 1) as f64,
            },
            GridPoint {
                x: column as f64,
                y: (row + 1) as f64,
            },
        ];
        let edges = [(0, 1), (1, 2), (2, 3), (3, 0)];
        let mut intersections = [(0_usize, GridPoint { x: 0.0, y: 0.0 }); 4];
        let mut intersection_count = 0;
        for (edge, (start, end)) in edges.into_iter().enumerate() {
            if crosses(values[start], values[end], level) {
                intersections[intersection_count] = (
                    edge,
                    interpolate(
                        corners[start],
                        corners[end],
                        values[start],
                        values[end],
                        level,
                    ),
                );
                intersection_count += 1;
            }
        }
        let segments = strata.entry(elevation_m).or_default();
        match &intersections[..intersection_count] {
            [(_, a), (_, b)] => segments.push(Segment { a: *a, b: *b }),
            [
                (top_edge, top),
                (right_edge, right),
                (_bottom_edge, bottom),
                (left_edge, left),
            ] => {
                let center_high = values.iter().sum::<f32>() * 0.25 >= level;
                let first_pair = match (case, center_high) {
                    (5, true) | (10, false) => (*top_edge, *right_edge),
                    _ => (*top_edge, *left_edge),
                };
                if first_pair == (*top_edge, *right_edge) {
                    segments.extend([
                        Segment { a: *top, b: *right },
                        Segment {
                            a: *bottom,
                            b: *left,
                        },
                    ]);
                } else {
                    segments.extend([
                        Segment { a: *top, b: *left },
                        Segment {
                            a: *right,
                            b: *bottom,
                        },
                    ]);
                }
            }
            _ => {}
        }
    }
}

fn crosses(a: f32, b: f32, level: f32) -> bool {
    (a < level && b >= level) || (b < level && a >= level)
}

fn interpolate(a: GridPoint, b: GridPoint, a_height: f32, b_height: f32, level: f32) -> GridPoint {
    let t = f64::from((level - a_height) / (b_height - a_height));
    GridPoint {
        x: (b.x - a.x).mul_add(t, a.x),
        y: (b.y - a.y).mul_add(t, a.y),
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Knot(i64, i64);

fn knot(point: GridPoint) -> Knot {
    Knot(
        (point.x * 1_000_000.0).round() as i64,
        (point.y * 1_000_000.0).round() as i64,
    )
}

fn stitch(segments: &[Segment]) -> Vec<Vec<GridPoint>> {
    let mut incidence = HashMap::<Knot, Incidence>::new();
    for (slot, segment) in segments.iter().enumerate() {
        incidence.entry(knot(segment.a)).or_default().push(slot);
        incidence.entry(knot(segment.b)).or_default().push(slot);
    }
    let mut unused = vec![true; segments.len()];
    let mut chains = Vec::new();
    for endpoints_only in [true, false] {
        for start in 0..segments.len() {
            if !unused[start] {
                continue;
            }
            let segment = segments[start];
            let a_open = incidence.get(&knot(segment.a)).is_some_and(Incidence::open);
            let b_open = incidence.get(&knot(segment.b)).is_some_and(Incidence::open);
            if endpoints_only && !a_open && !b_open {
                continue;
            }
            chains.push(trace(
                start,
                if a_open { segment.a } else { segment.b },
                segments,
                &incidence,
                &mut unused,
            ));
        }
    }
    chains
}

fn smooth(chain: &mut [GridPoint]) {
    let Some(last) = chain.len().checked_sub(1) else {
        return;
    };
    let closed = chain.len() >= 4 && knot(chain[0]) == knot(chain[last]);
    let unique = chain.len() - usize::from(closed);
    if unique < 3 {
        return;
    }
    let source = chain.to_vec();
    for slot in 0..unique {
        if !closed && (slot == 0 || slot + 1 == unique) {
            continue;
        }
        let prior = if slot == 0 { unique - 1 } else { slot - 1 };
        let next = (slot + 1) % unique;
        chain[slot] = GridPoint {
            x: (source[prior].x + source[next].x).mul_add(0.18, source[slot].x * 0.64),
            y: (source[prior].y + source[next].y).mul_add(0.18, source[slot].y * 0.64),
        };
    }
    if closed {
        chain[last] = chain[0];
    }
}

#[derive(Clone, Copy, Default)]
struct Incidence {
    slots: [usize; 4],
    len: u8,
}

impl Incidence {
    fn push(&mut self, slot: usize) {
        let len = usize::from(self.len);
        if len < self.slots.len() {
            self.slots[len] = slot;
            self.len += 1;
        }
    }

    const fn open(&self) -> bool {
        self.len == 1
    }

    fn next(&self, unused: &[bool]) -> Option<usize> {
        self.slots[..usize::from(self.len)]
            .iter()
            .copied()
            .find(|slot| unused[*slot])
    }
}

fn trace(
    start: usize,
    origin: GridPoint,
    segments: &[Segment],
    incidence: &HashMap<Knot, Incidence>,
    unused: &mut [bool],
) -> Vec<GridPoint> {
    let mut chain = vec![origin];
    let mut at = origin;
    let mut current = start;
    loop {
        unused[current] = false;
        let segment = segments[current];
        let next = if knot(segment.a) == knot(at) {
            segment.b
        } else {
            segment.a
        };
        chain.push(next);
        at = next;
        let Some(next_segment) = incidence
            .get(&knot(at))
            .and_then(|links| links.next(unused))
        else {
            break;
        };
        current = next_segment;
    }
    chain
}

fn bounds(points: &[[f64; 2]]) -> [f64; 4] {
    points.iter().fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |mut bounds, point| {
            bounds[0] = bounds[0].min(point[0]);
            bounds[1] = bounds[1].min(point[1]);
            bounds[2] = bounds[2].max(point[0]);
            bounds[3] = bounds[3].max(point[1]);
            bounds
        },
    )
}

const fn intersects(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

struct StrokeMesh {
    vertices: Vec<StrokePoint>,
    indices: Vec<u32>,
}

struct LabelIndex {
    cells: HashMap<TileKey, Arc<[usize]>>,
    roaming: Arc<[usize]>,
}

fn index_labels(isohypses: &[Isohypse]) -> LabelIndex {
    let scale = f64::from(1_u32 << LABEL_INDEX_ZOOM);
    let mut index = HashMap::<TileKey, Vec<usize>>::new();
    let mut roaming = Vec::new();
    for (slot, isohypse) in isohypses
        .iter()
        .enumerate()
        .filter(|(_, isohypse)| isohypse.label.is_some())
    {
        let left = (isohypse.bounds[0] * scale).floor() as i64;
        let right = (isohypse.bounds[2] * scale).floor() as i64;
        let top = (isohypse.bounds[1] * scale).floor().clamp(0.0, scale - 1.0) as u32;
        let bottom = (isohypse.bounds[3] * scale).floor().clamp(0.0, scale - 1.0) as u32;
        let width = right.saturating_sub(left).saturating_add(1);
        let height = i64::from(bottom.saturating_sub(top).saturating_add(1));
        if usize::try_from(width.saturating_mul(height)).unwrap_or(usize::MAX) > LABEL_CELL_CEILING
        {
            roaming.push(slot);
            continue;
        }
        for y in top..=bottom {
            for x in left..=right {
                index
                    .entry(TileKey {
                        zoom: LABEL_INDEX_ZOOM,
                        x: x.rem_euclid(scale as i64) as u32,
                        y,
                    })
                    .or_default()
                    .push(slot);
            }
        }
    }
    LabelIndex {
        cells: index
            .into_iter()
            .map(|(key, mut slots)| {
                slots.sort_unstable();
                slots.dedup();
                (key, slots.into())
            })
            .collect(),
        roaming: roaming.into(),
    }
}

fn visible_label_slots(index: &LabelIndex, bounds: [f64; 4]) -> Vec<usize> {
    let scale = f64::from(1_u32 << LABEL_INDEX_ZOOM);
    let left = (bounds[0] * scale).floor() as i64;
    let right = (bounds[2] * scale).floor() as i64;
    let top = (bounds[1] * scale).floor().max(0.0) as i64;
    let bottom = (bounds[3] * scale).floor().min(scale - 1.0).max(0.0) as i64;
    let width = right.saturating_sub(left).saturating_add(1);
    let height = bottom.saturating_sub(top).saturating_add(1);
    let cell_count = usize::try_from(width.saturating_mul(height)).unwrap_or(usize::MAX);
    let mut slots = index.roaming.iter().copied().collect::<HashSet<_>>();
    if cell_count <= index.cells.len().saturating_mul(2).max(64) {
        for y in top..=bottom {
            for x in left..=right {
                let key = TileKey {
                    zoom: LABEL_INDEX_ZOOM,
                    x: x.rem_euclid(scale as i64) as u32,
                    y: y as u32,
                };
                if let Some(indexed) = index.cells.get(&key) {
                    slots.extend(indexed.iter().copied());
                }
            }
        }
    } else {
        for (key, indexed) in &index.cells {
            if tile_intersects(bounds, *key) {
                slots.extend(indexed.iter().copied());
            }
        }
    }
    let mut slots = slots.into_iter().collect::<Vec<_>>();
    slots.sort_unstable();
    slots
}

fn raise_tiles(isohypses: &[Isohypse], law: ReliefLaw) -> Arc<[Arc<VectorTile>]> {
    let mut meshes = BTreeMap::<TileKey, StrokeMesh>::new();
    let tolerance = law.tolerance_world();
    for isohypse in isohypses {
        let indexed = isohypse.elevation_m % INDEX_INTERVAL_M == 0;
        if !indexed && !law.minors {
            continue;
        }
        let scale = f64::from(1_u32 << isohypse.key.zoom);
        let simplified = simplify_isohypse(&isohypse.points, tolerance);
        let mut points = Vec::with_capacity(simplified.len());
        for world in simplified {
            let point = [
                world[0].mul_add(scale, -f64::from(isohypse.key.x)) as f32,
                world[1].mul_add(scale, -f64::from(isohypse.key.y)) as f32,
            ];
            if points
                .last()
                .is_none_or(|prior| !basemap::same_point(*prior, point))
            {
                points.push(point);
            }
        }
        if points.len() < 2 {
            continue;
        }
        let mesh = meshes.entry(isohypse.key).or_insert_with(|| StrokeMesh {
            vertices: Vec::new(),
            indices: Vec::new(),
        });
        let Ok(base) = u32::try_from(mesh.vertices.len()) else {
            continue;
        };
        let (color, radius_points, onset_zoom) = if indexed {
            ([80, 67, 48, 92], map::INDEX_ISOHYPSE_RADIUS_POINTS, 10.5)
        } else {
            ([84, 73, 57, 31], 0.31, 11.75)
        };
        for (slot, point) in points.iter().copied().enumerate() {
            let extrusion = basemap::join_normal(&points, slot);
            mesh.vertices.extend([
                StrokePoint {
                    local: point,
                    extrusion: [-extrusion[0], -extrusion[1]],
                    srgb: color,
                    radius_points,
                    radius_world: 0.0,
                    onset_side: -(onset_zoom + 1.0),
                },
                StrokePoint {
                    local: point,
                    extrusion,
                    srgb: color,
                    radius_points,
                    radius_world: 0.0,
                    onset_side: onset_zoom + 1.0,
                },
            ]);
        }
        for slot in 0..points.len() - 1 {
            let Some(offset) = u32::try_from(slot)
                .ok()
                .and_then(|slot| slot.checked_mul(2))
            else {
                break;
            };
            let a = base + offset;
            mesh.indices.extend([a, a + 1, a + 2, a + 1, a + 3, a + 2]);
        }
    }
    meshes
        .into_iter()
        .map(|(key, mesh)| {
            Arc::new(VectorTile {
                key,
                fills: Mesh {
                    vertices: Arc::from([]),
                    indices: Arc::from([]),
                },
                strokes: Mesh {
                    vertices: mesh.vertices.into(),
                    indices: mesh.indices.into(),
                },
                labels: Arc::from([]),
                line_labels: Arc::from([]),
                parking: Arc::from([]),
            })
        })
        .collect()
}

fn simplify_isohypse(points: &[[f64; 2]], tolerance: f64) -> Vec<[f64; 2]> {
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
        let champion = (start + 1..end)
            .map(|slot| {
                (
                    slot,
                    segment_distance(points[slot], points[start], points[end]),
                )
            })
            .max_by(|left, right| left.1.total_cmp(&right.1));
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
    let squared = edge[0].mul_add(edge[0], edge[1] * edge[1]);
    if squared <= f64::EPSILON {
        return (point[0] - start[0]).hypot(point[1] - start[1]);
    }
    let offset = [point[0] - start[0], point[1] - start[1]];
    let progress = (offset[0].mul_add(edge[0], offset[1] * edge[1]) / squared).clamp(0.0, 1.0);
    (point[0] - edge[0].mul_add(progress, start[0]))
        .hypot(point[1] - edge[1].mul_add(progress, start[1]))
}

fn encode(field: &Field, identity: &str) -> Result<Vec<u8>> {
    let count = u32::try_from(field.isohypses.len()).context("too many isohypse chunks")?;
    let identity_len = u16::try_from(identity.len()).context("topography identity is too long")?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&identity_len.to_le_bytes());
    bytes.extend_from_slice(identity.as_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    for isohypse in &field.isohypses {
        bytes.push(isohypse.key.zoom);
        bytes.extend_from_slice(&isohypse.key.x.to_le_bytes());
        bytes.extend_from_slice(&isohypse.key.y.to_le_bytes());
        bytes.extend_from_slice(&isohypse.elevation_m.to_le_bytes());
        bytes.extend_from_slice(
            &u32::try_from(isohypse.points.len())
                .context("isohypse chunk is too long")?
                .to_le_bytes(),
        );
        for point in isohypse.points.iter() {
            bytes.extend_from_slice(&point[0].to_le_bytes());
            bytes.extend_from_slice(&point[1].to_le_bytes());
        }
    }
    Ok(bytes)
}

fn decode(bytes: &[u8], identity: &str) -> Result<Field> {
    let mut reader = Cursor::new(bytes);
    let mut magic = [0; 8];
    reader.read_exact(&mut magic)?;
    ensure!(&magic == MAGIC, "isohypse cache has the wrong magic");
    let identity_len = read_u16(&mut reader)? as usize;
    let mut cached_identity = vec![0; identity_len];
    reader.read_exact(&mut cached_identity)?;
    ensure!(
        cached_identity == identity.as_bytes(),
        "isohypse cache is stale"
    );
    let count = read_u32(&mut reader)? as usize;
    ensure!(count <= POINT_CEILING, "isohypse cache has too many chunks");
    let mut total_points = 0_usize;
    let mut isohypses = Vec::with_capacity(count);
    for _ in 0..count {
        let key = TileKey {
            zoom: read_u8(&mut reader)?,
            x: read_u32(&mut reader)?,
            y: read_u32(&mut reader)?,
        };
        let elevation_m = read_i32(&mut reader)?;
        let point_count = read_u32(&mut reader)? as usize;
        total_points = total_points.saturating_add(point_count);
        ensure!(
            point_count >= 2 && total_points <= POINT_CEILING,
            "isohypse cache has an unlawful point count"
        );
        let mut points = Vec::with_capacity(point_count);
        for _ in 0..point_count {
            points.push([read_f64(&mut reader)?, read_f64(&mut reader)?]);
        }
        let points = Arc::<[[f64; 2]]>::from(points);
        isohypses.push(Isohypse {
            key,
            elevation_m,
            label: (elevation_m % INDEX_INTERVAL_M == 0)
                .then(|| Arc::from(format!("{elevation_m} m"))),
            bounds: bounds(&points),
            points,
        });
    }
    ensure!(
        reader.position() as usize == bytes.len(),
        "isohypse cache has a tail"
    );
    Ok(Field::seal(isohypses))
}

fn write_cache(path: &Path, bytes: &[u8]) -> Result<()> {
    persistence::replace(path, bytes).with_context(|| format!("commit {}", path.display()))
}

fn read_u16(reader: &mut Cursor<&[u8]>) -> Result<u16> {
    let mut bytes = [0; 2];
    reader.read_exact(&mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u8(reader: &mut Cursor<&[u8]>) -> Result<u8> {
    let mut byte = [0];
    reader.read_exact(&mut byte)?;
    Ok(byte[0])
}

fn read_u32(reader: &mut Cursor<&[u8]>) -> Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_i32(reader: &mut Cursor<&[u8]>) -> Result<i32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(i32::from_le_bytes(bytes))
}

fn read_f64(reader: &mut Cursor<&[u8]>) -> Result<f64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(f64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stitch_joins_cell_fragments_into_one_isohypse() {
        let chains = stitch(&[
            Segment {
                a: GridPoint { x: 0.0, y: 0.5 },
                b: GridPoint { x: 1.0, y: 0.5 },
            },
            Segment {
                a: GridPoint { x: 2.0, y: 0.5 },
                b: GridPoint { x: 1.0, y: 0.5 },
            },
        ]);
        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].len(), 3);
    }

    #[test]
    fn cache_roundtrip_preserves_fixed_interval_semantics() -> Result<()> {
        let field = Field::seal(vec![Isohypse {
            key: TileKey {
                zoom: 0,
                x: 0,
                y: 0,
            },
            elevation_m: 250,
            label: Some(Arc::from("250 m")),
            points: Arc::from([[0.2, 0.3], [0.4, 0.5]]),
            bounds: [0.2, 0.3, 0.4, 0.5],
        }]);
        let decoded = decode(&encode(&field, "terrain")?, "terrain")?;
        assert_eq!(decoded.isohypses.len(), 1);
        assert_eq!(decoded.isohypses[0].elevation_m, 250);
        assert_eq!(decoded.isohypses[0].label.as_deref(), Some("250 m"));
        Ok(())
    }

    #[test]
    fn fine_view_submits_only_intersecting_relief_tiles() {
        let tile = |x| {
            Arc::new(VectorTile {
                key: TileKey {
                    zoom: 12,
                    x,
                    y: 1_532,
                },
                fills: Mesh {
                    vertices: Arc::from([]),
                    indices: Arc::from([]),
                },
                strokes: Mesh {
                    vertices: Arc::from([]),
                    indices: Arc::from([]),
                },
                labels: Arc::from([]),
                line_labels: Arc::from([]),
                parking: Arc::from([]),
            })
        };
        let tiles = [tile(1_203), tile(1_204), tile(1_205)];
        let viewport = map::Viewport {
            center: [(1_204.5) / 4_096.0, (1_532.5) / 4_096.0],
            zoom: 18.0,
        };
        let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1_120.0, 610.0));
        let visible = visible_tiles(&tiles, MapFramePlan::forge(viewport, rect), 0);
        assert_eq!(
            visible
                .iter()
                .map(|patch| patch.tile.key.x)
                .collect::<Vec<_>>(),
            [1_204]
        );

        let seam = visible_tiles(
            &tiles,
            MapFramePlan::forge(
                map::Viewport {
                    center: [1_205.0 / 4_096.0, viewport.center[1]],
                    ..viewport
                },
                rect,
            ),
            0,
        );
        assert_eq!(
            seam.iter()
                .map(|patch| patch.tile.key.x)
                .collect::<Vec<_>>(),
            [1_204, 1_205]
        );
    }
}
