use crate::library::{FamilyId, Library};
use anyhow::{Context as _, Result, ensure};
use crossbeam_channel::{Receiver, Sender, bounded};
use egui::Context;
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use trailgen_core::{LoopConstraints, Route, SearchParams, SolverKind, TrailGraph, VertexId};

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
        let config = read_toml(&root.join("trailgen.toml"))?;
        let cached_graph = read_optional_json::<TrailGraph>(&root.join("cache/graph.json"))?;
        let generated_graph =
            read_optional_json::<TrailGraph>(&root.join("routes/generated.graph.json"))?;
        let graph = Arc::new(
            cached_graph
                .or(generated_graph)
                .context("project has no trail data")?,
        );
        ensure!(!graph.vertices.is_empty(), "project has no usable trails");
        let library = Library::open(&root, &graph)?;
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
    match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw)
            .with_context(|| format!("parse {}", path.display()))
            .map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

#[derive(Clone)]
pub struct SearchRequest {
    pub serial: u64,
    pub family: FamilyId,
    pub start: VertexId,
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
        let constraints = &self.constraints;
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
}

pub enum SearchEvent {
    Found {
        serial: u64,
        family: FamilyId,
        routes: Vec<Route>,
        elapsed: Duration,
    },
}

pub struct SearchForge {
    graph: Arc<TrailGraph>,
    command: Sender<SearchRequest>,
    pub events: Receiver<SearchEvent>,
    _thread: thread::JoinHandle<()>,
}

impl SearchForge {
    pub fn spawn(ctx: Context, graph: Arc<TrailGraph>) -> Result<Self> {
        let (command, commands) = bounded::<SearchRequest>(1);
        let (events_tx, events) = bounded(2);
        let worker_graph = Arc::clone(&graph);
        let worker = thread::Builder::new()
            .name("trail-search".to_owned())
            .spawn(move || {
                while let Ok(request) = commands.recv() {
                    let started = Instant::now();
                    let solver = request.solver.resolve(&worker_graph);
                    let routes = solver.solve(
                        request.params,
                        &worker_graph,
                        request.start,
                        &request.constraints,
                        request.count,
                    );
                    if events_tx
                        .send(SearchEvent::Found {
                            serial: request.serial,
                            family: request.family,
                            routes,
                            elapsed: started.elapsed(),
                        })
                        .is_err()
                    {
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

    pub fn strike(&self, request: SearchRequest) -> Result<()> {
        request.validate(&self.graph)?;
        self.command
            .try_send(request)
            .map_err(|_| anyhow::anyhow!("trail search is already running"))
    }
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
        let mut library = Library::default();
        let family = library.add_family(&constraints);
        let request = SearchRequest {
            serial: 1,
            family,
            start: VertexId(0),
            constraints,
            params: SearchParams::default(),
            solver: SolverKind::Auto,
            count: 1,
        };
        assert!(request.validate(&graph).is_err());
        Ok(())
    }

    #[test]
    fn refreshed_cache_is_the_project_graph() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::create_dir(temp.path().join("cache"))?;
        fs::create_dir(temp.path().join("routes"))?;
        fs::write(temp.path().join("trailgen.toml"), "name = 'live project'\n")?;
        let cached = fixture_graph()?;
        let mut generated = cached.clone();
        generated.vertices[0].coord.lat += 1.0;
        fs::write(
            temp.path().join("cache/graph.json"),
            serde_json::to_vec_pretty(&cached)?,
        )?;
        fs::write(
            temp.path().join("routes/generated.graph.json"),
            serde_json::to_vec_pretty(&generated)?,
        )?;

        let project = Project::open(temp.path())?;

        assert_eq!(project.config.name, "live project");
        assert_eq!(project.graph.as_ref(), &cached);
        assert!(project.library.loose_trails().next().is_none());
        assert!(temp.path().join("library/index.json").is_file());
        Ok(())
    }
}
