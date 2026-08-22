use std::time::Duration;

use egui_tester::{Button, Key, Modifiers, PixelRegion, ReactionBudget, Result, demand};

use crate::harness::{
    DataMode, Harness, RunClass, Target, TargetClass, TrailFrame, TrailStory, durable_budget,
    first_anchor, map_pixel, read_json, search_reaction_budget,
};
use crate::observation::{SearchPhase, View, shows};
use crate::performance::{pan_during_search, stress_portfolio};

const ROOT: &str = "/test/compare";

pub fn run(harness: &Harness<'_>) -> Result<()> {
    harness.seed_project(ROOT, "/test/fixtures/dense_network.geojson", false)?;
    harness
        .testbed
        .retain_on_failure("compare/library/index.json")?;
    let app = harness.launch_gui(Some(ROOT), DataMode::Offline, RunClass::Performance)?;
    let mut story = harness.story(&app, RunClass::Performance)?;
    let frames = app.frames()?;

    let _ready = story.wait_within(Duration::from_secs(15), shows::map())?;
    let _finder = story.click(Target::Finder)?.next_frame()?;
    let frame = story.frame()?;
    let trailhead = map_pixel(&frame, [-105.0, 40.0])?;
    let _armed = story.click(Target::TrailheadPlacement)?.next_frame()?;
    let _placed = story
        .click_at(trailhead, Button::Primary)?
        .until(shows::trailhead())?;

    let mut strike = story.key(Key::Return)?;
    let strike_receipt = strike.receipt().clone();
    let progress = strike
        .within(search_reaction_budget())
        .until(shows::search(SearchPhase::Running))?;
    let progress = progress.into_value();
    drop(strike);
    pan_during_search(story.session(), &frames, &progress)?;
    let mut strike = story.reaction(strike_receipt);
    let eager = strike
        .within(
            ReactionBudget::performance(Duration::from_secs(2))
                .through_surface_present()
                .timeout(Duration::from_secs(12)),
        )
        .until(shows::candidates_at_least(1))?;
    demand(
        eager
            .value()
            .state
            .search
            .as_ref()
            .is_some_and(|search| search.phase == SearchPhase::Running)
            || eager.value().state.candidates == 12,
        "first candidate was not promoted until after an incomplete search ended",
    )?;
    drop(strike);

    let complete = story.wait_within(
        Duration::from_secs(30),
        shows::candidates(12) & shows::search(SearchPhase::Idle),
    )?;
    let report = stress_portfolio(&mut story, &frames, &complete)?;
    let tool_view = arm_boundary_without_camera_travel(&mut story, &report.settled)?;
    let _returned = focus_and_return(&mut story, &tool_view)?;
    revise_and_stop(&mut story)?;
    choose_and_save(&mut story)?;

    if let Some(artifacts) = harness.artifacts {
        story
            .capture()?
            .save_png(artifacts.join("story-3-compare.png"))?;
    }
    app.terminate()?;
    drop(story);
    drop(app);
    verify_restart(harness)?;
    println!(
        "comparison cadence: pan p50={:?} p95={:?} worst={:?}; zoom p50={:?} p95={:?} worst={:?}",
        report.pan.p50,
        report.pan.p95,
        report.pan.worst,
        report.zoom.p50,
        report.zoom.p95,
        report.zoom.worst,
    );
    Ok(())
}

fn arm_boundary_without_camera_travel(
    story: &mut TrailStory<'_, '_>,
    browse: &TrailFrame,
) -> Result<TrailFrame> {
    let candidate = first_anchor(
        browse,
        TargetClass::Candidate,
        "candidate portfolio omitted its first tile",
    )?;
    let focused = story
        .click_anchor(&candidate)?
        .until(shows::view(View::FocusCandidate))?
        .into_value();
    let baseline = focused
        .state
        .map
        .ok_or_else(|| crate::harness::verdict("focused candidate omitted its viewport"))?;
    let armed = story
        .click(Target::Boundary)?
        .until(shows::view(View::Browse) & shows::boundary_drawing(true))?
        .into_value();
    let actual = armed
        .state
        .map
        .ok_or_else(|| crate::harness::verdict("armed boundary omitted its viewport"))?;
    demand(
        near_map(actual.center, baseline.center)
            && (actual.world_points - baseline.world_points).abs() <= 1.0e-6,
        format!("arming the search boundary moved the viewport from {baseline:?} to {actual:?}"),
    )?;
    let _disarmed = story
        .key(Key::Escape)?
        .until(shows::boundary_drawing(false))?;
    story.wait_stable(
        Duration::from_secs(3),
        Duration::from_millis(180),
        "search-boundary cancellation to settle",
        |frame| {
            frame.state.map.map(|map| {
                [
                    map.center[0].to_bits(),
                    map.center[1].to_bits(),
                    map.world_points.to_bits(),
                ]
            })
        },
    )
}

fn choose_and_save(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let final_results = story.wait_within(
        Duration::from_secs(30),
        shows::view(View::Browse)
            & shows::candidates_at_least(1)
            & shows::search(SearchPhase::Idle),
    )?;
    let candidate = first_anchor(
        &final_results,
        TargetClass::Candidate,
        "candidate portfolio omitted its first tile",
    )?;
    let _focused = story
        .click_anchor(&candidate)?
        .until(shows::view(View::FocusCandidate))?;
    let _saved = story
        .click(Target::FocusSave)?
        .within(durable_budget())
        .until(shows::view(View::FocusSaved) & shows::library(1))?;
    Ok(())
}

