use crate::{chrome, map};
use anyhow::{Context as _, Result, bail, ensure};
use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{
    Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2, epaint::TextShape, vec2,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{Cursor, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};
use trailgen_core::Coord;

const SCHEMA: u32 = 1;
const INDEX: &str = "civic/index.json";
const SHAPES: &str = "civic/shapes";
const CATALOG: &[u8] = include_bytes!("../assets/civic-areas.tsv.zst");
const COMPLETION_LIMIT: usize = 8;
const CHUNK_POINTS: usize = 96;
const CENSUS_ENDPOINT: &str =
    "https://tigerweb.geo.census.gov/arcgis/rest/services/TIGERweb/tigerWMS_Current/MapServer";
const NYC_ENDPOINT: &str = "https://services5.arcgis.com/GfwWNkhOj9bNBqoJ/arcgis/rest/services/NYC_Borough_Boundary/FeatureServer/0";
const MAGENTA: Color32 = Color32::from_rgb(190, 91, 147);
const MAGENTA_INK: Color32 = Color32::from_rgb(76, 34, 57);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CivicSource {
    CensusIncorporated,
    CensusDesignated,
    NycBorough,
}

impl CivicSource {
    const fn slug(self) -> &'static str {
        match self {
            Self::CensusIncorporated => "inc",
            Self::CensusDesignated => "cdp",
            Self::NycBorough => "nyc",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CivicKey {
    source: CivicSource,
    geoid: String,
}

impl CivicKey {
    fn slug(&self) -> String {
        format!("{}-{}", self.source.slug(), self.geoid)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CivicRecord {
    pub key: CivicKey,
    pub name: String,
    pub kind: String,
    pub jurisdiction: String,
    pub anchor: Coord,
}

impl CivicRecord {
    #[must_use]
    pub fn caption(&self) -> String {
        format!(
            "{} · {} · {}",
            self.name,
            self.jurisdiction,
            self.kind.to_ascii_lowercase()
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CivicBounds {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

impl CivicBounds {
    const EMPTY: Self = Self {
        west: f64::INFINITY,
        south: f64::INFINITY,
        east: f64::NEG_INFINITY,
        north: f64::NEG_INFINITY,
    };

    fn admit(&mut self, coord: Coord) {
        self.west = self.west.min(coord.lon);
        self.south = self.south.min(coord.lat);
        self.east = self.east.max(coord.lon);
        self.north = self.north.max(coord.lat);
    }

    fn validate(self) -> Result<()> {
        ensure!(
            [self.west, self.south, self.east, self.north]
                .into_iter()
                .all(f64::is_finite),
            "civic area has nonfinite bounds"
        );
        ensure!(
            self.west < self.east && self.south < self.north,
            "civic area has empty bounds"
        );
        Ok(())
    }

    #[must_use]
    pub fn fit_points(self) -> [Coord; 2] {
        [
            Coord::new(self.west, self.south),
            Coord::new(self.east, self.north),
        ]
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Provenance {
    dataset: String,
    vintage: String,
    origin: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CivicSnapshot {
    schema: u32,
    record: CivicRecord,
    bounds: CivicBounds,
    rings: Vec<Vec<Coord>>,
    provenance: Provenance,
}

#[derive(Debug)]
pub struct CivicArea {
    pub record: CivicRecord,
    pub bounds: CivicBounds,
    rings: Vec<PreparedRing>,
}

#[derive(Debug)]
struct PreparedRing {
    coarse: PreparedLevel,
    middle: PreparedLevel,
    fine: PreparedLevel,
}

impl PreparedRing {
    fn level(&self, zoom: f64) -> &PreparedLevel {
        if zoom < 12.5 {
            &self.coarse
        } else if zoom < 15.5 {
            &self.middle
        } else {
            &self.fine
        }
    }
}

#[derive(Debug)]
struct PreparedLevel {
    chunks: Vec<LineChunk>,
}

#[derive(Debug)]
struct LineChunk {
    samples: Arc<[Sample]>,
    bounds: WorldBounds,
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    world: [f64; 2],
    arc: f64,
}

#[derive(Clone, Copy, Debug)]
struct WorldBounds {
    west: f64,
    north: f64,
    east: f64,
    south: f64,
}

impl WorldBounds {
    const EMPTY: Self = Self {
        west: f64::INFINITY,
        north: f64::INFINITY,
        east: f64::NEG_INFINITY,
        south: f64::NEG_INFINITY,
    };

    fn admit(&mut self, point: [f64; 2]) {
        self.west = self.west.min(point[0]);
        self.north = self.north.min(point[1]);
        self.east = self.east.max(point[0]);
        self.south = self.south.max(point[1]);
    }

    fn intersects(self, other: Self) -> bool {
        self.west <= other.east
            && other.west <= self.east
            && self.north <= other.south
            && other.north <= self.south
    }
}

pub enum CivicRowState {
    Preparing,
    Ready(Arc<CivicArea>),
    Fault(String),
}

pub struct CivicRow {
    pub record: CivicRecord,
    pub state: CivicRowState,
    generation: u64,
}

pub struct CivicAreas {
    rows: Vec<CivicRow>,
    query: String,
    suggestions: Vec<CivicRecord>,
    suggestion_pick: usize,
    lookup_serial: u64,
    generation: u64,
    catalog: CatalogForge,
    forge: CivicForge,
}

impl CivicAreas {
    pub fn raise(ctx: &egui::Context, root: &Path, offline: bool) -> Result<Self> {
        let records = read_index(root)?;
        let catalog = CatalogForge::spawn(ctx.clone())?;
        let forge = CivicForge::spawn(ctx.clone(), root.to_owned(), offline)?;
        let mut generation = 0_u64;
        let mut rows = Vec::with_capacity(records.len());
        for record in records {
            generation = generation.saturating_add(1);
            forge.prepare(generation, record.clone());
            rows.push(CivicRow {
                record,
                state: CivicRowState::Preparing,
                generation,
            });
        }
        Ok(Self {
            rows,
            query: String::new(),
            suggestions: Vec::new(),
            suggestion_pick: 0,
            lookup_serial: 0,
            generation,
            catalog,
            forge,
        })
    }

    pub fn pulse(&mut self) -> Option<String> {
        while let Ok(event) = self.catalog.events.try_recv() {
            let CompletionEvent::Ready {
                serial,
                suggestions,
            } = event;
            if serial == self.lookup_serial {
                self.suggestions = suggestions;
                self.suggestion_pick = self
                    .suggestion_pick
                    .min(self.suggestions.len().saturating_sub(1));
            }
        }
        let mut notice = None;
        while let Ok(event) = self.forge.events.try_recv() {
            match event {
                ForgeEvent::Ready { generation, area } => {
                    if let Some(row) = self.rows.iter_mut().find(|row| {
                        row.generation == generation && row.record.key == area.record.key
                    }) {
                        area.record.clone_into(&mut row.record);
                        let name = area.record.name.clone();
                        row.state = CivicRowState::Ready(area);
                        notice = Some(format!("{name} boundary ready."));
                    }
                }
                ForgeEvent::Fault {
                    generation,
                    key,
                    fault,
                } => {
                    if let Some(row) = self
                        .rows
                        .iter_mut()
                        .find(|row| row.generation == generation && row.record.key == key)
                    {
                        let name = row.record.name.clone();
                        row.state = CivicRowState::Fault(fault);
                        notice = Some(format!("Could not prepare {name}."));
                    }
                }
            }
        }
        notice
    }

    pub fn query_mut(&mut self) -> &mut String {
        &mut self.query
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn suggestions(&self) -> &[CivicRecord] {
        &self.suggestions
    }

    pub const fn suggestion_pick(&self) -> usize {
        self.suggestion_pick
    }

    pub fn set_suggestion_pick(&mut self, slot: usize) {
        self.suggestion_pick = slot.min(self.suggestions.len().saturating_sub(1));
    }

    pub fn cycle_suggestion(&mut self, backwards: bool) {
        if self.suggestions.is_empty() {
            return;
        }
        self.suggestion_pick = if backwards {
            self.suggestion_pick
                .checked_sub(1)
                .unwrap_or(self.suggestions.len() - 1)
        } else {
            (self.suggestion_pick + 1) % self.suggestions.len()
        };
    }

    pub fn selected_suggestion(&self) -> Option<CivicRecord> {
        self.suggestions.get(self.suggestion_pick).cloned()
    }

    pub fn lookup(&mut self, project_anchor: Coord) {
        let query = normalize(&self.query);
        if query.chars().filter(|char| char.is_alphanumeric()).count() < 2 {
            self.lookup_serial = self.lookup_serial.saturating_add(1);
            self.suggestions.clear();
            self.suggestion_pick = 0;
            return;
        }
        self.lookup_serial = self.lookup_serial.saturating_add(1);
        self.suggestion_pick = 0;
        self.catalog
            .lookup(self.lookup_serial, query, project_anchor);
    }

    pub fn add(&mut self, record: CivicRecord) -> AddOutcome {
        if let Some(slot) = self
            .rows
            .iter()
            .position(|row| row.record.key == record.key)
        {
            self.query.clear();
            self.suggestions.clear();
            return AddOutcome::Existing(slot);
        }
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        self.rows.push(CivicRow {
            record: record.clone(),
            state: CivicRowState::Preparing,
            generation,
        });
        self.forge.persist(self.records());
        self.forge.prepare(generation, record);
        self.query.clear();
        self.suggestions.clear();
        AddOutcome::Added(self.rows.len() - 1)
    }

    pub fn retry(&mut self, slot: usize) {
        let Some(row) = self.rows.get_mut(slot) else {
            return;
        };
        self.generation = self.generation.saturating_add(1);
        row.generation = self.generation;
        row.state = CivicRowState::Preparing;
        self.forge.prepare(row.generation, row.record.clone());
    }

    pub fn remove(&mut self, slot: usize) -> Option<CivicRecord> {
        if slot >= self.rows.len() {
            return None;
        }
        let row = self.rows.remove(slot);
        self.forge.persist(self.records());
        self.forge.excise(row.record.key.clone());
        Some(row.record)
    }

    pub fn rows(&self) -> &[CivicRow] {
        &self.rows
    }

    pub fn ready(&self) -> impl Iterator<Item = &Arc<CivicArea>> {
        self.rows.iter().filter_map(|row| match &row.state {
            CivicRowState::Ready(area) => Some(area),
            CivicRowState::Preparing | CivicRowState::Fault(_) => None,
        })
    }

    pub fn area(&self, key: &CivicKey) -> Option<&Arc<CivicArea>> {
        self.rows.iter().find_map(|row| match &row.state {
            CivicRowState::Ready(area) if &area.record.key == key => Some(area),
            CivicRowState::Ready(_) | CivicRowState::Preparing | CivicRowState::Fault(_) => None,
        })
    }

    fn records(&self) -> Vec<CivicRecord> {
        self.rows.iter().map(|row| row.record.clone()).collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddOutcome {
    Added(usize),
    Existing(usize),
}

struct CatalogForge {
    commands: Sender<CompletionCommand>,
    events: Receiver<CompletionEvent>,
    _thread: thread::JoinHandle<()>,
}

struct CompletionCommand {
    serial: u64,
    query: String,
    anchor: Coord,
}

enum CompletionEvent {
    Ready {
        serial: u64,
        suggestions: Vec<CivicRecord>,
    },
}

impl CatalogForge {
    fn spawn(ctx: egui::Context) -> Result<Self> {
        let (commands, jobs) = unbounded::<CompletionCommand>();
        let (publish, events) = unbounded();
        let thread = thread::Builder::new()
            .name("civic-catalog".to_owned())
            .spawn(move || {
                let catalog = Catalog::decode().expect("bundled civic catalog must decode");
                while let Ok(job) = jobs.recv() {
                    let suggestions = catalog.search(&job.query, job.anchor);
                    if publish
                        .send(CompletionEvent::Ready {
                            serial: job.serial,
                            suggestions,
                        })
                        .is_err()
                    {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .context("spawn civic catalog worker")?;
        Ok(Self {
            commands,
            events,
            _thread: thread,
        })
    }

    fn lookup(&self, serial: u64, query: String, anchor: Coord) {
        let _sent = self.commands.send(CompletionCommand {
            serial,
            query,
            anchor,
        });
    }
}

struct CivicForge {
    commands: Sender<ForgeCommand>,
    events: Receiver<ForgeEvent>,
    _thread: thread::JoinHandle<()>,
}

enum ForgeCommand {
    Prepare {
        generation: u64,
        record: CivicRecord,
    },
    Persist(Vec<CivicRecord>),
    Excise(CivicKey),
}

enum ForgeEvent {
    Ready {
        generation: u64,
        area: Arc<CivicArea>,
    },
    Fault {
        generation: u64,
        key: CivicKey,
        fault: String,
    },
}

impl CivicForge {
    fn spawn(ctx: egui::Context, root: PathBuf, offline: bool) -> Result<Self> {
        let (commands, jobs) = unbounded();
        let (publish, events) = unbounded();
        let thread = thread::Builder::new()
            .name("civic-boundary-forge".to_owned())
            .spawn(move || {
                while let Ok(command) = jobs.recv() {
                    match command {
                        ForgeCommand::Prepare { generation, record } => {
                            let key = record.key.clone();
                            let result = prepare_record(&root, record, offline);
                            let event = match result {
                                Ok(area) => ForgeEvent::Ready {
                                    generation,
                                    area: Arc::new(area),
                                },
                                Err(error) => ForgeEvent::Fault {
                                    generation,
                                    key,
                                    fault: format!("{error:#}"),
                                },
                            };
                            if publish.send(event).is_err() {
                                break;
                            }
                            ctx.request_repaint();
                        }
                        ForgeCommand::Persist(records) => {
                            if let Err(error) = write_index(&root, &records) {
                                tracing::error!(error = %error, "persist civic index");
                            }
                        }
                        ForgeCommand::Excise(key) => {
                            let path = snapshot_path(&root, &key);
                            if let Err(error) = fs::remove_file(&path)
                                && error.kind() != std::io::ErrorKind::NotFound
                            {
                                tracing::error!(error = %error, path = %path.display(), "remove civic snapshot");
                            }
                        }
                    }
                }
            })
            .context("spawn civic boundary worker")?;
        Ok(Self {
            commands,
            events,
            _thread: thread,
        })
    }

    fn prepare(&self, generation: u64, record: CivicRecord) {
        let _sent = self
            .commands
            .send(ForgeCommand::Prepare { generation, record });
    }

    fn persist(&self, records: Vec<CivicRecord>) {
        let _sent = self.commands.send(ForgeCommand::Persist(records));
    }

    fn excise(&self, key: CivicKey) {
        let _sent = self.commands.send(ForgeCommand::Excise(key));
    }
}

#[derive(Default)]
struct Catalog {
    entries: Vec<CatalogEntry>,
}

struct CatalogEntry {
    record: CivicRecord,
    name: String,
    haystack: String,
}

impl Catalog {
    fn decode() -> Result<Self> {
        let bytes = zstd::stream::decode_all(Cursor::new(CATALOG))
            .context("decode bundled civic catalog")?;
        let text = std::str::from_utf8(&bytes).context("civic catalog is not UTF-8")?;
        let mut records = nyc_boroughs().to_vec();
        for (line, row) in text.lines().enumerate() {
            let fields = row.split('|').collect::<Vec<_>>();
            ensure!(
                fields.len() == 7,
                "civic catalog row {} has {} fields",
                line + 1,
                fields.len()
            );
            let source = match fields[2] {
                "inc" => CivicSource::CensusIncorporated,
                "cdp" => CivicSource::CensusDesignated,
                other => bail!("civic catalog has unknown source {other}"),
            };
            records.push(CivicRecord {
                key: CivicKey {
                    source,
                    geoid: fields[1].to_owned(),
                },
                name: fields[3].to_owned(),
                kind: fields[4].to_owned(),
                jurisdiction: fields[0].to_owned(),
                anchor: Coord::new(
                    fields[6]
                        .parse()
                        .with_context(|| format!("catalog longitude on row {}", line + 1))?,
                    fields[5]
                        .parse()
                        .with_context(|| format!("catalog latitude on row {}", line + 1))?,
                ),
            });
        }
        let entries = records
            .into_iter()
            .map(|record| {
                let name = normalize(&record.name);
                let haystack = normalize(&format!(
                    "{} {} {}",
                    record.name, record.jurisdiction, record.kind
                ));
                CatalogEntry {
                    record,
                    name,
                    haystack,
                }
            })
            .collect();
        Ok(Self { entries })
    }

    fn search(&self, query: &str, anchor: Coord) -> Vec<CivicRecord> {
        let words = query.split_whitespace().collect::<Vec<_>>();
        let mut hits = self
            .entries
            .iter()
            .filter_map(|entry| {
                let rank = if entry.name == query {
                    0
                } else if entry.name.starts_with(query) {
                    1
                } else if words.iter().all(|word| {
                    entry
                        .haystack
                        .split_whitespace()
                        .any(|token| token.starts_with(word))
                }) {
                    2
                } else if entry.haystack.contains(query) {
                    3
                } else {
                    return None;
                };
                Some((rank, entry.record.anchor.haversine_m(anchor), &entry.record))
            })
            .collect::<Vec<_>>();
        hits.sort_unstable_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.total_cmp(&right.1))
                .then_with(|| left.2.name.cmp(&right.2.name))
                .then_with(|| left.2.jurisdiction.cmp(&right.2.jurisdiction))
                .then_with(|| left.2.kind.cmp(&right.2.kind))
        });
        hits.into_iter()
            .take(COMPLETION_LIMIT)
            .map(|(_, _, record)| record.clone())
            .collect()
    }
}

fn nyc_boroughs() -> [CivicRecord; 5] {
    [
        ("1", "Manhattan", 40.7831, -73.9712),
        ("2", "Bronx", 40.8448, -73.8648),
        ("3", "Brooklyn", 40.6782, -73.9442),
        ("4", "Queens", 40.7282, -73.7949),
        ("5", "Staten Island", 40.5795, -74.1502),
    ]
    .map(|(geoid, name, lat, lon)| CivicRecord {
        key: CivicKey {
            source: CivicSource::NycBorough,
            geoid: geoid.to_owned(),
        },
        name: name.to_owned(),
        kind: "borough".to_owned(),
        jurisdiction: "NY".to_owned(),
        anchor: Coord::new(lon, lat),
    })
}

fn normalize(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut separated = true;
    for char in text.chars().flat_map(char::to_lowercase) {
        if char.is_alphanumeric() {
            normalized.push(char);
            separated = false;
        } else if !separated {
            normalized.push(' ');
            separated = true;
        }
    }
    if separated {
        let _trailing_separator = normalized.pop();
    }
    normalized
}

fn read_index(root: &Path) -> Result<Vec<CivicRecord>> {
    let path = root.join(INDEX);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let index: CivicIndex =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    ensure!(index.schema == SCHEMA, "unsupported civic-area schema");
    let mut seen = HashSet::new();
    ensure!(
        index
            .areas
            .iter()
            .all(|record| seen.insert(record.key.clone())),
        "civic-area index contains duplicate identities"
    );
    Ok(index.areas)
}

fn write_index(root: &Path, records: &[CivicRecord]) -> Result<()> {
    let index = CivicIndex {
        schema: SCHEMA,
        areas: records.to_vec(),
    };
    write_atomic(
        &root.join(INDEX),
        &serde_json::to_vec_pretty(&index).context("serialize civic index")?,
    )
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CivicIndex {
    schema: u32,
    areas: Vec<CivicRecord>,
}

fn snapshot_path(root: &Path, key: &CivicKey) -> PathBuf {
    root.join(SHAPES).join(format!("{}.json.zst", key.slug()))
}

fn prepare_record(root: &Path, record: CivicRecord, offline: bool) -> Result<CivicArea> {
    let key = record.key.clone();
    let path = snapshot_path(root, &record.key);
    let snapshot = match fs::read(&path) {
        Ok(bytes) => {
            decode_snapshot(&bytes).with_context(|| format!("decode {}", path.display()))?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !offline => {
            let snapshot = acquire(record)?;
            let encoded = encode_snapshot(&snapshot)?;
            write_atomic(&path, &encoded)?;
            snapshot
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("boundary is not cached; reconnect and retry")
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    ensure!(
        snapshot.schema == SCHEMA,
        "unsupported civic snapshot schema"
    );
    ensure!(
        snapshot.record.key == key,
        "civic snapshot identity mismatch"
    );
    CivicArea::prepare(snapshot)
}

fn acquire(record: CivicRecord) -> Result<CivicSnapshot> {
    let (endpoint, query, dataset, vintage) = match record.key.source {
        CivicSource::CensusIncorporated | CivicSource::CensusDesignated => {
            let root = std::env::var("TRAILGEN_CIVIC_CENSUS_ENDPOINT")
                .unwrap_or_else(|_| CENSUS_ENDPOINT.to_owned());
            let layer = if record.key.source == CivicSource::CensusIncorporated {
                28
            } else {
                30
            };
            (
                format!("{root}/{layer}/query"),
                format!("GEOID='{}'", record.key.geoid),
                "US Census TIGERweb".to_owned(),
                "2025".to_owned(),
            )
        }
        CivicSource::NycBorough => {
            let root = std::env::var("TRAILGEN_CIVIC_NYC_ENDPOINT")
                .unwrap_or_else(|_| NYC_ENDPOINT.to_owned());
            (
                format!("{root}/query"),
                format!("BoroName='{}'", record.name.replace('\'', "''")),
                "NYC DCP Borough Boundaries".to_owned(),
                "26B".to_owned(),
            )
        }
    };
    let client = trailgen_data::provider_client("civic-boundary", Duration::from_secs(45))?;
    let response = client
        .get(&endpoint)
        .query(&[
            ("where", query.as_str()),
            ("outFields", "*"),
            ("returnGeometry", "true"),
            ("outSR", "4326"),
            ("f", "geojson"),
        ])
        .send()
        .with_context(|| format!("request {} boundary", record.name))?
        .error_for_status()
        .with_context(|| format!("provider rejected {} boundary", record.name))?;
    let bytes = response
        .bytes()
        .with_context(|| format!("read {} boundary", record.name))?;
    snapshot_from_geojson(
        record,
        &bytes,
        Provenance {
            dataset,
            vintage,
            origin: endpoint,
        },
    )
}

fn snapshot_from_geojson(
    record: CivicRecord,
    bytes: &[u8],
    provenance: Provenance,
) -> Result<CivicSnapshot> {
    let root: serde_json::Value = serde_json::from_slice(bytes).context("parse civic GeoJSON")?;
    let features = root
        .get("features")
        .and_then(serde_json::Value::as_array)
        .context("civic GeoJSON lacks features")?;
    ensure!(
        features.len() == 1,
        "civic provider returned {} features",
        features.len()
    );
    let geometry = features[0]
        .get("geometry")
        .context("civic feature lacks geometry")?;
    let coordinates = geometry
        .get("coordinates")
        .context("civic geometry lacks coordinates")?;
    let mut rings = Vec::new();
    match geometry.get("type").and_then(serde_json::Value::as_str) {
        Some("Polygon") => parse_polygon(coordinates, &mut rings)?,
        Some("MultiPolygon") => {
            for polygon in coordinates
                .as_array()
                .context("civic multipolygon coordinates are not an array")?
            {
                parse_polygon(polygon, &mut rings)?;
            }
        }
        kind => bail!("unsupported civic geometry {kind:?}"),
    }
    ensure!(!rings.is_empty(), "civic geometry contains no lawful rings");
    let mut bounds = CivicBounds::EMPTY;
    for coord in rings.iter().flatten().copied() {
        bounds.admit(coord);
    }
    bounds.validate()?;
    Ok(CivicSnapshot {
        schema: SCHEMA,
        record,
        bounds,
        rings,
        provenance,
    })
}

fn parse_polygon(value: &serde_json::Value, rings: &mut Vec<Vec<Coord>>) -> Result<()> {
    for raw_ring in value
        .as_array()
        .context("civic polygon coordinates are not an array")?
    {
        let mut ring = raw_ring
            .as_array()
            .context("civic ring is not an array")?
            .iter()
            .map(|raw| {
                let pair = raw.as_array().context("civic coordinate is not an array")?;
                ensure!(pair.len() >= 2, "civic coordinate has fewer than two axes");
                let lon = pair[0].as_f64().context("civic longitude is not numeric")?;
                let lat = pair[1].as_f64().context("civic latitude is not numeric")?;
                ensure!(
                    lon.is_finite()
                        && lat.is_finite()
                        && (-180.0..=180.0).contains(&lon)
                        && (-85.051_128_78..=85.051_128_78).contains(&lat),
                    "civic coordinate is outside Web Mercator"
                );
                Ok(Coord::new(lon, lat))
            })
            .collect::<Result<Vec<_>>>()?;
        ring.dedup_by(|left, right| {
            left.lon.to_bits() == right.lon.to_bits() && left.lat.to_bits() == right.lat.to_bits()
        });
        if ring.len() >= 2
            && ring.first().zip(ring.last()).is_some_and(|(first, last)| {
                first.lon.to_bits() == last.lon.to_bits()
                    && first.lat.to_bits() == last.lat.to_bits()
            })
        {
            let _closing_duplicate = ring.pop();
        }
        if ring.len() >= 3 {
            rings.push(ring);
        }
    }
    Ok(())
}

fn encode_snapshot(snapshot: &CivicSnapshot) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(snapshot).context("serialize civic snapshot")?;
    zstd::stream::encode_all(Cursor::new(json), 9).context("compress civic snapshot")
}

fn decode_snapshot(bytes: &[u8]) -> Result<CivicSnapshot> {
    let json = zstd::stream::decode_all(Cursor::new(bytes)).context("decompress civic snapshot")?;
    serde_json::from_slice(&json).context("parse civic snapshot")
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("civic path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let temporary = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", temporary.display()))?;
    }
    fs::rename(&temporary, path)
        .with_context(|| format!("replace {} with {}", temporary.display(), path.display()))
}

impl CivicArea {
    fn prepare(snapshot: CivicSnapshot) -> Result<Self> {
        let rings = snapshot
            .rings
            .iter()
            .map(|ring| prepare_ring(ring))
            .collect::<Result<Vec<_>>>()?;
        ensure!(!rings.is_empty(), "civic snapshot has no renderable rings");
        Ok(Self {
            record: snapshot.record,
            bounds: snapshot.bounds,
            rings,
        })
    }
}

fn prepare_ring(coords: &[Coord]) -> Result<PreparedRing> {
    ensure!(coords.len() >= 3, "civic ring has fewer than three points");
    let worlds = coords
        .iter()
        .copied()
        .map(map::world_from_coord)
        .collect::<Vec<_>>();
    let mut total = 0.0;
    let mut fine = Vec::with_capacity(worlds.len());
    for (slot, world) in worlds.iter().copied().enumerate() {
        if slot > 0 {
            total += distance(worlds[slot - 1], world);
        }
        fine.push(Sample { world, arc: total });
    }
    total += distance(worlds[worlds.len() - 1], worlds[0]);
    ensure!(total.is_normal(), "civic ring has no length");
    let middle = simplify_ring(&fine, 2.5e-8);
    let coarse = simplify_ring(&middle, 3.0e-7);
    Ok(PreparedRing {
        coarse: chunk_ring(&coarse, total),
        middle: chunk_ring(&middle, total),
        fine: chunk_ring(&fine, total),
    })
}

fn simplify_ring(samples: &[Sample], tolerance: f64) -> Vec<Sample> {
    if samples.len() <= 4 {
        return samples.to_vec();
    }
    let (west, _) = samples
        .iter()
        .enumerate()
        .min_by(|left, right| left.1.world[0].total_cmp(&right.1.world[0]))
        .expect("nonempty ring");
    let (east, _) = samples
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.world[0].total_cmp(&right.1.world[0]))
        .expect("nonempty ring");
    if west == east {
        return samples.to_vec();
    }
    let first = cyclic_arc(samples, west, east);
    let second = cyclic_arc(samples, east, west);
    let mut simplified = simplify_open(&first, tolerance);
    let other = simplify_open(&second, tolerance);
    let _shared_end = simplified.pop();
    simplified.extend(other);
    let _shared_start = simplified.pop();
    let retained = simplified
        .into_iter()
        .map(|sample| sample.arc.to_bits())
        .collect::<HashSet<_>>();
    samples
        .iter()
        .copied()
        .filter(|sample| retained.contains(&sample.arc.to_bits()))
        .collect()
}

fn cyclic_arc(samples: &[Sample], start: usize, end: usize) -> Vec<Sample> {
    let mut arc = Vec::new();
    let mut slot = start;
    loop {
        arc.push(samples[slot]);
        if slot == end {
            break;
        }
        slot = (slot + 1) % samples.len();
    }
    arc
}

fn simplify_open(samples: &[Sample], tolerance: f64) -> Vec<Sample> {
    if samples.len() <= 2 {
        return samples.to_vec();
    }
    let tolerance2 = tolerance * tolerance;
    let mut keep = vec![false; samples.len()];
    keep[0] = true;
    keep[samples.len() - 1] = true;
    let mut stack = vec![(0, samples.len() - 1)];
    while let Some((start, end)) = stack.pop() {
        let mut farthest = None;
        for slot in start + 1..end {
            let distance2 = segment_distance2(
                samples[slot].world,
                samples[start].world,
                samples[end].world,
            );
            if farthest.is_none_or(|(_, prior)| distance2 > prior) {
                farthest = Some((slot, distance2));
            }
        }
        if let Some((slot, distance2)) = farthest
            && distance2 > tolerance2
        {
            keep[slot] = true;
            stack.push((start, slot));
            stack.push((slot, end));
        }
    }
    samples
        .iter()
        .copied()
        .zip(keep)
        .filter_map(|(sample, keep)| keep.then_some(sample))
        .collect()
}

fn segment_distance2(point: [f64; 2], start: [f64; 2], end: [f64; 2]) -> f64 {
    let course = [end[0] - start[0], end[1] - start[1]];
    let length2 = course[0].mul_add(course[0], course[1] * course[1]);
    if length2 <= f64::EPSILON {
        return distance2(point, start);
    }
    let offset = [point[0] - start[0], point[1] - start[1]];
    let t = (offset[0].mul_add(course[0], offset[1] * course[1]) / length2).clamp(0.0, 1.0);
    let projection = [
        course[0].mul_add(t, start[0]),
        course[1].mul_add(t, start[1]),
    ];
    distance2(point, projection)
}

fn chunk_ring(samples: &[Sample], total: f64) -> PreparedLevel {
    let mut closed = Vec::with_capacity(samples.len() + 1);
    closed.extend_from_slice(samples);
    closed.push(Sample {
        world: samples[0].world,
        arc: total,
    });
    let mut chunks = Vec::new();
    let mut start = 0;
    while start + 1 < closed.len() {
        let end = (start + CHUNK_POINTS).min(closed.len() - 1);
        let samples: Arc<[Sample]> = closed[start..=end].into();
        let mut bounds = WorldBounds::EMPTY;
        for sample in samples.iter() {
            bounds.admit(sample.world);
        }
        chunks.push(LineChunk { samples, bounds });
        start = end;
    }
    PreparedLevel { chunks }
}

fn distance(left: [f64; 2], right: [f64; 2]) -> f64 {
    distance2(left, right).sqrt()
}

fn distance2(left: [f64; 2], right: [f64; 2]) -> f64 {
    let dx = left[0] - right[0];
    let dy = left[1] - right[1];
    dx.mul_add(dx, dy * dy)
}

#[derive(Clone)]
pub struct CivicLabel {
    name: String,
    anchor: Pos2,
    angle: f32,
}

pub fn paint_boundaries(
    painter: &Painter,
    frame: map::MapFramePlan,
    settled_zoom: f64,
    areas: impl IntoIterator<Item = Arc<CivicArea>>,
) -> Vec<CivicLabel> {
    let visible = {
        let [west, north, east, south] = frame.world_bounds();
        WorldBounds {
            west,
            north,
            east,
            south,
        }
    };
    let hatch_spacing = hatch_spacing(256.0 * settled_zoom.exp2());
    let mut labels = Vec::new();
    for area in areas {
        let mut shapes = Vec::new();
        let mut label = None;
        for ring in &area.rings {
            for chunk in &ring.level(settled_zoom).chunks {
                if !chunk.bounds.intersects(visible) {
                    continue;
                }
                let points = chunk
                    .samples
                    .iter()
                    .map(|sample| map::screen_at(frame.viewport, frame.rect, sample.world))
                    .collect::<Vec<_>>();
                shapes.push(Shape::line(
                    points.clone(),
                    Stroke::new(3.0_f32, Color32::from_black_alpha(128)),
                ));
                shapes.push(Shape::line(
                    points,
                    Stroke::new(1.25_f32, MAGENTA.gamma_multiply(0.92)),
                ));
                paint_hatches(&mut shapes, frame, chunk, hatch_spacing);
                best_label(&mut label, frame.rect, frame, chunk);
            }
        }
        painter.extend(shapes);
        if let Some((_, anchor, angle)) = label {
            labels.push(CivicLabel {
                name: area.record.name.to_ascii_uppercase(),
                anchor,
                angle,
            });
        }
    }
    labels
}

pub fn paint_labels(painter: &Painter, labels: &[CivicLabel]) {
    for label in labels {
        let galley = painter.layout_no_wrap(
            label.name.clone(),
            FontId::monospace(13.0),
            Color32::PLACEHOLDER,
        );
        let plate = Rect::from_center_size(label.anchor, galley.size() + vec2(8.0, 4.0));
        painter.rect_filled(plate, 1.0, chrome::SURFACE.gamma_multiply(0.88));
        let _ink = painter.add(
            TextShape::new(
                label.anchor - galley.rect.center().to_vec2(),
                galley,
                MAGENTA_INK,
            )
            .with_angle_and_anchor(label.angle, Align2::CENTER_CENTER),
        );
    }
}

fn hatch_spacing(world_points: f64) -> f64 {
    let desired = 13.0 / world_points;
    2.0_f64.powf(desired.log2().ceil())
}

fn paint_hatches(
    shapes: &mut Vec<Shape>,
    frame: map::MapFramePlan,
    chunk: &LineChunk,
    spacing: f64,
) {
    for pair in chunk.samples.windows(2) {
        let [start, end] = [pair[0], pair[1]];
        if end.arc <= start.arc {
            continue;
        }
        let a = map::screen_at(frame.viewport, frame.rect, start.world);
        let b = map::screen_at(frame.viewport, frame.rect, end.world);
        let Some((low, high)) = clip_parameters(frame.rect.expand(4.0), a, b) else {
            continue;
        };
        let visible_start = (end.arc - start.arc).mul_add(f64::from(low), start.arc);
        let visible_end = (end.arc - start.arc).mul_add(f64::from(high), start.arc);
        let first = (visible_start / spacing).ceil() as i64;
        let last = (visible_end / spacing).floor() as i64;
        if last < first {
            continue;
        }
        let tangent = (b - a).normalized();
        if tangent == Vec2::ZERO {
            continue;
        }
        let normal = vec2(-tangent.y, tangent.x);
        let axis = (tangent + normal).normalized() * 3.4;
        for mark in first..=last {
            let arc = mark as f64 * spacing;
            let t = ((arc - start.arc) / (end.arc - start.arc)).clamp(0.0, 1.0) as f32;
            let center = a.lerp(b, t);
            if frame.rect.expand(4.0).contains(center) {
                shapes.push(Shape::line_segment(
                    [center - axis, center + axis],
                    Stroke::new(1.0_f32, MAGENTA),
                ));
            }
        }
    }
}

fn best_label(
    best: &mut Option<(f32, Pos2, f32)>,
    canvas: Rect,
    frame: map::MapFramePlan,
    chunk: &LineChunk,
) {
    for pair in chunk.samples.windows(2) {
        let a = map::screen_at(frame.viewport, frame.rect, pair[0].world);
        let b = map::screen_at(frame.viewport, frame.rect, pair[1].world);
        let Some((a, b)) = clip_segment(canvas, a, b) else {
            continue;
        };
        let course = b - a;
        let length = course.length();
        if length <= f32::EPSILON {
            continue;
        }
        let anchor = a.lerp(b, 0.5);
        let centrality = 1.0 / (1.0 + anchor.distance(canvas.center()) * 0.01);
        let score = length * centrality;
        if best.as_ref().is_none_or(|prior| score > prior.0) {
            let mut angle = course.angle();
            if angle > std::f32::consts::FRAC_PI_2 {
                angle -= std::f32::consts::PI;
            } else if angle < -std::f32::consts::FRAC_PI_2 {
                angle += std::f32::consts::PI;
            }
            *best = Some((score, anchor, angle));
        }
    }
}

fn clip_segment(rect: Rect, mut a: Pos2, mut b: Pos2) -> Option<(Pos2, Pos2)> {
    let (low, high) = clip_parameters(rect, a, b)?;
    let course = b - a;
    b = a + course * high;
    a += course * low;
    Some((a, b))
}

fn clip_parameters(rect: Rect, a: Pos2, b: Pos2) -> Option<(f32, f32)> {
    let course = b - a;
    let mut low = 0.0_f32;
    let mut high = 1.0_f32;
    for (p, q) in [
        (-course.x, a.x - rect.left()),
        (course.x, rect.right() - a.x),
        (-course.y, a.y - rect.top()),
        (course.y, rect.bottom() - a.y),
    ] {
        if p.abs() <= f32::EPSILON {
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let t = q / p;
        if p < 0.0 {
            low = low.max(t);
        } else {
            high = high.min(t);
        }
        if low > high {
            return None;
        }
    }
    Some((low, high))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brooklyn() -> CivicRecord {
        nyc_boroughs()[2].clone()
    }

    #[test]
    fn catalog_prefers_brooklyn_to_distant_prefixes() -> Result<()> {
        let catalog = Catalog::decode()?;
        let hits = catalog.search("bro", brooklyn().anchor);
        assert_eq!(hits.first().map(|hit| hit.name.as_str()), Some("Brooklyn"));
        assert_eq!(hits.first().map(|hit| hit.kind.as_str()), Some("borough"));
        Ok(())
    }

    #[test]
    fn geojson_normalizes_polygon_and_multipolygon_rings() -> Result<()> {
        let source = serde_json::json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {
                    "type": "MultiPolygon",
                    "coordinates": [
                        [[[-74.0, 40.0], [-73.9, 40.0], [-73.9, 40.1], [-74.0, 40.0]]],
                        [[[-73.8, 40.0], [-73.7, 40.0], [-73.7, 40.1], [-73.8, 40.0]]]
                    ]
                },
                "properties": {}
            }]
        });
        let snapshot = snapshot_from_geojson(
            brooklyn(),
            &serde_json::to_vec(&source)?,
            Provenance {
                dataset: "fixture".to_owned(),
                vintage: "1".to_owned(),
                origin: "fixture".to_owned(),
            },
        )?;
        assert_eq!(snapshot.rings.len(), 2);
        assert!(snapshot.rings.iter().all(|ring| ring.len() == 3));
        snapshot.bounds.validate()
    }

    #[test]
    fn simplification_is_nested_and_retains_original_phase() {
        let fine = (0..100)
            .map(|slot| Sample {
                world: [f64::from(slot) / 100.0, f64::from(slot % 7) / 10_000.0],
                arc: f64::from(slot),
            })
            .collect::<Vec<_>>();
        let middle = simplify_ring(&fine, 5.0e-5);
        let coarse = simplify_ring(&middle, 2.0e-4);
        let fine_arcs = fine
            .iter()
            .map(|sample| sample.arc.to_bits())
            .collect::<HashSet<_>>();
        let middle_arcs = middle
            .iter()
            .map(|sample| sample.arc.to_bits())
            .collect::<HashSet<_>>();
        assert!(
            middle
                .iter()
                .all(|sample| fine_arcs.contains(&sample.arc.to_bits()))
        );
        assert!(
            coarse
                .iter()
                .all(|sample| middle_arcs.contains(&sample.arc.to_bits()))
        );
    }

    #[test]
    fn project_index_and_snapshot_round_trip() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let record = brooklyn();
        write_index(temp.path(), std::slice::from_ref(&record))?;
        assert_eq!(read_index(temp.path())?, vec![record.clone()]);
        let source = serde_json::json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {"type": "Polygon", "coordinates": [[
                    [-74.0, 40.0], [-73.9, 40.0], [-73.9, 40.1], [-74.0, 40.0]
                ]]},
                "properties": {}
            }]
        });
        let snapshot = snapshot_from_geojson(
            record,
            &serde_json::to_vec(&source)?,
            Provenance {
                dataset: "fixture".to_owned(),
                vintage: "1".to_owned(),
                origin: "fixture".to_owned(),
            },
        )?;
        assert_eq!(decode_snapshot(&encode_snapshot(&snapshot)?)?, snapshot);
        Ok(())
    }
}
