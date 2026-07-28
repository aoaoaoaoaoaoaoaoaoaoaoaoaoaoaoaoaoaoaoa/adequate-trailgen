use crate::{
    library::SearchBoundary,
    map::{self, Viewport},
};
use egui::{Color32, CursorIcon, Painter, Pos2, Rect, Response, Stroke, Ui};
use trailgen_core::Coord;

const SAMPLE_DISTANCE_POINTS: f32 = 4.0;
const MIN_AREA_POINTS2: f32 = 400.0;
const SEARCH_MAGENTA: Color32 = Color32::from_rgb(190, 91, 147);

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
            .collect();
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
            .collect();
        paint_ring(painter, points, SEARCH_MAGENTA);
    }
    if preview.len() >= 2 {
        let mut points = preview.to_vec();
        if points.len() >= 3 {
            points.push(points[0]);
        }
        let _preview = painter.line(points, Stroke::new(2.5_f32, SEARCH_MAGENTA));
    }
}

fn paint_ring(painter: &Painter, points: Vec<Pos2>, color: Color32) {
    if points.len() < 3 {
        return;
    }
    let _shadow = painter.add(egui::Shape::closed_line(
        points.clone(),
        Stroke::new(5.0_f32, Color32::from_black_alpha(180)),
    ));
    let _frame = painter.add(egui::Shape::closed_line(
        points,
        Stroke::new(2.4_f32, color),
    ));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_ignores_pointer_jitter() {
        let mut scribe = BoundaryScribe::default();
        scribe.sample(Pos2::ZERO);
        scribe.sample(Pos2::new(1.0, 1.0));
        scribe.sample(Pos2::new(5.0, 0.0));
        assert_eq!(scribe.preview(), [Pos2::ZERO, Pos2::new(5.0, 0.0)]);
    }

    #[test]
    fn screen_area_is_orientation_agnostic() {
        let clockwise = [
            Pos2::new(0.0, 0.0),
            Pos2::new(0.0, 10.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(10.0, 0.0),
        ];
        let counterclockwise = clockwise.into_iter().rev().collect::<Vec<_>>();
        assert!(
            (polygon_area2(&clockwise).abs() - polygon_area2(&counterclockwise).abs()).abs()
                < f32::EPSILON
        );
    }
}
