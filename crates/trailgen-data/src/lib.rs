//! Shared trail-source acquisition, sequestration, and graph indexing.

mod providers;
mod terrain;

pub use providers::{
    AuthorityTrailProvider, DEFAULT_NY_STATE_PARKS_ENDPOINT, DEFAULT_TEXAS_STATE_PARKS_ENDPOINT,
    DEFAULT_USGS_TRAILS_ENDPOINT, NetworkProvider, NormalizedNetwork, ProviderDescriptor,
    ProviderId, ProviderPayload, RawShard, UsgsNationalTrails,
};
pub use terrain::{TerrainTileId, TopographicTile};

use anyhow::{Context as _, Result, ensure};
use reqwest::blocking::Response;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    env,
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};
use trailgen_core::{
    Access, ContextOverlay, Coord, CrossingControl, CrossingKind, DEFAULT_SNAP_TOLERANCE_M, Edge,
    EdgeTravel, EnrichmentConfig, GRAPH_CACHE, GeometryClaim, GraphBuilder, JunctionKey,
    LineString, Provenance, SegmentDraft, Terrain, TrailMarking, TrailStanding, WalkGraph, WayKind,
    WayRealm, apply_context_overlays, decode_graph, encode_graph,
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
pub const MAX_REGION_DEG2: f64 = 4.0;
pub(crate) const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const AUTOMATIC_OSM_PROFILE: OsmProfile = OsmProfile::All;
const INDEX_SCHEMA: u8 = 23;
const RAW_SCHEMA: u8 = 4;
const MAX_OSM_CONNECTOR_M: f64 = 1_000.0;
const LOCATION_CACHE: &str = "sources/location.json";
const TRAIL_INDEX: &str = "cache/trails.json";
const GRAPH_GEOJSON: &str = "cache/graph.geojson";
const CONFLATION_REPORT: &str = "cache/conflation.json";
const SOURCE_MANIFEST: &str = "sources/manifest.json";
const GRAPH_AUXILIARIES: &[&str] = &[
    "cache/graph.json",
    GRAPH_GEOJSON,
    "cache/edges.csv",
    "cache/vertices.csv",
];
const OSM_TRAIL_SELECTORS: &[&str] = &[
    r#"way["highway"~"^(path|footway|cycleway|pedestrian|track|steps|bridleway)$"]"#,
    r#"way["disused:highway"~"^(path|footway|cycleway|track|pedestrian|steps|bridleway)$"]"#,
    r#"way["abandoned:highway"~"^(path|footway|cycleway|track|pedestrian|steps|bridleway)$"]"#,
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

/// One durable rectangle whose trail corpus should be live in a project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurveyRegion {
    pub id: String,
    pub bounds: GeoBounds,
}

impl SurveyRegion {
    pub fn new(bounds: GeoBounds) -> Result<Self> {
        validate_region(bounds)?;
        Ok(Self {
            id: region_key(bounds),
            bounds,
        })
    }

    pub fn validate(&self) -> Result<()> {
        validate_region(self.bounds)?;
        ensure!(
            self.id == region_key(self.bounds),
            "survey-region id does not match its bounds"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Inventory {
    pub trail_segments: usize,
    pub road_features: usize,
    pub waterway_features: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub regions: Vec<SurveyRegion>,
    pub providers: Vec<ProviderId>,
    pub inventory: Inventory,
    pub vertices: usize,
    pub edges: usize,
    #[serde(default)]
    pub elevation_tiles: usize,
    pub raw_paths: Vec<PathBuf>,
    pub conflation: trailgen_core::ConflationStats,
    pub reused: bool,
}

#[derive(Clone, Debug)]
pub struct Topography {
    pub identity: String,
    pub tiles: Vec<TopographicTile>,
}

/// The content address of the indexed elevation field, without decoding its rasters.
pub fn indexed_topography_identity(project: &Path) -> Result<Option<String>> {
    Ok(topographic_index(project)?.map(|index| topographic_identity(&index)))
}

/// Read the indexed elevation field used to enrich this project's trail graph.
pub fn indexed_topography(project: &Path) -> Result<Option<Topography>> {
    let Some(index) = topographic_index(project)? else {
        return Ok(None);
    };
    let identity = topographic_identity(&index);
    let tiles = index
        .elevation
        .iter()
        .map(|receipt| terrain::topographic_tile(project, receipt))
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(Topography { identity, tiles }))
}

fn topographic_index(project: &Path) -> Result<Option<TrailIndex>> {
    let path = project.join(TRAIL_INDEX);
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let index: TrailIndex =
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))?;
    ensure!(
        index.schema == INDEX_SCHEMA,
        "trail index schema is obsolete"
    );
    Ok((!index.elevation.is_empty()).then_some(index))
}

fn topographic_identity(index: &TrailIndex) -> String {
    let mut identity = Sha256::new();
    for receipt in &index.elevation {
        identity.update(receipt.tile.z.to_le_bytes());
        identity.update(receipt.tile.x.to_le_bytes());
        identity.update(receipt.tile.y.to_le_bytes());
        identity.update(receipt.raw.sha256.as_bytes());
    }
    hex(&identity.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            write!(text, "{byte:02x}").expect("write hexadecimal digest");
            text
        })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrailDataConfig {
    /// Whether this project graph is governed by the live-region corpus.
    pub managed: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<SurveyRegion>,
    /// User-facing names keyed by immutable survey-region identity.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub region_names: BTreeMap<String, String>,
    pub providers: Vec<ProviderId>,
}

impl Default for TrailDataConfig {
    fn default() -> Self {
        Self {
            managed: false,
            regions: Vec::new(),
            region_names: BTreeMap::new(),
            providers: automatic_provider_ids(),
        }
    }
}

fn automatic_provider_ids() -> Vec<ProviderId> {
    [
        "ny-state-parks",
        "osm",
        "texas-state-parks",
        "usgs-national-trails",
    ]
    .map(|id| ProviderId::new(id).expect("static provider id is valid"))
    .into_iter()
    .collect()
}

fn legacy_automatic_provider_ids() -> Vec<ProviderId> {
    ["osm", "usgs-national-trails"]
        .map(|id| ProviderId::new(id).expect("static provider id is valid"))
        .into_iter()
        .collect()
}

#[derive(Clone, Debug)]
pub enum Event {
    Locating,
    Located(Place),
    Ranging {
        provider: ProviderId,
        region: SurveyRegion,
    },
    Downloaded {
        provider: ProviderId,
        bytes: u64,
    },
    Elevating {
        complete: usize,
        total: usize,
    },
    Indexing,
    Ready(Summary),
}

impl Event {
    #[must_use]
    pub fn status(&self) -> String {
        match self {
            Self::Locating => "LOCATING US TRAIL AREA".to_owned(),
            Self::Located(place) => format!("LOCATED · {}", place.label.to_ascii_uppercase()),
            Self::Ranging { provider, region } => format!(
                "FETCHING {provider} · {:.4}, {:.4} TO {:.4}, {:.4}",
                region.bounds.west, region.bounds.south, region.bounds.east, region.bounds.north
            ),
            Self::Downloaded { provider, bytes } => {
                let mib = bytes / 1_048_576;
                let tenth = (bytes % 1_048_576) * 10 / 1_048_576;
                format!("SEQUESTERED {mib}.{tenth} MIB FROM {provider}")
            }
            Self::Elevating { complete, total } => {
                format!("FETCHING TOPOGRAPHY · {complete}/{total} TILES")
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
        let replies = provider_client("place-search", self.timeout)
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

    pub fn fetch(&self, profile: OsmProfile, bounds: GeoBounds) -> Result<OsmPayload> {
        let area_deg2 = (bounds.east - bounds.west) * (bounds.north - bounds.south);
        ensure!(bounds.is_valid(), "invalid trail-data bounds");
        ensure!(
            area_deg2 <= MAX_REGION_DEG2,
            "trail-data bounds span {area_deg2:.2} square degrees; limit is {MAX_REGION_DEG2:.2}"
        );
        let query = self.query(profile, bounds);
        let client = provider_client("trail-source", self.timeout)
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

pub struct Surveyor<L = Nominatim> {
    locator: L,
    providers: Vec<Box<dyn NetworkProvider>>,
    fixed_providers: bool,
}

impl Default for Surveyor {
    fn default() -> Self {
        Self {
            locator: Nominatim::default(),
            providers: vec![
                Box::new(AuthorityTrailProvider::new_york()),
                Box::new(Overpass::default()),
                Box::new(AuthorityTrailProvider::texas()),
                Box::new(UsgsNationalTrails::default()),
            ],
            fixed_providers: false,
        }
    }
}

impl<L> Surveyor<L>
where
    L: PlaceIndex,
{
    #[must_use]
    pub fn new<P: NetworkProvider + 'static>(locator: L, provider: P) -> Self {
        Self {
            locator,
            providers: vec![Box::new(provider)],
            fixed_providers: true,
        }
    }

    #[must_use]
    pub fn with_providers(locator: L, providers: Vec<Box<dyn NetworkProvider>>) -> Self {
        assert!(
            !providers.is_empty(),
            "a surveyor needs at least one provider"
        );
        let distinct = providers
            .iter()
            .map(|provider| provider.descriptor().id)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            distinct.len(),
            providers.len(),
            "a surveyor cannot carry duplicate provider ids"
        );
        Self {
            locator,
            providers,
            fixed_providers: true,
        }
    }

    pub fn survey(
        &self,
        project: &Path,
        query: &str,
        radius_km: f64,
        mut emit: impl FnMut(Event),
    ) -> Result<Summary> {
        validate_project(project)?;
        let query = query.trim();
        ensure!(!query.is_empty(), "enter a US place or trailhead");
        validate_radius(radius_km)?;
        emit(Event::Locating);
        let place =
            cached_place(project, query)?.map_or_else(|| self.locator.locate_us(query), Ok)?;
        write_json_atomic(project.join(LOCATION_CACHE), &place)?;
        emit(Event::Located(place.clone()));
        self.add_region(project, bounds_around(&place, radius_km)?, emit)
    }

    /// Add a rectangle to the project's live area and reconcile its union graph.
    pub fn add_region(
        &self,
        project: &Path,
        bounds: GeoBounds,
        mut emit: impl FnMut(Event),
    ) -> Result<Summary> {
        validate_project(project)?;
        let region = SurveyRegion::new(bounds)?;
        let mut config = project_config(project)?;
        config.managed = true;
        if self.fixed_providers {
            let providers = self.provider_ids();
            if config.providers != providers {
                config.providers = providers;
            }
        }
        if !config.regions.iter().any(|known| known.id == region.id) {
            config.regions.push(region);
        }
        configure_project(project, &config)?;
        self.reconcile(project, &config, true, &mut emit)
    }

    /// Rebuild the live-area graph, fetching any missing region receipts.
    pub fn refresh(&self, project: &Path, mut emit: impl FnMut(Event)) -> Result<Option<Summary>> {
        validate_project(project)?;
        let config = project_config(project)?;
        if config.regions.is_empty() {
            clear_corpus(project)?;
            reap_provider_receipts(project, &[], &self.provider_descriptors())?;
            return Ok(None);
        }
        // Reading refines legacy region identities from their bounds. A refresh
        // is the migration boundary: persist that canonical law before any
        // receipt is judged or fetched.
        configure_project(project, &config)?;
        self.reconcile(project, &config, true, &mut emit).map(Some)
    }

    /// Excise one rectangle and rebuild solely from the surviving receipts.
    pub fn remove_region(
        &self,
        project: &Path,
        id: &str,
        mut emit: impl FnMut(Event),
    ) -> Result<Option<Summary>> {
        validate_project(project)?;
        let mut config = project_config(project)?;
        let before = config.regions.len();
        config.regions.retain(|region| region.id != id);
        ensure!(
            config.regions.len() != before,
            "project has no survey region {id}"
        );
        let _name = config.region_names.remove(id);
        configure_project(project, &config)?;
        if config.regions.is_empty() {
            clear_corpus(project)?;
            reap_provider_receipts(project, &[], &self.provider_descriptors())?;
            return Ok(None);
        }
        self.reconcile(project, &config, false, &mut emit).map(Some)
    }

    /// Move one live rectangle while preserving its ordered slot and human name.
    /// The desired area is committed before acquisition, just like `add_region`,
    /// so an interrupted fetch remains restartable through `refresh`.
    pub fn replace_region(
        &self,
        project: &Path,
        id: &str,
        bounds: GeoBounds,
        mut emit: impl FnMut(Event),
    ) -> Result<Summary> {
        validate_project(project)?;
        let replacement = SurveyRegion::new(bounds)?;
        let mut config = project_config(project)?;
        let slot = config
            .regions
            .iter()
            .position(|region| region.id == id)
            .with_context(|| format!("project has no survey region {id}"))?;
        ensure!(
            config
                .regions
                .iter()
                .enumerate()
                .all(|(known_slot, known)| known_slot == slot || known.id != replacement.id),
            "that map area duplicates another downloaded area"
        );
        if replacement.id == id {
            return self.reconcile(project, &config, true, &mut emit);
        }
        config.regions[slot] = replacement;
        let replacement_id = config.regions[slot].id.clone();
        if let Some(name) = config.region_names.remove(id) {
            let _old = config.region_names.insert(replacement_id, name);
        }
        configure_project(project, &config)?;
        self.reconcile(project, &config, true, &mut emit)
    }

    fn reconcile(
        &self,
        project: &Path,
        config: &TrailDataConfig,
        fetch_missing: bool,
        emit: &mut impl FnMut(Event),
    ) -> Result<Summary> {
        ensure!(
            !config.regions.is_empty(),
            "live area has no survey regions"
        );
        let providers = self.active_providers(config)?;
        let descriptors = providers
            .iter()
            .map(|provider| provider.descriptor())
            .collect::<Vec<_>>();
        if let Some(mut summary) =
            reusable_index(project, config, Some(&descriptors), !self.fixed_providers)?
        {
            summary.reused = true;
            emit(Event::Ready(summary.clone()));
            return Ok(summary);
        }

        let mut sources = Vec::with_capacity(config.regions.len() * providers.len());
        for provider in &providers {
            let descriptor = provider.descriptor();
            for region in &config.regions {
                let raw_relative = PathBuf::from("sources")
                    .join(descriptor.id.as_str())
                    .join(format!("{}.{}", region.id, descriptor.extension));
                let raw_path = project.join(&raw_relative);
                let request_path = raw_path.with_extension(descriptor.request_extension);
                let artifact_path = raw_path.with_extension("json");
                let cached = cached_provider(
                    &raw_path,
                    &request_path,
                    &artifact_path,
                    region,
                    &descriptor,
                )?;
                let (bytes, origin) = if let Some(cached) = cached {
                    (cached.bytes, cached.origin)
                } else {
                    ensure!(
                        fetch_missing,
                        "{} region {} has no intact source receipt",
                        descriptor.label,
                        region.id
                    );
                    let covered = provider.covers(region.bounds);
                    if covered {
                        emit(Event::Ranging {
                            provider: descriptor.id.clone(),
                            region: region.clone(),
                        });
                    }
                    let payload = provider.acquire(region.bounds)?;
                    let artifact = ProviderArtifact {
                        schema: RAW_SCHEMA,
                        provider: descriptor.id.clone(),
                        adapter_revision: descriptor.adapter_revision,
                        region: region.clone(),
                        origin: payload.origin.clone(),
                        request: payload.request.clone(),
                        raw: fingerprint(&payload.bytes),
                    };
                    write_atomic(&request_path, payload.request.as_bytes())?;
                    write_json_atomic(&artifact_path, &artifact)?;
                    // Raw bytes are the provider receipt's commit marker.
                    write_atomic(&raw_path, &payload.bytes)?;
                    if covered {
                        emit(Event::Downloaded {
                            provider: descriptor.id.clone(),
                            bytes: payload.bytes.len() as u64,
                        });
                    }
                    (payload.bytes, payload.origin)
                };
                sources.push(ProviderSource {
                    descriptor: descriptor.clone(),
                    region: region.clone(),
                    raw_relative,
                    fingerprint: fingerprint(&bytes),
                    bytes,
                    origin,
                });
            }
        }
        let terrain = if self.fixed_providers {
            Vec::new()
        } else {
            terrain::acquire(project, &config.regions, fetch_missing, emit)?
        };
        emit(Event::Indexing);
        let summary = index_corpus(project, config, &sources, &providers, &terrain)?;
        reap_provider_receipts(project, &sources, &self.provider_descriptors())?;
        emit(Event::Ready(summary.clone()));
        Ok(summary)
    }

    fn provider_ids(&self) -> Vec<ProviderId> {
        let mut ids = self
            .providers
            .iter()
            .map(|provider| provider.descriptor().id)
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        ids
    }

    fn provider_descriptors(&self) -> Vec<ProviderDescriptor> {
        self.providers
            .iter()
            .map(|provider| provider.descriptor())
            .collect()
    }

    fn active_providers(&self, config: &TrailDataConfig) -> Result<Vec<&dyn NetworkProvider>> {
        let mut providers = Vec::with_capacity(config.providers.len());
        for id in &config.providers {
            let provider = self
                .providers
                .iter()
                .find(|provider| provider.descriptor().id == *id)
                .with_context(|| format!("project requests unavailable trail provider {id}"))?;
            providers.push(provider.as_ref());
        }
        ensure!(!providers.is_empty(), "project has no trail providers");
        Ok(providers)
    }
}

pub struct OsmPayload {
    pub bytes: Vec<u8>,
    pub query: String,
    pub origin: String,
}

impl NetworkProvider for Overpass {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: ProviderId::new("osm").expect("static provider id is valid"),
            label: "OpenStreetMap",
            adapter_revision: 7,
            precedence: 10,
            extension: "osm",
            request_extension: "overpassql",
        }
    }

    fn acquire(&self, bounds: GeoBounds) -> Result<ProviderPayload> {
        let payload = self.fetch(AUTOMATIC_OSM_PROFILE, bounds)?;
        Ok(ProviderPayload {
            bytes: payload.bytes,
            request: payload.query,
            origin: payload.origin,
        })
    }

    fn normalize(&self, shards: &[RawShard<'_>]) -> Result<NormalizedNetwork> {
        let merged = merge_osm(shards)?;
        Ok(NormalizedNetwork {
            drafts: classify_osm_realms(osm::network_from_str(&merged)?),
            context: osm::context_overlays_from_str(&merged)?,
        })
    }
}

type OsmJunction = JunctionKey;

#[derive(Clone, Debug)]
struct OsmConnector {
    draft: usize,
    a: OsmJunction,
    b: OsmJunction,
    length_m: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct OsmWalk {
    junction: OsmJunction,
    owner: OsmJunction,
    distance_m: f64,
}

#[derive(Clone, Debug)]
struct OsmReach {
    owner: OsmJunction,
    distance_m: f64,
    predecessor: Option<(OsmJunction, usize)>,
}

#[derive(Clone, Debug)]
struct OsmBridge {
    distance_m: f64,
    edge: usize,
    a: OsmJunction,
    b: OsmJunction,
}

impl Eq for OsmWalk {}

impl Ord for OsmWalk {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .distance_m
            .total_cmp(&self.distance_m)
            .then_with(|| other.owner.cmp(&self.owner))
            .then_with(|| self.junction.cmp(&other.junction))
    }
}

impl PartialOrd for OsmWalk {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Materialize Finder as a strict projection without amputating Manual's
/// pedestrian graph. Only the nearest short street bridges are promoted from
/// urban circulation into Finder connectors.
fn classify_osm_realms(mut drafts: Vec<SegmentDraft>) -> Vec<SegmentDraft> {
    let mut finder = drafts.iter().map(osm_trail_anchor).collect::<Vec<_>>();
    let mut anchor_junctions = BTreeSet::new();
    let mut connectors = Vec::new();
    for (index, draft) in drafts.iter().enumerate() {
        let Some([a, b]) = draft.junction_keys.clone() else {
            continue;
        };
        if finder[index] {
            let _ = anchor_junctions.insert(a.clone());
            let _ = anchor_junctions.insert(b.clone());
        } else if osm_connector(draft) {
            connectors.push(OsmConnector {
                draft: index,
                a,
                b,
                length_m: draft.geometry.length_m(),
            });
        }
    }

    let mut adjacency = BTreeMap::<OsmJunction, Vec<usize>>::new();
    for (index, connector) in connectors.iter().enumerate() {
        adjacency
            .entry(connector.a.clone())
            .or_default()
            .push(index);
        adjacency
            .entry(connector.b.clone())
            .or_default()
            .push(index);
    }
    let terminals = adjacency
        .keys()
        .filter(|junction| anchor_junctions.contains(junction))
        .cloned()
        .collect::<BTreeSet<_>>();
    retain_nearest_connectors(&terminals, &adjacency, &connectors, &mut finder);
    for (draft, admitted) in drafts.iter_mut().zip(finder) {
        if admitted && draft.realm == WayRealm::Urban {
            draft.realm = WayRealm::Connector;
        }
    }
    drafts
}

fn retain_nearest_connectors(
    terminals: &BTreeSet<OsmJunction>,
    adjacency: &BTreeMap<OsmJunction, Vec<usize>>,
    connectors: &[OsmConnector],
    finder: &mut [bool],
) {
    let mut reach = terminals
        .iter()
        .cloned()
        .map(|terminal| {
            (
                terminal.clone(),
                OsmReach {
                    owner: terminal,
                    distance_m: 0.0,
                    predecessor: None,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut frontier = terminals
        .iter()
        .cloned()
        .map(|terminal| OsmWalk {
            junction: terminal.clone(),
            owner: terminal,
            distance_m: 0.0,
        })
        .collect::<BinaryHeap<_>>();
    while let Some(walk) = frontier.pop() {
        if walk.distance_m > MAX_OSM_CONNECTOR_M
            || reach
                .get(&walk.junction)
                .is_none_or(|known| known.owner != walk.owner || walk.distance_m > known.distance_m)
        {
            continue;
        }
        for edge in adjacency.get(&walk.junction).into_iter().flatten() {
            let connector = &connectors[*edge];
            let next = if connector.a == walk.junction {
                connector.b.clone()
            } else {
                connector.a.clone()
            };
            let candidate = walk.distance_m + connector.length_m;
            let improves = reach.get(&next).is_none_or(|known| {
                candidate.total_cmp(&known.distance_m).is_lt()
                    || candidate.total_cmp(&known.distance_m).is_eq() && walk.owner < known.owner
            });
            if candidate <= MAX_OSM_CONNECTOR_M && improves {
                let _ = reach.insert(
                    next.clone(),
                    OsmReach {
                        owner: walk.owner.clone(),
                        distance_m: candidate,
                        predecessor: Some((walk.junction.clone(), *edge)),
                    },
                );
                frontier.push(OsmWalk {
                    junction: next,
                    owner: walk.owner.clone(),
                    distance_m: candidate,
                });
            }
        }
    }

    let mut nearest = BTreeMap::<OsmJunction, OsmBridge>::new();
    for (edge, connector) in connectors.iter().enumerate() {
        let (Some(a), Some(b)) = (reach.get(&connector.a), reach.get(&connector.b)) else {
            continue;
        };
        if a.owner == b.owner {
            continue;
        }
        let distance_m = a.distance_m + connector.length_m + b.distance_m;
        if distance_m > MAX_OSM_CONNECTOR_M {
            continue;
        }
        let bridge = OsmBridge {
            distance_m,
            edge,
            a: connector.a.clone(),
            b: connector.b.clone(),
        };
        for owner in [&a.owner, &b.owner] {
            let replace = nearest
                .get(owner)
                .is_none_or(|known| bridge_order(&bridge, known).is_lt());
            if replace {
                let _ = nearest.insert(owner.clone(), bridge.clone());
            }
        }
    }
    for bridge in nearest.into_values() {
        finder[connectors[bridge.edge].draft] = true;
        retain_reach(&bridge.a, &reach, connectors, finder);
        retain_reach(&bridge.b, &reach, connectors, finder);
    }
}

fn bridge_order(left: &OsmBridge, right: &OsmBridge) -> Ordering {
    left.distance_m
        .total_cmp(&right.distance_m)
        .then_with(|| left.edge.cmp(&right.edge))
        .then_with(|| left.a.cmp(&right.a))
        .then_with(|| left.b.cmp(&right.b))
}

fn retain_reach(
    start: &OsmJunction,
    reach: &BTreeMap<OsmJunction, OsmReach>,
    connectors: &[OsmConnector],
    finder: &mut [bool],
) {
    let mut junction = start.clone();
    while let Some((prior, edge)) = reach
        .get(&junction)
        .and_then(|label| label.predecessor.clone())
    {
        finder[connectors[edge].draft] = true;
        junction = prior;
    }
}

fn osm_trail_anchor(draft: &SegmentDraft) -> bool {
    draft.realm == WayRealm::Recreational
}

fn osm_connector(draft: &SegmentDraft) -> bool {
    draft.realm == WayRealm::Urban
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
        writeln!(query, "  {}{bbox};", OSM_ROAD_SELECTORS[0]).expect("write to string");
        let hydrology = OSM_HYDROLOGY_SELECTORS[0]
            .strip_prefix("way")
            .expect("way selector");
        writeln!(query, "  way(bn.trailnodes){hydrology};").expect("write to string");
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
    sources: Vec<ProviderReceipt>,
    #[serde(default)]
    elevation: Vec<terrain::TerrainReceipt>,
    graph: SourceFingerprint,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProviderReceipt {
    provider: ProviderId,
    adapter_revision: u16,
    region: SurveyRegion,
    raw_path: PathBuf,
    raw: SourceFingerprint,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProviderArtifact {
    schema: u8,
    provider: ProviderId,
    adapter_revision: u16,
    region: SurveyRegion,
    origin: String,
    request: String,
    raw: SourceFingerprint,
}

struct CachedProvider {
    bytes: Vec<u8>,
    origin: String,
}

struct ProviderSource {
    descriptor: ProviderDescriptor,
    region: SurveyRegion,
    raw_relative: PathBuf,
    fingerprint: SourceFingerprint,
    bytes: Vec<u8>,
    origin: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(default)]
struct GraphLaw {
    snap_tolerance_m: f64,
    conflation: trailgen_core::ConflationPolicy,
    enrichment: EnrichmentConfig,
}

impl Default for GraphLaw {
    fn default() -> Self {
        Self {
            snap_tolerance_m: DEFAULT_SNAP_TOLERANCE_M,
            conflation: trailgen_core::ConflationPolicy::default(),
            enrichment: EnrichmentConfig::default(),
        }
    }
}

fn index_corpus(
    project: &Path,
    config: &TrailDataConfig,
    sources: &[ProviderSource],
    providers: &[&dyn NetworkProvider],
    terrain: &[terrain::TerrainSource],
) -> Result<Summary> {
    let corpus_bytes = sources
        .iter()
        .map(|source| source.bytes.len() as u64)
        .sum::<u64>();
    ensure!(
        corpus_bytes <= MAX_SOURCE_BYTES * providers.len().max(1) as u64 * 4,
        "live trail corpus exceeds {} MiB",
        MAX_SOURCE_BYTES * providers.len().max(1) as u64 * 4 / 1_048_576
    );
    let mut strata = Vec::with_capacity(providers.len());
    let mut overlays = Vec::new();
    for provider in providers {
        let descriptor = provider.descriptor();
        let shards = sources
            .iter()
            .filter(|source| source.descriptor.id == descriptor.id)
            .map(|source| RawShard {
                region: &source.region,
                bytes: &source.bytes,
            })
            .collect::<Vec<_>>();
        let normalized = provider.normalize(&shards)?;
        strata.push(trailgen_core::NetworkStratum {
            precedence: descriptor.precedence,
            drafts: clip_drafts(normalized.drafts, &config.regions),
        });
        overlays.extend(clip_overlays(normalized.context, &config.regions));
    }
    let law = read_graph_law(project)?;
    let conflated = trailgen_core::conflate(strata, law.conflation);
    let drafts = conflated.drafts;
    ensure!(
        !drafts.is_empty(),
        "the live area contains no routable trails"
    );
    let inventory = Inventory {
        trail_segments: drafts.len(),
        road_features: overlays
            .iter()
            .filter(|overlay| overlay.kind == CrossingKind::Road)
            .count(),
        waterway_features: overlays
            .iter()
            .filter(|overlay| overlay.kind == CrossingKind::Water)
            .count(),
    };
    let graph = forge_graph(&drafts, &overlays, terrain, law)?;
    let graph_cache = encode_graph(&graph)?;
    let graph_fingerprint = fingerprint(&graph_cache);
    clear_graph_auxiliaries(project)?;
    write_json_atomic(project.join(CONFLATION_REPORT), &conflated.report)?;
    let bounds = live_bounds(&config.regions).context("live area has no bounds")?;
    store_area(project, Some(bounds))?;
    write_source_manifest(project, sources, &inventory, bounds)?;
    let summary = Summary {
        regions: config.regions.clone(),
        providers: config.providers.clone(),
        inventory,
        vertices: graph.vertices.len(),
        edges: graph.edges.len(),
        elevation_tiles: terrain.len(),
        raw_paths: sources
            .iter()
            .map(|source| source.raw_relative.clone())
            .collect(),
        conflation: trailgen_core::ConflationStats::from(&conflated.report),
        reused: false,
    };
    let receipts = sources
        .iter()
        .map(|source| ProviderReceipt {
            provider: source.descriptor.id.clone(),
            adapter_revision: source.descriptor.adapter_revision,
            region: source.region.clone(),
            raw_path: source.raw_relative.clone(),
            raw: source.fingerprint.clone(),
        })
        .collect();
    write_json_atomic(
        project.join(TRAIL_INDEX),
        &TrailIndex {
            schema: INDEX_SCHEMA,
            summary: summary.clone(),
            sources: receipts,
            elevation: terrain
                .iter()
                .map(|source| source.receipt.clone())
                .collect(),
            graph: graph_fingerprint,
        },
    )?;
    // The binary graph is the workbench commit marker. No GUI can mistake an
    // interrupted indexing pass for a ready corpus.
    write_atomic(&project.join(GRAPH_CACHE), &graph_cache)?;
    Ok(summary)
}

fn forge_graph(
    drafts: &[SegmentDraft],
    overlays: &[ContextOverlay],
    terrain: &[terrain::TerrainSource],
    law: GraphLaw,
) -> Result<WalkGraph> {
    let mut graph = GraphBuilder {
        snap_tolerance_m: law.snap_tolerance_m,
        enrichment: law.enrichment,
    }
    .build(drafts)
    .context("index live-area trail topology")?;
    apply_context_overlays(&mut graph, overlays);
    if let Some(atlas) = terrain::TerrainAtlas::decode(terrain)? {
        trailgen_core::enrich_graph(&mut graph, &atlas, law.enrichment)
            .context("sample live-area topography")?;
    }
    Ok(graph)
}

fn merge_osm(sources: &[RawShard<'_>]) -> Result<String> {
    struct Object {
        version: u64,
        xml: String,
    }

    let mut objects = BTreeMap::<(u8, String), Object>::new();
    for source in sources {
        let raw =
            std::str::from_utf8(source.bytes).context("OpenStreetMap response is not UTF-8 XML")?;
        let document = roxmltree::Document::parse(raw).context("parse region OSM XML")?;
        let root = document.root_element();
        ensure!(root.has_tag_name("osm"), "region source has no OSM root");
        for node in root.children().filter(roxmltree::Node::is_element) {
            let rank = match node.tag_name().name() {
                "node" => 0,
                "way" => 1,
                "relation" => 2,
                _ => continue,
            };
            let Some(id) = node.attribute("id") else {
                continue;
            };
            let version = node
                .attribute("version")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            let xml = raw[node.range()].to_owned();
            let key = (rank, id.to_owned());
            match objects.get_mut(&key) {
                Some(known)
                    if version > known.version || (version == known.version && xml > known.xml) =>
                {
                    *known = Object { version, xml };
                }
                Some(_) => {}
                None => {
                    objects.insert(key, Object { version, xml });
                }
            }
        }
    }
    let mut merged = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<osm version=\"0.6\" generator=\"trailgen-corpus\">\n",
    );
    for object in objects.into_values() {
        merged.push_str(&object.xml);
        merged.push('\n');
    }
    merged.push_str("</osm>\n");
    Ok(merged)
}

fn clip_drafts(drafts: Vec<SegmentDraft>, regions: &[SurveyRegion]) -> Vec<SegmentDraft> {
    drafts
        .into_iter()
        .flat_map(|draft| {
            clip_line(&draft.geometry, regions)
                .into_iter()
                .map(move |geometry| draft.fragment(geometry))
        })
        .collect()
}

fn clip_overlays(overlays: Vec<ContextOverlay>, regions: &[SurveyRegion]) -> Vec<ContextOverlay> {
    overlays
        .into_iter()
        .flat_map(|overlay| {
            clip_line(&overlay.geometry, regions)
                .into_iter()
                .map(move |geometry| {
                    let mut clipped = overlay.clone();
                    clipped.geometry = geometry;
                    clipped
                })
        })
        .collect()
}

fn clip_line(line: &LineString, regions: &[SurveyRegion]) -> Vec<LineString> {
    let mut result = Vec::new();
    let mut points = Vec::new();
    for segment in line.points.windows(2) {
        let [a, b] = [segment[0], segment[1]];
        let mut cuts = vec![0.0, 1.0];
        for region in regions {
            let dx = b.lon - a.lon;
            if dx.abs() > f64::EPSILON {
                cuts.extend(
                    [region.bounds.west, region.bounds.east]
                        .map(|lon| (lon - a.lon) / dx)
                        .into_iter()
                        .filter(|t| (0.0..=1.0).contains(t)),
                );
            }
            let dy = b.lat - a.lat;
            if dy.abs() > f64::EPSILON {
                cuts.extend(
                    [region.bounds.south, region.bounds.north]
                        .map(|lat| (lat - a.lat) / dy)
                        .into_iter()
                        .filter(|t| (0.0..=1.0).contains(t)),
                );
            }
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup_by(|left, right| (*left - *right).abs() <= 1.0e-12);
        for interval in cuts.windows(2) {
            let [from, to] = [interval[0], interval[1]];
            if to - from <= 1.0e-12 {
                continue;
            }
            let midpoint = a.lerp(b, (from + to) * 0.5);
            if regions
                .iter()
                .any(|region| contains(region.bounds, midpoint))
            {
                append_clipped_segment(&mut result, &mut points, a.lerp(b, from), a.lerp(b, to));
            } else {
                seal_line(&mut result, &mut points);
            }
        }
    }
    seal_line(&mut result, &mut points);
    result
}

fn append_clipped_segment(
    result: &mut Vec<LineString>,
    points: &mut Vec<Coord>,
    start: Coord,
    end: Coord,
) {
    let joins = points
        .last()
        .is_some_and(|last| same_location(*last, start));
    if !joins {
        seal_line(result, points);
        points.push(start);
    }
    if points.last().is_none_or(|last| !same_location(*last, end)) {
        points.push(end);
    }
}

fn seal_line(result: &mut Vec<LineString>, points: &mut Vec<Coord>) {
    if points.len() >= 2 {
        result.push(LineString::unchecked(std::mem::take(points)));
    } else {
        points.clear();
    }
}

fn contains(bounds: GeoBounds, coord: Coord) -> bool {
    bounds.west <= coord.lon
        && coord.lon <= bounds.east
        && bounds.south <= coord.lat
        && coord.lat <= bounds.north
}

const fn same_location(left: Coord, right: Coord) -> bool {
    left.lon.to_bits() == right.lon.to_bits() && left.lat.to_bits() == right.lat.to_bits()
}

fn cached_provider(
    raw_path: &Path,
    request_path: &Path,
    artifact_path: &Path,
    region: &SurveyRegion,
    descriptor: &ProviderDescriptor,
) -> Result<Option<CachedProvider>> {
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
    let Ok(artifact) = serde_json::from_str::<ProviderArtifact>(&raw) else {
        return Ok(None);
    };
    let request = match fs::read_to_string(request_path) {
        Ok(request) => request,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("read trail-source request {}", request_path.display()));
        }
    };
    if artifact.schema != RAW_SCHEMA
        || artifact.provider != descriptor.id
        || artifact.adapter_revision != descriptor.adapter_revision
        || &artifact.region != region
        || artifact.request != request
        || artifact.raw != fingerprint(&bytes)
    {
        return Ok(None);
    }
    Ok(Some(CachedProvider {
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

fn reusable_index(
    project: &Path,
    config: &TrailDataConfig,
    descriptors: Option<&[ProviderDescriptor]>,
    terrain_expected: bool,
) -> Result<Option<Summary>> {
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
    if !index_matches_config(&index, config)
        || !project.join(GRAPH_CACHE).is_file()
        || !project.join(CONFLATION_REPORT).is_file()
    {
        return Ok(None);
    }
    let mut receipts = BTreeSet::new();
    for receipt in &index.sources {
        let current = descriptors.and_then(|descriptors| {
            descriptors
                .iter()
                .find(|descriptor| descriptor.id == receipt.provider)
        });
        if !config.regions.contains(&receipt.region)
            || !config.providers.contains(&receipt.provider)
            || receipt.adapter_revision == 0
            || current.is_some_and(|descriptor| {
                receipt.adapter_revision != descriptor.adapter_revision
                    || receipt.raw_path
                        != PathBuf::from("sources")
                            .join(descriptor.id.as_str())
                            .join(format!("{}.{}", receipt.region.id, descriptor.extension))
            })
            || !receipts.insert((receipt.provider.clone(), receipt.region.id.clone()))
        {
            return Ok(None);
        }
        let bytes = match fs::read(project.join(&receipt.raw_path)) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).context("read region source receipt"),
        };
        if fingerprint(&bytes) != receipt.raw {
            return Ok(None);
        }
    }
    if receipts.len() != config.regions.len() * config.providers.len() {
        return Ok(None);
    }
    if terrain_expected {
        let desired = terrain::desired_tiles(&config.regions);
        if index.elevation.len() != desired.len()
            || index
                .elevation
                .iter()
                .map(|receipt| receipt.tile)
                .collect::<Vec<_>>()
                != desired
        {
            return Ok(None);
        }
        for receipt in &index.elevation {
            let bytes = match fs::read(project.join(&receipt.raw_path)) {
                Ok(bytes) => bytes,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(err).context("read terrain receipt"),
            };
            if fingerprint(&bytes) != receipt.raw {
                return Ok(None);
            }
        }
    } else if !index.elevation.is_empty() {
        return Ok(None);
    }
    let graph_bytes = fs::read(project.join(GRAPH_CACHE)).context("read cached trail graph")?;
    if fingerprint(&graph_bytes) != index.graph {
        return Ok(None);
    }
    let Ok(graph) = decode_graph(&graph_bytes) else {
        return Ok(None);
    };
    if graph.vertices.len() != index.summary.vertices || graph.edges.len() != index.summary.edges {
        return Ok(None);
    }
    Ok(Some(index.summary))
}

fn index_matches_config(index: &TrailIndex, config: &TrailDataConfig) -> bool {
    let mut receipts = BTreeSet::new();
    let lawful_receipts = index.sources.iter().all(|receipt| {
        config.regions.contains(&receipt.region)
            && config.providers.contains(&receipt.provider)
            && receipt.adapter_revision != 0
            && receipts.insert((receipt.provider.clone(), receipt.region.id.clone()))
    });
    let raw_paths = index
        .sources
        .iter()
        .map(|receipt| receipt.raw_path.as_path())
        .collect::<BTreeSet<_>>();
    index.schema == INDEX_SCHEMA
        && index.summary.regions == config.regions
        && index.summary.providers == config.providers
        && index.sources.len() == config.regions.len() * config.providers.len()
        && lawful_receipts
        && receipts.len() == index.sources.len()
        && raw_paths.len() == index.sources.len()
        && raw_paths
            == index
                .summary
                .raw_paths
                .iter()
                .map(PathBuf::as_path)
                .collect()
}

fn bounds_around(place: &Place, radius_km: f64) -> Result<GeoBounds> {
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
    validate_region(bounds)?;
    Ok(bounds)
}

fn validate_radius(radius_km: f64) -> Result<()> {
    ensure!(
        radius_km.is_finite() && (MIN_RADIUS_KM..=MAX_RADIUS_KM).contains(&radius_km),
        "trail survey radius must be within {MIN_RADIUS_KM}–{MAX_RADIUS_KM} km"
    );
    Ok(())
}

pub fn validate_region(bounds: GeoBounds) -> Result<()> {
    let area = (bounds.east - bounds.west) * (bounds.north - bounds.south);
    ensure!(
        bounds.is_valid(),
        "survey region has invalid lon/lat bounds"
    );
    ensure!(
        area <= MAX_REGION_DEG2,
        "survey region spans {area:.2} square degrees; limit is {MAX_REGION_DEG2:.2}"
    );
    Ok(())
}

fn validate_project(project: &Path) -> Result<()> {
    ensure!(
        project.join("trailgen.toml").is_file(),
        "{} is not a trailgen project",
        project.display()
    );
    Ok(())
}

fn region_key(bounds: GeoBounds) -> String {
    let digest = Sha256::digest(
        format!(
            "trail-region-v1:{:.8}:{:.8}:{:.8}:{:.8}",
            bounds.west, bounds.south, bounds.east, bounds.north
        )
        .as_bytes(),
    );
    hex(&digest[..12])
}

#[must_use]
pub fn live_bounds(regions: &[SurveyRegion]) -> Option<GeoBounds> {
    regions
        .iter()
        .map(|region| region.bounds)
        .reduce(|left, right| {
            GeoBounds::new(
                left.west.min(right.west),
                left.south.min(right.south),
                left.east.max(right.east),
                left.north.max(right.north),
            )
        })
}

fn read_graph_law(project: &Path) -> Result<GraphLaw> {
    let path = project.join("trailgen.toml");
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

/// Read and validate the project's durable live area.
pub fn project_config(project: &Path) -> Result<TrailDataConfig> {
    #[derive(Deserialize)]
    struct ProjectConfig {
        #[serde(default)]
        trail_data: TrailDataConfig,
        #[serde(default)]
        area: Option<GeoBounds>,
    }

    let path = project.join("trailgen.toml");
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let document =
        toml::from_str::<toml::Value>(&raw).with_context(|| format!("parse {}", path.display()))?;
    let parsed = toml::from_str::<ProjectConfig>(&raw)
        .with_context(|| format!("parse {}", path.display()))?;
    let mut config = parsed.trail_data;
    if config.providers == legacy_automatic_provider_ids() {
        config.providers = automatic_provider_ids();
    }
    let mut names = std::mem::take(&mut config.region_names);
    for region in &mut config.regions {
        validate_region(region.bounds)?;
        let legacy_id = region.id.clone();
        region.id = region_key(region.bounds);
        if legacy_id != region.id
            && let Some(name) = names.remove(&legacy_id)
        {
            let _replaced = names.insert(region.id.clone(), name);
        }
    }
    config.region_names = names;
    let legacy_place = document
        .get("trail_data")
        .and_then(|trail_data| trail_data.get("place"))
        .and_then(toml::Value::as_str)
        .is_some_and(|place| !place.trim().is_empty());
    if config.regions.is_empty()
        && legacy_place
        && let Some(bounds) = parsed.area
    {
        config.regions.push(SurveyRegion::new(bounds)?);
    }
    config.managed |= legacy_place || !config.regions.is_empty();
    validate_config(&config)?;
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
    let config = project_config(project)?;
    if !index_matches_config(&index, &config) {
        return Ok(None);
    }
    let graph = match fs::metadata(project.join(GRAPH_CACHE)) {
        Ok(graph) => graph,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).context("inspect cached trail graph"),
    };
    if !graph.is_file()
        || graph.len() != index.graph.bytes
        || !project.join(CONFLATION_REPORT).is_file()
    {
        return Ok(None);
    }
    if index.summary.elevation_tiles != index.elevation.len() {
        return Ok(None);
    }
    for receipt in &index.elevation {
        let bytes = match fs::read(project.join(&receipt.raw_path)) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).context("read terrain receipt"),
        };
        if fingerprint(&bytes) != receipt.raw {
            return Ok(None);
        }
    }
    Ok(Some(index.summary))
}

/// Persist the canonical live area without disturbing other project law.
pub fn configure_project(project: &Path, trail_data: &TrailDataConfig) -> Result<()> {
    validate_config(trail_data)?;
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

/// Name one map area without perturbing its immutable acquisition identity.
pub fn name_region(project: &Path, id: &str, name: &str) -> Result<TrailDataConfig> {
    validate_project(project)?;
    let mut config = project_config(project)?;
    ensure!(
        config.regions.iter().any(|region| region.id == id),
        "project has no survey region {id}"
    );
    let name = name.trim();
    if name.is_empty() {
        let _old = config.region_names.remove(id);
    } else {
        validate_region_name(name)?;
        let _old = config.region_names.insert(id.to_owned(), name.to_owned());
    }
    configure_project(project, &config)?;
    Ok(config)
}

fn validate_config(config: &TrailDataConfig) -> Result<()> {
    let mut ids = BTreeSet::new();
    for region in &config.regions {
        region.validate()?;
        ensure!(
            ids.insert(&region.id),
            "duplicate survey region {}",
            region.id
        );
    }
    for (id, name) in &config.region_names {
        ensure!(
            ids.contains(id),
            "map-area name refers to unknown region {id}"
        );
        validate_region_name(name)?;
    }
    let providers = config.providers.iter().collect::<BTreeSet<_>>();
    ensure!(
        !providers.is_empty(),
        "trail data needs at least one provider"
    );
    ensure!(
        providers.len() == config.providers.len(),
        "trail data contains duplicate providers"
    );
    Ok(())
}

fn validate_region_name(name: &str) -> Result<()> {
    ensure!(!name.trim().is_empty(), "map-area name is empty");
    ensure!(name.chars().count() <= 80, "map-area name is too long");
    ensure!(
        !name.chars().any(char::is_control),
        "map-area name contains control characters"
    );
    Ok(())
}

fn store_area(project: &Path, bounds: Option<GeoBounds>) -> Result<()> {
    let path = project.join("trailgen.toml");
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut config =
        toml::from_str::<toml::Value>(&raw).with_context(|| format!("parse {}", path.display()))?;
    let table = config
        .as_table_mut()
        .context("trailgen.toml root must be a table")?;
    if let Some(bounds) = bounds {
        table.insert("area".to_owned(), toml::Value::try_from(bounds)?);
    } else {
        table.remove("area");
    }
    write_atomic(&path, toml::to_string_pretty(&config)?.as_bytes())
}

fn write_source_manifest(
    project: &Path,
    sources: &[ProviderSource],
    inventory: &Inventory,
    bounds: GeoBounds,
) -> Result<()> {
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
    manifest
        .candidates
        .retain(|candidate| !is_live_provider_candidate(candidate));
    for source in sources {
        let raw_path = source.raw_relative.display().to_string();
        let region = source.region.bounds;
        let origin = format!(
            "provider:{}:{} bbox={},{},{},{}",
            source.descriptor.id,
            source.origin,
            region.west,
            region.south,
            region.east,
            region.north
        );
        manifest.candidates.push(candidate(
            &raw_path,
            SourceKind::TrailNetwork,
            if source.descriptor.id.as_str() == "osm" {
                "osm-xml-network"
            } else {
                "geojson-network"
            },
            &source.fingerprint,
            &origin,
        ));
        if source.descriptor.id.as_str() == "osm" && inventory.road_features > 0 {
            manifest.candidates.push(candidate(
                &raw_path,
                SourceKind::Road,
                "osm-road-context",
                &source.fingerprint,
                &origin,
            ));
        }
        if source.descriptor.id.as_str() == "osm" && inventory.waterway_features > 0 {
            manifest.candidates.push(candidate(
                &raw_path,
                SourceKind::Hydrology,
                "osm-hydrology-context",
                &source.fingerprint,
                &origin,
            ));
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

fn clear_corpus(project: &Path) -> Result<()> {
    clear_graph_auxiliaries(project)?;
    remove_files(project, &[TRAIL_INDEX, GRAPH_CACHE, CONFLATION_REPORT])?;
    store_area(project, None)?;
    let manifest_path = project.join(SOURCE_MANIFEST);
    match fs::read_to_string(&manifest_path) {
        Ok(raw) => {
            let mut manifest = serde_json::from_str::<SourceManifest>(&raw)
                .with_context(|| format!("parse {}", manifest_path.display()))?;
            manifest
                .candidates
                .retain(|candidate| !is_live_provider_candidate(candidate));
            manifest.adapters = adapter_registry();
            manifest.recommendations = discovery_recommendations(None);
            manifest.coverage = source_coverage(
                &manifest.adapters,
                &manifest.recommendations,
                &manifest.candidates,
            );
            write_json_atomic(manifest_path, &manifest)?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| format!("read {}", manifest_path.display()));
        }
    }
    Ok(())
}

fn remove_files(project: &Path, relatives: &[&str]) -> Result<()> {
    for relative in relatives {
        match fs::remove_file(project.join(relative)) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("remove {relative}")),
        }
    }
    Ok(())
}

fn reap_provider_receipts(
    project: &Path,
    sources: &[ProviderSource],
    descriptors: &[ProviderDescriptor],
) -> Result<()> {
    let desired = sources
        .iter()
        .flat_map(|source| {
            let raw = project.join(&source.raw_relative);
            [
                raw.clone(),
                raw.with_extension(source.descriptor.request_extension),
                raw.with_extension("json"),
            ]
        })
        .collect::<BTreeSet<_>>();
    for descriptor in descriptors {
        let root = project.join("sources").join(descriptor.id.as_str());
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", root.display()));
            }
        };
        let owned_extensions = [descriptor.extension, descriptor.request_extension, "json"];
        for entry in entries {
            let path = entry?.path();
            let owned = path.is_file()
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(region_receipt_stem)
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| owned_extensions.contains(&extension));
            if owned && !desired.contains(&path) {
                fs::remove_file(&path)
                    .with_context(|| format!("reap obsolete receipt {}", path.display()))?;
            }
        }
    }
    Ok(())
}

fn region_receipt_stem(stem: &str) -> bool {
    stem.len() == 24 && stem.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_live_provider_candidate(candidate: &SourceCandidate) -> bool {
    candidate
        .origin
        .as_deref()
        .is_some_and(|origin| origin.starts_with("provider:") || origin.starts_with("overpass:"))
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

pub fn provider_client(task: &str, timeout: Duration) -> Result<reqwest::blocking::Client> {
    let builder = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(user_agent(task));
    #[cfg(unix)]
    let builder = if let Some(raw) = env::var_os("TRAILGEN_HTTP_UNIX_SOCKET") {
        let socket = PathBuf::from(raw);
        ensure!(
            socket.is_absolute(),
            "TRAILGEN_HTTP_UNIX_SOCKET must name an absolute path"
        );
        builder.unix_socket(socket)
    } else {
        builder
    };
    #[cfg(not(unix))]
    ensure!(
        env::var_os("TRAILGEN_HTTP_UNIX_SOCKET").is_none(),
        "TRAILGEN_HTTP_UNIX_SOCKET is unavailable on this platform"
    );
    builder.build().context("build HTTP provider client")
}

fn fingerprint(bytes: &[u8]) -> SourceFingerprint {
    SourceFingerprint {
        bytes: bytes.len() as u64,
        sha256: hex(&Sha256::digest(bytes)),
    }
}

/// Persist explicit CLI audit surfaces, publishing the binary graph last.
pub fn store_graph(project: &Path, graph: &WalkGraph) -> Result<()> {
    let graph_cache = encode_graph(graph)?;
    clear_graph_auxiliaries(project)?;
    write_atomic(
        &project.join(GRAPH_GEOJSON),
        &serde_json::to_vec_pretty(&geojson::graph_to_geojson(graph))?,
    )?;
    write_atomic(
        &project.join("cache/edges.csv"),
        graph_edges_csv(graph).as_bytes(),
    )?;
    write_atomic(
        &project.join("cache/vertices.csv"),
        graph_vertices_csv(graph).as_bytes(),
    )?;
    write_atomic(&project.join(GRAPH_CACHE), &graph_cache)
}

fn clear_graph_auxiliaries(project: &Path) -> Result<()> {
    remove_files(project, GRAPH_AUXILIARIES)
}

fn graph_vertices_csv(graph: &WalkGraph) -> String {
    let mut out = String::from("vertex_id,junction_id,lon,lat,elevation_m,wkt\n");
    for vertex in &graph.vertices {
        let Coord { lon, lat, ele } = vertex.coord;
        writeln!(
            out,
            "{},{},{lon:.7},{lat:.7},{},{}",
            vertex.id.0,
            csv_cell(vertex.junction.as_ref().map_or("", |key| key.0.as_str())),
            csv_f64(ele),
            csv_cell(&point_wkt(vertex.coord))
        )
        .expect("write to string");
    }
    out
}

fn graph_edges_csv(graph: &WalkGraph) -> String {
    let mut out = String::from(
        "edge_id,from_vertex,to_vertex,travel,length_m,ascent_m,descent_m,grade_abs_mean,grade_abs_max,sustained_steep_m,hill_slope_deg,way_kind,realm,geometry_claim,crossing_control,trail_standing,trail_marking,terrain,surface,terrain_confidence,terrain_evidence,access,access_confidence,access_provenance,road_exposure,confidence,lower_limb_load_forward_km,moving_time_forward_s,lower_limb_load_reverse_km,moving_time_reverse_s,seed_count,seed_provenance,elevation_provenance,road_crossings,water_crossings,provenance,wkt\n",
    );
    for edge in &graph.edges {
        let (roads, water) = edge_crossing_counts(edge);
        let row = [
            edge.id.0.to_string(),
            edge.a.0.to_string(),
            edge.b.0.to_string(),
            edge_travel_tag(edge.attr.travel).to_owned(),
            format!("{:.3}", edge.attr.length_m),
            format!("{:.3}", edge.attr.ascent_m),
            format!("{:.3}", edge.attr.descent_m),
            format!("{:.6}", edge.attr.grade_abs_mean),
            format!("{:.6}", edge.attr.grade_abs_max),
            format!("{:.3}", edge.attr.sustained_steep_m),
            csv_f64(edge.attr.hill_slope_deg),
            way_kind_tag(edge.attr.way_kind).to_owned(),
            way_realm_tag(edge.attr.realm).to_owned(),
            geometry_claim_tag(edge.attr.geometry_claim).to_owned(),
            crossing_control_tag(edge.attr.crossing_control).to_owned(),
            trail_standing_tag(edge.attr.standing).to_owned(),
            trail_marking_tag(edge.attr.marking).to_owned(),
            terrain_tag(edge.attr.terrain).to_owned(),
            csv_cell(edge.attr.surface.as_deref().unwrap_or("")),
            format!("{:.6}", edge.attr.terrain_confidence),
            csv_cell(&terrain_evidence_summary(&edge.attr.terrain_evidence)),
            access_tag(edge.attr.access).to_owned(),
            format!("{:.6}", edge.attr.access_confidence),
            csv_cell(&provenance_summary(&edge.attr.access_provenance)),
            format!("{:.6}", edge.attr.road_exposure),
            format!("{:.6}", edge.attr.confidence),
            format!("{:.6}", edge.attr.traversal.forward.lower_limb_load_km),
            format!("{:.3}", edge.attr.traversal.forward.moving_time_s),
            format!("{:.6}", edge.attr.traversal.reverse.lower_limb_load_km),
            format!("{:.3}", edge.attr.traversal.reverse.moving_time_s),
            edge.attr.seed_count.to_string(),
            csv_cell(&provenance_summary(&edge.attr.seed_provenance)),
            csv_cell(&provenance_summary(&edge.attr.elevation_provenance)),
            roads.to_string(),
            water.to_string(),
            csv_cell(&provenance_summary(&edge.attr.provenance)),
            csv_cell(&line_wkt(&edge.geometry)),
        ];
        writeln!(out, "{}", row.join(",")).expect("write to string");
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

const fn way_kind_tag(class: WayKind) -> &'static str {
    match class {
        WayKind::Unknown => "unknown",
        WayKind::Path => "path",
        WayKind::Footway => "footway",
        WayKind::Sidewalk => "sidewalk",
        WayKind::Crossing => "crossing",
        WayKind::Track => "track",
        WayKind::ServiceRoad => "service",
        WayKind::PedestrianStreet => "pedestrian",
        WayKind::Steps => "steps",
        WayKind::Bridleway => "bridleway",
        WayKind::Bushwhack => "bushwhack",
        WayKind::Roadway => "road",
        WayKind::Cycleway => "cycleway",
    }
}

const fn trail_standing_tag(standing: TrailStanding) -> &'static str {
    match standing {
        TrailStanding::Unknown => "unknown",
        TrailStanding::Established => "established",
        TrailStanding::Unmaintained => "unmaintained",
        TrailStanding::Informal => "informal",
        TrailStanding::Historical => "historical",
    }
}

const fn way_realm_tag(realm: WayRealm) -> &'static str {
    match realm {
        WayRealm::Recreational => "recreational",
        WayRealm::Connector => "connector",
        WayRealm::Urban => "urban",
    }
}

const fn geometry_claim_tag(claim: GeometryClaim) -> &'static str {
    match claim {
        GeometryClaim::Surveyed => "surveyed",
        GeometryClaim::CenterlineProxy => "centerline-proxy",
    }
}

const fn crossing_control_tag(control: CrossingControl) -> &'static str {
    match control {
        CrossingControl::None => "none",
        CrossingControl::Uncontrolled => "uncontrolled",
        CrossingControl::Marked => "marked",
        CrossingControl::Signals => "signals",
        CrossingControl::GradeSeparated => "grade-separated",
    }
}

const fn trail_marking_tag(marking: TrailMarking) -> &'static str {
    match marking {
        TrailMarking::Unknown => "unknown",
        TrailMarking::Marked => "marked",
        TrailMarking::Unmarked => "unmarked",
    }
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
    use std::{cell::Cell, rc::Rc};
    use trailgen_core::JunctionPolicy;

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

    #[derive(Clone, Default)]
    struct FixedProvider {
        calls: Rc<Cell<usize>>,
        fail_at: Option<usize>,
    }

    impl NetworkProvider for FixedProvider {
        fn descriptor(&self) -> ProviderDescriptor {
            ProviderDescriptor {
                id: ProviderId::new("fixture").unwrap(),
                label: "Fixture trails",
                adapter_revision: 1,
                precedence: 0,
                extension: "osm",
                request_extension: "request",
            }
        }

        fn acquire(&self, bounds: GeoBounds) -> Result<ProviderPayload> {
            assert!(bounds.is_valid());
            let call = self.calls.get() + 1;
            self.calls.set(call);
            if self.fail_at == Some(call) {
                anyhow::bail!("fixture acquisition failure");
            }
            Ok(ProviderPayload {
                bytes: include_bytes!("../tests/fixtures/tiny-overpass.osm").to_vec(),
                request: "fixture request".to_owned(),
                origin: "fixture://overpass".to_owned(),
            })
        }

        fn normalize(&self, shards: &[RawShard<'_>]) -> Result<NormalizedNetwork> {
            let merged = merge_osm(shards)?;
            Ok(NormalizedNetwork {
                drafts: osm::network_from_str(&merged)?,
                context: osm::context_overlays_from_str(&merged)?,
            })
        }
    }

    #[derive(Clone, Default)]
    struct FixedUsgs {
        calls: Rc<Cell<usize>>,
    }

    impl NetworkProvider for FixedUsgs {
        fn descriptor(&self) -> ProviderDescriptor {
            UsgsNationalTrails::default().descriptor()
        }

        fn acquire(&self, bounds: GeoBounds) -> Result<ProviderPayload> {
            assert!(bounds.is_valid());
            self.calls.set(self.calls.get() + 1);
            Ok(ProviderPayload {
                bytes: include_bytes!("../tests/fixtures/tiny-usgs-trails.geojson").to_vec(),
                request: "fixture USGS request".to_owned(),
                origin: "fixture://usgs-national-trails".to_owned(),
            })
        }

        fn normalize(&self, shards: &[RawShard<'_>]) -> Result<NormalizedNetwork> {
            UsgsNationalTrails::default().normalize(shards)
        }
    }

    fn fixed_surveyor() -> (Surveyor<FixedPlace>, Rc<Cell<usize>>) {
        let provider = FixedProvider::default();
        let calls = Rc::clone(&provider.calls);
        (Surveyor::new(FixedPlace, provider), calls)
    }

    fn fixed_multi_surveyor() -> (Surveyor<FixedPlace>, Rc<Cell<usize>>, Rc<Cell<usize>>) {
        let osm = FixedProvider::default();
        let usgs = FixedUsgs::default();
        let osm_calls = Rc::clone(&osm.calls);
        let usgs_calls = Rc::clone(&usgs.calls);
        (
            Surveyor::with_providers(FixedPlace, vec![Box::new(osm), Box::new(usgs)]),
            osm_calls,
            usgs_calls,
        )
    }

    #[test]
    fn line_clipping_realizes_the_exact_rectangle_union() -> Result<()> {
        let line = LineString::new(vec![Coord::new(-2.0, 0.0), Coord::new(2.0, 0.0)])?;
        let regions = [
            SurveyRegion::new(GeoBounds::new(-1.0, -0.5, -0.2, 0.5))?,
            SurveyRegion::new(GeoBounds::new(0.3, -0.5, 1.0, 0.5))?,
        ];

        let clipped = clip_line(&line, &regions);

        assert_eq!(clipped.len(), 2);
        for (actual, expected) in [
            (clipped[0].start(), Coord::new(-1.0, 0.0)),
            (clipped[0].end(), Coord::new(-0.2, 0.0)),
            (clipped[1].start(), Coord::new(0.3, 0.0)),
            (clipped[1].end(), Coord::new(1.0, 0.0)),
        ] {
            assert!((actual.lon - expected.lon).abs() < 1.0e-12);
            assert!((actual.lat - expected.lat).abs() < 1.0e-12);
        }
        Ok(())
    }

    #[test]
    fn clipping_preserves_contracted_osm_junctions() -> Result<()> {
        let raw = r#"<osm version="0.6">
          <node id="1" lon="-74.1501" lat="41.2501"/>
          <node id="2" lon="-74.1402" lat="41.2502"/>
          <node id="3" lon="-74.1303" lat="41.2503"/>
          <node id="4" lon="-74.1404" lat="41.2604"/>
          <way id="10"><nd ref="1"/><nd ref="2"/><nd ref="3"/><tag k="highway" v="path"/></way>
          <way id="11"><nd ref="2"/><nd ref="4"/><tag k="highway" v="path"/></way>
        </osm>"#;
        let region = SurveyRegion::new(GeoBounds::new(-74.2, 41.2, -74.1, 41.3))?;

        let drafts = clip_drafts(osm::network_from_str(raw)?, &[region]);
        assert!(
            drafts
                .iter()
                .all(|draft| draft.junctions == JunctionPolicy::ExplicitEndpoints)
        );
        let graph = GraphBuilder::default().build(&drafts)?;
        let junction = graph
            .vertices
            .iter()
            .find(|vertex| same_location(vertex.coord, Coord::new(-74.1402, 41.2502)))
            .context("shared OSM node should survive clipping")?;
        assert_eq!(graph.adjacency[junction.id.0].len(), 3);
        Ok(())
    }

    #[test]
    fn clipping_does_not_join_distinct_osm_ways_at_a_shared_boundary_coordinate() -> Result<()> {
        let raw = r#"<osm version="0.6">
          <node id="1" lon="-1" lat="0"/><node id="2" lon="0" lat="0"/>
          <node id="3" lon="-1" lat="0"/><node id="4" lon="0.5" lat="0"/>
          <way id="10"><nd ref="1"/><nd ref="2"/><tag k="highway" v="path"/></way>
          <way id="11"><nd ref="3"/><nd ref="4"/><tag k="highway" v="path"/></way>
        </osm>"#;
        let region = SurveyRegion::new(GeoBounds::new(-0.5, -0.5, 0.5, 0.5))?;

        let drafts = clip_drafts(osm::network_from_str(raw)?, &[region]);
        let graph = GraphBuilder::default().build(&drafts)?;

        assert_eq!(graph.edges.len(), 2);
        assert_eq!(
            graph
                .vertices
                .iter()
                .filter(|vertex| same_location(vertex.coord, Coord::new(-0.5, 0.0)))
                .count(),
            2
        );
        Ok(())
    }

    #[test]
    fn survey_sequesters_indexes_and_reuses_one_canonical_pipeline() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path();
        fs::write(project.join("trailgen.toml"), "name = 'Harriman'\n")?;
        let (surveyor, calls) = fixed_surveyor();
        let mut first_events = Vec::new();

        let first = surveyor.survey(project, "Harriman", 20.0, |event| {
            first_events.push(event.status());
        })?;

        assert!(!first.reused);
        assert_eq!(first.inventory.trail_segments, 2);
        assert_eq!(first.inventory.road_features, 1);
        assert_eq!(first.inventory.waterway_features, 1);
        assert_eq!(calls.get(), 1);
        assert_eq!(first_events.len(), 6);
        assert_eq!(first.regions.len(), 1);
        assert_eq!(first.providers, vec![ProviderId::new("fixture")?]);
        assert_eq!(first.raw_paths.len(), 1);
        assert!(project.join(&first.raw_paths[0]).is_file());
        assert_eq!(indexed_summary(project)?, Some(first.clone()));
        assert!(
            project
                .join(&first.raw_paths[0])
                .with_extension("json")
                .is_file()
        );
        assert!(
            project
                .join(&first.raw_paths[0])
                .with_extension("request")
                .is_file()
        );
        for artifact in [LOCATION_CACHE, TRAIL_INDEX, GRAPH_CACHE, SOURCE_MANIFEST] {
            assert!(project.join(artifact).is_file(), "missing {artifact}");
        }
        for absent in [GRAPH_GEOJSON, "cache/edges.csv", "cache/vertices.csv"] {
            assert!(!project.join(absent).exists(), "unsolicited {absent}");
        }
        let manifest = serde_json::from_str::<SourceManifest>(&fs::read_to_string(
            project.join(SOURCE_MANIFEST),
        )?)?;
        assert_eq!(manifest.candidates.len(), 1);
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
        assert_eq!(second.raw_paths, first.raw_paths);
        assert_eq!(calls.get(), 1);
        assert_eq!(second_events.len(), 3);
        assert!(
            second_events
                .last()
                .is_some_and(|event| event.contains("CACHED"))
        );
        Ok(())
    }

    #[test]
    fn providers_keep_independent_receipts_but_feed_one_graph() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path();
        fs::write(project.join("trailgen.toml"), "name = 'Harriman'\n")?;
        let (surveyor, osm_calls, usgs_calls) = fixed_multi_surveyor();

        let first = surveyor.survey(project, "Harriman", 20.0, drop)?;

        assert_eq!(osm_calls.get(), 1);
        assert_eq!(usgs_calls.get(), 1);
        assert_eq!(first.providers.len(), 2);
        assert_eq!(first.raw_paths.len(), 2);
        assert_eq!(first.conflation.strata, 2);
        let graph = decode_graph(&fs::read(project.join(GRAPH_CACHE))?)?;
        let sources = graph
            .edges
            .iter()
            .flat_map(|edge| &edge.attr.provenance)
            .map(|provenance| provenance.source.as_str())
            .collect::<BTreeSet<_>>();
        assert!(sources.contains("osm-xml"));
        assert!(sources.contains("usgs-national-trails"));

        let cached = surveyor.survey(project, "Harriman", 20.0, drop)?;
        assert!(cached.reused);
        assert_eq!(osm_calls.get(), 1);
        assert_eq!(usgs_calls.get(), 1);

        let usgs_raw = first
            .raw_paths
            .iter()
            .find(|path| path.starts_with("sources/usgs-national-trails"))
            .context("USGS receipt missing")?;
        fs::write(project.join(usgs_raw), b"damaged")?;
        assert_eq!(indexed_summary(project)?, Some(first));
        let repaired = surveyor.survey(project, "Harriman", 20.0, drop)?;
        assert!(!repaired.reused);
        assert_eq!(osm_calls.get(), 1);
        assert_eq!(usgs_calls.get(), 2);
        Ok(())
    }

    #[test]
    fn damaged_derived_receipts_are_repaired_without_poisoning_the_project() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path();
        fs::write(project.join("trailgen.toml"), "name = 'Harriman'\n")?;
        let (surveyor, calls) = fixed_surveyor();
        let first = surveyor.survey(project, "Harriman", 20.0, drop)?;
        fs::write(project.join(TRAIL_INDEX), b"{")?;

        let repaired = surveyor.survey(project, "Harriman", 20.0, drop)?;

        assert!(!repaired.reused);
        assert_eq!(calls.get(), 1);
        let artifact = project.join(&first.raw_paths[0]).with_extension("json");
        fs::write(artifact, b"{")?;
        fs::write(project.join(TRAIL_INDEX), b"{")?;

        surveyor.survey(project, "Harriman", 20.0, drop)?;

        assert_eq!(calls.get(), 2);
        Ok(())
    }

    #[test]
    fn overlapping_regions_form_one_deduplicated_corpus_and_can_be_excised() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path();
        fs::write(project.join("trailgen.toml"), "name = 'Harriman'\n")?;
        let (surveyor, calls) = fixed_surveyor();
        let west = GeoBounds::new(-74.130, 41.225, -74.120, 41.235);
        let east = GeoBounds::new(-74.127, 41.228, -74.123, 41.234);

        let first = surveyor.add_region(project, west, drop)?;
        let joined = surveyor.add_region(project, east, drop)?;

        assert_eq!(calls.get(), 2);
        assert_eq!(joined.regions.len(), 2);
        assert_eq!(joined.inventory, first.inventory);
        assert_eq!(joined.vertices, first.vertices);
        assert_eq!(joined.edges, first.edges);
        assert_eq!(project_config(project)?.regions, joined.regions);

        let shorn = surveyor
            .remove_region(project, &joined.regions[1].id, drop)?
            .context("one region should survive")?;
        assert_eq!(shorn.regions, first.regions);
        assert_eq!(calls.get(), 2);
        let removed = joined
            .raw_paths
            .iter()
            .find(|path| path.to_string_lossy().contains(&joined.regions[1].id))
            .context("second region receipt missing")?;
        assert!(!project.join(removed).exists());
        assert!(
            surveyor
                .remove_region(project, &first.regions[0].id, drop)?
                .is_none()
        );
        assert!(project_config(project)?.regions.is_empty());
        assert!(project_config(project)?.managed);
        assert!(!project.join(GRAPH_CACHE).exists());
        assert!(!project.join(&first.raw_paths[0]).exists());
        Ok(())
    }

    #[test]
    fn replacement_intent_survives_an_interrupted_acquisition() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path();
        fs::write(project.join("trailgen.toml"), "name = 'Harriman'\n")?;
        let provider = FixedProvider {
            fail_at: Some(3),
            ..FixedProvider::default()
        };
        let calls = Rc::clone(&provider.calls);
        let surveyor = Surveyor::new(FixedPlace, provider);
        let west = GeoBounds::new(-74.130, 41.225, -74.120, 41.235);
        let east = GeoBounds::new(-74.127, 41.228, -74.123, 41.234);
        let moved = GeoBounds::new(-74.131, 41.224, -74.119, 41.236);
        let first = surveyor.add_region(project, west, drop)?;
        let joined = surveyor.add_region(project, east, drop)?;
        let displaced = first.regions[0].id.clone();
        let survivor = joined.regions[1].clone();
        name_region(project, &displaced, "West Gate")?;

        let fault = surveyor
            .replace_region(project, &displaced, moved, drop)
            .expect_err("the fixture's third acquisition must fail");
        let replacement = SurveyRegion::new(moved)?;
        let config = project_config(project)?;

        assert!(fault.to_string().contains("fixture acquisition failure"));
        assert_eq!(calls.get(), 3);
        assert_eq!(config.regions, vec![replacement.clone(), survivor]);
        assert_eq!(
            config.region_names,
            BTreeMap::from([(replacement.id, "West Gate".to_owned())])
        );
        assert!(indexed_summary(project)?.is_none());
        Ok(())
    }
}
