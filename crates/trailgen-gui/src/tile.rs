use crate::{
    habitat::platform_dirs,
    map::{self, Viewport},
};
use anyhow::{Context as _, Result, bail};
use crossbeam_channel::{Receiver, Sender, bounded};
use egui::Context;
use image::GenericImageView as _;
use std::{fs, io::Read as _, path::Path, sync::Arc, thread, time::Duration};

const ORIGIN: &str = "https://basemap.nationalmap.gov/arcgis/rest/services/USGSTopo/MapServer/tile";
const MAX_TILE_ZOOM: u8 = 23;
const RETAINED_DEPTH: u8 = 4;
const WORKERS: usize = 4;
const MAX_TILE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TileKey {
    pub zoom: u8,
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub key: TileKey,
    pub rect: egui::Rect,
}

pub struct Cover {
    pub strata: Vec<Stratum>,
}

impl Cover {
    pub fn finest_ready(&self, mut resident: impl FnMut(TileKey) -> bool) -> Option<&Stratum> {
        self.strata.iter().rev().find(|stratum| {
            stratum.intent.presents()
                && stratum
                    .placements
                    .iter()
                    .all(|placement| resident(placement.key))
        })
    }
}

pub struct Stratum {
    pub intent: Intent,
    pub placements: Vec<Placement>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Intent {
    Retained,
    Required,
    Prefetch,
}

impl Intent {
    pub const fn demands(self) -> bool {
        matches!(self, Self::Required | Self::Prefetch)
    }

