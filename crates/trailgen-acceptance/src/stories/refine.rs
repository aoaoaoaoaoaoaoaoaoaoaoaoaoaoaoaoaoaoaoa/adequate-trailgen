use std::{path::Path, time::Duration};

use egui_tester::{Key, Modifiers, Result, Testbed, Timed, WindowQuery, demand};
use serde_json::Value;

use crate::harness::{
    DataMode, Harness, RunClass, Target, TargetClass, TrailFrame, TrailStory, first_anchor,
    read_json, verdict,
};
use crate::interactions::drag_support;
use crate::observation::{View, shows};

const ROOT: &str = "/test/refine";
const INDEX: &str = "refine/library/index.json";
const RENAMED: &str = "Acceptance Ridge";
const EDITOR_RENAMED: &str = "Acceptance Ridge Refined";
const TARGET: [f64; 2] = [-105.0, 40.012];

pub fn run(harness: &Harness<'_>) -> Result<()> {
    harness.seed_project(ROOT, "/test/fixtures/mini_network.geojson", true)?;
    harness
        .testbed
        .retain_on_failure("refine/library/index.json")?;
    let app = harness.launch_gui(Some(ROOT), DataMode::Offline, RunClass::Functional)?;
    let mut story = harness.story(&app, RunClass::Functional)?;

    open_saved(&mut story)?;
    rename(&mut story, harness.testbed)?;
    let baseline = harness.testbed.read_private(INDEX)?;
    discard_refinement(&mut story, harness.testbed, &baseline)?;
    save_refinement(&mut story, harness.testbed, &baseline)?;
    reject_unconfirmed_delete(&mut story, harness.artifacts)?;

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

fn discard_refinement(
    story: &mut TrailStory<'_, '_>,
    testbed: &Testbed,
    baseline: &[u8],
) -> Result<()> {
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
        testbed
            .read_private(INDEX)
            .is_ok_and(|bytes| bytes == baseline),
        "dragging mutated the Library before Save",
    )?;
    let _undone = story.chord(Modifiers::CTRL, Key::Character('z'))?.until(
        shows::signature(before_signature) & shows::support(1, before_support) & shows::redoable(),
    )?;
    let redone = story
        .chord(Modifiers::CTRL, Key::Character('y'))?
        .until(shows::changed_signature(before_signature) & shows::support(1, TARGET))?;
    demand(
        redone
            .value()
            .state
            .profile
            .as_ref()
            .is_some_and(|profile| profile.visible),
        "redo discarded the editor elevation profile",
    )?;
    let escaped = story.key(Key::Escape)?.next_frame()?.into_value();
    demand(
        escaped.state.view == View::Edit,
        "Escape discarded an unfinished saved-trail refinement",
    )?;
    let _discarded = story
        .chord(Modifiers::ALT, Key::Delete)?
        .until(shows::view(View::FocusSaved))?;
    demand(
        testbed
            .read_private(INDEX)
            .is_ok_and(|bytes| bytes == baseline),
        "Discard persisted an unsaved refinement",
    )
}

fn reject_unconfirmed_delete(
    story: &mut TrailStory<'_, '_>,
    artifacts: Option<&Path>,
) -> Result<()> {
    let armed = story.click(Target::FocusDelete)?.next_frame()?.into_value();
    demand(
        armed.state.view == View::FocusSaved
            && armed.state.saved_trails == 1
            && armed
                .anchor(&Target::FocusDeleteConfirm.to_string())
                .is_some(),
        "one delete click removed a saved trail without confirmation",
    )?;
    if let Some(artifacts) = artifacts {
        story
            .capture()?
            .save_png(artifacts.join("story-2-delete-confirmation.png"))?;
    }
    let cancelled = story
        .click(Target::FocusDeleteCancel)?
        .next_frame()?
        .into_value();
    demand(
        cancelled.state.view == View::FocusSaved
            && cancelled.state.saved_trails == 1
            && cancelled
                .anchor(&Target::FocusDeleteConfirm.to_string())
                .is_none(),
        "cancelling saved-trail deletion did not restore its focused controls",
    )
}

