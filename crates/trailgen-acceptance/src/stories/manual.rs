use std::time::Duration;

use egui_tester::{Button, Key, Modifiers, PixelRegion, Result, demand};

use crate::harness::{
    DataMode, Harness, RunClass, Target, TargetClass, TrailFrame, TrailStory, first_anchor,
    read_json, verdict,
};
use crate::interactions::{
    add_support, delete_support, exercise_profile, exercise_support_delete_affordance,
};
use crate::observation::{EditorOrigin, RouteShape, View, shows};

const ROOT: &str = "/test/manual";
const SUPPORTS: [[f64; 2]; 5] = [
    [-104.984, 40.0],
    [-104.988, 40.0],
    [-104.988, 40.012],
    [-105.0, 40.012],
    [-105.0, 40.0],
];

pub fn run(harness: &Harness<'_>) -> Result<()> {
    harness.seed_project(ROOT, "/test/fixtures/mini_network.geojson", false)?;
    harness
        .testbed
        .retain_on_failure("manual/library/index.json")?;
    let (before_signature, trailhead) = {
        let app = harness.launch_gui(Some(ROOT), DataMode::Offline, RunClass::Functional)?;
        let mut story = harness.story(&app, RunClass::Functional)?;
        let design = draw_open_route(&mut story)?;
        let _persisted = story.wait_stable(
            Duration::from_secs(3),
            Duration::from_millis(650),
            "unfinished manual design to remain stable through autosave",
            |frame| {
                (frame.state.view == View::Edit
                    && frame
                        .state
                        .editor
                        .as_ref()
                        .is_some_and(|editor| editor.support_points.len() == SUPPORTS.len()))
                .then_some(())
            },
        )?;
        app.terminate()?;
        design
    };

    let app = harness.launch_gui(Some(ROOT), DataMode::Offline, RunClass::Functional)?;
    let mut story = harness.story(&app, RunClass::Functional)?;
    let restored = story.wait_within(
        Duration::from_secs(30),
        shows::view(View::Edit)
            & shows::editor_origin(EditorOrigin::New)
            & shows::supports(SUPPORTS.len())
            & shows::editor_ready(),
    )?;
    demand(
        signature(&restored) == Some(before_signature),
        "restarted manual editor realized a different unfinished route",
    )?;
    for (slot, expected) in SUPPORTS.iter().copied().enumerate() {
        demand(
            support(&restored, slot).is_some_and(|point| near(point, expected)),
            format!("restarted manual editor corrupted support {slot}"),
        )?;
    }
    close_reverse_and_save(&mut story, before_signature, trailhead)?;
    verify_export(&mut story, harness)?;
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

fn verify_export(story: &mut TrailStory<'_, '_>, harness: &Harness<'_>) -> Result<()> {
    let _exported = story
        .click(Target::SavedExport(0))?
        .until(shows::exported())?;
    let raw = harness
        .testbed
        .read_private_to_string("manual/exported.gpx")?;
    let route = trailgen_core::io::gpx::route_file_from_str(&raw)
        .map_err(|error| verdict(format!("saved export is not valid GPX: {error}")))?;
    demand(
        route.metadata.title.as_deref() == Some("manual trail"),
        "saved export lost the Library trail name",
    )?;
    demand(
        route.line.points.len() >= SUPPORTS.len(),
        "saved export lost the realized trail geometry",
    )
}

fn draw_open_route(story: &mut TrailStory<'_, '_>) -> Result<(u64, [f64; 2])> {
    let _dormant = story.wait(shows::results_open(false))?;
    let _manual = story.click(Target::Manual)?.until(
        shows::view(View::Edit) & shows::editor_origin(EditorOrigin::New) & shows::supports(0),
    )?;
    let _finder = story
        .click(Target::Finder)?
        .until(shows::view(View::Browse) & shows::results_open(false))?;
    let _editor = story.click(Target::Manual)?.until(
        shows::view(View::Edit) & shows::editor_origin(EditorOrigin::New) & shows::supports(0),
    )?;
    let first = add_support(story, SUPPORTS[0], 1)?;
    demand(
        support(first.value(), 0).is_some_and(|point| near(point, SUPPORTS[0])),
        "manual editor could not retain its trailhead in the middle of an edge",
    )?;
    let _second = add_support(story, SUPPORTS[1], 2)?;
    let _undone = story
        .chord(Modifiers::CTRL, Key::Character('z'))?
        .until(shows::supports(1) & shows::redoable())?;
    let _redone = story
        .chord(Modifiers::CTRL, Key::Character('y'))?
        .until(shows::supports(2) & shows::support(1, SUPPORTS[1]) & shows::editor_ready())?;
    for (slot, coordinate) in SUPPORTS.iter().copied().enumerate().skip(2) {
        let _added = add_support(story, coordinate, slot + 1)?;
    }
    exercise_support_callout(story, 0)?;
    exercise_support_delete_affordance(story, 2, SUPPORTS.len())?;
    let _deleted = delete_support(story, 2, SUPPORTS.len() - 1)?;
    let renumbered = story.frame()?;
    demand(
        support(&renumbered, 2).is_some_and(|point| near(point, SUPPORTS[3])),
        "deleting support 2 did not renumber its successor into slot 2",
    )?;
    let _restored = story.chord(Modifiers::CTRL, Key::Character('z'))?.until(
        shows::supports(SUPPORTS.len()) & shows::support(2, SUPPORTS[2]) & shows::editor_ready(),
    )?;
    let before_reverse = story.frame()?;
    let before_signature = signature(&before_reverse)
        .ok_or_else(|| verdict("ready manual route omitted its signature"))?;
    let trailhead =
        support(&before_reverse, 0).ok_or_else(|| verdict("manual route omitted support 0"))?;
    Ok((before_signature, trailhead))
}

fn exercise_support_callout(story: &mut TrailStory<'_, '_>, slot: usize) -> Result<()> {
    let bare = story.capture()?;
    let _shown = story
        .modified_click(Target::Support(slot), Button::Primary, Modifiers::ALT)?
        .until(shows::support_callout(slot, true))?;
    let plate = PixelRegion::anchor(&story.anchor(Target::SupportCallout(slot))?);
    let shown = story.capture()?;
    demand(
        bare.difference_region(&shown, plate, 2)? >= 0.01,
        "Alt-click reported a coordinate callout without painting one",
    )?;
    let _hidden = story
        .modified_click(Target::Support(slot), Button::Primary, Modifiers::ALT)?
        .until(shows::support_callout(slot, false))?;
    let hidden = story.capture()?;
    demand(
        shown.difference_region(&hidden, plate, 2)? >= 0.01,
        "a second Alt-click did not remove the coordinate callout",
    )
}

fn close_reverse_and_save(
    story: &mut TrailStory<'_, '_>,
    before_signature: u64,
    trailhead: [f64; 2],
) -> Result<()> {
    let _closed = story.click(Target::CloseLoop)?.until(
        shows::shape(RouteShape::Loop) & shows::editor_ready() & shows::support(0, trailhead),
    )?;
    let closed = story.frame()?;
    let closed_signature =
        signature(&closed).ok_or_else(|| verdict("closed loop omitted its route signature"))?;
    let _reversed = story.click(Target::Reverse)?.until(
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
        .click(Target::EditorSave)?
        .until(shows::view(View::FocusSaved) & shows::library(1))?;
    let _coherent = story.wait_stable(
        Duration::from_secs(3),
        Duration::from_millis(80),
        "saved focus to replace every editor surface",
        |frame| {
            (frame.state.view == View::FocusSaved
                && frame.anchor(&Target::EditorSave.to_string()).is_none())
            .then_some(())
        },
    )?;
    Ok(())
}

fn verify_saved(harness: &Harness<'_>) -> Result<()> {
    let library = read_json(harness.testbed, "manual/library/index.json")?;
    let trails = library["trails"]
        .as_array()
        .ok_or_else(|| verdict("manual Library omitted its trails"))?;
    let [trail] = trails.as_slice() else {
        return Err(verdict("manual story must save exactly one trail"));
    };
    demand(
        trail["design_shape"] == "loop",
        "saved manual trail lost its authored loop intent",
    )?;
    demand(
        trail["support_points"]
            .as_array()
            .is_some_and(|points| points.len() >= SUPPORTS.len()),
        "saved manual loop discarded its support design",
    )
}

fn verify_restart(harness: &Harness<'_>) -> Result<()> {
    let restarted = harness.launch_gui(Some(ROOT), DataMode::Offline, RunClass::Functional)?;
    let mut story = harness.story(&restarted, RunClass::Functional)?;
    let restored = story.wait_within(Duration::from_secs(30), shows::library(1))?;
    let _trail = first_anchor(
        &restored,
        TargetClass::LibraryTrail,
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
