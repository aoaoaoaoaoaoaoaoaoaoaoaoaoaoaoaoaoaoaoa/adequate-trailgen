use crate::{basemap, forge, map, vector_map::VectorGap};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Vec2, epaint::TextShape, vec2};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

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
    incumbents: Vec<Incumbent>,
}

#[derive(Clone)]
struct Incumbent {
    // Closed lower zoom bound: once admitted, this coordinate is never
    // re-anchored or evicted by anything born at a finer scale.
    inscription: Inscription,
    admitted_zoom: u8,
}

#[derive(Clone)]
struct Inscription {
    text: String,
    rank: u16,
    world: [f64; 2],
    angle: f32,
    size: f32,
    size_floor: f32,
    onset_zoom: f32,
    ink: Color32,
    halo: Option<Halo>,
    repeatable: bool,
    break_line: bool,
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
        let mut prepared = Vec::new();
        let mut parking_marks = Vec::new();
        prepare_incumbents(painter, viewport, rect, &self.incumbents, &mut prepared);
        prepare_parking(
            painter,
            viewport,
            rect,
            parking,
            &mut prepared,
            &mut parking_marks,
        );
        prepare_points(painter, viewport, rect, points, &mut prepared);
        prepare_lines(painter, viewport, rect, lines, &mut prepared);
        prepared.sort_unstable_by(|left, right| match (left.incumbent, right.incumbent) {
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left
                .rank
                .cmp(&right.rank)
                .then_with(|| left.score.total_cmp(&right.score))
                .then_with(|| left.text.cmp(&right.text)),
        });

        let reserved = self
            .incumbents
            .iter()
            .filter(|incumbent| incumbent.admitted_zoom > (viewport.zoom.floor() as u8))
            .filter(|incumbent| {
                rect.contains(map::screen_at(viewport, rect, incumbent.inscription.world))
            })
            .map(|incumbent| incumbent.inscription.text.as_str())
            .collect::<HashSet<_>>();
        // Incumbents precede every newly born obstruction. Because screen
        // separation grows with zoom, this makes admission upward-closed.
        let mut ledger = Ledger::new(&prepared, &reserved);
        ledger.admit(Admission::Incumbent, Repetition::Primary, &[]);
        if viewport.zoom >= 15.0 {
            ledger.admit(Admission::Incumbent, Repetition::Repeat, &[]);
        }
        let live_parking = ledger.admit_parking(&parking_marks);
        ledger.admit(Admission::Fresh, Repetition::Primary, &live_parking);
        if viewport.zoom >= 15.0 {
            ledger.admit(Admission::Fresh, Repetition::Repeat, &live_parking);
        }
        let selected = ledger.finish();

