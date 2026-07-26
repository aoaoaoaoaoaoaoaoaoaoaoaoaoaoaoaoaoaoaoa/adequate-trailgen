use crate::{basemap, forge, map, vector_map::VectorGap};
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

#[derive(Clone, Copy)]
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

pub fn compose<'a>(
    painter: &Painter,
    viewport: map::Viewport,
    rect: Rect,
    points: impl IntoIterator<Item = PointLabel<'a>>,
    lines: impl IntoIterator<Item = LineLabel<'a>>,
    parking: impl IntoIterator<Item = Parking<'a>>,
) -> Composition {
    let mut prepared = Vec::new();
    let mut parking_marks = Vec::new();
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
    prepared.sort_unstable_by(|left, right| {
        left.birth_zoom
            .total_cmp(&right.birth_zoom)
            .then_with(|| left.rank.cmp(&right.rank))
            .then_with(|| left.text.cmp(&right.text))
            .then_with(|| left.world_key.cmp(&right.world_key))
            .then_with(|| left.angle.total_cmp(&right.angle))
    });

    let mut ledger = Ledger::new(&prepared);
    let live_parking = ledger.admit_parking(&parking_marks);
    ledger.admit(Repetition::Primary, &live_parking);
    if viewport.zoom >= 15.0 {
        ledger.admit(Repetition::Repeat, &live_parking);
    }
    let selected = ledger.finish();

    let labels = selected
        .into_iter()
        .map(|slot| prepared[slot].clone())
        .collect::<Vec<_>>();
    let gaps = labels
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
        .into();
    Composition {
        labels,
        parking: parking_marks
            .into_iter()
            .zip(live_parking)
            .filter_map(|(mark, live)| live.then_some(mark))
            .collect(),
        gaps,
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Repetition {
    Primary,
    Repeat,
}

pub struct Composition {
    labels: Vec<Prepared>,
    parking: Vec<ParkingMark>,
    gaps: Arc<[VectorGap]>,
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

    pub fn contour_gaps(&self) -> Arc<[VectorGap]> {
        Arc::clone(&self.gaps)
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
    birth_zoom: f32,
    world_key: [u64; 2],
    anchor: Pos2,
    angle: f32,
    footprint: Rect,
    galley: Arc<egui::Galley>,
    ink: Color32,
    halo: Option<Halo>,
    repeatable: bool,
    break_line: bool,
    patron: Option<usize>,
}

struct Ledger<'a> {
    prepared: &'a [Prepared],
    occupied: Vec<Rect>,
    accepted: HashMap<String, Vec<Pos2>>,
    selected: Vec<usize>,
}

impl<'a> Ledger<'a> {
    fn new(prepared: &'a [Prepared]) -> Self {
        Self {
            prepared,
            occupied: Vec::new(),
            accepted: HashMap::new(),
            selected: Vec::new(),
        }
    }

    fn admit(&mut self, repetition: Repetition, live_parking: &[bool]) {
        for (slot, label) in self.prepared.iter().enumerate() {
            if label
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
                birth_zoom: mark.onset_zoom + 0.45,
                world_key: world_key(mark.world),
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
                birth_zoom: label.onset_zoom,
                world_key: world_key(label.world),
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
        for placement in
            line_placements(label.path, galley.size(), rect, viewport, label.onset_zoom)
        {
            let shape = text_shape(
                placement.anchor,
                placement.angle,
                Arc::clone(&galley),
                label.ink,
            );
            prepared.push(Prepared {
                text: label.text.to_owned(),
                rank: label.rank,
                birth_zoom: placement.birth_zoom,
                world_key: world_key(placement.world),
                anchor: placement.anchor,
                angle: placement.angle,
                footprint: shape.visual_bounding_rect().expand(3.0),
                galley: Arc::clone(&galley),
                ink: label.ink.gamma_multiply(maturity),
                halo: label.halo.map(|halo| Halo {
                    color: halo.color.gamma_multiply(maturity),
                    ..halo
                }),
                repeatable: label.repeatable,
                break_line: label.break_line,
                patron: None,
            });
        }
    }
}

const fn world_key(world: [f64; 2]) -> [u64; 2] {
    [world[0].to_bits(), world[1].to_bits()]
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
    world: [f64; 2],
    anchor: Pos2,
    angle: f32,
    birth_zoom: f32,
}

fn line_placements(
    path: &[[f64; 2]],
    label: Vec2,
    rect: Rect,
    viewport: map::Viewport,
    onset_zoom: f32,
) -> Vec<Placement> {
    let path = path
        .iter()
        .copied()
        .fold(Vec::with_capacity(path.len()), |mut clean, point| {
            if clean.last().is_none_or(|prior: &[f64; 2]| {
                (prior[0] - point[0]).hypot(prior[1] - point[1]) > 1.0e-12
            }) {
                clean.push(point);
            }
            clean
        });
    let lengths = cumulative_lengths(&path);
    let Some(&total) = lengths.last() else {
        return Vec::new();
    };
    let world_points = map::world_pixels(viewport);
    let total_points = total * world_points;
    let span = label.x + 18.0;
    if total_points < f64::from(span) {
        return Vec::new();
    }
    let half_world = f64::from(span * 0.5) / world_points;
    let target_spacing = (label.x + 96.0).max(220.0);
    let base_zoom = (f64::from(target_spacing) / (total * 256.0)).log2() as f32;
    dyadic_centers(total, total_points as f32, label.x)
        .into_iter()
        .filter_map(|center| {
            let birth_zoom = (base_zoom + f32::from(center.generation)).max(onset_zoom);
            if viewport.zoom as f32 + f32::EPSILON < birth_zoom {
                return None;
            }
            let world = point_at(&path, &lengths, center.distance);
            let start = map::screen_at(
                viewport,
                rect,
                point_at(&path, &lengths, center.distance - half_world),
            );
            let anchor = map::screen_at(viewport, rect, world);
            let end = map::screen_at(
                viewport,
                rect,
                point_at(&path, &lengths, center.distance + half_world),
            );
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
            rect.contains_rect(footprint).then_some(Placement {
                world,
                anchor,
                angle,
                birth_zoom,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct DyadicCenter {
    distance: f64,
    generation: u8,
}

fn dyadic_centers(total_world: f64, total_points: f32, label_width: f32) -> Vec<DyadicCenter> {
    let target_spacing = (label_width + 96.0).max(220.0);
    let refinements = if total_points > target_spacing {
        (total_points / target_spacing).log2().floor() as u32
    } else {
        0
    }
    .min(7);
    (0..=refinements)
        .flat_map(|generation| {
            let denominator = 2_u32 << generation;
            (1..denominator)
                .step_by(2)
                .map(move |numerator| DyadicCenter {
                    distance: total_world * f64::from(numerator) / f64::from(denominator),
                    generation: generation as u8,
                })
        })
        .collect()
}

fn cumulative_lengths(path: &[[f64; 2]]) -> Vec<f64> {
    let mut total = 0.0;
    std::iter::once(0.0)
        .chain(path.windows(2).map(|segment| {
            total += (segment[1][0] - segment[0][0]).hypot(segment[1][1] - segment[0][1]);
            total
        }))
        .collect()
}

fn point_at(path: &[[f64; 2]], lengths: &[f64], distance: f64) -> [f64; 2] {
    let slot = lengths
        .partition_point(|progress| *progress < distance)
        .clamp(1, lengths.len().saturating_sub(1));
    let start = lengths[slot - 1];
    let span = lengths[slot] - start;
    let progress = (distance - start) / span;
    [
        (path[slot][0] - path[slot - 1][0]).mul_add(progress, path[slot - 1][0]),
        (path[slot][1] - path[slot - 1][1]).mul_add(progress, path[slot - 1][1]),
    ]
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
    const CAMERA: map::Viewport = map::Viewport {
        center: [0.5, 0.5],
        zoom: 12.0,
    };

    fn world_path(points: impl IntoIterator<Item = Pos2>) -> Vec<[f64; 2]> {
        points
            .into_iter()
            .map(|point| map::world_at(CAMERA, VIEW, point))
            .collect()
    }

    #[test]
    fn line_label_chooses_a_visible_subsegment_and_stays_upright() {
        let placement = line_placements(
            &world_path([egui::pos2(700.0, 300.0), egui::pos2(100.0, 300.0)]),
            vec2(100.0, 12.0),
            VIEW,
            CAMERA,
            0.0,
        )
        .into_iter()
        .next()
        .expect("long road has a placement");
        assert!(placement.angle.abs() < f32::EPSILON);
        assert!(VIEW.contains(placement.anchor));
    }

    #[test]
    fn line_label_rejects_short_and_sharply_bent_paths() {
        assert!(
            line_placements(
                &world_path([egui::pos2(100.0, 100.0), egui::pos2(140.0, 100.0)]),
                vec2(80.0, 12.0),
                VIEW,
                CAMERA,
                0.0,
            )
            .is_empty()
        );
        assert!(
            line_placements(
                &world_path([
                    egui::pos2(200.0, 300.0),
                    egui::pos2(300.0, 300.0),
                    egui::pos2(300.0, 400.0),
                ]),
                vec2(150.0, 12.0),
                VIEW,
                CAMERA,
                0.0,
            )
            .is_empty()
        );
    }

    #[test]
    fn finer_zoom_cannot_displace_an_admitted_world_label() {
        let context = egui::Context::default();
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
            let first = compose(
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
            let second = compose(
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
                [old],
                std::iter::empty(),
            );
            assert_eq!(second.labels.len(), 1);
            assert_eq!(second.labels[0].text, "OLD");
            assert_eq!(second.labels[0].anchor, first.labels[0].anchor);
        });
    }

    #[test]
    fn navigation_history_cannot_alter_a_composition() {
        let context = egui::Context::default();
        let _output = context.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter().clone();
            let home = map::Viewport {
                center: [0.5, 0.5],
                zoom: 11.0,
            };
            let first = compose(
                &painter,
                home,
                VIEW,
                [PointLabel {
                    world: home.center,
                    text: "FIXED",
                    rank: 500,
                    size: 12.0,
                    onset_zoom: 0.0,
                }],
                std::iter::empty(),
                std::iter::empty(),
            );
            assert_eq!(first.labels.len(), 1);

            let away = map::Viewport {
                center: [home.center[0] + 0.05, home.center[1]],
                ..home
            };
            let _interlude = compose(
                &painter,
                away,
                VIEW,
                [PointLabel {
                    world: away.center,
                    text: "FIXED",
                    rank: 500,
                    size: 12.0,
                    onset_zoom: 0.0,
                }],
                std::iter::empty(),
                std::iter::empty(),
            );

            let restored = compose(
                &painter,
                home,
                VIEW,
                [PointLabel {
                    world: home.center,
                    text: "FIXED",
                    rank: 500,
                    size: 12.0,
                    onset_zoom: 0.0,
                }],
                std::iter::empty(),
                std::iter::empty(),
            );
            assert_eq!(restored.labels.len(), 1);
            assert_eq!(restored.labels[0].anchor, first.labels[0].anchor);
        });
    }

    #[test]
    fn zoom_refines_line_anchors_without_moving_existing_world_references() {
        let coarse = dyadic_centers(500.0, 500.0, 80.0)
            .into_iter()
            .map(|center| center.distance / 500.0)
            .collect::<Vec<_>>();
        let fine = dyadic_centers(500.0, 1_000.0, 80.0)
            .into_iter()
            .map(|center| center.distance / 500.0)
            .collect::<Vec<_>>();
        assert!(coarse.iter().all(|anchor| fine.contains(anchor)));
        assert!(fine.len() > coarse.len());
    }
}
