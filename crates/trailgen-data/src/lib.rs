//! Shared trail-source acquisition, sequestration, and graph indexing.

use anyhow::{Context as _, Result, ensure};
use reqwest::blocking::Response;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    env,
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};
use trailgen_core::{
    Access, Coord, CrossingKind, DifficultyWeights, Edge, EdgeTravel, EnrichmentConfig,
    GraphBuilder, LineString, Provenance, Terrain, TrailGraph, apply_context_overlays,
    io::{geojson, osm},
    model::TerrainEvidence,
    source::{
        GeoBounds, SourceCandidate, SourceFingerprint, SourceKind, SourceManifest,
        adapter_registry, discovery_recommendations, source_coverage,
    },
};

pub const MIN_RADIUS_KM: f64 = 2.0;
pub const DEFAULT_RADIUS_KM: f64 = 8.0;
pub const MAX_RADIUS_KM: f64 = 40.0;
pub const DEFAULT_NOMINATIM_ENDPOINT: &str = "https://nominatim.openstreetmap.org/search";
pub const DEFAULT_OVERPASS_ENDPOINT: &str = "https://overpass-api.de/api/interpreter";
pub const FALLBACK_OVERPASS_ENDPOINT: &str = "https://overpass.private.coffee/api/interpreter";
const MAX_OVERPASS_BBOX_DEG2: f64 = 4.0;
const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const AUTOMATIC_OSM_PROFILE: OsmProfile = OsmProfile::All;
const INDEX_SCHEMA: u8 = 5;
const RAW_SCHEMA: u8 = 2;
const LOCATION_CACHE: &str = "sources/location.json";
const TRAIL_INDEX: &str = "cache/trails.json";
const GRAPH: &str = "cache/graph.json";
const GRAPH_GEOJSON: &str = "cache/graph.geojson";
const SOURCE_MANIFEST: &str = "sources/manifest.json";
const OSM_TRAIL_SELECTORS: &[&str] = &[
    r#"way["highway"~"^(path|footway|track|pedestrian|steps|bridleway)$"]"#,
    r#"way["highway"~"^(service|unclassified|residential|tertiary|road)$"]["foot"~"^(yes|designated|permissive|official)$"]"#,
    r#"way["route"~"^(hiking|foot|walking)$"]"#,
];
const OSM_ROAD_SELECTORS: &[&str] = &[
    r#"way["highway"~"^(motorway|trunk|primary|secondary|tertiary|unclassified|residential|living_street|service|track|road)$"]"#,
];
const OSM_HYDROLOGY_SELECTORS: &[&str] =
    &[r#"way["waterway"~"^(stream|river|canal|drain|ditch|brook)$"]"#];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OsmProfile {
    All,
    Trails,
    Roads,
    Hydrology,
}

impl OsmProfile {
    #[must_use]
    pub const fn default_output(self) -> &'static str {
        match self {
            Self::All => "osm-extract.osm",
            Self::Trails => "osm-trails.osm",
            Self::Roads => "roads.osm",
            Self::Hydrology => "hydrology.osm",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Trails => "trails",
            Self::Roads => "roads",
            Self::Hydrology => "hydrology",
        }
    }
}

