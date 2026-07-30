use std::{fs, path::Path, time::Duration};

use egui_tester::{Button, Key, Modifiers, PerformanceBudget, ProbeFrame, Result, Timed};
use serde_json::Value;

use crate::harness::{
    Harness, click_budgeted, click_named, decode_state, demand, durable_budget, instant_budget,
    map_pixel, read_json, replace_text, state_flag, state_is, verdict,
};

const ROOT: &str = "/test/refine";
const RENAMED: &str = "Acceptance Ridge";
const TARGET: [f64; 2] = [-105.0, 40.012];

pub fn run(harness: &Harness<'_>) -> Result<()> {
    harness.seed_project(ROOT, "/test/fixtures/mini_network.geojson", true)?;
    harness
        .testbed
        .retain_on_failure("refine/library/index.json")?;
    let index = harness.testbed.private_path("refine/library/index.json")?;
    let app = harness
        .testbed
        .launch(harness.gui(Some(ROOT), true, false))?;
    let session = harness.session(&app)?;
    let mut probe = app.witness()?;
    let _first = session.wait_presented(&mut probe, Duration::from_secs(30))?;

    open_saved(&session, &mut probe)?;
    rename(&session, &mut probe, &index)?;
    let baseline = fs::read(&index).map_err(|source| egui_tester::Error::Io {
        operation: "read renamed trail baseline",
        path: index.clone(),
        source,
    })?;

    cancel_refinement(&session, &mut probe, &index, &baseline)?;
    save_refinement(&session, &mut probe, &index)?;

    if let Some(artifacts) = harness.artifacts {
        session
            .capture()?
            .save_png(artifacts.join("story-2-refine.png"))?;
    }
    app.terminate()?;
    drop(session);
    drop(app);
    verify_restart(harness)
}

fn cancel_refinement(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
    index: &Path,
    baseline: &[u8],
) -> Result<()> {
    let app = session.application();
    let editor = enter_editor(session, probe)?;
    let before = decode_state(editor.value())?;
    let before_editor = before
        .editor
        .as_ref()
        .ok_or_else(|| verdict("editor witness omitted its state"))?;
    let before_signature = before_editor
        .route_signature
        .ok_or_else(|| verdict("ready editor omitted its route signature"))?;
    let before_support = *before_editor
        .support_points
        .get(1)
        .ok_or_else(|| verdict("ready editor omitted support 1"))?;

    let _dragged = drag_pin(session, probe, editor.value(), before_signature)?;
    demand(
        fs::read(index).is_ok_and(|bytes| bytes == baseline),
        "dragging mutated the Library before Save",
    )?;

    let undo = session.chord(Modifiers::CTRL, Key::Character('z'))?;
    let _undone =
        probe.wait_budgeted(app, &undo, instant_budget(), "undo a pin drag", |frame| {
            editor_signature(frame) == Some(before_signature)
                && editor_support(frame, 1).is_some_and(|point| near(point, before_support))
                && frame.state["editor"]["redo_depth"]
                    .as_u64()
                    .is_some_and(|depth| depth > 0)
        })?;
    let redo = session.chord(Modifiers::CTRL, Key::Character('y'))?;
    let redone = probe.wait_budgeted(app, &redo, instant_budget(), "redo a pin drag", |frame| {
        editor_signature(frame).is_some_and(|signature| signature != before_signature)
            && editor_support(frame, 1).is_some_and(|point| near(point, TARGET))
    })?;
    demand(
        redone.value().state["profile"]["visible"] == true,
        "redo discarded the editor elevation profile",
    )?;

    let cancel = session.key(Key::Escape)?;
    let _cancelled = probe.wait_budgeted(
        app,
        &cancel,
        instant_budget(),
        "cancel an unsaved refinement",
        |frame| state_is(frame, "view", "focus-saved"),
    )?;
    demand(
        fs::read(index).is_ok_and(|bytes| bytes == baseline),
        "Cancel persisted an unsaved refinement",
    )
}

fn save_refinement(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
    index: &Path,
) -> Result<()> {
    let editor = enter_editor(session, probe)?;
    let before_signature = editor_signature(editor.value())
        .ok_or_else(|| verdict("reopened editor omitted its route signature"))?;
    let dragged = drag_pin(session, probe, editor.value(), before_signature)?;
    let save = dragged
        .value()
        .anchor("editor.save")
        .cloned()
        .ok_or_else(|| verdict("reforged editor omitted Save"))?;
    let _saved = click_budgeted(
        session,
        probe,
        &save,
        durable_budget(),
        "save a refined trail",
        |frame| state_is(frame, "view", "focus-saved"),
    )?;
    let durable = read_json(index)?;
    let trail = only_trail(&durable)?;
    demand(
        trail["name"] == RENAMED,
        "refinement discarded the trail name",
    )?;
    demand(
        support(trail, 1).is_some_and(|point| near(point, TARGET)),
        "saved refinement omitted the dragged support point",
    )
}

fn verify_restart(harness: &Harness<'_>) -> Result<()> {
    let restarted = harness
        .testbed
        .launch(harness.gui(Some(ROOT), true, false))?;
    let _session = harness.session(&restarted)?;
    let mut probe = restarted.witness()?;
    let restored = probe.wait(
        &restarted,
        Duration::from_secs(30),
        "refined trail after process restart",
        |frame| {
            frame.state["saved_trails"] == 1
                && frame
                    .anchors
                    .iter()
                    .any(|anchor| anchor.name.starts_with("library.trail/"))
        },
    )?;
    demand(
        restored.state["view"] == "browse",
        "restart resurrected transient focus or editor state",
    )?;
    restarted.terminate()?;
    Ok(())
}

