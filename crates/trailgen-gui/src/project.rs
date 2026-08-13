use crate::{
    library::{Library, SearchBoundary},
    portfolio::{CandidatePortfolio, CandidateWarmth},
};
use anyhow::{Context as _, Result, ensure};
use crossbeam_channel::{Receiver, Sender, bounded};
use egui::Context;
use eternalist_apps::NativeWake;
use serde::Deserialize;
use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use trailgen_core::{
    EdgeEdicts, GRAPH_CACHE, LoopConstraints, Route, SearchMonitor, SearchParams, SearchProgress,
    SearchScope, SearchStage, SolverKind, VertexId, WalkGraph, WalkRealmIndex, decode_graph,
};

const SEARCH_EVENT_CAPACITY: usize = 64;

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct WorkbenchConfig {
    pub name: String,
    pub constraints: LoopConstraints,
    pub search: SearchParams,
    pub solver: SolverKind,
}

impl Default for WorkbenchConfig {
    fn default() -> Self {
        Self {
            name: "Untitled trail project".to_owned(),
            constraints: LoopConstraints::default(),
            search: interactive_search(),
            solver: SolverKind::default(),
        }
    }
}

const fn interactive_search() -> SearchParams {
    SearchParams {
        max_hops: 256,
        max_frontier: 5_000,
        keep: 12,
        closure_paths: 1,
        seed: 2,
        routing: trailgen_core::RoutingLaw { road_aversion: 2.0 },
    }
}

pub struct Project {
    pub root: PathBuf,
    pub config: WorkbenchConfig,
    pub library: Library,
}

impl Project {
    pub fn open(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("open project {}", root.display()))?;
        let config: WorkbenchConfig = product_phase!(
            "project.read_config",
            read_toml(&root.join("trailgen.toml"))?
        );
        let library = product_phase!(
            "project.load_library",
            Library::open(&root, &config.constraints)?
        );
        Ok(Self {
            root,
            config,
            library,
        })
    }

    /// Load only the immutable routing corpus. Corpus replacement uses this
    /// boundary off the event loop before forging its spatial projections.
    pub fn load_graph(root: &Path) -> Result<Arc<WalkGraph>> {
        let graph = Arc::new(match read_optional_graph(&root.join(GRAPH_CACHE))? {
            Some(graph) => graph,
            None => read_optional_json::<WalkGraph>(&root.join("routes/generated.graph.json"))?
                .context("project has no trail data")?,
        });
        ensure!(!graph.vertices.is_empty(), "project has no usable trails");
        Ok(graph)
    }
}

fn read_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn read_optional_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(raw) => serde_json::from_slice(&raw)
            .with_context(|| format!("parse {}", path.display()))
            .map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

