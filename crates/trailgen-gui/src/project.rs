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
        let config = read_toml(&root.join("trailgen.toml"))?;
        let cached_graph = read_optional_json::<TrailGraph>(&root.join("cache/graph.json"))?;
        let generated_graph =
            read_optional_json::<TrailGraph>(&root.join("routes/generated.graph.json"))?;
        let saved_compatible = manifest.is_some()
            && generated_graph.as_ref().is_some_and(|generated| {
                cached_graph
                    .as_ref()
                    .is_none_or(|cached| cached == generated)
            });
        let graph = Arc::new(
            cached_graph
                .or(generated_graph)
                .context("project has no cached trail graph")?,
        );
        ensure!(!graph.vertices.is_empty(), "project graph has no vertices");

        let saved = manifest.as_ref().filter(|_| saved_compatible);
        let saved_constraints = saved.map(|manifest| &manifest.effective_config.constraints);
        let mut routes = if saved_compatible {
            read_optional_json::<Vec<Route>>(&root.join("routes/generated.routes.json"))?
                .unwrap_or_default()
                .into_iter()
                .map(|route| {
                    remeasure(
                        &graph,
                        route,
                        saved_constraints.expect("saved generation has constraints"),
                    )
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        if let Some(constraints) = saved_constraints {
            rank_routes(&mut routes, constraints);
        }

        let saved_start =
            saved.filter(|manifest| manifest.snapped_start_vertex.0 < graph.vertices.len());
        let start = routes.first().map_or_else(
            || {
                saved_start.map_or_else(
                    || central_vertex(&graph),
                    |manifest| manifest.snapped_start_vertex,
                )
            },
            |route| route.start,
        );
        let requested_start = saved_start
            .filter(|manifest| manifest.snapped_start_vertex == start)
            .map_or(graph.vertices[start.0].coord, |manifest| {
                manifest.requested_start
            });

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
    let topology = undirected_topology(graph);
    let component =
        dominant_loop_block(graph, &topology).unwrap_or_else(|| largest_component(&topology));
    let count = component.len() as f64;
    let center = component.iter().fold(Coord::new(0.0, 0.0), |sum, index| {
        let coord = graph.vertices[*index].coord;
        Coord::new(sum.lon + coord.lon, sum.lat + coord.lat)
    });
    let center = Coord::new(center.lon / count, center.lat / count);
    let quiet = component
        .iter()
        .copied()
        .filter(|vertex| topology[*vertex].len() == 2)
        .collect::<Vec<_>>();
    let candidates = if quiet.is_empty() { component } else { quiet };
    candidates
        .into_iter()
        .min_by(|left, right| {
            graph.vertices[*left]
                .coord
                .planar_distance2(center)
                .total_cmp(&graph.vertices[*right].coord.planar_distance2(center))
                .then_with(|| left.cmp(right))
        })
        .map(VertexId)
        .expect("nonempty graph must have a central component")
}

#[derive(Clone, Copy)]
struct Incidence {
    neighbor: usize,
    edge: usize,
}

fn undirected_topology(graph: &TrailGraph) -> Vec<Vec<Incidence>> {
    let mut topology = vec![Vec::new(); graph.vertices.len()];
    for (index, edge) in graph.edges.iter().enumerate() {
        topology[edge.a.0].push(Incidence {
            neighbor: edge.b.0,
            edge: index,
        });
        topology[edge.b.0].push(Incidence {
            neighbor: edge.a.0,
            edge: index,
        });
    }
    topology
}

#[derive(Clone, Copy)]
struct DfsFrame {
    vertex: usize,
    parent_edge: Option<usize>,
    cursor: usize,
}

fn dominant_loop_block(graph: &TrailGraph, topology: &[Vec<Incidence>]) -> Option<Vec<usize>> {
    let mut discovered = vec![usize::MAX; topology.len()];
    let mut low = vec![usize::MAX; topology.len()];
    let mut clock = 0usize;
    let mut edge_stack = Vec::new();
    let mut champion = None::<(f64, usize, Vec<usize>)>;

    for root in 0..topology.len() {
        if discovered[root] != usize::MAX {
            continue;
        }
        discovered[root] = clock;
        low[root] = clock;
        clock += 1;
        let mut frames = vec![DfsFrame {
            vertex: root,
            parent_edge: None,
            cursor: 0,
        }];
        while let Some(frame) = frames.last().copied() {
            if frame.cursor < topology[frame.vertex].len() {
                let incidence = topology[frame.vertex][frame.cursor];
                frames.last_mut().expect("active DFS frame").cursor += 1;
                if frame.parent_edge == Some(incidence.edge) {
                    continue;
                }
                if discovered[incidence.neighbor] == usize::MAX {
                    edge_stack.push(incidence.edge);
                    discovered[incidence.neighbor] = clock;
                    low[incidence.neighbor] = clock;
                    clock += 1;
                    frames.push(DfsFrame {
                        vertex: incidence.neighbor,
                        parent_edge: Some(incidence.edge),
                        cursor: 0,
                    });
                } else if discovered[incidence.neighbor] < discovered[frame.vertex] {
                    edge_stack.push(incidence.edge);
                    low[frame.vertex] = low[frame.vertex].min(discovered[incidence.neighbor]);
                }
                continue;
            }

            let finished = frames.pop().expect("active DFS frame");
            let Some(parent_edge) = finished.parent_edge else {
                continue;
            };
            let parent = frames.last().expect("child DFS frame has parent").vertex;
            low[parent] = low[parent].min(low[finished.vertex]);
            if low[finished.vertex] < discovered[parent] {
                continue;
            }
            let mut block = Vec::new();
            loop {
                let edge = edge_stack.pop().expect("tree edge remains on DFS stack");
                block.push(edge);
                if edge == parent_edge {
                    break;
                }
            }
            if block.len() < 2 {
                continue;
            }
            let length_m: f64 = block
                .iter()
                .map(|edge| graph.edges[*edge].attr.length_m)
                .sum();
            let mut vertices = block
                .iter()
                .flat_map(|edge| [graph.edges[*edge].a.0, graph.edges[*edge].b.0])
                .collect::<Vec<_>>();
            vertices.sort_unstable();
            vertices.dedup();
            let candidate = (length_m, block.len(), vertices);
            if champion.as_ref().is_none_or(|current| {
                candidate
                    .0
                    .total_cmp(&current.0)
                    .then_with(|| candidate.1.cmp(&current.1))
                    .then_with(|| current.2.cmp(&candidate.2))
                    .is_gt()
            }) {
                champion = Some(candidate);
            }
        }
    }
    champion.map(|(_, _, vertices)| vertices)
}

fn largest_component(topology: &[Vec<Incidence>]) -> Vec<usize> {
    let mut seen = vec![false; topology.len()];
    let mut champion = Vec::new();
    for root in 0..topology.len() {
        if seen[root] {
            continue;
        }
        let mut component = Vec::new();
        let mut frontier = vec![root];
        seen[root] = true;
        while let Some(vertex) = frontier.pop() {
            component.push(vertex);
            for incidence in &topology[vertex] {
                if !seen[incidence.neighbor] {
                    seen[incidence.neighbor] = true;
                    frontier.push(incidence.neighbor);
                }
            }
        }
        if component.len() > champion.len() {
            champion = component;
        }
    }
    champion
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
    use trailgen_core::{
        GraphBuilder, Terrain,
        io::{geojson, osm},
    };

    fn fixture_graph() -> Result<TrailGraph> {
        Ok(
            GraphBuilder::default().build(&geojson::network_from_str(include_str!(
                "../../trailgen-core/tests/fixtures/mini_network.geojson"
            ))?)?,
        )
    }

    #[test]
    fn project_defaults_and_saved_generation_constraints_remain_distinct() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        fs::create_dir(root.join("cache"))?;
        fs::create_dir(root.join("routes"))?;
        fs::write(
            root.join("trailgen.toml"),
            "name = 'live defaults'\n[constraints]\nmin_distance_m = 35000\nmax_distance_m = 50000\n",
        )?;
        let graph = fixture_graph()?;
        let saved_constraints = LoopConstraints {
            min_distance_m: 3_000.0,
            max_distance_m: 8_000.0,
            ..LoopConstraints::default()
        };
        let routes = SolverKind::Exact.solve(
            SearchParams::default(),
            &graph,
            VertexId(0),
            &saved_constraints,
            1,
        );
        let route = routes.first().context("fixture must contain a loop")?;
        fs::write(
            root.join("cache/graph.json"),
            serde_json::to_vec_pretty(&graph)?,
        )?;
        fs::write(
            root.join("routes/generated.graph.json"),
            serde_json::to_vec_pretty(&graph)?,
        )?;
        fs::write(
            root.join("routes/generated.routes.json"),
            serde_json::to_vec_pretty(&routes)?,
        )?;
        fs::write(
            root.join("routes/generated.manifest.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "requested_start": graph.vertices[route.start.0].coord,
                "snapped_start_vertex": route.start,
                "effective_config": {
                    "name": "saved generation",
                    "constraints": saved_constraints,
                    "search": SearchParams::default(),
                    "solver": "exact"
                }
            }))?,
        )?;

        let project = Project::open(root)?;

        assert!((project.config.constraints.min_distance_m - 35_000.0).abs() < f64::EPSILON);
        assert_eq!(project.start, route.start);
        assert!(!project.routes.is_empty());
        assert!(project.routes.iter().all(|route| {
            route
                .verdict
                .violations
                .iter()
                .all(|violation| !violation.contains("minimum 35.00 km"))
        }));
        Ok(())
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
            constraints,
            params: SearchParams::default(),
            solver: SolverKind::Auto,
            count: 1,
        };
        assert!(request.validate(&graph).is_err());
        Ok(())
    }

    #[test]
    fn default_trailhead_occupies_the_dominant_loop_block() -> Result<()> {
        let drafts = osm::network_from_str(
            r#"<osm version="0.6">
  <node id="1" lat="40.00" lon="-105.00"/><node id="2" lat="40.00" lon="-104.80"/>
  <node id="3" lat="40.20" lon="-104.80"/><node id="4" lat="40.20" lon="-105.20"/>
  <node id="5" lat="40.00" lon="-105.20"/><node id="6" lat="40.07" lon="-105.00"/>
  <node id="7" lat="40.06" lon="-105.01"/>
  <way id="10"><nd ref="1"/><nd ref="2"/><tag k="highway" v="path"/></way>
  <way id="11"><nd ref="2"/><nd ref="3"/><tag k="highway" v="path"/></way>
  <way id="12"><nd ref="3"/><nd ref="4"/><tag k="highway" v="path"/></way>
  <way id="13"><nd ref="4"/><nd ref="5"/><tag k="highway" v="path"/></way>
  <way id="14"><nd ref="5"/><nd ref="1"/><tag k="highway" v="path"/></way>
  <way id="15"><nd ref="1"/><nd ref="6"/><tag k="highway" v="path"/></way>
  <way id="16"><nd ref="6"/><nd ref="7"/><tag k="highway" v="path"/></way>
  <way id="17"><nd ref="7"/><nd ref="1"/><tag k="highway" v="path"/></way>
</osm>"#,
        )?;
        let graph = GraphBuilder::default().build(&drafts)?;

        let start = central_vertex(&graph);

        assert_eq!(graph.adjacency[start.0].len(), 2);
        assert!(matches!(graph.vertices[start.0].coord.lat, 40.0 | 40.2));
        Ok(())
    }

    #[test]
    fn refreshed_cache_graph_rejects_a_stale_generation_snapshot() -> Result<()> {
        fn square(latitude: f64) -> Result<TrailGraph> {
            let raw = format!(
                r#"<osm version="0.6">
  <node id="1" lat="{latitude}" lon="-105.01"/>
  <node id="2" lat="{latitude}" lon="-104.99"/>
  <node id="3" lat="{}" lon="-104.99"/>
  <node id="4" lat="{}" lon="-105.01"/>
  <way id="10"><nd ref="1"/><nd ref="2"/><tag k="highway" v="path"/></way>
  <way id="11"><nd ref="2"/><nd ref="3"/><tag k="highway" v="path"/></way>
  <way id="12"><nd ref="3"/><nd ref="4"/><tag k="highway" v="path"/></way>
  <way id="13"><nd ref="4"/><nd ref="1"/><tag k="highway" v="path"/></way>
</osm>"#,
                latitude + 0.02,
                latitude + 0.02,
            );
            Ok(GraphBuilder::default().build(&osm::network_from_str(&raw)?)?)
        }

        let temp = tempfile::tempdir()?;
        let root = temp.path();
        fs::create_dir(root.join("cache"))?;
        fs::create_dir(root.join("routes"))?;
        fs::write(root.join("trailgen.toml"), "name = 'live project'\n")?;
        let generated = square(40.0)?;
        let cached = square(41.0)?;
        fs::write(
            root.join("cache/graph.json"),
            serde_json::to_vec_pretty(&cached)?,
        )?;
        fs::write(
            root.join("routes/generated.graph.json"),
            serde_json::to_vec_pretty(&generated)?,
        )?;
        fs::write(root.join("routes/generated.routes.json"), "[]")?;
        fs::write(
            root.join("routes/generated.manifest.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "requested_start": generated.vertices[0].coord,
                "snapped_start_vertex": 0,
                "effective_config": { "name": "stale run" }
            }))?,
        )?;

        let project = Project::open(root)?;

        assert_eq!(project.config.name, "live project");
        assert_eq!(project.graph.as_ref(), &cached);
        assert!(project.routes.is_empty());
        assert!(project.requested_start.lat >= 41.0);
        Ok(())
    }
}
