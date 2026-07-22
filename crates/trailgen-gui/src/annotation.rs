use crate::{basemap, forge, map, vector_map::VectorGap};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Vec2, epaint::TextShape, vec2};
use std::{collections::HashMap, sync::Arc};

const LABEL_CEILING: usize = 180;
const REPEAT_SEPARATION: f32 = 480.0;
const CONTINUITY_TTL: u64 = 120;

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
    pub halo: Option<Halo>,
    pub repeatable: bool,
    pub break_line: bool,
}

#[derive(Clone, Copy)]
pub struct Halo {
    pub color: Color32,
    pub width: f32,
}

pub struct Parking<'a> {
    pub world: [f64; 2],
    pub name: Option<&'a str>,
    pub onset_zoom: f32,
}

#[derive(Default)]
pub struct Compositor {
    epoch: u64,
    memory: HashMap<String, Memory>,
}

#[derive(Clone, Copy)]
struct Memory {
    world: [f64; 2],
    angle: f32,
    touched: u64,
}

impl Compositor {
    pub fn compose<'a>(
        &mut self,
        painter: &Painter,
        viewport: map::Viewport,
        rect: Rect,
        points: impl IntoIterator<Item = PointLabel<'a>>,
        lines: impl IntoIterator<Item = LineLabel<'a>>,
        parking: impl IntoIterator<Item = Parking<'a>>,
    ) -> Composition {
        self.epoch = self.epoch.saturating_add(1);
        let mut occupied = Vec::new();
        let mut prepared = Vec::new();
        let mut parking_marks = Vec::new();
        prepare_parking(
            painter,
            viewport,
            rect,
            parking,
            &mut occupied,
            &mut prepared,
            &mut parking_marks,
        );
        prepare_points(painter, viewport, rect, points, &mut prepared);
        prepare_lines(painter, viewport, rect, lines, &self.memory, &mut prepared);
        prepared.sort_unstable_by(|left, right| {
            left.rank
                .cmp(&right.rank)
                .then_with(|| left.score.total_cmp(&right.score))
                .then_with(|| left.unique.cmp(&right.unique))
        });

        let mut accepted = HashMap::<String, Vec<Pos2>>::new();
        let mut selected = Vec::new();
        for (slot, label) in prepared.iter().enumerate() {
            if accepted.contains_key(&label.unique)
                || occupied
                    .iter()
                    .any(|prior: &Rect| prior.intersects(label.footprint))
            {
                continue;
            }
            occupied.push(label.footprint);
            accepted
                .entry(label.unique.clone())
                .or_default()
                .push(label.anchor);
            selected.push(slot);
            self.remember(label);
            if occupied.len() >= LABEL_CEILING {
                break;
            }
        }

        if viewport.zoom >= 15.0 && occupied.len() < LABEL_CEILING {
            for (slot, label) in prepared.iter().enumerate() {
                if !label.repeatable
                    || accepted.get(&label.unique).is_none_or(|anchors| {
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
                occupied.push(label.footprint);
                accepted
                    .entry(label.unique.clone())
                    .or_default()
                    .push(label.anchor);
                selected.push(slot);
                if occupied.len() >= LABEL_CEILING {
                    break;
                }
            }
        }
        let epoch = self.epoch;
        self.memory
            .retain(|_, memory| epoch.saturating_sub(memory.touched) <= CONTINUITY_TTL);
        Composition {
            labels: selected
                .into_iter()
                .map(|slot| prepared[slot].clone())
                .collect(),
            parking: parking_marks,
        }
    }

    fn remember(&mut self, label: &Prepared) {
        let Some(world) = label.world_anchor else {
            return;
        };
        if self
            .memory
            .get(&label.unique)
            .is_some_and(|memory| memory.touched == self.epoch)
        {
            return;
        }
        self.memory.insert(
            label.unique.clone(),
            Memory {
                world,
                angle: label.angle,
                touched: self.epoch,
            },
        );
    }
}

pub struct Composition {
    labels: Vec<Prepared>,
    parking: Vec<ParkingMark>,
}

impl Composition {
    pub fn paint(&self, painter: &Painter) {
        for mark in &self.parking {
            forge::parking(painter, mark.anchor, mark.maturity);
        }
        for label in &self.labels {
            stamp(painter, label);
        }
    }

    pub fn contour_gaps(&self, rect: Rect) -> Arc<[VectorGap]> {
        self.labels
            .iter()
            .filter(|label| label.break_line)
            .map(|label| {
                VectorGap::screen(
                    label.anchor - rect.min.to_vec2(),
                    Vec2::angled(label.angle),
                    label.galley.size() * 0.5 + vec2(3.0, 1.5),
                )
            })
            .collect::<Vec<_>>()
            .into()
    }
}

#[derive(Clone, Copy)]
struct ParkingMark {
    anchor: Pos2,
    maturity: f32,
}

#[derive(Clone)]
struct Prepared {
    unique: String,
    rank: u16,
    score: f32,
    anchor: Pos2,
    angle: f32,
    footprint: Rect,
    galley: Arc<egui::Galley>,
    ink: Color32,
    halo: Option<Halo>,
    repeatable: bool,
    world_anchor: Option<[f64; 2]>,
    break_line: bool,
}

fn prepare_parking<'a>(
    painter: &Painter,
    viewport: map::Viewport,
    rect: Rect,
    parking: impl IntoIterator<Item = Parking<'a>>,
    occupied: &mut Vec<Rect>,
    prepared: &mut Vec<Prepared>,
    marks: &mut Vec<ParkingMark>,
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
        marks.push(ParkingMark { anchor, maturity });
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
                unique: name.to_owned(),
                rank: 720,
                score: center.distance(rect.center()),
                anchor: center,
                angle: 0.0,
                footprint,
                galley,
                ink: Color32::from_rgb(47, 39, 28).gamma_multiply(label_maturity),
                halo: Some(Halo {
                    color: Color32::from_white_alpha(185).gamma_multiply(label_maturity),
                    width: 1.0,
                }),
                repeatable: false,
                world_anchor: None,
                break_line: false,
            });
        }
    }
}

