use std::{
    fs::File,
    io::{BufRead as _, BufReader, Cursor, Write as _},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use egui_tester::{AppCommand, Error, Result, Testbed};
use fast_mvt::{DEFAULT_EXTENT, MvtFeature, MvtGeometry, MvtLayer, MvtTile, MvtValue};
use geo_types::{line_string, polygon};
use num_traits::ToPrimitive as _;
use pmtiles::{Compression, PmTilesWriter, TileCoord, TileType};

const PROVIDER_SOCKET: &str = "fixtures/provider.sock";
const MINI_NETWORK: &[u8] =
    include_bytes!("../../trailgen-core/tests/fixtures/mini_network.geojson");

pub struct FixtureWorld {
    server: FixtureServer,
}

impl FixtureWorld {
    pub fn raise(testbed: &Testbed) -> Result<Self> {
        let _mini = testbed.write_private("fixtures/mini_network.geojson", MINI_NETWORK)?;
        let _dense = testbed.write_private("fixtures/dense_network.geojson", dense_network())?;
        let basemap = testbed.private_path("fixtures/basemap.pmtiles")?;
        fixture_basemap(&basemap)?;
        Ok(Self {
            server: FixtureServer::raise(testbed.private_path(PROVIDER_SOCKET)?)?,
        })
    }

    pub fn admit(command: AppCommand) -> AppCommand {
        command
            .env("TRAILGEN_OVERPASS_ENDPOINT", provider_url("overpass"))
            .env("TRAILGEN_USGS_TRAILS_ENDPOINT", provider_url("usgs"))
            .env("TRAILGEN_TERRAIN_ENDPOINT", provider_url("terrain"))
            .env(
                "TRAILGEN_CIVIC_CENSUS_ENDPOINT",
                provider_url("civic/census"),
            )
            .env("TRAILGEN_CIVIC_NYC_ENDPOINT", provider_url("civic/nyc"))
            .private_env("TRAILGEN_HTTP_UNIX_SOCKET", PROVIDER_SOCKET)
    }

    pub fn assert_harvested(&self) -> Result<()> {
        if let Some(fault) = self.server.fault()? {
            return Err(Error::Verdict {
                detail: format!("private provider failed: {fault}"),
            });
        }
        egui_tester::demand(
            self.server.overpass.load(Ordering::Acquire) != 0
                && self.server.usgs.load(Ordering::Acquire) != 0
                && self.server.terrain.load(Ordering::Acquire) != 0
                && self.server.civic.load(Ordering::Acquire) != 0,
            "GUI map-area acquisition did not traverse every private provider",
        )
    }
}

struct FixtureServer {
    socket: PathBuf,
    shutdown: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    overpass: Arc<AtomicUsize>,
    usgs: Arc<AtomicUsize>,
    terrain: Arc<AtomicUsize>,
    civic: Arc<AtomicUsize>,
    fault: Arc<Mutex<Option<String>>>,
}

impl FixtureServer {
    fn raise(socket: PathBuf) -> Result<Self> {
        let listener = UnixListener::bind(&socket).map_err(|source| Error::Io {
            operation: "bind private fixture provider",
            path: socket.clone(),
            source,
        })?;
        listener.set_nonblocking(true).map_err(|source| Error::Io {
            operation: "make private fixture provider nonblocking",
            path: socket.clone(),
            source,
        })?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let overpass = Arc::new(AtomicUsize::new(0));
        let usgs = Arc::new(AtomicUsize::new(0));
        let terrain = Arc::new(AtomicUsize::new(0));
        let civic = Arc::new(AtomicUsize::new(0));
        let fault = Arc::new(Mutex::new(None));
        let worker = {
            let shutdown = Arc::clone(&shutdown);
            let overpass = Arc::clone(&overpass);
            let usgs = Arc::clone(&usgs);
            let terrain = Arc::clone(&terrain);
            let civic = Arc::clone(&civic);
            let fault = Arc::clone(&fault);
            let socket = socket.clone();
            thread::Builder::new()
                .name("trailgen-acceptance-provider".to_owned())
                .spawn(move || {
                    let terrain_png = terrain_tile();
                    while !shutdown.load(Ordering::Acquire) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                if let Err(error) =
                                    serve(stream, &terrain_png, &overpass, &usgs, &terrain, &civic)
                                {
                                    lodge_fault(&fault, error);
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(2));
                            }
                            Err(error) => {
                                lodge_fault(&fault, format!("accept request: {error}"));
                                break;
                            }
                        }
                    }
                    let _removed = std::fs::remove_file(socket);
                })
                .map_err(|source| Error::Io {
                    operation: "spawn private fixture provider",
                    path: PathBuf::from("<thread>"),
                    source,
                })?
        };
        Ok(Self {
            socket,
            shutdown,
            worker: Some(worker),
            overpass,
            usgs,
            terrain,
            civic,
            fault,
        })
    }

    fn fault(&self) -> Result<Option<String>> {
        self.fault
            .lock()
            .map(|fault| fault.clone())
            .map_err(|_| Error::Verdict {
                detail: "private provider fault lock was poisoned".to_owned(),
            })
    }
}

