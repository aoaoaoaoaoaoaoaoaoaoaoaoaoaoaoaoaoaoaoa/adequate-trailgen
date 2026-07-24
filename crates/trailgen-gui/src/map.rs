use crate::{forge, library::SavedTrail};
use dwemer_poolrooms::chrome;
use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, f64::consts::PI};
use trailgen_core::{
    Access, Coord, LineString, Route, Terrain, TrailClass, TrailGraph, TrailMarking, TrailStanding,
};

const TILE_EDGE: f64 = 256.0;
const EARTH_CIRCUMFERENCE_M: f64 = 40_075_016.686;
const FIT_PADDING: f32 = 44.0;

pub const MAP_GROUND_SRGB: [u8; 3] = [196, 194, 176];
pub const MAP_GROUND: Color32 =
    Color32::from_rgb(MAP_GROUND_SRGB[0], MAP_GROUND_SRGB[1], MAP_GROUND_SRGB[2]);
pub const INDEX_ISOHYPSE_RADIUS_POINTS: f32 = 0.56;
pub const SELECTED_TRAIL_COLOR: Color32 = Color32::from_rgb(244, 91, 55);
pub const CANDIDATE_COLORS: [Color32; 8] = [
    SELECTED_TRAIL_COLOR,
    Color32::from_rgb(35, 164, 224),
    Color32::from_rgb(245, 150, 38),
    Color32::from_rgb(61, 151, 238),
    Color32::from_rgb(190, 91, 214),
    Color32::from_rgb(237, 67, 132),
    Color32::from_rgb(216, 194, 39),
    Color32::from_rgb(126, 102, 226),
];