fn prepare_points<'a>(
    painter: &Painter,
    viewport: map::Viewport,
    rect: Rect,
    points: impl IntoIterator<Item = PointLabel<'a>>,
    prepared: &mut Vec<Prepared>,
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
                unique: label.text.to_owned(),
                rank: label.rank,
                score: anchor.distance(rect.center()),
                anchor,
                angle: 0.0,
                footprint,
                galley,
                ink: Color32::from_black_alpha((225.0 * maturity) as u8),
                halo: Some(Halo {
                    color: Color32::from_white_alpha((92.0 * maturity) as u8),
                    width: 1.0,
                }),
                repeatable: false,
                world_anchor: None,
                break_line: false,
            });
        }
    }
}

fn prepare_lines<'a>(
    painter: &Painter,
    viewport: map::Viewport,
    rect: Rect,
    lines: impl IntoIterator<Item = LineLabel<'a>>,
    memory: &HashMap<String, Memory>,
    prepared: &mut Vec<Prepared>,
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
        let preference = memory.get(label.text).map(|memory| Preference {
            anchor: map::screen_at(viewport, rect, memory.world),
            angle: memory.angle,
        });
        let Some(placement) = line_placement(&path, galley.size(), rect, preference) else {
            continue;
        };
        let shape = text_shape(
            placement.anchor,
            placement.angle,
            Arc::clone(&galley),
            label.ink,
        );
        prepared.push(Prepared {
            unique: label.text.to_owned(),
            rank: label.rank,
            score: placement.score,
            anchor: placement.anchor,
            angle: placement.angle,
            footprint: shape.visual_bounding_rect().expand(3.0),
            galley,
            ink: label.ink.gamma_multiply(maturity),
            halo: label.halo.map(|halo| Halo {
                color: halo.color.gamma_multiply(maturity),
                ..halo
            }),
            repeatable: label.repeatable,
            world_anchor: Some(map::world_at(viewport, rect, placement.anchor)),
            break_line: label.break_line,
        });
    }
}

fn stamp(painter: &Painter, label: &Prepared) {
    if let Some(halo) = label.halo {
        let diagonal = halo.width * std::f32::consts::FRAC_1_SQRT_2;
        for offset in [
            vec2(-halo.width, 0.0),
            vec2(halo.width, 0.0),
            vec2(0.0, -halo.width),
            vec2(0.0, halo.width),
            vec2(-diagonal, -diagonal),
            vec2(diagonal, -diagonal),
            vec2(-diagonal, diagonal),
            vec2(diagonal, diagonal),
        ] {
            let _halo = painter.add(text_shape(
                label.anchor + offset,
                label.angle,
                Arc::clone(&label.galley),
                halo.color,
            ));
        }
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

#[derive(Clone, Copy)]
struct Preference {
    anchor: Pos2,
    angle: f32,
}

fn line_placement(
    path: &[Pos2],
    label: Vec2,
    viewport: Rect,
    preference: Option<Preference>,
) -> Option<Placement> {
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
    dyadic_centers(total, label.x)
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
                score: preference.map_or_else(
                    || bend.mul_add(1_000.0, anchor.distance(viewport.center())),
                    |preference| {
                        let angular_travel =
                            unsigned_angle(Vec2::angled(angle), Vec2::angled(preference.angle));
                        bend.mul_add(
                            180.0,
                            angular_travel.mul_add(90.0, anchor.distance(preference.anchor)),
                        )
                    },
                ),
            })
        })
        .min_by(|left, right| left.score.total_cmp(&right.score))
}

fn dyadic_centers(total: f32, label_width: f32) -> Vec<f32> {
    let target_spacing = (label_width + 96.0).max(220.0);
    let refinements = if total > target_spacing {
        (total / target_spacing).log2().floor() as u32
    } else {
        0
    }
    .min(7);
    let denominator = 2_u32 << refinements;
    (1..denominator)
        .map(|numerator| total * numerator as f32 / denominator as f32)
        .collect()
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
            None,
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
                None,
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
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn line_label_continuity_dominates_viewport_recentering() {
        let path = [egui::pos2(100.0, 300.0), egui::pos2(700.0, 300.0)];
        let label = vec2(80.0, 12.0);
        let centered = line_placement(&path, label, VIEW, None).expect("long road has a label");
        let remembered = line_placement(
            &path,
            label,
            VIEW,
            Some(Preference {
                anchor: egui::pos2(250.0, 300.0),
                angle: 0.0,
            }),
        )
        .expect("remembered road has a label");
        assert!(centered.anchor.distance(VIEW.center()) < 1.0);
        assert!(remembered.anchor.distance(egui::pos2(250.0, 300.0)) < 1.0);
    }

    #[test]
    fn zoom_refines_line_anchors_without_moving_existing_world_references() {
        let coarse = dyadic_centers(500.0, 80.0)
            .into_iter()
            .map(|center| center / 500.0)
            .collect::<Vec<_>>();
        let fine = dyadic_centers(1_000.0, 80.0)
            .into_iter()
            .map(|center| center / 1_000.0)
            .collect::<Vec<_>>();
        assert!(coarse.iter().all(|anchor| fine.contains(anchor)));
        assert!(fine.len() > coarse.len());
    }
}
