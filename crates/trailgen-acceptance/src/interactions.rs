use std::time::Duration;

use egui_tester::{Button, PixelRegion, Result, Stroke, Timed};

use crate::{
    harness::{Target, TrailFrame, TrailStory, demand, map_pixel, screen_point, verdict},
    observation::shows,
};

pub fn add_support(
    story: &mut TrailStory<'_, '_>,
    coordinate: [f64; 2],
    expected: usize,
) -> Result<Timed<TrailFrame>> {
    let target = map_pixel(&story.frame()?, coordinate)?;
    let mut effect = shows::supports(expected) & shows::support(expected - 1, coordinate);
    if expected >= 2 {
        effect = effect & shows::editor_ready();
    }
    story.click_at(target, Button::Primary)?.until(effect)
}

pub fn drag_support(
    story: &mut TrailStory<'_, '_>,
    frame: &TrailFrame,
    slot: usize,
    target: [f64; 2],
    before_signature: u64,
) -> Result<Timed<TrailFrame>> {
    let editor = frame
        .state
        .editor
        .as_ref()
        .ok_or_else(|| verdict("editor witness omitted editor state"))?;
    let current = map_pixel(
        frame,
        *editor
            .support_points
            .get(slot)
            .ok_or_else(|| verdict(format!("editor omitted support {slot} coordinate")))?,
    )?;
    let target_pixel = map_pixel(frame, target)?;
    let grip = frame
        .anchor(&Target::Support(slot).to_string())
        .ok_or_else(|| verdict(format!("editor omitted draggable support {slot}")))?
        .center();
    let destination = (
        target_pixel
            .0
            .saturating_add(grip.0.saturating_sub(current.0)),
        target_pixel
            .1
            .saturating_add(grip.1.saturating_sub(current.1)),
    );

    let press = story
        .session()
        .button_down(grip.0, grip.1, Button::Primary)?;
    let _acquired = story
        .reaction(press)
        .until(shows::dragging_support(Some(slot)))?;
    let motion = story.session().move_to(destination.0, destination.1)?;
    let previewed = story
        .reaction(motion)
        .until(shows::signature(before_signature) & shows::support(slot, target))?;
    let release = story.session().button_up(Button::Primary)?;
    let reforged = story.reaction(release).until(
        shows::dragging_support(None)
            & shows::editor_ready()
            & shows::changed_signature(before_signature)
            & shows::support(slot, target),
    )?;
    demand(
        previewed
            .value()
            .state
            .editor
            .as_ref()
            .and_then(|editor| editor.route_signature)
            == Some(before_signature),
        "pin drag recomputed route geometry before release",
    )?;
    Ok(reforged)
}

pub fn lasso_boundary(story: &mut TrailStory<'_, '_>, inset: f32) -> Result<Timed<TrailFrame>> {
    let [x0, y0, x1, y1] = story.anchor(Target::Map)?.rect;
    let inset_x = (x1 - x0) * inset;
    let inset_y = (y1 - y0) * inset;
    let point = |x, y| screen_point([f64::from(x), f64::from(y)]);
    let knots = [
        point(x0 + inset_x, y0 + inset_y)?,
        point(x1 - inset_x, y0 + inset_y)?,
        point(x1 - inset_x, y1 - inset_y)?,
        point(x0 + inset_x, y1 - inset_y)?,
        point(x0 + inset_x, y0 + inset_y)?,
    ];
    story
        .stroke(
            &knots,
            Stroke {
                steps_per_leg: 6,
                leg_duration: Duration::from_millis(120),
                knot_dwell: Duration::from_millis(120),
                ..Stroke::default()
            },
        )?
        .until(shows::boundary())
}

pub fn exercise_profile(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let profile = story.anchor(Target::Profile)?;
    let [x0, y0, x1, y1] = profile.rect;
    let region = PixelRegion::anchor(&profile);
    let baseline = story.capture()?;
    let target = screen_point([
        f64::from((x1 - x0).mul_add(0.62, x0)),
        f64::from(f32::midpoint(y0, y1)),
    ])?;
    let hover = story.session().move_to(target.0, target.1)?;
    let _hovered = story.reaction(hover).until(shows::profile_hovering())?;
    let hovered = story.session().wait_changed_region(
        &baseline,
        region,
        0.000_5,
        2,
        Duration::from_secs(4),
    )?;
    demand(
        baseline.difference_region(&hovered, region, 2)? >= 0.000_5,
        "profile witness moved without a rendered elevation cursor",
    )?;
    let _locked = story
        .click_at(target, Button::Primary)?
        .until(shows::profile_locked(true))?;
    let _released = story
        .click_at(target, Button::Secondary)?
        .until(shows::profile_locked(false))?;
    Ok(())
}
