mod compare;
mod discover;
mod manual;
mod prepare;
mod refine;

use std::time::Duration;

use egui_tester::{Backend, Frame, Result, WindowQuery};

use crate::{
    harness::{DataMode, Harness, RunClass, TITLE_FRAGMENT, demand},
    observation::{Observation, View, Workspace},
};

type UserStory = for<'a> fn(&Harness<'a>) -> Result<()>;

pub fn smoke(harness: &Harness<'_>, backend: Backend) -> Result<()> {
    match backend {
        Backend::X11(_) => smoke_x11(harness),
        Backend::Wayland(_) => smoke_wayland(harness),
    }
}

fn smoke_x11(harness: &Harness<'_>) -> Result<()> {
    let app = harness.launch_uninstrumented_smoke()?;
    let session = harness.testbed.x11_session(
        &app,
        WindowQuery::title_contains(TITLE_FRAGMENT),
        Duration::from_secs(30),
    )?;
    session.focus()?;
    let first = session.capture()?;
    let frame = if visible(&first) {
        first
    } else {
        session.wait_changed(&first, 0.001, 2, Duration::from_secs(30))?
    };
    demand(
        visible(&frame),
        "uninstrumented Trailgen rendered only black pixels",
    )?;
    app.terminate()
}

fn smoke_wayland(harness: &Harness<'_>) -> Result<()> {
    let app = harness.launch_gui(None, DataMode::Offline, RunClass::Functional)?;
    let mut witness = app.witness()?.typed::<Observation>();
    let presented = witness.wait_surface_presented(&app, Duration::from_secs(30))?;
    demand(
        presented.state.contract == trailgen_contract::UI_FINGERPRINT,
        format!(
            "Trailgen UI contract mismatch: expected {}, observed {}",
            trailgen_contract::UI_FINGERPRINT,
            presented.state.contract
        ),
    )?;
    demand(
        presented.state.workspace == Workspace::Projects && presented.state.view == View::Projects,
        format!(
            "Wayland Trailgen opened {:?}/{:?} instead of the project deck",
            presented.state.workspace, presented.state.view
        ),
    )?;
    app.wait_until(
        Duration::from_secs(30),
        "nonblack pixels on the headless Wayland output",
        || Ok(visible(&harness.testbed.capture_wayland()?)),
    )?;
    app.terminate()
}

fn visible(frame: &Frame) -> bool {
    frame
        .rgba()
        .chunks_exact(4)
        .any(|pixel| pixel[..3] != [0, 0, 0])
}

pub fn run(harness: &Harness<'_>, selected: Option<&str>) -> Result<()> {
    let stories: [(&str, UserStory); 5] = [
        ("discover", discover::run),
        ("refine", refine::run),
        ("compare", compare::run),
        ("manual", manual::run),
        ("prepare", prepare::run),
    ];
    let mut ran = 0;
    for (name, story) in stories {
        if selected.is_none_or(|selected| selected == name) {
            story(harness)?;
            ran += 1;
        }
    }
    if ran == 0 {
        return Err(egui_tester::Error::Verdict {
            detail: format!(
                "unknown Trailgen story `{}`; expected discover, refine, compare, manual, or prepare",
                selected.unwrap_or_default()
            ),
        });
    }
    println!(
        "trailgen acceptance passed: {ran} user stor{}",
        if ran == 1 { "y" } else { "ies" }
    );
    Ok(())
}