fn focus_and_return(story: &mut TrailStory<'_, '_>, browse: &TrailFrame) -> Result<TrailFrame> {
    let baseline = browse
        .state
        .map
        .ok_or_else(|| crate::harness::verdict("portfolio browse omitted its viewport"))?;
    let map = browse
        .anchor(&Target::Map.to_string())
        .ok_or_else(|| crate::harness::verdict("portfolio browse omitted its map target"))?;
    let region = PixelRegion::anchor(map);
    let baseline_pixels = neutral_capture(story)?;
    let candidate = first_anchor(
        browse,
        TargetClass::Candidate,
        "candidate portfolio omitted its first tile",
    )?;
    let _focused = story
        .click_anchor(&candidate)?
        .until(shows::view(View::FocusCandidate))?;
    let focused_pixels = neutral_capture(story)?;
    let returned = story
        .click(Target::FocusBack)?
        .until(shows::view(View::Browse))?;
    let restored = returned
        .value()
        .state
        .map
        .ok_or_else(|| crate::harness::verdict("returned comparison omitted its viewport"))?;
    demand(
        near_map(restored.center, baseline.center)
            && (restored.world_points - baseline.world_points).abs() <= 1.0e-6,
        format!("focus return changed viewport from {baseline:?} to {restored:?}"),
    )?;
    let restored_pixels = neutral_capture(story)?;
    let focused_difference = baseline_pixels.difference_region(&focused_pixels, region, 8)?;
    let restored_difference = baseline_pixels.difference_region(&restored_pixels, region, 8)?;
    demand(
        restored_difference <= 0.05 && restored_difference + 0.02 <= focused_difference,
        format!(
            "focus return differs by {:.2}% of map pixels; focused control differs by {:.2}%",
            restored_difference * 100.0,
            focused_difference * 100.0,
        ),
    )?;
    Ok(returned.into_value())
}

fn neutral_capture(story: &mut TrailStory<'_, '_>) -> Result<egui_tester::Frame> {
    let motion = story.session().move_to(4, 4)?;
    let _neutral = story.reaction(motion).next_frame()?;
    story.capture()
}

fn revise_and_stop(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let _typed = story
        .replace_text(Target::DistanceMax, "11.0", shows::text_focused())?
        .next_frame()?;
    let _scheduled = story
        .key(Key::Return)?
        .within(
            ReactionBudget::performance(Duration::from_millis(700))
                .through_surface_present()
                .timeout(Duration::from_secs(8)),
        )
        .until(shows::revision())?;
    let _revised = story.wait_within(
        Duration::from_secs(30),
        shows::candidates_at_least(1) & shows::search(SearchPhase::Idle),
    )?;

    let require = map_pixel(&story.frame()?, [-104.997, 40.0])?;
    let _required = story
        .click_at(require, Button::Primary)?
        .until(shows::required(1))?;
    let _undone = story
        .chord(Modifiers::CTRL, Key::Character('z'))?
        .until(shows::required(0))?;
    let _redone = story
        .chord(Modifiers::CTRL, Key::Character('y'))?
        .until(shows::required(1))?;
    let striking = story.wait_within(
        Duration::from_secs(8),
        shows::candidates_at_least(1) & shows::search(SearchPhase::Running),
    )?;
    let retained = striking.state.candidates;
    let stopped = story
        .click(Target::Stop)?
        .within(
            ReactionBudget::performance(Duration::from_millis(500))
                .through_surface_present()
                .timeout(Duration::from_secs(8)),
        )
        .until(shows::candidates_at_least(retained) & shows::search(SearchPhase::Idle))?
        .into_value();

    let forbid = map_pixel(&stopped, [-105.0, 40.003])?;
    let _forbidden = story
        .modified_click_at(forbid, Button::Primary, Modifiers::SHIFT)?
        .until(shows::forbidden(1))?;
    let _undone = story
        .chord(Modifiers::CTRL, Key::Character('z'))?
        .until(shows::forbidden(0))?;
    let _redone = story
        .chord(Modifiers::CTRL, Key::Character('y'))?
        .until(shows::forbidden(1))?;
    Ok(())
}

fn verify_restart(harness: &Harness<'_>) -> Result<()> {
    let library = read_json(harness.testbed, "compare/library/index.json")?;
    demand(
        library["trails"]
            .as_array()
            .is_some_and(|trails| trails.len() == 1),
        "comparison story did not durably save its chosen candidate",
    )?;
    let restarted = harness.launch_gui(Some(ROOT), DataMode::Offline, RunClass::Functional)?;
    let mut story = harness.story(&restarted, RunClass::Functional)?;
    let restored = story.wait_within(Duration::from_secs(30), shows::library(1))?;
    let _trail = first_anchor(
        &restored,
        TargetClass::LibraryTrail,
        "restarted comparison omitted its saved trail",
    )?;
    restarted.terminate()
}

fn near_map(left: [f64; 2], right: [f64; 2]) -> bool {
    (left[0] - right[0]).abs() <= 1.0e-12 && (left[1] - right[1]).abs() <= 1.0e-12
}