fn provider_url(path: &str) -> String {
    format!("http://trailgen.fixture/{path}")
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _wake = UnixStream::connect(&self.socket);
        if let Some(worker) = self.worker.take() {
            let _joined = worker.join();
        }
    }
}

fn serve(
    stream: UnixStream,
    terrain_png: &[u8],
    overpass: &AtomicUsize,
    usgs: &AtomicUsize,
    terrain: &AtomicUsize,
    civic: &AtomicUsize,
) -> std::result::Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("set request timeout: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("set response timeout: {error}"))?;
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    reader
        .read_line(&mut request)
        .map_err(|error| format!("read request line: {error}"))?;
    loop {
        let mut header = String::new();
        reader
            .read_line(&mut header)
            .map_err(|error| format!("read request header: {error}"))?;
        if matches!(header.as_str(), "\r\n" | "\n" | "") {
            break;
        }
    }
    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let (status, kind, body): (&str, &str, &[u8]) = if path.starts_with("/overpass") {
        overpass.fetch_add(1, Ordering::AcqRel);
        ("200 OK", "application/xml", OSM.as_bytes())
    } else if path.starts_with("/usgs") {
        usgs.fetch_add(1, Ordering::AcqRel);
        ("200 OK", "application/geo+json", EMPTY_USGS.as_bytes())
    } else if path.starts_with("/terrain/") {
        terrain.fetch_add(1, Ordering::AcqRel);
        ("200 OK", "image/png", terrain_png)
    } else if path.starts_with("/civic/") {
        civic.fetch_add(1, Ordering::AcqRel);
        ("200 OK", "application/geo+json", CIVIC_BROOKLYN.as_bytes())
    } else {
        ("404 Not Found", "text/plain", b"not found")
    };
    thread::sleep(Duration::from_millis(40));
    let stream = reader.get_mut();
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .map_err(|error| format!("write response header: {error}"))?;
    stream
        .write_all(body)
        .map_err(|error| format!("write response body: {error}"))?;
    stream
        .flush()
        .map_err(|error| format!("flush response: {error}"))
}

const CIVIC_BROOKLYN: &str = r#"{
  "type": "FeatureCollection",
  "features": [{
    "type": "Feature",
    "properties": { "BoroName": "Brooklyn" },
    "geometry": {
      "type": "Polygon",
      "coordinates": [[
        [-74.050, 40.570],
        [-73.860, 40.570],
        [-73.860, 40.740],
        [-74.050, 40.740],
        [-74.050, 40.570]
      ]]
    }
  }]
}"#;

fn lodge_fault(fault: &Mutex<Option<String>>, detail: String) {
    match fault.lock() {
        Ok(mut fault) => {
            if fault.is_none() {
                *fault = Some(detail);
            }
        }
        Err(poisoned) => {
            let mut fault = poisoned.into_inner();
            if fault.is_none() {
                *fault = Some(detail);
            }
        }
    }
}

fn terrain_tile() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(Cursor::new(&mut bytes), 2, 2);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("static PNG header is valid");
        // Terrarium 128, 100, 0 encodes 100 m.
        writer
            .write_image_data(&[128, 100, 0, 128, 100, 0, 128, 100, 0, 128, 100, 0])
            .expect("static terrain tile is valid");
    }
    bytes
}

