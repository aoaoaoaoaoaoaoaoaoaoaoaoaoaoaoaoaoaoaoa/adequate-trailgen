use crate::library::{Library, SearchBoundary};
use anyhow::{Context as _, Result, ensure};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use egui::Context;
use serde::Deserialize;
use std::{
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
    LoopConstraints, Route, SearchMonitor, SearchParams, SearchProgress, SearchScope, SearchStage,
    SolverKind, TrailGraph, VertexId,
};

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
    pub graph: Arc<TrailGraph>,
    pub config: WorkbenchConfig,
    pub library: Library,
}

impl Project {
    pub fn open(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("open project {}", root.display()))?;
        let config: WorkbenchConfig = read_toml(&root.join("trailgen.toml"))?;
        let graph = Arc::new(
            match read_optional_json::<TrailGraph>(&root.join("cache/graph.json"))? {
                Some(graph) => graph,
                None => {
                    read_optional_json::<TrailGraph>(&root.join("routes/generated.graph.json"))?
                        .context("project has no trail data")?
                }
            },
        );
        ensure!(!graph.vertices.is_empty(), "project has no usable trails");
        let library = Library::open(&root, &graph, &config.constraints)?;
        Ok(Self {
            root,
            graph,
            config,
            library,
        })
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

#[derive(Clone)]
pub struct SearchRequest {
    pub serial: u64,
    pub start: VertexId,
    pub boundary: Option<SearchBoundary>,
    pub constraints: LoopConstraints,
    pub params: SearchParams,
    pub solver: SolverKind,
    pub count: usize,
}

impl SearchRequest {
    pub fn validate(&self, graph: &TrailGraph) -> Result<()> {
        ensure!(
            self.start.0 < graph.vertices.len(),
            "trailhead is outside downloaded trail data"
        );
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
        ("minimum difficulty", constraints.min_difficulty),
        ("maximum difficulty", constraints.max_difficulty),
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
            .target_difficulty
            .is_none_or(|target| target.is_finite() && target >= 0.0),
        "target difficulty must be finite and nonnegative"
    );
    for (name, low, high) in [
        (
            "distance",
            constraints.min_distance_m,
            constraints.max_distance_m,
        ),
        (
            "difficulty",
            constraints.min_difficulty,
            constraints.max_difficulty,
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
    Found {
        serial: u64,
        routes: Vec<Route>,
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

struct ForgeMonitor {
    serial: u64,
    halt: Arc<AtomicBool>,
    events: Sender<SearchEvent>,
    ctx: Context,
}

impl SearchMonitor for ForgeMonitor {
    fn cancelled(&self) -> bool {
        self.halt.load(Ordering::Acquire)
    }

    fn report(&self, progress: SearchProgress) {
        let _event = self.events.send(SearchEvent::Progress {
            serial: self.serial,
            progress,
        });
        self.ctx.request_repaint();
    }
}

pub struct SearchForge {
    graph: Arc<TrailGraph>,
    command: Sender<SearchJob>,
    pub events: Receiver<SearchEvent>,
    _thread: thread::JoinHandle<()>,
}

impl SearchForge {
    pub fn spawn(ctx: Context, graph: Arc<TrailGraph>) -> Result<Self> {
        let (command, commands) = bounded::<SearchJob>(1);
        let (events_tx, events) = unbounded();
        let worker_graph = Arc::clone(&graph);
        let worker = thread::Builder::new()
            .name("trail-search".to_owned())
            .spawn(move || {
                while let Ok(SearchJob { request, halt }) = commands.recv() {
                    let started = Instant::now();
                    let monitor = ForgeMonitor {
                        serial: request.serial,
                        halt,
                        events: events_tx.clone(),
                        ctx: ctx.clone(),
                    };
                    let mask = if let Some(boundary) = &request.boundary {
                        monitor.report(SearchProgress {
                            stage: SearchStage::Preparing,
                            explored: 0,
                            limit: worker_graph.edges.len(),
                            candidates: 0,
                        });
                        let Some(mask) = boundary.edge_mask(&worker_graph, |explored, limit| {
                            monitor.report(SearchProgress {
                                stage: SearchStage::Preparing,
                                explored,
                                limit,
                                candidates: 0,
                            });
                            !monitor.cancelled()
                        }) else {
                            let _stopped = events_tx.send(SearchEvent::Stopped {
                                serial: request.serial,
                                elapsed: started.elapsed(),
                            });
                            ctx.request_repaint();
                            continue;
                        };
                        Some(mask)
                    } else {
                        None
                    };
                    let scope = mask.as_deref().map_or_else(
                        || SearchScope::all(&worker_graph),
                        |mask| SearchScope::restricted(&worker_graph, mask),
                    );
                    let routes = exact_matches(request.solver.solve_scoped(
                        request.params,
                        scope,
                        request.start,
                        &request.constraints,
                        request.count,
                        &monitor,
                    ));
                    let event = if monitor.cancelled() {
                        SearchEvent::Stopped {
                            serial: request.serial,
                            elapsed: started.elapsed(),
                        }
                    } else {
                        SearchEvent::Found {
                            serial: request.serial,
                            routes,
                            elapsed: started.elapsed(),
                        }
                    };
                    if events_tx.send(event).is_err() {
                        break;
                    }
                    ctx.request_repaint();
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

fn exact_matches(routes: Vec<Route>) -> Vec<Route> {
    routes
        .into_iter()
        .filter(|route| route.verdict.satisfied)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use trailgen_core::{GraphBuilder, Terrain, io::geojson};

    fn fixture_graph() -> Result<TrailGraph> {
        Ok(
            GraphBuilder::default().build(&geojson::network_from_str(include_str!(
                "../../trailgen-core/tests/fixtures/mini_network.geojson"
            ))?)?,
        )
    }

    #[test]
    fn gui_defaults_bound_interactive_search_work() {
        let search = WorkbenchConfig::default().search;
        assert_eq!(search.max_frontier, 5_000);
        assert_eq!(search.closure_paths, 1);
        assert!(search.keep >= 12);
    }

    #[test]
    fn search_rejects_an_inverted_terrain_window() -> Result<()> {
        let graph = fixture_graph()?;
        let mut constraints = LoopConstraints::default();
        let _minimum = constraints.min_terrain_fraction.insert(Terrain::Trail, 0.8);
        let _maximum = constraints.max_terrain_fraction.insert(Terrain::Trail, 0.2);
        let request = SearchRequest {
            serial: 1,
            start: VertexId(0),
            boundary: None,
            constraints,
            params: SearchParams::default(),
            solver: SolverKind::Auto,
            count: 1,
        };
        assert!(request.validate(&graph).is_err());
        Ok(())
    }

    #[test]
    fn gui_discards_solver_near_misses() -> Result<()> {
        let graph = fixture_graph()?;
        let mut constraints = LoopConstraints {
            min_distance_m: 100_000.0,
            max_distance_m: 200_000.0,
            ..LoopConstraints::default()
        };
        constraints.max_road_fraction = 1.0;
        constraints.max_low_confidence_fraction = 1.0;
        constraints.max_repeated_edge_fraction = 1.0;
        let routes = SolverKind::Exact.solve(
            SearchParams::default(),
            &graph,
            VertexId(0),
            &constraints,
            12,
        );
        assert!(
            !routes.is_empty(),
            "fixture should produce ranked near misses"
        );
        assert!(routes.iter().all(|route| !route.verdict.satisfied));
        assert!(exact_matches(routes).is_empty());
        Ok(())
    }

    #[test]
    fn refreshed_cache_is_the_project_graph() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::create_dir(temp.path().join("cache"))?;
        fs::create_dir(temp.path().join("routes"))?;
        fs::write(temp.path().join("trailgen.toml"), "name = 'live project'\n")?;
        let cached = fixture_graph()?;
        fs::write(
            temp.path().join("cache/graph.json"),
            serde_json::to_vec_pretty(&cached)?,
        )?;
        fs::write(temp.path().join("routes/generated.graph.json"), b"obsolete")?;
        Library::default().save(temp.path())?;

        let project = Project::open(temp.path())?;

        assert_eq!(project.config.name, "live project");
        assert_eq!(project.graph.as_ref(), &cached);
        assert!(project.library.trails().is_empty());
        assert!(temp.path().join("library/index.json").is_file());
        Ok(())
    }
}
