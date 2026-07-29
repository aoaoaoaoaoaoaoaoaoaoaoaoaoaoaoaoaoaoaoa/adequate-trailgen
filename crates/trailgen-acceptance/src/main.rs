use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use egui_tester::{
    Anchor, AppCommand, Button, Error, Graphics, JsonProbe, Key, Modifiers, PerformanceBudget,
    ProbeFrame, Result, Testbed, TestbedBuilder, Timed, WindowQuery, X11Session,
};
use num_traits::ToPrimitive as _;
use serde::Deserialize;
use serde_json::Value;

const TITLE: &str = "trailgen · trail workbench";
const TARGET: [f64; 2] = [-105.0, 40.012];
const RENAMED: &str = "Acceptance Ridge";

fn main() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("acceptance crate must remain in the Trailgen workspace");
    let binary = match env::var_os("TRAILGEN_ACCEPTANCE_BINARY") {
        Some(binary) => PathBuf::from(binary),
        None => sibling_binary()?,
    };
    let artifacts = env::args_os()
        .nth(1)
        .or_else(|| env::var_os("TRAILGEN_ACCEPTANCE_ARTIFACTS"))
        .map(PathBuf::from);
    let mut builder = TestbedBuilder::default();
    if let Some(artifacts) = &artifacts {
        builder = builder.failure_artifacts(artifacts);
    }
    builder.run(|testbed| dogfood(testbed, &binary, root, artifacts.as_deref()))
}

fn dogfood(testbed: &Testbed, binary: &Path, root: &Path, artifacts: Option<&Path>) -> Result<()> {
    seed_project(testbed, binary, root)?;
    testbed.retain_on_failure("project/library/index.json")?;
    testbed.retain_on_failure("project/routes")?;

    let app = testbed.launch(
        AppCommand::new(binary)
            .args(["gui", "/test/project", "--offline"])
            .graphics(Graphics::Host)
            .witness("probes/trailgen.json")
            .runtime(Duration::from_mins(2)),
    )?;
    let session = testbed.x11_session(
        &app,
        WindowQuery::title_exact(TITLE),
        Duration::from_secs(30),
    )?;
    session.focus()?;
    let mut probe = app.witness()?;
    let _first_pixels = session.wait_presented(&mut probe, Duration::from_secs(30))?;
    let selected = open_saved(&session, &mut probe)?;
    let index = testbed.private_path("project/library/index.json")?;
    let (rename_open, renamed) = rename_saved(&session, &mut probe, &index)?;
    let (editor, reforged, saved) = edit_saved(&session, &mut probe, &index)?;

    if let Some(artifacts) = artifacts {
        session
            .capture()?
            .save_png(artifacts.join("trailgen-acceptance.png"))?;
    }
    app.terminate()?;
    println!(
        "trailgen acceptance passed: select={:?}, rename-open={:?}, rename={:?}, edit={:?}, reforge={:?}, save={:?}",
        selected.elapsed(),
        rename_open.elapsed(),
        renamed.elapsed(),
        editor.elapsed(),
        reforged.elapsed(),
        saved.elapsed(),
    );
    Ok(())
}

fn open_saved(session: &X11Session<'_, '_>, probe: &mut JsonProbe) -> Result<Timed<ProbeFrame>> {
    let app = session.application();
    let library = probe.wait(
        app,
        Duration::from_secs(15),
        "a migrated saved trail to enter the library",
        |frame| {
            state_is(frame, "workspace", "trail")
                && frame
                    .anchors
                    .iter()
                    .any(|anchor| anchor.name.starts_with("library.trail/"))
        },
    )?;
    let saved = library
        .anchors
        .iter()
        .find(|anchor| anchor.name.starts_with("library.trail/"))
        .cloned()
        .ok_or_else(|| verdict("saved trail anchor vanished"))?;
    click_budgeted(
        session,
        probe,
        &saved,
        instant_budget(),
        "open a saved trail",
        |frame| {
            state_is(frame, "view", "focus-saved")
                && frame.anchor("focus.rename").is_some()
                && frame.anchor("editor.save").is_none()
        },
    )
}

