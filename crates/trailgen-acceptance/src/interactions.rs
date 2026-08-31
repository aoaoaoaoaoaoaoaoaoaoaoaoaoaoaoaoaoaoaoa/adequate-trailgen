use std::time::Duration;

use egui_tester::{Button, Key, Modifiers, Motion, PixelRegion, Result, Timed, Wheel};

use crate::{
    harness::{Target, TrailFrame, TrailStory, demand, map_pixel, screen_point, verdict},
    observation::shows,
};

pub fn reveal_inspector_target(
    story: &mut TrailStory<'_, '_>,
    target: impl std::fmt::Display,
) -> Result<()> {
    let target = target.to_string();
    let reflow = story.session().move_to(4, 4)?;
    let _reflowed = story.reaction(reflow).next_frame()?;
    let frame = story.wait(shows::map())?;
    let map = frame
        .state
        .map
        .as_ref()
        .ok_or_else(|| verdict("inspector reveal omitted its map transform"))?;
    let ppp = f64::from(frame.ppp);
    let point = screen_point([
        f64::from(map.rect[0]) * ppp * 0.5,
        f64::from(f32::midpoint(map.rect[1], map.rect[3])) * ppp,
    ])?;
    let screen_bottom = f64::from(story.capture()?.height().saturating_sub(20));
    // Coarse ten-notch strokes can leap across a target when enlarged typography makes the
    // inspector tall. Traverse in smaller strokes and let the witness decide when to stop.
    for _ in 0..12 {
        let anchor = story.anchor(target.as_str())?;
        let center = anchor.center();
        if f64::from(center.1) >= 50.0 && f64::from(center.1) <= screen_bottom {
            let _settled = story.wait_stable(
                Duration::from_secs(2),
                Duration::from_millis(300),
                format!("inspector target `{target}` to stop moving"),
                |frame| {
                    frame
                        .anchor(target.as_str())
                        .map(|anchor| anchor.rect.map(f32::to_bits))
                },
            )?;
            return Ok(());
        }
        let ticks = if f64::from(center.1) > screen_bottom {
            4
        } else {
            -4
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
    Err(verdict(format!("inspector could not reveal {target}")))
}

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
    let previewed = story
        .motion_to(destination, Motion::default())?
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

pub fn delete_support(
    story: &mut TrailStory<'_, '_>,
    slot: usize,
    expected: usize,
) -> Result<Timed<TrailFrame>> {
    story
        .modified_click(Target::Support(slot), Button::Primary, Modifiers::SHIFT)?
        .until(shows::supports(expected) & shows::editor_ready())
}

pub fn exercise_support_delete_affordance(
    story: &mut TrailStory<'_, '_>,
    slot: usize,
    expected: usize,
) -> Result<()> {
    let region = PixelRegion::anchor(&story.anchor(Target::Support(slot))?);
    let numbered = story.capture()?;
    let press = story.session().key_down(Key::Shift)?;
    let _armed = story.reaction(press).until(shows::supports(expected))?;
    let marked =
        story
            .session()
            .wait_changed_region(&numbered, region, 0.001, 2, Duration::from_secs(4))?;
    demand(
        numbered.difference_region(&marked, region, 2)? >= 0.001,
        "Shift armed support deletion without replacing its numbered pin head",
    )?;
    let release = story.session().key_up(Key::Shift)?;
    let _disarmed = story.reaction(release).until(shows::supports(expected))?;
    Ok(())
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
    let press = story
        .session()
        .button_down(knots[0].0, knots[0].1, Button::Primary)?;
    let _acquired = story.reaction(press).next_frame()?;
    for knot in &knots[1..] {
        let _sampled = story
            .motion_to(
                *knot,
                Motion {
                    steps: 8,
                    duration: Duration::from_millis(160),
                },
            )?
            .next_frame()?;
    }
    let release = story.session().button_up(Button::Primary)?;
    story.reaction(release).until(shows::boundary())
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
    let _hovered = story
        .motion_to(target, Motion::default())?
        .until(shows::profile_hovering())?;
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
