mod compare;
mod discover;
mod manual;
mod prepare;
mod refine;

use egui_tester::Result;

use crate::harness::{Harness, TITLE_FRAGMENT, demand};

type UserStory = for<'a> fn(&Harness<'a>) -> Result<()>;

pub fn smoke(harness: &Harness<'_>) -> Result<()> {
    let app = harness.launch_uninstrumented_smoke()?;
    let session = harness.testbed.x11_session(
        &app,
        egui_tester::WindowQuery::title_contains(TITLE_FRAGMENT),
        std::time::Duration::from_secs(30),
    )?;
    session.focus()?;
    let first = session.capture()?;
    let visible = |frame: &egui_tester::Frame| {
        frame
            .rgba()
            .chunks_exact(4)
            .any(|pixel| pixel[..3] != [0, 0, 0])
    };
    let frame = if visible(&first) {
        first
    } else {
        session.wait_changed(&first, 0.001, 2, std::time::Duration::from_secs(30))?
    };
    demand(
        visible(&frame),
        "uninstrumented Trailgen rendered only black pixels",
    )?;
    app.terminate()
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
