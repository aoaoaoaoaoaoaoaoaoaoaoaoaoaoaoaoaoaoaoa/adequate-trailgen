use std::{fs, time::Duration};

use egui_tester::{Button, Drag, Key, Modifiers, PerformanceBudget, Result, Stroke, Wheel};

use crate::harness::{
    Harness, click_budgeted, click_named, demand, durable_budget, instant_budget, replace_text,
    screen_point, search_reaction_budget, state_is, verdict,
};

const ROOT: &str = "/test/discover-loop";

pub fn run(harness: &Harness<'_>) -> Result<()> {
    harness.testbed.retain_on_failure("discover-loop")?;
    let app = harness.testbed.launch(harness.gui(None, false, true))?;
    let session = harness.session(&app)?;
    let mut probe = app.witness()?;
    let _first = session.wait_presented(&mut probe, Duration::from_secs(30))?;

    create_project(&session, &mut probe)?;
    acquire_region(&session, &mut probe)?;
    harness.fixtures.assert_harvested()?;
    find_and_keep(&session, &mut probe)?;
    verify_discovery(harness)?;

    if let Some(artifacts) = harness.artifacts {
        session
            .capture()?
            .save_png(artifacts.join("story-1-discover.png"))?;
    }
    app.terminate()?;
    drop(session);
    drop(app);

    let restarted = harness
        .testbed
        .launch(harness.gui(Some(ROOT), true, false))?;
    let session = harness.session(&restarted)?;
    let mut probe = restarted.witness()?;
    let restored = probe.wait(
        &restarted,
        Duration::from_secs(30),
        "saved discovery to survive a fresh process",
        |frame| {
            state_is(frame, "workspace", "trail")
                && frame.state["saved_trails"] == 1
                && frame
                    .anchors
                    .iter()
                    .any(|anchor| anchor.name.starts_with("library.trail/"))
        },
    )?;
    let trail = restored
        .anchors
        .iter()
        .find(|anchor| anchor.name.starts_with("library.trail/"))
        .cloned()
        .ok_or_else(|| verdict("restored Library row vanished"))?;
    let _opened = click_budgeted(
        &session,
        &mut probe,
        &trail,
        instant_budget(),
        "open the trail recovered after restart",
        |frame| state_is(frame, "view", "focus-saved"),
    )?;
    restarted.terminate()?;
    Ok(())
}

fn create_project(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
) -> Result<()> {
    let app = session.application();
    let deck = probe.wait(
        app,
        Duration::from_secs(10),
        "empty project deck",
        |frame| {
            state_is(frame, "workspace", "projects")
                && frame.anchor("projects.new.name").is_some()
                && frame.anchor("projects.new.parent").is_some()
        },
    )?;
    let name = deck
        .anchor("projects.new.name")
        .cloned()
        .ok_or_else(|| verdict("project deck omitted its name field"))?;
    let parent = deck
        .anchor("projects.new.parent")
        .cloned()
        .ok_or_else(|| verdict("project deck omitted its parent field"))?;
    let _name = replace_text(session, probe, &name, "Discover Loop")?;
    let parent_typed = replace_text(session, probe, &parent, "/test")?;
    let armed = probe.wait_budgeted(
        app,
        &parent_typed,
        instant_budget(),
        "arm project creation after completing its fields",
        |frame| frame.anchor("projects.new.create").is_some(),
    )?;
    let create = armed
        .value()
        .anchor("projects.new.create")
        .cloned()
        .ok_or_else(|| verdict("armed project creation control vanished"))?;
    let _created = click_budgeted(
        session,
        probe,
        &create,
        durable_budget(),
        "create a project through the project deck",
        |frame| state_is(frame, "workspace", "survey"),
    )?;
    Ok(())
}

fn acquire_region(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
) -> Result<()> {
    let app = session.application();
    let drawing = click_named(
        session,
        probe,
        "survey.add-area",
        instant_budget(),
        "arm map-area selection",
        |frame| frame.state["survey"]["drawing"] == true,
    )?;
    let map = drawing
        .value()
        .anchor("survey.map")
        .cloned()
        .ok_or_else(|| verdict("survey omitted its map canvas"))?;
    let [x0, y0, x1, y1] = map.rect;
    let center = (f32::midpoint(x0, x1), f32::midpoint(y0, y1));
    let from = screen_point([f64::from(center.0 - 13.0), f64::from(center.1 - 13.0)])?;
    let to = screen_point([f64::from(center.0 + 13.0), f64::from(center.1 + 13.0)])?;
    let drag = session.drag(
        from,
        to,
        Drag {
            duration: Duration::from_millis(120),
            ..Drag::default()
        },
    )?;
    let _started = probe.wait_budgeted(
        app,
        &drag,
        PerformanceBudget::new(Duration::from_millis(600))
            .through_presentation()
            .timeout(Duration::from_secs(8)),
        "begin trail-data acquisition after selecting a region",
        |frame| frame.state["survey"]["acquiring"] == true && frame.state["survey"]["regions"] == 1,
    )?;
    let _ready = probe.wait(
        app,
        Duration::from_secs(30),
        "provider acquisition, indexing, and workbench promotion",
        |frame| state_is(frame, "workspace", "trail") && frame.state["candidates"] == 0,
    )?;
    Ok(())
}

fn find_and_keep(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
) -> Result<()> {
    configure_search(session, probe)?;
    search_and_keep(session, probe)
}

