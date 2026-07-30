use std::time::Duration;

use egui_tester::{Button, Key, Modifiers, ProbeFrame, Result};

use crate::harness::{
    Harness, click_named, demand, durable_budget, instant_budget, map_pixel, read_json,
    screen_point, state_is, verdict,
};

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
    let app = harness
        .testbed
        .launch(harness.gui(Some(ROOT), true, false))?;
    let session = harness.session(&app)?;
    let mut probe = app.witness()?;
    let _first = session.wait_presented(&mut probe, Duration::from_secs(30))?;

    let (before_signature, trailhead) = draw_open_route(&session, &mut probe)?;
    close_reverse_and_save(&session, &mut probe, before_signature, trailhead)?;
    verify_saved(harness)?;

    if let Some(artifacts) = harness.artifacts {
        session
            .capture()?
            .save_png(artifacts.join("story-4-manual.png"))?;
    }
    app.terminate()?;
    drop(session);
    drop(app);
    verify_restart(harness)
}

fn draw_open_route(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
) -> Result<(u64, [f64; 2])> {
    let app = session.application();
    let _editor = click_named(
        session,
        probe,
        "search.manual",
        instant_budget(),
        "start a trail without running a search",
        |frame| {
            state_is(frame, "view", "edit")
                && frame.state["editor"]["origin"] == "new"
                && frame.state["editor"]["support_points"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
        },
    )?;
    let _first = add_support(session, probe, SUPPORTS[0], 1)?;
    let second = add_support(session, probe, SUPPORTS[1], 2)?;
    demand(
        support(second.value(), 1).is_some_and(|point| near(point, SUPPORTS[1])),
        "manual editor could not retain a support in the middle of an edge",
    )?;

    let undo = session.chord(Modifiers::CTRL, Key::Character('z'))?;
    let _undone = probe.wait_budgeted(
        app,
        &undo,
        instant_budget(),
        "undo a manually inserted support",
        |frame| support_count(frame) == 1 && frame.state["editor"]["redo_depth"] == 1,
    )?;
    let redo = session.chord(Modifiers::CTRL, Key::Character('y'))?;
    let _redone = probe.wait_budgeted(
        app,
        &redo,
        instant_budget(),
        "redo a manually inserted support",
        |frame| {
            support_count(frame) == 2
                && support(frame, 1).is_some_and(|point| near(point, SUPPORTS[1]))
        },
    )?;

    for (slot, coordinate) in SUPPORTS.iter().copied().enumerate().skip(2) {
        let _added = add_support(session, probe, coordinate, slot + 1)?;
    }
    let before_reverse = probe.read()?;
    let before_signature = signature(&before_reverse)
        .ok_or_else(|| verdict("ready manual route omitted its signature"))?;
    let trailhead =
        support(&before_reverse, 0).ok_or_else(|| verdict("manual route omitted support 0"))?;
    Ok((before_signature, trailhead))
}

fn close_reverse_and_save(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
    before_signature: u64,
    trailhead: [f64; 2],
) -> Result<()> {
    let _closed = click_named(
        session,
        probe,
        "editor.close-loop",
        instant_budget(),
        "close a manually drawn route",
        |frame| {
            frame.state["editor"]["shape"] == "loop"
                && frame.state["editor"]["ready"] == true
                && support(frame, 0).is_some_and(|point| near(point, trailhead))
        },
    )?;
    let closed = probe.read()?;
    let closed_signature =
        signature(&closed).ok_or_else(|| verdict("closed loop omitted its route signature"))?;
    let _reversed = click_named(
        session,
        probe,
        "editor.reverse",
        instant_budget(),
        "reverse a manual loop without moving its trailhead",
        |frame| {
            frame.state["editor"]["shape"] == "loop"
                && frame.state["editor"]["ready"] == true
                && support(frame, 0).is_some_and(|point| near(point, trailhead))
                && signature(frame).is_some_and(|signature| signature != closed_signature)
        },
    )?;
    demand(
        before_signature != closed_signature,
        "closing the manual route did not alter its realized walk",
    )?;

    exercise_profile(session, probe)?;
    let _saved = click_named(
        session,
        probe,
        "editor.save",
        durable_budget(),
        "save a manual loop",
        |frame| state_is(frame, "view", "focus-saved") && frame.state["saved_trails"] == 1,
    )?;
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
    let restarted = harness
        .testbed
        .launch(harness.gui(Some(ROOT), true, false))?;
    let mut probe = restarted.witness()?;
    let _restored = probe.wait(
        &restarted,
        Duration::from_secs(30),
        "manual loop in the restarted Library",
        |frame| {
            frame.state["saved_trails"] == 1
                && frame
                    .anchors
                    .iter()
                    .any(|anchor| anchor.name.starts_with("library.trail/"))
        },
    )?;
    restarted.terminate()?;
    Ok(())
}

fn add_support(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
    coordinate: [f64; 2],
    expected: usize,
) -> Result<egui_tester::Timed<ProbeFrame>> {
    let frame = probe.read()?;
    let target = map_pixel(&frame, coordinate)?;
    let receipt = session.click(target.0, target.1, Button::Primary)?;
    probe.wait_budgeted(
        session.application(),
        &receipt,
        instant_budget(),
        "add a manual support point",
        |frame| {
            support_count(frame) == expected
                && support(frame, expected - 1).is_some_and(|point| near(point, coordinate))
                && (expected < 2 || frame.state["editor"]["ready"] == true)
        },
    )
}

fn exercise_profile(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
) -> Result<()> {
    let app = session.application();
    let profile = probe.wait_anchor(app, "profile.canvas", Duration::from_secs(6))?;
    let [x0, y0, x1, y1] = profile.rect;
    let target = screen_point([
        f64::from((x1 - x0).mul_add(0.62, x0)),
        f64::from(f32::midpoint(y0, y1)),
    ])?;
    let hover = session.move_to(target.0, target.1)?;
    let _hovered = probe.wait_budgeted(
        app,
        &hover,
        instant_budget(),
        "link profile hover to its map marker",
        |frame| {
            frame.state["profile"]["marker"].is_array()
                && frame.state["profile"]["locked_distance_m"].is_null()
        },
    )?;
    let lock = session.click(target.0, target.1, Button::Primary)?;
    let _locked = probe.wait_budgeted(
        app,
        &lock,
        instant_budget(),
        "lock the elevation cursor",
        |frame| frame.state["profile"]["locked_distance_m"].is_number(),
    )?;
    let release = session.click(target.0, target.1, Button::Secondary)?;
    let _released = probe.wait_budgeted(
        app,
        &release,
        instant_budget(),
        "release the elevation cursor",
        |frame| frame.state["profile"]["locked_distance_m"].is_null(),
    )?;
    Ok(())
}

fn support_count(frame: &ProbeFrame) -> usize {
    frame.state["editor"]["support_points"]
        .as_array()
        .map_or(0, Vec::len)
}

fn support(frame: &ProbeFrame, slot: usize) -> Option<[f64; 2]> {
    let point = frame.state["editor"]["support_points"]
        .as_array()?
        .get(slot)?;
    let point = point.as_array()?;
    Some([point.first()?.as_f64()?, point.get(1)?.as_f64()?])
}

fn signature(frame: &ProbeFrame) -> Option<u64> {
    frame.state["editor"]["route_signature"].as_u64()
}

fn near(left: [f64; 2], right: [f64; 2]) -> bool {
    (left[0] - right[0]).abs() <= 5.0e-5 && (left[1] - right[1]).abs() <= 5.0e-5
}
