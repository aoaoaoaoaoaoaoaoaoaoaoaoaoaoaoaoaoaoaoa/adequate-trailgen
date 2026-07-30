use std::{collections::BTreeSet, path::Path, time::Duration};

use egui_tester::{Button, Drag, Frame, Key, Modifiers, PixelRegion, Result, Wheel, demand};

use crate::harness::{
    DataMode, Harness, RunClass, Target, TargetClass, TrailStory, first_anchor, map_pixel,
    read_json, screen_point,
};
use crate::interactions::lasso_boundary;
use crate::observation::{CorpusPhase, SearchPhase, TrailColoring, View, Workspace, shows};

const ROOT: &str = "/test/discover-loop";

pub fn run(harness: &Harness<'_>) -> Result<()> {
    harness.testbed.retain_on_failure("discover-loop")?;
    let app = harness.launch_gui(None, DataMode::FixtureProviders, RunClass::Functional)?;
    let mut story = harness.story(&app, RunClass::Functional)?;

    create_project(&mut story)?;
    acquire_region(&mut story)?;
    harness.fixtures.assert_harvested()?;
    find_and_keep(&mut story)?;
    exercise_map_area_controls(&mut story)?;
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
        shows::workspace(Workspace::Trail)
            & shows::view(View::Browse)
            & shows::library(1)
            & shows::coloring(TrailColoring::Terrain),
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

fn exercise_map_area_controls(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let focused = story.wait(shows::view(View::FocusSaved) & shows::library(1))?;
    let baseline = focused
        .state
        .map
        .ok_or_else(|| crate::harness::verdict("saved trail omitted its viewport"))?;
    let armed = story
        .click(Target::AddMapArea)?
        .until(shows::view(View::Browse))?
        .into_value();
    let actual = armed
        .state
        .map
        .ok_or_else(|| crate::harness::verdict("map-area tool omitted its viewport"))?;
    demand(
        near_map(actual.center, baseline.center)
            && (actual.world_points - baseline.world_points).abs() <= 1.0e-6,
        format!("arming Add Map Area moved the viewport from {baseline:?} to {actual:?}"),
    )?;
    let browse = story.key(Key::Escape)?.next_frame()?.into_value();
    let saved = first_anchor(
        &browse,
        TargetClass::LibraryTrail,
        "saved trail vanished after cancelling Add Map Area",
    )?;
    let _focused = story
        .click_anchor(&saved)?
        .until(shows::view(View::FocusSaved))?;

    let _started = story
        .click(Target::RefreshTrails)?
        .until(shows::corpus(CorpusPhase::Updating) & shows::view(View::FocusSaved))?;
    let refreshed = story.wait_within(
        Duration::from_secs(30),
        shows::corpus(CorpusPhase::Idle) & shows::view(View::FocusSaved) & shows::library(1),
    )?;
    let actual = refreshed
        .state
        .map
        .ok_or_else(|| crate::harness::verdict("refreshed trail omitted its viewport"))?;
    demand(
        near_map(actual.center, baseline.center)
            && (actual.world_points - baseline.world_points).abs() <= 1.0e-6,
        format!("refreshing trails moved the viewport from {baseline:?} to {actual:?}"),
    )
}

fn near_map(left: [f64; 2], right: [f64; 2]) -> bool {
    (left[0] - right[0]).abs() <= 1.0e-12 && (left[1] - right[1]).abs() <= 1.0e-12
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
            -14,
            Wheel {
                tick_duration: Duration::from_millis(24),
            },
        )?
        .until(shows::map_scale_at_least(
            800_000.0_f64.max(initial_scale * 128.0),
        ))?;
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
    exercise_color_legend(story, &frame)?;
    let frame = story.wait(shows::coloring(TrailColoring::Terrain))?;
    let trailhead = map_pixel(&frame, [-98.5, 39.5])?;
    let _placed = story
        .modified_click_at(trailhead, Button::Primary, Modifiers::ALT)?
        .until(shows::trailhead())?;
    let _armed = story.click(Target::Boundary)?.next_frame()?;

    let _bounded = lasso_boundary(story, 0.15)?;
    Ok(())
}