impl FromStr for OsmProfile {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        match raw {
            "all" => Ok(Self::All),
            "trails" => Ok(Self::Trails),
            "roads" => Ok(Self::Roads),
            "hydrology" => Ok(Self::Hydrology),
            _ => Err("expected all, trails, roads, or hydrology".to_owned()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub query: String,
    pub label: String,
    pub center: Coord,
    pub license: String,
    pub provider: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Demand {
    pub center: Coord,
    pub bounds: GeoBounds,
    pub radius_km: f64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Inventory {
    pub trail_segments: usize,
    pub road_features: usize,
    pub waterway_features: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub place: Place,
    pub demand: Demand,
    pub inventory: Inventory,
    pub vertices: usize,
    pub edges: usize,
    pub raw_path: PathBuf,
    pub reused: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrailDataConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    pub radius_km: f64,
}

impl Default for TrailDataConfig {
    fn default() -> Self {
        Self {
            place: None,
            radius_km: DEFAULT_RADIUS_KM,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    Locating,
    Located(Place),
    Ranging(Demand),
    Downloaded { bytes: u64 },
    Indexing,
    Ready(Summary),
}

impl Event {
    #[must_use]
    pub fn status(&self) -> String {
        match self {
            Self::Locating => "LOCATING US TRAIL AREA".to_owned(),
            Self::Located(place) => format!("LOCATED · {}", place.label.to_ascii_uppercase()),
            Self::Ranging(_) => "RANGING OPENSTREETMAP TRAILS + CONTEXT".to_owned(),
            Self::Downloaded { bytes } => {
                let mib = bytes / 1_048_576;
                let tenth = (bytes % 1_048_576) * 10 / 1_048_576;
                format!("SEQUESTERED {mib}.{tenth} MIB OF RAW OSM DATA")
            }
            Self::Indexing => "INDEXING ROUTABLE TRAIL GRAPH".to_owned(),
            Self::Ready(summary) if summary.reused => {
                format!("TRAIL INDEX READY · {} EDGES · CACHED", summary.edges)
            }
            Self::Ready(summary) => format!("TRAIL INDEX READY · {} EDGES", summary.edges),
        }
    }
}

pub trait PlaceIndex {
    fn locate_us(&self, query: &str) -> Result<Place>;
}

pub trait TrailProvider {
    fn fetch(&self, profile: OsmProfile, bounds: GeoBounds) -> Result<OsmPayload>;
}

#[derive(Clone, Debug)]
pub struct Nominatim {
    endpoint: String,
    timeout: Duration,
}

impl Default for Nominatim {
    fn default() -> Self {
        Self {
            endpoint: env::var("TRAILGEN_GEOCODER_ENDPOINT")
                .unwrap_or_else(|_| DEFAULT_NOMINATIM_ENDPOINT.to_owned()),
            timeout: Duration::from_secs(30),
        }
    }
}

impl Nominatim {
    #[must_use]
    pub fn new(endpoint: impl Into<String>, timeout: Duration) -> Self {
        Self {
            endpoint: endpoint.into(),
            timeout,
        }
    }
}

impl PlaceIndex for Nominatim {
    fn locate_us(&self, query: &str) -> Result<Place> {
        let query = query.trim();
        ensure!(!query.is_empty(), "enter a US place or trailhead");
        let replies = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .user_agent(user_agent("place-search"))
            .build()
            .context("build OpenStreetMap place-search client")?
            .get(&self.endpoint)
            .query(&[
                ("q", query),
                ("format", "jsonv2"),
                ("countrycodes", "us"),
                ("limit", "1"),
            ])
            .header("Accept-Language", "en-US,en;q=0.8")
            .send()
            .with_context(|| format!("search US places through {}", self.endpoint))?
            .error_for_status()
            .with_context(|| format!("place-search endpoint {} returned an error", self.endpoint))?
            .json::<Vec<NominatimReply>>()
            .context("decode OpenStreetMap place-search response")?;
        let reply = replies
            .into_iter()
            .next()
            .with_context(|| format!("no US place matched {query:?}"))?;
        let lat = parse_coordinate(&reply.lat, "latitude")?;
        let lon = parse_coordinate(&reply.lon, "longitude")?;
        ensure!(
            (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon),
            "place search returned an invalid coordinate"
        );
        Ok(Place {
            query: query.to_owned(),
            label: reply.display_name,
            center: Coord::new(lon, lat),
            license: reply.licence,
            provider: self.endpoint.clone(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Overpass {
    endpoints: Vec<String>,
    timeout: Duration,
}

impl Default for Overpass {
    fn default() -> Self {
        let endpoints = env::var("TRAILGEN_OVERPASS_ENDPOINT").map_or_else(
            |_| {
                vec![
                    DEFAULT_OVERPASS_ENDPOINT.to_owned(),
                    FALLBACK_OVERPASS_ENDPOINT.to_owned(),
                ]
            },
            |endpoint| vec![endpoint],
        );
        Self {
            endpoints,
            timeout: Duration::from_secs(90),
        }
    }
}

impl Overpass {
    #[must_use]
    pub fn new(endpoint: impl Into<String>, timeout: Duration) -> Self {
        Self {
            endpoints: vec![endpoint.into()],
            timeout,
        }
    }

    #[must_use]
    pub fn query(&self, profile: OsmProfile, bounds: GeoBounds) -> String {
        overpass_query(profile, bounds, self.timeout.as_secs())
    }
}

impl TrailProvider for Overpass {
    fn fetch(&self, profile: OsmProfile, bounds: GeoBounds) -> Result<OsmPayload> {
        let area_deg2 = (bounds.east - bounds.west) * (bounds.north - bounds.south);
        ensure!(bounds.is_valid(), "invalid trail-data bounds");
        ensure!(
            area_deg2 <= MAX_OVERPASS_BBOX_DEG2,
            "trail-data bounds span {area_deg2:.2} square degrees; limit is {MAX_OVERPASS_BBOX_DEG2:.2}"
        );
        let query = self.query(profile, bounds);
        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .user_agent(user_agent("trail-source"))
            .build()
            .context("build OpenStreetMap trail-source client")?;
        let mut faults = Vec::new();
        for endpoint in &self.endpoints {
            let response = match client
                .post(endpoint)
                .form(&[("data", query.as_str())])
                .send()
            {
                Ok(response) => response,
                Err(err) => {
                    faults.push(format!("{endpoint}: {err}"));
                    continue;
                }
            };
            let status = response.status();
            if !status.is_success() {
                let fault = format!("{endpoint}: HTTP {status}");
                if status.as_u16() == 429 || status.is_server_error() {
                    faults.push(fault);
                    continue;
                }
                anyhow::bail!("Overpass rejected the trail query through {fault}");
            }
            match read_bounded(response, MAX_SOURCE_BYTES, "OpenStreetMap response") {
                Ok(bytes) => {
                    return Ok(OsmPayload {
                        bytes,
                        query,
                        origin: endpoint.clone(),
                    });
                }
                Err(err) => faults.push(format!("{endpoint}: {err:#}")),
            }
        }
        anyhow::bail!("all Overpass providers failed: {}", faults.join("; "))
    }
}

pub struct Surveyor<L = Nominatim, P = Overpass> {
    locator: L,
    provider: P,
}

impl Default for Surveyor {
    fn default() -> Self {
        Self {
            locator: Nominatim::default(),
            provider: Overpass::default(),
        }
    }
}

impl<L, P> Surveyor<L, P>
where
    L: PlaceIndex,
    P: TrailProvider,
{
    #[must_use]
    pub const fn new(locator: L, provider: P) -> Self {
        Self { locator, provider }
    }

    pub fn survey(
        &self,
        project: &Path,
        query: &str,
        radius_km: f64,
        mut emit: impl FnMut(Event),
    ) -> Result<Summary> {
        ensure!(
            project.join("trailgen.toml").is_file(),
            "{} is not a trailgen project",
            project.display()
        );
        let query = query.trim();
        ensure!(!query.is_empty(), "enter a US place or trailhead");
        validate_radius(radius_km)?;
        configure_project(
            project,
            &TrailDataConfig {
                place: Some(query.to_owned()),
                radius_km,
            },
        )?;
        emit(Event::Locating);
        let place =
            cached_place(project, query)?.map_or_else(|| self.locator.locate_us(query), Ok)?;
        write_json_atomic(project.join(LOCATION_CACHE), &place)?;
        emit(Event::Located(place.clone()));
        let demand = demand(&place, radius_km)?;

        if let Some(mut summary) = reusable_index(project, &demand)? {
            summary.reused = true;
            emit(Event::Ready(summary.clone()));
            return Ok(summary);
        }

        let key = demand_key(&demand);
        let raw_relative = PathBuf::from("sources/osm").join(format!("{key}.osm"));
        let raw_path = project.join(&raw_relative);
        let query_path = raw_path.with_extension("overpassql");
        let artifact_path = raw_path.with_extension("json");
        let (bytes, origin) =
            if let Some(cached) = cached_osm(&raw_path, &query_path, &artifact_path, &demand)? {
                (cached.bytes, cached.origin)
            } else {
                emit(Event::Ranging(demand.clone()));
                let payload = self.provider.fetch(AUTOMATIC_OSM_PROFILE, demand.bounds)?;
                let artifact = OsmArtifact {
                    schema: RAW_SCHEMA,
                    demand: demand.clone(),
                    profile: AUTOMATIC_OSM_PROFILE,
                    origin: payload.origin.clone(),
                    query: payload.query.clone(),
                    raw: fingerprint(&payload.bytes),
                };
                write_atomic(&query_path, payload.query.as_bytes())?;
                write_json_atomic(&artifact_path, &artifact)?;
                // The raw path is the artifact's commit marker: its sidecars must
                // already be durable before it becomes visible.
                write_atomic(&raw_path, &payload.bytes)?;
                emit(Event::Downloaded {
                    bytes: payload.bytes.len() as u64,
                });
                (payload.bytes, payload.origin)
            };
        emit(Event::Indexing);
        let summary = index_osm(project, place, demand, raw_relative, &bytes, &origin)?;
        emit(Event::Ready(summary.clone()));
        Ok(summary)
    }
}

pub struct OsmPayload {
    pub bytes: Vec<u8>,
    pub query: String,
    pub origin: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OsmInventory {
    pub trail_segments: usize,
    pub road_features: usize,
    pub waterway_features: usize,
}

pub fn inspect_osm(profile: OsmProfile, raw: &str) -> Result<OsmInventory> {
    let trails = if matches!(profile, OsmProfile::All | OsmProfile::Trails) {
        osm::network_from_str(raw)?.len()
    } else {
        0
    };
    let (roads, waterways) = if matches!(
        profile,
        OsmProfile::All | OsmProfile::Roads | OsmProfile::Hydrology
    ) {
        osm::context_overlays_from_str(raw)?.into_iter().fold(
            (0, 0),
            |(roads, waterways), overlay| match overlay.kind {
                trailgen_core::CrossingKind::Road => (roads + 1, waterways),
                trailgen_core::CrossingKind::Water => (roads, waterways + 1),
            },
        )
    } else {
        (0, 0)
    };
    let inventory = OsmInventory {
        trail_segments: trails,
        road_features: roads,
        waterway_features: waterways,
    };
    let selected = match profile {
        OsmProfile::All => trails + roads + waterways,
        OsmProfile::Trails => trails,
        OsmProfile::Roads => roads,
        OsmProfile::Hydrology => waterways,
    };
    ensure!(
        selected > 0,
        "OpenStreetMap response contained no normalizable {} ways",
        profile.label()
    );
    Ok(inventory)
}

#[must_use]
pub fn overpass_query(profile: OsmProfile, area: GeoBounds, timeout_s: u64) -> String {
    let bbox = format!(
        "({},{},{},{})",
        area.south, area.west, area.north, area.east
    );
    if matches!(profile, OsmProfile::All | OsmProfile::Trails) {
        return trail_overpass_query(&bbox, timeout_s, profile == OsmProfile::All);
    }
    let selectors = match profile {
        OsmProfile::Roads => OSM_ROAD_SELECTORS.to_vec(),
        OsmProfile::Hydrology => OSM_HYDROLOGY_SELECTORS.to_vec(),
        OsmProfile::All | OsmProfile::Trails => unreachable!("trail profiles handled above"),
    };
    let mut query = format!("[out:xml][timeout:{timeout_s}];\n(\n");
    for selector in selectors {
        query.push_str("  ");
        query.push_str(selector);
        query.push_str(&bbox);
        query.push_str(";\n");
    }
    query.push_str(");\n(._;>;);\nout body;\n");
    query
}

fn trail_overpass_query(bbox: &str, timeout_s: u64, context: bool) -> String {
    let mut query = format!("[out:xml][timeout:{timeout_s}];\n(\n");
    for selector in OSM_TRAIL_SELECTORS {
        writeln!(query, "  {selector}{bbox};").expect("write to string");
    }
    query.push_str(
        ")->.trailways;\n\
         rel(bw.trailways)[\"type\"=\"route\"][\"route\"~\"^(hiking|foot|walking)$\"]->.routes;\n\
         rel(bw.trailways)[\"type\"=\"restriction\"][\"restriction:foot\"]->.restrictions;\n",
    );
    if context {
        query.push_str("node(w.trailways)->.trailnodes;\n(\n");
        for selector in [OSM_ROAD_SELECTORS[0], OSM_HYDROLOGY_SELECTORS[0]] {
            let filter = selector.strip_prefix("way").expect("way selector");
            writeln!(query, "  way(bn.trailnodes){filter};").expect("write to string");
        }
        query.push_str(
            ")->.context;\n\
             (.trailways; .routes; .restrictions; .context; .trailways >; .context >;);\n",
        );
    } else {
        query.push_str("(.trailways; .routes; .restrictions; .trailways >;);\n");
    }
    query.push_str("out body;\n");
    query
}

#[must_use]
pub const fn overpass_selector_count(profile: OsmProfile) -> usize {
    match profile {
        OsmProfile::All => {
            OSM_TRAIL_SELECTORS.len() + OSM_ROAD_SELECTORS.len() + OSM_HYDROLOGY_SELECTORS.len() + 2
        }
        OsmProfile::Trails => OSM_TRAIL_SELECTORS.len() + 2,
        OsmProfile::Roads => OSM_ROAD_SELECTORS.len(),
        OsmProfile::Hydrology => OSM_HYDROLOGY_SELECTORS.len(),
    }
}

#[derive(Deserialize)]
struct NominatimReply {
    licence: String,
    lat: String,
    lon: String,
    display_name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TrailIndex {
    schema: u8,
    summary: Summary,
    raw: SourceFingerprint,
    graph: SourceFingerprint,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct OsmArtifact {
    schema: u8,
    demand: Demand,
    profile: OsmProfile,
    origin: String,
    query: String,
    raw: SourceFingerprint,
}

struct CachedOsm {
    bytes: Vec<u8>,
    origin: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(default)]
struct GraphLaw {
    snap_tolerance_m: f64,
    enrichment: EnrichmentConfig,
    difficulty: DifficultyWeights,
}

impl Default for GraphLaw {
    fn default() -> Self {
        Self {
            snap_tolerance_m: 8.0,
            enrichment: EnrichmentConfig::default(),
            difficulty: DifficultyWeights::default(),
        }
    }
}

fn index_osm(
    project: &Path,
    place: Place,
    demand: Demand,
    raw_relative: PathBuf,
    bytes: &[u8],
    origin: &str,
) -> Result<Summary> {
    let raw = std::str::from_utf8(bytes).context("OpenStreetMap response is not UTF-8 XML")?;
    let inventory = inspect_osm(OsmProfile::All, raw)?;
    ensure!(
        inventory.trail_segments > 0,
        "the selected area contains no routable OSM trails"
    );
    let law = read_graph_law(project)?;
    let drafts = osm::network_from_str(raw)?;
    let mut graph = GraphBuilder {
        snap_tolerance_m: law.snap_tolerance_m,
        enrichment: law.enrichment,
        weights: law.difficulty,
    }
    .build(&drafts)
    .context("index OpenStreetMap trail topology")?;
    let overlays = osm::context_overlays_from_str(raw)?;
    apply_context_overlays(&mut graph, &overlays, law.difficulty);
    let surfaces = GraphSurfaces::engrave(&graph)?;
    surfaces.write_auxiliaries(project)?;
    store_area(project, demand.bounds)?;
    let raw_fingerprint = fingerprint(bytes);
    write_source_manifest(
        project,
        &raw_relative,
        &inventory,
        &raw_fingerprint,
        origin,
        demand.bounds,
    )?;
    let summary = Summary {
        place,
        demand,
        inventory: Inventory {
            trail_segments: inventory.trail_segments,
            road_features: inventory.road_features,
            waterway_features: inventory.waterway_features,
        },
        vertices: graph.vertices.len(),
        edges: graph.edges.len(),
        raw_path: raw_relative,
        reused: false,
    };
    write_json_atomic(
        project.join(TRAIL_INDEX),
        &TrailIndex {
            schema: INDEX_SCHEMA,
            summary: summary.clone(),
            raw: raw_fingerprint,
            graph: surfaces.fingerprint(),
        },
    )?;
    // graph.json is the workbench commit marker. No GUI can mistake an
    // interrupted indexing pass for a ready project.
    surfaces.commit(project)?;
    Ok(summary)
}

fn cached_osm(
    raw_path: &Path,
    query_path: &Path,
    artifact_path: &Path,
    demand: &Demand,
) -> Result<Option<CachedOsm>> {
    let bytes = match fs::read(raw_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("read trail source {}", raw_path.display()));
        }
    };
    let raw = match fs::read_to_string(artifact_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("read trail-source index {}", artifact_path.display()));
        }
    };
    let Ok(artifact) = serde_json::from_str::<OsmArtifact>(&raw) else {
        return Ok(None);
    };
    let query = match fs::read_to_string(query_path) {
        Ok(query) => query,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("read trail-source query {}", query_path.display()));
        }
    };
    if artifact.schema != RAW_SCHEMA
        || artifact.profile != AUTOMATIC_OSM_PROFILE
        || &artifact.demand != demand
        || artifact.query != query
        || artifact.raw != fingerprint(&bytes)
    {
        return Ok(None);
    }
    Ok(Some(CachedOsm {
        bytes,
        origin: artifact.origin,
    }))
}

fn cached_place(project: &Path, query: &str) -> Result<Option<Place>> {
    let path = project.join(LOCATION_CACHE);
    match fs::read_to_string(&path) {
        Ok(raw) => {
            let Ok(place) = serde_json::from_str::<Place>(&raw) else {
                return Ok(None);
            };
            Ok(place
                .query
                .trim()
                .eq_ignore_ascii_case(query.trim())
                .then_some(place))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("read location cache {}", path.display())),
    }
}

fn reusable_index(project: &Path, demand: &Demand) -> Result<Option<Summary>> {
    let index_path = project.join(TRAIL_INDEX);
    let raw = match fs::read_to_string(&index_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("read trail index {}", index_path.display()));
        }
    };
    let Ok(index) = serde_json::from_str::<TrailIndex>(&raw) else {
        return Ok(None);
    };
    if index.schema != INDEX_SCHEMA
        || &index.summary.demand != demand
        || !project.join(GRAPH).is_file()
    {
        return Ok(None);
    }
    let raw_path = project.join(&index.summary.raw_path);
    let bytes = match fs::read(&raw_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("read trail source {}", raw_path.display()));
        }
    };
    if fingerprint(&bytes) != index.raw {
        return Ok(None);
    }
    let graph_bytes = fs::read(project.join(GRAPH)).context("read cached trail graph")?;
    if fingerprint(&graph_bytes) != index.graph {
        return Ok(None);
    }
    let Ok(graph) = serde_json::from_slice::<TrailGraph>(&graph_bytes) else {
        return Ok(None);
    };
    if graph.vertices.len() != index.summary.vertices || graph.edges.len() != index.summary.edges {
        return Ok(None);
    }
    Ok(Some(index.summary))
}

fn demand(place: &Place, radius_km: f64) -> Result<Demand> {
    validate_radius(radius_km)?;
    let lat_radius = radius_km / 111.32;
    let longitude_scale = place.center.lat.to_radians().cos().abs().max(0.05);
    let lon_radius = radius_km / (111.32 * longitude_scale);
    let bounds = GeoBounds::new(
        (place.center.lon - lon_radius).max(-180.0),
        (place.center.lat - lat_radius).max(-90.0),
        (place.center.lon + lon_radius).min(180.0),
        (place.center.lat + lat_radius).min(90.0),
    );
    ensure!(
        bounds.is_valid(),
        "place produces invalid trail survey bounds"
    );
    Ok(Demand {
        center: place.center,
        bounds,
        radius_km,
    })
}

fn validate_radius(radius_km: f64) -> Result<()> {
    ensure!(
        radius_km.is_finite() && (MIN_RADIUS_KM..=MAX_RADIUS_KM).contains(&radius_km),
        "trail survey radius must be within {MIN_RADIUS_KM}–{MAX_RADIUS_KM} km"
    );
    Ok(())
}

fn demand_key(demand: &Demand) -> String {
    let digest = Sha256::digest(
        format!(
            "osm-{}-v{RAW_SCHEMA}:{:.8}:{:.8}:{:.8}:{:.8}",
            AUTOMATIC_OSM_PROFILE.label(),
            demand.bounds.west,
            demand.bounds.south,
            demand.bounds.east,
            demand.bounds.north
        )
        .as_bytes(),
    );
    digest[..12]
        .iter()
        .fold(String::with_capacity(24), |mut key, byte| {
            write!(key, "{byte:02x}").expect("write to string");
            key
        })
}

fn read_graph_law(project: &Path) -> Result<GraphLaw> {
    let path = project.join("trailgen.toml");
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

/// Read the durable trail-data demand from a project marker.
pub fn project_config(project: &Path) -> Result<TrailDataConfig> {
    #[derive(Deserialize)]
    struct ProjectConfig {
        #[serde(default)]
        trail_data: TrailDataConfig,
    }

    let path = project.join("trailgen.toml");
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let config = toml::from_str::<ProjectConfig>(&raw)
        .with_context(|| format!("parse {}", path.display()))?
        .trail_data;
    validate_radius(config.radius_km)?;
    Ok(config)
}

/// Read the committed trail-index receipt, if this project was surveyed.
pub fn indexed_summary(project: &Path) -> Result<Option<Summary>> {
    let path = project.join(TRAIL_INDEX);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let Ok(index) = serde_json::from_str::<TrailIndex>(&raw) else {
        return Ok(None);
    };
    if index.schema != INDEX_SCHEMA {
        return Ok(None);
    }
    reusable_index(project, &index.summary.demand)
}

/// Persist the canonical trail-data demand without disturbing other project law.
pub fn configure_project(project: &Path, trail_data: &TrailDataConfig) -> Result<()> {
    validate_radius(trail_data.radius_km)?;
    if let Some(place) = &trail_data.place {
        ensure!(
            !place.trim().is_empty(),
            "trail-data place must not be empty"
        );
    }
    let path = project.join("trailgen.toml");
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut config =
        toml::from_str::<toml::Value>(&raw).with_context(|| format!("parse {}", path.display()))?;
    config
        .as_table_mut()
        .context("trailgen.toml root must be a table")?
        .insert("trail_data".to_owned(), toml::Value::try_from(trail_data)?);
    write_atomic(&path, toml::to_string_pretty(&config)?.as_bytes())
}

fn store_area(project: &Path, bounds: GeoBounds) -> Result<()> {
    let path = project.join("trailgen.toml");
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut config =
        toml::from_str::<toml::Value>(&raw).with_context(|| format!("parse {}", path.display()))?;
    let table = config
        .as_table_mut()
        .context("trailgen.toml root must be a table")?;
    table.insert("area".to_owned(), toml::Value::try_from(bounds)?);
    write_atomic(&path, toml::to_string_pretty(&config)?.as_bytes())
}

fn write_source_manifest(
    project: &Path,
    raw_relative: &Path,
    inventory: &OsmInventory,
    fingerprint: &SourceFingerprint,
    origin: &str,
    bounds: GeoBounds,
) -> Result<()> {
    let raw_path = raw_relative.display().to_string();
    let origin = format!(
        "overpass:{origin} profile={} bbox={},{},{},{}",
        AUTOMATIC_OSM_PROFILE.label(),
        bounds.west,
        bounds.south,
        bounds.east,
        bounds.north
    );
    let mut candidates = vec![candidate(
        &raw_path,
        SourceKind::TrailNetwork,
        "osm-xml-network",
        fingerprint,
        &origin,
    )];
    if inventory.road_features > 0 {
        candidates.push(candidate(
            &raw_path,
            SourceKind::Road,
            "osm-road-context",
            fingerprint,
            &origin,
        ));
    }
    if inventory.waterway_features > 0 {
        candidates.push(candidate(
            &raw_path,
            SourceKind::Hydrology,
            "osm-hydrology-context",
            fingerprint,
            &origin,
        ));
    }
    let manifest_path = project.join(SOURCE_MANIFEST);
    let mut manifest = match fs::read_to_string(&manifest_path) {
        Ok(raw) => serde_json::from_str::<SourceManifest>(&raw)
            .with_context(|| format!("parse {}", manifest_path.display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => SourceManifest {
            adapters: Vec::new(),
            recommendations: Vec::new(),
            coverage: Vec::new(),
            candidates: Vec::new(),
        },
        Err(err) => {
            return Err(err).with_context(|| format!("read {}", manifest_path.display()));
        }
    };
    for candidate in candidates {
        if let Some(existing) = manifest
            .candidates
            .iter_mut()
            .find(|existing| existing.path == candidate.path && existing.kind == candidate.kind)
        {
            *existing = candidate;
        } else {
            manifest.candidates.push(candidate);
        }
    }
    manifest.candidates.sort_by(|left, right| {
        (&left.path, left.kind, &left.adapter_id).cmp(&(&right.path, right.kind, &right.adapter_id))
    });
    manifest.adapters = adapter_registry();
    manifest.recommendations = discovery_recommendations(Some(bounds));
    manifest.coverage = source_coverage(
        &manifest.adapters,
        &manifest.recommendations,
        &manifest.candidates,
    );
    write_json_atomic(manifest_path, &manifest)
}

fn candidate(
    path: &str,
    kind: SourceKind,
    adapter_id: &str,
    fingerprint: &SourceFingerprint,
    origin: &str,
) -> SourceCandidate {
    SourceCandidate {
        path: path.to_owned(),
        kind,
        adapter_id: adapter_id.to_owned(),
        origin: Some(origin.to_owned()),
        fingerprint: Some(fingerprint.clone()),
    }
}

fn parse_coordinate(raw: &str, name: &str) -> Result<f64> {
    raw.parse::<f64>()
        .with_context(|| format!("place search returned an invalid {name}"))
}

fn user_agent(task: &str) -> String {
    format!(
        "adequate-trailgen/{} {task} ({})",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_REPOSITORY")
    )
}

fn fingerprint(bytes: &[u8]) -> SourceFingerprint {
    SourceFingerprint {
        bytes: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(bytes)),
    }
}

struct GraphSurfaces {
    graph: Vec<u8>,
    geojson: Vec<u8>,
    edges: String,
    vertices: String,
}

impl GraphSurfaces {
    fn engrave(graph: &TrailGraph) -> Result<Self> {
        Ok(Self {
            graph: serde_json::to_vec_pretty(graph)?,
            geojson: serde_json::to_vec_pretty(&geojson::graph_to_geojson(graph))?,
            edges: graph_edges_csv(graph),
            vertices: graph_vertices_csv(graph),
        })
    }

    fn fingerprint(&self) -> SourceFingerprint {
        fingerprint(&self.graph)
    }

    fn write_auxiliaries(&self, project: &Path) -> Result<()> {
        write_atomic(&project.join(GRAPH_GEOJSON), &self.geojson)?;
        write_atomic(&project.join("cache/edges.csv"), self.edges.as_bytes())?;
        write_atomic(
            &project.join("cache/vertices.csv"),
            self.vertices.as_bytes(),
        )
    }

    fn commit(self, project: &Path) -> Result<()> {
        write_atomic(&project.join(GRAPH), &self.graph)
    }
}

/// Persist every canonical graph surface, publishing `graph.json` last.
pub fn store_graph(project: &Path, graph: &TrailGraph) -> Result<()> {
    let surfaces = GraphSurfaces::engrave(graph)?;
    surfaces.write_auxiliaries(project)?;
    surfaces.commit(project)
}

fn graph_vertices_csv(graph: &TrailGraph) -> String {
    let mut out = String::from("vertex_id,lon,lat,elevation_m,wkt\n");
    for vertex in &graph.vertices {
        let Coord { lon, lat, ele } = vertex.coord;
        writeln!(
            out,
            "{},{lon:.7},{lat:.7},{},{}",
            vertex.id.0,
            csv_f64(ele),
            csv_cell(&point_wkt(vertex.coord))
        )
        .expect("write to string");
    }
    out
}

fn graph_edges_csv(graph: &TrailGraph) -> String {
    let mut out = String::from(
        "edge_id,from_vertex,to_vertex,travel,length_m,ascent_m,descent_m,grade_abs_mean,grade_abs_max,sustained_steep_m,terrain,surface,terrain_confidence,terrain_evidence,access,access_confidence,access_provenance,road_exposure,confidence,difficulty,seed_count,seed_provenance,elevation_provenance,road_crossings,water_crossings,provenance,wkt\n",
    );
    for edge in &graph.edges {
        let (roads, water) = edge_crossing_counts(edge);
        writeln!(
            out,
            "{},{},{},{},{:.3},{:.3},{:.3},{:.6},{:.6},{:.3},{},{},{:.6},{},{},{:.6},{},{:.6},{:.6},{:.6},{},{},{},{},{},{},{}",
            edge.id.0,
            edge.a.0,
            edge.b.0,
            edge_travel_tag(edge.attr.travel),
            edge.attr.length_m,
            edge.attr.ascent_m,
            edge.attr.descent_m,
            edge.attr.grade_abs_mean,
            edge.attr.grade_abs_max,
            edge.attr.sustained_steep_m,
            terrain_tag(edge.attr.terrain),
            csv_cell(edge.attr.surface.as_deref().unwrap_or("")),
            edge.attr.terrain_confidence,
            csv_cell(&terrain_evidence_summary(&edge.attr.terrain_evidence)),
            access_tag(edge.attr.access),
            edge.attr.access_confidence,
            csv_cell(&provenance_summary(&edge.attr.access_provenance)),
            edge.attr.road_exposure,
            edge.attr.confidence,
            edge.attr.difficulty,
            edge.attr.seed_count,
            csv_cell(&provenance_summary(&edge.attr.seed_provenance)),
            csv_cell(&provenance_summary(&edge.attr.elevation_provenance)),
            roads,
            water,
            csv_cell(&provenance_summary(&edge.attr.provenance)),
            csv_cell(&line_wkt(&edge.geometry))
        )
        .expect("write to string");
    }
    out
}

fn terrain_evidence_summary(evidence: &[TerrainEvidence]) -> String {
    evidence
        .iter()
        .map(|evidence| {
            let mut summary = format!(
                "{}:{:.0}%:{}",
                terrain_tag(evidence.terrain),
                evidence.confidence * 100.0,
                evidence.rationale
            );
            if let Some(provenance) = &evidence.provenance {
                write!(summary, ":{}", provenance_csv_label(provenance)).expect("write to string");
            }
            summary
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn edge_crossing_counts(edge: &Edge) -> (u32, u32) {
    edge.attr
        .crossings
        .iter()
        .fold((0, 0), |(roads, water), crossing| match crossing.kind {
            CrossingKind::Road => (roads + crossing.count, water),
            CrossingKind::Water => (roads, water + crossing.count),
        })
}

fn provenance_summary(provenance: &[Provenance]) -> String {
    provenance
        .iter()
        .map(provenance_csv_label)
        .collect::<Vec<_>>()
        .join("|")
}

fn provenance_csv_label(provenance: &Provenance) -> String {
    let mut label = provenance.source.clone();
    if let Some(layer) = &provenance.layer {
        write!(label, ":{layer}").expect("write to string");
    }
    if let Some(source_id) = &provenance.source_id {
        write!(label, ":{source_id}").expect("write to string");
    }
    label
}

fn line_wkt(line: &LineString) -> String {
    format!(
        "LINESTRING Z ({})",
        line.points
            .iter()
            .map(coord_wkt_tuple)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn point_wkt(coord: Coord) -> String {
    format!("POINT Z ({})", coord_wkt_tuple(&coord))
}

fn coord_wkt_tuple(coord: &Coord) -> String {
    format!(
        "{:.7} {:.7} {:.3}",
        coord.lon,
        coord.lat,
        coord.ele.unwrap_or(0.0)
    )
}

fn csv_cell(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn csv_f64(value: Option<f64>) -> String {
    value.map_or_else(String::new, |value| format!("{value:.3}"))
}

const fn terrain_tag(terrain: Terrain) -> &'static str {
    match terrain {
        Terrain::Unknown => "unknown",
        Terrain::Trail => "trail",
        Terrain::Forest => "forest",
        Terrain::Alpine => "alpine",
        Terrain::Talus => "talus",
        Terrain::Scramble => "scramble",
        Terrain::Pavement => "pavement",
        Terrain::Road => "road",
        Terrain::Water => "water",
    }
}

const fn access_tag(access: Access) -> &'static str {
    match access {
        Access::Unknown => "unknown",
        Access::Open => "open",
        Access::Restricted => "restricted",
        Access::Closed => "closed",
        Access::Private => "private",
    }
}

const fn edge_travel_tag(travel: EdgeTravel) -> &'static str {
    match travel {
        EdgeTravel::Both => "both",
        EdgeTravel::Forward => "forward",
        EdgeTravel::Backward => "backward",
    }
}

fn read_bounded(mut response: Response, limit: u64, label: &str) -> Result<Vec<u8>> {
    if let Some(length) = response.content_length() {
        ensure!(
            length <= limit,
            "{label} is {length} bytes; limit is {limit}"
        );
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {label}"))?;
    ensure!(bytes.len() as u64 <= limit, "{label} exceeds {limit} bytes");
    Ok(bytes)
}

fn write_json_atomic(path: impl AsRef<Path>, value: &impl Serialize) -> Result<()> {
    write_atomic(path.as_ref(), serde_json::to_vec_pretty(value)?.as_slice())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("artifact path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let mut staging = Staging::raise(path)?;
    staging
        .file_mut()?
        .write_all(bytes)
        .with_context(|| format!("write {}", staging.path.display()))?;
    staging
        .file_mut()?
        .sync_all()
        .with_context(|| format!("flush {}", staging.path.display()))?;
    staging.commit(path)
}

struct Staging {
    path: PathBuf,
    file: Option<File>,
}

impl Staging {
    fn raise(target: &Path) -> Result<Self> {
        let extension = target
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("");
        for nonce in 0..64 {
            let path = target.with_extension(format!(
                "{extension}.{}.{}.partial",
                std::process::id(),
                nonce
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("raise staging file {}", path.display()));
                }
            }
        }
        anyhow::bail!("staging namespace exhausted beside {}", target.display())
    }

    fn file_mut(&mut self) -> Result<&mut File> {
        self.file.as_mut().context("staging file already sealed")
    }

    fn commit(mut self, target: &Path) -> Result<()> {
        drop(self.file.take());
        fs::rename(&self.path, target).with_context(|| format!("commit {}", target.display()))?;
        self.path.clear();
        Ok(())
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _partial = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Clone)]
    struct FixedPlace;

    impl PlaceIndex for FixedPlace {
        fn locate_us(&self, query: &str) -> Result<Place> {
            Ok(Place {
                query: query.to_owned(),
                label: "Harriman State Park, New York, United States".to_owned(),
                center: Coord::new(-74.124_792_4, 41.230_375_5),
                license: "OpenStreetMap contributors".to_owned(),
                provider: "fixture".to_owned(),
            })
        }
    }

    #[derive(Default)]
    struct FixedProvider {
        calls: Cell<usize>,
    }

    impl TrailProvider for FixedProvider {
        fn fetch(&self, profile: OsmProfile, bounds: GeoBounds) -> Result<OsmPayload> {
            assert_eq!(profile, AUTOMATIC_OSM_PROFILE);
            assert!(bounds.is_valid());
            self.calls.set(self.calls.get() + 1);
            Ok(OsmPayload {
                bytes: include_bytes!("../tests/fixtures/tiny-overpass.osm").to_vec(),
                query: "fixture overpass query".to_owned(),
                origin: "fixture://overpass".to_owned(),
            })
        }
    }

    #[test]
    fn demands_are_bounded_around_the_resolved_place() -> Result<()> {
        let place = FixedPlace.locate_us("Harriman")?;
        let demand = demand(&place, 20.0)?;
        assert!(demand.bounds.is_valid());
        assert!((demand.bounds.north - demand.bounds.south) < 0.37);
        assert!((demand.bounds.east - demand.bounds.west) < 0.49);
        Ok(())
    }

    #[test]
    fn demand_receipts_round_trip_without_spatial_drift() -> Result<()> {
        let expected = demand(&FixedPlace.locate_us("Harriman")?, DEFAULT_RADIUS_KM)?;
        let encoded = serde_json::to_vec(&expected)?;
        let decoded = serde_json::from_slice::<Demand>(&encoded)?;
        assert_eq!(decoded, expected);
        Ok(())
    }

    #[test]
    fn overpass_queries_are_profiled_and_bbox_scoped() {
        let area = GeoBounds::new(-74.2, 41.1, -74.0, 41.35);
        let query = overpass_query(OsmProfile::All, area, 90);
        assert!(query.starts_with("[out:xml][timeout:90];"));
        assert!(query.contains("(41.1,-74.2,41.35,-74)"));
        assert!(query.contains(r#"["waterway"~"#));
        assert!(query.contains(r#"rel(bw.trailways)["type"="route"]"#));
        assert!(query.contains("way(bn.trailnodes)"));
    }

    #[test]
    fn project_area_update_preserves_the_project_name() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("trailgen.toml"), "name = 'Harriman'\n")?;
        let bounds = GeoBounds::new(-74.2, 41.1, -74.0, 41.35);
        store_area(temp.path(), bounds)?;
        let config = fs::read_to_string(temp.path().join("trailgen.toml"))?;
        assert!(config.contains("name = \"Harriman\""));
        assert!(config.contains("[area]"));
        Ok(())
    }

    #[test]
    fn survey_sequesters_indexes_and_reuses_one_canonical_pipeline() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path();
        fs::write(project.join("trailgen.toml"), "name = 'Harriman'\n")?;
        let surveyor = Surveyor::new(FixedPlace, FixedProvider::default());
        let mut first_events = Vec::new();

        let first = surveyor.survey(project, "Harriman", 20.0, |event| {
            first_events.push(event.status());
        })?;

        assert!(!first.reused);
        assert_eq!(first.inventory.trail_segments, 1);
        assert_eq!(first.inventory.road_features, 1);
        assert_eq!(first.inventory.waterway_features, 1);
        assert_eq!(surveyor.provider.calls.get(), 1);
        assert_eq!(first_events.len(), 6);
        assert!(project.join(&first.raw_path).is_file());
        assert_eq!(indexed_summary(project)?, Some(first.clone()));
        assert!(
            project
                .join(&first.raw_path)
                .with_extension("json")
                .is_file()
        );
        assert!(
            project
                .join(&first.raw_path)
                .with_extension("overpassql")
                .is_file()
        );
        for artifact in [
            LOCATION_CACHE,
            TRAIL_INDEX,
            GRAPH,
            GRAPH_GEOJSON,
            "cache/edges.csv",
            "cache/vertices.csv",
            SOURCE_MANIFEST,
        ] {
            assert!(project.join(artifact).is_file(), "missing {artifact}");
        }
        let manifest = serde_json::from_str::<SourceManifest>(&fs::read_to_string(
            project.join(SOURCE_MANIFEST),
        )?)?;
        assert_eq!(manifest.candidates.len(), 3);
        assert!(manifest.candidates.iter().all(|candidate| {
            candidate
                .origin
                .as_deref()
                .is_some_and(|origin| origin.contains("fixture://overpass"))
        }));

        let mut second_events = Vec::new();
        let second = surveyor.survey(project, " harriman ", 20.0, |event| {
            second_events.push(event.status());
        })?;
        assert!(second.reused);
        assert_eq!(second.raw_path, first.raw_path);
        assert_eq!(surveyor.provider.calls.get(), 1);
        assert_eq!(second_events.len(), 3);
        assert!(
            second_events
                .last()
                .is_some_and(|event| event.contains("CACHED"))
        );
        Ok(())
    }

    #[test]
    fn a_drifted_graph_is_rebuilt_from_the_sequestered_source() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path();
        fs::write(project.join("trailgen.toml"), "name = 'Harriman'\n")?;
        let surveyor = Surveyor::new(FixedPlace, FixedProvider::default());
        surveyor.survey(project, "Harriman", 20.0, drop)?;
        fs::write(project.join(GRAPH), b"drift")?;

        let repaired = surveyor.survey(project, "Harriman", 20.0, drop)?;

        assert!(!repaired.reused);
        assert_eq!(surveyor.provider.calls.get(), 1);
        serde_json::from_slice::<TrailGraph>(&fs::read(project.join(GRAPH))?)?;
        Ok(())
    }

    #[test]
    fn damaged_derived_receipts_are_repaired_without_poisoning_the_project() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path();
        fs::write(project.join("trailgen.toml"), "name = 'Harriman'\n")?;
        let surveyor = Surveyor::new(FixedPlace, FixedProvider::default());
        let first = surveyor.survey(project, "Harriman", 20.0, drop)?;
        fs::write(project.join(TRAIL_INDEX), b"{")?;

        let repaired = surveyor.survey(project, "Harriman", 20.0, drop)?;

        assert!(!repaired.reused);
        assert_eq!(surveyor.provider.calls.get(), 1);
        let artifact = project.join(first.raw_path).with_extension("json");
        fs::write(artifact, b"{")?;
        fs::write(project.join(TRAIL_INDEX), b"{")?;

        surveyor.survey(project, "Harriman", 20.0, drop)?;

        assert_eq!(surveyor.provider.calls.get(), 2);
        Ok(())
    }
}
