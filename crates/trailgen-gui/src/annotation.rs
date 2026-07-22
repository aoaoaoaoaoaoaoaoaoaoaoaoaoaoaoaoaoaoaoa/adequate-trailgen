use crate::{basemap, forge, map};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Vec2, epaint::TextShape, vec2};
use std::{collections::HashMap, sync::Arc};

const LABEL_CEILING: usize = 180;
const REPEAT_SEPARATION: f32 = 480.0;

pub struct PointLabel<'a> {
    pub world: [f64; 2],
    pub text: &'a str,
    pub rank: u16,
    pub size: f32,
    pub onset_zoom: f32,
}

pub struct LineLabel<'a> {
    pub path: &'a [[f64; 2]],
    pub text: &'a str,
    pub rank: u16,
    pub size: f32,
    pub onset_zoom: f32,
    pub ink: Color32,
    pub halo: Color32,
    pub repeatable: bool,
}

pub struct Parking<'a> {
    pub world: [f64; 2],
    pub name: Option<&'a str>,
    pub onset_zoom: f32,
}

pub fn paint<'a>(
    painter: &Painter,
    viewport: map::Viewport,
    rect: Rect,
    points: impl IntoIterator<Item = PointLabel<'a>>,
    lines: impl IntoIterator<Item = LineLabel<'a>>,
    parking: impl IntoIterator<Item = Parking<'a>>,
) {
    let mut occupied = Vec::new();
    let mut prepared = Vec::new();
    prepare_parking(
        painter,
        viewport,
        rect,
        parking,
        &mut occupied,
        &mut prepared,
    );
    prepare_points(painter, viewport, rect, points, &mut prepared);
    prepare_lines(painter, viewport, rect, lines, &mut prepared);
    prepared.sort_unstable_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| left.score.total_cmp(&right.score))
    });

    let mut accepted = HashMap::<&str, Vec<Pos2>>::new();
    for label in &prepared {
        if accepted.contains_key(label.unique)
            || occupied
                .iter()
                .any(|prior: &Rect| prior.intersects(label.footprint))
        {
            continue;
        }
        stamp(painter, label);
        occupied.push(label.footprint);
        accepted.entry(label.unique).or_default().push(label.anchor);
        if occupied.len() >= LABEL_CEILING {
            return;
        }
    }

    if viewport.zoom < 15.0 {
        return;
    }
    for label in &prepared {
        if !label.repeatable
            || accepted.get(label.unique).is_none_or(|anchors| {
                anchors
                    .iter()
                    .any(|anchor| anchor.distance(label.anchor) < REPEAT_SEPARATION)
            })
            || occupied
                .iter()
                .any(|prior| prior.intersects(label.footprint))
        {
            continue;
        }
        stamp(painter, label);
        occupied.push(label.footprint);
        accepted.entry(label.unique).or_default().push(label.anchor);
        if occupied.len() >= LABEL_CEILING {
            return;
        }
    }
}

struct Prepared<'a> {
    unique: &'a str,
    rank: u16,
    score: f32,
    anchor: Pos2,
    angle: f32,
    footprint: Rect,
    galley: Arc<egui::Galley>,
    ink: Color32,
    halo: Color32,
    repeatable: bool,
}

fn prepare_parking<'a>(
    painter: &Painter,
    viewport: map::Viewport,
    rect: Rect,
    parking: impl IntoIterator<Item = Parking<'a>>,
    occupied: &mut Vec<Rect>,
    prepared: &mut Vec<Prepared<'a>>,
) {
    let mut anchors = Vec::<Pos2>::new();
    for mark in parking {
        let maturity = basemap::apparition(viewport.zoom as f32, mark.onset_zoom);
        if maturity <= 0.01 {
            continue;
        }
        let anchor = map::screen_at(viewport, rect, mark.world);
        let footprint = Rect::from_center_size(anchor, Vec2::splat(19.0)).expand(2.0);
        if !rect.contains_rect(footprint)
            || anchors.iter().any(|prior| prior.distance(anchor) < 12.0)
        {
            continue;
        }
        anchors.push(anchor);
        occupied.push(footprint);
        forge::parking(painter, anchor, maturity);
        let Some(name) = mark.name else { continue };
        let label_maturity = basemap::apparition(viewport.zoom as f32, mark.onset_zoom + 0.45);
        if label_maturity <= 0.01 {
            continue;
        }
        let galley = painter.layout_no_wrap(
            name.to_owned(),
            FontId::proportional(9.8),
            Color32::PLACEHOLDER,
        );
        let center = anchor + vec2(galley.size().x.mul_add(0.5, 13.0), 0.0);
        let footprint = Rect::from_center_size(center, galley.size()).expand(3.0);
        if rect.contains_rect(footprint) {
            prepared.push(Prepared {
                unique: name,
                rank: 720,
                score: center.distance(rect.center()),
                anchor: center,
                angle: 0.0,
                footprint,
                galley,
                ink: Color32::from_rgb(47, 39, 28).gamma_multiply(label_maturity),
                halo: Color32::from_white_alpha(185).gamma_multiply(label_maturity),
                repeatable: false,
            });
        }
    }
}