    pub const fn presents(self) -> bool {
        !matches!(self, Self::Prefetch)
    }
}

pub enum Event {
    Loaded {
        key: TileKey,
        size: [usize; 2],
        rgba: Arc<[u8]>,
    },
    Fault {
        key: TileKey,
        message: String,
    },
}

pub struct Basemap {
    commands: Sender<TileKey>,
    pub events: Receiver<Event>,
    _threads: Vec<thread::JoinHandle<()>>,
}

impl Basemap {
    pub fn spawn(ctx: &Context) -> Result<Self> {
        let cache = platform_dirs()?.cache_dir().join("usgs-topo");
        fs::create_dir_all(&cache)
            .with_context(|| format!("create tile cache {}", cache.display()))?;
        let (commands, command_rx) = bounded(128);
        let (event_tx, events) = bounded(128);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("adequate-trailgen/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("build USGS topographic tile client")?;
        let mut threads = Vec::with_capacity(WORKERS);
        for slot in 0..WORKERS {
            let rx = command_rx.clone();
            let tx = event_tx.clone();
            let ctx = ctx.clone();
            let cache = cache.clone();
            let client = client.clone();
            threads.push(
                thread::Builder::new()
                    .name(format!("usgs-topo-quarry-{slot}"))
                    .spawn(move || quarry(&ctx, &client, &cache, &rx, &tx))
                    .context("spawn USGS topographic tile quarry")?,
            );
        }
        Ok(Self {
            commands,
            events,
            _threads: threads,
        })
    }

    pub fn request(&self, key: TileKey) -> bool {
        self.commands.try_send(key).is_ok()
    }
}

pub fn cover(view: Viewport, rect: egui::Rect) -> Cover {
    let zoom = view.zoom.floor().clamp(0.0, f64::from(MAX_TILE_ZOOM)) as u8;
    let ceiling = zoom.saturating_add(1).min(MAX_TILE_ZOOM);
    Cover {
        strata: (zoom.saturating_sub(RETAINED_DEPTH)..=ceiling)
            .map(|level| Stratum {
                intent: match level.cmp(&zoom) {
                    std::cmp::Ordering::Less => Intent::Retained,
                    std::cmp::Ordering::Equal => Intent::Required,
                    std::cmp::Ordering::Greater => Intent::Prefetch,
                },
                placements: placements_at(view, rect, level),
            })
            .collect(),
    }
}

fn placements_at(view: Viewport, rect: egui::Rect, zoom: u8) -> Vec<Placement> {
    let divisions = 1_u32 << zoom;
    let bounds = map::world_bounds(view, rect);
    let scale = f64::from(divisions);
    let left = (bounds[0] * scale).floor() as i64;
    let right = (bounds[2] * scale).floor() as i64;
    let top = (bounds[1] * scale).floor().max(0.0) as i64;
    let bottom = (bounds[3] * scale)
        .floor()
        .min(f64::from(divisions.saturating_sub(1))) as i64;
    let mut placements = Vec::new();
    for raw_y in top..=bottom {
        for raw_x in left..=right {
            let x = raw_x.rem_euclid(i64::from(divisions)) as u32;
            let y = raw_y as u32;
            let minimum = [raw_x as f64 / scale, raw_y as f64 / scale];
            let maximum = [(raw_x + 1) as f64 / scale, (raw_y + 1) as f64 / scale];
            placements.push(Placement {
                key: TileKey { zoom, x, y },
                rect: egui::Rect::from_min_max(
                    map::screen_at(view, rect, minimum),
                    map::screen_at(view, rect, maximum),
                ),
            });
        }
    }
    placements.sort_unstable_by(|left, right| {
        left.rect
            .center()
            .distance_sq(rect.center())
            .total_cmp(&right.rect.center().distance_sq(rect.center()))
    });
    placements
}

fn quarry(
    ctx: &Context,
    client: &reqwest::blocking::Client,
    cache: &Path,
    commands: &Receiver<TileKey>,
    events: &Sender<Event>,
) {
    while let Ok(key) = commands.recv() {
        let event = load(client, cache, key).map_or_else(
            |err| Event::Fault {
                key,
                message: format!("{err:#}"),
            },
            |(size, rgba)| Event::Loaded {
                key,
                size,
                rgba: rgba.into(),
            },
        );
        if events.send(event).is_err() {
            break;
        }
        ctx.request_repaint();
    }
}

fn load(
    client: &reqwest::blocking::Client,
    cache: &Path,
    key: TileKey,
) -> Result<([usize; 2], Vec<u8>)> {
    let blade = cache
        .join(key.zoom.to_string())
        .join(key.x.to_string())
        .join(format!("{}.tile", key.y));
    let (bytes, fresh) = match fs::read(&blade) {
        Ok(bytes) => (bytes, false),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let url = format!("{ORIGIN}/{}/{}/{}", key.zoom, key.y, key.x);
            let response = client
                .get(&url)
                .send()
                .with_context(|| format!("fetch USGS tile {key:?}"))?
                .error_for_status()
                .with_context(|| format!("USGS tile service rejected {key:?}"))?;
            let mut bytes = Vec::new();
            response
                .take(MAX_TILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .with_context(|| format!("read USGS tile {key:?}"))?;
            (bytes, true)
        }
        Err(err) => return Err(err).with_context(|| format!("read {}", blade.display())),
    };
    if bytes.len() as u64 > MAX_TILE_BYTES {
        bail!("cached USGS tile {key:?} exceeds {MAX_TILE_BYTES} bytes");
    }
    let image =
        image::load_from_memory(&bytes).with_context(|| format!("decode USGS tile {key:?}"))?;
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 || width > 1_024 || height > 1_024 {
        bail!("USGS tile {key:?} has implausible dimensions {width}×{height}");
    }
    if fresh {
        persist(&blade, &bytes)?;
    }
    Ok((
        [width as usize, height as usize],
        image.to_rgba8().into_raw(),
    ))
}

fn persist(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("tile path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let staging = path.with_extension(format!("part-{}", std::process::id()));
    fs::write(&staging, bytes).with_context(|| format!("write {}", staging.display()))?;
    fs::rename(&staging, path).with_context(|| format!("seal tile cache blade {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_cover_is_bounded_and_nonempty() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1_200.0, 800.0));
        let view = Viewport {
            center: map::world_from_coord(trailgen_core::Coord::new(-74.05, 41.23)),
            zoom: 12.4,
        };
        let cover = cover(view, rect);
        assert_eq!(cover.strata.len(), 6);
        assert_eq!(cover.strata[4].intent, Intent::Required);
        assert_eq!(cover.strata[5].intent, Intent::Prefetch);
        assert!(
            cover.strata.iter().all(|stratum| {
                !stratum.placements.is_empty() && stratum.placements.len() < 200
            })
        );
    }
}
