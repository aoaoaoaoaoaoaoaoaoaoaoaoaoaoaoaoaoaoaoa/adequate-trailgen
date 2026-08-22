use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

use egui_tester::{
    Anchor, AppCommand, Application, Error, Graphics, Network, ProbeFrame, ReactionBudget, Result,
    Story, Testbed, WindowQuery,
};
use num_traits::ToPrimitive as _;
use serde_json::Value;
use trailgen_core::{
    Coord, GraphBuilder, LoopConstraints, RouteShape, SearchParams, SolverKind, decode_graph,
    encode_graph, io::geojson,
};

use crate::{fixture::FixtureWorld, observation::Observation};

pub use egui_tester::demand;

pub const TITLE_FRAGMENT: &str = "trailgen";

pub type TrailStory<'app, 'bed> = Story<'app, 'bed, Observation>;
pub type TrailFrame = ProbeFrame<Observation>;
pub use trailgen_contract::{Target, TargetClass};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataMode {
    Offline,
    FixtureProviders,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunClass {
    Functional,
    Performance,
}

pub struct Harness<'a> {
    pub testbed: &'a Testbed,
    pub binary: &'a Path,
    pub fixtures: &'a FixtureWorld,
    pub artifacts: Option<&'a Path>,
}

impl<'a> Harness<'a> {
    pub const fn new(
        testbed: &'a Testbed,
        binary: &'a Path,
        fixtures: &'a FixtureWorld,
        artifacts: Option<&'a Path>,
    ) -> Self {
        Self {
            testbed,
            binary,
            fixtures,
            artifacts,
        }
    }

    pub fn clipboard_text(&self) -> Result<String> {
        let probe = env::current_exe()
            .map_err(|error| verdict(format!("resolve Trailgen acceptance executable: {error}")))?;
        let clipboard = self.testbed.launch(
            AppCommand::new(probe)
                .arg(crate::CLIPBOARD_PROBE)
                .runtime(Duration::from_secs(5)),
        )?;
        let exit = clipboard.wait(Duration::from_secs(5))?;
        demand(
            exit.success(),
            format!("clipboard probe could not read native text: {exit:#?}"),
        )?;
        clipboard.terminate()?;
        Ok(exit.stdout.trim().to_owned())
    }

    pub fn seed_project(&self, root: &str, source: &str, route: bool) -> Result<()> {
        let relative_root = Path::new(root)
            .strip_prefix("/test")
            .map_err(|_| verdict(format!("fixture project escaped /test: {root}")))?;
        let relative_source = Path::new(source)
            .strip_prefix("/test")
            .map_err(|_| verdict(format!("fixture source escaped /test: {source}")))?;
        let raw = self.testbed.read_private_to_string(relative_source)?;
        let drafts = geojson::network_from_str(&raw)
            .map_err(|error| verdict(format!("parse fixture network: {error}")))?;
        let graph = GraphBuilder::default()
            .build(&drafts)
            .map_err(|error| verdict(format!("build fixture graph: {error}")))?;
        let encoded = encode_graph(&graph)
            .map_err(|error| verdict(format!("encode fixture graph: {error}")))?;
        let graph = decode_graph(&encoded)
            .map_err(|error| verdict(format!("decode fixture graph: {error}")))?;
        let _config = self.testbed.write_private(
            relative_root.join("trailgen.toml"),
            b"name = \"Acceptance\"\n",
        )?;
        let _graph = self
            .testbed
            .write_private(relative_root.join("cache/graph.bin"), encoded)?;
        if route {
            let constraints = LoopConstraints {
                min_distance_m: 0.0,
                max_distance_m: 10_000.0,
                max_low_confidence_fraction: 1.0,
                max_restricted_access_fraction: 1.0,
                allowed_shapes: vec![RouteShape::Loop],
                ..LoopConstraints::default()
            };
            let start = graph
                .nearest_vertex(Coord {
                    lon: -105.0,
                    lat: 40.0,
                    ele: None,
                })
                .ok_or_else(|| verdict("fixture graph has no trailhead"))?;
            let routes = SolverKind::Exact.solve(
                SearchParams {
                    max_hops: 256,
                    max_frontier: 200_000,
                    keep: 12,
                    closure_paths: 4,
                    seed: 2,
                    routing: trailgen_core::RoutingLaw::default(),
                },
                &graph,
                start,
                &constraints,
                1,
            );
            demand(!routes.is_empty(), "fixture network yielded no saved route")?;
            let generated_graph = serde_json::to_vec(&graph)
                .map_err(|error| verdict(format!("serialize fixture graph: {error}")))?;
            let generated_routes = serde_json::to_vec(&routes)
                .map_err(|error| verdict(format!("serialize fixture route: {error}")))?;
            let _legacy_graph = self.testbed.write_private(
                relative_root.join("routes/generated.graph.json"),
                generated_graph,
            )?;
            let _legacy_routes = self.testbed.write_private(
                relative_root.join("routes/generated.routes.json"),
                generated_routes,
            )?;
        }
        Ok(())
    }