fn save_refinement(
    story: &mut TrailStory<'_, '_>,
    testbed: &Testbed,
    baseline: &[u8],
) -> Result<()> {
    let baseline = serde_json::from_slice::<Value>(baseline)
        .map_err(|error| verdict(format!("decode refinement baseline: {error}")))?;
    let baseline_legs = only_trail(&baseline)?["legs"].clone();
    let editor = enter_editor(story)?;
    let before_signature = signature(editor.value())
        .ok_or_else(|| verdict("reopened editor omitted its route signature"))?;
    let support_count = editor
        .value()
        .state
        .editor
        .as_ref()
        .map_or(0, |editor| editor.support_points.len());
    let _rename = story
        .key(Key::Function(3))?
        .until(shows::view(View::Edit) & shows::rename(true) & shows::text_focused())?;
    let _typed = story
        .replace_text(
            Target::EditorRenameField,
            EDITOR_RENAMED,
            shows::text_focused(),
        )?
        .next_frame()?;
    let _committed = story.key(Key::Return)?.until(
        shows::view(View::Edit)
            & shows::rename(false)
            & shows::signature(before_signature)
            & shows::supports(support_count),
    )?;
    demand(
        only_trail(&read_json(testbed, INDEX)?)?["name"] == RENAMED,
        "renaming inside the editor mutated the Library before Save",
    )?;
    let _dragged = drag_support(story, editor.value(), 1, TARGET, before_signature)?;
    let _saved = story
        .click(Target::EditorSave)?
        .until(shows::view(View::FocusSaved))?;
    let durable = read_json(testbed, INDEX)?;
    let trail = only_trail(&durable)?;
    demand(
        trail["name"] == EDITOR_RENAMED,
        "refinement discarded the trail name",
    )?;
    demand(
        support_json(trail, 1).is_some_and(|point| near(point, TARGET)),
        "saved refinement omitted the dragged support point",
    )?;
    demand(
        trail["legs"] != baseline_legs,
        "saved support drag did not alter durable route geometry",
    )
}

fn verify_restart(harness: &Harness<'_>) -> Result<()> {
    let restarted = harness.launch_gui(Some(ROOT), DataMode::Offline, RunClass::Functional)?;
    let mut story = harness.story(&restarted, RunClass::Functional)?;
    let restored = story.wait_within(Duration::from_secs(30), shows::library(1))?;
    let _trail = first_anchor(
        &restored,
        TargetClass::LibraryTrail,
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
    let saved = first_anchor(
        &library,
        TargetClass::LibraryTrail,
        "saved Library row vanished",
    )?;
    let _focused = story
        .click_anchor(&saved)?
        .until(shows::view(View::FocusSaved))?;
    Ok(())
}

fn rename(story: &mut TrailStory<'_, '_>, testbed: &Testbed) -> Result<()> {
    let _opened = story
        .click(Target::FocusRename)?
        .until(shows::rename(true) & shows::text_focused())?;
    let _typed = story
        .replace_text(Target::RenameField, RENAMED, shows::text_focused())?
        .next_frame()?;
    let _renamed = story
        .key(Key::Return)?
        .until(shows::view(View::FocusSaved) & shows::rename(false))?;
    demand(
        only_trail(&read_json(testbed, INDEX)?)?["name"] == RENAMED,
        "rename witness advanced before durable state",
    )?;
    let window = testbed.x11()?.wait_window_query(
        story.session().application(),
        WindowQuery::title_contains(RENAMED),
        Duration::from_secs(2),
    )?;
    demand(
        window.title() == format!("{RENAMED} · Acceptance · trailgen"),
        format!(
            "renamed trail did not reach the native window title: {:?}",
            window.title()
        ),
    )
}

fn enter_editor(story: &mut TrailStory<'_, '_>) -> Result<Timed<TrailFrame>> {
    story.key(Key::Character('e'))?.until(
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