        for &slot in &selected {
            self.remember(&prepared[slot], viewport.zoom);
        }
        Composition {
            labels: selected
                .into_iter()
                .map(|slot| prepared[slot].clone())
                .collect(),
            parking: parking_marks
                .into_iter()
                .zip(live_parking)
                .filter_map(|(mark, live)| live.then_some(mark))
                .collect(),
        }
    }

    fn remember(&mut self, label: &Prepared, zoom: f64) {
        if label.incumbent.is_some() {
            return;
        }
        let Some(inscription) = label.inscription.clone() else {
            return;
        };
        self.incumbents.push(Incumbent {
            inscription,
            admitted_zoom: zoom.floor() as u8,
        });
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Admission {
    Incumbent,
    Fresh,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Repetition {
    Primary,
    Repeat,
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
    footprint: Rect,
}

#[derive(Clone)]
struct Prepared {
    text: String,
    rank: u16,
    score: f32,
    anchor: Pos2,
    angle: f32,
    footprint: Rect,
    galley: Arc<egui::Galley>,
    ink: Color32,
    halo: Option<Halo>,
    repeatable: bool,
    break_line: bool,
    inscription: Option<Inscription>,
    incumbent: Option<usize>,
    patron: Option<usize>,
}

struct Ledger<'a> {
    prepared: &'a [Prepared],
    reserved: &'a HashSet<&'a str>,
    occupied: Vec<Rect>,
    accepted: HashMap<String, Vec<Pos2>>,
    selected: Vec<usize>,
}

impl<'a> Ledger<'a> {
    fn new(prepared: &'a [Prepared], reserved: &'a HashSet<&'a str>) -> Self {
        Self {
            prepared,
            reserved,
            occupied: Vec::new(),
            accepted: HashMap::new(),
            selected: Vec::new(),
        }
    }

    fn admit(&mut self, admission: Admission, repetition: Repetition, live_parking: &[bool]) {
        for (slot, label) in self.prepared.iter().enumerate() {
            if label.incumbent.is_some() != (admission == Admission::Incumbent)
                || admission == Admission::Fresh && self.reserved.contains(label.text.as_str())
                || label
                    .patron
                    .is_some_and(|patron| !live_parking.get(patron).copied().unwrap_or(false))
            {
                continue;
            }
            match repetition {
                Repetition::Primary if self.accepted.contains_key(&label.text) => continue,
                Repetition::Repeat
                    if !label.repeatable
                        || self.accepted.get(&label.text).is_none_or(|anchors| {
                            anchors
                                .iter()
                                .any(|anchor| anchor.distance(label.anchor) < REPEAT_SEPARATION)
                        }) =>
                {
                    continue;
                }
                _ => {}
            }
            if self
                .occupied
                .iter()
                .any(|prior| prior.intersects(label.footprint))
            {
                continue;
            }
            self.occupied.push(label.footprint);
            self.accepted
                .entry(label.text.clone())
                .or_default()
                .push(label.anchor);
            self.selected.push(slot);
            if self.occupied.len() >= LABEL_CEILING {
                return;
            }
        }
    }

    fn admit_parking(&mut self, marks: &[ParkingMark]) -> Vec<bool> {
        marks
            .iter()
            .map(|mark| {
                let free = self
                    .occupied
                    .iter()
                    .all(|prior| !prior.intersects(mark.footprint));
                if free {
                    self.occupied.push(mark.footprint);
                }
                free
            })
            .collect()
    }

    fn finish(self) -> Vec<usize> {
        self.selected
    }
}

fn prepare_incumbents(
    painter: &Painter,
    viewport: map::Viewport,
    rect: Rect,
    incumbents: &[Incumbent],
    prepared: &mut Vec<Prepared>,
) {
    for (slot, incumbent) in incumbents.iter().enumerate() {
        if (viewport.zoom.floor() as u8) < incumbent.admitted_zoom {
            continue;
        }
        let inscription = &incumbent.inscription;
        let maturity = basemap::apparition(viewport.zoom as f32, inscription.onset_zoom);
        if maturity <= 0.01 {
            continue;
        }
        let anchor = map::screen_at(viewport, rect, inscription.world);
        if !rect.contains(anchor) {
            continue;
        }
        let size = inscription.size
            * (1.0 - inscription.size_floor).mul_add(maturity, inscription.size_floor);
        let galley = painter.layout_no_wrap(
            inscription.text.clone(),
            FontId::proportional(size),
            Color32::PLACEHOLDER,
        );
        let shape = text_shape(
            anchor,
            inscription.angle,
            Arc::clone(&galley),
            inscription.ink,
        );
        let footprint = shape.visual_bounding_rect().expand(3.0);
        if rect.contains_rect(footprint) {
            prepared.push(Prepared {
                text: inscription.text.clone(),
                rank: inscription.rank,
                score: 0.0,
                anchor,
                angle: inscription.angle,
                footprint,
                galley,
                ink: inscription.ink.gamma_multiply(maturity),
                halo: inscription.halo.map(|halo| Halo {
                    color: halo.color.gamma_multiply(maturity),
                    ..halo
                }),
                repeatable: inscription.repeatable,
                break_line: inscription.break_line,
                inscription: Some(inscription.clone()),
                incumbent: Some(slot),
                patron: None,
            });
        }
    }
}

fn prepare_parking<'a>(
    painter: &Painter,
    viewport: map::Viewport,
    rect: Rect,
    parking: impl IntoIterator<Item = Parking<'a>>,
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
        let patron = marks.len();
        marks.push(ParkingMark {
            anchor,
            maturity,
            footprint,
        });
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
                text: name.to_owned(),
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
                break_line: false,
                inscription: None,
                incumbent: None,
                patron: Some(patron),
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
                text: label.text.to_owned(),
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
                break_line: false,
                inscription: Some(Inscription {
                    text: label.text.to_owned(),
                    rank: label.rank,
                    world: label.world,
                    angle: 0.0,
                    size: label.size,
                    size_floor: 0.88,
                    onset_zoom: label.onset_zoom,
                    ink: Color32::from_black_alpha(225),
                    halo: Some(Halo {
                        color: Color32::from_white_alpha(92),
                        width: 1.0,
                    }),
                    repeatable: false,
                    break_line: false,
                }),
                incumbent: None,
                patron: None,
            });
        }
    }
}

