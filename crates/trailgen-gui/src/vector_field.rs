use crate::{
    annotation,
    basemap::{self, Basemap, Source, TileKey, VectorTile},
    map::{self, CartographicPlan, MapFramePlan},
    vector_map::{GeometryPass, VectorCorpus, VectorLayer, VectorPaint, VectorPatch},
};
use anyhow::Result;
use egui::{Color32, Painter};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use trailgen_core::TrailGraph;

const VECTOR_CEILING: usize = 512 * 1_048_576;
const RETRY_FLOOR: Duration = Duration::from_millis(250);
const RETRY_CEILING: Duration = Duration::from_secs(30);
const READY_LATENCY_SEED: Duration = Duration::from_millis(250);
const TRAILHEAD_PARKING_REACH_M: f64 = 160.0;
const PRESENTATION_TRANSITION: Duration = Duration::from_millis(160);

/// The reusable, streaming vector-map plane beneath every trail workbench.
pub struct VectorField {
    annotations: annotation::Engine,
    corpus: VectorCorpus,
    armory: Option<Basemap>,
    tiles: VectorBank,
    presented: Arc<[VectorPatch]>,
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
    trails: Option<Arc<TrailGraph>>,
    trailhead_parking: HashMap<TileKey, Arc<[basemap::Parking]>>,
}

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
        trails: Option<Arc<TrailGraph>>,
    ) -> Result<Self> {
        Ok(Self {
            annotations: annotation::Engine::default(),
            corpus: VectorCorpus::mint(),
            armory: Some(Basemap::spawn(ctx.clone(), source, !offline)?),
            tiles: VectorBank::new(VECTOR_CEILING),
            presented: Arc::from([]),
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
            trailhead_parking: HashMap::new(),
        })
    }

    #[must_use]
    pub fn has_presented_tiles(&self) -> bool {
        !self.presented.is_empty()
    }

    pub fn absorb(&mut self) {
        if self.armory.is_none() {
            return;
        }
        while let Some(event) = self
            .armory
            .as_ref()
            .and_then(|armory| armory.events.try_recv().ok())
        {
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
                    self.index_parking(&tile);
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
        self.trailhead_parking
            .retain(|key, _| self.tiles.contains(*key));
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
        if self
            .transition
            .as_ref()
            .is_some_and(|transition| transition.begun.elapsed() >= PRESENTATION_TRANSITION)
        {
            self.transition = None;
        }
        if self.armory.is_none() {
            return;
        }
        self.cartographic_zoom = cartography.zoom.get() as f32;
        let bank_revision = self.tiles.revision();
        if !self.demand_dirty
            && self
                .presentation
                .as_ref()
                .is_some_and(|stamp| stamp.frame == frame && stamp.bank_revision == bank_revision)
        {
            return;
        }
        let detail = self.detail.resolve(
            cartography.zoom.get(),
            self.readiness.estimate(),
            Instant::now(),
        );
        let cover = basemap::cover(frame, detail, self.archive_zoom);
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
        let parking = self
            .presented
            .iter()
            .filter_map(|patch| {
                self.trailhead_parking
                    .get(&patch.tile.key)
                    .map(|parking| (patch, parking))
            })
            .flat_map(|(patch, parking)| {
                parking
                    .iter()
                    .filter(|parking| patch.contains(parking.world))
            })
            .map(|parking| annotation::Parking {
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

    fn index_parking(&mut self, tile: &VectorTile) {
        let Some(trails) = &self.trails else { return };
        let parking = tile
            .parking
            .iter()
            .filter(|parking| abuts_trail(trails, parking))
            .cloned()
            .collect::<Arc<[_]>>();
        if parking.is_empty() {
            self.trailhead_parking.remove(&tile.key);
        } else {
            self.trailhead_parking.insert(tile.key, parking);
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

fn abuts_trail(trails: &TrailGraph, parking: &basemap::Parking) -> bool {
    trails
        .nearest_edge_with_distance(map::world_to_coord(parking.world))
        .is_some_and(|(_, distance_m)| distance_m <= TRAILHEAD_PARKING_REACH_M)
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
        assert!(abuts_trail(&graph, &beside));
        assert!(!abuts_trail(&graph, &remote));
        Ok(())
    }
}
