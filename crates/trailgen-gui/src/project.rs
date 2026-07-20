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
use trailgen_core::{
    Coord, LoopConstraints, Route, SearchParams, SolverKind, TrailGraph, VertexId, rank_routes,
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
            search: SearchParams::default(),
            solver: SolverKind::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct GenerationManifest {
    requested_start: Coord,
    snapped_start_vertex: VertexId,
    effective_config: WorkbenchConfig,
}

pub struct Project {
    pub root: PathBuf,
    pub graph: Arc<TrailGraph>,
    pub routes: Vec<Route>,
    pub config: WorkbenchConfig,
    pub start: VertexId,
    pub requested_start: Coord,
}

impl Project {
    pub fn open(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("open project {}", root.display()))?;
        let manifest_path = root.join("routes/generated.manifest.json");
        let manifest = read_optional_json::<GenerationManifest>(&manifest_path)?;
        let config = match &manifest {
            Some(manifest) => manifest.effective_config.clone(),
            None => read_toml(&root.join("trailgen.toml"))?,
        };

        let generated_graph = root.join("routes/generated.graph.json");
        let graph_path = if generated_graph.is_file() {
            generated_graph
        } else {
            root.join("cache/graph.json")
        };
        let graph = Arc::new(read_json::<TrailGraph>(&graph_path)?);
        ensure!(!graph.vertices.is_empty(), "project graph has no vertices");

        let mut routes =
            read_optional_json::<Vec<Route>>(&root.join("routes/generated.routes.json"))?
                .unwrap_or_default()
                .into_iter()
                .map(|route| remeasure(&graph, route, &config.constraints))
                .collect::<Result<Vec<_>>>()?;
        rank_routes(&mut routes, &config.constraints);

        let (requested_start, start) = manifest.map_or_else(
            || {
                let start = routes
                    .first()
                    .map_or_else(|| central_vertex(&graph), |route| route.start);
                (graph.vertices[start.0].coord, start)
            },
            |manifest| (manifest.requested_start, manifest.snapped_start_vertex),
        );
        ensure!(
            start.0 < graph.vertices.len(),
            "generation manifest start vertex {} is outside graph",
            start.0
        );

        Ok(Self {
            root,
            graph,
            routes,
            config,
            start,
            requested_start,
        })
    }
}

fn remeasure(graph: &TrailGraph, route: Route, constraints: &LoopConstraints) -> Result<Route> {
    ensure!(
        graph.walk_edges(route.start, &route.edges).is_some(),
        "stored route `{}` is not a legal walk through the generated graph",
        route.name
    );
    Ok(Route::from_edges(
        route.name,
        graph,
        route.start,
        route.edges,
        constraints,
    ))
}

fn central_vertex(graph: &TrailGraph) -> VertexId {
    let n = graph.vertices.len() as f64;
    let center = graph
        .vertices
        .iter()
        .fold(Coord::new(0.0, 0.0), |sum, vertex| {
            Coord::new(sum.lon + vertex.coord.lon, sum.lat + vertex.coord.lat)
        });
    graph
        .nearest_vertex(Coord::new(center.lon / n, center.lat / n))
        .expect("nonempty graph must have a central vertex")
}

fn read_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
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
            "trailhead is outside graph"
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
        let c = &self.constraints;
        for (name, value) in [
            ("minimum distance", c.min_distance_m),
            ("maximum distance", c.max_distance_m),
            ("minimum difficulty", c.min_difficulty),
            ("maximum difficulty", c.max_difficulty),
            ("minimum ascent", c.min_ascent_m),
            ("maximum ascent", c.max_ascent_m),
            ("minimum descent", c.min_descent_m),
            ("maximum descent", c.max_descent_m),
        ] {
            ensure!(
                value.is_finite() && value >= 0.0,
                "{name} must be finite and nonnegative"
            );
        }
        for (name, low, high) in [
            ("distance", c.min_distance_m, c.max_distance_m),
            ("difficulty", c.min_difficulty, c.max_difficulty),
            ("ascent", c.min_ascent_m, c.max_ascent_m),
            ("descent", c.min_descent_m, c.max_descent_m),
        ] {
            ensure!(low <= high, "{name} minimum exceeds maximum");
        }
        for (name, fraction) in [
            ("road", c.max_road_fraction),
            ("low confidence", c.max_low_confidence_fraction),
            ("restricted access", c.max_restricted_access_fraction),
            ("repeated edge", c.max_repeated_edge_fraction),
        ] {
            ensure!(
                fraction.is_finite() && (0.0..=1.0).contains(&fraction),
                "maximum {name} fraction must lie in 0–1"
            );
        }
        for (terrain, fraction) in &c.min_terrain_fraction {
            ensure!(
                fraction.is_finite() && (0.0..=1.0).contains(fraction),
                "minimum {terrain:?} fraction must lie in 0–1"
            );
        }
        for (terrain, maximum) in &c.max_terrain_fraction {
            ensure!(
                maximum.is_finite() && (0.0..=1.0).contains(maximum),
                "maximum {terrain:?} fraction must lie in 0–1"
            );
            let minimum = c
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
            !c.allowed_shapes.is_empty(),
            "at least one route shape must be allowed"
        );
        Ok(())
    }
}

pub enum SearchEvent {
    Found {
        serial: u64,
        routes: Vec<Route>,
        elapsed: Duration,
        solver: SolverKind,
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
            .name("trail-loop-forge".to_owned())
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
                            routes,
                            elapsed: started.elapsed(),
                            solver,
                        })
                        .is_err()
                    {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .context("spawn trail-loop forge")?;
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
            .map_err(|_| anyhow::anyhow!("trail-loop forge is already occupied"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trailgen_core::Terrain;

    #[test]
    fn demo_prefers_effective_generation_configuration() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo/mini-loop");
        let project = Project::open(&root)?;
        assert_eq!(project.config.name, "Mini Loop");
        assert!((project.config.constraints.min_distance_m - 3_000.0).abs() < f64::EPSILON);
        assert_eq!(project.start, VertexId(3));
        assert!(!project.routes.is_empty());
        Ok(())
    }

    #[test]
    fn search_rejects_an_inverted_terrain_window() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo/mini-loop");
        let project = Project::open(&root)?;
        let mut constraints = project.config.constraints;
        let _minimum = constraints.min_terrain_fraction.insert(Terrain::Trail, 0.8);
        let _maximum = constraints.max_terrain_fraction.insert(Terrain::Trail, 0.2);
        let request = SearchRequest {
            serial: 1,
            start: project.start,
            constraints,
            params: SearchParams::default(),
            solver: SolverKind::Auto,
            count: 1,
        };
        assert!(request.validate(&project.graph).is_err());
        Ok(())
    }
}
