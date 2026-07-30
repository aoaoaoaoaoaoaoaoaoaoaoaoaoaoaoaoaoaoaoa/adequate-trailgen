use std::time::Duration;

use egui_tester::{
    Button, CadenceBudget, Key, Modifiers, PerformanceBudget, ProbeFrame, Result, Stroke, Wheel,
};

use crate::harness::{
    Harness, click_budgeted, click_named, decode_state, demand, durable_budget, instant_budget,
    map_pixel, replace_text, search_reaction_budget, state_is, verdict,
};

const ROOT: &str = "/test/compare";

pub fn run(harness: &Harness<'_>) -> Result<()> {
    harness.seed_project(ROOT, "/test/fixtures/dense_network.geojson", false)?;
    harness
        .testbed
        .retain_on_failure("compare/library/index.json")?;
    let app = harness
        .testbed
        .launch(harness.gui(Some(ROOT), true, false))?;
    let session = harness.session(&app)?;
    let mut probe = app.witness()?;
    let _first = session.wait_presented(&mut probe, Duration::from_secs(30))?;
    let frames = app.frames()?;

    let frame = probe.wait(
        &app,
        Duration::from_secs(15),
        "dense comparison graph",
        |frame| frame.anchor("map.canvas").is_some(),
    )?;
    let trailhead = map_pixel(&frame, [-105.0, 40.0])?;
    let placed =
        session.modified_click(trailhead.0, trailhead.1, Button::Primary, Modifiers::ALT)?;
    let _placed = probe.wait_budgeted(
        &app,
        &placed,
        instant_budget(),
        "place the dense-workload trailhead",
        |frame| frame.state["search"]["trailhead"].is_array(),
    )?;
    let strike = session.key(Key::Return)?;
    let progress = probe.wait_budgeted(
        &app,
        &strike,
        search_reaction_budget(),
        "enter a visible dense-search progress state",
        |frame| frame.state["search"]["phase"] == "striking",
    )?;
    pan_during_search(&session, &frames, progress.value())?;
    let eager = probe.wait_budgeted(
        &app,
        &strike,
        PerformanceBudget::new(Duration::from_secs(2))
            .through_presentation()
            .timeout(Duration::from_secs(12)),
        "eagerly promote a dense-search candidate",
        |frame| {
            frame.state["candidates"]
                .as_u64()
                .is_some_and(|count| count >= 1)
        },
    )?;
    demand(
        eager.value().state["search"]["phase"] == "striking"
            || eager.value().state["candidates"] == 12,
        "first candidate was not promoted until after an incomplete search ended",
    )?;
    let complete = probe.wait(
        &app,
        Duration::from_secs(30),
        "a full twelve-candidate portfolio",
        |frame| frame.state["search"]["phase"] == "idle" && frame.state["candidates"] == 12,
    )?;

    let (pan_report, zoom_report, settled) =
        stress_portfolio(&session, &frames, &mut probe, &complete)?;
    let returned = focus_and_return(&session, &mut probe, &settled)?;
    revise_and_stop(&session, &mut probe, &returned)?;
    choose_and_save(&session, &mut probe)?;

    if let Some(artifacts) = harness.artifacts {
        session
            .capture()?
            .save_png(artifacts.join("story-3-compare.png"))?;
    }
    app.terminate()?;
    println!(
        "comparison cadence: pan p50={:?} p95={:?} worst={:?}; zoom p50={:?} p95={:?} worst={:?}",
        pan_report.p50,
        pan_report.p95,
        pan_report.worst,
        zoom_report.p50,
        zoom_report.p95,
        zoom_report.worst,
    );
    Ok(())
}

fn choose_and_save(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
) -> Result<()> {
    let app = session.application();
    let final_results = probe.wait(
        app,
        Duration::from_secs(30),
        "retained candidates after force, ban, and stop",
        |frame| {
            state_is(frame, "view", "browse")
                && frame.state["search"]["phase"] == "idle"
                && frame.state["candidates"]
                    .as_u64()
                    .is_some_and(|count| count >= 1)
        },
    )?;
    let candidate = first_candidate(&final_results)?;
    let _focus = click_budgeted(
        session,
        probe,
        &candidate,
        instant_budget(),
        "focus a compared alternative",
        |frame| state_is(frame, "view", "focus-candidate"),
    )?;
    let _saved = click_named(
        session,
        probe,
        "focus.save",
        durable_budget(),
        "save the chosen alternative",
        |frame| state_is(frame, "view", "focus-saved") && frame.state["saved_trails"] == 1,
    )?;

    Ok(())
}

