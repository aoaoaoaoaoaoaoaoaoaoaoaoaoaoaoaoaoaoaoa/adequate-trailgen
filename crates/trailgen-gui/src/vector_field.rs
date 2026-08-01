use crate::{
    annotation,
    basemap::{self, Basemap, Source, TileKey, VectorTile},
    map::{self, CartographicPlan, MapFramePlan},
    vector_map::{GeometryPass, VectorCorpus, VectorLayer, VectorPaint, VectorPatch},
};
use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use egui::{Color32, Painter};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use trailgen_core::{EdgeIndex, WalkGraph};

const VECTOR_CEILING: usize = 512 * 1_048_576;
const RETRY_FLOOR: Duration = Duration::from_millis(250);
const RETRY_CEILING: Duration = Duration::from_secs(30);
const READY_LATENCY_SEED: Duration = Duration::from_millis(250);
const TRAILHEAD_PARKING_REACH_M: f64 = 160.0;
const PRESENTATION_TRANSITION: Duration = Duration::from_millis(160);
const ABSORB_BUDGET: Duration = Duration::from_millis(2);
const ABSORB_LIMIT: usize = 8;
const PARKING_CHANNEL_CAPACITY: usize = 32;
const PARKING_DRAIN_LIMIT: usize = 16;

/// The reusable, streaming vector-map plane beneath every trail workbench.
pub struct VectorField {
    annotations: annotation::Engine,
    corpus: VectorCorpus,
    armory: Option<Basemap>,
    tiles: VectorBank,
    presented: Arc<[VectorPatch]>,
    prewarm: Arc<[VectorPatch]>,
    transition: Option<PresentationTransition>,
    inflight: HashMap<TileKey, Instant>,
    missing: HashSet<TileKey>,
    retries: HashMap<TileKey, Retry>,
    demand: Vec<TileKey>,
    demand_dirty: bool,
    detail: basemap::DetailGovernor,
    cartographic_zoom: f32,
    readiness: ReadinessOracle,
    presentation: Option<PresentationStamp>,
    presentation_revision: u64,
    archive_zoom: Option<u8>,
    trails: Option<Arc<WalkGraph>>,
    trail_index: Option<Arc<EdgeIndex>>,
    parking_forge: Option<ParkingForge>,
    trailhead_parking: HashMap<TileKey, Arc<[basemap::Parking]>>,
    parking_queue: VecDeque<Arc<VectorTile>>,
    parking_queued: HashSet<TileKey>,
}

pub type RetiredTrailArmament = (Option<Arc<WalkGraph>>, Option<Arc<EdgeIndex>>);

struct Retry {
    failures: u8,
    after: Instant,
}

struct ReadinessOracle(Duration);

struct PresentationStamp {
    frame: MapFramePlan,
    source: basemap::SourceLevel,
    cells: Vec<basemap::TileCell>,
    bank_revision: u64,
}

struct PresentationTransition {
    prior: Arc<[VectorPatch]>,
    begun: Instant,
}

struct ParkingForge {
    command: Sender<Arc<VectorTile>>,
    events: Receiver<ParkingArmament>,
    _thread: thread::JoinHandle<()>,
}

struct ParkingArmament {
    key: TileKey,
    parking: Arc<[basemap::Parking]>,
}