fn exercise_color_legend(
    story: &mut TrailStory<'_, '_>,
    frame: &crate::harness::TrailFrame,
) -> Result<()> {
    let northwest = map_pixel(frame, [-98.516, 39.516])?;
    let southeast = map_pixel(frame, [-98.484, 39.484])?;
    let region = PixelRegion::new(
        i32::from(northwest.0.min(southeast.0)) - 12,
        i32::from(northwest.1.min(southeast.1)) - 12,
        i32::from(northwest.0.max(southeast.0)) + 12,
        i32::from(northwest.1.max(southeast.1)) + 12,
    );
    let class = story.capture()?;
    let _formal_state = story
        .click(Target::LegendFormality)?
        .until(shows::coloring(TrailColoring::Formality))?;
    let formal =
        story
            .session()
            .wait_changed_region(&class, region, 0.002, 5, Duration::from_secs(4))?;
    let formal_difference = class.difference_region(&formal, region, 5)?;
    demand(
        formal_difference >= 0.000_5,
        "formality selector changed semantic state without recoloring map trails",
    )?;
    let formality_agreement = core_agreement(&class, &formal, region)?;
    demand(
        formality_agreement >= 0.98,
        format!(
            "formality coloring perturbed the fixed surface / wayfinding cadence \
             ({formality_agreement:.3} agreement)"
        ),
    )?;

    let _terrain_state = story
        .click(Target::LegendTerrain)?
        .until(shows::coloring(TrailColoring::Terrain))?;
    let terrain =
        story
            .session()
            .wait_changed_region(&formal, region, 0.002, 5, Duration::from_secs(4))?;
    let terrain_difference = formal.difference_region(&terrain, region, 5)?;
    demand(
        terrain_difference >= 0.000_5,
        "terrain selector changed semantic state without recoloring map trails",
    )?;
    let terrain_agreement = core_agreement(&formal, &terrain, region)?;
    demand(
        terrain_agreement >= 0.98,
        format!(
            "terrain coloring perturbed the fixed surface / wayfinding cadence \
             ({terrain_agreement:.3} agreement)"
        ),
    )
}

fn core_agreement(left: &Frame, right: &Frame, region: PixelRegion) -> Result<f64> {
    let left = left.crop(region)?;
    let right = right.crop(region)?;
    demand(
        (left.width(), left.height()) == (right.width(), right.height()),
        "trail-color cadence oracle received differently sized frames",
    )?;
    let dark = |pixel: &[u8]| pixel[0] <= 92 && pixel[1] <= 92 && pixel[2] <= 92;
    let left_ink = left.rgba().chunks_exact(4).map(dark).collect::<Vec<_>>();
    let right_ink = right.rgba().chunks_exact(4).map(dark).collect::<Vec<_>>();
    let left_count = left_ink.iter().filter(|ink| **ink).count();
    let right_count = right_ink.iter().filter(|ink| **ink).count();
    let total = left_count + right_count;
    demand(
        total >= 128,
        "trail-color cadence oracle found too little rendered core ink",
    )?;
    let width = usize::try_from(left.width())
        .map_err(|_| crate::harness::verdict("cadence crop width exceeded usize"))?;
    let height = usize::try_from(left.height())
        .map_err(|_| crate::harness::verdict("cadence crop height exceeded usize"))?;
    let covered = covered_ink(&left_ink, &right_ink, width, height)
        + covered_ink(&right_ink, &left_ink, width, height);
    let covered = u32::try_from(covered)
        .map_err(|_| crate::harness::verdict("covered core ink exceeded u32"))?;
    let total =
        u32::try_from(total).map_err(|_| crate::harness::verdict("total core ink exceeded u32"))?;
    Ok(f64::from(covered) / f64::from(total))
}

fn covered_ink(source: &[bool], target: &[bool], width: usize, height: usize) -> usize {
    source
        .iter()
        .enumerate()
        .filter(|(index, ink)| {
            if !**ink {
                return false;
            }
            let x = index % width;
            let y = index / width;
            let x_range = x.saturating_sub(1)..=(x + 1).min(width - 1);
            let y_range = y.saturating_sub(1)..=(y + 1).min(height - 1);
            y_range
                .flat_map(|y| x_range.clone().map(move |x| y * width + x))
                .any(|index| target[index])
        })
        .count()
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