fn open_saved(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
) -> Result<()> {
    let app = session.application();
    let library = probe.wait(
        app,
        Duration::from_secs(15),
        "seeded route in the saved Library",
        |frame| {
            frame
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
        .ok_or_else(|| verdict("saved Library row vanished"))?;
    let _focused = click_budgeted(
        session,
        probe,
        &saved,
        instant_budget(),
        "focus a saved trail",
        |frame| state_is(frame, "view", "focus-saved"),
    )?;
    Ok(())
}

fn rename(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
    index: &Path,
) -> Result<()> {
    let _opened = click_named(
        session,
        probe,
        "focus.rename",
        instant_budget(),
        "open the rename transaction",
        |frame| {
            state_flag(frame, "rename_active")
                && state_flag(frame, "text_edit_focused")
                && frame.anchor("focus.rename.field").is_some()
        },
    )?;
    let field = probe.wait_anchor(
        session.application(),
        "focus.rename.field",
        Duration::from_secs(5),
    )?;
    let _typed = replace_text(session, probe, &field, RENAMED)?;
    let commit = session.key(Key::Return)?;
    let _renamed = probe.wait_budgeted(
        session.application(),
        &commit,
        durable_budget(),
        "commit a saved-trail rename",
        |frame| !state_flag(frame, "rename_active") && state_is(frame, "view", "focus-saved"),
    )?;
    demand(
        only_trail(&read_json(index)?)?["name"] == RENAMED,
        "rename witness advanced before durable state",
    )
}

fn enter_editor(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
) -> Result<Timed<ProbeFrame>> {
    click_named(
        session,
        probe,
        "focus.edit",
        instant_budget(),
        "enter the saved-trail editor",
        |frame| {
            state_is(frame, "view", "edit")
                && frame.state["editor"]["ready"] == true
                && frame.anchor("editor.support/1").is_some()
                && frame.state["profile"]["visible"] == true
        },
    )
}

fn drag_pin(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
    editor: &ProbeFrame,
    before_signature: u64,
) -> Result<Timed<ProbeFrame>> {
    let app = session.application();
    let state = decode_state(editor)?;
    let editor_state = state
        .editor
        .as_ref()
        .ok_or_else(|| verdict("editor witness omitted editor state"))?;
    let pin = editor
        .anchor("editor.support/1")
        .cloned()
        .ok_or_else(|| verdict("editor omitted draggable support 1"))?;
    let current = map_pixel(
        editor,
        *editor_state
            .support_points
            .get(1)
            .ok_or_else(|| verdict("editor omitted support 1 coordinate"))?,
    )?;
    let target = map_pixel(editor, TARGET)?;
    let grip = pin.center();
    let destination = (
        target.0.saturating_add(grip.0.saturating_sub(current.0)),
        target.1.saturating_add(grip.1.saturating_sub(current.1)),
    );
    let press = session.button_down(grip.0, grip.1, Button::Primary)?;
    let _acquired = probe.wait_budgeted(
        app,
        &press,
        instant_budget(),
        "acquire support 1 for dragging",
        |frame| frame.state["editor"]["dragging_support"] == 1,
    )?;
    let motion = session.move_to(destination.0, destination.1)?;
    let reforged = probe.wait_budgeted(
        app,
        &motion,
        PerformanceBudget::new(Duration::from_millis(240))
            .through_presentation()
            .timeout(Duration::from_secs(6)),
        "drag a support and reforge its route",
        |frame| {
            frame.state["editor"]["ready"] == true
                && editor_signature(frame).is_some_and(|signature| signature != before_signature)
                && editor_support(frame, 1).is_some_and(|point| near(point, TARGET))
        },
    )?;
    let release = session.button_up(Button::Primary)?;
    let _released = probe.wait_budgeted(
        app,
        &release,
        instant_budget(),
        "release a dragged support",
        |frame| frame.state["editor"]["dragging_support"].is_null(),
    )?;
    Ok(reforged)
}

fn editor_signature(frame: &ProbeFrame) -> Option<u64> {
    frame.state["editor"]["route_signature"].as_u64()
}

fn editor_support(frame: &ProbeFrame, slot: usize) -> Option<[f64; 2]> {
    let point = frame.state["editor"]["support_points"]
        .as_array()?
        .get(slot)?;
    let point = point.as_array()?;
    Some([point.first()?.as_f64()?, point.get(1)?.as_f64()?])
}

fn only_trail(library: &Value) -> Result<&Value> {
    let trails = library["trails"]
        .as_array()
        .ok_or_else(|| verdict("Library omitted its trail array"))?;
    let [trail] = trails.as_slice() else {
        return Err(verdict("refinement Library must contain exactly one trail"));
    };
    Ok(trail)
}

fn support(trail: &Value, slot: usize) -> Option<[f64; 2]> {
    let support = trail["support_points"].as_array()?.get(slot)?;
    Some([support["lon"].as_f64()?, support["lat"].as_f64()?])
}

fn near(left: [f64; 2], right: [f64; 2]) -> bool {
    (left[0] - right[0]).abs() <= 5.0e-5 && (left[1] - right[1]).abs() <= 5.0e-5
}
