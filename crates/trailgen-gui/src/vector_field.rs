use crate::{
    annotation,
    basemap::{self, Basemap, Source, TileKey, VectorTile},
    map::{self, Viewport},
    vector_map::{GeometryPass, VectorCorpus, VectorLayer, VectorPaint},
};
use anyhow::Result;
use egui::{Color32, Painter, Rect};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use trailgen_core::TrailGraph;

const VECTOR_CEILING: usize = 512 * 1_048_576;
const RETRY_FLOOR: Duration = Duration::from_millis(250);
const RETRY_CEILING: Duration = Duration::from_secs(30);
const TRAILHEAD_PARKING_REACH_M: f64 = 160.0;

/// The reusable, streaming vector-map plane beneath every trail workbench.
pub struct VectorField {
    annotations: annotation::Compositor,
    corpus: VectorCorpus,
    armory: Option<Basemap>,
    tiles: VectorBank,
    presented: Arc<[Arc<VectorTile>]>,
    inflight: HashSet<TileKey>,
    missing: HashSet<TileKey>,
    retries: HashMap<TileKey, Retry>,
    archive_zoom: Option<u8>,
    trails: Option<Arc<TrailGraph>>,
    trailhead_parking: HashMap<TileKey, Arc<[basemap::Parking]>>,
}

struct Retry {
    failures: u8,
    after: Instant,
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

impl VectorField {
    pub fn raise(
        ctx: &egui::Context,
        source: Source,
        offline: bool,
        trails: Option<Arc<TrailGraph>>,
    ) -> Result<Self> {
        Ok(Self {
            annotations: annotation::Compositor::default(),
            corpus: VectorCorpus::mint(),
            armory: Some(Basemap::spawn(ctx.clone(), source, !offline)?),
            tiles: VectorBank::new(VECTOR_CEILING),
            presented: Arc::from([]),
            inflight: HashSet::new(),
            missing: HashSet::new(),
            retries: HashMap::new(),
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
                    self.inflight.remove(&key);
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
        }
        self.trailhead_parking
            .retain(|key, _| self.tiles.contains(*key));
    }

    pub fn paint_base(&mut self, painter: &Painter, viewport: Viewport, rect: Rect) {
        self.resolve(viewport, rect, painter.ctx());
        self.submit(painter, viewport, rect, GeometryPass::Both);
    }

    pub fn paint_fills(&mut self, painter: &Painter, viewport: Viewport, rect: Rect) {
        self.resolve(viewport, rect, painter.ctx());
        self.submit(painter, viewport, rect, GeometryPass::Fills);
    }

    pub fn paint_strokes(&self, painter: &Painter, viewport: Viewport, rect: Rect) {
        self.submit(painter, viewport, rect, GeometryPass::Strokes);
    }

    fn resolve(&mut self, viewport: Viewport, rect: Rect, ctx: &egui::Context) {
        if self.armory.is_none() {
            return;
        }
        let cover = basemap::cover(viewport, rect, self.archive_zoom);
        self.demand_cover(&cover, ctx);
        let coherent = cover
            .finest_resolved(|key| {
                if self.tiles.contains(key) {
                    basemap::Residency::Resident
                } else if self.missing.contains(&key) {
                    basemap::Residency::Missing
                } else {
                    basemap::Residency::Pending
                }
            })
            .map(|stratum| {
                stratum
                    .keys
                    .iter()
                    .copied()
                    .filter(|key| self.tiles.contains(*key))
                    .collect::<Vec<_>>()
            });
        if let Some(keys) = coherent
            && (keys.len() != self.presented.len()
                || keys
                    .iter()
                    .zip(self.presented.iter())
                    .any(|(key, tile)| *key != tile.key))
        {
            self.presented = keys
                .into_iter()
                .filter_map(|key| self.tiles.get(key).cloned())
                .collect();
        }
    }

    fn submit(&self, painter: &Painter, viewport: Viewport, rect: Rect, geometry: GeometryPass) {
        if !self.presented.is_empty() {
            painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                VectorPaint {
                    layer: VectorLayer::Basemap,
                    corpus: self.corpus,
                    geometry,
                    tiles: Arc::clone(&self.presented),
                    center_world: viewport.center,
                    world_points: map::world_pixels(viewport) as f32,
                    viewport_points: [rect.width(), rect.height()],
                    view_zoom: viewport.zoom as f32,
                    apparition_span: basemap::APPARITION_SPAN,
                },
            ));
        }
    }