impl ParkingForge {
    fn spawn(ctx: egui::Context, graph: Arc<WalkGraph>, index: Arc<EdgeIndex>) -> Result<Self> {
        let (command, jobs) = bounded::<Arc<VectorTile>>(PARKING_CHANNEL_CAPACITY);
        let (armament, events) = bounded(PARKING_CHANNEL_CAPACITY);
        let thread = thread::Builder::new()
            .name("trailhead-parking-forge".to_owned())
            .spawn(move || {
                while let Ok(tile) = jobs.recv() {
                    let parking = tile
                        .parking
                        .iter()
                        .filter(|parking| abuts_trail(&graph, &index, parking))
                        .cloned()
                        .collect::<Arc<[_]>>();
                    if armament
                        .send(ParkingArmament {
                            key: tile.key,
                            parking,
                        })
                        .is_err()
                    {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .context("spawn trailhead-parking forge")?;
        Ok(Self {
            command,
            events,
            _thread: thread,
        })
    }
}

impl Retry {
    fn fail(prior: Option<&Self>, now: Instant) -> Self {
        let failures = prior.map_or(1, |retry| retry.failures.saturating_add(1));
        let factor = 1_u32 << failures.saturating_sub(1).min(7);
        let delay = RETRY_FLOOR.saturating_mul(factor).min(RETRY_CEILING);
        Self {
            failures,
            after: now + delay,
        }
    }
}

impl Default for ReadinessOracle {
    fn default() -> Self {
        Self(READY_LATENCY_SEED)
    }
}

impl ReadinessOracle {
    const fn estimate(&self) -> Duration {
        self.0
    }

    fn observe(&mut self, elapsed: Duration) {
        self.0 = if elapsed > self.0 {
            elapsed
        } else {
            self.0.mul_f64(0.875) + elapsed.mul_f64(0.125)
        };
    }
}

impl VectorField {
    pub fn raise(
        ctx: &egui::Context,
        source: Source,
        offline: bool,
        trails: Option<(Arc<WalkGraph>, Arc<EdgeIndex>)>,
    ) -> Result<Self> {
        let (trails, trail_index) = trails.unzip();
        let parking_forge = trails
            .as_ref()
            .zip(trail_index.as_ref())
            .map(|(graph, index)| {
                ParkingForge::spawn(ctx.clone(), Arc::clone(graph), Arc::clone(index))
            })
            .transpose()?;
        Ok(Self {
            annotations: annotation::Engine::default(),
            corpus: VectorCorpus::mint(),
            armory: Some(Basemap::spawn(ctx.clone(), source, !offline)?),
            tiles: VectorBank::new(VECTOR_CEILING),
            presented: Arc::from([]),
            prewarm: Arc::from([]),
            transition: None,
            inflight: HashMap::new(),
            missing: HashSet::new(),
            retries: HashMap::new(),
            demand: Vec::new(),
            demand_dirty: true,
            detail: basemap::DetailGovernor::default(),
            cartographic_zoom: 0.0,
            readiness: ReadinessOracle::default(),
            presentation: None,
            presentation_revision: 0,
            archive_zoom: None,
            trails,
            trail_index,
            parking_forge,
            trailhead_parking: HashMap::new(),
            parking_queue: VecDeque::new(),
            parking_queued: HashSet::new(),
        })
    }

    /// Rebind a live field to an expanded project archive without surrendering
    /// its already-presented tiles. Coordinate-addressed Protomaps tiles are
    /// reusable across region-union archives; only acquisition and trail-aware
    /// parking law change.
    pub fn retarget(
        &mut self,
        ctx: &egui::Context,
        source: Source,
        offline: bool,
        graph: Arc<WalkGraph>,
        index: Arc<EdgeIndex>,
    ) -> Result<RetiredTrailArmament> {
        let armory = Basemap::spawn(ctx.clone(), source, !offline)?;
        self.armory = Some(armory);
        self.inflight.clear();
        self.missing.clear();
        self.retries.clear();
        self.demand.clear();
        self.demand_dirty = true;
        self.archive_zoom = None;
        self.readiness = ReadinessOracle::default();
        self.presentation = None;
        self.prewarm = Arc::from([]);
        self.bind_trails(ctx, graph, index)
    }

    /// Add trail-aware wayfinding to an already presented basemap without
    /// replacing its source, resident tiles, or GPU corpus.
    pub fn bind_trails(
        &mut self,
        ctx: &egui::Context,
        graph: Arc<WalkGraph>,
        index: Arc<EdgeIndex>,
    ) -> Result<RetiredTrailArmament> {
        let parking_forge =
            ParkingForge::spawn(ctx.clone(), Arc::clone(&graph), Arc::clone(&index))?;
        self.parking_forge = Some(parking_forge);
        self.parking_queue.clear();
        self.parking_queued.clear();
        for tile in self
            .tiles
            .tiles
            .values()
            .map(|entry| Arc::clone(&entry.tile))
        {
            if !tile.parking.is_empty() && self.parking_queued.insert(tile.key) {
                self.parking_queue.push_back(tile);
            }
        }
        let prior_graph = self.trails.replace(graph);
        let prior_index = self.trail_index.replace(index);
        ctx.request_repaint();
        Ok((prior_graph, prior_index))
    }

    #[must_use]
    pub fn has_presented_tiles(&self) -> bool {
        !self.presented.is_empty()
    }

    #[cfg(feature = "egui-test")]
    #[must_use]
    pub fn presented_tile_count(&self) -> usize {
        self.presented.len()
    }

    pub fn absorb(&mut self, ctx: &egui::Context) {
        let _phase = tracing::info_span!(
            target: "eternalist::product",
            "basemap.absorb"
        )
        .entered();
        if self.armory.is_none() {
            return;
        }
        let begun = Instant::now();
        let mut absorbed = 0;
        while absorbed < ABSORB_LIMIT
            && begun.elapsed() < ABSORB_BUDGET
            && let Some(event) = self
                .armory
                .as_ref()
                .and_then(|armory| armory.events.try_recv().ok())
        {
            absorbed += 1;
            match event {
                basemap::Event::Ready { source_zoom } => {
                    self.archive_zoom = Some(source_zoom);
                }
                basemap::Event::Relinquished(keys) => {
                    for key in keys {
                        self.inflight.remove(&key);
                    }
                }
                basemap::Event::Loaded(tile) => {
                    let key = tile.key;
                    if let Some(begun) = self.inflight.remove(&key) {
                        self.readiness.observe(begun.elapsed());
                    }
                    self.retries.remove(&key);
                    self.enqueue_parking(&tile);
                    self.tiles.insert(tile);
                }
                basemap::Event::Missing(key) => {
                    self.inflight.remove(&key);
                    self.retries.remove(&key);
                    self.missing.insert(key);
                }
                basemap::Event::Fault { key, message } => {
                    eprintln!("basemap unavailable: {message}");
                    if let Some(key) = key {
                        self.inflight.remove(&key);
                        let retry = Retry::fail(self.retries.get(&key), Instant::now());
                        self.retries.insert(key, retry);
                    }
                }
            }
            self.demand_dirty = true;
        }
        if self
            .armory
            .as_ref()
            .is_some_and(|armory| !armory.events.is_empty())
        {
            ctx.request_repaint();
        }
        self.absorb_parking(ctx);
        let parking_tiles = self.trailhead_parking.len();
        self.trailhead_parking
            .retain(|key, _| self.tiles.contains(*key));
        if parking_tiles != self.trailhead_parking.len() {
            self.presentation_revision = self.presentation_revision.saturating_add(1);
        }
    }

    pub fn paint_base(
        &mut self,
        painter: &Painter,
        frame: MapFramePlan,
        cartography: CartographicPlan,
    ) {
        self.resolve(frame, cartography, painter.ctx());
        self.submit(painter, frame, GeometryPass::Both, Arc::from([]));
    }

    pub fn paint_fills(
        &mut self,
        painter: &Painter,
        frame: MapFramePlan,
        cartography: CartographicPlan,
    ) {
        self.resolve(frame, cartography, painter.ctx());
        self.submit(painter, frame, GeometryPass::Fills, Arc::from([]));
    }

    pub fn paint_strokes(
        &self,
        painter: &Painter,
        frame: MapFramePlan,
        gaps: Arc<[crate::vector_map::VectorGap]>,
    ) {
        self.submit(painter, frame, GeometryPass::Strokes, gaps);
    }

    fn resolve(&mut self, frame: MapFramePlan, cartography: CartographicPlan, ctx: &egui::Context) {
        let _phase = tracing::info_span!(
            target: "eternalist::product",
            "basemap.resolve"
        )
        .entered();
        self.retire_mature_transition();
        if self.armory.is_none() {
            return;
        }
        self.cartographic_zoom = frame.zoom.get() as f32;
        let bank_revision = self.tiles.revision();
        if !self.demand_dirty
            && self
                .presentation
                .as_ref()
                .is_some_and(|stamp| stamp.frame == frame && stamp.bank_revision == bank_revision)
        {
            return;
        }
        let detail =
            self.detail
                .resolve(frame.zoom.get(), self.readiness.estimate(), Instant::now());
        let cover = basemap::cover(frame, detail, self.archive_zoom, self.trails.is_some());
        self.demand_cover(&cover, ctx);
        if self.presentation.as_ref().is_some_and(|stamp| {
            stamp.frame == frame
                && stamp.source == cover.source
                && stamp.cells == cover.cells
                && stamp.bank_revision == bank_revision
        }) {
            return;
        }
        let preferred = self
            .presented
            .iter()
            .filter_map(|patch| patch.cell.map(|cell| (cell, patch.tile.key)))
            .collect::<HashMap<_, _>>();
        let choices = choose_residents(
            &cover.cells,
            cover.source,
            !cartography.moving,
            &preferred,
            |key| self.tiles.contains(key),
        );
        let mut resident = choices.iter().map(|(_, key)| *key).collect::<Vec<_>>();
        resident.sort_unstable();
        resident.dedup();
        let resident = resident
            .into_iter()
            .filter_map(|key| self.tiles.get(key).cloned().map(|tile| (key, tile)))
            .collect::<HashMap<_, _>>();
        let patches = choices
            .into_iter()
            .filter_map(|(cell, key)| {
                resident
                    .get(&key)
                    .cloned()
                    .map(|tile| VectorPatch::clipped(tile, cell))
            })
            .collect::<Vec<_>>();
        let chosen = patches
            .iter()
            .map(|patch| patch.tile.key)
            .collect::<HashSet<_>>();
        let mut prewarm = cover
            .cells
            .iter()
            .copied()
            .filter(|cell| !chosen.contains(&cell.key))
            .filter_map(|cell| {
                self.tiles
                    .get(cell.key)
                    .cloned()
                    .map(|tile| VectorPatch::clipped(tile, cell))
            })
            .collect::<Vec<_>>();
        prewarm.sort_unstable_by_key(|patch| patch.tile.key);
        prewarm.dedup_by_key(|patch| patch.tile.key);
        self.prewarm = prewarm.into();
        if !same_patches(&patches, &self.presented) {
            if !cartography.moving
                && !self.presented.is_empty()
                && patch_sources_differ(&patches, &self.presented)
            {
                self.transition = Some(PresentationTransition {
                    prior: Arc::clone(&self.presented),
                    begun: Instant::now(),
                });
            }
            self.presented = patches.into();
            self.presentation_revision = self.presentation_revision.saturating_add(1);
        }
        self.presentation = Some(PresentationStamp {
            frame,
            source: cover.source,
            cells: cover.cells,
            bank_revision,
        });
    }

    fn retire_mature_transition(&mut self) {
        if self
            .transition
            .as_ref()
            .is_some_and(|transition| transition.begun.elapsed() >= PRESENTATION_TRANSITION)
        {
            self.transition = None;
        }
    }

    fn submit(
        &self,
        painter: &Painter,
        frame: MapFramePlan,
        geometry: GeometryPass,
        gaps: Arc<[crate::vector_map::VectorGap]>,
    ) {
        if !self.presented.is_empty() {
            let patches = self.presentation_layers(painter);
            painter.add(egui_wgpu::Callback::new_paint_callback(
                frame.rect,
                VectorPaint {
                    layer: VectorLayer::Basemap,
                    corpus: self.corpus,
                    geometry,
                    gaps,
                    patches,
                    prewarm: Arc::clone(&self.prewarm),
                    repaint: painter.ctx().clone(),
                    center_world: frame.viewport.center,
                    world_points: frame.world_points as f32,
                    viewport_points: [frame.rect.width(), frame.rect.height()],
                    view_zoom: self.cartographic_zoom,
                    apparition_span: basemap::APPARITION_SPAN,
                },
            ));
        }
    }

    fn presentation_layers(&self, painter: &Painter) -> Arc<[VectorPatch]> {
        let Some(transition) = &self.transition else {
            return Arc::clone(&self.presented);
        };
        let maturity = smooth_transition(
            transition.begun.elapsed().as_secs_f32() / PRESENTATION_TRANSITION.as_secs_f32(),
        );
        if maturity >= 1.0 {
            return Arc::clone(&self.presented);
        }
        painter.ctx().request_repaint();
        transition
            .prior
            .iter()
            .cloned()
            .map(|patch| patch.with_opacity(1.0))
            .chain(
                self.presented
                    .iter()
                    .cloned()
                    .map(|patch| patch.with_opacity(maturity)),
            )
            .collect()
    }

    pub fn paint_annotations<'a, F>(
        &'a mut self,
        painter: &Painter,
        frame: MapFramePlan,
        cartography: CartographicPlan,
        relief_revision: u64,
        relief: F,
    ) where
        F: FnOnce() -> Vec<annotation::LineLabel<'a>>,
    {
        self.compose_annotations(painter, frame, cartography, relief_revision, relief)
            .paint(painter);
    }

    pub fn compose_annotations<'a, F>(
        &'a mut self,
        painter: &Painter,
        frame: MapFramePlan,
        cartography: CartographicPlan,
        relief_revision: u64,
        relief: F,
    ) -> Arc<annotation::Composition>
    where
        F: FnOnce() -> Vec<annotation::LineLabel<'a>>,
    {
        let _phase = tracing::info_span!(
            target: "eternalist::product",
            "basemap.annotations"
        )
        .entered();
        let stamp = annotation::Stamp {
            epoch: cartography.epoch,
            presentation: self.presentation_revision,
            relief: relief_revision,
        };
        if (cartography.moving && self.annotations.inhabited()) || self.annotations.coherent(stamp)
        {
            return self
                .annotations
                .project(painter, frame.viewport, frame.rect);
        }
        let points = self
            .presented
            .iter()
            .flat_map(|patch| {
                patch
                    .tile
                    .labels
                    .iter()
                    .filter(|label| patch.contains(label.world))
            })
            .map(|label| annotation::PointLabel {
                world: label.world,
                text: label.text.as_ref(),
                kind: label.kind,
                rank: label.rank,
                size: label.size,
                onset_zoom: label.onset_zoom,
            });
        let roads = self
            .presented
            .iter()
            .flat_map(|patch| {
                patch.tile.line_labels.iter().filter(|label| {
                    label
                        .path
                        .get(label.path.len() / 2)
                        .is_some_and(|world| patch.contains(*world))
                })
            })
            .map(|label| annotation::LineLabel {
                path: &label.path,
                text: label.text.as_ref(),
                rank: label.rank,
                size: label.size + 1.0,
                onset_zoom: label.onset_zoom,
                ink: Color32::BLACK,
                halo: None,
                repeatable: true,
                break_line: false,
            });
        let mut parking = self
            .trailhead_parking
            .values()
            .flat_map(|parking| parking.iter())
            .collect::<Vec<_>>();
        parking.sort_unstable_by(|left, right| {
            left.world[1]
                .total_cmp(&right.world[1])
                .then_with(|| left.world[0].total_cmp(&right.world[0]))
        });
        let parking = parking.into_iter().map(|parking| annotation::Parking {
            world: parking.world,
            name: parking.name.as_deref(),
            onset_zoom: parking.onset_zoom,
        });
        self.annotations.reconcile(
            painter,
            annotation::Reconciliation {
                frame,
                cartography,
                stamp,
            },
            points,
            roads.chain(relief()),
            parking,
        )
    }

    fn enqueue_parking(&mut self, tile: &Arc<VectorTile>) {
        if self.parking_forge.is_none() {
            return;
        }
        if tile.parking.is_empty() {
            if self.trailhead_parking.remove(&tile.key).is_some() {
                self.presentation_revision = self.presentation_revision.saturating_add(1);
            }
            return;
        }
        if self.parking_queued.insert(tile.key) {
            self.parking_queue.push_back(Arc::clone(tile));
        }
    }

    fn absorb_parking(&mut self, ctx: &egui::Context) {
        let Some(forge) = &self.parking_forge else {
            return;
        };
        for _ in 0..PARKING_DRAIN_LIMIT {
            let Ok(armament) = forge.events.try_recv() else {
                break;
            };
            self.parking_queued.remove(&armament.key);
            if armament.parking.is_empty() {
                self.trailhead_parking.remove(&armament.key);
            } else {
                self.trailhead_parking
                    .insert(armament.key, armament.parking);
            }
            self.presentation_revision = self.presentation_revision.saturating_add(1);
        }
        while let Some(tile) = self.parking_queue.front() {
            match forge.command.try_send(Arc::clone(tile)) {
                Ok(()) => {
                    self.parking_queue.pop_front();
                }
                Err(TrySendError::Full(_)) => break,
                Err(TrySendError::Disconnected(_)) => {
                    self.parking_queue.clear();
                    self.parking_queued.clear();
                    break;
                }
            }
        }
        if !self.parking_queue.is_empty() || !forge.events.is_empty() {
            ctx.request_repaint();
        }
    }

    fn demand_cover(&mut self, cover: &basemap::Cover, ctx: &egui::Context) {
        let demand = cover.demand_order();
        let changed = demand != self.demand;
        if changed {
            if let Some(armory) = &self.armory {
                for key in armory.preempt() {
                    self.inflight.remove(&key);
                }
            }
            self.demand = demand;
        }
        if !changed && !std::mem::take(&mut self.demand_dirty) {
            return;
        }
        let mut unsettled = false;
        for key in self.demand.clone() {
            unsettled |= self.demand(key, ctx);
        }
        self.demand_dirty = unsettled;
    }

    fn demand(&mut self, key: TileKey, ctx: &egui::Context) -> bool {
        let Some(armory) = &self.armory else {
            return false;
        };
        if self.tiles.contains(key)
            || self.inflight.contains_key(&key)
            || self.missing.contains(&key)
        {
            return false;
        }
        if let Some(retry) = self.retries.get(&key) {
            let delay = retry.after.saturating_duration_since(Instant::now());
            if !delay.is_zero() {
                ctx.request_repaint_after(delay);
                return true;
            }
        }
        if armory.request(key) {
            self.inflight.insert(key, Instant::now());
            false
        } else {
            ctx.request_repaint();
            true
        }
    }
}

fn choose_residents(
    cells: &[basemap::TileCell],
    source: basemap::SourceLevel,
    permit_upgrade: bool,
    preferred: &HashMap<basemap::TileCell, TileKey>,
    mut resident: impl FnMut(TileKey) -> bool,
) -> Vec<(basemap::TileCell, TileKey)> {
    let coherent = permit_upgrade && cells.iter().all(|cell| resident(cell.key));
    cells
        .iter()
        .copied()
        .filter_map(|cell| {
            if coherent {
                return Some((cell, cell.key));
            }
            if let Some(&key) = preferred.get(&cell)
                && resident(key)
            {
                return Some((cell, key));
            }
            (0..=source.get()).rev().find_map(|level| {
                let key = cell.key.ancestor(basemap::SourceLevel::new(level));
                resident(key).then_some((cell, key))
            })
        })
        .collect()
}

fn same_patches(left: &[VectorPatch], right: &[VectorPatch]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| Arc::ptr_eq(&left.tile, &right.tile) && left.cell == right.cell)
}

fn patch_sources_differ(left: &[VectorPatch], right: &[VectorPatch]) -> bool {
    left.len() != right.len()
        || left
            .iter()
            .zip(right)
            .any(|(left, right)| left.tile.key != right.tile.key)
}

fn smooth_transition(phase: f32) -> f32 {
    let phase = phase.clamp(0.0, 1.0);
    phase * phase * 2.0_f32.mul_add(-phase, 3.0)
}

fn abuts_trail(trails: &WalkGraph, index: &EdgeIndex, parking: &basemap::Parking) -> bool {
    index
        .project(trails, map::world_to_coord(parking.world))
        .is_some_and(|projection| projection.distance_m <= TRAILHEAD_PARKING_REACH_M)
}

struct VectorBank {
    ceiling: usize,
    bytes: usize,
    epoch: u64,
    revision: u64,
    tiles: HashMap<TileKey, VectorEntry>,
    order: VecDeque<(TileKey, u64)>,
}

struct VectorEntry {
    tile: Arc<VectorTile>,
    bytes: usize,
    touched: u64,
}

impl VectorBank {
    fn new(ceiling: usize) -> Self {
        Self {
            ceiling,
            bytes: 0,
            epoch: 0,
            revision: 0,
            tiles: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn contains(&self, key: TileKey) -> bool {
        self.tiles.contains_key(&key)
    }

    const fn revision(&self) -> u64 {
        self.revision
    }

    fn get(&mut self, key: TileKey) -> Option<&Arc<VectorTile>> {
        self.epoch = self.epoch.saturating_add(1);
        let entry = self.tiles.get_mut(&key)?;
        entry.touched = self.epoch;
        self.order.push_back((key, self.epoch));
        Some(&entry.tile)
    }

    fn insert(&mut self, tile: Arc<VectorTile>) {
        let key = tile.key;
        let bytes = tile.resident_bytes();
        self.epoch = self.epoch.saturating_add(1);
        self.revision = self.revision.saturating_add(1);
        let fresh = VectorEntry {
            tile,
            bytes,
            touched: self.epoch,
        };
        self.order.push_back((key, self.epoch));
        if let Some(prior) = self.tiles.insert(key, fresh) {
            self.bytes = self.bytes.saturating_sub(prior.bytes);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        while self.bytes > self.ceiling && self.tiles.len() > 1 {
            let Some((victim, epoch)) = self.order.pop_front() else {
                break;
            };
            if self
                .tiles
                .get(&victim)
                .is_none_or(|entry| entry.touched != epoch)
            {
                continue;
            }
            let Some(victim) = self.tiles.remove(&victim) else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(victim.bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use trailgen_core::{GraphBuilder, io::geojson};

    #[test]
    fn transient_vector_faults_back_off_monotonically_and_cap() {
        let now = Instant::now();
        let mut retry = Retry::fail(None, now);
        let mut prior = retry.after;
        for _ in 0..32 {
            retry = Retry::fail(Some(&retry), now);
            assert!(retry.after >= prior);
            prior = retry.after;
        }
        assert_eq!(retry.after.duration_since(now), RETRY_CEILING);
    }

    #[test]
    fn demand_serves_detail_and_fallback_before_speculation() {
        fn stratum(intent: basemap::Intent, zoom: u8) -> basemap::Stratum {
            basemap::Stratum {
                intent,
                keys: vec![TileKey { zoom, x: 0, y: 0 }],
            }
        }
        let cover = basemap::Cover {
            source: basemap::SourceLevel::new(10),
            cells: Vec::new(),
            strata: vec![
                stratum(basemap::Intent::Fallback, 7),
                stratum(basemap::Intent::Required, 10),
                stratum(basemap::Intent::Prefetch, 11),
            ],
        };

        assert_eq!(
            {
                cover
                    .demand_order()
                    .into_iter()
                    .map(|key| key.zoom)
                    .collect::<Vec<_>>()
            },
            [7, 10, 11]
        );
    }

    #[test]
    fn each_visible_cell_selects_its_own_finest_resident_ancestor() {
        let frame = MapFramePlan::forge(
            map::Viewport {
                center: [0.5, 0.5],
                zoom: 10.0,
            },
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1_200.0, 800.0)),
        );
        let cover = basemap::cover(
            frame,
            basemap::DetailPlan {
                source: basemap::SourceLevel::new(9),
                prefetch: false,
            },
            None,
            false,
        );
        assert!(cover.cells.len() > 1);
        let exact = cover.cells[0].key;
        let fallback = cover.cells[1]
            .key
            .ancestor(basemap::SourceLevel::new(cover.source.get() - 1));
        let choices = choose_residents(
            &cover.cells[..2],
            cover.source,
            true,
            &HashMap::new(),
            |key| key == exact || key == fallback,
        );
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].1, exact);
        assert_eq!(choices[1].1, fallback);
    }

    #[test]
    fn source_cover_upgrades_only_when_every_visible_cell_is_ready() {
        let frame = MapFramePlan::forge(
            map::Viewport {
                center: [0.5, 0.5],
                zoom: 10.0,
            },
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1_200.0, 800.0)),
        );
        let cover = basemap::cover(
            frame,
            basemap::DetailPlan {
                source: basemap::SourceLevel::new(9),
                prefetch: false,
            },
            None,
            false,
        );
        let cells = &cover.cells[..2];
        let parent = basemap::SourceLevel::new(8);
        let preferred = cells
            .iter()
            .map(|cell| (*cell, cell.key.ancestor(parent)))
            .collect::<HashMap<_, _>>();
        let mut resident = preferred.values().copied().collect::<HashSet<_>>();
        resident.insert(cells[0].key);

        let partial = choose_residents(cells, cover.source, true, &preferred, |key| {
            resident.contains(&key)
        });
        assert!(partial.iter().all(|(cell, key)| *key == preferred[cell]));

        resident.extend(cells.iter().map(|cell| cell.key));
        let coherent = choose_residents(cells, cover.source, true, &preferred, |key| {
            resident.contains(&key)
        });
        assert!(coherent.iter().all(|(cell, key)| *key == cell.key));
    }