fn prepare_lines<'a>(
    painter: &Painter,
    viewport: map::Viewport,
    rect: Rect,
    lines: impl IntoIterator<Item = LineLabel<'a>>,
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
            text: label.text.to_owned(),
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
            break_line: label.break_line,
            inscription: Some(Inscription {
                text: label.text.to_owned(),
                rank: label.rank,
                world: map::world_at(viewport, rect, placement.anchor),
                angle: placement.angle,
                size: label.size,
                size_floor: 0.92,
                onset_zoom: label.onset_zoom,
                ink: label.ink,
                halo: label.halo,
                repeatable: label.repeatable,
                break_line: label.break_line,
            }),
            incumbent: None,
            patron: None,
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
                score: bend.mul_add(1_000.0, anchor.distance(viewport.center())),
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

    #[test]
    fn finer_zoom_cannot_displace_an_admitted_world_label() {
        let context = egui::Context::default();
        let mut compositor = Compositor::default();
        let _output = context.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter().clone();
            let coarse = map::Viewport {
                center: [0.5, 0.5],
                zoom: 10.0,
            };
            let path = [
                map::world_at(coarse, VIEW, egui::pos2(100.0, 300.0)),
                map::world_at(coarse, VIEW, egui::pos2(700.0, 300.0)),
            ];
            let old = LineLabel {
                path: &path,
                text: "OLD",
                rank: 900,
                size: 12.0,
                onset_zoom: 0.0,
                ink: Color32::BLACK,
                halo: None,
                repeatable: true,
                break_line: true,
            };
            let first = compositor.compose(
                &painter,
                coarse,
                VIEW,
                std::iter::empty(),
                [old],
                std::iter::empty(),
            );
            assert_eq!(first.labels.len(), 1);
            assert_eq!(first.labels[0].text, "OLD");

            let fine = map::Viewport {
                zoom: coarse.zoom + 1.0,
                ..coarse
            };
            let second = compositor.compose(
                &painter,
                fine,
                VIEW,
                [PointLabel {
                    world: fine.center,
                    text: "NEW BUT STRONGER",
                    rank: 1,
                    size: 12.0,
                    onset_zoom: 11.0,
                }],
                std::iter::empty(),
                std::iter::empty(),
            );
            assert_eq!(second.labels.len(), 1);
            assert_eq!(second.labels[0].text, "OLD");
            assert_eq!(second.labels[0].anchor, first.labels[0].anchor);
        });
    }

    #[test]
    fn coarser_zoom_may_hide_but_cannot_reanchor_an_inscription() {
        let context = egui::Context::default();
        let mut compositor = Compositor::default();
        let _output = context.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter().clone();
            let fine = map::Viewport {
                center: [0.5, 0.5],
                zoom: 11.0,
            };
            let first = compositor.compose(
                &painter,
                fine,
                VIEW,
                [PointLabel {
                    world: fine.center,
                    text: "FIXED",
                    rank: 500,
                    size: 12.0,
                    onset_zoom: 0.0,
                }],
                std::iter::empty(),
                std::iter::empty(),
            );
            assert_eq!(first.labels.len(), 1);

            let coarse = map::Viewport { zoom: 10.0, ..fine };
            let retreat = compositor.compose(
                &painter,
                coarse,
                VIEW,
                [PointLabel {
                    world: map::world_at(coarse, VIEW, egui::pos2(500.0, 300.0)),
                    text: "FIXED",
                    rank: 1,
                    size: 12.0,
                    onset_zoom: 0.0,
                }],
                std::iter::empty(),
                std::iter::empty(),
            );
            assert!(retreat.labels.is_empty());

            let restored = compositor.compose(
                &painter,
                fine,
                VIEW,
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
            );
            assert_eq!(restored.labels.len(), 1);
            assert_eq!(restored.labels[0].anchor, first.labels[0].anchor);
        });
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
