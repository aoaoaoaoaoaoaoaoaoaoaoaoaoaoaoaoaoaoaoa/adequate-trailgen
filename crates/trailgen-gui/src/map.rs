use crate::{cadence, forge, library::SavedTrail, trail_map::TrailField};
use dwemer_poolrooms::chrome;
use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    f64::consts::PI,
    time::{Duration, Instant},
};
use trailgen_core::{
    Access, Coord, EdgeId, Route, Terrain, TrailClass, TrailGraph, TrailMarking, TrailStanding,
};

const TILE_EDGE: f64 = 256.0;
const CARTOGRAPHIC_SETTLE: Duration = Duration::from_millis(120);
pub const EARTH_CIRCUMFERENCE_M: f64 = 40_075_016.686;
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

#[derive(Debug, Default)]
pub struct ScaleBar {
    current_m: Option<f64>,
    born: Option<Instant>,
    departing: Option<(f64, Instant)>,
}

impl ScaleBar {
    pub fn paint(&mut self, painter: &Painter, view: Viewport, rect: Rect) {
        let latitude = world_to_coord(view.center).lat.to_radians();
        let meters_per_point = EARTH_CIRCUMFERENCE_M * latitude.cos() / world_pixels(view);
        let target = pleasant_length(meters_per_point * 105.0);
        let current = self.current_m.get_or_insert(target);
        let current_width = *current / meters_per_point;
        if current.to_bits() != target.to_bits() && !(72.0..=148.0).contains(&current_width) {
            self.departing = Some((*current, Instant::now()));
            *current = target;
            self.born = Some(Instant::now());
        }
        let now = Instant::now();
        if let Some((departing, begun)) = self.departing {
            let maturity = smooth_transition(now.saturating_duration_since(begun));
            paint_scale_length(painter, rect, departing, meters_per_point, 1.0 - maturity);
            if maturity >= 1.0 {
                self.departing = None;
            } else {
                painter.ctx().request_repaint();
            }
        }
        let maturity = self.born.map_or(1.0, |begun| {
            smooth_transition(now.saturating_duration_since(begun))
        });
        paint_scale_length(painter, rect, *current, meters_per_point, maturity);
        if maturity < 1.0 {
            painter.ctx().request_repaint();
        } else {
            self.born = None;
        }
    }
}

fn smooth_transition(elapsed: Duration) -> f32 {
    let phase = (elapsed.as_secs_f32() / 0.16).clamp(0.0, 1.0);
    phase * phase * 2.0_f32.mul_add(-phase, 3.0)
}

