use crate::chrome;
use crate::map::{self, Viewport};
use egui::{Color32, CursorIcon, Painter, Pos2, Rect, Response, Stroke, Ui, pos2, vec2};
use std::collections::BTreeMap;
use trailgen_contract::{AreaCorner, Target};
use trailgen_core::{Coord, source::GeoBounds};
use trailgen_data::SurveyRegion;

const BRONZE: Color32 = Color32::from_rgb(196, 170, 124);
const DEAD: Color32 = Color32::from_black_alpha(104);
const HANDLE_RADIUS: f32 = 5.0;
const HANDLE_GRIP: f32 = 13.0;

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

#[derive(Default)]
pub struct RegionHandles {
    drag: Option<RegionDrag>,
}

struct RegionDrag {
    id: String,
    #[cfg(feature = "egui-test")]
    slot: usize,
    corner: usize,
    before: GeoBounds,
    preview: GeoBounds,
    grab: egui::Vec2,
}

pub enum ResizeEvent {
    None,
    Committed {
        id: String,
        before: GeoBounds,
        bounds: GeoBounds,
    },
    Fault(&'static str),
}

impl RegionHandles {
    #[must_use]
    pub const fn captured(&self) -> bool {
        self.drag.is_some()
    }

    pub fn cancel(&mut self) {
        self.drag = None;
    }

    #[must_use]
    pub fn preview(&self) -> Option<(&str, GeoBounds)> {
        self.drag
            .as_ref()
            .map(|drag| (drag.id.as_str(), drag.preview))
    }

    #[cfg(feature = "egui-test")]
    #[must_use]
    pub fn resizing(&self) -> Option<(usize, AreaCorner)> {
        self.drag
            .as_ref()
            .map(|drag| (drag.slot, AreaCorner::ALL[drag.corner]))
    }

