use std::{path::Path, time::Duration};

use egui_tester::{Drag, Key, PixelRegion, Result, demand};

use crate::harness::{
    DataMode, Harness, RunClass, Target, TargetClass, first_anchor, read_json, screen_point,
};
use crate::observation::{SearchPhase, View, Workspace, shows};

const ROOT: &str = "/test/preparing-workbench";
const RENAMED: &str = "Prepared Without Waiting";

pub fn run(harness: &Harness<'_>) -> Result<()> {
    harness.seed_project(ROOT, "/test/fixtures/mini_network.geojson", true)?;
    migrate_legacy_library(harness)?;

    let app = harness.testbed.launch(
        harness
            .gui(Some(ROOT), DataMode::Offline, RunClass::Functional)
            .env("TRAILGEN_STALL_ARMAMENT_MS", "15000"),
    )?;
    let mut story = harness.story(&app, RunClass::Functional)?;
    let preparing = story.wait_within(
        Duration::from_secs(6),
        shows::workspace(Workspace::Preparing)
            & shows::view(View::Browse)
            & shows::library(1)
            & shows::areas(0)
            & shows::map(),
    )?;
    prove_wait_is_alive(&story, &preparing, harness.artifacts)?;

    let _distance = story
        .replace_text(Target::DistanceMax, "12.3", shows::text_focused())?
        .next_frame()?;
    let idle = story
        .click(Target::Find)?
        .until(shows::workspace(Workspace::Preparing) & shows::search(SearchPhase::Idle))?
        .into_value();
    demand(
        idle.state.candidates == 0,
        "disabled Find Trails launched work before graph armament",
    )?;

    let trail = first_anchor(
        &idle,
        TargetClass::LibraryTrail,
        "preparing workbench omitted its saved trail row",
    )?;
    let _focused = story.click_anchor(&trail)?.until(
        shows::workspace(Workspace::Preparing)
            & shows::view(View::FocusSaved)
            & shows::profile_visible(),
    )?;
    let _renaming = story
        .click(Target::FocusRename)?
        .until(shows::workspace(Workspace::Preparing) & shows::rename(true))?;
    let _typed = story
        .replace_text(Target::RenameField, RENAMED, shows::text_focused())?
        .next_frame()?;
    let renamed = story
        .key(Key::Return)?
        .until(shows::workspace(Workspace::Preparing) & shows::rename(false))?
        .into_value();

    let map = renamed
        .anchor(&Target::Map.to_string())
        .ok_or_else(|| crate::harness::verdict("preparing workbench omitted its map target"))?
        .clone();
    let [x0, y0, x1, y1] = map.rect;
    let from = screen_point([
        f64::from(f32::midpoint(x0, x1)),
        f64::from(f32::midpoint(y0, y1)),
    ])?;
    let to = (from.0.saturating_add(72), from.1.saturating_add(36));
    let panned = story
        .drag_from(
            from,
            to,
            Drag {
                duration: Duration::from_millis(120),
                ..Drag::default()
            },
        )?
        .until(shows::workspace(Workspace::Preparing))?
        .into_value();
    let panned_map = panned
        .state
        .map
        .ok_or_else(|| crate::harness::verdict("pan omitted the preparing viewport"))?;

    let armed = story.wait_within(
        Duration::from_secs(25),
        shows::workspace(Workspace::Trail)
            & shows::view(View::FocusSaved)
            & shows::library(1)
            & shows::profile_visible(),
    )?;
    let armed_map = armed
        .state
        .map
        .ok_or_else(|| crate::harness::verdict("armament publication omitted the viewport"))?;
    demand(
        near_map(armed_map.center, panned_map.center)
            && (armed_map.world_points - panned_map.world_points).abs() <= 1.0e-6,
        format!(
            "armament publication moved the live viewport from {panned_map:?} to {armed_map:?}"
        ),
    )?;
    verify_library(harness)?;
    app.terminate()
}

fn prove_wait_is_alive(
    story: &crate::harness::TrailStory<'_, '_>,
    frame: &crate::harness::TrailFrame,
    artifacts: Option<&Path>,
) -> Result<()> {
    let anchor = frame
        .anchor(&Target::TrailDataWait.to_string())
        .ok_or_else(|| crate::harness::verdict("preparing workbench omitted its waiting target"))?;
    let baseline = story.capture()?;
    let motion = story.session().wait_changed_region(
        &baseline,
        PixelRegion::anchor(anchor),
        0.002,
        2,
        Duration::from_secs(4),
    )?;
    if let Some(artifacts) = artifacts {
        baseline.save_png(artifacts.join("prepare-wait-before.png"))?;
        motion.save_png(artifacts.join("prepare-wait-motion.png"))?;
    }
    Ok(())
}

fn migrate_legacy_library(harness: &Harness<'_>) -> Result<()> {
    let app = harness.launch_gui(Some(ROOT), DataMode::Offline, RunClass::Functional)?;
    let mut story = harness.story(&app, RunClass::Functional)?;
    let _ready = story.wait_within(
        Duration::from_secs(30),
        shows::workspace(Workspace::Trail) & shows::library(1),
    )?;
    app.terminate()
}

fn verify_library(harness: &Harness<'_>) -> Result<()> {
    let library = read_json(harness.testbed, "preparing-workbench/library/index.json")?;
    demand(
        library["search"]["distance_m"]["max"].as_f64() == Some(12_300.0),
        "search edit made during preparation did not reach durable state",
    )?;
    demand(
        library["trails"][0]["name"].as_str() == Some(RENAMED),
        "saved-trail rename made during preparation was lost at publication",
    )
}

fn near_map(left: [f64; 2], right: [f64; 2]) -> bool {
    (left[0] - right[0]).abs() <= 1.0e-12 && (left[1] - right[1]).abs() <= 1.0e-12
}