fn prepare_points<'a>(
    painter: &Painter,
    viewport: map::Viewport,
    rect: Rect,
    points: impl IntoIterator<Item = PointLabel<'a>>,
    prepared: &mut Vec<Prepared<'a>>,
) {
    for label in points {
        let maturity = basemap::apparition(viewport.zoom as f32, label.onset_zoom);
        if maturity <= 0.01 {
            continue;
        }
        let anchor = map::screen_at(viewport, rect, label.world);
        let size = label.size * 0.12_f32.mul_add(maturity, 0.88);
        let galley = painter.layout_no_wrap(
            label.text.to_owned(),
            FontId::proportional(size),
            Color32::PLACEHOLDER,
        );
        let footprint = Rect::from_center_size(anchor, galley.size()).expand(3.0);
        if rect.contains_rect(footprint) {
            prepared.push(Prepared {
                unique: label.text,
                rank: label.rank,
                score: anchor.distance(rect.center()),
                anchor,
                angle: 0.0,
                footprint,
                galley,
                ink: Color32::from_black_alpha((225.0 * maturity) as u8),
                halo: Color32::from_white_alpha((92.0 * maturity) as u8),
                repeatable: false,
            });
        }
    }
}

fn prepare_lines<'a>(
    painter: &Painter,
    viewport: map::Viewport,
    rect: Rect,
    lines: impl IntoIterator<Item = LineLabel<'a>>,
    prepared: &mut Vec<Prepared<'a>>,
) {
    for label in lines {
        let maturity = basemap::apparition(viewport.zoom as f32, label.onset_zoom);
        if maturity <= 0.01 {
            continue;
        }
        let size = label.size * 0.08_f32.mul_add(maturity, 0.92);
        let galley = painter.layout_no_wrap(
            label.text.to_owned(),
            FontId::proportional(size),
            Color32::PLACEHOLDER,
        );
        let path = label
            .path
            .iter()
            .copied()
            .map(|world| map::screen_at(viewport, rect, world))
            .collect::<Vec<_>>();
        let Some(placement) = line_placement(&path, galley.size(), rect) else {
            continue;
        };
        let shape = text_shape(
            placement.anchor,
            placement.angle,
            Arc::clone(&galley),
            label.ink,
        );
        prepared.push(Prepared {
            unique: label.text,
            rank: label.rank,
            score: placement.score,
            anchor: placement.anchor,
            angle: placement.angle,
            footprint: shape.visual_bounding_rect().expand(3.0),
            galley,
            ink: label.ink.gamma_multiply(maturity),
            halo: label.halo.gamma_multiply(maturity),
            repeatable: label.repeatable,
        });
    }
}

fn stamp(painter: &Painter, label: &Prepared<'_>) {
    for offset in [
        vec2(-1.25, 0.0),
        vec2(1.25, 0.0),
        vec2(0.0, -1.25),
        vec2(0.0, 1.25),
        vec2(-0.9, -0.9),
        vec2(0.9, -0.9),
        vec2(-0.9, 0.9),
        vec2(0.9, 0.9),
    ] {
        let _halo = painter.add(text_shape(
            label.anchor + offset,
            label.angle,
            Arc::clone(&label.galley),
            label.halo,
        ));
    }
    let _ink = painter.add(text_shape(
        label.anchor,
        label.angle,
        Arc::clone(&label.galley),
        label.ink,
    ));
}

fn text_shape(anchor: Pos2, angle: f32, galley: Arc<egui::Galley>, color: Color32) -> TextShape {
    TextShape::new(anchor - galley.rect.center().to_vec2(), galley, color)
        .with_angle_and_anchor(angle, Align2::CENTER_CENTER)
}

