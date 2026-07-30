use std::time::Duration;

use egui_tester::{Button, Key, Modifiers, PerformanceBudget, Result, demand};

use crate::harness::{
    Control, Harness, TrailFrame, TrailStory, durable_budget, first_anchor, map_pixel,
    search_reaction_budget,
};
use crate::observation::{SearchPhase, View, shows};
use crate::performance::{pan_during_search, stress_portfolio};

const ROOT: &str = "/test/compare";

pub fn run(harness: &Harness<'_>) -> Result<()> {
    harness.seed_project(ROOT, "/test/fixtures/dense_network.geojson", false)?;
    harness
        .testbed
        .retain_on_failure("compare/library/index.json")?;
    let app = harness.launch_gui(Some(ROOT), true, false)?;
    let mut story = harness.story(&app)?;
    let _ready = story.ready(Duration::from_secs(30))?;
    let frames = app.frames()?;

    let frame = story.wait_within(Duration::from_secs(15), shows::map())?;
    let trailhead = map_pixel(&frame, [-105.0, 40.0])?;
    let _placed = story
        .modified_click_at(trailhead, Button::Primary, Modifiers::ALT)?
        .expect(shows::trailhead())?;

    let mut strike = story.key(Key::Return)?;
    let progress = strike
        .within(search_reaction_budget())
        .expect(shows::search(SearchPhase::Striking))?;
    pan_during_search(strike.session(), &frames, progress.value())?;
    let eager = strike
        .within(
            PerformanceBudget::new(Duration::from_secs(2))
                .through_presentation()
                .timeout(Duration::from_secs(12)),
        )
        .expect(shows::candidates_at_least(1))?;
    demand(
        eager
            .value()
            .state
            .search
            .as_ref()
            .is_some_and(|search| search.phase == SearchPhase::Striking)
            || eager.value().state.candidates == 12,
        "first candidate was not promoted until after an incomplete search ended",
    )?;
    drop(strike);

    let complete = story.wait_within(
        Duration::from_secs(30),
        shows::candidates(12) & shows::search(SearchPhase::Idle),
    )?;
    let report = stress_portfolio(&mut story, &frames, &complete)?;
    let _returned = focus_and_return(&mut story, &report.settled)?;
    revise_and_stop(&mut story)?;
    choose_and_save(&mut story)?;

    if let Some(artifacts) = harness.artifacts {
        story
            .capture()?
            .save_png(artifacts.join("story-3-compare.png"))?;
    }
    app.terminate()?;
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

fn choose_and_save(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let final_results = story.wait_within(
        Duration::from_secs(30),
        shows::view(View::Browse)
            & shows::candidates_at_least(1)
            & shows::search(SearchPhase::Idle),
    )?;
    let candidate = first_anchor(
        &final_results,
        "results.candidate/",
        "candidate portfolio omitted its first tile",
    )?;
    let _focused = story
        .click_anchor(&candidate)?
        .expect(shows::view(View::FocusCandidate))?;
    let _saved = story
        .click(Control::FocusSave)?
        .within(durable_budget())
        .expect(shows::view(View::FocusSaved) & shows::library(1))?;
    Ok(())
}

fn focus_and_return(story: &mut TrailStory<'_, '_>, browse: &TrailFrame) -> Result<TrailFrame> {
    let baseline = browse
        .state
        .map
        .ok_or_else(|| crate::harness::verdict("portfolio browse omitted its viewport"))?;
    let candidate = first_anchor(
        browse,
        "results.candidate/",
        "candidate portfolio omitted its first tile",
    )?;
    let _focused = story
        .click_anchor(&candidate)?
        .expect(shows::view(View::FocusCandidate))?;
    let returned = story
        .click(Control::FocusBack)?
        .expect(shows::view(View::Browse))?;
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
    Ok(returned.into_value())
}

fn revise_and_stop(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let _typed = story
        .replace_text(Control::DistanceMax, "11.0", shows::text_focused())?
        .presented()?;
    let _scheduled = story
        .key(Key::Return)?
        .within(
            PerformanceBudget::new(Duration::from_millis(700))
                .through_presentation()
                .timeout(Duration::from_secs(8)),
        )
        .expect(shows::revision())?;
    let _revised = story.wait_within(
        Duration::from_secs(30),
        shows::candidates_at_least(1) & shows::search(SearchPhase::Idle),
    )?;

    let require = map_pixel(&story.frame()?, [-104.997, 40.0])?;
    let _required = story
        .click_at(require, Button::Primary)?
        .expect(shows::required(1))?;
    let striking = story.wait_within(
        Duration::from_secs(8),
        shows::candidates_at_least(1) & shows::search(SearchPhase::Striking),
    )?;
    let retained = striking.state.candidates;
    let stopped = story
        .click(Control::Stop)?
        .within(
            PerformanceBudget::new(Duration::from_millis(500))
                .through_presentation()
                .timeout(Duration::from_secs(8)),
        )
        .expect(shows::candidates_at_least(retained) & shows::search(SearchPhase::Idle))?
        .into_value();

    let forbid = map_pixel(&stopped, [-105.0, 40.003])?;
    let _forbidden = story
        .modified_click_at(forbid, Button::Primary, Modifiers::SHIFT)?
        .expect(shows::forbidden(1))?;
    Ok(())
}

fn near_map(left: [f64; 2], right: [f64; 2]) -> bool {
    (left[0] - right[0]).abs() <= 1.0e-12 && (left[1] - right[1]).abs() <= 1.0e-12
}