fn rename_saved(
    session: &X11Session<'_, '_>,
    probe: &mut JsonProbe,
    index: &Path,
) -> Result<(Timed<ProbeFrame>, Timed<ProbeFrame>)> {
    let app = session.application();
    let rename = probe.wait_anchor(app, "focus.rename", Duration::from_secs(5))?;
    let rename_open = click_budgeted(
        session,
        probe,
        &rename,
        instant_budget(),
        "open the saved-trail rename field",
        |frame| {
            state_flag(frame, "rename_active")
                && state_flag(frame, "text_edit_focused")
                && frame.anchor("focus.rename.field").is_some()
        },
    )?;
    let _select_all = session.chord(Modifiers::CTRL, Key::Character('a'))?;
    let _typed_name = session.type_text(RENAMED)?;
    let commit = session.key(Key::Return)?;
    let renamed = probe.wait_budgeted(
        app,
        &commit,
        durable_budget(),
        "commit a saved-trail rename",
        |frame| {
            !state_flag(frame, "rename_active")
                && state_is(frame, "view", "focus-saved")
                && frame.anchor("focus.rename").is_some()
        },
    )?;
    demand(
        trail(&read_json(index)?)?["name"] == RENAMED,
        "rename witness advanced without durable library mutation",
    )?;
    Ok((rename_open, renamed))
}

fn edit_saved(
    session: &X11Session<'_, '_>,
    probe: &mut JsonProbe,
    index: &Path,
) -> Result<(Timed<ProbeFrame>, Timed<ProbeFrame>, Timed<ProbeFrame>)> {
    let app = session.application();
    let edit = probe.wait_anchor(app, "focus.edit", Duration::from_secs(5))?;
    let editor = click_budgeted(
        session,
        probe,
        &edit,
        instant_budget(),
        "enter the saved-trail editor",
        |frame| {
            state_is(frame, "view", "edit")
                && frame.state["editor"]["ready"] == true
                && frame.state["editor"]["support_points"]
                    .as_array()
                    .is_some_and(|points| points.len() >= 3)
                && frame.anchor("editor.support/1").is_some()
        },
    )?;
    let (reforged, before_legs) = drag_pin(session, probe, &editor, index)?;
    let saved = persist_drag(session, probe, &reforged, index, &before_legs)?;
    Ok((editor, reforged, saved))
}

fn drag_pin(
    session: &X11Session<'_, '_>,
    probe: &mut JsonProbe,
    editor: &Timed<ProbeFrame>,
    index: &Path,
) -> Result<(Timed<ProbeFrame>, Value)> {
    let app = session.application();
    let before = decode_state(editor.value())?;
    let before_editor = before
        .editor
        .as_ref()
        .ok_or_else(|| verdict("editor witness omitted editor state"))?;
    let before_signature = before_editor
        .route_signature
        .ok_or_else(|| verdict("ready editor omitted route signature"))?;
    let before_library = read_json(index)?;
    let before_legs = trail(&before_library)?["legs"].clone();
    let pin = editor
        .value()
        .anchor("editor.support/1")
        .cloned()
        .ok_or_else(|| verdict("editor omitted draggable pin 1"))?;
    let current = target_pixel(
        &before,
        editor.value(),
        *before_editor
            .support_points
            .get(1)
            .ok_or_else(|| verdict("ready editor omitted support point 1"))?,
    )?;
    let target = target_pixel(&before, editor.value(), TARGET)?;
    let grip = pin.center();
    let destination = (
        target.0.saturating_add(grip.0.saturating_sub(current.0)),
        target.1.saturating_add(grip.1.saturating_sub(current.1)),
    );
    let press = session.button_down(pin.center().0, pin.center().1, Button::Primary)?;
    let _acquired = probe.wait_budgeted(
        app,
        &press,
        instant_budget(),
        "acquire pin 1 for dragging",
        |frame| frame.state["editor"]["dragging_support"] == 1,
    )?;
    let drag = session.move_to(destination.0, destination.1)?;
    let reforged = probe.wait_budgeted(
        app,
        &drag,
        reforge_budget(),
        "drag pin and reforge its route",
        |frame| {
            let Some(editor) = decode_state(frame).ok().and_then(|state| state.editor) else {
                return false;
            };
            editor.ready
                && editor
                    .support_points
                    .get(1)
                    .is_some_and(|point| coordinate_near(*point, TARGET))
                && editor
                    .route_signature
                    .is_some_and(|signature| signature != before_signature)
                && frame
                    .anchor("editor.support/1")
                    .is_some_and(|anchor| pixel_near(anchor.center(), destination))
        },
    )?;
    let release = session.button_up(Button::Primary)?;
    let _released = probe.wait_budgeted(
        app,
        &release,
        instant_budget(),
        "release the dragged pin",
        |frame| {
            frame.state["editor"]["dragging_support"].is_null()
                && frame
                    .anchor("editor.support/1")
                    .is_some_and(|anchor| pixel_near(anchor.center(), destination))
        },
    )?;
    Ok((reforged, before_legs))
}

