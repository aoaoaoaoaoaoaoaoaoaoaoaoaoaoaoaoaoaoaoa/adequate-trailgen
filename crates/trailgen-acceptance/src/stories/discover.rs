use std::{collections::BTreeSet, path::Path, time::Duration};

use egui_tester::{Button, Drag, Frame, Key, Modifiers, PixelRegion, Result, Wheel, demand};

use crate::harness::{
    DataMode, Harness, RunClass, Target, TargetClass, TrailStory, durable_budget, first_anchor,
    instant_budget, map_pixel, read_json, screen_point,
};
use crate::interactions::lasso_boundary;
use crate::observation::{
    AreaCorner, CorpusPhase, SearchPhase, TrailColoring, View, Workspace, shows,
};

const ROOT: &str = "/test/discover-loop";
const AREA_NAME: &str = "West Ridge";

pub fn run(harness: &Harness<'_>) -> Result<()> {
    harness.testbed.retain_on_failure("discover-loop")?;
    let app = harness.launch_gui(None, DataMode::FixtureProviders, RunClass::Functional)?;
    let mut story = harness.story(&app, RunClass::Functional)?;

    create_project(&mut story)?;
    acquire_region(&mut story)?;
    exercise_shortcut_guide(&mut story, harness.artifacts)?;
    find_and_keep(&mut story)?;
    add_civic_area(&mut story, harness)?;
    harness.fixtures.assert_harvested()?;
    verify_discovery(harness)?;

    if let Some(artifacts) = harness.artifacts {
        story
            .capture()?
            .save_png(artifacts.join("story-1-discover.png"))?;
    }
    app.terminate()?;
    drop(story);
    drop(app);

    exercise_map_area_lifecycle(harness)?;
    verify_area_persistence(harness)?;

    let restarted = harness.launch_gui(Some(ROOT), DataMode::Offline, RunClass::Functional)?;
    let mut story = harness.story(&restarted, RunClass::Functional)?;
    let restored = story.wait_within(
        Duration::from_secs(30),
        shows::workspace(Workspace::Trail)
            & shows::view(View::Browse)
            & shows::library(1)
            & shows::areas(2)
            & shows::civic(1, 1)
            & shows::basemap_tiles_at_least(1)
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

fn exercise_shortcut_guide(
    story: &mut TrailStory<'_, '_>,
    artifacts: Option<&std::path::Path>,
) -> Result<()> {
    let baseline = neutral_capture(story)?;
    let opened = story
        .key(Key::Character('?'))?
        .within(instant_budget())
        .until(shows::shortcut_help(true))?;
    let card = opened
        .value()
        .anchor(&Target::ShortcutHelpCard.to_string())
        .ok_or_else(|| crate::harness::verdict("shortcut guide omitted its card anchor"))?
        .clone();
    // A completed wgpu present can precede X11 capture visibility; the tester
    // needs a capturable-presentation endpoint before this fence can collapse.
    let visible = story
        .session()
        .wait_changed(&baseline, 0.20, 8, Duration::from_secs(2))?;
    demand(
        baseline.difference_region(&visible, PixelRegion::anchor(&card), 8)? >= 0.20,
        "the shortcut guide was witnessed open but produced no modal presentation",
    )?;
    if let Some(artifacts) = artifacts {
        visible.save_png(artifacts.join("story-1-shortcut-guide.png"))?;
    }
    let _closed = story
        .key(Key::Escape)?
        .within(instant_budget())
        .until(shows::shortcut_help(false))?;
    Ok(())
}

fn add_civic_area(story: &mut TrailStory<'_, '_>, harness: &Harness<'_>) -> Result<()> {
    reveal_civic_search(story)?;
    let baseline = story.wait(shows::civic(0, 0) & shows::map())?;
    let baseline_map = baseline
        .state
        .map
        .ok_or_else(|| crate::harness::verdict("civic story omitted its baseline viewport"))?;
    let _suggested = story
        .replace_text(
            Target::CivicSearch,
            "brooklyn borough",
            shows::text_focused(),
        )?
        .within(instant_budget())
        .until(shows::civic_suggestions_at_least(1))?;
    let prepared = story
        .key(Key::Return)?
        .within(instant_budget())
        .until(shows::civic(1, 0) & shows::civic_preparing(1))?
        .into_value();
    let after_add = prepared
        .state
        .map
        .ok_or_else(|| crate::harness::verdict("civic addition withdrew the map"))?;
    demand(
        near_map(after_add.center, baseline_map.center)
            && (after_add.world_points - baseline_map.world_points).abs() <= 1.0e-6,
        format!("adding a civic area moved the viewport from {baseline_map:?} to {after_add:?}"),
    )?;
    let _ready = story.wait_within(Duration::from_secs(10), shows::civic(1, 1))?;
    verify_civic_persistence(harness)?;

    reveal_civic_target(story, Target::CivicArea(0))?;
    let fitted = story
        .click(Target::CivicArea(0))?
        .within(instant_budget())
        .until(shows::condition("a fitted civic viewport", move |state| {
            state.map.as_ref().is_some_and(|map| {
                !near_map(map.center, baseline_map.center)
                    || (map.world_points - baseline_map.world_points).abs() > 1.0e-6
            })
        }))?
        .into_value();
    let map = fitted
        .anchor(&Target::Map.to_string())
        .ok_or_else(|| crate::harness::verdict("fitted civic view omitted its map anchor"))?;
    let frame = story.capture()?;
    let ink = civic_ink(&frame, PixelRegion::anchor(map))?;
    demand(
        ink >= 80,
        format!("fitted Brooklyn boundary rendered only {ink} magenta pixels"),
    )?;
    if let Some(artifacts) = harness.artifacts {
        frame.save_png(artifacts.join("story-1-civic-boundary.png"))?;
    }
    Ok(())
}

fn reveal_civic_search(story: &mut TrailStory<'_, '_>) -> Result<()> {
    reveal_civic_target(story, Target::CivicSearch)
}

fn reveal_civic_target(story: &mut TrailStory<'_, '_>, target: Target) -> Result<()> {
    let frame = story.wait(shows::map())?;
    let map = frame
        .state
        .map
        .as_ref()
        .ok_or_else(|| crate::harness::verdict("civic reveal omitted its map transform"))?;
    let ppp = f64::from(frame.ppp);
    let point = screen_point([
        f64::from(map.rect[0]) * ppp * 0.5,
        f64::from(f32::midpoint(map.rect[1], map.rect[3])) * ppp,
    ])?;
    let screen_bottom = f64::from(story.capture()?.height().saturating_sub(20));
    for _ in 0..4 {
        let anchor = story.anchor(target)?;
        let center = anchor.center();
        if f64::from(center.1) >= 50.0 && f64::from(center.1) <= screen_bottom {
            return Ok(());
        }
        let ticks = if f64::from(center.1) > screen_bottom {
            10
        } else {
            -10
        };
        let _scrolled = story
            .wheel(
                point,
                ticks,
                Wheel {
                    tick_duration: Duration::from_millis(8),
                },
            )?
            .next_frame()?;
    }
    Err(crate::harness::verdict(format!(
        "inspector could not reveal {target}"
    )))
}

fn civic_ink(frame: &Frame, region: PixelRegion) -> Result<usize> {
    let crop = frame.crop(region)?;
    Ok(crop
        .rgba()
        .chunks_exact(4)
        .filter(|pixel| {
            pixel[0] >= 140
                && pixel[0].saturating_sub(pixel[1]) >= 45
                && pixel[2].saturating_sub(pixel[1]) >= 25
        })
        .count())
}

fn verify_civic_persistence(harness: &Harness<'_>) -> Result<()> {
    let index = read_json(harness.testbed, "discover-loop/civic/index.json")?;
    demand(
        index["areas"]
            .as_array()
            .is_some_and(|areas| areas.len() == 1 && areas[0]["name"].as_str() == Some("Brooklyn")),
        "Brooklyn did not reach the durable civic-area index",
    )?;
    let snapshot = harness
        .testbed
        .read_private("discover-loop/civic/shapes/nyc-3.json.zst")?;
    demand(
        !snapshot.is_empty(),
        "Brooklyn civic-area snapshot was not persisted",
    )
}

fn exercise_map_area_lifecycle(harness: &Harness<'_>) -> Result<()> {
    let app = harness.launch_gui(
        Some(ROOT),
        DataMode::FixtureProviders,
        RunClass::Performance,
    )?;
    let mut story = harness.story(&app, RunClass::Performance)?;
    let settled = story.wait_within(
        Duration::from_secs(30),
        shows::workspace(Workspace::Trail)
            & shows::view(View::Browse)
            & shows::library(1)
            & shows::areas(1)
            & shows::basemap_tiles_at_least(1),
    )?;
    let map = settled
        .anchor(&Target::Map.to_string())
        .ok_or_else(|| crate::harness::verdict("map-area story omitted its map target"))?
        .clone();
    let baseline = neutral_capture(&mut story)?;
    let map_region = PixelRegion::anchor(&map);
    let baseline_water = water_ink(&baseline, map_region)?;
    demand(
        baseline_water >= 1_000,
        format!(
            "fixture basemap rendered only {baseline_water} water-like pixels before area mutation"
        ),
    )?;

    let _armed = story
        .click(Target::AddMapArea)?
        .within(instant_budget())
        .until(shows::area_drawing(true) & shows::basemap_tiles_at_least(1))?;
    let selection = selection_geometry(map.rect)?;
    let press = story
        .session()
        .button_down(selection.from.0, selection.from.1, Button::Primary)?;
    let _pressed = story
        .reaction(press)
        .within(instant_budget())
        .next_frame()?;
    let motion = story.session().move_to(selection.to.0, selection.to.1)?;
    let _previewed = story
        .reaction(motion)
        .within(instant_budget())
        .until(shows::area_drawing(true))?;
    let preview = story.capture()?;
    demand(
        baseline.difference_region(&preview, selection.region, 4)? >= 0.003,
        "dragging a map area produced no immediate rendered rectangle",
    )?;
    let release = story.session().button_up(Button::Primary)?;
    let added = story
        .reaction(release)
        .within(instant_budget())
        .until(
            shows::areas(2)
                & shows::area_drawing(false)
                & shows::corpus(CorpusPhase::Updating)
                & shows::basemap_tiles_at_least(1),
        )?
        .into_value();
    demand(
        added.state.saved_trails == 1,
        "adding a map area unloaded the saved trail Library",
    )?;
    assert_basemap_frame(
        &story.capture()?,
        map_region,
        baseline_water,
        "adding a map area",
    )?;
    let _ready = await_corpus_retaining_basemap(&mut story, 2)?;
    assert_basemap_retained(&mut story, baseline_water, "installing the added map area")?;

    rename_and_resize_second_area(&mut story, harness, baseline_water)?;

    exercise_refresh(&mut story)?;
    if let Some(artifacts) = harness.artifacts {
        story
            .capture()?
            .save_png(artifacts.join("story-1-area-lifecycle.png"))?;
    }
    app.terminate()
}

fn rename_and_resize_second_area(
    story: &mut TrailStory<'_, '_>,
    harness: &Harness<'_>,
    baseline_water: usize,
) -> Result<()> {
    let before = region_ledger(harness)?;
    demand(
        before.len() == 2,
        "second map area did not reach durable project configuration",
    )?;
    rename_second_area(story)?;
    demand(
        area_name(harness, &before[1].id)?.as_deref() == Some(AREA_NAME),
        "map-area rename witness advanced before durable configuration",
    )?;
    resize_second_area(story, baseline_water)?;
    let _ready = await_corpus_retaining_basemap(story, 2)?;
    assert_basemap_retained(story, baseline_water, "installing the resized map area")?;
    let after = region_ledger(harness)?;
    demand(
        after.len() == 2 && after[0] == before[0] && after[1] != before[1],
        "corner drag did not replace exactly the selected durable map area",
    )?;
    demand(
        area_name(harness, &after[1].id)?.as_deref() == Some(AREA_NAME),
        "resizing a named map area discarded or mis-keyed its name",
    )
}

fn rename_second_area(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let _opened = story
        .click(Target::AreaRename(1))?
        .within(instant_budget())
        .until(shows::rename(true) & shows::text_focused())?;
    let _typed = story
        .replace_text(Target::AreaRenameField(1), AREA_NAME, shows::text_focused())?
        .next_frame()?;
    let _committed = story
        .key(Key::Return)?
        .within(durable_budget())
        .until(shows::rename(false))?;
    Ok(())
}

fn exercise_refresh(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let browse = story.wait(shows::view(View::Browse) & shows::library(1))?;
    let saved = first_anchor(
        &browse,
        TargetClass::LibraryTrail,
        "saved trail vanished before corpus refresh",
    )?;
    let focused = story
        .click_anchor(&saved)?
        .until(shows::view(View::FocusSaved))?
        .into_value();
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
        .within(instant_budget())
        .until(shows::corpus(CorpusPhase::Updating) & shows::view(View::FocusSaved))?;
    let refreshed = await_corpus_retaining_basemap(story, 2)?;
    demand(
        refreshed.state.view == View::FocusSaved && refreshed.state.saved_trails == 1,
        "refreshing the corpus unloaded or unfocused the saved trail",
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

fn resize_second_area(story: &mut TrailStory<'_, '_>, baseline_water: usize) -> Result<()> {
    let target = Target::AreaHandle {
        slot: 1,
        corner: AreaCorner::Southeast,
    };
    let handle = story.anchor(target)?.center();
    let map = story.anchor(Target::Map)?;
    let [x0, y0, x1, y1] = map.rect;
    let map_region = PixelRegion::anchor(&map);
    let destination = screen_point([
        f64::from((f32::from(handle.0) + 44.0).min(x1 - 18.0).max(x0 + 18.0)),
        f64::from((f32::from(handle.1) + 34.0).min(y1 - 18.0).max(y0 + 18.0)),
    ])?;
    demand(
        destination != handle,
        "fixture viewport left no room to resize the second map area",
    )?;
    let baseline = neutral_capture(story)?;
    let mutation = PixelRegion::new(
        i32::from(handle.0.min(destination.0)) - 18,
        i32::from(handle.1.min(destination.1)) - 18,
        i32::from(handle.0.max(destination.0)) + 18,
        i32::from(handle.1.max(destination.1)) + 18,
    );

    let press = story
        .session()
        .button_down(handle.0, handle.1, Button::Primary)?;
    let _captured = story
        .reaction(press)
        .within(instant_budget())
        .until(shows::area_resizing(Some((1, AreaCorner::Southeast))))?;
    let motion = story.session().move_to(destination.0, destination.1)?;
    let _previewed = story
        .reaction(motion)
        .within(instant_budget())
        .until(shows::area_resizing(Some((1, AreaCorner::Southeast))))?;
    let preview = story.capture()?;
    demand(
        baseline.difference_region(&preview, mutation, 4)? >= 0.003,
        "dragging an area corner produced no rendered resize preview",
    )?;
    let release = story.session().button_up(Button::Primary)?;
    let _committed = story.reaction(release).within(instant_budget()).until(
        shows::area_resizing(None)
            & shows::areas(2)
            & shows::corpus(CorpusPhase::Updating)
            & shows::basemap_tiles_at_least(1),
    )?;
    assert_basemap_frame(
        &story.capture()?,
        map_region,
        baseline_water,
        "resizing a map area",
    )
}

struct Selection {
    from: (i16, i16),
    to: (i16, i16),
    region: PixelRegion,
}

fn selection_geometry(rect: [f32; 4]) -> Result<Selection> {
    let [x0, y0, x1, y1] = rect;
    let center = (f32::midpoint(x0, x1), f32::midpoint(y0, y1));
    let from = screen_point([f64::from(center.0 - 64.0), f64::from(center.1 - 48.0)])?;
    let to = screen_point([f64::from(center.0 + 64.0), f64::from(center.1 + 48.0)])?;
    let region = PixelRegion::new(
        i32::from(from.0) - 8,
        i32::from(from.1) - 8,
        i32::from(to.0) + 8,
        i32::from(to.1) + 8,
    );
    Ok(Selection { from, to, region })
}

fn assert_basemap_retained(
    story: &mut TrailStory<'_, '_>,
    baseline_water: usize,
    operation: &str,
) -> Result<()> {
    let map = story.anchor(Target::Map)?;
    let frame = neutral_capture(story)?;
    assert_basemap_frame(&frame, PixelRegion::anchor(&map), baseline_water, operation)
}

fn assert_basemap_frame(
    frame: &Frame,
    region: PixelRegion,
    baseline_water: usize,
    operation: &str,
) -> Result<()> {
    let actual = water_ink(frame, region)?;
    demand(
        actual.saturating_mul(4) >= baseline_water.saturating_mul(3),
        format!(
            "{operation} blanked the fixture basemap: {actual} water-like pixels remain from {baseline_water}"
        ),
    )
}

fn await_corpus_retaining_basemap(
    story: &mut TrailStory<'_, '_>,
    regions: usize,
) -> Result<crate::harness::TrailFrame> {
    let terminal = story.wait_within(
        Duration::from_secs(30),
        shows::condition("corpus completion or a blank basemap", |state| {
            state.map.as_ref().is_some_and(|map| map.basemap_tiles == 0)
                || state
                    .search
                    .as_ref()
                    .is_some_and(|search| search.corpus == CorpusPhase::Idle)
        }),
    )?;
    let basemap_tiles = terminal
        .state
        .map
        .as_ref()
        .map_or(0, |map| map.basemap_tiles);
    demand(
        basemap_tiles > 0,
        "the basemap lost every presented tile during corpus replacement",
    )?;
    demand(
        terminal
            .state
            .search
            .as_ref()
            .is_some_and(|search| search.corpus == CorpusPhase::Idle)
            && terminal
                .state
                .areas
                .as_ref()
                .is_some_and(|areas| areas.regions == regions)
            && terminal.state.saved_trails == 1,
        "corpus replacement reached an invalid terminal workbench state",
    )?;
    Ok(terminal)
}

fn neutral_capture(story: &mut TrailStory<'_, '_>) -> Result<Frame> {
    let motion = story.session().move_to(4, 4)?;
    let _neutral = story.reaction(motion).next_frame()?;
    story.capture()
}

fn water_ink(frame: &Frame, region: PixelRegion) -> Result<usize> {
    let crop = frame.crop(region)?;
    Ok(crop
        .rgba()
        .chunks_exact(4)
        .filter(|pixel| {
            i16::from(pixel[2]) - i16::from(pixel[0]) >= 28
                && i16::from(pixel[1]) - i16::from(pixel[0]) >= 12
        })
        .count())
}

#[derive(Clone, Debug, PartialEq)]
struct RegionReceipt {
    id: String,
    bounds: [f64; 4],
}

fn region_ledger(harness: &Harness<'_>) -> Result<Vec<RegionReceipt>> {
    let config = harness
        .testbed
        .read_private_to_string("discover-loop/trailgen.toml")?
        .parse::<toml::Table>()
        .map_err(|error| crate::harness::verdict(format!("parse project config: {error}")))?;
    config
        .get("trail_data")
        .and_then(toml::Value::as_table)
        .and_then(|trail_data| trail_data.get("regions"))
        .and_then(toml::Value::as_array)
        .ok_or_else(|| crate::harness::verdict("project config omitted trail-data regions"))?
        .iter()
        .map(|region| {
            let table = region.as_table().ok_or_else(|| {
                crate::harness::verdict("project config carried a non-table region")
            })?;
            let id = table
                .get("id")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| crate::harness::verdict("project region omitted its identity"))?
                .to_owned();
            let bounds = table
                .get("bounds")
                .and_then(toml::Value::as_table)
                .ok_or_else(|| crate::harness::verdict("project region omitted its bounds"))?;
            let coordinate = |name| {
                bounds
                    .get(name)
                    .and_then(toml::Value::as_float)
                    .ok_or_else(|| {
                        crate::harness::verdict(format!(
                            "project region omitted floating-point `{name}`"
                        ))
                    })
            };
            Ok(RegionReceipt {
                id,
                bounds: [
                    coordinate("west")?,
                    coordinate("south")?,
                    coordinate("east")?,
                    coordinate("north")?,
                ],
            })
        })
        .collect()
}

fn area_name(harness: &Harness<'_>, id: &str) -> Result<Option<String>> {
    let config = harness
        .testbed
        .read_private_to_string("discover-loop/trailgen.toml")?
        .parse::<toml::Table>()
        .map_err(|error| crate::harness::verdict(format!("parse project config: {error}")))?;
    Ok(config
        .get("trail_data")
        .and_then(toml::Value::as_table)
        .and_then(|trail_data| trail_data.get("region_names"))
        .and_then(toml::Value::as_table)
        .and_then(|names| names.get(id))
        .and_then(toml::Value::as_str)
        .map(str::to_owned))
}

fn verify_area_persistence(harness: &Harness<'_>) -> Result<()> {
    demand(
        region_ledger(harness)?.len() == 2,
        "map-area add/resize transaction did not survive process exit",
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
    let to = screen_point([f64::from(center.0 + 20.0), f64::from(center.1 + 20.0)])?;
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
    let _dormant = story.wait(shows::results_open(false))?;
    configure_search(story)?;
    let mut strike = story.key(Key::Return)?;
    let _progress =
        strike.until(shows::search(SearchPhase::Running) & shows::results_open(true))?;
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
    let _time_min = story
        .replace_text(Target::MovingTimeMin, "0.5", shows::text_focused())?
        .next_frame()?;
    let _time_max = story
        .replace_text(Target::MovingTimeMax, "4.0", shows::text_focused())?
        .next_frame()?;
    let _load = story
        .replace_text(Target::LowerLimbLoad, "8.0", shows::text_focused())?
        .next_frame()?;
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
    assert_solid_tube_is_coreless(&class, frame)?;
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
    Ok(())
}

fn assert_solid_tube_is_coreless(
    frame: &Frame,
    witness: &crate::harness::TrailFrame,
) -> Result<()> {
    let left = map_pixel(witness, [-98.508, 39.486])?;
    let right = map_pixel(witness, [-98.492, 39.486])?;
    let y = i32::midpoint(i32::from(left.1), i32::from(right.1));
    let region = PixelRegion::new(
        i32::from(left.0.min(right.0)),
        y - 4,
        i32::from(left.0.max(right.0)) + 1,
        y + 5,
    );
    let crop = frame.crop(region)?;
    let pixels = crop.rgba().chunks_exact(4);
    let colored = pixels
        .clone()
        .filter(|pixel| {
            pixel[0].saturating_sub(pixel[2]) >= 40 && pixel[1].saturating_sub(pixel[2]) >= 20
        })
        .count();
    let dark = pixels
        .filter(|pixel| pixel[0] <= 115 && pixel[1] <= 115 && pixel[2] <= 115)
        .count();
    demand(
        colored >= 48,
        "solid-core oracle could not find the fixture's easy / gravel tube",
    )?;
    demand(
        dark <= 8,
        format!("easy / gravel tube retained {dark} near-black core pixels"),
    )
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
    let index = read_json(harness.testbed, "discover-loop/cache/trails.json")?;
    demand(
        index["summary"]["edges"]
            .as_u64()
            .is_some_and(|edges| edges > 0),
        "provider acquisition committed an empty graph",
    )?;
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
    )?;
    demand(
        library["search"]["moving_time_s"]["min"].as_f64() == Some(0.5 * 3_600.0)
            && library["search"]["moving_time_s"]["max"].as_f64() == Some(4.0 * 3_600.0),
        "moving-time search window did not survive into the durable recipe",
    )?;
    demand(
        library["search"]["lower_limb_load_km"].as_f64() == Some(8.0),
        "lower-limb-load target did not survive into the durable recipe",
    )
}