fn fixture_basemap(path: &Path) -> Result<()> {
    let file = File::create(path).map_err(|source| Error::Io {
        operation: "create fixture basemap",
        path: path.to_owned(),
        source,
    })?;
    let mut writer = PmTilesWriter::new(TileType::Mvt)
        .internal_compression(Compression::None)
        .tile_compression(Compression::None)
        .min_zoom(0)
        .max_zoom(15)
        .bounds(-180.0, -85.0, 180.0, 85.0)
        .center(-98.5, 39.5)
        .center_zoom(4)
        .metadata(
            r#"{"vector_layers":[{"id":"earth"},{"id":"landcover"},{"id":"water"},{"id":"roads"}]}"#,
        )
        .create(file)
        .map_err(|error| verdict_owned(format!("raise fixture PMTiles writer: {error}")))?;
    let tile = fixture_vector_tile()?;
    for coordinate in fixture_tile_cover()? {
        writer
            .add_tile(coordinate, &tile)
            .map_err(|error| verdict_owned(format!("write fixture vector tile: {error}")))?;
    }
    writer
        .finalize()
        .map_err(|error| verdict_owned(format!("seal fixture PMTiles archive: {error}")))
}

fn fixture_tile_cover() -> Result<Vec<TileCoord>> {
    let world = world_from_coord([-98.5, 39.5]);
    let mut cover = Vec::new();
    for zoom in 0_u8..=15 {
        let side = 1_u32 << zoom;
        let center_x = (world[0] * f64::from(side))
            .floor()
            .to_i64()
            .ok_or_else(|| verdict_owned("fixture tile x is not integral".to_owned()))?;
        let center_y = (world[1] * f64::from(side))
            .floor()
            .to_i64()
            .ok_or_else(|| verdict_owned("fixture tile y is not integral".to_owned()))?;
        for y in center_y.saturating_sub(2)..=(center_y + 2).min(i64::from(side) - 1) {
            for x in center_x.saturating_sub(2)..=(center_x + 2).min(i64::from(side) - 1) {
                if x < 0 || y < 0 {
                    continue;
                }
                let x = u32::try_from(x)
                    .map_err(|error| verdict_owned(format!("forge fixture tile x: {error}")))?;
                let y = u32::try_from(y)
                    .map_err(|error| verdict_owned(format!("forge fixture tile y: {error}")))?;
                cover.push(TileCoord::new(zoom, x, y).map_err(|error| {
                    verdict_owned(format!("forge fixture tile coordinate: {error}"))
                })?);
            }
        }
    }
    Ok(cover)
}

fn fixture_vector_tile() -> Result<Vec<u8>> {
    let polygon = |west, north, east, south| {
        MvtGeometry::Polygon(polygon![
            (x: west, y: north),
            (x: east, y: north),
            (x: east, y: south),
            (x: west, y: south),
            (x: west, y: north),
        ])
    };
    let feature = |id, geometry, properties| MvtFeature {
        id: Some(id),
        geometry,
        properties,
    };
    MvtTile {
        layers: vec![
            MvtLayer {
                name: "earth".to_owned(),
                extent: DEFAULT_EXTENT,
                features: vec![feature(1, polygon(0, 0, 4_096, 4_096), Vec::new())],
            },
            MvtLayer {
                name: "landcover".to_owned(),
                extent: DEFAULT_EXTENT,
                features: vec![feature(
                    2,
                    polygon(2_080, 180, 3_900, 3_900),
                    vec![("kind".to_owned(), MvtValue::String("forest".to_owned()))],
                )],
            },
            MvtLayer {
                name: "water".to_owned(),
                extent: DEFAULT_EXTENT,
                features: vec![feature(3, polygon(680, 520, 1_620, 3_700), Vec::new())],
            },
            MvtLayer {
                name: "roads".to_owned(),
                extent: DEFAULT_EXTENT,
                features: vec![feature(
                    4,
                    MvtGeometry::LineString(line_string![
                        (x: 120, y: 3_600),
                        (x: 1_850, y: 2_080),
                        (x: 3_980, y: 620),
                    ]),
                    vec![
                        ("kind".to_owned(), MvtValue::String("major_road".to_owned())),
                        (
                            "kind_detail".to_owned(),
                            MvtValue::String("secondary".to_owned()),
                        ),
                        ("min_zoom".to_owned(), MvtValue::Double(0.0)),
                    ],
                )],
            },
        ],
    }
    .encode()
    .map_err(|error| verdict_owned(format!("encode fixture vector tile: {error}")))
}

fn world_from_coord([longitude, latitude]: [f64; 2]) -> [f64; 2] {
    let latitude = latitude.to_radians();
    [
        (longitude + 180.0) / 360.0,
        (1.0 - latitude.tan().asinh() / std::f64::consts::PI) * 0.5,
    ]
}