fn persist_drag(
    session: &X11Session<'_, '_>,
    probe: &mut JsonProbe,
    reforged: &Timed<ProbeFrame>,
    index: &Path,
    before_legs: &Value,
) -> Result<Timed<ProbeFrame>> {
    let save = reforged
        .value()
        .anchor("editor.save")
        .cloned()
        .ok_or_else(|| verdict("reforged editor omitted save control"))?;
    let saved = click_budgeted(
        session,
        probe,
        &save,
        durable_budget(),
        "persist a dragged trail",
        |frame| {
            state_is(frame, "view", "focus-saved")
                && frame.anchor("focus.rename").is_some()
                && frame.anchor("editor.save").is_none()
        },
    )?;
    let after_library = read_json(index)?;
    let after = trail(&after_library)?;
    demand(
        after["name"] == RENAMED,
        "editing discarded the saved trail name",
    )?;
    demand(
        support(after, 1).is_some_and(|point| coordinate_near(point, TARGET)),
        "dragged support point did not reach durable trail state",
    )?;
    demand(
        after["legs"] != *before_legs,
        "dragged trail retained its prior routed geometry",
    )?;
    Ok(saved)
}

fn seed_project(testbed: &Testbed, binary: &Path, root: &Path) -> Result<()> {
    let fixture = root.join("crates/trailgen-core/tests/fixtures/mini_network.geojson");
    let _fixture = testbed.copy_private("fixtures/mini_network.geojson", fixture)?;
    run_cli(
        testbed,
        binary,
        ["init", "/test/project", "--name", "Acceptance"],
    )?;
    run_cli(
        testbed,
        binary,
        [
            "build",
            "/test/project",
            "--source",
            "/test/fixtures/mini_network.geojson",
        ],
    )?;
    run_cli(
        testbed,
        binary,
        [
            "generate",
            "/test/project",
            "--start=-105.0,40.0",
            "--min-km",
            "0",
            "--max-km",
            "10",
            "--count",
            "1",
            "--solver",
            "exact",
        ],
    )
}

