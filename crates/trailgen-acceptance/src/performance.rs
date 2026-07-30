use std::time::Duration;

use egui_tester::{CadenceBudget, CadenceReport, FrameProbe, Result, Stroke, Wheel, X11Session};

use crate::{
    harness::{Target, TrailFrame, TrailStory, verdict},
    observation::MapState,
};

pub struct PortfolioReport {
    pub pan: CadenceReport,
    pub zoom: CadenceReport,
    pub settled: TrailFrame,
}

pub fn pan_during_search(
    session: &X11Session<'_, '_>,
    frames: &FrameProbe,
    frame: &TrailFrame,
) -> Result<()> {
    let (cx, cy) = frame
        .anchor(&Target::Map.to_string())
        .ok_or_else(|| verdict("search progress omitted the map"))?
        .center();
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

pub fn stress_portfolio(
    story: &mut TrailStory<'_, '_>,
    frames: &FrameProbe,
    frame: &TrailFrame,
) -> Result<PortfolioReport> {
    let (cx, cy) = frame
        .anchor(&Target::Map.to_string())
        .ok_or_else(|| verdict("portfolio omitted the map canvas"))?
        .center();
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
    let pan = story.session().stroke(
        &knots,
        Stroke {
            steps_per_leg: 6,
            leg_duration: Duration::from_millis(60),
            ..Stroke::default()
        },
    )?;
    let trace = frames.trace(story.session().application(), &pan, Duration::from_secs(10))?;
    let pan = trace.adjudicate(
        "pan a twelve-candidate portfolio",
        cadence_budget().minimum_frames(28),
    )?;

    let wheel = |tick_duration| Wheel {
        tick_duration: Duration::from_millis(tick_duration),
    };
    let retreat = story.session().wheel(cx, cy, 5, wheel(20))?;
    let _retreated = frames.trace(
        story.session().application(),
        &retreat,
        Duration::from_secs(10),
    )?;
    let zoom_action = story.session().wheel(cx, cy, -10, wheel(28))?;
    let trace = frames.trace(
        story.session().application(),
        &zoom_action,
        Duration::from_secs(10),
    )?;
    let zoom = trace.adjudicate(
        "zoom a twelve-candidate portfolio",
        cadence_budget().minimum_frames(7),
    )?;
    let restore = story.session().wheel(cx, cy, 5, wheel(20))?;
    let _restored = frames.trace(
        story.session().application(),
        &restore,
        Duration::from_secs(10),
    )?;
    let settled = story.wait_stable(
        Duration::from_secs(8),
        Duration::from_millis(160),
        "map zoom kinetics to settle",
        viewport_fingerprint,
    )?;
    Ok(PortfolioReport { pan, zoom, settled })
}

fn viewport_fingerprint(frame: &TrailFrame) -> Option<[u64; 3]> {
    let MapState {
        center,
        world_points,
        ..
    } = frame.state.map?;
    Some([
        center[0].to_bits(),
        center[1].to_bits(),
        world_points.to_bits(),
    ])
}

fn cadence_budget() -> CadenceBudget {
    CadenceBudget::default()
        .p50(Duration::from_millis(40))
        .p95(Duration::from_millis(50))
        .worst(Duration::from_millis(180))
        .paint_p95(Duration::from_millis(40))
}