fn read_optional_graph(path: &Path) -> Result<Option<WalkGraph>> {
    match fs::read(path) {
        Ok(raw) => product_phase!(
            "project.decode_graph",
            decode_graph(&raw)
                .with_context(|| format!("parse {}", path.display()))
                .map(Some)
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

#[derive(Clone)]
pub struct SearchRequest {
    pub serial: u64,
    pub start: VertexId,
    pub boundary: Option<SearchBoundary>,
    pub constraints: LoopConstraints,
    pub params: SearchParams,
    pub solver: SolverKind,
    pub count: usize,
    pub manual_defaults: LoopConstraints,
    pub edicts: EdgeEdicts,
    pub warmth: CandidateWarmth,
}

impl SearchRequest {
    pub fn validate(&self, graph: &WalkGraph) -> Result<()> {
        ensure!(
            self.start.0 < graph.vertices.len(),
            "trailhead is outside downloaded trail data"
        );
        self.edicts.validate(graph)?;
        if let Some(boundary) = &self.boundary {
            boundary.validate()?;
            ensure!(
                boundary.contains(graph.vertices[self.start.0].coord),
                "trailhead is outside the search area"
            );
            ensure!(
                graph.adjacency[self.start.0]
                    .iter()
                    .any(|edge| boundary.allows_edge(&graph.edges[edge.0])),
                "search area contains no trail leaving this trailhead"
            );
            ensure!(
                self.edicts
                    .required()
                    .all(|edge| boundary.allows_edge(&graph.edges[edge.0])),
                "a required segment lies outside the search area"
            );
        }
        ensure!(
            (1..=32).contains(&self.count),
            "candidate count must be 1–32"
        );
        ensure!(self.params.max_hops > 0, "maximum hops must be positive");
        ensure!(
            self.params.max_frontier > 0,
            "search frontier must be positive"
        );
        ensure!(
            self.params.keep >= self.count,
            "solver keep must cover count"
        );
        ensure!(
            self.params.closure_paths > 0,
            "closure path count must be positive"
        );
        self.params.routing.validate()?;
        validate_constraints(&self.constraints)
    }
}

fn validate_constraints(constraints: &LoopConstraints) -> Result<()> {
    for (name, value) in [
        ("minimum distance", constraints.min_distance_m),
        ("maximum distance", constraints.max_distance_m),
        (
            "minimum lower-limb load",
            constraints.min_lower_limb_load_km,
        ),
        (
            "maximum lower-limb load",
            constraints.max_lower_limb_load_km,
        ),
        ("minimum moving time", constraints.min_moving_time_s),
        ("maximum moving time", constraints.max_moving_time_s),
        ("minimum ascent", constraints.min_ascent_m),
        ("maximum ascent", constraints.max_ascent_m),
        ("minimum descent", constraints.min_descent_m),
        ("maximum descent", constraints.max_descent_m),
    ] {
        ensure!(
            value.is_finite() && value >= 0.0,
            "{name} must be finite and nonnegative"
        );
    }
    ensure!(
        constraints
            .target_lower_limb_load_km
            .is_none_or(|target| target.is_finite() && target >= 0.0),
        "target lower-limb load must be finite and nonnegative"
    );
    for (name, low, high) in [
        (
            "distance",
            constraints.min_distance_m,
            constraints.max_distance_m,
        ),
        (
            "lower-limb load",
            constraints.min_lower_limb_load_km,
            constraints.max_lower_limb_load_km,
        ),
        (
            "moving time",
            constraints.min_moving_time_s,
            constraints.max_moving_time_s,
        ),
        ("ascent", constraints.min_ascent_m, constraints.max_ascent_m),
        (
            "descent",
            constraints.min_descent_m,
            constraints.max_descent_m,
        ),
    ] {
        ensure!(low <= high, "{name} minimum exceeds maximum");
    }
    for (name, fraction) in [
        ("road", constraints.max_road_fraction),
        ("low confidence", constraints.max_low_confidence_fraction),
        (
            "restricted access",
            constraints.max_restricted_access_fraction,
        ),
        ("repeated edge", constraints.max_repeated_edge_fraction),
    ] {
        ensure!(
            fraction.is_finite() && (0.0..=1.0).contains(&fraction),
            "maximum {name} fraction must lie in 0–1"
        );
    }
    for (terrain, fraction) in &constraints.min_terrain_fraction {
        ensure!(
            fraction.is_finite() && (0.0..=1.0).contains(fraction),
            "minimum {terrain:?} fraction must lie in 0–1"
        );
    }
    for (terrain, maximum) in &constraints.max_terrain_fraction {
        ensure!(
            maximum.is_finite() && (0.0..=1.0).contains(maximum),
            "maximum {terrain:?} fraction must lie in 0–1"
        );
        let minimum = constraints
            .min_terrain_fraction
            .get(terrain)
            .copied()
            .unwrap_or_default();
        ensure!(
            minimum <= *maximum,
            "{terrain:?} minimum fraction exceeds maximum"
        );
    }
    ensure!(
        !constraints.allowed_shapes.is_empty(),
        "choose a route shape"
    );
    Ok(())
}

pub enum SearchEvent {
    Progress {
        serial: u64,
        progress: SearchProgress,
    },
    Preview {
        serial: u64,
        portfolio: Box<CandidatePortfolio>,
        elapsed: Duration,
    },
    Found {
        serial: u64,
        portfolio: Box<CandidatePortfolio>,
        elapsed: Duration,
    },
    PreparingResults {
        serial: u64,
        count: usize,
        elapsed: Duration,
    },
    Stopped {
        serial: u64,
        elapsed: Duration,
    },
}

struct SearchJob {
    request: SearchRequest,
    halt: Arc<AtomicBool>,
}

enum BoundaryMask {
    All,
    Edges(Vec<bool>),
    Halted,
}

#[derive(Debug)]
pub struct SearchHandle(Arc<AtomicBool>);

impl SearchHandle {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Drop for SearchHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

struct ForgeMonitor<'a> {
    serial: u64,
    graph: &'a WalkGraph,
    halt: Arc<AtomicBool>,
    events: Sender<SearchEvent>,
    wake: NativeWake,
    routing: trailgen_core::RoutingLaw,
    manual_defaults: LoopConstraints,
    warmth: CandidateWarmth,
    started: Instant,
    previewed: Cell<bool>,
}

impl SearchMonitor for ForgeMonitor<'_> {
    fn cancelled(&self) -> bool {
        self.halt.load(Ordering::Acquire)
    }

    fn report(&self, progress: SearchProgress) {
        let _event = self.events.send(SearchEvent::Progress {
            serial: self.serial,
            progress,
        });
        let _woken = self.wake.request_foreground_repaint();
    }

    fn preview(&self, routes: &[Route]) {
        if self.previewed.get() || self.cancelled() {
            return;
        }
        let routes = routes
            .iter()
            .filter(|route| route.verdict.satisfied)
            .take(1)
            .cloned()
            .collect::<Vec<_>>();
        if routes.is_empty() {
            return;
        }
        let Some(portfolio) = CandidatePortfolio::forge(
            self.graph,
            routes,
            self.routing,
            &self.manual_defaults,
            &self.warmth,
            || self.cancelled(),
        ) else {
            return;
        };
        self.previewed.set(true);
        let _published = publish(
            &self.events,
            &self.wake,
            SearchEvent::Preview {
                serial: self.serial,
                portfolio: Box::new(portfolio),
                elapsed: self.started.elapsed(),
            },
        );
    }
}

pub struct SearchForge {
    graph: Arc<WalkGraph>,
    command: Sender<SearchJob>,
    pub events: Receiver<SearchEvent>,
    _thread: thread::JoinHandle<()>,
}

impl SearchForge {
    pub fn spawn(
        ctx: &Context,
        graph: Arc<WalkGraph>,
        finder: Arc<WalkRealmIndex>,
    ) -> Result<Self> {
        let (command, commands) = bounded::<SearchJob>(1);
        let (events_tx, events) = bounded(SEARCH_EVENT_CAPACITY);
        let worker_graph = Arc::clone(&graph);
        let wake = NativeWake::from_context(ctx);
        let worker = thread::Builder::new()
            .name("trail-search".to_owned())
            .spawn(move || {
                while let Ok(job) = commands.recv() {
                    if !forge_search(&worker_graph, &finder, &events_tx, &wake, job) {
                        break;
                    }
                }
            })
            .context("spawn trail search")?;
        Ok(Self {
            graph,
            command,
            events,
            _thread: worker,
        })
    }

    pub fn strike(&self, request: SearchRequest) -> Result<SearchHandle> {
        request.validate(&self.graph)?;
        let handle = SearchHandle(Arc::new(AtomicBool::new(false)));
        self.command
            .try_send(SearchJob {
                request,
                halt: Arc::clone(&handle.0),
            })
            .map_err(|_| anyhow::anyhow!("trail search is already running"))?;
        Ok(handle)
    }
}

fn forge_search(
    graph: &WalkGraph,
    finder: &WalkRealmIndex,
    events: &Sender<SearchEvent>,
    wake: &NativeWake,
    job: SearchJob,
) -> bool {
    let SearchJob { request, halt } = job;
    let started = Instant::now();
    let monitor = ForgeMonitor {
        serial: request.serial,
        graph,
        halt,
        events: events.clone(),
        wake: wake.clone(),
        routing: request.params.routing,
        manual_defaults: request.manual_defaults.clone(),
        warmth: request.warmth.clone(),
        started,
        previewed: Cell::new(false),
    };
    let mask = forge_boundary_mask(graph, &request, &monitor);
    let clipped;
    let scope = match &mask {
        BoundaryMask::All => SearchScope::projected(graph, finder.allowed(), finder.adjacency()),
        BoundaryMask::Edges(mask) => {
            clipped = finder
                .allowed()
                .iter()
                .zip(mask)
                .map(|(realm, boundary)| *realm && *boundary)
                .collect::<Vec<_>>();
            SearchScope::projected(graph, &clipped, finder.adjacency())
        }
        BoundaryMask::Halted => {
            return publish(
                events,
                wake,
                SearchEvent::Stopped {
                    serial: request.serial,
                    elapsed: started.elapsed(),
                },
            );
        }
    };
    let routes = exact_matches(request.solver.revise_scoped(
        request.params,
        scope,
        request.start,
        &request.constraints,
        request.count,
        &request.edicts,
        request.warmth.routes(),
        &monitor,
    ));
    let search_elapsed = started.elapsed();
    if monitor.cancelled() {
        return publish(
            events,
            wake,
            SearchEvent::Stopped {
                serial: request.serial,
                elapsed: search_elapsed,
            },
        );
    }
    if !publish(
        events,
        wake,
        SearchEvent::PreparingResults {
            serial: request.serial,
            count: routes.len(),
            elapsed: search_elapsed,
        },
    ) {
        return false;
    }
    let portfolio = CandidatePortfolio::forge(
        graph,
        routes,
        request.params.routing,
        &request.manual_defaults,
        &request.warmth,
        || monitor.cancelled(),
    );
    let event = if monitor.cancelled() {
        SearchEvent::Stopped {
            serial: request.serial,
            elapsed: started.elapsed(),
        }
    } else if let Some(portfolio) = portfolio {
        SearchEvent::Found {
            serial: request.serial,
            portfolio: Box::new(portfolio),
            elapsed: search_elapsed,
        }
    } else {
        SearchEvent::Stopped {
            serial: request.serial,
            elapsed: started.elapsed(),
        }
    };
    publish(events, wake, event)
}

fn forge_boundary_mask(
    graph: &WalkGraph,
    request: &SearchRequest,
    monitor: &ForgeMonitor<'_>,
) -> BoundaryMask {
    let Some(boundary) = &request.boundary else {
        return BoundaryMask::All;
    };
    monitor.report(SearchProgress {
        stage: SearchStage::Preparing,
        explored: 0,
        limit: graph.edges.len(),
        candidates: 0,
    });
    boundary
        .edge_mask(graph, |explored, limit| {
            monitor.report(SearchProgress {
                stage: SearchStage::Preparing,
                explored,
                limit,
                candidates: 0,
            });
            !monitor.cancelled()
        })
        .map_or(BoundaryMask::Halted, BoundaryMask::Edges)
}

fn publish(events: &Sender<SearchEvent>, wake: &NativeWake, event: SearchEvent) -> bool {
    let live = events.send(event).is_ok();
    let _woken = wake.request_foreground_repaint();
    live
}

fn exact_matches(routes: Vec<Route>) -> Vec<Route> {
    routes
        .into_iter()
        .filter(|route| route.verdict.satisfied)
        .collect()
}