fn configure_search(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
) -> Result<()> {
    let app = session.application();
    let frame = probe.wait(
        app,
        Duration::from_secs(10),
        "acquired trail map",
        |frame| frame.anchor("map.canvas").is_some(),
    )?;
    let initial_scale = frame.state["map"]["world_points"]
        .as_f64()
        .ok_or_else(|| verdict("map witness omitted its scale"))?;
    let map = frame
        .anchor("map.canvas")
        .cloned()
        .ok_or_else(|| verdict("acquired project omitted its map"))?;
    let (cx, cy) = map.center();
    let _zoom = session.wheel(
        cx,
        cy,
        -7,
        Wheel {
            tick_duration: Duration::from_millis(24),
        },
    )?;
    let frame = probe.wait(
        app,
        Duration::from_secs(8),
        "zoom close enough to place a precise trailhead",
        |frame| {
            frame.state["map"]["world_points"]
                .as_f64()
                .is_some_and(|scale| scale >= initial_scale * 16.0)
        },
    )?;
    let map = frame
        .anchor("map.canvas")
        .cloned()
        .ok_or_else(|| verdict("zoomed project omitted its map"))?;
    let trailhead = crate::harness::map_pixel(&frame, [-98.5, 39.5])?;
    let placed =
        session.modified_click(trailhead.0, trailhead.1, Button::Primary, Modifiers::ALT)?;
    let placed = probe.wait_budgeted(
        app,
        &placed,
        instant_budget(),
        "place a trailhead with Alt-click",
        |frame| frame.state["search"]["trailhead"].is_array(),
    )?;
    let boundary = placed
        .value()
        .anchor("search.boundary")
        .cloned()
        .ok_or_else(|| verdict("search controls omitted the boundary tool"))?;
    let _armed = click_budgeted(
        session,
        probe,
        &boundary,
        instant_budget(),
        "arm the search boundary lasso",
        |frame| frame.anchor("map.canvas").is_some(),
    )?;
    let [x0, y0, x1, y1] = map.rect;
    let inset_x = (x1 - x0) * 0.15;
    let inset_y = (y1 - y0) * 0.15;
    let point = |x, y| screen_point([f64::from(x), f64::from(y)]);
    let knots = [
        point(x0 + inset_x, y0 + inset_y)?,
        point(x1 - inset_x, y0 + inset_y)?,
        point(x1 - inset_x, y1 - inset_y)?,
        point(x0 + inset_x, y1 - inset_y)?,
        point(x0 + inset_x, y0 + inset_y)?,
    ];
    let lasso = session.stroke(
        &knots,
        Stroke {
            steps_per_leg: 5,
            leg_duration: Duration::from_millis(45),
            ..Stroke::default()
        },
    )?;
    let _bounded = probe.wait_budgeted(
        app,
        &lasso,
        instant_budget(),
        "commit a free-hand search boundary",
        |frame| frame.state["search"]["boundary"] == true,
    )?;
    Ok(())
}

fn search_and_keep(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
) -> Result<()> {
    let app = session.application();
    let strike = session.key(Key::Return)?;
    let _progress = probe.wait_budgeted(
        app,
        &strike,
        search_reaction_budget(),
        "show search progress after Enter",
        |frame| frame.state["search"]["phase"] == "striking",
    )?;
    let _eager = probe.wait_budgeted(
        app,
        &strike,
        PerformanceBudget::new(Duration::from_secs(2))
            .through_presentation()
            .timeout(Duration::from_secs(12)),
        "promote the first useful candidate eagerly",
        |frame| {
            frame.state["candidates"]
                .as_u64()
                .is_some_and(|count| count >= 1)
        },
    )?;
    let complete = probe.wait(
        app,
        Duration::from_secs(20),
        "search to settle with retained candidates",
        |frame| {
            frame.state["search"]["phase"] == "idle"
                && frame.state["candidates"]
                    .as_u64()
                    .is_some_and(|count| count >= 1)
        },
    )?;
    let candidate = complete
        .anchors
        .iter()
        .find(|anchor| anchor.name.starts_with("results.candidate/"))
        .cloned()
        .ok_or_else(|| verdict("search produced no visible result tile"))?;
    let _focused = click_budgeted(
        session,
        probe,
        &candidate,
        instant_budget(),
        "inspect an eager search result",
        |frame| state_is(frame, "view", "focus-candidate"),
    )?;
    let _saved = click_named(
        session,
        probe,
        "focus.save",
        durable_budget(),
        "save the inspected candidate",
        |frame| state_is(frame, "view", "focus-saved") && frame.state["saved_trails"] == 1,
    )?;
    Ok(())
}

fn verify_discovery(harness: &Harness<'_>) -> Result<()> {
    let project = harness.testbed.private_path("discover-loop")?;
    let config = fs::read_to_string(project.join("trailgen.toml")).map_err(|source| {
        egui_tester::Error::Io {
            operation: "read discovered project config",
            path: project.join("trailgen.toml"),
            source,
        }
    })?;
    demand(
        config.contains("managed = true"),
        "discovered project is not managed",
    )?;
    demand(
        project.join("cache/graph.json").is_file(),
        "provider acquisition did not commit a graph",
    )?;
    demand(
        project.join("sources/osm").is_dir()
            && project.join("sources/usgs-national-trails").is_dir()
            && project.join("sources/mapzen-terrain").is_dir(),
        "discovered project omitted a provider receipt family",
    )?;
    let library = crate::harness::read_json(&project.join("library/index.json"))?;
    demand(
        library["trails"]
            .as_array()
            .is_some_and(|trails| trails.len() == 1),
        "saved discovery did not reach the durable Library",
    )?;
    Ok(())
}
