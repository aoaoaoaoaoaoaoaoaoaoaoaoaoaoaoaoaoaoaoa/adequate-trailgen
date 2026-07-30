use std::time::Duration;

use egui_tester::{Button, PerformanceBudget, Result, Stroke, Timed};

use crate::{
    harness::{Control, TrailFrame, TrailStory, map_pixel, screen_point, verdict},
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
    story.click_at(target, Button::Primary)?.expect(effect)
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
        .anchor(&format!("editor.support/{slot}"))
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
        .expect(shows::dragging_support(Some(slot)))?;
    let motion = story.session().move_to(destination.0, destination.1)?;
    let reforged = story
        .reaction(motion)
        .within(
            PerformanceBudget::new(Duration::from_millis(240))
                .through_presentation()
                .timeout(Duration::from_secs(6)),
        )
        .expect(
            shows::editor_ready()
                & shows::changed_signature(before_signature)
                & shows::support(slot, target),
        )?;
    let release = story.session().button_up(Button::Primary)?;
    let _released = story
        .reaction(release)
        .expect(shows::dragging_support(None))?;
    Ok(reforged)
}

pub fn lasso_boundary(story: &mut TrailStory<'_, '_>, inset: f32) -> Result<Timed<TrailFrame>> {
    let [x0, y0, x1, y1] = story.anchor(Control::Map)?.rect;
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
                steps_per_leg: 5,
                leg_duration: Duration::from_millis(45),
                ..Stroke::default()
            },
        )?
        .expect(shows::boundary())
}

pub fn exercise_profile(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let [x0, y0, x1, y1] = story.anchor(Control::Profile)?.rect;
    let target = screen_point([
        f64::from((x1 - x0).mul_add(0.62, x0)),
        f64::from(f32::midpoint(y0, y1)),
    ])?;
    let hover = story.session().move_to(target.0, target.1)?;
    let _hovered = story.reaction(hover).expect(shows::profile_hovering())?;
    let _locked = story
        .click_at(target, Button::Primary)?
        .expect(shows::profile_locked(true))?;
    let _released = story
        .click_at(target, Button::Secondary)?
        .expect(shows::profile_locked(false))?;
    Ok(())
}