pub const fn candidate_color(ordinal: usize, selected: bool) -> Color32 {
    if selected {
        SELECTED_TRAIL_COLOR
    } else {
        CANDIDATE_COLORS[ordinal % CANDIDATE_COLORS.len()]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrailSalience {
    Context,
    Selected,
}

impl TrailSalience {
    const fn width(self) -> f32 {
        match self {
            Self::Context => 4.6,
            Self::Selected => 9.2,
        }
    }

    const fn access_color(self, color: Color32, access: Access) -> Color32 {
        if matches!(access, Access::Closed | Access::Private) {
            match self {
                Self::Context => Color32::from_rgb(188, 112, 101),
                Self::Selected => Color32::from_rgb(234, 72, 53),
            }
        } else {
            color
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct Viewport {
    pub center: [f64; 2],
    pub zoom: f64,
}

impl Viewport {
    pub const MIN_ZOOM: f64 = 1.0;
    pub const MAX_ZOOM: f64 = 23.75;

    pub fn normalize(&mut self) {
        self.center[0] = self.center[0].rem_euclid(1.0);
        self.center[1] = self.center[1].clamp(0.0, 1.0);
        self.zoom = self.zoom.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
    }

    pub fn fit_graph(graph: &TrailGraph, rect: Rect) -> Self {
        fit_coords(graph.vertices.iter().map(|vertex| vertex.coord), rect)
    }

    pub fn fit_route(graph: &TrailGraph, route: &Route, rect: Rect) -> Self {
        fit_coords(route.geometry(graph).points.into_iter(), rect)
    }

    pub fn fit_saved(trail: &SavedTrail, rect: Rect) -> Self {
        fit_coords(trail.geometry().points.into_iter(), rect)
    }
}

pub fn world_pixels(view: Viewport) -> f64 {
    TILE_EDGE * view.zoom.exp2()
}

pub fn world_bounds(view: Viewport, rect: Rect) -> [f64; 4] {
    let scale = world_pixels(view);
    let half = rect.size() * 0.5;
    [
        view.center[0] - f64::from(half.x) / scale,
        view.center[1] - f64::from(half.y) / scale,
        view.center[0] + f64::from(half.x) / scale,
        view.center[1] + f64::from(half.y) / scale,
    ]
}

pub fn world_at(view: Viewport, rect: Rect, point: Pos2) -> [f64; 2] {
    let scale = world_pixels(view);
    [
        view.center[0] + f64::from(point.x - rect.center().x) / scale,
        view.center[1] + f64::from(point.y - rect.center().y) / scale,
    ]
}

pub fn screen_at(view: Viewport, rect: Rect, world: [f64; 2]) -> Pos2 {
    let scale = world_pixels(view);
    rect.center()
        + vec2(
            ((wrapped_delta(world[0], view.center[0])) * scale) as f32,
            ((world[1] - view.center[1]) * scale) as f32,
        )
}

pub fn coord_at(view: Viewport, rect: Rect, point: Pos2) -> Coord {
    world_to_coord(world_at(view, rect, point))
}

pub fn world_from_coord(coord: Coord) -> [f64; 2] {
    let x = (coord.lon + 180.0) / 360.0;
    let latitude = coord.lat.clamp(-85.051_128_78, 85.051_128_78).to_radians();
    let y = (1.0 - latitude.tan().asinh() / PI) * 0.5;
    [x, y]
}

pub fn world_to_coord(world: [f64; 2]) -> Coord {
    let lon = world[0].mul_add(360.0, -180.0);
    let lat = (PI * 2.0_f64.mul_add(-world[1], 1.0))
        .sinh()
        .atan()
        .to_degrees();
    Coord::new(lon, lat)
}

pub fn navigate_with(
    view: &mut Viewport,
    ui: &egui::Ui,
    response: &egui::Response,
    rect: Rect,
    pan: bool,
) -> bool {
    let before = *view;
    if pan && response.dragged_by(egui::PointerButton::Primary) {
        let delta = ui.input(|input| input.pointer.delta());
        let scale = world_pixels(*view);
        view.center[0] -= f64::from(delta.x) / scale;
        view.center[1] -= f64::from(delta.y) / scale;
    }
    if response.hovered() {
        let (scroll, pointer) = ui.input(|input| {
            (
                input.smooth_scroll_delta.y,
                input.pointer.hover_pos().unwrap_or_else(|| rect.center()),
            )
        });
        if scroll.abs() > f32::EPSILON {
            let anchor = world_at(*view, rect, pointer);
            view.zoom = f64::from(scroll)
                .mul_add(0.008, view.zoom)
                .clamp(Viewport::MIN_ZOOM, Viewport::MAX_ZOOM);
            let scale = world_pixels(*view);
            view.center[0] = anchor[0] - f64::from(pointer.x - rect.center().x) / scale;
            view.center[1] = anchor[1] - f64::from(pointer.y - rect.center().y) / scale;
        }
    }
    view.normalize();
    *view != before
}

pub struct Atlas {
    edges: Vec<WorldEdge>,
    classes: Vec<TrailClass>,
}

struct WorldEdge {
    points: Vec<[f64; 2]>,
    bounds: [f64; 4],
    trail_class: TrailClass,
    mark: TrailMark,
    access: Access,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrailMark {
    Solid,
    Dashed,
    DashDot,
    Unmarked,
}

impl TrailMark {
    const ALL: [Self; 4] = [Self::Solid, Self::Dashed, Self::DashDot, Self::Unmarked];

    const fn label(self) -> &'static str {
        match self {
            Self::Solid => "EASY / GRAVEL",
            Self::Dashed => "ROUGHER",
            Self::DashDot => "SEVERE / FADED",
            Self::Unmarked => "UNMARKED / NAVIGATION",
        }
    }
}

impl Atlas {
    pub fn forge(graph: &TrailGraph) -> Self {
        let edges = graph
            .edges
            .iter()
            .map(|edge| {
                let points = edge
                    .geometry
                    .points
                    .iter()
                    .copied()
                    .map(world_from_coord)
                    .collect::<Vec<_>>();
                WorldEdge {
                    bounds: enclosing_bounds(&points),
                    points,
                    trail_class: edge.attr.trail_class,
                    mark: trail_mark(
                        edge.attr.trail_class,
                        edge.attr.standing,
                        edge.attr.marking,
                        edge.attr.terrain,
                        edge.attr.surface.as_deref(),
                    ),
                    access: edge.attr.access,
                }
            })
            .collect::<Vec<_>>();
        let classes = edges
            .iter()
            .map(|edge| edge.trail_class)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Self { edges, classes }
    }

    pub fn paint_legend(&self, painter: &Painter, rect: Rect) {
        if self.classes.is_empty() {
            return;
        }
        let width = 204.0;
        let row = 21.0;
        let height = 51.0 + row * (self.classes.len() + TrailMark::ALL.len()) as f32;
        let plate = Rect::from_min_size(
            pos2(rect.right() - width - 12.0, rect.top() + 12.0),
            vec2(width, height),
        );
        let _fill = painter.rect_filled(plate, 1.0, chrome::SURFACE.gamma_multiply(0.94));
        let _edge = painter.rect_stroke(
            plate,
            1.0,
            Stroke::new(1.0_f32, chrome::EDGE_STRONG),
            egui::StrokeKind::Inside,
        );
        painter.text(
            plate.min + vec2(9.0, 7.0),
            egui::Align2::LEFT_TOP,
            "TRAIL TYPES",
            egui::FontId::monospace(13.0),
            chrome::MUTED,
        );
        for (slot, class) in self.classes.iter().copied().enumerate() {
            let y = (slot as f32).mul_add(row, plate.top() + 29.0);
            paint_trail_tube(
                painter,
                &[pos2(plate.left() + 10.0, y), pos2(plate.left() + 32.0, y)],
                TrailSalience::Context.width(),
                trail_class_color(class),
                TrailMark::Solid,
            );
            painter.text(
                pos2(plate.left() + 41.0, y),
                egui::Align2::LEFT_CENTER,
                trail_class_label(class),
                egui::FontId::monospace(13.0),
                chrome::TEXT,
            );
        }
        let heading_y = (self.classes.len() as f32).mul_add(row, plate.top() + 34.0);
        painter.text(
            pos2(plate.left() + 8.0, heading_y),
            egui::Align2::LEFT_TOP,
            "SURFACE / WAYFINDING",
            egui::FontId::monospace(13.0),
            chrome::MUTED,
        );
        for (slot, mark) in TrailMark::ALL.into_iter().enumerate() {
            let y = (slot as f32).mul_add(row, heading_y + 24.0);
            paint_trail_tube(
                painter,
                &[pos2(plate.left() + 10.0, y), pos2(plate.left() + 32.0, y)],
                TrailSalience::Context.width(),
                trail_class_color(TrailClass::Path),
                mark,
            );
            painter.text(
                pos2(plate.left() + 41.0, y),
                egui::Align2::LEFT_CENTER,
                mark.label(),
                egui::FontId::monospace(13.0),
                chrome::TEXT,
            );
        }
    }

    pub fn paint_network(&self, painter: &Painter, view: Viewport, rect: Rect) {
        let bounds = world_bounds(view, rect);
        for edge in &self.edges {
            if !intersects(edge, bounds) {
                continue;
            }
            let points = edge
                .points
                .iter()
                .copied()
                .map(|world| screen_at(view, rect, world))
                .collect::<Vec<_>>();
            paint_trail_tube(
                painter,
                &points,
                TrailSalience::Context.width(),
                TrailSalience::Context
                    .access_color(trail_class_color(edge.trail_class), edge.access),
                edge.mark,
            );
        }
    }
}

pub fn paint_route(
    painter: &Painter,
    graph: &TrailGraph,
    route: &Route,
    view: Viewport,
    rect: Rect,
    color: Color32,
) {
    let mut at = route.start;
    for edge_id in &route.edges {
        let edge = &graph.edges[edge_id.0];
        let line = edge.oriented_geometry(at);
        let points = line
            .points
            .iter()
            .copied()
            .map(world_from_coord)
            .map(|world| screen_at(view, rect, world))
            .collect::<Vec<_>>();
        paint_trail_tube(
            painter,
            &points,
            TrailSalience::Selected.width(),
            TrailSalience::Selected.access_color(color, edge.attr.access),
            trail_mark(
                edge.attr.trail_class,
                edge.attr.standing,
                edge.attr.marking,
                edge.attr.terrain,
                edge.attr.surface.as_deref(),
            ),
        );
        at = edge
            .traverse(at)
            .expect("validated route edge must be traversable");
    }
}

pub fn paint_saved_trail(
    painter: &Painter,
    trail: &SavedTrail,
    view: Viewport,
    rect: Rect,
    color: Color32,
) {
    for leg in &trail.legs {
        paint_line(
            painter,
            &leg.geometry,
            view,
            rect,
            TrailSalience::Selected.access_color(color, leg.access),
            trail_mark(
                leg.trail_class,
                leg.standing,
                leg.marking,
                leg.terrain,
                leg.surface.as_deref(),
            ),
        );
    }
}

fn paint_line(
    painter: &Painter,
    line: &LineString,
    view: Viewport,
    rect: Rect,
    color: Color32,
    mark: TrailMark,
) {
    let points = line
        .points
        .iter()
        .copied()
        .map(world_from_coord)
        .map(|world| screen_at(view, rect, world))
        .collect::<Vec<_>>();
    if points.len() < 2 {
        return;
    }
    paint_trail_tube(
        painter,
        &points,
        TrailSalience::Selected.width(),
        color,
        mark,
    );
}

pub fn paint_trail_tube(
    painter: &Painter,
    points: &[Pos2],
    width: f32,
    color: Color32,
    mark: TrailMark,
) {
    if points.len() < 2 {
        return;
    }
    let _tube = painter.add(Shape::line(points.to_vec(), Stroke::new(width, color)));
    let core = Stroke::new((width * 0.30).max(1.2), Color32::from_rgb(20, 19, 17));
    if mark == TrailMark::Solid {
        let _core = painter.add(Shape::line(points.to_vec(), core));
    } else {
        painter.extend(patterned_trail_core(points, width, core, mark));
    }
}

fn patterned_trail_core(points: &[Pos2], width: f32, core: Stroke, mark: TrailMark) -> Vec<Shape> {
    match mark {
        TrailMark::Solid => unreachable!("solid trail cores bypass cadence tessellation"),
        TrailMark::Dashed => {
            let gap = width * 0.82;
            Shape::dashed_line_with_offset(points, core, &[width * 1.35], &[gap], gap)
        }
        TrailMark::DashDot => {
            let gap = width * 0.72;
            Shape::dashed_line_with_offset(
                points,
                core,
                &[width * 1.35, core.width * 0.18],
                &[gap; 2],
                gap,
            )
        }
        TrailMark::Unmarked => {
            Shape::dotted_line(points, core.color, width * 2.05, core.width * 0.48)
        }
    }
}

pub fn trail_mark(
    class: TrailClass,
    standing: TrailStanding,
    marking: TrailMarking,
    terrain: Terrain,
    surface: Option<&str>,
) -> TrailMark {
    if marking == TrailMarking::Unmarked {
        return TrailMark::Unmarked;
    }
    if class.pathless()
        || matches!(
            standing,
            TrailStanding::Unmaintained | TrailStanding::Informal | TrailStanding::Historical
        )
        || matches!(terrain, Terrain::Talus | Terrain::Scramble | Terrain::Water)
        || surface_has_any(
            surface,
            &[
                "rock", "rocky", "scree", "boulder", "mud", "sand", "snow", "ice",
            ],
        )
    {
        return TrailMark::DashDot;
    }
    if surface_has_any(
        surface,
        &[
            "dirt",
            "earth",
            "ground",
            "grass",
            "unpaved",
            "native",
            "clay",
            "soil",
            "stone",
            "cobblestone",
            "woodchips",
            "leaf",
            "litter",
        ],
    ) {
        return TrailMark::Dashed;
    }
    if surface_has_any(
        surface,
        &[
            "asphalt",
            "concrete",
            "paved",
            "paving",
            "compacted",
            "gravel",
            "pebblestone",
            "boardwalk",
            "wood",
        ],
    ) {
        return TrailMark::Solid;
    }
    if matches!(
        terrain,
        Terrain::Unknown | Terrain::Forest | Terrain::Alpine
    ) || matches!(
        class,
        TrailClass::Unknown | TrailClass::Path | TrailClass::Steps | TrailClass::Bridleway
    ) {
        TrailMark::Dashed
    } else {
        TrailMark::Solid
    }
}

fn surface_has_any(surface: Option<&str>, needles: &[&str]) -> bool {
    surface.is_some_and(|surface| {
        surface
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|word| {
                needles
                    .iter()
                    .any(|needle| word.eq_ignore_ascii_case(needle))
            })
    })
}

pub fn paint_start(painter: &Painter, trailhead: Coord, view: Viewport, rect: Rect) {
    let anchor = screen_at(view, rect, world_from_coord(trailhead));
    forge::pin(painter, anchor, false);
}

pub fn paint_scale(painter: &Painter, view: Viewport, rect: Rect) {
    let latitude = world_to_coord(view.center).lat.to_radians();
    let meters_per_point = EARTH_CIRCUMFERENCE_M * latitude.cos() / world_pixels(view);
    let target_m = meters_per_point * 105.0;
    let meters = pleasant_length(target_m);
    let width = (meters / meters_per_point) as f32;
    let origin = pos2(rect.left() + 18.0, rect.bottom() - 19.0);
    let stroke = Stroke::new(2.0_f32, Color32::from_rgb(238, 232, 216));
    painter.line_segment([origin, origin + vec2(width, 0.0)], stroke);
    painter.line_segment([origin - vec2(0.0, 4.0), origin + vec2(0.0, 4.0)], stroke);
    painter.line_segment(
        [origin + vec2(width, -4.0), origin + vec2(width, 4.0)],
        stroke,
    );
    let label = if meters >= 1_000.0 {
        format!("{:.1} km", meters / 1_000.0)
    } else {
        format!("{meters:.0} m")
    };
    painter.text(
        origin + vec2(width * 0.5, -5.0),
        egui::Align2::CENTER_BOTTOM,
        label,
        egui::FontId::monospace(11.0),
        Color32::from_rgb(238, 232, 216),
    );
}

pub const fn terrain_color(terrain: Terrain) -> Color32 {
    match terrain {
        Terrain::Unknown => Color32::from_rgb(141, 132, 118),
        Terrain::Trail => Color32::from_rgb(121, 184, 79),
        Terrain::Forest => Color32::from_rgb(62, 132, 75),
        Terrain::Alpine => Color32::from_rgb(204, 186, 104),
        Terrain::Talus => Color32::from_rgb(191, 139, 81),
        Terrain::Scramble => Color32::from_rgb(202, 83, 62),
        Terrain::Pavement => Color32::from_rgb(142, 145, 151),
        Terrain::Road => Color32::from_rgb(158, 112, 73),
        Terrain::Water => Color32::from_rgb(60, 137, 179),
    }
}

pub const fn terrain_label(terrain: Terrain) -> &'static str {
    match terrain {
        Terrain::Unknown => "UNKNOWN",
        Terrain::Trail => "TRAIL",
        Terrain::Forest => "FOREST",
        Terrain::Alpine => "ALPINE",
        Terrain::Talus => "TALUS",
        Terrain::Scramble => "SCRAMBLE",
        Terrain::Pavement => "PAVEMENT",
        Terrain::Road => "ROAD",
        Terrain::Water => "WATER",
    }
}

pub const fn trail_class_color(class: TrailClass) -> Color32 {
    match class {
        TrailClass::Unknown => Color32::from_rgb(207, 199, 184),
        TrailClass::Path => Color32::from_rgb(213, 180, 104),
        TrailClass::Footway => Color32::from_rgb(111, 171, 190),
        TrailClass::Track => Color32::from_rgb(198, 137, 91),
        TrailClass::Service => Color32::from_rgb(181, 122, 104),
        TrailClass::Pedestrian => Color32::from_rgb(176, 151, 198),
        TrailClass::Steps => Color32::from_rgb(204, 112, 108),
        TrailClass::Bridleway => Color32::from_rgb(205, 145, 173),
        TrailClass::Bushwhack => Color32::from_rgb(205, 133, 190),
        TrailClass::Road => Color32::from_rgb(187, 185, 176),
    }
}

pub const fn trail_class_label(class: TrailClass) -> &'static str {
    match class {
        TrailClass::Unknown => "UNCLASSIFIED",
        TrailClass::Path => "PATH",
        TrailClass::Footway => "FOOTWAY",
        TrailClass::Track => "TRACK",
        TrailClass::Service => "SERVICE",
        TrailClass::Pedestrian => "PEDESTRIAN",
        TrailClass::Steps => "STEPS",
        TrailClass::Bridleway => "BRIDLEWAY",
        TrailClass::Bushwhack => "BUSHWHACK",
        TrailClass::Road => "ROAD",
    }
}

pub const fn trail_standing_color(standing: TrailStanding) -> Color32 {
    match standing {
        TrailStanding::Unknown => Color32::from_black_alpha(205),
        TrailStanding::Established => Color32::from_rgb(32, 30, 27),
        TrailStanding::Unmaintained => Color32::from_rgb(190, 114, 61),
        TrailStanding::Informal => Color32::from_rgb(207, 91, 137),
        TrailStanding::Historical => Color32::from_rgb(116, 101, 139),
    }
}

pub const fn trail_standing_label(standing: TrailStanding) -> &'static str {
    match standing {
        TrailStanding::Unknown => "UNKNOWN",
        TrailStanding::Established => "ESTABLISHED",
        TrailStanding::Unmaintained => "UNMAINTAINED",
        TrailStanding::Informal => "INFORMAL / YOLO",
        TrailStanding::Historical => "HISTORICAL",
    }
}

pub const fn trail_standing_badge(standing: TrailStanding) -> &'static str {
    match standing {
        TrailStanding::Unknown => "UNKNOWN",
        TrailStanding::Established => "",
        TrailStanding::Unmaintained => "UNMAINTAINED",
        TrailStanding::Informal => "YOLO PATH",
        TrailStanding::Historical => "HISTORICAL",
    }
}

