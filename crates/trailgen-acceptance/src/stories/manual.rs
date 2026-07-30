use std::time::Duration;

use egui_tester::{Key, Modifiers, Result, demand};

use crate::harness::{
    Control, Harness, TrailFrame, TrailStory, durable_budget, first_anchor, read_json, verdict,
};
use crate::interactions::{add_support, exercise_profile};
use crate::observation::{EditorOrigin, RouteShape, View, shows};

const ROOT: &str = "/test/manual";
const SUPPORTS: [[f64; 2]; 5] = [
    [-105.0, 40.0],
    [-104.994, 40.0],
    [-104.988, 40.0],
    [-104.988, 40.012],
    [-105.0, 40.012],
];

pub fn run(harness: &Harness<'_>) -> Result<()> {
    harness.seed_project(ROOT, "/test/fixtures/mini_network.geojson", false)?;
    harness
        .testbed
        .retain_on_failure("manual/library/index.json")?;
    let app = harness.launch_gui(Some(ROOT), true, false)?;
    let mut story = harness.story(&app)?;
    let _ready = story.ready(Duration::from_secs(30))?;

    let (before_signature, trailhead) = draw_open_route(&mut story)?;
    close_reverse_and_save(&mut story, before_signature, trailhead)?;
    verify_saved(harness)?;

    if let Some(artifacts) = harness.artifacts {
        story
            .capture()?
            .save_png(artifacts.join("story-4-manual.png"))?;
    }
    app.terminate()?;
    drop(story);
    drop(app);
    verify_restart(harness)
}

fn draw_open_route(story: &mut TrailStory<'_, '_>) -> Result<(u64, [f64; 2])> {
    let _editor = story.click(Control::Manual)?.expect(
        shows::view(View::Edit) & shows::editor_origin(EditorOrigin::New) & shows::supports(0),
    )?;
    let _first = add_support(story, SUPPORTS[0], 1)?;
    let second = add_support(story, SUPPORTS[1], 2)?;
    demand(
        support(second.value(), 1).is_some_and(|point| near(point, SUPPORTS[1])),
        "manual editor could not retain a support in the middle of an edge",
    )?;
    let _undone = story
        .chord(Modifiers::CTRL, Key::Character('z'))?
        .expect(shows::supports(1) & shows::redoable())?;
    let _redone = story
        .chord(Modifiers::CTRL, Key::Character('y'))?
        .expect(shows::supports(2) & shows::support(1, SUPPORTS[1]))?;
    for (slot, coordinate) in SUPPORTS.iter().copied().enumerate().skip(2) {
        let _added = add_support(story, coordinate, slot + 1)?;
    }
    let before_reverse = story.frame()?;
    let before_signature = signature(&before_reverse)
        .ok_or_else(|| verdict("ready manual route omitted its signature"))?;
    let trailhead =
        support(&before_reverse, 0).ok_or_else(|| verdict("manual route omitted support 0"))?;
    Ok((before_signature, trailhead))
}

fn close_reverse_and_save(
    story: &mut TrailStory<'_, '_>,
    before_signature: u64,
    trailhead: [f64; 2],
) -> Result<()> {
    let _closed = story.click(Control::CloseLoop)?.expect(
        shows::shape(RouteShape::Loop) & shows::editor_ready() & shows::support(0, trailhead),
    )?;
    let closed = story.frame()?;
    let closed_signature =
        signature(&closed).ok_or_else(|| verdict("closed loop omitted its route signature"))?;
    let _reversed = story.click(Control::Reverse)?.expect(
        shows::shape(RouteShape::Loop)
            & shows::editor_ready()
            & shows::changed_signature(closed_signature)
            & shows::support(0, trailhead),
    )?;
    demand(
        before_signature != closed_signature,
        "closing the manual route did not alter its realized walk",
    )?;

    exercise_profile(story)?;
    let _saved = story
        .click(Control::EditorSave)?
        .within(durable_budget())
        .expect(shows::view(View::FocusSaved) & shows::library(1))?;
    Ok(())
}

fn verify_saved(harness: &Harness<'_>) -> Result<()> {
    let library = read_json(&harness.testbed.private_path("manual/library/index.json")?)?;
    let trails = library["trails"]
        .as_array()
        .ok_or_else(|| verdict("manual Library omitted its trails"))?;
    let [trail] = trails.as_slice() else {
        return Err(verdict("manual story must save exactly one trail"));
    };
    demand(
        trail["metrics"]["shape"] == "loop",
        "saved manual trail is not a loop",
    )?;
    demand(
        trail["support_points"]
            .as_array()
            .is_some_and(|points| points.len() >= SUPPORTS.len()),
        "saved manual loop discarded its support design",
    )
}

fn verify_restart(harness: &Harness<'_>) -> Result<()> {
    let restarted = harness.launch_gui(Some(ROOT), true, false)?;
    let mut story = harness.story(&restarted)?;
    let restored = story.wait_within(Duration::from_secs(30), shows::library(1))?;
    let _trail = first_anchor(
        &restored,
        "library.trail/",
        "restarted Library omitted its manual loop",
    )?;
    restarted.terminate()
}

fn support(frame: &TrailFrame, slot: usize) -> Option<[f64; 2]> {
    frame
        .state
        .editor
        .as_ref()?
        .support_points
        .get(slot)
        .copied()
}

fn signature(frame: &TrailFrame) -> Option<u64> {
    frame.state.editor.as_ref()?.route_signature
}

fn near(left: [f64; 2], right: [f64; 2]) -> bool {
    (left[0] - right[0]).abs() <= 5.0e-5 && (left[1] - right[1]).abs() <= 5.0e-5
}