fn pan_during_search(
    session: &egui_tester::X11Session<'_, '_>,
    frames: &egui_tester::FrameProbe,
    frame: &ProbeFrame,
) -> Result<()> {
    let map = frame
        .anchor("map.canvas")
        .ok_or_else(|| verdict("search progress omitted the map"))?;
    let (cx, cy) = map.center();
    let knots = [
        (cx - 30, cy - 20),
        (cx + 30, cy - 20),
        (cx + 30, cy + 20),
        (cx - 30, cy + 20),
        (cx - 30, cy - 20),
    ];
    let action = session.stroke(
        &knots,
        Stroke {
            steps_per_leg: 4,
            leg_duration: Duration::from_millis(60),
            ..Stroke::default()
        },
    )?;
    let trace = frames.trace(session.application(), &action, Duration::from_secs(8))?;
    let _report = trace.adjudicate(
        "pan while search is preparing",
        cadence_budget().minimum_frames(6),
    )?;
    Ok(())
}

fn stress_portfolio(
    session: &egui_tester::X11Session<'_, '_>,
    frames: &egui_tester::FrameProbe,
    probe: &mut egui_tester::JsonProbe,
    frame: &ProbeFrame,
) -> Result<(
    egui_tester::CadenceReport,
    egui_tester::CadenceReport,
    ProbeFrame,
)> {
    let map = frame
        .anchor("map.canvas")
        .ok_or_else(|| verdict("portfolio omitted the map canvas"))?;
    let (cx, cy) = map.center();
    let mut knots = Vec::with_capacity(21);
    for _ in 0..5 {
        knots.extend([
            (cx - 110, cy - 70),
            (cx + 110, cy - 70),
            (cx + 110, cy + 70),
            (cx - 110, cy + 70),
        ]);
    }
    knots.push((cx - 110, cy - 70));
    let pan = session.stroke(
        &knots,
        Stroke {
            steps_per_leg: 6,
            leg_duration: Duration::from_millis(60),
            ..Stroke::default()
        },
    )?;
    let pan_trace = frames.trace(session.application(), &pan, Duration::from_secs(10))?;
    let pan_report = pan_trace.adjudicate(
        "pan a twelve-candidate portfolio",
        cadence_budget().minimum_frames(28),
    )?;

    let retreat = session.wheel(
        cx,
        cy,
        5,
        Wheel {
            tick_duration: Duration::from_millis(20),
        },
    )?;
    let _retreated = frames.trace(session.application(), &retreat, Duration::from_secs(10))?;
    let zoom = session.wheel(
        cx,
        cy,
        -10,
        Wheel {
            tick_duration: Duration::from_millis(28),
        },
    )?;
    let zoom_trace = frames.trace(session.application(), &zoom, Duration::from_secs(10))?;
    let zoom_report = zoom_trace.adjudicate(
        "zoom a twelve-candidate portfolio",
        cadence_budget().minimum_frames(7),
    )?;
    let restore = session.wheel(
        cx,
        cy,
        5,
        Wheel {
            tick_duration: Duration::from_millis(20),
        },
    )?;
    let _restored = frames.trace(session.application(), &restore, Duration::from_secs(10))?;
    let settled = probe.wait_stable(
        session.application(),
        Duration::from_secs(8),
        Duration::from_millis(160),
        "map zoom kinetics to settle",
        viewport_fingerprint,
    )?;
    Ok((pan_report, zoom_report, settled))
}

fn focus_and_return(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
    browse: &ProbeFrame,
) -> Result<ProbeFrame> {
    let baseline = decode_state(browse)?
        .map
        .ok_or_else(|| verdict("portfolio browse omitted its viewport"))?;
    let candidate = first_candidate(browse)?;
    let _focused = click_budgeted(
        session,
        probe,
        &candidate,
        instant_budget(),
        "focus one alternative",
        |frame| state_is(frame, "view", "focus-candidate"),
    )?;
    let returned = click_named(
        session,
        probe,
        "focus.back",
        instant_budget(),
        "return to the exact comparison viewport",
        |frame| state_is(frame, "view", "browse"),
    )?;
    let restored = decode_state(returned.value())?
        .map
        .ok_or_else(|| verdict("returned comparison omitted its viewport"))?;
    demand(
        near_map(restored.center, baseline.center)
            && (restored.world_points - baseline.world_points).abs() <= 1.0e-6,
        format!("focus return changed viewport from {baseline:?} to {restored:?}"),
    )?;
    Ok(returned.into_value())
}

