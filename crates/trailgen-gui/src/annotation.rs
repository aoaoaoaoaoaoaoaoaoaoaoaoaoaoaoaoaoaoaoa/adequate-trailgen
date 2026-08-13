use crate::{basemap, forge, map, vector_map::VectorGap};
use egui::{
    Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2, epaint::TextShape, vec2,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

const LABEL_CEILING: usize = 180;
const REPEAT_SEPARATION: f32 = 480.0;
const TRANSITION: Duration = Duration::from_millis(100);
const IDENTITY_SCALE: f64 = 2_097_152.0;
const STRAIGHT_SUPPORT_COSINE: f64 = 0.93;
const PARKING_LABEL_LAG: f32 = 1.25;
const PEAK_HALF_WIDTH: f32 = 5.2;
const PEAK_TEXT_GAP: f32 = 4.0;

pub struct PointLabel<'a> {
    pub world: [f64; 2],
    pub text: &'a str,
    pub kind: basemap::LabelKind,
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

#[cfg(test)]
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
    sort_prepared(&mut prepared);

    let mut ledger = Ledger::new(&prepared);
    let live_parking = ledger.admit_parking(&parking_marks);
    ledger.admit(Repetition::Primary, &live_parking);
    ledger.admit(Repetition::Repeat, &live_parking);
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
                1.0,
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

fn sort_prepared(prepared: &mut [Prepared]) {
    prepared.sort_unstable_by(|left, right| {
        left.birth_zoom
            .total_cmp(&right.birth_zoom)
            .then_with(|| left.rank.cmp(&right.rank))
            .then_with(|| left.text.cmp(&right.text))
            .then_with(|| left.world_key.cmp(&right.world_key))
            .then_with(|| left.angle.total_cmp(&right.angle))
    });
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stamp {
    pub epoch: u64,
    pub presentation: u64,
    pub relief: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Reconciliation {
    pub frame: map::MapFramePlan,
    pub cartography: map::CartographicPlan,
    pub stamp: Stamp,
}

#[derive(Default)]
pub struct Engine {
    stamp: Option<Stamp>,
    labels: Vec<ResidentLabel>,
    parking: Vec<ResidentParking>,
}

struct ResidentLabel {
    label: Prepared,
    born: Instant,
    leaving: Option<Instant>,
}

struct ResidentParking {
    mark: ParkingMark,
    born: Instant,
    leaving: Option<Instant>,
}

impl Engine {
    #[must_use]
    pub fn coherent(&self, stamp: Stamp) -> bool {
        self.stamp == Some(stamp)
    }

    #[must_use]
    pub const fn inhabited(&self) -> bool {
        self.stamp.is_some()
    }

    pub fn reconcile<'a>(
        &mut self,
        painter: &Painter,
        scene: Reconciliation,
        points: impl IntoIterator<Item = PointLabel<'a>>,
        lines: impl IntoIterator<Item = LineLabel<'a>>,
        parking: impl IntoIterator<Item = Parking<'a>>,
    ) -> Arc<Composition> {
        let now = Instant::now();
        self.reap(now);
        let semantic = map::Viewport {
            zoom: scene.cartography.zoom.get(),
            ..scene.frame.viewport
        };
        let mut prepared = Vec::new();
        let mut marks = Vec::new();
        prepare_parking(
            painter,
            semantic,
            scene.frame.rect,
            parking,
            &mut prepared,
            &mut marks,
        );
        prepare_points(painter, semantic, scene.frame.rect, points, &mut prepared);
        prepare_lines(painter, semantic, scene.frame.rect, lines, &mut prepared);
        sort_prepared(&mut prepared);

        let mark_slots = marks
            .iter()
            .enumerate()
            .map(|(slot, mark)| (mark.id, slot))
            .collect::<HashMap<_, _>>();
        let mut ledger = Ledger::new(&prepared);
        let mut live_parking = vec![false; marks.len()];
        for resident in &self.parking {
            if let Some(&slot) = mark_slots.get(&resident.mark.id)
                && ledger.admit_parking_mark(&marks[slot])
            {
                live_parking[slot] = true;
            }
        }
        let mut new_marks = (0..marks.len())
            .filter(|slot| !live_parking[*slot])
            .collect::<Vec<_>>();
        new_marks.sort_unstable_by_key(|slot| marks[*slot].id);
        for slot in new_marks {
            if ledger.admit_parking_mark(&marks[slot]) {
                live_parking[slot] = true;
            }
        }

        let prepared_slots = prepared
            .iter()
            .enumerate()
            .map(|(slot, label)| (label.id, slot))
            .collect::<HashMap<_, _>>();
        for resident in &self.labels {
            if let Some(&slot) = prepared_slots.get(&resident.label.id) {
                ledger.admit_retained(slot, &live_parking);
            }
        }
        ledger.admit(Repetition::Primary, &live_parking);
        ledger.admit(Repetition::Repeat, &live_parking);
        let selected = ledger.finish();
        self.reconcile_parking(
            marks
                .into_iter()
                .zip(live_parking)
                .filter_map(|(mark, live)| live.then_some(mark)),
            now,
        );
        self.reconcile_labels(selected.into_iter().map(|slot| prepared[slot].clone()), now);
        self.stamp = Some(scene.stamp);
        self.project(painter, scene.frame.viewport, scene.frame.rect)
    }

    pub fn project(
        &mut self,
        painter: &Painter,
        camera: map::Viewport,
        rect: Rect,
    ) -> Arc<Composition> {
        let now = Instant::now();
        self.reap(now);
        let mut transitioning = false;
        let parking = self
            .parking
            .iter()
            .filter_map(|resident| {
                let alpha = resident.maturity(now);
                transitioning |= alpha < 1.0;
                (alpha > 0.0).then(|| {
                    let mut mark = resident.mark;
                    mark.anchor = map::screen_at(camera, rect, mark.world);
                    mark.footprint =
                        Rect::from_center_size(mark.anchor, Vec2::splat(19.0)).expand(2.0);
                    mark.maturity *= alpha;
                    mark
                })
            })
            .collect();
        let labels = self
            .labels
            .iter()
            .filter_map(|resident| {
                let alpha = resident.maturity(now);
                transitioning |= alpha < 1.0;
                (alpha > 0.0).then(|| resident.label.project(camera, rect, alpha))
            })
            .collect::<Vec<_>>();
        let gaps = labels
            .iter()
            .filter(|label| label.break_line)
            .map(|label| {
                VectorGap::screen(
                    label.anchor - rect.min.to_vec2(),
                    Vec2::angled(label.angle),
                    label.galley.size() * 0.5 + vec2(3.0, 1.5),
                    label.transition,
                )
            })
            .collect::<Vec<_>>()
            .into();
        if transitioning {
            painter.ctx().request_repaint();
        }
        Arc::new(Composition {
            labels,
            parking,
            gaps,
        })
    }

    fn reconcile_labels(&mut self, selected: impl IntoIterator<Item = Prepared>, now: Instant) {
        let mut prior = self
            .labels
            .drain(..)
            .map(|resident| (resident.label.id, resident))
            .collect::<HashMap<_, _>>();
        let mut next = Vec::new();
        let mut seen = HashSet::new();
        for mut label in selected {
            if !seen.insert(label.id) {
                continue;
            }
            if let Some(mut resident) = prior.remove(&label.id) {
                label.world = resident.label.world;
                label.angle = resident.label.angle;
                resident.label = label;
                resident.leaving = None;
                next.push(resident);
            } else {
                next.push(ResidentLabel {
                    label,
                    born: now,
                    leaving: None,
                });
            }
        }
        next.extend(prior.into_values().map(|mut resident| {
            resident.leaving.get_or_insert(now);
            resident
        }));
        self.labels = next;
    }

    fn reconcile_parking(&mut self, selected: impl IntoIterator<Item = ParkingMark>, now: Instant) {
        let mut prior = self
            .parking
            .drain(..)
            .map(|resident| (resident.mark.id, resident))
            .collect::<HashMap<_, _>>();
        let mut next = Vec::new();
        for mut mark in selected {
            if let Some(mut resident) = prior.remove(&mark.id) {
                mark.world = resident.mark.world;
                resident.mark = mark;
                resident.leaving = None;
                next.push(resident);
            } else {
                next.push(ResidentParking {
                    mark,
                    born: now,
                    leaving: None,
                });
            }
        }
        next.extend(prior.into_values().map(|mut resident| {
            resident.leaving.get_or_insert(now);
            resident
        }));
        self.parking = next;
    }

    fn reap(&mut self, now: Instant) {
        self.labels.retain(|resident| !resident.departed(now));
        self.parking.retain(|resident| !resident.departed(now));
    }
}

impl ResidentLabel {
    fn maturity(&self, now: Instant) -> f32 {
        transition_maturity(self.born, self.leaving, now)
    }

    fn departed(&self, now: Instant) -> bool {
        self.leaving
            .is_some_and(|leaving| now.saturating_duration_since(leaving) >= TRANSITION)
    }
}

impl ResidentParking {
    fn maturity(&self, now: Instant) -> f32 {
        transition_maturity(self.born, self.leaving, now)
    }

    fn departed(&self, now: Instant) -> bool {
        self.leaving
            .is_some_and(|leaving| now.saturating_duration_since(leaving) >= TRANSITION)
    }
}

fn transition_maturity(born: Instant, leaving: Option<Instant>, now: Instant) -> f32 {
    let entering = smooth_time(now.saturating_duration_since(born));
    let leaving = leaving.map_or(1.0, |leaving| {
        1.0 - smooth_time(now.saturating_duration_since(leaving))
    });
    entering.min(leaving)
}

fn smooth_time(elapsed: Duration) -> f32 {
    let phase = (elapsed.as_secs_f32() / TRANSITION.as_secs_f32()).clamp(0.0, 1.0);
    phase * phase * 2.0_f32.mul_add(-phase, 3.0)
}

#[derive(Clone, Copy)]
struct ParkingMark {
    id: Identity,
    world: [f64; 2],
    anchor: Pos2,
    maturity: f32,
    footprint: Rect,
}

#[derive(Clone)]
struct Prepared {
    id: Identity,
    text: String,
    rank: u16,
    birth_zoom: f32,
    world_key: [u64; 2],
    world: [f64; 2],
    offset: Vec2,
    symbol: Option<PointSymbol>,
    anchor: Pos2,
    angle: f32,
    footprint: Rect,
    galley: Arc<egui::Galley>,
    ink: Color32,
    halo: Option<Halo>,
    repeatable: bool,
    break_line: bool,
    patron: Option<usize>,
    transition: f32,
}

#[derive(Clone, Copy)]
enum PointSymbol {
    Peak,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Identity {
    kind: u8,
    text: u64,
    cell: [i64; 2],
}

impl Identity {
    fn forge(kind: u8, text: &str, world: [f64; 2]) -> Self {
        Self {
            kind,
            text: text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
            }),
            cell: world.map(|axis| (axis * IDENTITY_SCALE).round() as i64),
        }
    }
}

impl Prepared {
    fn project(&self, camera: map::Viewport, rect: Rect, transition: f32) -> Self {
        let mut projected = self.clone();
        projected.anchor = map::screen_at(camera, rect, self.world) + self.offset;
        projected.footprint = text_shape(
            projected.anchor,
            projected.angle,
            Arc::clone(&projected.galley),
            projected.ink,
        )
        .visual_bounding_rect()
        .expand(3.0);
        if let Some(symbol) = projected.symbol {
            projected.footprint = projected.footprint.union(symbol_footprint(
                projected.anchor - projected.offset,
                symbol,
            ));
        }
        projected.ink = projected.ink.gamma_multiply(transition);
        projected.halo = projected.halo.map(|halo| Halo {
            color: halo.color.gamma_multiply(transition),
            ..halo
        });
        projected.transition = transition;
        projected
    }
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

    fn admit_retained(&mut self, slot: usize, live_parking: &[bool]) {
        let label = &self.prepared[slot];
        if label
            .patron
            .is_some_and(|patron| !live_parking.get(patron).copied().unwrap_or(false))
            || self
                .occupied
                .iter()
                .any(|prior| prior.intersects(label.footprint))
        {
            return;
        }
        self.occupied.push(label.footprint);
        self.accepted
            .entry(label.text.clone())
            .or_default()
            .push(label.anchor);
        self.selected.push(slot);
    }

    #[cfg(test)]
    fn admit_parking(&mut self, marks: &[ParkingMark]) -> Vec<bool> {
        marks
            .iter()
            .map(|mark| self.admit_parking_mark(mark))
            .collect()
    }

    fn admit_parking_mark(&mut self, mark: &ParkingMark) -> bool {
        let free = self
            .occupied
            .iter()
            .all(|prior| !prior.intersects(mark.footprint));
        if free {
            self.occupied.push(mark.footprint);
        }
        free
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
            id: Identity::forge(0, mark.name.unwrap_or("parking"), mark.world),
            world: mark.world,
            anchor,
            maturity,
            footprint,
        });
        let Some(name) = mark.name else { continue };
        let label_maturity =
            basemap::apparition(viewport.zoom as f32, mark.onset_zoom + PARKING_LABEL_LAG);
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
                id: Identity::forge(1, name, mark.world),
                text: name.to_owned(),
                rank: 720,
                birth_zoom: mark.onset_zoom + PARKING_LABEL_LAG,
                world_key: world_key(mark.world),
                world: mark.world,
                offset: center - anchor,
                symbol: None,
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
                transition: 1.0,
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
        let world_anchor = map::screen_at(viewport, rect, label.world);
        let galley = painter.layout_no_wrap(
            label.text.to_owned(),
            FontId::proportional(label.size),
            Color32::PLACEHOLDER,
        );
        let (identity_kind, ink, halo, symbol, offset) = match label.kind {
            basemap::LabelKind::Place => (
                2,
                Color32::from_black_alpha((225.0 * maturity) as u8),
                Some(Halo {
                    color: Color32::from_white_alpha((92.0 * maturity) as u8),
                    width: 1.0,
                }),
                None,
                Vec2::ZERO,
            ),
            basemap::LabelKind::Lake => (
                3,
                Color32::from_rgb(25, 82, 112).gamma_multiply(maturity),
                None,
                None,
                Vec2::ZERO,
            ),
            basemap::LabelKind::Peak => (
                4,
                Color32::from_rgb(49, 39, 28).gamma_multiply(maturity),
                None,
                Some(PointSymbol::Peak),
                vec2(
                    galley
                        .size()
                        .x
                        .mul_add(0.5, PEAK_HALF_WIDTH + PEAK_TEXT_GAP),
                    0.0,
                ),
            ),
        };
        let anchor = world_anchor + offset;
        let mut footprint = Rect::from_center_size(anchor, galley.size()).expand(3.0);
        if let Some(symbol) = symbol {
            footprint = footprint.union(symbol_footprint(world_anchor, symbol));
        }
        if rect.contains_rect(footprint) {
            prepared.push(Prepared {
                id: Identity::forge(identity_kind, label.text, label.world),
                text: label.text.to_owned(),
                rank: label.rank,
                birth_zoom: label.onset_zoom,
                world_key: world_key(label.world),
                world: label.world,
                offset,
                symbol,
                anchor,
                angle: 0.0,
                footprint,
                galley,
                ink,
                halo,
                repeatable: false,
                break_line: false,
                patron: None,
                transition: 1.0,
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
        let galley = painter.layout_no_wrap(
            label.text.to_owned(),
            FontId::proportional(label.size),
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
                id: Identity::forge(3, label.text, placement.world),
                text: label.text.to_owned(),
                rank: label.rank,
                birth_zoom: placement.birth_zoom,
                world_key: world_key(placement.world),
                world: placement.world,
                offset: Vec2::ZERO,
                symbol: None,
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
                transition: 1.0,
            });
        }
    }
}