    pub fn paint_annotations<'a>(
        &'a mut self,
        painter: &Painter,
        viewport: Viewport,
        rect: Rect,
        relief: impl IntoIterator<Item = annotation::LineLabel<'a>>,
    ) {
        let points = self
            .presented
            .iter()
            .flat_map(|tile| tile.labels.iter())
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
            .flat_map(|tile| tile.line_labels.iter())
            .map(|label| annotation::LineLabel {
                path: &label.path,
                text: label.text.as_ref(),
                rank: label.rank,
                size: label.size,
                onset_zoom: label.onset_zoom,
                ink: Color32::from_rgb(52, 47, 39),
                halo: Color32::from_rgba_unmultiplied(205, 203, 187, 210),
                halo_width: 0.72,
                repeatable: true,
            });
        let parking = self
            .presented
            .iter()
            .filter_map(|tile| self.trailhead_parking.get(&tile.key))
            .flat_map(|parking| parking.iter())
            .map(|parking| annotation::Parking {
                world: parking.world,
                name: parking.name.as_deref(),
                onset_zoom: parking.onset_zoom,
            });
        self.annotations.paint(
            painter,
            viewport,
            rect,
            points,
            roads.chain(relief),
            parking,
        );
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
        for_each_demand(cover, |key| {
            self.demand(key, ctx);
        });
    }

    fn demand(&mut self, key: TileKey, ctx: &egui::Context) {
        let Some(armory) = &self.armory else {
            return;
        };
        if self.tiles.contains(key) || self.inflight.contains(&key) || self.missing.contains(&key) {
            return;
        }
        if let Some(retry) = self.retries.get(&key) {
            let delay = retry.after.saturating_duration_since(Instant::now());
            if !delay.is_zero() {
                ctx.request_repaint_after(delay);
                return;
            }
        }
        if armory.request(key) {
            self.inflight.insert(key);
        }
    }
}

fn abuts_trail(trails: &TrailGraph, parking: &basemap::Parking) -> bool {
    trails
        .nearest_edge_with_distance(map::world_to_coord(parking.world))
        .is_some_and(|(_, distance_m)| distance_m <= TRAILHEAD_PARKING_REACH_M)
}

fn for_each_demand(cover: &basemap::Cover, mut demand: impl FnMut(TileKey)) {
    if let Some(fallback) = cover.strata.first() {
        fallback.keys.iter().copied().for_each(&mut demand);
    }
    cover
        .strata
        .iter()
        .skip(1)
        .filter(|stratum| stratum.intent == basemap::Intent::Required)
        .flat_map(|stratum| stratum.keys.iter().copied())
        .for_each(&mut demand);
    cover
        .strata
        .iter()
        .skip(1)
        .rev()
        .filter(|stratum| stratum.intent == basemap::Intent::Retained)
        .flat_map(|stratum| stratum.keys.iter().copied())
        .for_each(&mut demand);
    cover
        .strata
        .iter()
        .skip(1)
        .filter(|stratum| stratum.intent == basemap::Intent::Prefetch)
        .flat_map(|stratum| stratum.keys.iter().copied())
        .for_each(demand);
}

struct VectorBank {
    ceiling: usize,
    bytes: usize,
    epoch: u64,
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
            tiles: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn contains(&self, key: TileKey) -> bool {
        self.tiles.contains_key(&key)
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
            strata: vec![
                stratum(basemap::Intent::Retained, 7),
                stratum(basemap::Intent::Retained, 8),
                stratum(basemap::Intent::Retained, 9),
                stratum(basemap::Intent::Required, 10),
                stratum(basemap::Intent::Prefetch, 11),
            ],
        };

        assert_eq!(
            {
                let mut order = Vec::new();
                for_each_demand(&cover, |key| order.push(key.zoom));
                order
            },
            [7, 10, 9, 8, 11]
        );
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
