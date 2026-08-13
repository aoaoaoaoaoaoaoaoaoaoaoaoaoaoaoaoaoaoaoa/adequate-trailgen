//! Trail-specific cartographic marks that have no shared physical law.

use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};

const BRONZE_SHADOW: [f32; 3] = [34.0, 28.0, 19.0];
const BRONZE_BODY: [f32; 3] = [104.0, 86.0, 58.0];
const BRONZE_GLINT: [f32; 3] = [196.0, 170.0, 124.0];

/// A support mandate at an exact map coordinate. This carries no trailhead or
/// draggable-control semantics.
pub fn reticle(painter: &Painter, anchor: Pos2) {
    let _shadow = painter.circle_stroke(
        anchor,
        9.0,
        Stroke::new(5.0_f32, Color32::from_black_alpha(190)),
    );
    let _ring = painter.circle_stroke(anchor, 9.0, Stroke::new(2.8_f32, bronze(0.92)));
}

/// The universally legible parking sigil, reserved here for trail-access lots.
pub fn parking(painter: &Painter, anchor: Pos2, maturity: f32) {
    let body = Rect::from_center_size(anchor, Vec2::splat(16.0));
    let _shadow = painter.rect_filled(
        body.translate(Vec2::splat(1.0)),
        2.5,
        Color32::from_black_alpha((80.0 * maturity) as u8),
    );
    let _body = painter.rect_filled(body, 2.5, bronze(0.50).gamma_multiply(maturity));
    let _rim = painter.rect_stroke(
        body,
        2.5,
        Stroke::new(1.0_f32, bronze(0.95).gamma_multiply(maturity)),
        egui::StrokeKind::Inside,
    );
    let _p = painter.text(
        anchor,
        egui::Align2::CENTER_CENTER,
        "P",
        egui::FontId::monospace(11.0),
        Color32::from_rgb(244, 235, 211).gamma_multiply(maturity),
    );
}

fn bronze(tone: f32) -> Color32 {
    let tone = tone.clamp(0.0, 1.0);
    let (lo, hi, t) = if tone < 0.6 {
        (BRONZE_SHADOW, BRONZE_BODY, tone / 0.6)
    } else {
        (BRONZE_BODY, BRONZE_GLINT, (tone - 0.6) / 0.4)
    };
    let channel = |i: usize| (hi[i] - lo[i]).mul_add(t, lo[i]).round() as u8;
    Color32::from_rgb(channel(0), channel(1), channel(2))
}