pub fn frailest_standing(
    standings: impl IntoIterator<Item = TrailStanding>,
) -> Option<TrailStanding> {
    standings.into_iter().max_by_key(|standing| match standing {
        TrailStanding::Established => 0,
        TrailStanding::Unknown => 1,
        TrailStanding::Unmaintained => 2,
        TrailStanding::Informal => 3,
        TrailStanding::Historical => 4,
    })
}

pub fn fit_coords(coords: impl Iterator<Item = Coord>, rect: Rect) -> Viewport {
    let mut worlds = coords.map(world_from_coord);
    let Some(first) = worlds.next() else {
        return Viewport {
            center: [0.5, 0.5],
            zoom: 2.0,
        };
    };
    let bounds = worlds.fold([first[0], first[1], first[0], first[1]], |mut b, p| {
        b[0] = b[0].min(p[0]);
        b[1] = b[1].min(p[1]);
        b[2] = b[2].max(p[0]);
        b[3] = b[3].max(p[1]);
        b
    });
    let available = (rect.size() - Vec2::splat(FIT_PADDING * 2.0)).max(Vec2::splat(32.0));
    let span_x = (bounds[2] - bounds[0]).max(1.0e-9);
    let span_y = (bounds[3] - bounds[1]).max(1.0e-9);
    let pixels_per_world = (f64::from(available.x) / span_x).min(f64::from(available.y) / span_y);
    let mut view = Viewport {
        center: [(bounds[0] + bounds[2]) * 0.5, (bounds[1] + bounds[3]) * 0.5],
        zoom: (pixels_per_world / TILE_EDGE).log2(),
    };
    view.normalize();
    view
}