    pub fn gui(&self, project: Option<&str>, data: DataMode, run: RunClass) -> AppCommand {
        let mut args = vec!["gui"];
        if let Some(project) = project {
            args.push(project);
        }
        if data == DataMode::Offline {
            args.push("--offline");
        }
        let mut command = AppCommand::new(self.binary)
            .args(args)
            .env("TRAILGEN_BASEMAP_ARCHIVE", "/test/fixtures/basemap.pmtiles")
            .graphics(match run {
                RunClass::Functional => Graphics::Software,
                RunClass::Performance => Graphics::Host,
            })
            .network(Network::Deny)
            .witness("probes/trailgen.observations")
            .runtime(Duration::from_mins(3));
        if let Some(project) = project {
            command = command.env(
                "TRAILGEN_ACCEPTANCE_EXPORT_PATH",
                format!("{project}/exported.gpx"),
            );
        }
        match data {
            DataMode::Offline => command,
            DataMode::FixtureProviders => FixtureWorld::admit(command),
        }
    }

    pub fn launch_gui(
        &self,
        project: Option<&str>,
        data: DataMode,
        run: RunClass,
    ) -> Result<Application<'a>> {
        self.testbed.launch(self.gui(project, data, run))
    }

    pub fn launch_uninstrumented_smoke(&self) -> Result<Application<'a>> {
        self.testbed.launch(
            AppCommand::new(self.binary)
                .args(["gui", "--offline"])
                .env("TRAILGEN_BASEMAP_ARCHIVE", "/test/fixtures/basemap.pmtiles")
                .graphics(Graphics::Software)
                .network(Network::Deny)
                .runtime(Duration::from_secs(45)),
        )
    }

    pub fn story<'app>(
        &'a self,
        app: &'app Application<'a>,
        run: RunClass,
    ) -> Result<TrailStory<'app, 'a>> {
        let mut story: TrailStory<'app, 'a> = Story::bind(
            self.testbed,
            app,
            WindowQuery::title_contains(TITLE_FRAGMENT),
            match run {
                RunClass::Functional => ReactionBudget::functional(Duration::from_secs(30)),
                RunClass::Performance => instant_budget(),
            },
        )?;
        let frame = story.ready(Duration::from_secs(30))?;
        demand(
            frame.state.contract == trailgen_contract::UI_FINGERPRINT,
            format!(
                "Trailgen UI contract mismatch: expected {}, observed {}",
                trailgen_contract::UI_FINGERPRINT,
                frame.state.contract
            ),
        )?;
        Ok(story)
    }
}

pub fn first_anchor(
    frame: &TrailFrame,
    class: TargetClass,
    missing: &'static str,
) -> Result<Anchor> {
    frame
        .anchors
        .iter()
        .find(|anchor| anchor.name.starts_with(class.prefix()))
        .cloned()
        .ok_or_else(|| verdict(missing))
}

pub fn map_pixel(frame: &TrailFrame, coordinate: [f64; 2]) -> Result<(i16, i16)> {
    let map = frame
        .state
        .map
        .as_ref()
        .ok_or_else(|| verdict("witness omitted map transform"))?;
    let ppp = f64::from(frame.ppp);
    let world = world_from_coord(coordinate);
    let [x0, y0, x1, y1] = map.rect.map(f64::from);
    let center = [(x0 + x1) * 0.5, (y0 + y1) * 0.5];
    let logical = [
        (world[0] - map.center[0]).mul_add(map.world_points, center[0]),
        (world[1] - map.center[1]).mul_add(map.world_points, center[1]),
    ];
    screen_point([logical[0] * ppp, logical[1] * ppp])
}

pub fn screen_point([x, y]: [f64; 2]) -> Result<(i16, i16)> {
    Ok((
        x.round()
            .to_i16()
            .ok_or_else(|| verdict("screen target exceeded X11 coordinate range"))?,
        y.round()
            .to_i16()
            .ok_or_else(|| verdict("screen target exceeded X11 coordinate range"))?,
    ))
}

pub fn read_json(testbed: &Testbed, relative: impl AsRef<Path>) -> Result<Value> {
    let relative = relative.as_ref();
    let bytes = testbed.read_private(relative)?;
    serde_json::from_slice(&bytes).map_err(|error| Error::Probe {
        path: relative.to_owned(),
        detail: error.to_string(),
    })
}

pub fn instant_budget() -> ReactionBudget {
    ReactionBudget::performance(Duration::from_millis(300))
        .through_surface_present()
        .timeout(Duration::from_secs(6))
}

pub fn durable_budget() -> ReactionBudget {
    ReactionBudget::performance(Duration::from_millis(650))
        .through_surface_present()
        .timeout(Duration::from_secs(10))
}

pub fn search_reaction_budget() -> ReactionBudget {
    ReactionBudget::performance(Duration::from_millis(450))
        .through_surface_present()
        .timeout(Duration::from_secs(8))
}

pub fn verdict(detail: impl Into<String>) -> Error {
    Error::Verdict {
        detail: detail.into(),
    }
}

pub fn sibling_binary() -> Result<PathBuf> {
    let executable = env::current_exe().map_err(|source| Error::Io {
        operation: "resolve acceptance executable",
        path: PathBuf::from("<current-exe>"),
        source,
    })?;
    let binary = executable
        .parent()
        .map(|target| target.join("trailgen"))
        .ok_or_else(|| verdict("acceptance executable has no sibling target directory"))?;
    demand(
        binary.is_file(),
        format!("Trailgen binary is absent at {}", binary.display()),
    )?;
    Ok(binary)
}

fn world_from_coord([longitude, latitude]: [f64; 2]) -> [f64; 2] {
    let x = (longitude + 180.0) / 360.0;
    let latitude = latitude.clamp(-85.051_128_78, 85.051_128_78).to_radians();
    let y = (1.0 - latitude.tan().asinh() / std::f64::consts::PI) * 0.5;
    [x, y]
}
