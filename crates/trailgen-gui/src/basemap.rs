use crate::{
    habitat::platform_dirs,
    map::{self, MapFramePlan},
    persistence,
};
use anyhow::{Context as _, Result, ensure};
use bytemuck::{Pod, Zeroable};
use crossbeam_channel::{Receiver, Sender, bounded};
use egui::Context;
use eternalist_apps::NativeWake;
use fast_mvt::{MvtFeatureRef, MvtGeometry, MvtReaderRef, MvtValueRef};
use futures_util::{StreamExt as _, stream};
use geo_types::{Coord, LineString, Point, Polygon};
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex, VertexBuffers, math::point,
    path::Path,
};
use pmtiles::{
    AsyncPmTilesReader, Compression, HashMapCache, HttpBackend, MmapBackend, PmTilesWriter,
    TileCoord, TileId, TileType,
};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::{
    cmp::Reverse,
    collections::HashSet,
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime},
};
use trailgen_core::{WalkGraph, source::GeoBounds};

pub const ARCHIVE_NAME: &str = "basemap.pmtiles";
pub const MAX_SOURCE_ZOOM: u8 = 15;
pub const APPARITION_SPAN: f32 = 1.35;
const BUILDS_INDEX: &str = "https://build-metadata.protomaps.dev/builds.json";
const BUILDS_ORIGIN: &str = "https://build.protomaps.com";
const FORGE_BATCH: usize = 64;
const FORGE_CONCURRENCY: usize = 8;
const MAX_FORGE_TILES: usize = 8_192;
const NOMAD_BATCH: usize = 64;
const ROAMING_CACHE: &str = "protomaps-v4";
const ROAMING_CACHE_CEILING: u64 = 512 * 1_048_576;
const CANONICAL_TILE_POINTS: f64 = 256.0;
const PROMOTION_SPAN_POINTS: f64 = 640.0;
const DEMOTION_SPAN_POINTS: f64 = 512.0;
const PREFETCH_GUARD_ZOOM: f64 = 0.12;
const PREFETCH_MARGIN_CEILING: f64 = 0.50;
const MOTION_LEASE: Duration = Duration::from_millis(120);
const FALLBACK_DEPTH: u8 = 2;
const WORKERS: usize = 8;
const REQUIRED_APRON: i64 = 1;

#[derive(Clone, Debug)]
pub struct Source {
    archive: PathBuf,
    forge_regions: Option<Vec<GeoBounds>>,
    forge_zoom: u8,
    roaming_cache: Option<PathBuf>,
}

impl Source {
    pub fn project(root: &FsPath, graph: &WalkGraph, regions: &[GeoBounds]) -> Result<Self> {
        if !regions.is_empty() {
            return Self::regions(root, regions);
        }
        if let Some(override_path) = std::env::var_os("TRAILGEN_BASEMAP_ARCHIVE") {
            return Ok(Self {
                archive: override_path.into(),
                forge_regions: None,
                forge_zoom: MAX_SOURCE_ZOOM,
                roaming_cache: None,
            });
        }
        let roaming_cache = platform_dirs()?.cache_dir().join(ROAMING_CACHE);
        Ok(Self {
            archive: root.join("cache").join(ARCHIVE_NAME),
            forge_regions: Some(vec![forge_bounds(graph)?]),
            forge_zoom: MAX_SOURCE_ZOOM,
            roaming_cache: Some(roaming_cache),
        })
    }

    /// Build a managed project's basemap source without loading its trail graph.
    pub fn regions(root: &FsPath, regions: &[GeoBounds]) -> Result<Self> {
        ensure!(!regions.is_empty(), "a managed basemap requires a map area");
        if let Some(override_path) = std::env::var_os("TRAILGEN_BASEMAP_ARCHIVE") {
            return Ok(Self {
                archive: override_path.into(),
                forge_regions: None,
                forge_zoom: MAX_SOURCE_ZOOM,
                roaming_cache: None,
            });
        }
        Ok(Self {
            archive: root
                .join("cache")
                .join(format!("basemap-{}.pmtiles", region_key(regions))),
            forge_regions: Some(regions.to_vec()),
            forge_zoom: MAX_SOURCE_ZOOM,
            roaming_cache: Some(platform_dirs()?.cache_dir().join(ROAMING_CACHE)),
        })
    }

    pub fn bootstrap() -> Result<Self> {
        let roaming_cache = platform_dirs()?.cache_dir().join(ROAMING_CACHE);
        if let Some(override_path) = std::env::var_os("TRAILGEN_BASEMAP_ARCHIVE") {
            return Ok(Self {
                archive: override_path.into(),
                forge_regions: None,
                forge_zoom: MAX_SOURCE_ZOOM,
                roaming_cache: None,
            });
        }
        Ok(Self {
            archive: roaming_cache.join("bootstrap-us.pmtiles"),
            forge_regions: Some(vec![GeoBounds::new(-125.0, 24.0, -66.0, 50.0)]),
            forge_zoom: 4,
            roaming_cache: Some(roaming_cache),
        })
    }
}