    pub fn interact(
        &mut self,
        viewport: Viewport,
        ui: &Ui,
        canvas: Rect,
        regions: &[SurveyRegion],
        enabled: bool,
    ) -> ResizeEvent {
        if !enabled {
            self.cancel();
            return ResizeEvent::None;
        }
        let (pointer, pressed, released, down) = ui.input(|input| {
            (
                input.pointer.interact_pos(),
                input.pointer.button_pressed(egui::PointerButton::Primary),
                input.pointer.button_released(egui::PointerButton::Primary),
                input.pointer.button_down(egui::PointerButton::Primary),
            )
        });
        if self.drag.is_none()
            && pressed
            && let Some(pointer) = pointer.filter(|pointer| canvas.contains(*pointer))
            && let Some((slot, region, corner, anchor)) =
                nearest_handle(viewport, canvas, regions, pointer)
        {
            #[cfg(not(feature = "egui-test"))]
            let _ = slot;
            self.drag = Some(RegionDrag {
                id: region.id.clone(),
                #[cfg(feature = "egui-test")]
                slot,
                corner,
                before: region.bounds,
                preview: region.bounds,
                grab: pointer - anchor,
            });
        }
        if let Some(drag) = &mut self.drag {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            if (down || released)
                && let Some(pointer) = pointer
            {
                let pointer = (pointer - drag.grab).clamp(canvas.min, canvas.max);
                drag.preview = move_corner(viewport, canvas, drag.before, drag.corner, pointer);
            }
            if released {
                let drag = self.drag.take().expect("captured area handle exists");
                if drag.preview == drag.before {
                    return ResizeEvent::None;
                }
                if trailgen_data::validate_region(drag.preview).is_err() {
                    return ResizeEvent::Fault(
                        "MAP AREA IS TOO SMALL OR OUTSIDE THE SUPPORTED US EXTENT",
                    );
                }
                return ResizeEvent::Committed {
                    id: drag.id,
                    before: drag.before,
                    bounds: drag.preview,
                };
            }
        } else if let Some(pointer) = pointer.filter(|pointer| canvas.contains(*pointer))
            && nearest_handle(viewport, canvas, regions, pointer).is_some()
        {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        ResizeEvent::None
    }
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
        if response.dragged_by(egui::PointerButton::Primary)
            || response.drag_stopped_by(egui::PointerButton::Primary)
        {
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
            return ScribeEvent::Fault("MAP AREA IS TOO SMALL; DRAG A WIDER RECTANGLE");
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

#[derive(Clone, Copy)]
pub struct Scene<'a> {
    pub viewport: Viewport,
    pub canvas: Rect,
    pub regions: &'a [SurveyRegion],
    pub names: &'a BTreeMap<String, String>,
    pub preview: Option<GeoBounds>,
    pub adjustment: Option<(&'a str, GeoBounds)>,
    pub handles: bool,
}

pub fn paint(painter: &Painter, scene: Scene<'_>) {
    let Scene {
        viewport,
        canvas,
        regions,
        names,
        preview,
        adjustment,
        handles,
    } = scene;
    let live = regions
        .iter()
        .enumerate()
        .filter_map(|(slot, region)| {
            let bounds = adjustment
                .filter(|(id, _)| *id == region.id)
                .map_or(region.bounds, |(_, bounds)| bounds);
            screen_rect(viewport, canvas, bounds).map(|rect| (slot, region, bounds, rect))
        })
        .collect::<Vec<_>>();
    let live_rects = live.iter().map(|(_, _, _, rect)| *rect).collect::<Vec<_>>();
    paint_dead_ground(painter, canvas, &live_rects);
    for (slot, region, bounds, rect) in &live {
        painter.rect_stroke(
            *rect,
            0.0,
            Stroke::new(2.0_f32, BRONZE),
            egui::StrokeKind::Inside,
        );
        let text = names
            .get(&region.id)
            .map_or_else(|| slot.to_string(), |name| name.to_ascii_uppercase());
        let galley = painter.layout_no_wrap(text, egui::FontId::monospace(10.5), chrome::HOT);
        let plate = Rect::from_min_size(
            rect.left_top() + vec2(4.0, 4.0),
            galley.size() + vec2(10.0, 6.0),
        );
        painter.rect_filled(plate, 1.0, chrome::SURFACE.gamma_multiply(0.92));
        painter.galley(plate.min + vec2(5.0, 3.0), galley, chrome::HOT);
        if handles {
            paint_handles(painter, viewport, canvas, *slot, *bounds);
        }
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

fn nearest_handle(
    viewport: Viewport,
    canvas: Rect,
    regions: &[SurveyRegion],
    pointer: Pos2,
) -> Option<(usize, &SurveyRegion, usize, Pos2)> {
    regions
        .iter()
        .enumerate()
        .flat_map(|(slot, region)| {
            corner_points(viewport, canvas, region.bounds)
                .into_iter()
                .enumerate()
                .map(move |(corner, anchor)| (slot, region, corner, anchor))
        })
        .filter(|(_, _, _, anchor)| canvas.expand(HANDLE_RADIUS).contains(*anchor))
        .filter_map(|candidate @ (_, _, _, anchor)| {
            let distance2 = anchor.distance_sq(pointer);
            (distance2 <= HANDLE_GRIP * HANDLE_GRIP).then_some((distance2, candidate))
        })
        .min_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, candidate)| candidate)
}

fn move_corner(
    viewport: Viewport,
    canvas: Rect,
    bounds: GeoBounds,
    corner: usize,
    pointer: Pos2,
) -> GeoBounds {
    let coord = map::coord_at(viewport, canvas, pointer);
    let east_gap = map::coord_at(viewport, canvas, pointer + vec2(12.0, 0.0)).lon - coord.lon;
    let south_gap = coord.lat - map::coord_at(viewport, canvas, pointer + vec2(0.0, 12.0)).lat;
    let mut moved = bounds;
    match corner {
        0 => {
            moved.west = coord.lon.min(bounds.east - east_gap.abs());
            moved.north = coord.lat.max(bounds.south + south_gap.abs());
        }
        1 => {
            moved.east = coord.lon.max(bounds.west + east_gap.abs());
            moved.north = coord.lat.max(bounds.south + south_gap.abs());
        }
        2 => {
            moved.east = coord.lon.max(bounds.west + east_gap.abs());
            moved.south = coord.lat.min(bounds.north - south_gap.abs());
        }
        3 => {
            moved.west = coord.lon.min(bounds.east - east_gap.abs());
            moved.south = coord.lat.min(bounds.north - south_gap.abs());
        }
        _ => unreachable!("a map area has four corners"),
    }
    moved
}

fn paint_handles(
    painter: &Painter,
    viewport: Viewport,
    canvas: Rect,
    slot: usize,
    bounds: GeoBounds,
) {
    for (corner, point) in corner_points(viewport, canvas, bounds)
        .into_iter()
        .enumerate()
    {
        if !canvas.expand(HANDLE_RADIUS).contains(point) {
            continue;
        }
        let grip = Rect::from_center_size(point, vec2(HANDLE_GRIP * 2.0, HANDLE_GRIP * 2.0));
        crate::witness::rect(
            painter.ctx(),
            Target::AreaHandle {
                slot,
                corner: AreaCorner::ALL[corner],
            },
            grip,
        );
        painter.circle_filled(point, HANDLE_RADIUS + 1.5, chrome::SURFACE);
        painter.circle_filled(point, HANDLE_RADIUS, BRONZE);
        painter.circle_stroke(point, HANDLE_RADIUS, Stroke::new(1.2_f32, chrome::HOT));
    }
}

fn corner_points(viewport: Viewport, canvas: Rect, bounds: GeoBounds) -> [Pos2; 4] {
    [
        Coord::new(bounds.west, bounds.north),
        Coord::new(bounds.east, bounds.north),
        Coord::new(bounds.east, bounds.south),
        Coord::new(bounds.west, bounds.south),
    ]
    .map(|coord| map::screen_at(viewport, canvas, map::world_from_coord(coord)))
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
