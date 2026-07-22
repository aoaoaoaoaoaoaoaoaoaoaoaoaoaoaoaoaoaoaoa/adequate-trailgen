//! Local dies awaiting promotion into `dwemer_poolrooms::forge`.

use egui::{Color32, Mesh, Painter, Pos2, Rect, Stroke, Vec2};
use std::f32::consts::TAU;

pub const PIN_RISE: f32 = 25.0;
pub const PIN_GRIP: f32 = 28.0;

const BULB_RADIUS: f32 = 8.0;
const BRONZE_SHADOW: [f32; 3] = [34.0, 28.0, 19.0];
const BRONZE_BODY: [f32; 3] = [104.0, 86.0, 58.0];
const BRONZE_GLINT: [f32; 3] = [196.0, 170.0, 124.0];
const LIGHT_Y: f32 = -0.5;
const LIGHT_Z: f32 = 0.866_025_4;
const HALF_Y: f32 = -0.258_819_04;
const HALF_Z: f32 = 0.965_925_8;

pub fn pin_bulb(anchor: Pos2) -> Pos2 {
    anchor - Vec2::new(0.0, PIN_RISE)
}

pub fn pin_grip(anchor: Pos2) -> Rect {
    Rect::from_min_max(
        pin_bulb(anchor) - Vec2::splat(PIN_GRIP / 2.0),
        anchor + Vec2::new(PIN_GRIP / 2.0, 4.0),
    )
}

pub fn pin(painter: &Painter, anchor: Pos2, seized: bool) {
    let bulb = pin_bulb(anchor);
    let heat = if seized { 0.07 } else { 0.0 };
    let left = bulb + Vec2::new(-4.2, 3.7);
    let right = bulb + Vec2::new(4.2, 3.7);
    let shadow = vec![
        left + Vec2::splat(1.1),
        right + Vec2::splat(1.1),
        anchor + Vec2::new(0.0, 1.0),
    ];
    let _shadow = painter.add(egui::Shape::convex_polygon(
        shadow,
        Color32::from_black_alpha(96),
        Stroke::NONE,
    ));
    stamp(
        painter,
        vec![left, right, anchor],
        &[[right, anchor]],
        &[[anchor, left]],
        heat,
    );
    sphere(painter, bulb, heat);
    let _rim = painter.circle_stroke(bulb, BULB_RADIUS, Stroke::new(1.1_f32, bronze(0.16)));
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

fn stamp(
    painter: &Painter,
    silhouette: Vec<Pos2>,
    crowns: &[[Pos2; 2]],
    soles: &[[Pos2; 2]],
    heat: f32,
) {
    let _body = painter.add(egui::Shape::convex_polygon(
        silhouette,
        bronze(0.56 + heat),
        Stroke::NONE,
    ));
    for edge in crowns {
        let _crown = painter.line_segment(*edge, Stroke::new(0.8_f32, bronze(0.80 + heat)));
    }
    for edge in soles {
        let _sole = painter.line_segment(*edge, Stroke::new(0.8_f32, bronze(0.18 + heat)));
    }
}

fn sphere(painter: &Painter, center: Pos2, heat: f32) {
    const RINGS: u32 = 4;
    const SECTORS: u32 = 24;

    let mut mesh = Mesh::default();
    mesh.reserve_vertices((1 + RINGS * SECTORS) as usize);
    mesh.reserve_triangles((SECTORS * (2 * RINGS - 1)) as usize);
    mesh.colored_vertex(center, sphere_bronze(0.0, 0.0, heat));
    for ring in 1..=RINGS {
        let radius = ring as f32 / RINGS as f32;
        for sector in 0..SECTORS {
            let angle = TAU * sector as f32 / SECTORS as f32;
            let x = radius * angle.cos();
            let y = radius * angle.sin();
            mesh.colored_vertex(
                center + Vec2::new(x, y) * BULB_RADIUS,
                sphere_bronze(y, radius.mul_add(-radius, 1.0).sqrt(), heat),
            );
        }
    }
    for sector in 0..SECTORS {
        mesh.add_triangle(0, 1 + sector, 1 + (sector + 1) % SECTORS);
    }
    for ring in 2..=RINGS {
        let inner = 1 + (ring - 2) * SECTORS;
        let outer = inner + SECTORS;
        for sector in 0..SECTORS {
            let next = (sector + 1) % SECTORS;
            mesh.add_triangle(inner + sector, outer + sector, inner + next);
            mesh.add_triangle(inner + next, outer + sector, outer + next);
        }
    }
    let _bulb = painter.add(egui::Shape::mesh(mesh));
}

fn sphere_bronze(ny: f32, nz: f32, heat: f32) -> Color32 {
    let diffuse = nz.mul_add(LIGHT_Z, ny * LIGHT_Y).max(0.0);
    let specular = nz.mul_add(HALF_Z, ny * HALF_Y).max(0.0).powi(14);
    bronze(0.38_f32.mul_add(specular, 0.42_f32.mul_add(diffuse, 0.12 + heat)))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_die_keeps_the_bulb_above_its_map_coordinate() {
        let anchor = Pos2::new(30.0, 40.0);
        assert_eq!(pin_bulb(anchor), Pos2::new(30.0, 40.0 - PIN_RISE));
        assert!(pin_grip(anchor).contains(pin_bulb(anchor)));
        assert!(pin_grip(anchor).contains(anchor));
    }

    #[test]
    fn bronze_charge_preserves_its_three_fixed_swatch_anchors() {
        assert_eq!(bronze(0.0), Color32::from_rgb(34, 28, 19));
        assert_eq!(bronze(0.6), Color32::from_rgb(104, 86, 58));
        assert_eq!(bronze(1.0), Color32::from_rgb(196, 170, 124));
    }
}
