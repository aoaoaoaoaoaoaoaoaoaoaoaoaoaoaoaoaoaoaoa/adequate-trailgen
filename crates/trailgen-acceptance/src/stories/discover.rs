use std::{collections::BTreeSet, path::Path, time::Duration};

use egui_tester::{Button, Drag, Key, Modifiers, Result, Wheel, demand};

use crate::harness::{
    DataMode, Harness, RunClass, Target, TargetClass, TrailStory, first_anchor, map_pixel,
    read_json, screen_point,
};
use crate::interactions::lasso_boundary;
use crate::observation::{SearchPhase, View, Workspace, shows};

const ROOT: &str = "/test/discover-loop";

pub fn run(harness: &Harness<'_>) -> Result<()> {
    harness.testbed.retain_on_failure("discover-loop")?;
    let app = harness.launch_gui(None, DataMode::FixtureProviders, RunClass::Functional)?;
    let mut story = harness.story(&app, RunClass::Functional)?;

    create_project(&mut story)?;
    acquire_region(&mut story)?;
    harness.fixtures.assert_harvested()?;
    find_and_keep(&mut story)?;
    verify_discovery(harness)?;

    if let Some(artifacts) = harness.artifacts {
        story
            .capture()?
            .save_png(artifacts.join("story-1-discover.png"))?;
    }
    app.terminate()?;
    drop(story);
    drop(app);

    let restarted = harness.launch_gui(Some(ROOT), DataMode::Offline, RunClass::Functional)?;
    let mut story = harness.story(&restarted, RunClass::Functional)?;
    let restored = story.wait_within(
        Duration::from_secs(30),
        shows::workspace(Workspace::Trail) & shows::view(View::Browse) & shows::library(1),
    )?;
    let trail = first_anchor(
        &restored,
        TargetClass::LibraryTrail,
        "restored Library row vanished",
    )?;
    let _opened = story
        .click_anchor(&trail)?
        .until(shows::view(View::FocusSaved))?;
    restarted.terminate()?;
    Ok(())
}

fn create_project(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let _deck = story.wait(shows::workspace(Workspace::Projects) & shows::view(View::Projects))?;
    let _name = story
        .replace_text(Target::ProjectName, "Discover Loop", shows::text_focused())?
        .next_frame()?;
    let _parent = story
        .replace_text(Target::ProjectParent, "/test", shows::text_focused())?
        .next_frame()?;
    let _created = story
        .click(Target::ProjectCreate)?
        .until(shows::workspace(Workspace::Survey))?;
    Ok(())
}

fn acquire_region(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let _drawing = story
        .click(Target::SurveyAddArea)?
        .until(shows::survey_drawing())?;
    let [x0, y0, x1, y1] = story.anchor(Target::SurveyMap)?.rect;
    let center = (f32::midpoint(x0, x1), f32::midpoint(y0, y1));
    let from = screen_point([f64::from(center.0 - 13.0), f64::from(center.1 - 13.0)])?;
    let to = screen_point([f64::from(center.0 + 13.0), f64::from(center.1 + 13.0)])?;
    let _started = story
        .drag_from(
            from,
            to,
            Drag {
                duration: Duration::from_millis(120),
                ..Drag::default()
            },
        )?
        .until(shows::survey_acquiring(1))?;
    let _ready = story.wait_within(
        Duration::from_secs(30),
        shows::workspace(Workspace::Trail) & shows::candidates(0),
    )?;
    Ok(())
}

fn find_and_keep(story: &mut TrailStory<'_, '_>) -> Result<()> {
    configure_search(story)?;
    let mut strike = story.key(Key::Return)?;
    let _progress = strike.until(shows::search(SearchPhase::Running))?;
    let _eager = strike.until(shows::candidates_at_least(1))?;
    drop(strike);

    let complete = story.wait_within(
        Duration::from_secs(20),
        shows::candidates_at_least(1) & shows::search(SearchPhase::Idle),
    )?;
    let candidate = first_anchor(
        &complete,
        TargetClass::Candidate,
        "search produced no visible result tile",
    )?;
    let _focused = story
        .click_anchor(&candidate)?
        .until(shows::view(View::FocusCandidate))?;
    let _saved = story
        .click(Target::FocusSave)?
        .until(shows::view(View::FocusSaved) & shows::library(1))?;
    Ok(())
}

fn configure_search(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let frame = story.wait(shows::map())?;
    let initial_scale = frame
        .state
        .map
        .as_ref()
        .map(|map| map.world_points)
        .unwrap_or_default();
    let center = story.anchor(Target::Map)?.center();
    let _zoom = story
        .wheel(
            center,
            -7,
            Wheel {
                tick_duration: Duration::from_millis(24),
            },
        )?
        .until(shows::map_scale_at_least(initial_scale * 16.0))?;
    let frame = story.wait_stable(
        Duration::from_secs(8),
        Duration::from_millis(160),
        "trailhead-placement viewport to settle",
        |frame| {
            frame.state.map.as_ref().map(|map| {
                [
                    map.center[0].to_bits(),
                    map.center[1].to_bits(),
                    map.world_points.to_bits(),
                ]
            })
        },
    )?;
    let trailhead = map_pixel(&frame, [-98.5, 39.5])?;
    let _placed = story
        .modified_click_at(trailhead, Button::Primary, Modifiers::ALT)?
        .until(shows::trailhead())?;
    let _armed = story.click(Target::Boundary)?.next_frame()?;

    let _bounded = lasso_boundary(story, 0.15)?;
    Ok(())
}

fn verify_discovery(harness: &Harness<'_>) -> Result<()> {
    let config = harness
        .testbed
        .read_private_to_string("discover-loop/trailgen.toml")?
        .parse::<toml::Table>()
        .map_err(|error| crate::harness::verdict(format!("parse project config: {error}")))?;
    demand(
        config
            .get("trail_data")
            .and_then(toml::Value::as_table)
            .and_then(|trail_data| trail_data.get("managed"))
            .and_then(toml::Value::as_bool)
            == Some(true),
        "discovered project is not managed",
    )?;
    let graph = read_json(harness.testbed, "discover-loop/cache/graph.json")?;
    demand(
        graph["edges"]
            .as_array()
            .is_some_and(|edges| !edges.is_empty()),
        "provider acquisition committed an empty graph",
    )?;
    let index = read_json(harness.testbed, "discover-loop/cache/trails.json")?;
    let providers = index["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|receipt| receipt["provider"].as_str())
        .collect::<BTreeSet<_>>();
    demand(
        providers.contains("osm") && providers.contains("usgs-national-trails"),
        "discovered project omitted a trail-provider receipt family",
    )?;
    for raw in index["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|receipt| receipt["raw_path"].as_str())
    {
        let _receipt = harness
            .testbed
            .read_private(Path::new("discover-loop").join(raw))?;
    }
    demand(
        index["elevation"]
            .as_array()
            .is_some_and(|receipts| !receipts.is_empty()),
        "discovered project omitted terrain receipts",
    )?;
    let library = read_json(harness.testbed, "discover-loop/library/index.json")?;
    demand(
        library["trails"]
            .as_array()
            .is_some_and(|trails| trails.len() == 1),
        "saved discovery did not reach the durable Library",
    )
}