fn run_cli<const N: usize>(testbed: &Testbed, binary: &Path, args: [&str; N]) -> Result<()> {
    let command = args.join(" ");
    let app = testbed.launch(
        AppCommand::new(binary)
            .args(args)
            .runtime(Duration::from_secs(30)),
    )?;
    let exit = app.wait(Duration::from_secs(30))?;
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

fn click_budgeted(
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

#[derive(Deserialize)]
struct WitnessState {
    map: Option<MapState>,
    editor: Option<EditorState>,
}

#[derive(Deserialize)]
struct MapState {
    rect: [f32; 4],
    center: [f64; 2],
    world_points: f64,
}

#[derive(Deserialize)]
struct EditorState {
    ready: bool,
    support_points: Vec<[f64; 2]>,
    route_signature: Option<u64>,
}

fn decode_state(frame: &ProbeFrame) -> Result<WitnessState> {
    serde_json::from_value(frame.state.clone()).map_err(|error| Error::Probe {
        path: PathBuf::from("<trailgen-witness>"),
        detail: error.to_string(),
    })
}

fn target_pixel(
    state: &WitnessState,
    frame: &ProbeFrame,
    coordinate: [f64; 2],
) -> Result<(i16, i16)> {
    let map = state
        .map
        .as_ref()
        .ok_or_else(|| verdict("editor witness omitted map transform"))?;
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
    let x = (logical[0] * ppp)
        .round()
        .to_i16()
        .ok_or_else(|| verdict("map target exceeded X11 coordinate range"))?;
    let y = (logical[1] * ppp)
        .round()
        .to_i16()
        .ok_or_else(|| verdict("map target exceeded X11 coordinate range"))?;
    Ok((x, y))
}

fn world_from_coord([longitude, latitude]: [f64; 2]) -> [f64; 2] {
    let x = (longitude + 180.0) / 360.0;
    let latitude = latitude.clamp(-85.051_128_78, 85.051_128_78).to_radians();
    let y = (1.0 - latitude.tan().asinh() / std::f64::consts::PI) * 0.5;
    [x, y]
}

fn read_json(path: &Path) -> Result<Value> {
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

fn trail(library: &Value) -> Result<&Value> {
    let trails = library["trails"]
        .as_array()
        .ok_or_else(|| verdict("library omitted trail array"))?;
    if trails.len() != 1 {
        return Err(verdict("acceptance library must contain exactly one trail"));
    }
    Ok(&trails[0])
}

fn support(trail: &Value, slot: usize) -> Option<[f64; 2]> {
    let support = trail["support_points"].as_array()?.get(slot)?;
    Some([support["lon"].as_f64()?, support["lat"].as_f64()?])
}

fn coordinate_near(left: [f64; 2], right: [f64; 2]) -> bool {
    (left[0] - right[0]).abs() <= 5.0e-5 && (left[1] - right[1]).abs() <= 5.0e-5
}

const fn pixel_near(left: (i16, i16), right: (i16, i16)) -> bool {
    left.0.abs_diff(right.0) <= 3 && left.1.abs_diff(right.1) <= 3
}

fn state_is(frame: &ProbeFrame, key: &str, expected: &str) -> bool {
    frame.state[key].as_str() == Some(expected)
}

fn state_flag(frame: &ProbeFrame, key: &str) -> bool {
    frame.state[key].as_bool() == Some(true)
}

fn instant_budget() -> PerformanceBudget {
    PerformanceBudget::new(Duration::from_millis(250))
        .through_presentation()
        .timeout(Duration::from_secs(5))
}

fn durable_budget() -> PerformanceBudget {
    PerformanceBudget::new(Duration::from_millis(500))
        .through_presentation()
        .timeout(Duration::from_secs(8))
}

fn reforge_budget() -> PerformanceBudget {
    PerformanceBudget::new(Duration::from_millis(200))
        .through_presentation()
        .timeout(Duration::from_secs(5))
}

fn demand(condition: bool, detail: &'static str) -> Result<()> {
    condition.then_some(()).ok_or_else(|| verdict(detail))
}

fn verdict(detail: &'static str) -> Error {
    Error::X11 {
        operation: "adjudicate Trailgen acceptance",
        detail: detail.to_owned(),
    }
}

fn sibling_binary() -> Result<PathBuf> {
    let executable = env::current_exe().map_err(|source| Error::Io {
        operation: "locate acceptance executable",
        path: PathBuf::from("<current-exe>"),
        source,
    })?;
    Ok(executable.with_file_name("trailgen"))
}