#[derive(Clone, Copy, Debug)]
struct Placement {
    anchor: Pos2,
    angle: f32,
    score: f32,
}

fn line_placement(path: &[Pos2], label: Vec2, viewport: Rect) -> Option<Placement> {
    let path = path
        .iter()
        .copied()
        .fold(Vec::with_capacity(path.len()), |mut clean, point| {
            if clean
                .last()
                .is_none_or(|prior: &Pos2| prior.distance(point) > 0.1)
            {
                clean.push(point);
            }
            clean
        });
    let lengths = cumulative_lengths(&path);
    let total = *lengths.last()?;
    let span = label.x + 18.0;
    if total < span {
        return None;
    }
    let half = span * 0.5;
    let stride = (label.x * 0.85).max(54.0);
    let mut centers = vec![total * 0.5];
    let slots = ((total - span) / stride).floor() as usize;
    for slot in 0..=slots {
        centers.push((slot as f32).mul_add(stride, half));
    }
    centers
        .into_iter()
        .filter_map(|center| {
            let start = point_at(&path, &lengths, center - half);
            let anchor = point_at(&path, &lengths, center);
            let end = point_at(&path, &lengths, center + half);
            let left = anchor - start;
            let right = end - anchor;
            let bend = unsigned_angle(left, right);
            let chord = end.distance(start);
            if chord < span * 0.90 || bend > 0.38 {
                return None;
            }
            let mut angle = (end.y - start.y).atan2(end.x - start.x);
            if angle > std::f32::consts::FRAC_PI_2 {
                angle -= std::f32::consts::PI;
            } else if angle < -std::f32::consts::FRAC_PI_2 {
                angle += std::f32::consts::PI;
            }
            let footprint = rotated_footprint(anchor, label, angle).expand(3.0);
            viewport.contains_rect(footprint).then_some(Placement {
                anchor,
                angle,
                score: bend.mul_add(1_000.0, anchor.distance(viewport.center())),
            })
        })
        .min_by(|left, right| left.score.total_cmp(&right.score))
}

fn cumulative_lengths(path: &[Pos2]) -> Vec<f32> {
    let mut total = 0.0;
    std::iter::once(0.0)
        .chain(path.windows(2).map(|segment| {
            total += segment[0].distance(segment[1]);
            total
        }))
        .collect()
}

fn point_at(path: &[Pos2], lengths: &[f32], distance: f32) -> Pos2 {
    let slot = lengths
        .partition_point(|progress| *progress < distance)
        .clamp(1, lengths.len().saturating_sub(1));
    let start = lengths[slot - 1];
    let span = lengths[slot] - start;
    path[slot - 1].lerp(path[slot], (distance - start) / span)
}

fn unsigned_angle(left: Vec2, right: Vec2) -> f32 {
    left.x
        .mul_add(right.y, -left.y * right.x)
        .atan2(left.dot(right))
        .abs()
}

fn rotated_footprint(anchor: Pos2, size: Vec2, angle: f32) -> Rect {
    let rotation = egui::emath::Rot2::from_angle(angle);
    let half = size * 0.5;
    [
        vec2(-half.x, -half.y),
        vec2(half.x, -half.y),
        vec2(half.x, half.y),
        vec2(-half.x, half.y),
    ]
    .into_iter()
    .map(|corner| anchor + rotation * corner)
    .fold(Rect::NOTHING, |mut rect, point| {
        rect.extend_with(point);
        rect
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: Rect = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 600.0));

    #[test]
    fn line_label_chooses_a_visible_subsegment_and_stays_upright() {
        let placement = line_placement(
            &[egui::pos2(700.0, 300.0), egui::pos2(100.0, 300.0)],
            vec2(100.0, 12.0),
            VIEW,
        )
        .expect("long road has a placement");
        assert!(placement.angle.abs() < f32::EPSILON);
        assert!(VIEW.contains(placement.anchor));
    }

    #[test]
    fn line_label_rejects_short_and_sharply_bent_paths() {
        assert!(
            line_placement(
                &[egui::pos2(100.0, 100.0), egui::pos2(140.0, 100.0)],
                vec2(80.0, 12.0),
                VIEW,
            )
            .is_none()
        );
        assert!(
            line_placement(
                &[
                    egui::pos2(200.0, 300.0),
                    egui::pos2(300.0, 300.0),
                    egui::pos2(300.0, 400.0),
                ],
                vec2(150.0, 12.0),
                VIEW,
            )
            .is_none()
        );
    }
}
