use crate::map::{self, Viewport};
use dwemer_poolrooms::chrome;
use egui::{Color32, CursorIcon, Painter, Pos2, Rect, Response, Stroke, Ui, pos2, vec2};
use trailgen_core::{Coord, source::GeoBounds};
use trailgen_data::SurveyRegion;

const BRONZE: Color32 = Color32::from_rgb(196, 170, 124);
const DEAD: Color32 = Color32::from_black_alpha(104);

#[derive(Default)]
pub struct RegionScribe {
    active: bool,
    anchor: Option<Pos2>,
    cursor: Option<Pos2>,
}

pub enum ScribeEvent {
    None,
    Committed(GeoBounds),
    Fault(&'static str),
}

impl RegionScribe {
    #[must_use]
    pub const fn active(&self) -> bool {
        self.active
    }

    pub const fn arm(&mut self) {
        self.active = true;
        self.anchor = None;
        self.cursor = None;
    }

    pub const fn disarm(&mut self) {
        self.active = false;
        self.anchor = None;
        self.cursor = None;
    }

    pub fn interact(
        &mut self,
        viewport: Viewport,
        ui: &Ui,
        response: &Response,
        rect: Rect,
    ) -> ScribeEvent {
        if !self.active {
            return ScribeEvent::None;
        }
        response.clone().on_hover_cursor(CursorIcon::Crosshair);
        if response.drag_started_by(egui::PointerButton::Primary) {
            self.anchor = ui
                .input(|input| input.pointer.press_origin())
                .map(|point| point.clamp(rect.min, rect.max));
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            self.cursor = response
                .interact_pointer_pos()
                .map(|point| point.clamp(rect.min, rect.max));
        }
        if !response.drag_stopped_by(egui::PointerButton::Primary) {
            return ScribeEvent::None;
        }
        let Some((anchor, cursor)) = self.anchor.zip(self.cursor) else {
            self.anchor = None;
            self.cursor = None;
            return ScribeEvent::Fault("DRAG A RECTANGLE; A CLICK HAS NO AREA");
        };
        self.anchor = None;
        self.cursor = None;
        if (anchor.x - cursor.x).abs() < 12.0 || (anchor.y - cursor.y).abs() < 12.0 {
            return ScribeEvent::Fault("REGION IS TOO SMALL; DRAG A WIDER RECTANGLE");
        }
        self.active = false;
        let a = map::coord_at(viewport, rect, anchor);
        let b = map::coord_at(viewport, rect, cursor);
        ScribeEvent::Committed(GeoBounds::new(
            a.lon.min(b.lon),
            a.lat.min(b.lat),
            a.lon.max(b.lon),
            a.lat.max(b.lat),
        ))
    }

    #[must_use]
    pub fn preview(&self, viewport: Viewport, rect: Rect) -> Option<GeoBounds> {
        let (anchor, cursor) = self.anchor.zip(self.cursor)?;
        let a = map::coord_at(viewport, rect, anchor);
        let b = map::coord_at(viewport, rect, cursor);
        Some(GeoBounds::new(
            a.lon.min(b.lon),
            a.lat.min(b.lat),
            a.lon.max(b.lon),
            a.lat.max(b.lat),
        ))
    }
}

pub fn paint(
    painter: &Painter,
    viewport: Viewport,
    canvas: Rect,
    regions: &[SurveyRegion],
    preview: Option<GeoBounds>,
) {
    let live = regions
        .iter()
        .filter_map(|region| screen_rect(viewport, canvas, region.bounds))
        .collect::<Vec<_>>();
    paint_dead_ground(painter, canvas, &live);
    for (slot, rect) in live.iter().copied().enumerate() {
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(2.0_f32, BRONZE),
            egui::StrokeKind::Inside,
        );
        let plate = Rect::from_min_size(rect.left_top() + vec2(4.0, 4.0), vec2(24.0, 18.0));
        painter.rect_filled(plate, 1.0, chrome::SURFACE.gamma_multiply(0.92));
        painter.text(
            plate.center(),
            egui::Align2::CENTER_CENTER,
            (slot + 1).to_string(),
            egui::FontId::monospace(10.0),
            chrome::HOT,
        );
    }
    if let Some(preview) = preview
        && let Some(rect) = screen_rect(viewport, canvas, preview)
    {
        painter.rect_filled(
            rect,
            0.0,
            Color32::from_rgba_unmultiplied(196, 170, 124, 38),
        );
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(2.5_f32, chrome::HOT),
            egui::StrokeKind::Inside,
        );
    }
}

fn paint_dead_ground(painter: &Painter, canvas: Rect, live: &[Rect]) {
    let mut y = vec![canvas.top(), canvas.bottom()];
    for rect in live {
        y.extend([
            rect.top().clamp(canvas.top(), canvas.bottom()),
            rect.bottom().clamp(canvas.top(), canvas.bottom()),
        ]);
    }
    y.sort_by(f32::total_cmp);
    y.dedup_by(|left, right| (*left - *right).abs() <= f32::EPSILON);
    for band in y.windows(2) {
        let top = band[0];
        let bottom = band[1];
        if bottom <= top {
            continue;
        }
        let middle = (top + bottom) * 0.5;
        let mut spans = live
            .iter()
            .filter(|rect| rect.top() <= middle && middle <= rect.bottom())
            .map(|rect| {
                (
                    rect.left().max(canvas.left()),
                    rect.right().min(canvas.right()),
                )
            })
            .filter(|(left, right)| left < right)
            .collect::<Vec<_>>();
        spans.sort_by(|left, right| left.0.total_cmp(&right.0));
        let mut cursor = canvas.left();
        for (left, right) in spans {
            if cursor < left {
                painter.rect_filled(
                    Rect::from_min_max(pos2(cursor, top), pos2(left, bottom)),
                    0.0,
                    DEAD,
                );
            }
            cursor = cursor.max(right);
        }
        if cursor < canvas.right() {
            painter.rect_filled(
                Rect::from_min_max(pos2(cursor, top), pos2(canvas.right(), bottom)),
                0.0,
                DEAD,
            );
        }
    }
}

fn screen_rect(viewport: Viewport, canvas: Rect, bounds: GeoBounds) -> Option<Rect> {
    let north_west = map::screen_at(
        viewport,
        canvas,
        map::world_from_coord(Coord::new(bounds.west, bounds.north)),
    );
    let south_east = map::screen_at(
        viewport,
        canvas,
        map::world_from_coord(Coord::new(bounds.east, bounds.south)),
    );
    let rect = Rect::from_two_pos(north_west, south_east).intersect(canvas);
    rect.is_positive().then_some(rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_rects_follow_lon_lat_orientation() {
        let viewport = Viewport {
            center: map::world_from_coord(Coord::new(-74.0, 41.0)),
            zoom: 10.0,
        };
        let canvas = Rect::from_min_max(Pos2::ZERO, pos2(800.0, 600.0));
        let rect = screen_rect(viewport, canvas, GeoBounds::new(-74.1, 40.9, -73.9, 41.1))
            .expect("region should cross the viewport");
        assert!(rect.left() < rect.right());
        assert!(rect.top() < rect.bottom());
        assert!(rect.contains(canvas.center()));
    }
}
