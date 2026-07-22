use super::{Event, SurveyRegion, fingerprint, read_bounded, write_atomic};
use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use trailgen_core::{
    Coord, ElevationSample, ElevationSampler, Provenance, source::SourceFingerprint,
};

const DEFAULT_ENDPOINT: &str = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium";
const PREFERRED_ZOOM: u8 = 12;
const MINIMUM_ZOOM: u8 = 8;
const MAX_TILES: usize = 256;
const MAX_TILE_BYTES: u64 = 4 * 1024 * 1024;
const PLAUSIBLE_ELEVATION_M: std::ops::RangeInclusive<f64> = -150.0..=9_000.0;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TerrainTileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TerrainTileId {
    fn relative_path(self) -> PathBuf {
        PathBuf::from("sources/mapzen-terrain")
            .join(self.z.to_string())
            .join(self.x.to_string())
            .join(format!("{}.png", self.y))
    }

    fn url(self, endpoint: &str) -> String {
        format!(
            "{}/{}/{}/{}.png",
            endpoint.trim_end_matches('/'),
            self.z,
            self.x,
            self.y
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TerrainReceipt {
    pub tile: TerrainTileId,
    pub raw_path: PathBuf,
    pub raw: SourceFingerprint,
}

pub struct TerrainSource {
    pub receipt: TerrainReceipt,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct TopographicTile {
    pub id: TerrainTileId,
    pub width: u32,
    pub height: u32,
    elevations_m: Arc<[f32]>,
}

impl TopographicTile {
    #[must_use]
    pub fn elevation(&self, x: u32, y: u32) -> Option<f32> {
        (x < self.width && y < self.height)
            .then(|| self.elevations_m[(y as usize * self.width as usize) + x as usize])
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "sub-centimeter DEM precision is immaterial to ten-meter isohypses"
)]
pub fn topographic_tile(project: &Path, receipt: &TerrainReceipt) -> Result<TopographicTile> {
    let path = project.join(&receipt.raw_path);
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    ensure!(
        fingerprint(&bytes) == receipt.raw,
        "topographic tile {} does not match its index receipt",
        path.display()
    );
    let tile = Tile::decode(&bytes).with_context(|| format!("decode {}", path.display()))?;
    let mut elevations_m = Vec::with_capacity(tile.width as usize * tile.height as usize);
    for y in 0..tile.height {
        for x in 0..tile.width {
            elevations_m.push(tile.elevation(x, y) as f32);
        }
    }
    Ok(TopographicTile {
        id: receipt.tile,
        width: tile.width,
        height: tile.height,
        elevations_m: elevations_m.into(),
    })
}

pub fn acquire(
    project: &Path,
    regions: &[SurveyRegion],
    fetch_missing: bool,
    emit: &mut impl FnMut(Event),
) -> Result<Vec<TerrainSource>> {
    let tiles = desired_tiles(regions);
    let total = tiles.len();
    let endpoint =
        env::var("TRAILGEN_TERRAIN_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_owned());
    let mut client = None;
    let mut sources = Vec::with_capacity(tiles.len());
    emit(Event::Elevating { complete: 0, total });
    for (slot, tile) in tiles.into_iter().enumerate() {
        let relative = tile.relative_path();
        let path = project.join(&relative);
        let cached = fs::read(&path)
            .ok()
            .filter(|bytes| Tile::decode(bytes).is_ok());
        let bytes = if let Some(bytes) = cached {
            bytes
        } else {
            ensure!(
                fetch_missing,
                "terrain tile z{}/x{}/y{} is absent or corrupt",
                tile.z,
                tile.x,
                tile.y
            );
            let client = client.get_or_insert_with(|| {
                reqwest::blocking::Client::builder()
                    .timeout(Duration::from_mins(1))
                    .user_agent(concat!("trailgen/", env!("CARGO_PKG_VERSION")))
                    .build()
                    .expect("static terrain client configuration is valid")
            });
            let response = client
                .get(tile.url(&endpoint))
                .send()
                .with_context(|| format!("fetch terrain tile z{}/x{}/y{}", tile.z, tile.x, tile.y))?
                .error_for_status()
                .with_context(|| {
                    format!(
                        "terrain provider rejected z{}/x{}/y{}",
                        tile.z, tile.x, tile.y
                    )
                })?;
            let bytes = read_bounded(response, MAX_TILE_BYTES, "terrain tile")?;
            Tile::decode(&bytes).context("decode terrain provider response")?;
            write_atomic(&path, &bytes)?;
            bytes
        };
        sources.push(TerrainSource {
            receipt: TerrainReceipt {
                tile,
                raw_path: relative,
                raw: fingerprint(&bytes),
            },
            bytes,
        });
        emit(Event::Elevating {
            complete: slot + 1,
            total,
        });
    }
    Ok(sources)
}

pub fn desired_tiles(regions: &[SurveyRegion]) -> Vec<TerrainTileId> {
    for zoom in (MINIMUM_ZOOM..=PREFERRED_ZOOM).rev() {
        let tiles = regions
            .iter()
            .flat_map(|region| tiles_for_bounds(region, zoom))
            .collect::<BTreeSet<_>>();
        if tiles.len() <= MAX_TILES || zoom == MINIMUM_ZOOM {
            return tiles.into_iter().collect();
        }
    }
    unreachable!("terrain zoom interval is nonempty")
}

fn tiles_for_bounds(region: &SurveyRegion, z: u8) -> Vec<TerrainTileId> {
    let north_west = tile_at(Coord::new(region.bounds.west, region.bounds.north), z);
    let south_east = tile_at(Coord::new(region.bounds.east, region.bounds.south), z);
    (north_west.x..=south_east.x)
        .flat_map(move |x| (north_west.y..=south_east.y).map(move |y| TerrainTileId { z, x, y }))
        .collect()
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn tile_at(coord: Coord, z: u8) -> TerrainTileId {
    let n = f64::from(1_u32 << z);
    let x = ((coord.lon + 180.0) / 360.0 * n)
        .floor()
        .clamp(0.0, n - 1.0) as u32;
    let latitude = coord.lat.clamp(-85.051_128_78, 85.051_128_78).to_radians();
    let y = ((1.0 - latitude.tan().asinh() / std::f64::consts::PI) * 0.5 * n)
        .floor()
        .clamp(0.0, n - 1.0) as u32;
    TerrainTileId { z, x, y }
}

pub struct TerrainAtlas {
    zoom: u8,
    tiles: BTreeMap<(u32, u32), Tile>,
}

impl TerrainAtlas {
    pub fn decode(sources: &[TerrainSource]) -> Result<Option<Self>> {
        let Some(zoom) = sources.first().map(|source| source.receipt.tile.z) else {
            return Ok(None);
        };
        ensure!(
            sources.iter().all(|source| source.receipt.tile.z == zoom),
            "terrain corpus mixes zoom levels"
        );
        let tiles = sources
            .iter()
            .map(|source| {
                let id = source.receipt.tile;
                Ok(((id.x, id.y), Tile::decode(&source.bytes)?))
            })
            .collect::<Result<_>>()?;
        Ok(Some(Self { zoom, tiles }))
    }
}

impl ElevationSampler for TerrainAtlas {
    #[allow(clippy::cast_possible_truncation)]
    fn sample(&self, coord: Coord) -> Option<ElevationSample> {
        let id = tile_at(coord, self.zoom);
        let tile = self.tiles.get(&(id.x, id.y))?;
        let n = f64::from(1_u32 << self.zoom);
        let world_x = (coord.lon + 180.0) / 360.0 * n;
        let latitude = coord.lat.clamp(-85.051_128_78, 85.051_128_78).to_radians();
        let world_y = (1.0 - latitude.tan().asinh() / std::f64::consts::PI) * 0.5 * n;
        let pixel_x = world_x.mul_add(f64::from(tile.width), -0.5);
        let pixel_y = world_y.mul_add(f64::from(tile.height), -0.5);
        let x_floor = pixel_x.floor();
        let y_floor = pixel_y.floor();
        let x0 = x_floor as i64;
        let y0 = y_floor as i64;
        let dx = pixel_x - x_floor;
        let dy = pixel_y - y_floor;
        let mut elevation = 0.0;
        let mut weight = 0.0;
        for (x, x_weight) in [(x0, 1.0 - dx), (x0 + 1, dx)] {
            for (y, y_weight) in [(y0, 1.0 - dy), (y0 + 1, dy)] {
                let pixel_weight = x_weight * y_weight;
                if let Some(value) = self.pixel(x, y, tile.width, tile.height)
                    && PLAUSIBLE_ELEVATION_M.contains(&value)
                {
                    elevation = value.mul_add(pixel_weight, elevation);
                    weight += pixel_weight;
                }
            }
        }
        (weight > f64::EPSILON).then(|| ElevationSample {
                ele_m: elevation / weight,
                confidence: 0.82,
                provenance: Provenance {
                    source: "mapzen-terrain-tiles".to_owned(),
                    layer: Some(format!("terrarium-z{}", self.zoom)),
                    source_id: Some(format!("{}/{}/{}", id.z, id.x, id.y)),
                    license: Some(
                        "source-specific; https://github.com/tilezen/joerd/blob/master/docs/attribution.md"
                            .to_owned(),
                    ),
                },
            })
    }
}

impl TerrainAtlas {
    fn pixel(&self, x: i64, y: i64, width: u32, height: u32) -> Option<f64> {
        let side = i64::from(1_u32 << self.zoom);
        let world_width = side * i64::from(width);
        let world_height = side * i64::from(height);
        let x = x.rem_euclid(world_width);
        let y = y.clamp(0, world_height - 1);
        let tile_x = u32::try_from(x / i64::from(width)).ok()?;
        let tile_y = u32::try_from(y / i64::from(height)).ok()?;
        let tile = self.tiles.get(&(tile_x, tile_y))?;
        if tile.width != width || tile.height != height {
            return None;
        }
        Some(tile.elevation(
            u32::try_from(x % i64::from(width)).ok()?,
            u32::try_from(y % i64::from(height)).ok()?,
        ))
    }
}

struct Tile {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl Tile {
    fn decode(bytes: &[u8]) -> Result<Self> {
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().context("read terrain PNG header")?;
        let mut buffer = vec![
            0;
            reader
                .output_buffer_size()
                .context("terrain PNG is too large")?
        ];
        let info = reader
            .next_frame(&mut buffer)
            .context("decode terrain PNG")?;
        let pixels = &buffer[..info.buffer_size()];
        let rgb = match info.color_type {
            png::ColorType::Rgb => pixels.to_vec(),
            png::ColorType::Rgba => pixels
                .chunks_exact(4)
                .flat_map(|pixel| pixel[..3].iter().copied())
                .collect(),
            other => bail!("terrain PNG has unsupported color type {other:?}"),
        };
        ensure!(
            rgb.len() == info.width as usize * info.height as usize * 3,
            "terrain PNG pixel count is inconsistent"
        );
        Ok(Self {
            width: info.width,
            height: info.height,
            rgb,
        })
    }

    fn elevation(&self, x: u32, y: u32) -> f64 {
        let offset = (y as usize * self.width as usize + x as usize) * 3;
        f64::from(self.rgb[offset]).mul_add(
            256.0,
            f64::from(self.rgb[offset + 1]) + f64::from(self.rgb[offset + 2]) / 256.0,
        ) - 32_768.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trailgen_core::source::GeoBounds;

    #[test]
    fn bounded_regions_choose_one_common_zoom_and_tile_set() {
        let regions = [
            SurveyRegion::new(GeoBounds::new(-74.15, 41.15, -74.0, 41.30))
                .expect("valid Harriman bounds"),
        ];
        let tiles = desired_tiles(&regions);
        assert!(!tiles.is_empty() && tiles.len() <= MAX_TILES);
        assert!(tiles.iter().all(|tile| tile.z == tiles[0].z));
    }

    #[test]
    fn terrarium_formula_preserves_fractional_meters() {
        let tile = Tile {
            width: 1,
            height: 1,
            rgb: vec![129, 44, 128],
        };
        assert!((tile.elevation(0, 0) - 300.5).abs() < f64::EPSILON);
    }

    #[test]
    fn atlas_interpolates_between_terrain_pixels() {
        let atlas = TerrainAtlas {
            zoom: 0,
            tiles: BTreeMap::from([(
                (0, 0),
                Tile {
                    width: 2,
                    height: 1,
                    rgb: vec![128, 100, 0, 128, 200, 0],
                },
            )]),
        };
        let sample = atlas.sample(Coord::new(0.0, 0.0)).expect("covered sample");
        assert!((sample.ele_m - 150.0).abs() < f64::EPSILON);
    }

    #[test]
    fn void_and_bathymetric_pixels_do_not_poison_hiking_profiles() {
        let atlas = TerrainAtlas {
            zoom: 0,
            tiles: BTreeMap::from([(
                (0, 0),
                Tile {
                    width: 1,
                    height: 1,
                    rgb: vec![107, 0, 0],
                },
            )]),
        };
        assert!(atlas.sample(Coord::new(-74.0, 41.0)).is_none());
    }
}