pub fn apparition(view_zoom: f32, onset_zoom: f32) -> f32 {
    let phase = ((view_zoom - onset_zoom) / APPARITION_SPAN).clamp(0.0, 1.0);
    phase * phase * 2.0_f32.mul_add(-phase, 3.0)
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TileKey {
    pub zoom: u8,
    pub x: u32,
    pub y: u32,
}

impl TileKey {
    fn coordinate(self) -> Result<TileCoord> {
        TileCoord::new(self.zoom, self.x, self.y).context("invalid PMTiles coordinate")
    }

    #[must_use]
    pub fn ancestor(self, level: SourceLevel) -> Self {
        assert!(level.get() <= self.zoom, "a tile ancestor cannot be finer");
        let shift = u32::from(self.zoom - level.get());
        Self {
            zoom: level.get(),
            x: self.x >> shift,
            y: self.y >> shift,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SourceLevel(u8);

impl SourceLevel {
    #[must_use]
    pub const fn new(level: u8) -> Self {
        assert!(level <= MAX_SOURCE_ZOOM, "source level exceeds archive law");
        Self(level)
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    #[must_use]
    const fn saturating_sub(self, depth: u8) -> Self {
        Self(self.0.saturating_sub(depth))
    }

    #[must_use]
    fn successor(self) -> Self {
        Self(self.0.saturating_add(1).min(MAX_SOURCE_ZOOM))
    }

    fn tile_span(self, view_zoom: f64) -> f64 {
        CANONICAL_TILE_POINTS * (view_zoom - f64::from(self.0)).exp2()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DetailPlan {
    pub source: SourceLevel,
    pub prefetch: bool,
}

#[derive(Debug)]
pub struct DetailGovernor {
    source: Option<SourceLevel>,
    prior: Option<(f64, Instant)>,
    last_motion: Option<Instant>,
    zooming_in: bool,
    zoom_rate: f64,
}

impl Default for DetailGovernor {
    fn default() -> Self {
        Self {
            source: None,
            prior: None,
            last_motion: None,
            zooming_in: false,
            zoom_rate: 0.0,
        }
    }
}

impl DetailGovernor {
    pub fn resolve(&mut self, view_zoom: f64, ready_latency: Duration, now: Instant) -> DetailPlan {
        self.observe_motion(view_zoom, now);
        let mut source = self
            .source
            .unwrap_or_else(|| coarsest_adequate_source(view_zoom));
        while source.get() < MAX_SOURCE_ZOOM && source.tile_span(view_zoom) > PROMOTION_SPAN_POINTS
        {
            source = source.successor();
        }
        while source.get() > 0 {
            let parent = source.saturating_sub(1);
            if parent.tile_span(view_zoom) >= DEMOTION_SPAN_POINTS {
                break;
            }
            source = parent;
        }
        self.source = Some(source);
        let remaining = (PROMOTION_SPAN_POINTS / source.tile_span(view_zoom))
            .log2()
            .max(0.0);
        let margin = self
            .zoom_rate
            .mul_add(ready_latency.as_secs_f64(), PREFETCH_GUARD_ZOOM)
            .min(PREFETCH_MARGIN_CEILING);
        DetailPlan {
            source,
            prefetch: self.zooming_in && source.get() < MAX_SOURCE_ZOOM && remaining <= margin,
        }
    }

    fn observe_motion(&mut self, view_zoom: f64, now: Instant) {
        if let Some((prior_zoom, prior_at)) = self.prior {
            let delta = view_zoom - prior_zoom;
            if delta.abs() > f64::EPSILON {
                self.zooming_in = delta > 0.0;
                let elapsed = now.saturating_duration_since(prior_at).as_secs_f64();
                self.zoom_rate = if self.zooming_in && elapsed > 0.0 {
                    (delta / elapsed).clamp(0.0, 32.0)
                } else {
                    0.0
                };
                self.prior = Some((view_zoom, now));
                self.last_motion = Some(now);
            } else if self
                .last_motion
                .is_none_or(|motion| now.saturating_duration_since(motion) > MOTION_LEASE)
            {
                self.zooming_in = false;
                self.zoom_rate = 0.0;
            }
        } else {
            self.prior = Some((view_zoom, now));
        }
    }
}

fn coarsest_adequate_source(view_zoom: f64) -> SourceLevel {
    (0..=MAX_SOURCE_ZOOM)
        .map(SourceLevel)
        .find(|source| source.tile_span(view_zoom) <= PROMOTION_SPAN_POINTS)
        .unwrap_or(SourceLevel(MAX_SOURCE_ZOOM))
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TileCell {
    pub key: TileKey,
    wrap: i32,
}

impl TileCell {
    #[must_use]
    pub fn world_bounds(self) -> [f64; 4] {
        let scale = f64::from(1_u32 << self.key.zoom);
        let west = f64::from(self.key.x) / scale + f64::from(self.wrap);
        [
            west,
            f64::from(self.key.y) / scale,
            west + 1.0 / scale,
            f64::from(self.key.y + 1) / scale,
        ]
    }

    #[must_use]
    pub fn contains(self, world: [f64; 2]) -> bool {
        let bounds = self.world_bounds();
        let x = world[0] + f64::from(self.wrap);
        x >= bounds[0] && x < bounds[2] && world[1] >= bounds[1] && world[1] < bounds[3]
    }
}

#[derive(Debug)]
pub struct Cover {
    pub source: SourceLevel,
    pub cells: Vec<TileCell>,
    pub strata: Vec<Stratum>,
}

impl Cover {
    pub fn demand_order(&self) -> Vec<TileKey> {
        let mut ordered = Vec::new();
        for intent in [
            Intent::Fallback,
            Intent::Required,
            Intent::Wayfinding,
            Intent::Prefetch,
        ] {
            ordered.extend(
                self.strata
                    .iter()
                    .filter(|stratum| stratum.intent == intent)
                    .flat_map(|stratum| stratum.keys.iter().copied()),
            );
        }
        ordered
    }
}

#[derive(Debug)]
pub struct Stratum {
    pub intent: Intent,
    pub keys: Vec<TileKey>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Intent {
    Fallback,
    Required,
    Wayfinding,
    Prefetch,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct FillPoint {
    pub local: [f32; 2],
    pub srgb: [u8; 4],
    pub onset_zoom: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct StrokePoint {
    pub local: [f32; 2],
    pub extrusion: [f32; 2],
    pub srgb: [u8; 4],
    pub radius_points: f32,
    pub radius_world: f32,
    /// Magnitude is `onset_zoom + 1`; sign selects the extrusion bank.
    pub onset_side: f32,
}

#[derive(Clone, Debug)]
pub struct Label {
    pub world: [f64; 2],
    pub text: Arc<str>,
    pub kind: LabelKind,
    pub rank: u16,
    pub size: f32,
    pub onset_zoom: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabelKind {
    Place,
    Lake,
    Peak,
}

#[derive(Clone, Debug)]
pub struct LineLabel {
    pub path: Arc<[[f64; 2]]>,
    pub text: Arc<str>,
    pub rank: u16,
    pub size: f32,
    pub onset_zoom: f32,
}

#[derive(Clone, Debug)]
pub struct Parking {
    pub world: [f64; 2],
    pub name: Option<Arc<str>>,
    pub onset_zoom: f32,
}

#[derive(Clone, Debug)]
pub struct Mesh<V> {
    pub vertices: Arc<[V]>,
    pub indices: Arc<[u32]>,
}

impl<V> Default for Mesh<V> {
    fn default() -> Self {
        Self {
            vertices: Arc::from([]),
            indices: Arc::from([]),
        }
    }
}

#[derive(Clone, Debug)]
pub struct VectorTile {
    pub key: TileKey,
    pub fills: Mesh<FillPoint>,
    pub strokes: Mesh<StrokePoint>,
    pub labels: Arc<[Label]>,
    pub line_labels: Arc<[LineLabel]>,
    pub parking: Arc<[Parking]>,
}

impl VectorTile {
    pub fn resident_bytes(&self) -> usize {
        self.fills
            .vertices
            .len()
            .saturating_mul(size_of::<FillPoint>())
            .saturating_add(self.fills.indices.len().saturating_mul(size_of::<u32>()))
            .saturating_add(
                self.strokes
                    .vertices
                    .len()
                    .saturating_mul(size_of::<StrokePoint>()),
            )
            .saturating_add(self.strokes.indices.len().saturating_mul(size_of::<u32>()))
            .saturating_add(self.labels.len().saturating_mul(size_of::<Label>()))
            .saturating_add(
                self.labels
                    .iter()
                    .map(|label| label.text.len())
                    .sum::<usize>(),
            )
            .saturating_add(
                self.line_labels
                    .len()
                    .saturating_mul(size_of::<LineLabel>()),
            )
            .saturating_add(
                self.line_labels
                    .iter()
                    .map(|label| {
                        label
                            .path
                            .len()
                            .saturating_mul(size_of::<[f64; 2]>())
                            .saturating_add(label.text.len())
                    })
                    .sum::<usize>(),
            )
            .saturating_add(self.parking.len().saturating_mul(size_of::<Parking>()))
            .saturating_add(
                self.parking
                    .iter()
                    .filter_map(|parking| parking.name.as_ref())
                    .map(|name| name.len())
                    .sum::<usize>(),
            )
    }
}

#[derive(Debug)]
pub enum Event {
    Ready {
        source_zoom: u8,
    },
    Relinquished(Vec<TileKey>),
    Loaded(Arc<VectorTile>),
    Missing(TileKey),
    Fault {
        key: Option<TileKey>,
        message: String,
    },
}

pub struct Basemap {
    commands: Sender<TileKey>,
    pending: Receiver<TileKey>,
    pub events: Receiver<Event>,
    _thread: thread::JoinHandle<()>,
}

impl Basemap {
    pub fn spawn(ctx: &Context, source: Source, online: bool) -> Result<Self> {
        Self::spawn_with_workers(ctx, source, online, WORKERS)
    }

    fn spawn_with_workers(
        ctx: &Context,
        source: Source,
        online: bool,
        workers: usize,
    ) -> Result<Self> {
        purge_partials(&source.archive)?;
        let (commands, command_rx) = bounded(workers.max(1).saturating_mul(2));
        let pending = command_rx.clone();
        let (event_tx, events) = bounded(256);
        let wake = NativeWake::from_context(ctx);
        let thread = thread::Builder::new()
            .name("vector-armory".to_owned())
            .spawn(move || armory(&wake, &source, command_rx, event_tx, online, workers))
            .context("spawn vector basemap armory")?;
        Ok(Self {
            commands,
            pending,
            events,
            _thread: thread,
        })
    }

    pub fn request(&self, key: TileKey) -> bool {
        self.commands.try_send(key).is_ok()
    }

    pub fn preempt(&self) -> Vec<TileKey> {
        self.pending.try_iter().collect()
    }
}

pub fn cover(
    frame: MapFramePlan,
    detail: DetailPlan,
    archive_zoom: Option<u8>,
    with_wayfinding: bool,
) -> Cover {
    let source = detail.source;
    let fallback = archive_zoom
        .filter(|archive_zoom| *archive_zoom < source.get())
        .map_or_else(|| source.saturating_sub(FALLBACK_DEPTH), SourceLevel::new);
    let mut strata = (fallback != source)
        .then(|| Stratum {
            intent: Intent::Fallback,
            keys: keys_at(frame, fallback, 0),
        })
        .into_iter()
        .collect::<Vec<_>>();
    strata.push(Stratum {
        intent: Intent::Required,
        keys: keys_at(frame, source, REQUIRED_APRON),
    });
    if detail.prefetch && source.get() < MAX_SOURCE_ZOOM {
        strata.push(Stratum {
            intent: Intent::Prefetch,
            keys: keys_at(frame, source.successor(), 0),
        });
    }
    if with_wayfinding
        && archive_zoom.is_some_and(|zoom| zoom >= TRAILHEAD_PARKING_SOURCE_ZOOM)
        && source.get() < TRAILHEAD_PARKING_SOURCE_ZOOM
        && frame.zoom.get() >= f64::from(TRAILHEAD_PARKING_ONSET_ZOOM) + TRAILHEAD_PARKING_FETCH_LAG
    {
        strata.push(Stratum {
            intent: Intent::Wayfinding,
            keys: keys_at(frame, SourceLevel::new(TRAILHEAD_PARKING_SOURCE_ZOOM), 0),
        });
    }
    Cover {
        source,
        cells: cells_at(frame, source, 0),
        strata,
    }
}

fn keys_at(frame: MapFramePlan, level: SourceLevel, apron: i64) -> Vec<TileKey> {
    let mut keys = cells_at(frame, level, apron)
        .into_iter()
        .map(|cell| cell.key)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys.dedup();
    keys.sort_unstable_by(|left, right| {
        tile_distance(*left, frame.viewport.center)
            .total_cmp(&tile_distance(*right, frame.viewport.center))
    });
    keys
}

fn cells_at(frame: MapFramePlan, level: SourceLevel, apron: i64) -> Vec<TileCell> {
    let divisions = 1_u32 << level.get();
    let bounds = frame.world_bounds();
    let scale = f64::from(divisions);
    let left = (bounds[0] * scale).floor() as i64 - apron;
    let right = (bounds[2] * scale).floor() as i64 + apron;
    let top = ((bounds[1] * scale).floor() as i64 - apron).max(0);
    let bottom = (bounds[3] * scale)
        .floor()
        .min(f64::from(divisions.saturating_sub(1))) as i64
        + apron;
    let bottom = bottom.min(i64::from(divisions.saturating_sub(1)));
    let mut cells = Vec::new();
    for raw_y in top..=bottom {
        for raw_x in left..=right {
            cells.push(TileCell {
                key: TileKey {
                    zoom: level.get(),
                    x: raw_x.rem_euclid(i64::from(divisions)) as u32,
                    y: raw_y as u32,
                },
                wrap: i32::try_from(raw_x.div_euclid(i64::from(divisions)))
                    .expect("viewport wrap fits i32"),
            });
        }
    }
    cells.sort_unstable_by(|left, right| {
        tile_distance(left.key, frame.viewport.center)
            .total_cmp(&tile_distance(right.key, frame.viewport.center))
    });
    cells
}

fn tile_distance(key: TileKey, center: [f64; 2]) -> f64 {
    let scale = f64::from(1_u32 << key.zoom);
    let x = (f64::from(key.x) + 0.5) / scale;
    let y = (f64::from(key.y) + 0.5) / scale;
    (x - center[0]).mul_add(x - center[0], (y - center[1]).powi(2))
}

type Archive = AsyncPmTilesReader<MmapBackend, HashMapCache>;
type RemoteArchive = AsyncPmTilesReader<HttpBackend, HashMapCache>;

fn armory(
    wake: &NativeWake,
    source: &Source,
    commands: Receiver<TileKey>,
    events: Sender<Event>,
    online: bool,
    worker_count: usize,
) {
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(err) => {
            send_fault(wake, &events, None, &err);
            return;
        }
    };
    if let Err(err) = materialize_archive(&runtime, source, online) {
        send_fault(wake, &events, None, &err);
        return;
    }
    if let Err(err) = reap_region_archives(&source.archive) {
        send_fault(wake, &events, None, &err);
        return;
    }
    let reader = match runtime.block_on(Archive::new_with_cached_path(
        HashMapCache::default(),
        &source.archive,
    )) {
        Ok(reader) => Arc::new(reader),
        Err(err) => {
            send_fault(
                wake,
                &events,
                None,
                &anyhow::Error::new(err).context(format!("open {}", source.archive.display())),
            );
            return;
        }
    };
    let source_zoom = reader.get_header().max_zoom.min(MAX_SOURCE_ZOOM);
    if events.send(Event::Ready { source_zoom }).is_err() {
        return;
    }
    let _woken = wake.request_foreground_repaint();
    let (roaming_tx, roaming_rx) = bounded(256);
    let nomad = source
        .roaming_cache
        .as_deref()
        .filter(|_| online)
        .and_then(|cache| spawn_nomad(wake, cache, roaming_rx.clone(), &events));
    drop(roaming_rx);
    let mut workers = Vec::with_capacity(worker_count);
    for slot in 0..worker_count {
        let worker_wake = wake.clone();
        let reader = reader.clone();
        let commands = commands.clone();
        let worker_events = events.clone();
        let worker_roaming = roaming_tx.clone();
        let worker = thread::Builder::new()
            .name(format!("vector-quarry-{slot}"))
            .spawn(move || {
                quarry(
                    &worker_wake,
                    &reader,
                    source_zoom,
                    &commands,
                    &worker_roaming,
                    &worker_events,
                );
            });
        match worker {
            Ok(worker) => workers.push(worker),
            Err(err) => {
                send_fault(
                    wake,
                    &events,
                    None,
                    &anyhow::Error::new(err).context("spawn vector quarry"),
                );
                break;
            }
        }
    }
    drop(commands);
    drop(roaming_tx);
    for worker in workers {
        let _joined = worker.join();
    }
    if let Some(nomad) = nomad {
        let _joined = nomad.join();
    }
    drop(events);
}

fn materialize_archive(
    runtime: &tokio::runtime::Runtime,
    source: &Source,
    online: bool,
) -> Result<()> {
    if source.archive.is_file() {
        return Ok(());
    }
    ensure!(
        online,
        "cached basemap {} is unavailable while offline",
        source.archive.display()
    );
    let regions = source.forge_regions.as_deref().with_context(|| {
        format!(
            "basemap override {} does not exist",
            source.archive.display()
        )
    })?;
    forge_archive(runtime, &source.archive, regions, source.forge_zoom)
}

fn spawn_nomad(
    wake: &NativeWake,
    cache: &FsPath,
    commands: Receiver<TileKey>,
    events: &Sender<Event>,
) -> Option<thread::JoinHandle<()>> {
    let nomad_wake = wake.clone();
    let nomad_events = events.clone();
    let roaming_cache = cache.to_owned();
    thread::Builder::new()
        .name("vector-nomad".to_owned())
        .spawn(move || nomad(&nomad_wake, &roaming_cache, &commands, &nomad_events))
        .map_err(|err| {
            send_fault(
                wake,
                events,
                None,
                &anyhow::Error::new(err).context("spawn roaming vector quarry"),
            );
        })
        .ok()
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build basemap runtime")
}

#[derive(Deserialize)]
struct Build {
    key: String,
}

struct RemoteSource {
    archive: RemoteArchive,
    metadata: String,
    tile_type: TileType,
    compression: Compression,
    max_zoom: u8,
}

fn forge_archive(
    runtime: &tokio::runtime::Runtime,
    target: &FsPath,
    regions: &[GeoBounds],
    zoom: u8,
) -> Result<()> {
    let parent = target.parent().context("basemap archive has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create basemap cache {}", parent.display()))?;
    let remote = runtime.block_on(open_remote())?;
    let requested_zoom = remote.max_zoom.min(zoom).min(MAX_SOURCE_ZOOM);
    let bounds = region_hull(regions).context("basemap cut has no regions")?;
    let (max_zoom, tiles) = bounded_extraction(regions, requested_zoom, MAX_FORGE_TILES)?;
    let mut staging = persistence::AtomicReplacement::raise(target)?;
    let center = [
        (bounds.west + bounds.east) * 0.5,
        (bounds.south + bounds.north) * 0.5,
    ];
    let mut writer = PmTilesWriter::new(remote.tile_type)
        .tile_compression(remote.compression)
        .min_zoom(0)
        .max_zoom(max_zoom)
        .bounds(bounds.west, bounds.south, bounds.east, bounds.north)
        .center(center[0], center[1])
        .center_zoom(max_zoom.saturating_sub(2))
        .metadata(&remote.metadata)
        .create(staging.take_file()?)
        .context("raise project basemap writer")?;
    for keys in tiles.chunks(FORGE_BATCH) {
        for (coordinate, bytes) in runtime.block_on(fetch_tiles(&remote.archive, keys))? {
            if let Some(bytes) = bytes {
                writer
                    .add_raw_tile(coordinate, &bytes)
                    .context("write project basemap tile")?;
            }
        }
    }
    writer.finalize().context("seal project basemap")?;
    staging.commit()
}

fn region_hull(regions: &[GeoBounds]) -> Option<GeoBounds> {
    regions.iter().copied().reduce(|left, right| {
        GeoBounds::new(
            left.west.min(right.west),
            left.south.min(right.south),
            left.east.max(right.east),
            left.north.max(right.north),
        )
    })
}

fn region_key(regions: &[GeoBounds]) -> String {
    let mut law = String::from("protomaps-v4:");
    let mut canonical = regions.to_vec();
    canonical.sort_by(|left, right| {
        left.west
            .total_cmp(&right.west)
            .then_with(|| left.south.total_cmp(&right.south))
            .then_with(|| left.east.total_cmp(&right.east))
            .then_with(|| left.north.total_cmp(&right.north))
    });
    for region in canonical {
        write!(
            law,
            "{:.8},{:.8},{:.8},{:.8};",
            region.west, region.south, region.east, region.north
        )
        .expect("write region law");
    }
    let digest = Sha256::digest(law.as_bytes());
    digest[..8]
        .iter()
        .fold(String::with_capacity(16), |mut key, byte| {
            write!(key, "{byte:02x}").expect("write region key");
            key
        })
}

async fn open_remote() -> Result<RemoteSource> {
    let client = pmtiles::reqwest::Client::builder()
        .use_rustls_tls()
        .user_agent(concat!("trailgen/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build Protomaps client")?;
    let response = client
        .get(BUILDS_INDEX)
        .send()
        .await
        .context("fetch Protomaps build index")?
        .error_for_status()
        .context("Protomaps build index rejected request")?;
    let builds: Vec<Build> = serde_json::from_slice(
        &response
            .bytes()
            .await
            .context("read Protomaps build index")?,
    )
    .context("parse Protomaps build index")?;
    let key = builds
        .into_iter()
        .map(|build| build.key)
        .filter(|key| valid_build_key(key))
        .max()
        .context("Protomaps build index contains no daily PMTiles archive")?;
    let origin = format!("{BUILDS_ORIGIN}/{key}");
    let remote = RemoteArchive::new_with_cached_url(HashMapCache::default(), client, &origin)
        .await
        .with_context(|| format!("open Protomaps build {key}"))?;
    let header = remote.get_header();
    ensure!(
        header.tile_type == TileType::Mvt,
        "Protomaps build is not an MVT archive"
    );
    let tile_type = header.tile_type;
    let compression = header.tile_compression;
    let max_zoom = header.max_zoom;
    let metadata = remote
        .get_metadata()
        .await
        .context("read Protomaps metadata")?;
    Ok(RemoteSource {
        archive: remote,
        metadata,
        tile_type,
        compression,
        max_zoom,
    })
}

async fn fetch_tiles(
    remote: &RemoteArchive,
    keys: &[TileKey],
) -> Result<Vec<(TileCoord, Option<Vec<u8>>)>> {
    let requests = stream::iter(keys.iter().copied().map(|key| async move {
        let coordinate = key.coordinate()?;
        let bytes = remote
            .get_tile(coordinate)
            .await
            .map_err(anyhow::Error::new)
            .with_context(|| format!("fetch Protomaps tile {key:?}"))?;
        Ok::<_, anyhow::Error>((coordinate, bytes.map(|bytes| bytes.to_vec())))
    }))
    .buffered(FORGE_CONCURRENCY);
    futures_util::pin_mut!(requests);
    let mut tiles = Vec::with_capacity(keys.len());
    while let Some(result) = requests.next().await {
        tiles.push(result?);
    }
    Ok(tiles)
}

fn nomad(wake: &NativeWake, cache: &FsPath, commands: &Receiver<TileKey>, events: &Sender<Event>) {
    let mut resident = reap_roaming_cache(cache, ROAMING_CACHE_CEILING).unwrap_or_else(|err| {
        eprintln!("could not reap roaming basemap cache: {err:#}");
        0
    });
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(err) => {
            send_fault(wake, events, None, &err);
            return;
        }
    };
    let mut remote = None;
    while let Ok(first) = commands.recv() {
        let (batch, relinquished) =
            latest_nomad_batch(std::iter::once(first).chain(commands.try_iter()));
        if !relinquished.is_empty() && events.send(Event::Relinquished(relinquished)).is_err() {
            return;
        }
        let mut absent = Vec::new();
        for key in batch {
            let path = roaming_tile_path(cache, key);
            match fs::read(&path) {
                Ok(bytes) => {
                    let event = cut_event(key, &bytes);
                    if matches!(event, Event::Loaded(_)) {
                        if events.send(event).is_err() {
                            return;
                        }
                        let _woken = wake.request_foreground_repaint();
                    } else {
                        let _discarded = fs::remove_file(path);
                        absent.push(key);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => absent.push(key),
                Err(_) => absent.push(key),
            }
        }
        if absent.is_empty() {
            continue;
        }
        if remote.is_none() {
            match runtime.block_on(open_remote()) {
                Ok(source) => remote = Some(source),
                Err(err) => {
                    for key in absent {
                        send_fault(wake, events, Some(key), &err);
                    }
                    continue;
                }
            }
        }
        let Some(source) = remote.as_ref() else {
            continue;
        };
        for cut in runtime.block_on(fetch_roaming_tiles(&source.archive, &absent)) {
            let event = match cut.result {
                Ok(Some(bytes)) => {
                    if let Err(err) = cache_roaming_tile(cache, cut.key, &bytes) {
                        eprintln!("could not cache roaming vector tile {:?}: {err:#}", cut.key);
                    } else {
                        resident = resident.saturating_add(bytes.len() as u64);
                        if resident > ROAMING_CACHE_CEILING {
                            resident = reap_roaming_cache(cache, ROAMING_CACHE_CEILING)
                                .unwrap_or_else(|err| {
                                    eprintln!("could not reap roaming basemap cache: {err:#}");
                                    resident
                                });
                        }
                    }
                    cut_event(cut.key, &bytes)
                }
                Ok(None) => Event::Missing(cut.key),
                Err(err) => Event::Fault {
                    key: Some(cut.key),
                    message: format!("fetch roaming vector tile {:?}: {err:#}", cut.key),
                },
            };
            if events.send(event).is_err() {
                return;
            }
            let _woken = wake.request_foreground_repaint();
        }
    }
}

fn latest_nomad_batch(keys: impl IntoIterator<Item = TileKey>) -> (Vec<TileKey>, Vec<TileKey>) {
    let queued = keys.into_iter().collect::<Vec<_>>();
    let mut distinct = HashSet::with_capacity(queued.len());
    let mut batch = queued
        .iter()
        .rev()
        .copied()
        .filter(|key| distinct.insert(*key))
        .take(NOMAD_BATCH)
        .collect::<Vec<_>>();
    batch.sort_unstable_by_key(|key| Reverse(key.zoom));
    let retained = batch.iter().copied().collect::<HashSet<_>>();
    distinct.clear();
    let relinquished = queued
        .into_iter()
        .filter(|key| !retained.contains(key) && distinct.insert(*key))
        .collect();
    (batch, relinquished)
}

struct RoamingCut {
    key: TileKey,
    result: Result<Option<Vec<u8>>>,
}

async fn fetch_roaming_tiles(remote: &RemoteArchive, keys: &[TileKey]) -> Vec<RoamingCut> {
    let requests = stream::iter(keys.iter().copied().map(|key| async move {
        let result = match key.coordinate() {
            Ok(coordinate) => remote
                .get_tile_decompressed(coordinate)
                .await
                .map_err(anyhow::Error::new)
                .with_context(|| format!("fetch Protomaps tile {key:?}"))
                .map(|bytes| bytes.map(|bytes| bytes.to_vec())),
            Err(err) => Err(err),
        };
        RoamingCut { key, result }
    }))
    .buffered(FORGE_CONCURRENCY);
    futures_util::pin_mut!(requests);
    let mut cuts = Vec::with_capacity(keys.len());
    while let Some(cut) = requests.next().await {
        cuts.push(cut);
    }
    cuts
}

fn cut_event(key: TileKey, bytes: &[u8]) -> Event {
    static PROFILE: OnceLock<bool> = OnceLock::new();
    let started = PROFILE
        .get_or_init(|| std::env::var_os("TRAILGEN_PROFILE_BASEMAP").is_some())
        .then(Instant::now);
    match decode_tile(key, bytes) {
        Ok(tile) => {
            if let Some(started) = started {
                eprintln!(
                    "vector-decode key={}/{}/{} input_bytes={} resident_bytes={} elapsed_us={}",
                    key.zoom,
                    key.x,
                    key.y,
                    bytes.len(),
                    tile.resident_bytes(),
                    started.elapsed().as_micros()
                );
            }
            Event::Loaded(Arc::new(tile))
        }
        Err(err) => Event::Fault {
            key: Some(key),
            message: format!("decode vector tile {key:?}: {err:#}"),
        },
    }
}

fn roaming_tile_path(root: &FsPath, key: TileKey) -> PathBuf {
    root.join(key.zoom.to_string())
        .join(key.x.to_string())
        .join(format!("{}.mvt", key.y))
}

fn cache_roaming_tile(root: &FsPath, key: TileKey, bytes: &[u8]) -> Result<()> {
    let target = roaming_tile_path(root, key);
    let parent = target.parent().context("roaming tile has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create roaming tile cache {}", parent.display()))?;
    let mut staging = persistence::AtomicReplacement::raise(&target)?;
    let mut file = staging.take_file()?;
    file.write_all(bytes)
        .with_context(|| format!("write roaming tile {}", target.display()))?;
    drop(file);
    staging.commit()
}

struct CacheFile {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
}

fn reap_roaming_cache(root: &FsPath, ceiling: u64) -> Result<u64> {
    let mut files = Vec::new();
    collect_roaming_cache(root, &mut files)?;
    let mut resident = files.iter().map(|file| file.bytes).sum::<u64>();
    if resident <= ceiling {
        return Ok(resident);
    }
    files.sort_unstable_by_key(|file| file.modified);
    let target = ceiling.saturating_mul(9) / 10;
    for file in files {
        fs::remove_file(&file.path)
            .with_context(|| format!("reap roaming tile {}", file.path.display()))?;
        resident = resident.saturating_sub(file.bytes);
        if resident <= target {
            break;
        }
    }
    Ok(resident)
}

fn collect_roaming_cache(root: &FsPath, files: &mut Vec<CacheFile>) -> Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("read cache {}", root.display())),
    };
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_roaming_cache(&entry.path(), files)?;
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let path = entry.path();
        let metadata = entry.metadata()?;
        if path.extension().is_some_and(|extension| extension == "mvt") {
            files.push(CacheFile {
                path,
                bytes: metadata.len(),
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        } else if path
            .extension()
            .is_some_and(|extension| extension == "partial")
            && metadata
                .modified()
                .ok()
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age >= Duration::from_hours(24))
        {
            fs::remove_file(&path)
                .with_context(|| format!("remove stale roaming tile {}", path.display()))?;
        }
    }
    Ok(())
}

fn valid_build_key(key: &str) -> bool {
    key.len() == 16
        && key.ends_with(".pmtiles")
        && key[..8].bytes().all(|byte| byte.is_ascii_digit())
}

fn quarry(
    wake: &NativeWake,
    archive: &Arc<Archive>,
    archive_zoom: u8,
    commands: &Receiver<TileKey>,
    roaming: &Sender<TileKey>,
    events: &Sender<Event>,
) {
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(err) => {
            send_fault(wake, events, None, &err);
            return;
        }
    };
    while let Ok(key) = commands.recv() {
        if !archive_can_answer(archive_zoom, key) {
            if roaming.send(key).is_err() && events.send(Event::Missing(key)).is_err() {
                break;
            }
            let _woken = wake.request_foreground_repaint();
            continue;
        }
        let bytes = match key.coordinate().and_then(|coordinate| {
            runtime
                .block_on(archive.get_tile_decompressed(coordinate))
                .map_err(anyhow::Error::new)
        }) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                if roaming.send(key).is_err() && events.send(Event::Missing(key)).is_err() {
                    break;
                }
                let _woken = wake.request_foreground_repaint();
                continue;
            }
            Err(err) => {
                send_fault(wake, events, Some(key), &err);
                continue;
            }
        };
        let event = cut_event(key, &bytes);
        if events.send(event).is_err() {
            break;
        }
        let _woken = wake.request_foreground_repaint();
    }
}

const fn archive_can_answer(archive_zoom: u8, key: TileKey) -> bool {
    key.zoom <= archive_zoom
}

fn send_fault(
    wake: &NativeWake,
    events: &Sender<Event>,
    key: Option<TileKey>,
    err: &anyhow::Error,
) {
    let _sent = events.send(Event::Fault {
        key,
        message: format!("{err:#}"),
    });
    let _woken = wake.request_foreground_repaint();
}

fn decode_tile(key: TileKey, bytes: &[u8]) -> Result<VectorTile> {
    let reader = MvtReaderRef::new(bytes).context("parse MVT")?;
    let mut forge = Forge::new(key);
    for layer in reader.layers() {
        match layer.name() {
            "earth" => forge.fill_layer(layer, FillKind::Earth)?,
            "landcover" => forge.fill_layer(layer, FillKind::Landcover)?,
            "landuse" => forge.fill_layer(layer, FillKind::Landuse)?,
            "water" => forge.water_layer(layer)?,
            "boundaries" => forge.boundary_layer(layer)?,
            "roads" => forge.road_layer(layer)?,
            "places" => forge.label_layer(layer)?,
            "pois" => forge.poi_layer(layer)?,
            _ => {}
        }
    }
    Ok(forge.finish())
}

#[derive(Clone, Copy)]
enum FillKind {
    Earth,
    Landcover,
    Landuse,
}

impl FillKind {
    fn color(self, kind: Option<&str>) -> Option<[u8; 4]> {
        match self {
            Self::Earth => Some([
                map::MAP_GROUND_SRGB[0],
                map::MAP_GROUND_SRGB[1],
                map::MAP_GROUND_SRGB[2],
                255,
            ]),
            Self::Landcover => match kind {
                Some("forest" | "wood") => Some([145, 170, 137, 210]),
                Some("grass" | "grassland" | "scrub") => Some([173, 187, 148, 175]),
                _ => None,
            },
            Self::Landuse => match kind {
                Some("forest" | "wood" | "nature_reserve") => Some([145, 170, 137, 205]),
                Some("park" | "garden" | "grass" | "grassland" | "meadow") => {
                    Some([170, 190, 151, 185])
                }
                Some("wetland") => Some([145, 177, 165, 190]),
                Some("farmland") => Some([198, 192, 155, 145]),
                Some("beach" | "sand") => Some([222, 207, 164, 220]),
                Some("industrial" | "commercial" | "retail" | "railway") => {
                    Some([186, 169, 164, 155])
                }
                Some("school" | "college" | "university" | "hospital") => {
                    Some([211, 184, 178, 135])
                }
                Some("residential") => Some([205, 199, 186, 105]),
                _ => None,
            },
        }
    }
}

#[derive(Clone, Copy)]
struct StrokeStyle {
    color: [u8; 4],
    radius_points: f32,
    radius_world: f32,
    onset_zoom: f32,
}

const WATER_FILL: [u8; 4] = [101, 156, 181, 255];
const WATER_STROKE: StrokeStyle = StrokeStyle {
    color: [71, 130, 154, 180],
    radius_points: 0.48,
    radius_world: 0.0,
    onset_zoom: 0.0,
};
const TRAILHEAD_PARKING_ONSET_ZOOM: f32 = 10.25;
const TRAILHEAD_PARKING_SOURCE_ZOOM: u8 = 13;
const TRAILHEAD_PARKING_FETCH_LAG: f64 = 0.5;

const fn trailhead_parking_onset(_provider_onset: Option<f64>) -> f32 {
    TRAILHEAD_PARKING_ONSET_ZOOM
}

struct Forge {
    key: TileKey,
    fills: VertexBuffers<FillPoint, u32>,
    strokes: VertexBuffers<StrokePoint, u32>,
    labels: Vec<Label>,
    line_labels: Vec<LineLabel>,
    parking: Vec<Parking>,
    tessellator: FillTessellator,
}

impl Forge {
    fn new(key: TileKey) -> Self {
        Self {
            key,
            fills: VertexBuffers::new(),
            strokes: VertexBuffers::new(),
            labels: Vec::new(),
            line_labels: Vec::new(),
            parking: Vec::new(),
            tessellator: FillTessellator::new(),
        }
    }

    fn fill_layer(&mut self, layer: fast_mvt::MvtLayerRef<'_>, fill: FillKind) -> Result<()> {
        let extent = layer.extent();
        for feature in layer.features() {
            let tags = FeatureTags::read(feature)?;
            let Some(color) = fill.color(tags.kind) else {
                continue;
            };
            self.fill_geometry(
                &feature.geometry()?,
                extent,
                color,
                tags.min_zoom.unwrap_or(0.0) as f32,
            )?;
        }
        Ok(())
    }

    fn water_layer(&mut self, layer: fast_mvt::MvtLayerRef<'_>) -> Result<()> {
        let extent = layer.extent();
        for feature in layer.features() {
            let tags = FeatureTags::read(feature)?;
            let onset_zoom = tags.min_zoom.unwrap_or(0.0) as f32;
            let geometry = feature.geometry()?;
            match geometry {
                MvtGeometry::Polygon(_) | MvtGeometry::MultiPolygon(_) => {
                    self.fill_geometry(&geometry, extent, WATER_FILL, onset_zoom)?;
                }
                MvtGeometry::LineString(_) | MvtGeometry::MultiLineString(_) => {
                    self.stroke_geometry(
                        &geometry,
                        extent,
                        StrokeStyle {
                            onset_zoom,
                            ..WATER_STROKE
                        },
                    );
                }
                MvtGeometry::Point(point) => {
                    if let (Some(name), Some(style)) = (
                        tags.name,
                        water_label_style(tags.kind, tags.detail, tags.min_zoom),
                    ) {
                        self.push_label(point, extent, name, style, LabelKind::Lake);
                    }
                }
                MvtGeometry::MultiPoint(points) => {
                    if let (Some(name), Some(style)) = (
                        tags.name,
                        water_label_style(tags.kind, tags.detail, tags.min_zoom),
                    ) {
                        for point in points {
                            self.push_label(point, extent, name, style, LabelKind::Lake);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn boundary_layer(&mut self, layer: fast_mvt::MvtLayerRef<'_>) -> Result<()> {
        let extent = layer.extent();
        for feature in layer.features() {
            let style = boundary_style(
                integer_property(feature, "kind_detail")?,
                numeric_property(feature, "min_zoom")?,
            );
            self.stroke_geometry(&feature.geometry()?, extent, style);
        }
        Ok(())
    }

    fn road_layer(&mut self, layer: fast_mvt::MvtLayerRef<'_>) -> Result<()> {
        let extent = layer.extent();
        for feature in layer.features() {
            let tags = FeatureTags::read(feature)?;
            let geometry = feature.geometry()?;
            if let Some(style) = road_style(tags.kind, tags.detail, tags.min_zoom, self.key) {
                self.stroke_geometry(&geometry, extent, style);
            }
            let Some(name) = tags.name else { continue };
            let Some(style) = road_label_style(tags.kind, tags.detail, tags.min_zoom) else {
                continue;
            };
            match geometry {
                MvtGeometry::LineString(line) => {
                    self.push_line_label(&line, extent, name, style);
                }
                MvtGeometry::MultiLineString(lines) => {
                    for line in lines {
                        self.push_line_label(&line, extent, name, style);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn push_line_label(
        &mut self,
        line: &LineString<i32>,
        extent: u32,
        text: &str,
        style: LabelStyle,
    ) {
        let path = line
            .0
            .iter()
            .copied()
            .map(|point| world64(self.key, extent, point))
            .collect::<Vec<_>>();
        if path.len() >= 2 {
            self.line_labels.push(LineLabel {
                path: path.into(),
                text: Arc::from(text),
                rank: style.rank,
                size: style.size,
                onset_zoom: style.onset_zoom,
            });
        }
    }

    fn poi_layer(&mut self, layer: fast_mvt::MvtLayerRef<'_>) -> Result<()> {
        let extent = layer.extent();
        for feature in layer.features() {
            let tags = FeatureTags::read(feature)?;
            match feature.geometry()? {
                MvtGeometry::Point(point) => self.push_poi(point, extent, tags),
                MvtGeometry::MultiPoint(points) => {
                    for point in points {
                        self.push_poi(point, extent, tags);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn push_poi(&mut self, point: Point<i32>, extent: u32, tags: FeatureTags<'_>) {
        match tags.kind {
            Some("parking") if public_access(tags.access) => {
                // Generic POI priority is immaterial after VectorField
                // reclassifies this as public parking abutting a trail.
                self.parking.push(Parking {
                    world: world64(self.key, extent, point.0),
                    name: tags.name.map(Arc::from),
                    onset_zoom: trailhead_parking_onset(tags.min_zoom),
                });
            }
            Some("peak") => {
                let Some(name) = tags.name else { return };
                let text = tags.elevation_m.map_or_else(
                    || name.to_owned(),
                    |elevation| format!("{name} · {elevation:.0} m"),
                );
                self.push_label(
                    point,
                    extent,
                    &text,
                    peak_label_style(tags.min_zoom),
                    LabelKind::Peak,
                );
            }
            Some("lake" | "pond" | "reservoir") => {
                let Some(name) = tags.name else { return };
                self.push_label(
                    point,
                    extent,
                    name,
                    pond_label_style(tags.min_zoom),
                    LabelKind::Lake,
                );
            }
            _ => {}
        }
    }

    fn label_layer(&mut self, layer: fast_mvt::MvtLayerRef<'_>) -> Result<()> {
        let extent = layer.extent();
        for feature in layer.features() {
            let tags = FeatureTags::read(feature)?;
            let Some(name) = tags.name else { continue };
            let Some(style) =
                label_style(tags.kind, tags.detail, tags.population_rank, tags.min_zoom)
            else {
                continue;
            };
            let geometry = feature.geometry()?;
            match geometry {
                MvtGeometry::Point(point) => {
                    self.push_label(point, extent, name, style, LabelKind::Place);
                }
                MvtGeometry::MultiPoint(points) => {
                    for point in points {
                        self.push_label(point, extent, name, style, LabelKind::Place);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn push_label(
        &mut self,
        point: Point<i32>,
        extent: u32,
        text: &str,
        style: LabelStyle,
        kind: LabelKind,
    ) {
        self.labels.push(Label {
            world: world64(self.key, extent, point.0),
            text: Arc::from(text),
            kind,
            rank: style.rank,
            size: style.size,
            onset_zoom: style.onset_zoom,
        });
    }

    fn fill_geometry(
        &mut self,
        geometry: &MvtGeometry,
        extent: u32,
        color: [u8; 4],
        onset_zoom: f32,
    ) -> Result<()> {
        match geometry {
            MvtGeometry::Polygon(polygon) => {
                self.fill_polygon(polygon, extent, color, onset_zoom)?;
            }
            MvtGeometry::MultiPolygon(polygons) => {
                for polygon in polygons {
                    self.fill_polygon(polygon, extent, color, onset_zoom)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn fill_polygon(
        &mut self,
        polygon: &Polygon<i32>,
        extent: u32,
        color: [u8; 4],
        onset_zoom: f32,
    ) -> Result<()> {
        let mut path = Path::builder();
        push_ring(&mut path, extent, polygon.exterior());
        for ring in polygon.interiors() {
            push_ring(&mut path, extent, ring);
        }
        let path = path.build();
        self.tessellator
            .tessellate_path(
                &path,
                &FillOptions::default().with_fill_rule(FillRule::EvenOdd),
                &mut BuffersBuilder::new(&mut self.fills, |vertex: FillVertex<'_>| FillPoint {
                    local: vertex.position().to_array(),
                    srgb: color,
                    onset_zoom,
                }),
            )
            .context("tessellate vector polygon")?;
        Ok(())
    }

    fn stroke_geometry(&mut self, geometry: &MvtGeometry, extent: u32, style: StrokeStyle) {
        match geometry {
            MvtGeometry::LineString(line) => self.stroke_line(line, extent, style),
            MvtGeometry::MultiLineString(lines) => {
                for line in lines {
                    self.stroke_line(line, extent, style);
                }
            }
            _ => {}
        }
    }

    fn stroke_line(&mut self, line: &LineString<i32>, extent: u32, style: StrokeStyle) {
        let mut points = Vec::with_capacity(line.0.len());
        for &coordinate in &line.0 {
            let point = local32(extent, coordinate);
            if points.last().is_none_or(|prior| !same_point(*prior, point)) {
                points.push(point);
            }
        }
        if points.len() < 2 {
            return;
        }
        let Ok(base) = u32::try_from(self.strokes.vertices.len()) else {
            return;
        };
        for slot in 0..points.len() {
            let extrusion = join_normal(&points, slot);
            self.strokes.vertices.extend([
                StrokePoint {
                    local: points[slot],
                    extrusion: [-extrusion[0], -extrusion[1]],
                    srgb: style.color,
                    radius_points: style.radius_points,
                    radius_world: style.radius_world,
                    onset_side: -(style.onset_zoom.max(0.0) + 1.0),
                },
                StrokePoint {
                    local: points[slot],
                    extrusion,
                    srgb: style.color,
                    radius_points: style.radius_points,
                    radius_world: style.radius_world,
                    onset_side: style.onset_zoom.max(0.0) + 1.0,
                },
            ]);
        }
        for slot in 0..points.len() - 1 {
            let Some(offset) = u32::try_from(slot)
                .ok()
                .and_then(|slot| slot.checked_mul(2))
            else {
                return;
            };
            let a = base + offset;
            self.strokes
                .indices
                .extend([a, a + 1, a + 2, a + 1, a + 3, a + 2]);
        }
    }

    fn finish(mut self) -> VectorTile {
        self.labels.sort_unstable_by_key(|label| label.rank);
        VectorTile {
            key: self.key,
            fills: Mesh {
                vertices: self.fills.vertices.into(),
                indices: self.fills.indices.into(),
            },
            strokes: Mesh {
                vertices: self.strokes.vertices.into(),
                indices: self.strokes.indices.into(),
            },
            labels: self.labels.into(),
            line_labels: self.line_labels.into(),
            parking: self.parking.into(),
        }
    }
}

fn boundary_style(detail: Option<i64>, min_zoom: Option<f64>) -> StrokeStyle {
    let (color, radius, fallback_onset) = match detail {
        Some(2 | 3) => ([82, 76, 66, 195], 0.72, 0.0),
        Some(4 | 5) => ([103, 94, 80, 150], 0.48, 3.0),
        Some(6 | 7) => ([122, 113, 97, 105], 0.30, 6.0),
        _ => ([130, 121, 105, 72], 0.22, 8.0),
    };
    StrokeStyle {
        color,
        radius_points: radius,
        radius_world: 0.0,
        onset_zoom: disclosure_onset(min_zoom, fallback_onset),
    }
}

fn road_style(
    kind: Option<&str>,
    detail: Option<&str>,
    min_zoom: Option<f64>,
    key: TileKey,
) -> Option<StrokeStyle> {
    // The routable corpus exclusively owns pedestrian geometry. A second,
    // lower-fidelity PMTiles copy would disclose on an independent zoom
    // schedule and make paths appear, vanish, then reappear.
    if kind == Some("path") {
        return None;
    }
    let (color, radius, width_m, fallback_onset) = match (kind, detail) {
        (Some("highway"), Some("motorway")) => (road_ink(235), 1.40, 11.5, 4.0),
        (Some("highway"), Some("motorway_link")) | (Some("major_road"), Some("trunk_link")) => {
            (road_ink(220), 1.05, 7.0, 7.0)
        }
        (Some("major_road"), Some("primary")) => (road_ink(220), 1.15, 10.0, 7.0),
        (Some("major_road"), Some("primary_link")) => (road_ink(210), 0.95, 6.5, 8.0),
        (Some("major_road"), Some("secondary_link")) => (road_ink(205), 0.90, 6.0, 9.0),
        (Some("major_road"), Some("tertiary")) => (road_ink(210), 0.96, 8.0, 9.0),
        (Some("major_road"), Some("tertiary_link")) => (road_ink(195), 0.82, 5.5, 10.0),
        (Some("minor_road"), Some("residential" | "unclassified" | "road")) => {
            (road_ink(170), 0.72, 7.0, 10.5)
        }
        (Some("minor_road"), Some("living_street")) => (road_ink(165), 0.68, 6.0, 10.5),
        (Some("minor_road"), Some("service")) => (road_ink(132), 0.50, 4.5, 11.5),
        (Some("minor_road"), Some("alley" | "parking_aisle" | "driveway")) => {
            (road_ink(120), 0.44, 3.0, 12.0)
        }
        (Some("rail"), _) | (_, Some("rail" | "light_rail" | "tram" | "subway")) => {
            ([104, 101, 94, 62], 0.15, 0.0, 9.0)
        }
        (Some("ferry" | "ferryway"), _) => ([74, 129, 150, 60], 0.14, 0.0, 8.0),
        (Some("highway"), _) | (Some("major_road"), Some("trunk")) => {
            (road_ink(230), 1.25, 11.0, 5.0)
        }
        (Some("major_road"), _) => (road_ink(215), 1.05, 9.0, 8.0),
        (Some("minor_road"), _) => (road_ink(165), 0.70, 7.0, 10.5),
        _ => return None,
    };
    let onset_zoom = disclosure_onset(min_zoom, fallback_onset);
    (f32::from(key.zoom) + 1.0 >= onset_zoom).then_some(StrokeStyle {
        color,
        radius_points: radius,
        radius_world: projected_radius_world(width_m * 0.5, key),
        onset_zoom,
    })
}

fn projected_radius_world(radius_m: f64, key: TileKey) -> f32 {
    if radius_m <= 0.0 {
        return 0.0;
    }
    let divisions = f64::from(1_u32 << key.zoom);
    let latitude = map::world_to_coord([0.5, (f64::from(key.y) + 0.5) / divisions])
        .lat
        .to_radians();
    (radius_m / (map::EARTH_CIRCUMFERENCE_M * latitude.cos())) as f32
}

const fn road_ink(alpha: u8) -> [u8; 4] {
    [
        map::ROAD_SRGB[0],
        map::ROAD_SRGB[1],
        map::ROAD_SRGB[2],
        alpha,
    ]
}

fn road_label_style(
    kind: Option<&str>,
    detail: Option<&str>,
    min_zoom: Option<f64>,
) -> Option<LabelStyle> {
    let (rank, size, fallback_onset) = match (kind, detail) {
        (Some("highway"), Some("motorway")) => (760, 10.8, 7.0),
        (Some("major_road"), Some("trunk" | "trunk_link")) => (780, 10.6, 8.0),
        (Some("major_road"), Some("primary" | "primary_link")) => (800, 10.4, 9.0),
        (Some("major_road"), Some("secondary" | "secondary_link")) => (820, 10.2, 10.0),
        (Some("major_road"), Some("tertiary" | "tertiary_link")) => (840, 10.0, 11.0),
        (Some("minor_road"), Some("residential" | "unclassified" | "road")) => (900, 9.6, 13.0),
        (Some("minor_road"), Some("service")) => (960, 9.2, 14.0),
        (Some("highway" | "major_road"), _) => (850, 10.0, 10.0),
        (Some("minor_road"), _) => (980, 9.2, 14.0),
        _ => return None,
    };
    Some(LabelStyle {
        rank,
        size,
        onset_zoom: disclosure_onset(min_zoom, fallback_onset),
    })
}

fn public_access(access: Option<&str>) -> bool {
    !matches!(
        access,
        Some("private" | "no" | "customers" | "permit" | "destination")
    )
}

#[derive(Clone, Copy)]
struct LabelStyle {
    rank: u16,
    size: f32,
    onset_zoom: f32,
}

const LANDMARK_DISCLOSURE_LEAD: f64 = 1.25;
const LANDMARK_FALLBACK_ADVANCE: f64 = 0.5;

fn water_label_style(
    kind: Option<&str>,
    detail: Option<&str>,
    min_zoom: Option<f64>,
) -> Option<LabelStyle> {
    let kind = detail.or(kind);
    let (rank, size, onset) = match kind {
        Some("lake" | "reservoir") => (650, 11.8, 9.5),
        Some("pond") => return Some(pond_label_style(min_zoom)),
        Some("water") | None => (700, 11.2, 10.5),
        Some(_) => return None,
    };
    Some(LabelStyle {
        rank,
        size,
        onset_zoom: landmark_onset(min_zoom, onset),
    })
}

fn pond_label_style(min_zoom: Option<f64>) -> LabelStyle {
    LabelStyle {
        rank: 760,
        size: 10.8,
        onset_zoom: landmark_onset(min_zoom, 11.25),
    }
}

fn peak_label_style(min_zoom: Option<f64>) -> LabelStyle {
    LabelStyle {
        rank: 620,
        size: 11.4,
        onset_zoom: landmark_onset(min_zoom, 9.75),
    }
}

fn label_style(
    kind: Option<&str>,
    detail: Option<&str>,
    population_rank: Option<f64>,
    min_zoom: Option<f64>,
) -> Option<LabelStyle> {
    let population = population_rank.unwrap_or(0.0).clamp(0.0, 18.0);
    let scarcity = (18.0 - population).round() as u16;
    let (base, size, fallback_onset) = match (kind, detail) {
        (Some("country"), _) => (0, 15.0, 1.0),
        (Some("region"), Some("state" | "province")) => (40, 13.0, 3.0),
        (Some("region"), _) => (50, 12.5, 4.0),
        (Some("locality"), Some("city")) => (
            100,
            10.4 + population * 0.22,
            (11.5 - population * 0.48).clamp(3.0, 10.0),
        ),
        (Some("locality"), Some("town")) => (
            220,
            9.6 + population * 0.16,
            (13.0 - population * 0.34).clamp(7.0, 11.5),
        ),
        (Some("locality"), Some("village")) => (
            340,
            9.2 + population * 0.11,
            (13.5 - population * 0.25).clamp(9.0, 12.0),
        ),
        (Some("locality"), Some("hamlet" | "locality")) => (
            460,
            8.7 + population * 0.07,
            (14.0 - population * 0.20).clamp(10.5, 12.0),
        ),
        (Some("macrohood"), _) => (560, 9.5, 10.0),
        (Some("neighbourhood"), Some("suburb")) => (620, 9.2, 10.5),
        (Some("neighbourhood"), _) => (700, 8.8, 11.5),
        _ => return None,
    };
    Some(LabelStyle {
        rank: base + scarcity.saturating_mul(4),
        size: size as f32,
        onset_zoom: disclosure_onset(min_zoom, fallback_onset),
    })
}

fn disclosure_onset(provider: Option<f64>, style: f64) -> f32 {
    provider.unwrap_or(style).max(style) as f32
}

fn landmark_onset(provider: Option<f64>, style: f64) -> f32 {
    disclosure_onset(
        provider.map(|zoom| zoom - LANDMARK_DISCLOSURE_LEAD),
        style - LANDMARK_FALLBACK_ADVANCE,
    )
}

#[derive(Clone, Copy, Default)]
struct FeatureTags<'a> {
    kind: Option<&'a str>,
    detail: Option<&'a str>,
    name: Option<&'a str>,
    population_rank: Option<f64>,
    min_zoom: Option<f64>,
    access: Option<&'a str>,
    elevation_m: Option<f64>,
}

impl<'a> FeatureTags<'a> {
    fn read(feature: MvtFeatureRef<'a>) -> Result<Self> {
        let mut tags = Self::default();
        for property in feature.properties() {
            let (key, value) = property?;
            match (key, value) {
                ("kind", MvtValueRef::String(value)) => tags.kind = Some(value),
                ("kind_detail", MvtValueRef::String(value)) => tags.detail = Some(value),
                ("name:en", MvtValueRef::String(value)) => tags.name = Some(value),
                ("name", MvtValueRef::String(value)) if tags.name.is_none() => {
                    tags.name = Some(value);
                }
                ("population_rank", value) => tags.population_rank = numeric(value),
                ("min_zoom", value) => tags.min_zoom = numeric(value),
                ("access", MvtValueRef::String(value)) => tags.access = Some(value),
                ("ele" | "elevation", value) => tags.elevation_m = numeric(value),
                _ => {}
            }
        }
        Ok(tags)
    }
}

fn integer_property(feature: MvtFeatureRef<'_>, needle: &str) -> Result<Option<i64>> {
    for property in feature.properties() {
        let (key, value) = property?;
        if key == needle {
            return Ok(integer(value));
        }
    }
    Ok(None)
}

fn numeric_property(feature: MvtFeatureRef<'_>, needle: &str) -> Result<Option<f64>> {
    for property in feature.properties() {
        let (key, value) = property?;
        if key == needle {
            return Ok(numeric(value));
        }
    }
    Ok(None)
}

fn numeric(value: MvtValueRef<'_>) -> Option<f64> {
    match value {
        MvtValueRef::Float(value) => Some(f64::from(value)),
        MvtValueRef::Double(value) => Some(value),
        MvtValueRef::Int(value) | MvtValueRef::SInt(value) => Some(value as f64),
        MvtValueRef::UInt(value) => Some(value as f64),
        _ => None,
    }
}

fn integer(value: MvtValueRef<'_>) -> Option<i64> {
    match value {
        MvtValueRef::Int(value) | MvtValueRef::SInt(value) => Some(value),
        MvtValueRef::UInt(value) => i64::try_from(value).ok(),
        _ => None,
    }
}

fn push_ring(
    path: &mut lyon_tessellation::path::path::Builder,
    extent: u32,
    ring: &LineString<i32>,
) {
    let Some((first, rest)) = ring.0.split_first() else {
        return;
    };
    let first = local32(extent, *first);
    let _first = path.begin(point(first[0], first[1]));
    for coord in rest {
        let next = local32(extent, *coord);
        let _next = path.line_to(point(next[0], next[1]));
    }
    path.end(true);
}

fn local32(extent: u32, coordinate: Coord<i32>) -> [f32; 2] {
    let extent = extent as f32;
    [coordinate.x as f32 / extent, coordinate.y as f32 / extent]
}

fn world64(key: TileKey, extent: u32, coordinate: Coord<i32>) -> [f64; 2] {
    let scale = f64::from(1_u32 << key.zoom);
    let extent = f64::from(extent);
    [
        (f64::from(key.x) + f64::from(coordinate.x) / extent) / scale,
        (f64::from(key.y) + f64::from(coordinate.y) / extent) / scale,
    ]
}

pub fn join_normal(points: &[[f32; 2]], slot: usize) -> [f32; 2] {
    let prior = slot.saturating_sub(1);
    let next = (slot + 1).min(points.len() - 1);
    let incoming = direction(points[prior], points[slot]);
    let outgoing = direction(points[slot], points[next]);
    let first = if slot == 0 { outgoing } else { incoming };
    let second = if slot + 1 == points.len() {
        incoming
    } else {
        outgoing
    };
    let normal_a = [-first[1], first[0]];
    let normal_b = [-second[1], second[0]];
    let sum = [normal_a[0] + normal_b[0], normal_a[1] + normal_b[1]];
    let length = sum[0].hypot(sum[1]);
    if length <= f32::EPSILON {
        return normal_b;
    }
    let miter = [sum[0] / length, sum[1] / length];
    let divisor = miter[0].mul_add(normal_b[0], miter[1] * normal_b[1]);
    let reach = if divisor.abs() <= 0.25 {
        1.0
    } else {
        (1.0 / divisor).clamp(-3.0, 3.0)
    };
    [miter[0] * reach, miter[1] * reach]
}

fn direction(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    let delta = [b[0] - a[0], b[1] - a[1]];
    let length = delta[0].hypot(delta[1]);
    if length <= f32::EPSILON {
        [1.0, 0.0]
    } else {
        [delta[0] / length, delta[1] / length]
    }
}

pub fn same_point(left: [f32; 2], right: [f32; 2]) -> bool {
    left.map(f32::to_bits) == right.map(f32::to_bits)
}

fn forge_bounds(graph: &WalkGraph) -> Result<GeoBounds> {
    let first = graph
        .vertices
        .first()
        .context("cannot forge a basemap for an empty graph")?
        .coord;
    let raw = graph.vertices.iter().skip(1).fold(
        GeoBounds::new(first.lon, first.lat, first.lon, first.lat),
        |mut bounds, vertex| {
            bounds.west = bounds.west.min(vertex.coord.lon);
            bounds.south = bounds.south.min(vertex.coord.lat);
            bounds.east = bounds.east.max(vertex.coord.lon);
            bounds.north = bounds.north.max(vertex.coord.lat);
            bounds
        },
    );
    ensure!(
        [raw.west, raw.south, raw.east, raw.north]
            .into_iter()
            .all(f64::is_finite),
        "graph contains non-finite coordinates"
    );
    let longitude_margin = ((raw.east - raw.west) * 0.18).max(0.01);
    let latitude_margin = ((raw.north - raw.south) * 0.18).max(0.01);
    let bounds = GeoBounds::new(
        (raw.west - longitude_margin).max(-180.0),
        (raw.south - latitude_margin).max(-85.051_128_78),
        (raw.east + longitude_margin).min(180.0),
        (raw.north + latitude_margin).min(85.051_128_78),
    );
    ensure!(bounds.is_valid(), "graph does not enclose a valid map area");
    Ok(bounds)
}

fn extraction_keys(bounds: GeoBounds, max_zoom: u8) -> Result<Vec<TileKey>> {
    ensure!(bounds.is_valid(), "invalid basemap extraction bounds");
    let west = (bounds.west + 180.0) / 360.0;
    let east = (bounds.east + 180.0) / 360.0;
    let north = mercator_y(bounds.north);
    let south = mercator_y(bounds.south);
    let mut keys = Vec::new();
    for zoom in 0..=max_zoom {
        let divisions = 1_u32 << zoom;
        let scale = f64::from(divisions);
        let crown = divisions.saturating_sub(1);
        let left = (west * scale).floor().clamp(0.0, f64::from(crown)) as u32;
        let right = (east * scale).floor().clamp(0.0, f64::from(crown)) as u32;
        let top = (north * scale).floor().clamp(0.0, f64::from(crown)) as u32;
        let bottom = (south * scale).floor().clamp(0.0, f64::from(crown)) as u32;
        for y in top..=bottom {
            for x in left..=right {
                keys.push(TileKey { zoom, x, y });
            }
        }
    }
    keys.sort_unstable_by_key(|key| {
        TileId::from(
            key.coordinate()
                .expect("extraction bounds must yield valid coordinates"),
        )
    });
    Ok(keys)
}

fn extraction_keys_for_regions(regions: &[GeoBounds], max_zoom: u8) -> Result<Vec<TileKey>> {
    let mut keys = regions
        .iter()
        .map(|bounds| extraction_keys(*bounds, max_zoom))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    keys.sort_unstable_by_key(|key| {
        TileId::from(
            key.coordinate()
                .expect("validated extraction key must have a tile id"),
        )
    });
    keys.dedup();
    Ok(keys)
}

fn bounded_extraction(
    regions: &[GeoBounds],
    requested_zoom: u8,
    tile_ceiling: usize,
) -> Result<(u8, Vec<TileKey>)> {
    ensure!(tile_ceiling > 0, "basemap extraction tile ceiling is zero");
    let mut accepted = extraction_keys_for_regions(regions, 0)?;
    for zoom in 1..=requested_zoom {
        let candidate = extraction_keys_for_regions(regions, zoom)?;
        if candidate.len() > tile_ceiling {
            return Ok((zoom - 1, accepted));
        }
        accepted = candidate;
    }
    Ok((requested_zoom, accepted))
}

fn mercator_y(latitude: f64) -> f64 {
    (1.0 - latitude.to_radians().tan().asinh() / std::f64::consts::PI) * 0.5
}

fn purge_partials(target: &FsPath) -> Result<()> {
    let Some(directory) = target.parent() else {
        return Ok(());
    };
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("read {}", directory.display())),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let stale = path
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age >= Duration::from_hours(24));
        if persistence::owns_staging_path(target, &path) && stale {
            std::fs::remove_file(&path)
                .with_context(|| format!("remove stale basemap {}", path.display()))?;
        }
    }
    Ok(())
}

fn reap_region_archives(retained: &FsPath) -> Result<()> {
    let Some(directory) = retained.parent() else {
        return Ok(());
    };
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("read {}", directory.display())),
    };
    for entry in entries {
        let path = entry?.path();
        if path != retained && region_archive_path(&path) {
            fs::remove_file(&path)
                .with_context(|| format!("remove superseded basemap {}", path.display()))?;
        }
    }
    Ok(())
}

fn region_archive_path(path: &FsPath) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(identity) = name
        .strip_prefix("basemap-")
        .and_then(|name| name.strip_suffix(".pmtiles"))
    else {
        return false;
    };
    identity.len() == 16 && identity.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::Viewport;

    const VIEW: Viewport = Viewport {
        center: [0.5, 0.5],
        zoom: 10.0,
    };

    const fn detail(source: u8, prefetch: bool) -> DetailPlan {
        DetailPlan {
            source: SourceLevel::new(source),
            prefetch,
        }
    }

    #[test]
    fn cover_demands_one_fallback_and_current_detail_only() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
        let cover = cover(
            MapFramePlan::forge(VIEW, rect),
            detail(9, false),
            None,
            false,
        );
        assert_eq!(cover.strata.len(), 2);
        assert_eq!(cover.strata[0].intent, Intent::Fallback);
        assert_eq!(cover.strata[1].intent, Intent::Required);
        assert_eq!(cover.source, SourceLevel::new(9));
        assert!(
            cover
                .cells
                .iter()
                .all(|cell| cell.key.zoom == cover.source.get())
        );
    }

    #[test]
    fn visible_cells_are_independent_units_of_refinement() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
        let cover = cover(
            MapFramePlan::forge(VIEW, rect),
            detail(9, true),
            None,
            false,
        );
        assert_eq!(cover.strata.len(), 3);
        assert_eq!(cover.strata[2].intent, Intent::Prefetch);
        let cells = cover.cells;
        assert!(cells.len() > 1);
        let demoted = cells[0]
            .key
            .ancestor(SourceLevel::new(cover.source.get() - 1));
        assert_eq!(demoted.zoom + 1, cells[0].key.zoom);
        assert!(
            cells[1..]
                .iter()
                .all(|cell| cell.key.zoom == cover.source.get())
        );
    }

    #[test]
    fn world_wrap_never_duplicates_archive_demand() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1440.0, 920.0));
        let cover = cover(
            MapFramePlan::forge(
                Viewport {
                    zoom: Viewport::MIN_ZOOM,
                    ..VIEW
                },
                rect,
            ),
            detail(0, false),
            None,
            false,
        );
        for stratum in cover.strata {
            let distinct = stratum
                .keys
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(distinct.len(), stratum.keys.len());
        }
    }

    #[test]
    fn detail_governor_overscales_hysteretically_and_prefetches_by_deadline() {
        let now = Instant::now();
        let mut governor = DetailGovernor::default();
        assert_eq!(
            governor.resolve(10.0, Duration::from_millis(250), now),
            detail(9, false)
        );
        assert_eq!(
            governor.resolve(
                10.30,
                Duration::from_millis(250),
                now + Duration::from_millis(100),
            ),
            detail(9, true)
        );
        assert_eq!(
            governor.resolve(
                10.30,
                Duration::from_millis(250),
                now + Duration::from_millis(250),
            ),
            detail(9, false)
        );
        assert_eq!(
            governor.resolve(
                10.34,
                Duration::from_millis(250),
                now + Duration::from_millis(300),
            ),
            detail(10, false)
        );
        assert_eq!(
            governor.resolve(
                10.05,
                Duration::from_millis(250),
                now + Duration::from_millis(400),
            ),
            detail(10, false)
        );
        assert_eq!(
            governor.resolve(
                9.99,
                Duration::from_millis(250),
                now + Duration::from_millis(500),
            ),
            detail(9, false)
        );
    }

    #[test]
    fn disjoint_region_cut_is_sparse_and_order_invariant() -> Result<()> {
        let east = GeoBounds::new(-74.2, 41.1, -74.0, 41.3);
        let west = GeoBounds::new(-105.2, 39.9, -105.0, 40.1);
        let sparse = extraction_keys_for_regions(&[east, west], 8)?;
        let hull = extraction_keys(
            region_hull(&[east, west]).context("two regions have a hull")?,
            8,
        )?;

        assert!(sparse.len() < hull.len());
        assert_eq!(region_key(&[east, west]), region_key(&[west, east]));
        assert!(sparse.windows(2).all(|pair| {
            let left = TileId::from(pair[0].coordinate().expect("valid coordinate"));
            let right = TileId::from(pair[1].coordinate().expect("valid coordinate"));
            left < right
        }));
        Ok(())
    }

    #[test]
    fn nomad_preempts_stale_tiles_for_the_latest_deep_view() {
        let stale = (0..NOMAD_BATCH as u32 + 8).map(|x| TileKey { zoom: 8, x, y: 0 });
        let current = TileKey {
            zoom: 15,
            x: 9_642,
            y: 12_276,
        };
        let (batch, relinquished) = latest_nomad_batch(stale.chain(std::iter::once(current)));
        assert_eq!(batch.first(), Some(&current));
        assert_eq!(batch.len(), NOMAD_BATCH);
        assert_eq!(relinquished.len(), 9);
        assert!(!relinquished.contains(&current));
    }

    #[test]
    fn partial_reaper_is_bound_to_its_archive() {
        let target = FsPath::new("basemap.pmtiles");
        assert!(persistence::owns_staging_path(
            target,
            FsPath::new("basemap.pmtiles.atomic-a71B.partial")
        ));
        assert!(!persistence::owns_staging_path(
            target,
            FsPath::new("dem.pmtiles.atomic-a71B.partial")
        ));
        assert!(!persistence::owns_staging_path(
            target,
            FsPath::new("basemap.pmtiles.backup.partial")
        ));
    }
}
