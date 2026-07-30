use std::{fs, path::Path, time::Duration};

use egui_tester::{Key, Modifiers, Result, Timed, demand};
use serde_json::Value;

use crate::harness::{
    Control, Harness, TrailFrame, TrailStory, durable_budget, first_anchor, read_json, verdict,
};
use crate::interactions::drag_support;
use crate::observation::{View, shows};

const ROOT: &str = "/test/refine";
const RENAMED: &str = "Acceptance Ridge";
const TARGET: [f64; 2] = [-105.0, 40.012];

pub fn run(harness: &Harness<'_>) -> Result<()> {
    harness.seed_project(ROOT, "/test/fixtures/mini_network.geojson", true)?;
    harness
        .testbed
        .retain_on_failure("refine/library/index.json")?;
    let index = harness.testbed.private_path("refine/library/index.json")?;
    let app = harness.launch_gui(Some(ROOT), true, false)?;
    let mut story = harness.story(&app)?;
    let _ready = story.ready(Duration::from_secs(30))?;

    open_saved(&mut story)?;
    rename(&mut story, &index)?;
    let baseline = fs::read(&index).map_err(|source| egui_tester::Error::Io {
        operation: "read renamed trail baseline",
        path: index.clone(),
        source,
    })?;
    cancel_refinement(&mut story, &index, &baseline)?;
    save_refinement(&mut story, &index)?;

    if let Some(artifacts) = harness.artifacts {
        story
            .capture()?
            .save_png(artifacts.join("story-2-refine.png"))?;
    }
    app.terminate()?;
    drop(story);
    drop(app);
    verify_restart(harness)
}

fn cancel_refinement(story: &mut TrailStory<'_, '_>, index: &Path, baseline: &[u8]) -> Result<()> {
    let editor = enter_editor(story)?;
    let before = editor
        .value()
        .state
        .editor
        .as_ref()
        .ok_or_else(|| verdict("editor witness omitted its state"))?;
    let before_signature = before
        .route_signature
        .ok_or_else(|| verdict("ready editor omitted its route signature"))?;
    let before_support = *before
        .support_points
        .get(1)
        .ok_or_else(|| verdict("ready editor omitted support 1"))?;

    let _dragged = drag_support(story, editor.value(), 1, TARGET, before_signature)?;
    demand(
        fs::read(index).is_ok_and(|bytes| bytes == baseline),
        "dragging mutated the Library before Save",
    )?;
    let _undone = story.chord(Modifiers::CTRL, Key::Character('z'))?.expect(
        shows::signature(before_signature) & shows::support(1, before_support) & shows::redoable(),
    )?;
    let redone = story
        .chord(Modifiers::CTRL, Key::Character('y'))?
        .expect(shows::changed_signature(before_signature) & shows::support(1, TARGET))?;
    demand(
        redone
            .value()
            .state
            .profile
            .as_ref()
            .is_some_and(|profile| profile.visible),
        "redo discarded the editor elevation profile",
    )?;
    let _cancelled = story
        .key(Key::Escape)?
        .expect(shows::view(View::FocusSaved))?;
    demand(
        fs::read(index).is_ok_and(|bytes| bytes == baseline),
        "Cancel persisted an unsaved refinement",
    )
}

fn save_refinement(story: &mut TrailStory<'_, '_>, index: &Path) -> Result<()> {
    let editor = enter_editor(story)?;
    let before_signature = signature(editor.value())
        .ok_or_else(|| verdict("reopened editor omitted its route signature"))?;
    let _dragged = drag_support(story, editor.value(), 1, TARGET, before_signature)?;
    let _saved = story
        .click(Control::EditorSave)?
        .within(durable_budget())
        .expect(shows::view(View::FocusSaved))?;
    let durable = read_json(index)?;
    let trail = only_trail(&durable)?;
    demand(
        trail["name"] == RENAMED,
        "refinement discarded the trail name",
    )?;
    demand(
        support_json(trail, 1).is_some_and(|point| near(point, TARGET)),
        "saved refinement omitted the dragged support point",
    )
}

fn verify_restart(harness: &Harness<'_>) -> Result<()> {
    let restarted = harness.launch_gui(Some(ROOT), true, false)?;
    let mut story = harness.story(&restarted)?;
    let restored = story.wait_within(Duration::from_secs(30), shows::library(1))?;
    let _trail = first_anchor(
        &restored,
        "library.trail/",
        "restarted Library omitted its saved trail",
    )?;
    demand(
        restored.state.view == View::Browse,
        "restart resurrected transient focus or editor state",
    )?;
    restarted.terminate()
}

fn open_saved(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let library = story.wait_within(Duration::from_secs(15), shows::library(1))?;
    let saved = first_anchor(&library, "library.trail/", "saved Library row vanished")?;
    let _focused = story
        .click_anchor(&saved)?
        .expect(shows::view(View::FocusSaved))?;
    Ok(())
}

fn rename(story: &mut TrailStory<'_, '_>, index: &Path) -> Result<()> {
    let _opened = story
        .click(Control::FocusRename)?
        .expect(shows::rename(true) & shows::text_focused())?;
    let _typed = story
        .replace_text(Control::RenameField, RENAMED, shows::text_focused())?
        .presented()?;
    let _renamed = story
        .key(Key::Return)?
        .within(durable_budget())
        .expect(shows::view(View::FocusSaved) & shows::rename(false))?;
    demand(
        only_trail(&read_json(index)?)?["name"] == RENAMED,
        "rename witness advanced before durable state",
    )
}

fn enter_editor(story: &mut TrailStory<'_, '_>) -> Result<Timed<TrailFrame>> {
    story.click(Control::FocusEdit)?.expect(
        shows::view(View::Edit)
            & shows::editor_ready()
            & shows::supports_at_least(2)
            & shows::profile_visible(),
    )
}

fn signature(frame: &TrailFrame) -> Option<u64> {
    frame.state.editor.as_ref()?.route_signature
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

fn support_json(trail: &Value, slot: usize) -> Option<[f64; 2]> {
    let support = trail["support_points"].as_array()?.get(slot)?;
    Some([support["lon"].as_f64()?, support["lat"].as_f64()?])
}

fn near(left: [f64; 2], right: [f64; 2]) -> bool {
    (left[0] - right[0]).abs() <= 5.0e-5 && (left[1] - right[1]).abs() <= 5.0e-5
}