fn intersects(edge: &WorldEdge, bounds: [f64; 4]) -> bool {
    [-1.0, 0.0, 1.0].into_iter().any(|shift| {
        edge.bounds[0] + shift <= bounds[2]
            && edge.bounds[2] + shift >= bounds[0]
            && edge.bounds[1] <= bounds[3]
            && edge.bounds[3] >= bounds[1]
    })
}

fn enclosing_bounds(points: &[[f64; 2]]) -> [f64; 4] {
    let first = points
        .first()
        .copied()
        .expect("validated graph edge must contain geometry");
    points.iter().skip(1).fold(
        [first[0], first[1], first[0], first[1]],
        |mut bounds, point| {
            bounds[0] = bounds[0].min(point[0]);
            bounds[1] = bounds[1].min(point[1]);
            bounds[2] = bounds[2].max(point[0]);
            bounds[3] = bounds[3].max(point[1]);
            bounds
        },
    )
}

fn wrapped_delta(x: f64, center: f64) -> f64 {
    let delta = x - center;
    if delta > 0.5 {
        delta - 1.0
    } else if delta < -0.5 {
        delta + 1.0
    } else {
        delta
    }
}

fn pleasant_length(target: f64) -> f64 {
    let exponent = target.max(1.0).log10().floor();
    let magnitude = 10_f64.powf(exponent);
    let unit = target / magnitude;
    let step = if unit >= 5.0 {
        5.0
    } else if unit >= 2.0 {
        2.0
    } else {
        1.0
    };
    step * magnitude
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mercator_round_trip_is_tight() {
        let coord = Coord::new(-74.102, 41.221);
        let round_trip = world_to_coord(world_from_coord(coord));
        assert!((round_trip.lon - coord.lon).abs() < 1.0e-10);
        assert!((round_trip.lat - coord.lat).abs() < 1.0e-10);
    }

    #[test]
    fn fitting_a_point_stays_finite() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0));
        let view = fit_coords(std::iter::once(Coord::new(-74.0, 41.0)), rect);
        assert!(view.zoom.is_finite());
        assert!((Viewport::MIN_ZOOM..=Viewport::MAX_ZOOM).contains(&view.zoom));
    }

    #[test]
    fn scale_lengths_are_one_two_or_five_decades() {
        for target in [3.0, 27.0, 650.0, 8_400.0] {
            let length = pleasant_length(target);
            let decade = 10_f64.powf(length.log10().floor());
            assert!([1.0, 2.0, 5.0].contains(&(length / decade)));
            assert!(length <= target);
        }
    }

    #[test]
    fn crossing_edge_survives_viewport_culling() {
        let edge = WorldEdge {
            points: vec![[0.1, 0.5], [0.9, 0.5]],
            bounds: [0.1, 0.5, 0.9, 0.5],
            trail_class: TrailClass::Path,
            mark: TrailMark::Solid,
            access: Access::Open,
        };
        assert!(intersects(&edge, [0.4, 0.4, 0.6, 0.6]));
    }

    #[test]
    fn bushwhacks_have_an_unambiguous_legend_identity() {
        assert_eq!(trail_class_label(TrailClass::Bushwhack), "BUSHWHACK");
        assert_ne!(
            trail_class_color(TrailClass::Bushwhack),
            trail_class_color(TrailClass::Path)
        );
    }

    #[test]
    fn selected_trails_dominate_by_width_and_chroma() {
        const CLASSES: [TrailClass; 10] = [
            TrailClass::Unknown,
            TrailClass::Path,
            TrailClass::Footway,
            TrailClass::Track,
            TrailClass::Service,
            TrailClass::Pedestrian,
            TrailClass::Steps,
            TrailClass::Bridleway,
            TrailClass::Bushwhack,
            TrailClass::Road,
        ];
        let chroma = |color: Color32| {
            color.r().max(color.g()).max(color.b()) - color.r().min(color.g()).min(color.b())
        };

        assert!(TrailSalience::Selected.width() >= TrailSalience::Context.width() * 2.0);
        assert!(
            CANDIDATE_COLORS
                .into_iter()
                .all(|color| chroma(color) >= 120)
        );
        assert!(
            CLASSES
                .into_iter()
                .map(trail_class_color)
                .all(|color| chroma(color) <= 110)
        );
        assert!(
            chroma(TrailSalience::Selected.access_color(SELECTED_TRAIL_COLOR, Access::Closed))
                > chroma(TrailSalience::Context.access_color(SELECTED_TRAIL_COLOR, Access::Closed))
        );
    }

    #[test]
    fn trail_marks_encode_surface_and_condition() {
        assert_eq!(
            trail_mark(
                TrailClass::Path,
                TrailStanding::Established,
                TrailMarking::Unknown,
                Terrain::Trail,
                Some("gravel")
            ),
            TrailMark::Solid
        );
        assert_eq!(
            trail_mark(
                TrailClass::Path,
                TrailStanding::Established,
                TrailMarking::Unknown,
                Terrain::Trail,
                Some("dirt")
            ),
            TrailMark::Dashed
        );
        assert_eq!(
            trail_mark(
                TrailClass::Path,
                TrailStanding::Established,
                TrailMarking::Unknown,
                Terrain::Scramble,
                Some("gravel")
            ),
            TrailMark::DashDot
        );
        assert_eq!(
            trail_mark(
                TrailClass::Path,
                TrailStanding::Unmaintained,
                TrailMarking::Unknown,
                Terrain::Trail,
                Some("gravel")
            ),
            TrailMark::DashDot
        );
        assert_eq!(
            trail_mark(
                TrailClass::Bushwhack,
                TrailStanding::Unknown,
                TrailMarking::Unknown,
                Terrain::Forest,
                None
            ),
            TrailMark::DashDot
        );
        assert_eq!(
            trail_mark(
                TrailClass::Path,
                TrailStanding::Established,
                TrailMarking::Unmarked,
                Terrain::Trail,
                Some("gravel")
            ),
            TrailMark::Unmarked
        );
    }

    #[test]
    fn patterned_cores_cede_subpattern_fragments_to_the_colored_tube() {
        let width = TrailSalience::Context.width();
        let core = Stroke::new(width * 0.30, Color32::BLACK);
        let fragment = [Pos2::ZERO, pos2(width * 0.70, 0.0)];
        let legible = [Pos2::ZERO, pos2(width * 3.0, 0.0)];

        assert!(patterned_trail_core(&fragment, width, core, TrailMark::Dashed).is_empty());
        assert!(patterned_trail_core(&fragment, width, core, TrailMark::DashDot).is_empty());
        assert!(!patterned_trail_core(&legible, width, core, TrailMark::Dashed).is_empty());
        assert!(!patterned_trail_core(&legible, width, core, TrailMark::DashDot).is_empty());
    }
}
