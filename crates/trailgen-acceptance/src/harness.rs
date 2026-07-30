use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use egui_tester::{
    Anchor, AppCommand, Application, Button, Error, Graphics, JsonProbe, Network,
    PerformanceBudget, ProbeFrame, Result, Testbed, Timed, WindowQuery, X11Session,
};
use num_traits::ToPrimitive as _;
use serde::Deserialize;
use serde_json::Value;

use crate::fixture::FixtureWorld;

pub const TITLE: &str = "trailgen · trail workbench";

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

    pub fn seed_project(&self, root: &str, source: &str, route: bool) -> Result<()> {
        self.run_cli(&["init", root, "--name", "Acceptance"])?;
        self.run_cli(&["build", root, "--source", source])?;
        if route {
            self.run_cli(&[
                "generate",
                root,
                "--start=-105.0,40.0",
                "--min-km",
                "0",
                "--max-km",
                "10",
                "--count",
                "1",
                "--solver",
                "exact",
            ])?;
        }
        Ok(())
    }

    pub fn run_cli(&self, args: &[&str]) -> Result<()> {
        let command = args.join(" ");
        let app = self.testbed.launch(
            AppCommand::new(self.binary)
                .args(args.iter().copied())
                .runtime(Duration::from_secs(45)),
        )?;
        let exit = app.wait(Duration::from_secs(45))?;
        app.terminate()?;
        if exit.success() {
            Ok(())
        } else {
            Err(Error::Command {
                command,
                status: format!("code {}, result {}", exit.code, exit.result),
                stderr: exit.stderr,
            })
        }
    }

    pub fn gui(&self, project: Option<&str>, offline: bool, online: bool) -> AppCommand {
        let mut args = vec!["gui"];
        if let Some(project) = project {
            args.push(project);
        }
        if offline {
            args.push("--offline");
        }
        let command = AppCommand::new(self.binary)
            .args(args)
            .env("TRAILGEN_BASEMAP_ARCHIVE", "/test/fixtures/empty.pmtiles")
            .graphics(Graphics::Host)
            .network(if online { Network::Host } else { Network::Deny })
            .witness("probes/trailgen.json")
            .runtime(Duration::from_mins(3));
        if online {
            self.fixtures.online(command)
        } else {
            command
        }
    }

    pub fn session<'app>(&'a self, app: &'app Application<'a>) -> Result<X11Session<'app, 'a>> {
        let session = self.testbed.x11_session(
            app,
            WindowQuery::title_exact(TITLE),
            Duration::from_secs(30),
        )?;
        session.focus()?;
        Ok(session)
    }
}

pub fn click_budgeted(
    session: &X11Session<'_, '_>,
    probe: &mut JsonProbe,
    anchor: &Anchor,
    budget: PerformanceBudget,
    description: &'static str,
    predicate: impl FnMut(&ProbeFrame) -> bool,
) -> Result<Timed<ProbeFrame>> {
    let (x, y) = anchor.center();
    let receipt = session.click(x, y, Button::Primary)?;
    probe.wait_budgeted(
        session.application(),
        &receipt,
        budget,
        description,
        predicate,
    )
}

pub fn click_named(
    session: &X11Session<'_, '_>,
    probe: &mut JsonProbe,
    name: &str,
    budget: PerformanceBudget,
    description: &'static str,
    predicate: impl FnMut(&ProbeFrame) -> bool,
) -> Result<Timed<ProbeFrame>> {
    let anchor = probe.wait_anchor(session.application(), name, Duration::from_secs(8))?;
    click_budgeted(session, probe, &anchor, budget, description, predicate)
}

pub fn replace_text(
    session: &X11Session<'_, '_>,
    probe: &mut JsonProbe,
    anchor: &Anchor,
    text: &str,
) -> Result<egui_tester::ActionReceipt> {
    let (x, y) = anchor.center();
    let focus = session.click(x, y, Button::Primary)?;
    let _focused = probe.wait_budgeted(
        session.application(),
        &focus,
        instant_budget(),
        "focus a text control before typing",
        |frame| state_flag(frame, "text_edit_focused"),
    )?;
    let _select = session.chord(
        egui_tester::Modifiers::CTRL,
        egui_tester::Key::Character('a'),
    )?;
    session.type_text(text)
}

pub fn map_pixel(frame: &ProbeFrame, coordinate: [f64; 2]) -> Result<(i16, i16)> {
    let state = decode_state(frame)?;
    let map = state
        .map
        .as_ref()
        .ok_or_else(|| verdict("witness omitted map transform"))?;
    let ppp = f64::from(
        frame
            .ppp
            .ok_or_else(|| verdict("sealed witness omitted pixels per point"))?,
    );
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

pub fn decode_state(frame: &ProbeFrame) -> Result<WitnessState> {
    serde_json::from_value(frame.state.clone()).map_err(|error| Error::Probe {
        path: PathBuf::from("<trailgen-witness>"),
        detail: error.to_string(),
    })
}

pub fn read_json(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).map_err(|source| Error::Io {
        operation: "read acceptance oracle",
        path: path.to_owned(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|error| Error::Probe {
        path: path.to_owned(),
        detail: error.to_string(),
    })
}

pub fn state_is(frame: &ProbeFrame, key: &str, expected: &str) -> bool {
    frame.state[key].as_str() == Some(expected)
}

pub fn state_flag(frame: &ProbeFrame, key: &str) -> bool {
    frame.state[key].as_bool() == Some(true)
}

pub fn instant_budget() -> PerformanceBudget {
    PerformanceBudget::new(Duration::from_millis(300))
        .through_presentation()
        .timeout(Duration::from_secs(6))
}

pub fn durable_budget() -> PerformanceBudget {
    PerformanceBudget::new(Duration::from_millis(650))
        .through_presentation()
        .timeout(Duration::from_secs(10))
}

pub fn search_reaction_budget() -> PerformanceBudget {
    PerformanceBudget::new(Duration::from_millis(450))
        .through_presentation()
        .timeout(Duration::from_secs(8))
}

pub fn demand(condition: bool, detail: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(verdict_owned(detail.into()))
    }
}

pub fn verdict(detail: &'static str) -> Error {
    verdict_owned(detail.to_owned())
}

pub const fn verdict_owned(detail: String) -> Error {
    Error::X11 {
        operation: "adjudicate Trailgen user story",
        detail,
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

#[derive(Deserialize)]
pub struct WitnessState {
    pub map: Option<MapState>,
    pub editor: Option<EditorState>,
}

#[derive(Debug, Deserialize)]
pub struct MapState {
    pub rect: [f32; 4],
    pub center: [f64; 2],
    pub world_points: f64,
}

#[derive(Deserialize)]
pub struct EditorState {
    pub support_points: Vec<[f64; 2]>,
    pub route_signature: Option<u64>,
}