fn revise_and_stop(
    session: &egui_tester::X11Session<'_, '_>,
    probe: &mut egui_tester::JsonProbe,
    frame: &ProbeFrame,
) -> Result<()> {
    let app = session.application();
    let maximum = frame
        .anchor("search.distance.max")
        .cloned()
        .ok_or_else(|| verdict("search recipe omitted maximum distance"))?;
    let _typed = replace_text(session, probe, &maximum, "11.0")?;
    let commit = session.key(Key::Return)?;
    let _scheduled = probe.wait_budgeted(
        app,
        &commit,
        PerformanceBudget::new(Duration::from_millis(700))
            .through_presentation()
            .timeout(Duration::from_secs(8)),
        "warm-revise after changing search distance",
        |frame| {
            frame.state["search"]["revision_scheduled"] == true
                || frame.state["search"]["phase"] == "striking"
        },
    )?;
    let _revised = probe.wait(
        app,
        Duration::from_secs(30),
        "warmed parameter revision",
        |frame| {
            frame.state["search"]["phase"] == "idle"
                && frame.state["candidates"]
                    .as_u64()
                    .is_some_and(|count| count >= 1)
        },
    )?;

    let frame = probe.read()?;
    let require = map_pixel(&frame, [-104.997, 40.0])?;
    let required = session.click(require.0, require.1, Button::Primary)?;
    let _edict = probe.wait_budgeted(
        app,
        &required,
        instant_budget(),
        "require a clicked trail segment",
        |frame| frame.state["search"]["required"] == 1,
    )?;
    let striking = probe.wait(
        app,
        Duration::from_secs(8),
        "required-segment revision to start",
        |frame| {
            frame.state["search"]["phase"] == "striking"
                && frame.state["candidates"]
                    .as_u64()
                    .is_some_and(|count| count >= 1)
                && frame.anchor("search.stop").is_some()
        },
    )?;
    let retained = striking.state["candidates"].as_u64().unwrap_or_default();
    let stop = striking
        .anchor("search.stop")
        .cloned()
        .ok_or_else(|| verdict("active search omitted Stop"))?;
    let stopped = click_budgeted(
        session,
        probe,
        &stop,
        PerformanceBudget::new(Duration::from_millis(500))
            .through_presentation()
            .timeout(Duration::from_secs(8)),
        "stop a revision without discarding promoted candidates",
        |frame| frame.state["search"]["phase"] == "idle",
    )?;
    demand(
        stopped.value().state["candidates"]
            .as_u64()
            .is_some_and(|count| count >= retained && count >= 1),
        "Stop Search discarded promoted candidates",
    )?;

    let frame = stopped.into_value();
    let forbid = map_pixel(&frame, [-105.0, 40.003])?;
    let forbidden =
        session.modified_click(forbid.0, forbid.1, Button::Primary, Modifiers::SHIFT)?;
    let _banned = probe.wait_budgeted(
        app,
        &forbidden,
        instant_budget(),
        "ban a Shift-clicked trail segment",
        |frame| frame.state["search"]["forbidden"] == 1,
    )?;
    Ok(())
}

fn first_candidate(frame: &ProbeFrame) -> Result<egui_tester::Anchor> {
    frame
        .anchors
        .iter()
        .find(|anchor| anchor.name.starts_with("results.candidate/"))
        .cloned()
        .ok_or_else(|| verdict("candidate portfolio omitted its first tile"))
}

fn near_map(left: [f64; 2], right: [f64; 2]) -> bool {
    (left[0] - right[0]).abs() <= 1.0e-12 && (left[1] - right[1]).abs() <= 1.0e-12
}

fn viewport_fingerprint(frame: &ProbeFrame) -> Option<[u64; 3]> {
    let map = decode_state(frame).ok()?.map?;
    Some([
        map.center[0].to_bits(),
        map.center[1].to_bits(),
        map.world_points.to_bits(),
    ])
}

fn cadence_budget() -> CadenceBudget {
    CadenceBudget::default()
        .p50(Duration::from_millis(40))
        .p95(Duration::from_millis(50))
        .worst(Duration::from_millis(180))
        .paint_p95(Duration::from_millis(40))
}