const fn world_key(world: [f64; 2]) -> [u64; 2] {
    [world[0].to_bits(), world[1].to_bits()]
}

fn stamp(painter: &Painter, label: &Prepared) {
    if let Some(symbol) = label.symbol {
        paint_symbol(painter, label.anchor - label.offset, symbol, label.ink);
    }
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

fn symbol_footprint(anchor: Pos2, symbol: PointSymbol) -> Rect {
    match symbol {
        PointSymbol::Peak => {
            Rect::from_center_size(anchor, vec2(PEAK_HALF_WIDTH * 2.0, 9.0)).expand(2.0)
        }
    }
}

fn paint_symbol(painter: &Painter, anchor: Pos2, symbol: PointSymbol, ink: Color32) {
    match symbol {
        PointSymbol::Peak => {
            let _peak = painter.add(Shape::closed_line(
                vec![
                    anchor + vec2(0.0, -6.0),
                    anchor + vec2(PEAK_HALF_WIDTH, 3.0),
                    anchor + vec2(-PEAK_HALF_WIDTH, 3.0),
                ],
                Stroke::new(1.35_f32, ink),
            ));
        }
    }
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
    let target_spacing = (label.x + 96.0).max(220.0);
    let base_zoom = (f64::from(target_spacing) / (total * 256.0)).log2() as f32;
    dyadic_centers(total, total_points as f32, label.x)
        .into_iter()
        .filter_map(|center| {
            let (direction, reach) = straight_support(&path, &lengths, center.distance)?;
            let support_zoom = (f64::from(span * 0.5) / (reach * 256.0)).log2() as f32;
            let birth_zoom = (base_zoom + f32::from(center.generation))
                .max(onset_zoom)
                .max(support_zoom);
            if viewport.zoom as f32 + f32::EPSILON < birth_zoom {
                return None;
            }
            let world = point_at(&path, &lengths, center.distance);
            let anchor = map::screen_at(viewport, rect, world);
            let mut angle = direction[1].atan2(direction[0]) as f32;
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

fn straight_support(path: &[[f64; 2]], lengths: &[f64], distance: f64) -> Option<([f64; 2], f64)> {
    let slot = lengths
        .partition_point(|length| *length <= distance)
        .saturating_sub(1)
        .min(path.len().saturating_sub(2));
    let spine = unit(segment_vector(path, slot))?;
    let aligned = |candidate: [f64; 2]| {
        unit(candidate).is_some_and(|candidate| {
            spine[0].mul_add(candidate[0], spine[1] * candidate[1]) >= STRAIGHT_SUPPORT_COSINE
        })
    };
    let mut west = distance - lengths[slot];
    for prior in (0..slot).rev() {
        if !aligned(segment_vector(path, prior)) {
            break;
        }
        west += lengths[prior + 1] - lengths[prior];
    }
    let mut east = lengths[slot + 1] - distance;
    for next in slot + 1..path.len() - 1 {
        if !aligned(segment_vector(path, next)) {
            break;
        }
        east += lengths[next + 1] - lengths[next];
    }
    let reach = west.min(east);
    if reach <= f64::EPSILON {
        return None;
    }
    let start = point_at(path, lengths, distance - reach);
    let end = point_at(path, lengths, distance + reach);
    let direction = unit([end[0] - start[0], end[1] - start[1]])?;
    Some((direction, reach))
}

fn segment_vector(path: &[[f64; 2]], slot: usize) -> [f64; 2] {
    [
        path[slot + 1][0] - path[slot][0],
        path[slot + 1][1] - path[slot][1],
    ]
}

fn unit(vector: [f64; 2]) -> Option<[f64; 2]> {
    let length = vector[0].hypot(vector[1]);
    (length > f64::EPSILON).then(|| [vector[0] / length, vector[1] / length])
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
    fn navigation_history_cannot_alter_a_composition() {
        let context = egui::Context::default();
        context
            .run_ui(egui::RawInput::default(), |ui| {
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
                        kind: basemap::LabelKind::Place,
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
                        kind: basemap::LabelKind::Place,
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
                        kind: basemap::LabelKind::Place,
                        rank: 500,
                        size: 12.0,
                        onset_zoom: 0.0,
                    }],
                    std::iter::empty(),
                    std::iter::empty(),
                );
                assert_eq!(restored.labels.len(), 1);
                assert_eq!(restored.labels[0].anchor, first.labels[0].anchor);
            })
            .drop_without_applying_deltas();
    }

    #[test]
    fn temporal_ledger_retains_an_established_label_against_new_priority() {
        let context = egui::Context::default();
        context
            .run_ui(egui::RawInput::default(), |ui| {
                let painter = ui.painter().clone();
                let camera = map::Viewport {
                    center: [0.5, 0.5],
                    zoom: 11.0,
                };
                let path = [
                    map::world_at(camera, VIEW, egui::pos2(100.0, 300.0)),
                    map::world_at(camera, VIEW, egui::pos2(700.0, 300.0)),
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
                let mut engine = Engine::default();
                let frame = map::MapFramePlan::forge(camera, VIEW);
                let scene = |epoch| Reconciliation {
                    frame,
                    cartography: map::CartographicPlan {
                        zoom: frame.zoom,
                        epoch,
                        moving: false,
                    },
                    stamp: Stamp {
                        epoch,
                        presentation: 0,
                        relief: 0,
                    },
                };
                let _first = engine.reconcile(
                    &painter,
                    scene(0),
                    std::iter::empty(),
                    [old],
                    std::iter::empty(),
                );
                assert_eq!(engine.labels.len(), 1);
                assert_eq!(engine.labels[0].label.text, "OLD");
                let _second = engine.reconcile(
                    &painter,
                    scene(1),
                    [PointLabel {
                        world: camera.center,
                        text: "NEW BUT STRONGER",
                        kind: basemap::LabelKind::Place,
                        rank: 1,
                        size: 12.0,
                        onset_zoom: 0.0,
                    }],
                    [old],
                    std::iter::empty(),
                );
                assert_eq!(
                    engine
                        .labels
                        .iter()
                        .filter(|resident| resident.leaving.is_none())
                        .map(|resident| resident.label.text.as_str())
                        .collect::<Vec<_>>(),
                    ["OLD"]
                );
            })
            .drop_without_applying_deltas();
    }
}