    #[test]
    fn readiness_oracle_rises_immediately_and_recedes_gradually() {
        let mut oracle = ReadinessOracle::default();
        oracle.observe(Duration::from_millis(800));
        assert_eq!(oracle.estimate(), Duration::from_millis(800));
        oracle.observe(Duration::from_millis(80));
        assert!(oracle.estimate() > Duration::from_millis(600));
    }

    #[test]
    fn parking_becomes_a_trailhead_mark_only_when_it_abuts_the_graph() -> Result<()> {
        let graph = GraphBuilder::default().build(&geojson::network_from_str(include_str!(
            "../../trailgen-core/tests/fixtures/mini_network.geojson"
        ))?)?;
        let beside = basemap::Parking {
            world: map::world_from_coord(graph.edges[0].geometry.points[0]),
            name: None,
            onset_zoom: 15.0,
        };
        let remote = basemap::Parking {
            world: map::world_from_coord(trailgen_core::Coord::new(-120.0, 30.0)),
            ..beside.clone()
        };
        let index = EdgeIndex::forge(&graph);
        assert!(abuts_trail(&graph, &index, &beside));
        assert!(!abuts_trail(&graph, &index, &remote));
        Ok(())
    }

    #[test]
    fn trailhead_parking_projection_is_forged_off_the_event_loop() -> Result<()> {
        let graph = Arc::new(GraphBuilder::default().build(&geojson::network_from_str(
            include_str!("../../trailgen-core/tests/fixtures/mini_network.geojson"),
        )?)?);
        let beside = basemap::Parking {
            world: map::world_from_coord(graph.edges[0].geometry.points[0]),
            name: None,
            onset_zoom: 15.0,
        };
        let remote = basemap::Parking {
            world: map::world_from_coord(trailgen_core::Coord::new(-120.0, 30.0)),
            ..beside.clone()
        };
        let forge = ParkingForge::spawn(
            egui::Context::default(),
            Arc::clone(&graph),
            Arc::new(EdgeIndex::forge(&graph)),
        )?;
        let key = TileKey {
            zoom: 12,
            x: 1_205,
            y: 1_539,
        };
        forge.command.send(Arc::new(VectorTile {
            key,
            fills: basemap::Mesh::default(),
            strokes: basemap::Mesh::default(),
            labels: Arc::from([]),
            line_labels: Arc::from([]),
            parking: Arc::from([beside, remote]),
        }))?;

        let armament = forge.events.recv_timeout(Duration::from_secs(2))?;
        assert_eq!(armament.key, key);
        assert_eq!(armament.parking.len(), 1);
        Ok(())
    }
}
