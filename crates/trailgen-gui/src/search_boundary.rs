use crate::{
    library::SearchBoundary,
    map::{self, Viewport},
};
use egui::{Color32, CursorIcon, Painter, Pos2, Rect, Response, Shape, Stroke, Ui};
use trailgen_core::Coord;

const SAMPLE_DISTANCE_POINTS: f32 = 4.0;
const MIN_AREA_POINTS2: f32 = 400.0;
const SEARCH_MAGENTA: Color32 = Color32::from_rgb(190, 91, 147);
const ROUND_JOIN_LIMIT: f32 = 1.8;

#[derive(Default)]
pub struct BoundaryScribe {
    active: bool,
    stroke: Vec<Pos2>,
}

pub enum BoundaryEvent {
    None,
    Committed(SearchBoundary),
    Fault(String),
}

impl BoundaryScribe {
    #[must_use]
    pub const fn active(&self) -> bool {
        self.active
    }

    pub fn arm(&mut self) {
        self.active = true;
        self.stroke.clear();
    }

    pub fn disarm(&mut self) {
        self.active = false;
        self.stroke.clear();
    }

    pub fn interact(
        &mut self,
        viewport: Viewport,
        ui: &Ui,
        response: &Response,
        rect: Rect,
    ) -> BoundaryEvent {
        if !self.active {
            return BoundaryEvent::None;
        }
        response.clone().on_hover_cursor(CursorIcon::Crosshair);
        if response.drag_started_by(egui::PointerButton::Primary) {
            self.stroke.clear();
            if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
                self.stroke.push(origin.clamp(rect.min, rect.max));
            }
        }
        if response.dragged_by(egui::PointerButton::Primary)
            && let Some(point) = response.interact_pointer_pos()
        {
            self.sample(point.clamp(rect.min, rect.max));
        }
        if !response.drag_stopped_by(egui::PointerButton::Primary) {
            return BoundaryEvent::None;
        }
        if let Some(point) = response.interact_pointer_pos() {
            self.sample(point.clamp(rect.min, rect.max));
        }
        self.active = false;
        let stroke = std::mem::take(&mut self.stroke);
        if stroke.len() < 3 || polygon_area2(&stroke).abs() < MIN_AREA_POINTS2 * 2.0 {
            return BoundaryEvent::Fault(
                "DRAW A WIDER LOOP; THE SEARCH AREA IS TOO SMALL".to_owned(),
            );
        }
        let points = stroke
            .into_iter()
            .map(|point| map::coord_at(viewport, rect, point))
            .collect::<Vec<_>>();
        match SearchBoundary::forge(points) {
            Ok(boundary) => BoundaryEvent::Committed(boundary),
            Err(error) => BoundaryEvent::Fault(format!("INVALID SEARCH AREA: {error}")),
        }
    }

    pub fn preview(&self) -> &[Pos2] {
        &self.stroke
    }

    fn sample(&mut self, point: Pos2) {
        if self
            .stroke
            .last()
            .is_none_or(|prior| prior.distance(point) >= SAMPLE_DISTANCE_POINTS)
        {
            self.stroke.push(point);
        }
    }
}

pub fn paint(
    painter: &Painter,
    viewport: Viewport,
    canvas: Rect,
    boundary: Option<&SearchBoundary>,
    preview: &[Pos2],
) {
    if let Some(boundary) = boundary {
        let points = boundary
            .points()
            .iter()
            .copied()
            .map(|point| {
                map::screen_at(
                    viewport,
                    canvas,
                    map::world_from_coord(Coord::new(point.lon, point.lat)),
                )
            })
            .collect::<Vec<_>>();
        paint_ring(painter, &points, SEARCH_MAGENTA);
    }
    if preview.len() >= 2 {
        painter.extend(round_stroke(
            preview,
            preview.len() >= 3,
            Stroke::new(2.5_f32, SEARCH_MAGENTA),
        ));
    }
}

fn paint_ring(painter: &Painter, points: &[Pos2], color: Color32) {
    if points.len() < 3 {
        return;
    }
    painter.extend(round_stroke(
        points,
        true,
        Stroke::new(5.0_f32, Color32::from_black_alpha(180)),
    ));
    painter.extend(round_stroke(points, true, Stroke::new(2.4_f32, color)));
}

fn round_stroke(points: &[Pos2], closed: bool, stroke: Stroke) -> Vec<Shape> {
    if points.len() < 2 {
        return Vec::new();
    }
    let dangerous = (0..points.len())
        .map(|slot| join_reach(points, slot, closed) > ROUND_JOIN_LIMIT)
        .collect::<Vec<_>>();
    let dangerous_count = dangerous.iter().filter(|dangerous| **dangerous).count();
    if closed && dangerous_count == 0 {
        return vec![Shape::closed_line(points.to_vec(), stroke)];
    }
    let start = if closed {
        dangerous
            .iter()
            .position(|dangerous| *dangerous)
            .expect("nonempty dangerous-join set")
    } else {
        0
    };
    let mut shapes = Vec::with_capacity(dangerous_count * 2 + 3);
    let mut run = vec![points[start]];
    let steps = if closed {
        points.len()
    } else {
        points.len() - 1
    };
    for step in 1..=steps {
        let slot = start + step;
        let slot = if closed { slot % points.len() } else { slot };
        run.push(points[slot]);
        if dangerous[slot] || step == steps {
            shapes.push(Shape::line(std::mem::take(&mut run), stroke));
            shapes.push(Shape::circle_filled(
                points[slot],
                stroke.width * 0.5,
                stroke.color,
            ));
            run.push(points[slot]);
        }
    }
    if !closed {
        shapes.push(Shape::circle_filled(
            points[0],
            stroke.width * 0.5,
            stroke.color,
        ));
    }
    shapes
}

fn join_reach(points: &[Pos2], slot: usize, closed: bool) -> f32 {
    if !closed && (slot == 0 || slot + 1 == points.len()) {
        return 1.0;
    }
    let prior = points[(slot + points.len() - 1) % points.len()];
    let point = points[slot];
    let next = points[(slot + 1) % points.len()];
    let incoming = (point - prior).normalized();
    let outgoing = (next - point).normalized();
    let denominator = ((1.0 + incoming.dot(outgoing)) * 0.5).max(f32::EPSILON);
    denominator.sqrt().recip()
}

fn polygon_area2(points: &[Pos2]) -> f32 {
    points
        .iter()
        .copied()
        .zip(points.iter().copied().cycle().skip(1))
        .take(points.len())
        .map(|(a, b)| a.x.mul_add(b.y, -(b.x * a.y)))
        .sum()
}