fn dense_network() -> Vec<u8> {
    const SIDE: u32 = 10;
    const STEP: f64 = 0.006;
    const WEST: f64 = -105.027;
    const SOUTH: f64 = 39.973;
    let point = |x: u32, y: u32| {
        [
            f64::from(x).mul_add(STEP, WEST),
            f64::from(y).mul_add(STEP, SOUTH),
            f64::from(x * 7 + y * 11) + 100.0,
        ]
    };
    let mut features = Vec::new();
    for y in 0..SIDE {
        for x in 0..SIDE - 1 {
            features.push(segment(&format!("h-{x}-{y}"), point(x, y), point(x + 1, y)));
        }
    }
    for x in 0..SIDE {
        for y in 0..SIDE - 1 {
            features.push(segment(&format!("v-{x}-{y}"), point(x, y), point(x, y + 1)));
        }
    }
    for y in 0..SIDE - 1 {
        for x in 0..SIDE - 1 {
            if (x + y) % 2 == 0 {
                features.push(segment(
                    &format!("d-{x}-{y}"),
                    point(x, y),
                    point(x + 1, y + 1),
                ));
            }
        }
    }
    serde_json::to_vec(&serde_json::json!({
        "type": "FeatureCollection",
        "features": features,
    }))
    .expect("synthetic dense network serializes")
}

fn segment(id: &str, a: [f64; 3], b: [f64; 3]) -> serde_json::Value {
    serde_json::json!({
        "type": "Feature",
        "properties": {
            "id": id,
            "source": "acceptance",
            "terrain": "trail",
            "access": "open",
            "confidence": 0.98
        },
        "geometry": {"type": "LineString", "coordinates": [a, b]}
    })
}

const fn verdict_owned(detail: String) -> Error {
    Error::X11 {
        operation: "forge Trailgen acceptance fixtures",
        detail,
    }
}

const EMPTY_USGS: &str = r#"{"type":"FeatureCollection","features":[]}"#;

const OSM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<osm version="0.6" generator="trailgen-acceptance">
  <node id="1" lat="39.486" lon="-98.514"/>
  <node id="2" lat="39.486" lon="-98.486"/>
  <node id="3" lat="39.514" lon="-98.486"/>
  <node id="4" lat="39.514" lon="-98.514"/>
  <node id="5" lat="39.500" lon="-98.500"/>
  <way id="101"><nd ref="1"/><nd ref="2"/><tag k="highway" v="path"/><tag k="surface" v="asphalt"/><tag k="name" v="South Trail"/></way>
  <way id="102"><nd ref="2"/><nd ref="3"/><tag k="highway" v="path"/><tag k="name" v="East Trail"/></way>
  <way id="103"><nd ref="3"/><nd ref="4"/><tag k="highway" v="path"/><tag k="name" v="North Trail"/></way>
  <way id="104"><nd ref="4"/><nd ref="1"/><tag k="highway" v="path"/><tag k="name" v="West Trail"/></way>
  <way id="105"><nd ref="1"/><nd ref="5"/><nd ref="3"/><tag k="highway" v="path"/><tag k="informal" v="yes"/><tag k="surface" v="scree"/><tag k="name" v="Diagonal Trail"/></way>
  <way id="106"><nd ref="2"/><nd ref="5"/><nd ref="4"/><tag k="highway" v="path"/><tag k="name" v="Cross Trail"/></way>
</osm>"#;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use fast_mvt::MvtReaderRef;

    use super::*;

    #[test]
    fn basemap_fixture_is_nonempty_and_decodable() -> Result<()> {
        let bytes = fixture_vector_tile()?;
        let reader = MvtReaderRef::new(&bytes)
            .map_err(|error| verdict_owned(format!("decode fixture vector tile: {error}")))?;
        let layers = reader
            .layers()
            .map(fast_mvt::MvtLayerRef::name)
            .collect::<Vec<_>>();
        egui_tester::demand(
            layers == ["earth", "landcover", "water", "roads"],
            format!("fixture basemap layers drifted: {layers:?}"),
        )
    }

    #[test]
    fn basemap_fixture_covers_every_supported_detail_at_its_center() -> Result<()> {
        let cover = fixture_tile_cover()?;
        let zooms = cover.iter().map(TileCoord::z).collect::<BTreeSet<_>>();
        egui_tester::demand(
            zooms == (0_u8..=15).collect(),
            format!("fixture basemap zoom cover drifted: {zooms:?}"),
        )
    }
}