pub const fn candidate_color(ordinal: usize, selected: bool) -> Color32 {
    if selected {
        SELECTED_TRAIL_COLOR
    } else {
        CANDIDATE_COLORS[ordinal % CANDIDATE_COLORS.len()]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrailSalience {
    Context,
    Selected,
}

impl TrailSalience {
    pub const fn width(self) -> f32 {
        match self {
            Self::Context => 4.6,
            Self::Selected => 9.2,
        }
    }

    pub const fn access_color(self, color: Color32, access: Access) -> Color32 {
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

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct CameraZoom(f64);

impl CameraZoom {
    #[must_use]
    pub fn from_viewport(viewport: Viewport) -> Self {
        assert!(
            viewport.zoom.is_finite(),
            "camera zoom must be finite before frame planning"
        );
        Self(viewport.zoom)
    }

    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }

    #[must_use]
    pub fn world_points(self) -> f64 {
        TILE_EDGE * self.0.exp2()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapFramePlan {
    pub viewport: Viewport,
    pub rect: Rect,
    pub zoom: CameraZoom,
    pub world_points: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct CartographicPlan {
    pub zoom: CameraZoom,
    pub epoch: u64,
    pub moving: bool,
}

#[derive(Debug)]
pub struct CartographicClock {
    observed: Viewport,
    committed_zoom: CameraZoom,
    last_motion: Option<Instant>,
    epoch: u64,
}

impl CartographicClock {
    #[must_use]
    pub fn new(viewport: Viewport) -> Self {
        Self {
            observed: viewport,
            committed_zoom: CameraZoom::from_viewport(viewport),
            last_motion: None,
            epoch: 0,
        }
    }

    pub fn observe(&mut self, viewport: Viewport, ctx: &egui::Context) -> CartographicPlan {
        let (plan, repaint_after) = self.resolve(viewport, Instant::now());
        if let Some(repaint_after) = repaint_after {
            ctx.request_repaint_after(repaint_after);
        }
        plan
    }

    fn resolve(
        &mut self,
        viewport: Viewport,
        now: Instant,
    ) -> (CartographicPlan, Option<Duration>) {
        let changed = viewport != self.observed;
        if changed {
            self.observed = viewport;
            self.last_motion = Some(now);
        }

        let repaint_after = if let Some(last_motion) = self.last_motion {
            let quiet = now.saturating_duration_since(last_motion);
            if quiet >= CARTOGRAPHIC_SETTLE {
                self.committed_zoom = CameraZoom::from_viewport(viewport);
                self.epoch = self.epoch.saturating_add(1);
                self.last_motion = None;
                None
            } else {
                Some(CARTOGRAPHIC_SETTLE.saturating_sub(quiet))
            }
        } else {
            None
        };
        (
            CartographicPlan {
                zoom: self.committed_zoom,
                epoch: self.epoch,
                moving: self.last_motion.is_some(),
            },
            repaint_after,
        )
    }
}

impl MapFramePlan {
    #[must_use]
    pub fn forge(viewport: Viewport, rect: Rect) -> Self {
        assert!(rect.is_positive(), "map frame requires a positive viewport");
        let zoom = CameraZoom::from_viewport(viewport);
        Self {
            viewport,
            rect,
            zoom,
            world_points: zoom.world_points(),
        }
    }

    #[must_use]
    pub fn world_bounds(self) -> [f64; 4] {
        let half = self.rect.size() * 0.5;
        [
            self.viewport.center[0] - f64::from(half.x) / self.world_points,
            self.viewport.center[1] - f64::from(half.y) / self.world_points,
            self.viewport.center[0] + f64::from(half.x) / self.world_points,
            self.viewport.center[1] + f64::from(half.y) / self.world_points,
        ]
    }
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
    CameraZoom::from_viewport(view).world_points()
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
    classes: Vec<TrailClass>,
    field: TrailField,
}

pub struct RouteOverlay {
    field: TrailField,
}

pub struct WorldEdge {
    pub endpoints: [usize; 2],
    pub points: Vec<[f64; 2]>,
    pub length_world: f64,
    pub lineage: Option<CadenceLineage>,
    pub color: Color32,
    pub trail_class: TrailClass,
    pub mark: TrailMark,
    pub access: Access,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CadenceLineage {
    Stem { datum_world: f64, reverse: bool },
    // Arbitrary cycle lengths cannot close a fixed screen-space period. A chord
    // inherits both endpoint phases and confines the reset to one interior splice.
    Chord { endpoint_datums_world: [f64; 2] },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TrailMark {
    Solid,
    Dashed,
    DashDot,
    Unmarked,
}

impl TrailMark {
    const ALL: [Self; 4] = [Self::Solid, Self::Dashed, Self::DashDot, Self::Unmarked];

    const fn patterned(self) -> bool {
        !matches!(self, Self::Solid)
    }

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
        let mut edges = graph
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
                    endpoints: [edge.a.0, edge.b.0],
                    length_world: world_polyline_length(&points),
                    points,
                    lineage: None,
                    color: trail_class_color(edge.attr.trail_class),
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
        weave_cadence(graph.vertices.len(), &mut edges);
        let classes = edges
            .iter()
            .map(|edge| edge.trail_class)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let field = TrailField::forge(&edges);
        Self { classes, field }
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
            let _swatch = painter.line_segment(
                [pos2(plate.left() + 10.0, y), pos2(plate.left() + 32.0, y)],
                Stroke::new(TrailSalience::Context.width(), trail_class_color(class)),
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

    pub fn paint_network(&mut self, painter: &Painter, frame: MapFramePlan) {
        self.field.paint(painter, frame);
    }
}

impl RouteOverlay {
    pub fn candidates(graph: &TrailGraph, routes: &[Route], order: &[usize]) -> Self {
        let mut edges = candidate_crown(routes, order)
            .into_iter()
            .map(|(edge_id, color)| {
                let edge = &graph.edges[edge_id.0];
                let points = edge
                    .geometry
                    .points
                    .iter()
                    .copied()
                    .map(world_from_coord)
                    .collect::<Vec<_>>();
                WorldEdge {
                    endpoints: [edge.a.0, edge.b.0],
                    length_world: world_polyline_length(&points),
                    points,
                    lineage: None,
                    color,
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
        weave_cadence(graph.vertices.len(), &mut edges);
        Self {
            field: TrailField::overlay(&edges),
        }
    }

    pub fn paint(&mut self, painter: &Painter, frame: MapFramePlan) {
        self.field.paint(painter, frame);
    }
}

fn candidate_crown(routes: &[Route], order: &[usize]) -> Vec<(EdgeId, Color32)> {
    let mut crown = BTreeMap::new();
    let mut z = 0;
    for (ordinal, slot) in order.iter().copied().enumerate() {
        let color = candidate_color(ordinal, false);
        for edge in &routes[slot].edges {
            crown.insert(*edge, (z, color));
            z += 1;
        }
    }
    let mut crown = crown
        .into_iter()
        .map(|(edge, (z, color))| (z, edge, color))
        .collect::<Vec<_>>();
    crown.sort_unstable_by_key(|(z, _, _)| *z);
    crown
        .into_iter()
        .map(|(_, edge, color)| (edge, color))
        .collect()
}

fn weave_cadence(vertex_count: usize, edges: &mut [WorldEdge]) {
    let mut adjacency = vec![Vec::new(); vertex_count];
    for (edge_id, edge) in edges
        .iter()
        .enumerate()
        .filter(|(_, edge)| edge.mark.patterned())
    {
        for endpoint in edge.endpoints {
            adjacency[endpoint].push(edge_id);
        }
    }

    for mark in [TrailMark::Dashed, TrailMark::DashDot, TrailMark::Unmarked] {
        let mut datums = vec![None; vertex_count];
        for root in 0..vertex_count {
            if datums[root].is_some()
                || !adjacency[root]
                    .iter()
                    .any(|edge_id| edges[*edge_id].mark == mark)
            {
                continue;
            }
            datums[root] = Some(0.0);
            let mut frontier = vec![root];
            while let Some(vertex) = frontier.pop() {
                let datum = datums[vertex].expect("frontier vertices own cadence datums");
                for edge_id in adjacency[vertex].iter().copied() {
                    if edges[edge_id].mark != mark || edges[edge_id].lineage.is_some() {
                        continue;
                    }
                    let edge = &edges[edge_id];
                    let [a, b] = edge.endpoints;
                    let other = if vertex == a {
                        b
                    } else {
                        assert_eq!(vertex, b, "cadence adjacency must remain incident");
                        a
                    };
                    if datums[other].is_none() {
                        datums[other] = Some(datum + edge.length_world);
                        edges[edge_id].lineage = Some(CadenceLineage::Stem {
                            datum_world: datum,
                            reverse: vertex == b,
                        });
                        frontier.push(other);
                    } else {
                        edges[edge_id].lineage = Some(CadenceLineage::Chord {
                            endpoint_datums_world: [
                                datums[a].expect("chord endpoint a owns a cadence datum"),
                                datums[b].expect("chord endpoint b owns a cadence datum"),
                            ],
                        });
                    }
                }
            }
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
    let mut strokes = Vec::with_capacity(route.edges.len());
    for edge_id in &route.edges {
        let edge = &graph.edges[edge_id.0];
        let line = edge.oriented_geometry(at);
        let world = line
            .points
            .iter()
            .copied()
            .map(world_from_coord)
            .collect::<Vec<_>>();
        strokes.push(SelectedStroke {
            length_world: world_polyline_length(&world),
            points: world
                .into_iter()
                .map(|world| screen_at(view, rect, world))
                .collect(),
            color: TrailSalience::Selected.access_color(color, edge.attr.access),
            mark: trail_mark(
                edge.attr.trail_class,
                edge.attr.standing,
                edge.attr.marking,
                edge.attr.terrain,
                edge.attr.surface.as_deref(),
            ),
        });
        at = edge
            .traverse(at)
            .expect("validated route edge must be traversable");
    }
    paint_selected_strokes(painter, &strokes, view);
}

pub fn paint_saved_trail(
    painter: &Painter,
    trail: &SavedTrail,
    view: Viewport,
    rect: Rect,
    color: Color32,
) {
    let strokes = trail
        .legs
        .iter()
        .map(|leg| {
            let world = leg
                .geometry
                .points
                .iter()
                .copied()
                .map(world_from_coord)
                .collect::<Vec<_>>();
            SelectedStroke {
                length_world: world_polyline_length(&world),
                points: world
                    .into_iter()
                    .map(|world| screen_at(view, rect, world))
                    .collect(),
                color: TrailSalience::Selected.access_color(color, leg.access),
                mark: trail_mark(
                    leg.trail_class,
                    leg.standing,
                    leg.marking,
                    leg.terrain,
                    leg.surface.as_deref(),
                ),
            }
        })
        .collect::<Vec<_>>();
    paint_selected_strokes(painter, &strokes, view);
}

struct SelectedStroke {
    points: Vec<Pos2>,
    length_world: f64,
    color: Color32,
    mark: TrailMark,
}

fn paint_selected_strokes(painter: &Painter, strokes: &[SelectedStroke], view: Viewport) {
    let width = TrailSalience::Selected.width();
    let core_width = trail_core(width).width;
    let scale = world_pixels(view);
    let lattice = cadence::WorldLevel::at_zoom(view.zoom);
    let cells_per_world = lattice.cells_per_world();
    let cell_points = (scale / cells_per_world) as f32;
    let mut datum_world = 0.0;
    for stroke in strokes {
        let pattern = trail_lattice_pattern(stroke.mark, core_width, cell_points);
        let phase = (datum_world * cells_per_world).rem_euclid(2.0) as f32 * cell_points;
        let _length =
            paint_trail_tube_pattern(painter, &stroke.points, width, stroke.color, pattern, phase);
        datum_world += stroke.length_world;
    }
}

fn trail_lattice_pattern(
    mark: TrailMark,
    core_width: f32,
    cell_points: f32,
) -> Option<cadence::Pattern> {
    match mark {
        TrailMark::Solid => None,
        TrailMark::Dashed => Some(cadence::Pattern::Dash {
            dash: cell_points,
            gap: cell_points,
        }),
        TrailMark::DashDot => {
            let micro = cell_points * 0.25;
            Some(cadence::Pattern::DashDot {
                dash: micro * 3.0,
                gap: micro * 2.0,
                dot: micro,
            })
        }
        TrailMark::Unmarked => Some(cadence::Pattern::Dots {
            spacing: cell_points * 2.0,
            radius: core_width * 0.48,
        }),
    }
}

pub fn paint_trail_tube(
    painter: &Painter,
    points: &[Pos2],
    width: f32,
    color: Color32,
    mark: TrailMark,
) {
    paint_trail_tube_at(painter, points, width, color, mark, 0.0);
}

pub fn paint_trail_tube_at(
    painter: &Painter,
    points: &[Pos2],
    width: f32,
    color: Color32,
    mark: TrailMark,
    datum: f32,
) -> f32 {
    let core = trail_core(width);
    paint_trail_tube_pattern(
        painter,
        points,
        width,
        color,
        trail_pattern(mark, width, core.width),
        datum,
    )
}

fn paint_trail_tube_pattern(
    painter: &Painter,
    points: &[Pos2],
    width: f32,
    color: Color32,
    pattern: Option<cadence::Pattern>,
    datum: f32,
) -> f32 {
    if points.len() < 2 {
        return 0.0;
    }
    let length = cadence::polyline_length(points);
    let _tube = painter.add(Shape::line(points.to_vec(), Stroke::new(width, color)));
    let core = trail_core(width);
    if let Some(pattern) = pattern {
        let mut shapes = Vec::new();
        pattern.tessellate(
            points.iter().copied(),
            core,
            datum,
            f32::INFINITY,
            &mut shapes,
        );
        painter.extend(shapes);
    } else {
        let _core = painter.add(Shape::line(points.to_vec(), core));
    }
    length
}

pub fn trail_core(width: f32) -> Stroke {
    Stroke::new((width * 0.30).max(1.2), Color32::from_rgb(20, 19, 17))
}

pub fn trail_pattern(mark: TrailMark, width: f32, core_width: f32) -> Option<cadence::Pattern> {
    match mark {
        TrailMark::Solid => None,
        TrailMark::Dashed => Some(cadence::Pattern::Dash {
            dash: width * 1.35,
            gap: width * 0.82,
        }),
        TrailMark::DashDot => Some(cadence::Pattern::DashDot {
            dash: width * 1.35,
            gap: width * 0.72,
            dot: core_width * 0.18,
        }),
        TrailMark::Unmarked => Some(cadence::Pattern::Dots {
            spacing: width * 2.05,
            radius: core_width * 0.48,
        }),
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

pub fn paint_start(painter: &Painter, trailhead: Coord, view: Viewport, rect: Rect, seized: bool) {
    let anchor = screen_at(view, rect, world_from_coord(trailhead));
    forge::pin(painter, anchor, seized);
}

fn paint_scale_length(
    painter: &Painter,
    rect: Rect,
    meters: f64,
    meters_per_point: f64,
    maturity: f32,
) {
    let width = (meters / meters_per_point) as f32;
    let origin = pos2(rect.left() + 18.0, rect.bottom() - 19.0);
    let ink = Color32::from_rgb(238, 232, 216).gamma_multiply(maturity);
    let stroke = Stroke::new(2.0_f32, ink);
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
        ink,
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

fn world_polyline_length(points: &[[f64; 2]]) -> f64 {
    points
        .windows(2)
        .map(|window| {
            let dx = wrapped_delta(window[1][0], window[0][0]);
            let dy = window[1][1] - window[0][1];
            dx.hypot(dy)
        })
        .sum()
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
    use trailgen_core::{ConstraintVerdict, RouteMetrics, VertexId};

    fn route(edges: impl IntoIterator<Item = usize>) -> Route {
        Route {
            name: String::new(),
            start: VertexId(0),
            edges: edges.into_iter().map(EdgeId).collect(),
            pareto_rank: 0,
            metrics: RouteMetrics::default(),
            verdict: ConstraintVerdict {
                satisfied: true,
                violations: Vec::new(),
                audit: Vec::new(),
                penalty: 0.0,
            },
            score: 0.0,
        }
    }

    #[test]
    fn cartographic_epoch_ignores_every_intermediate_camera_sample() {
        let initial = Viewport {
            center: [0.5, 0.5],
            zoom: 12.0,
        };
        let begun = Instant::now();
        let mut clock = CartographicClock::new(initial);
        let intermediate = Viewport {
            zoom: 12.4,
            ..initial
        };
        let (moving, _) = clock.resolve(intermediate, begun);
        assert!(moving.moving);
        assert_eq!(moving.zoom.get().to_bits(), initial.zoom.to_bits());
        assert_eq!(moving.epoch, 0);

        let target = Viewport {
            zoom: 13.0,
            ..initial
        };
        let (still_moving, _) = clock.resolve(
            target,
            begun + CARTOGRAPHIC_SETTLE.saturating_sub(Duration::from_millis(1)),
        );
        assert!(still_moving.moving);
        assert_eq!(still_moving.zoom.get().to_bits(), initial.zoom.to_bits());

        let (settled, _) = clock.resolve(target, begun + CARTOGRAPHIC_SETTLE * 2);
        assert!(!settled.moving);
        assert_eq!(settled.zoom.get().to_bits(), target.zoom.to_bits());
        assert_eq!(settled.epoch, 1);
    }

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
    fn candidate_crown_keeps_only_the_topmost_copy_of_shared_support() {
        let routes = [route([0, 1]), route([1, 2])];
        let crown = candidate_crown(&routes, &[0, 1]);

        assert_eq!(
            crown,
            vec![
                (EdgeId(0), candidate_color(0, false)),
                (EdgeId(1), candidate_color(1, false)),
                (EdgeId(2), candidate_color(1, false)),
            ]
        );
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
    fn forks_inherit_one_node_cadence_datum() {
        let mut edges = vec![
            cadence_edge(0, 1, 0.01),
            cadence_edge(1, 2, 0.02),
            cadence_edge(1, 3, 0.03),
        ];
        weave_cadence(4, &mut edges);
        let trunk = endpoint_datums(&edges[0]);
        let left = endpoint_datums(&edges[1]);
        let right = endpoint_datums(&edges[2]);

        assert!((trunk[1] - left[0]).abs() < f64::EPSILON);
        assert!((trunk[1] - right[0]).abs() < f64::EPSILON);
    }

    #[test]
    fn cycles_confine_inconsistent_phase_to_one_chord_seam() {
        let mut edges = vec![
            cadence_edge(0, 1, 0.01),
            cadence_edge(1, 2, 0.02),
            cadence_edge(2, 0, 0.03),
        ];
        weave_cadence(3, &mut edges);

        assert_eq!(
            edges
                .iter()
                .filter(|edge| matches!(edge.lineage, Some(CadenceLineage::Chord { .. })))
                .count(),
            1
        );
        assert!(edges.iter().all(|edge| edge.lineage.is_some()));
    }

    fn cadence_edge(a: usize, b: usize, length_world: f64) -> WorldEdge {
        WorldEdge {
            endpoints: [a, b],
            points: vec![[0.0, 0.0], [length_world, 0.0]],
            length_world,
            lineage: None,
            color: trail_class_color(TrailClass::Path),
            trail_class: TrailClass::Path,
            mark: TrailMark::Dashed,
            access: Access::Open,
        }
    }

    fn endpoint_datums(edge: &WorldEdge) -> [f64; 2] {
        match edge.lineage.expect("test edge owns cadence lineage") {
            CadenceLineage::Stem {
                datum_world,
                reverse: false,
            } => [datum_world, datum_world + edge.length_world],
            CadenceLineage::Stem {
                datum_world,
                reverse: true,
            } => [datum_world + edge.length_world, datum_world],
            CadenceLineage::Chord {
                endpoint_datums_world,
            } => endpoint_datums_world,
        }
    }
}
