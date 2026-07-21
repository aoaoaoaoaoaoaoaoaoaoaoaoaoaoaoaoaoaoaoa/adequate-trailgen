use crate::{
    basemap::{self, Basemap, Source, TileKey, VectorTile},
    map::{self, Viewport},
    vector_map::VectorPaint,
};
use anyhow::Result;
use egui::{Color32, Painter, Rect, vec2};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

const VECTOR_CEILING: usize = 512 * 1_048_576;
const RETRY_FLOOR: Duration = Duration::from_millis(250);
const RETRY_CEILING: Duration = Duration::from_secs(30);

/// The reusable, streaming vector-map plane beneath every trail workbench.
pub struct VectorField {
    armory: Option<Basemap>,
    tiles: VectorBank,
    presented: Arc<[Arc<VectorTile>]>,
    inflight: HashSet<TileKey>,
    missing: HashSet<TileKey>,
    retries: HashMap<TileKey, Retry>,
    archive_zoom: Option<u8>,
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
    pub fn raise(ctx: &egui::Context, source: Source, offline: bool) -> Result<Self> {
        Ok(Self {
            armory: Some(Basemap::spawn(ctx.clone(), source, !offline)?),
            tiles: VectorBank::new(VECTOR_CEILING),
            presented: Arc::from([]),
            inflight: HashSet::new(),
            missing: HashSet::new(),
            retries: HashMap::new(),
            archive_zoom: None,
        })
    }

    #[must_use]
    pub fn has_presented_tiles(&self) -> bool {
        !self.presented.is_empty()
    }

    pub fn absorb(&mut self) {
        let Some(armory) = &self.armory else {
            return;
        };
        while let Ok(event) = armory.events.try_recv() {
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
    }

    pub fn paint(&mut self, painter: &Painter, viewport: Viewport, rect: Rect) {
        if self.armory.is_none() {
            return;
        }
        let cover = basemap::cover(viewport, rect, self.archive_zoom);
        self.demand_cover(&cover, painter.ctx());
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
        if !self.presented.is_empty() {
            painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                VectorPaint {
                    tiles: Arc::clone(&self.presented),
                    center_world: viewport.center,
                    world_points: map::world_pixels(viewport) as f32,
                    viewport_points: [rect.width(), rect.height()],
                    view_zoom: viewport.zoom as f32,
                    apparition_span: basemap::APPARITION_SPAN,
                },
            ));
        }
        self.paint_labels(painter, viewport, rect);
    }

    fn paint_labels(&self, painter: &Painter, viewport: Viewport, rect: Rect) {
        let mut candidates = self
            .presented
            .iter()
            .flat_map(|tile| tile.labels.iter())
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|label| label.rank);
        let mut occupied = Vec::<Rect>::new();
        for label in candidates {
            let maturity = basemap::apparition(viewport.zoom as f32, label.onset_zoom);
            if maturity <= 0.01 {
                continue;
            }
            let anchor = map::screen_at(viewport, rect, label.world);
            let size = label.size * 0.12_f32.mul_add(maturity, 0.88);
            let width = label.text.chars().count() as f32 * size * 0.58;
            let footprint =
                Rect::from_center_size(anchor, vec2(width.max(size), size * 1.25)).expand(2.0);
            if !rect.contains_rect(footprint)
                || occupied.iter().any(|prior| prior.intersects(footprint))
            {
                continue;
            }
            occupied.push(footprint);
            if occupied.len() >= 180 {
                break;
            }
            let font = egui::FontId::proportional(size);
            let halo = Color32::from_white_alpha((75.0 * maturity) as u8);
            for offset in [
                vec2(-1.0, 0.0),
                vec2(1.0, 0.0),
                vec2(0.0, -1.0),
                vec2(0.0, 1.0),
            ] {
                painter.text(
                    anchor + offset,
                    egui::Align2::CENTER_CENTER,
                    label.text.as_ref(),
                    font.clone(),
                    halo,
                );
            }
            painter.text(
                anchor,
                egui::Align2::CENTER_CENTER,
                label.text.as_ref(),
                font,
                Color32::from_black_alpha((225.0 * maturity) as u8),
            );
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
}
