use std::{
    fs::File,
    io::{BufRead as _, BufReader, Cursor, Write as _},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use egui_tester::{AppCommand, Error, Result, Testbed};
use pmtiles::{Compression, PmTilesWriter, TileType};

pub struct FixtureWorld {
    server: FixtureServer,
}

impl FixtureWorld {
    pub fn raise(testbed: &Testbed) -> Result<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .ok_or_else(|| verdict("acceptance crate escaped the Trailgen workspace"))?;
        let _mini = testbed.copy_private(
            "fixtures/mini_network.geojson",
            root.join("crates/trailgen-core/tests/fixtures/mini_network.geojson"),
        )?;
        let _dense = testbed.write_private("fixtures/dense_network.geojson", dense_network())?;
        let basemap = testbed.private_path("fixtures/empty.pmtiles")?;
        empty_basemap(&basemap)?;
        Ok(Self {
            server: FixtureServer::raise()?,
        })
    }

    pub fn online(&self, command: AppCommand) -> AppCommand {
        command
            .env("TRAILGEN_OVERPASS_ENDPOINT", self.server.url("overpass"))
            .env("TRAILGEN_USGS_TRAILS_ENDPOINT", self.server.url("usgs"))
            .env("TRAILGEN_TERRAIN_ENDPOINT", self.server.url("terrain"))
    }

    pub fn assert_harvested(&self) -> Result<()> {
        if self.server.overpass.load(Ordering::Acquire) == 0
            || self.server.usgs.load(Ordering::Acquire) == 0
            || self.server.terrain.load(Ordering::Acquire) == 0
        {
            return Err(verdict(
                "GUI map-area acquisition did not traverse every local provider",
            ));
        }
        Ok(())
    }
}

struct FixtureServer {
    address: std::net::SocketAddr,
    shutdown: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    overpass: Arc<AtomicUsize>,
    usgs: Arc<AtomicUsize>,
    terrain: Arc<AtomicUsize>,
}

impl FixtureServer {
    fn raise() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|source| Error::Io {
            operation: "bind fixture provider",
            path: PathBuf::from("<loopback>"),
            source,
        })?;
        listener.set_nonblocking(true).map_err(|source| Error::Io {
            operation: "make fixture provider nonblocking",
            path: PathBuf::from("<loopback>"),
            source,
        })?;
        let address = listener.local_addr().map_err(|source| Error::Io {
            operation: "read fixture provider address",
            path: PathBuf::from("<loopback>"),
            source,
        })?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let overpass = Arc::new(AtomicUsize::new(0));
        let usgs = Arc::new(AtomicUsize::new(0));
        let terrain = Arc::new(AtomicUsize::new(0));
        let worker = {
            let shutdown = Arc::clone(&shutdown);
            let overpass = Arc::clone(&overpass);
            let usgs = Arc::clone(&usgs);
            let terrain = Arc::clone(&terrain);
            thread::Builder::new()
                .name("trailgen-acceptance-provider".to_owned())
                .spawn(move || {
                    let terrain_png = terrain_tile();
                    while !shutdown.load(Ordering::Acquire) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                serve(stream, &terrain_png, &overpass, &usgs, &terrain);
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(2));
                            }
                            Err(_) => break,
                        }
                    }
                })
                .map_err(|source| Error::Io {
                    operation: "spawn fixture provider",
                    path: PathBuf::from("<loopback>"),
                    source,
                })?
        };
        Ok(Self {
            address,
            shutdown,
            worker: Some(worker),
            overpass,
            usgs,
            terrain,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}/{path}", self.address)
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _wake = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let _joined = worker.join();
        }
    }
}

fn serve(
    stream: TcpStream,
    terrain_png: &[u8],
    overpass: &AtomicUsize,
    usgs: &AtomicUsize,
    terrain: &AtomicUsize,
) {
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || matches!(header.as_str(), "\r\n" | "\n" | "") {
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
    } else {
        ("404 Not Found", "text/plain", b"not found")
    };
    let stream = reader.get_mut();
    let _response = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _body = stream.write_all(body);
    let _flush = stream.flush();
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

fn empty_basemap(path: &Path) -> Result<()> {
    let file = File::create(path).map_err(|source| Error::Io {
        operation: "create empty basemap",
        path: path.to_owned(),
        source,
    })?;
    let writer = PmTilesWriter::new(TileType::Mvt)
        .internal_compression(Compression::None)
        .tile_compression(Compression::None)
        .min_zoom(0)
        .max_zoom(15)
        .bounds(-180.0, -85.0, 180.0, 85.0)
        .center(-98.5, 39.5)
        .center_zoom(4)
        .metadata(r#"{"vector_layers":[]}"#)
        .create(file)
        .map_err(|error| verdict_owned(format!("raise empty PMTiles writer: {error}")))?;
    writer
        .finalize()
        .map_err(|error| verdict_owned(format!("seal empty PMTiles archive: {error}")))
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

fn verdict(detail: &'static str) -> Error {
    verdict_owned(detail.to_owned())
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
  <way id="101"><nd ref="1"/><nd ref="2"/><tag k="highway" v="path"/><tag k="name" v="South Trail"/></way>
  <way id="102"><nd ref="2"/><nd ref="3"/><tag k="highway" v="path"/><tag k="name" v="East Trail"/></way>
  <way id="103"><nd ref="3"/><nd ref="4"/><tag k="highway" v="path"/><tag k="name" v="North Trail"/></way>
  <way id="104"><nd ref="4"/><nd ref="1"/><tag k="highway" v="path"/><tag k="name" v="West Trail"/></way>
  <way id="105"><nd ref="1"/><nd ref="5"/><nd ref="3"/><tag k="highway" v="path"/><tag k="name" v="Diagonal Trail"/></way>
  <way id="106"><nd ref="2"/><nd ref="5"/><nd ref="4"/><tag k="highway" v="path"/><tag k="name" v="Cross Trail"/></way>
</osm>"#;
