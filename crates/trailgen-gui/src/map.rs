use crate::chrome;
use crate::{cadence, forge, library::SavedTrail, trail_map::TrailField};
use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    f64::consts::PI,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
pub use trailgen_contract::TrailColoring;
use trailgen_core::{
    Access, Coord, EdgeDisposition, EdgeId, Route, RouteShape, Terrain, TrailClass, TrailGraph,
    TrailMarking, TrailStanding,
};

const TILE_EDGE: f64 = 256.0;
const CARTOGRAPHIC_SETTLE: Duration = Duration::from_millis(90);
pub const EARTH_CIRCUMFERENCE_M: f64 = 40_075_016.686;
const FIT_PADDING: f32 = 44.0;

pub const MAP_GROUND_SRGB: [u8; 3] = [196, 194, 176];
pub const MAP_GROUND: Color32 =
    Color32::from_rgb(MAP_GROUND_SRGB[0], MAP_GROUND_SRGB[1], MAP_GROUND_SRGB[2]);
pub const INDEX_ISOHYPSE_RADIUS_POINTS: f32 = 0.56;
pub const SELECTED_TRAIL_COLOR: Color32 = Color32::from_rgb(244, 91, 55);

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

pub fn candidate_color(ordinal: usize, selected: bool) -> Color32 {
    if selected {
        SELECTED_TRAIL_COLOR
    } else {
        static PALETTE: OnceLock<Mutex<CandidatePalette>> = OnceLock::new();
        PALETTE
            .get_or_init(|| Mutex::new(CandidatePalette::default()))
            .lock()
            .expect("candidate palette lock poisoned")
            .color(ordinal)
    }
}

#[derive(Default)]
struct CandidatePalette {
    colors: Vec<Color32>,
    occupied: BTreeSet<[u8; 3]>,
}

impl CandidatePalette {
    fn color(&mut self, ordinal: usize) -> Color32 {
        while self.colors.len() <= ordinal {
            let identity = self.colors.len();
            let target = candidate_color_target(identity);
            let [red, green, blue, _] = target.to_array();
            let channels = [red, green, blue];
            let color = (0..=1_530)
                .filter_map(|probe| {
                    if probe == 0 {
                        return Some(channels);
                    }
                    let wave = (probe - 1) / 6 + 1;
                    let axis = ((probe - 1) / 2) % 3;
                    let sign = if probe % 2 == 0 { 1 } else { -1 };
                    let wave = i16::try_from(wave).expect("palette probe fits i16");
                    let value = i16::from(channels[axis]) + sign * wave;
                    let mut candidate = channels;
                    candidate[axis] = u8::try_from(value).ok()?;
                    Some(candidate)
                })
                .find(|candidate| {
                    let [red, green, blue] = *candidate;
                    !(self.occupied.contains(candidate)
                        || f32::from(green) > f32::from(red) * 1.15
                            && f32::from(green) > f32::from(blue) * 1.15)
                })
                .expect("24-bit candidate palette exhausted near a perceptual target");
            self.occupied.insert(color);
            self.colors
                .push(Color32::from_rgb(color[0], color[1], color[2]));
        }
        self.colors[ordinal]
    }
}

fn candidate_color_target(ordinal: usize) -> Color32 {
    let phase = if ordinal < 8 {
        ordinal as f64 / 8.0
    } else {
        let base = 1_usize << ordinal.ilog2();
        let slot = ordinal - base;
        (slot * 2 + 1) as f64 / (base * 2) as f64
    };
    let hue = palette_hue(phase);
    let grain = splitmix64(ordinal as u64);
    let lightness = if ordinal < 8 {
        0.72
    } else {
        ((grain & 15) as f64).mul_add(0.011, 0.64)
    };
    let chroma = (((grain >> 8) & 3) as f64).mul_add(0.012, 0.17);
    oklch_srgb(lightness, chroma, hue)
}

fn palette_hue(phase: f64) -> f64 {
    const WARM_START: f64 = 18.0;
    const WARM_END: f64 = 82.0;
    const COOL_START: f64 = 220.0;
    const COOL_END: f64 = 355.0;
    const WARM_SPAN: f64 = WARM_END - WARM_START;
    const COOL_SPAN: f64 = COOL_END - COOL_START;
    const SPAN: f64 = WARM_SPAN + COOL_SPAN;
    let distance = phase * SPAN;
    if distance < WARM_SPAN {
        WARM_START + distance
    } else {
        COOL_START + distance - WARM_SPAN
    }
}

const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn oklch_srgb(lightness: f64, chroma: f64, hue_degrees: f64) -> Color32 {
    let hue = hue_degrees.to_radians();
    let (sin, cos) = hue.sin_cos();
    let in_gamut = |chroma: f64| {
        let ok_a = chroma * cos;
        let ok_b = chroma * sin;
        let lms_l = 0.215_803_757_3_f64
            .mul_add(ok_b, 0.396_337_777_4_f64.mul_add(ok_a, lightness))
            .powi(3);
        let lms_m = 0.063_854_172_8_f64
            .mul_add(-ok_b, 0.105_561_345_8_f64.mul_add(-ok_a, lightness))
            .powi(3);
        let lms_s = 1.291_485_548_f64
            .mul_add(-ok_b, 0.089_484_177_5_f64.mul_add(-ok_a, lightness))
            .powi(3);
        [
            0.230_969_929_2_f64.mul_add(
                lms_s,
                3.307_711_591_3_f64.mul_add(-lms_m, 4.076_741_662_1 * lms_l),
            ),
            0.341_319_396_5_f64.mul_add(
                -lms_s,
                2.609_757_401_1_f64.mul_add(lms_m, -1.268_438_004_6 * lms_l),
            ),
            1.707_614_701_f64.mul_add(
                lms_s,
                0.703_418_614_7_f64.mul_add(-lms_m, -0.004_196_086_3 * lms_l),
            ),
        ]
    };
    let mut lo = 0.0;
    let mut hi = chroma;
    for _ in 0..14 {
        let probe = (lo + hi) * 0.5;
        if in_gamut(probe)
            .into_iter()
            .all(|channel| (0.0..=1.0).contains(&channel))
        {
            lo = probe;
        } else {
            hi = probe;
        }
    }
    let [red, green, blue] = in_gamut(lo);
    let gamma = |linear: f64| {
        let srgb = if linear <= 0.003_130_8 {
            12.92 * linear
        } else {
            1.055_f64.mul_add(linear.powf(1.0 / 2.4), -0.055)
        };
        (srgb.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    Color32::from_rgb(gamma(red), gamma(green), gamma(blue))
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

const fn coloring_tab(coloring: TrailColoring) -> &'static str {
    match coloring {
        TrailColoring::Class => "TYPE",
        TrailColoring::Formality => "FORM",
        TrailColoring::Terrain => "TERRAIN",
    }
}

const fn coloring_heading(coloring: TrailColoring) -> &'static str {
    match coloring {
        TrailColoring::Class => "COLOR · TRAIL TYPE",
        TrailColoring::Formality => "COLOR · FORMALITY",
        TrailColoring::Terrain => "COLOR · TERRAIN",
    }
}

pub const fn coloring_shader_code(coloring: TrailColoring) -> u32 {
    match coloring {
        TrailColoring::Class => 0,
        TrailColoring::Formality => 1,
        TrailColoring::Terrain => 2,
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

#[must_use]
pub fn meters_per_point(view: Viewport) -> f64 {
    let latitude = world_to_coord(view.center).lat.to_radians();
    EARTH_CIRCUMFERENCE_M * latitude.cos() / world_pixels(view)
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
    zoom: bool,
) -> bool {
    let before = *view;
    if pan && response.dragged_by(egui::PointerButton::Primary) {
        let delta = ui.input(|input| input.pointer.delta());
        let scale = world_pixels(*view);
        view.center[0] -= f64::from(delta.x) / scale;
        view.center[1] -= f64::from(delta.y) / scale;
    }
    if zoom && response.hovered() {
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
    terrains: Vec<Terrain>,
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
    pub standing: TrailStanding,
    pub terrain: Terrain,
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
                    standing: edge.attr.standing,
                    terrain: edge.attr.terrain,
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
        let terrains = edges
            .iter()
            .map(|edge| edge.terrain)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let field = TrailField::forge(&edges);
        Self {
            classes,
            terrains,
            field,
        }
    }

    pub fn show_legend(
        &self,
        ctx: &egui::Context,
        rect: Rect,
        current: TrailColoring,
    ) -> LegendResponse {
        const WIDTH: f32 = 224.0;
        if self.classes.is_empty() {
            return LegendResponse::default();
        }
        let mut answer = LegendResponse::default();
        let shown = egui::Area::new(egui::Id::new("trail-color-legend"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(rect.right_top() + vec2(-12.0, 12.0))
            .constrain_to(rect)
            .movable(false)
            .sense(egui::Sense::click_and_drag())
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(chrome::SURFACE.gamma_multiply(0.94))
                    .stroke(Stroke::new(1.0_f32, chrome::EDGE_STRONG))
                    .corner_radius(1.0)
                    .inner_margin(9.0)
                    .show(ui, |ui| {
                        ui.set_width(WIDTH - 18.0);
                        let _heading = ui.label(chrome::eyebrow("TRAIL COLORS"));
                        let _tabs = ui.horizontal(|ui| {
                            for coloring in TrailColoring::ALL {
                                let response = chrome::command(
                                    ui,
                                    coloring_tab(coloring),
                                    coloring == current,
                                );
                                answer.tabs.push((coloring, response.rect));
                                if response.clicked() {
                                    answer.clicked = Some((coloring, response.rect));
                                }
                            }
                        });
                        ui.add_space(4.0);
                        let _heading = ui.label(chrome::eyebrow(coloring_heading(current)));
                        match current {
                            TrailColoring::Class => {
                                for class in self.classes.iter().copied() {
                                    legend_row(
                                        ui,
                                        trail_class_label(class),
                                        trail_class_color(class),
                                        None,
                                    );
                                }
                            }
                            TrailColoring::Formality => {
                                legend_row(
                                    ui,
                                    "FORMAL",
                                    formality_color(false, TrailSalience::Context),
                                    None,
                                );
                                legend_row(
                                    ui,
                                    "INFORMAL",
                                    formality_color(true, TrailSalience::Context),
                                    None,
                                );
                            }
                            TrailColoring::Terrain => {
                                for terrain in self.terrains.iter().copied() {
                                    legend_row(
                                        ui,
                                        terrain_label(terrain),
                                        terrain_color(terrain, TrailSalience::Context),
                                        None,
                                    );
                                }
                            }
                        }
                        ui.add_space(4.0);
                        let _heading =
                            ui.label(chrome::eyebrow("LINE STYLE · SURFACE / WAYFINDING"));
                        for mark in TrailMark::ALL {
                            legend_row(
                                ui,
                                mark.label(),
                                trail_class_color(TrailClass::Path),
                                Some(mark),
                            );
                        }
                    });
            });
        answer.rect = Some(shown.response.rect);
        answer
    }

    pub fn paint_network(
        &mut self,
        painter: &Painter,
        frame: MapFramePlan,
        coloring: TrailColoring,
    ) {
        self.field.paint_colored(painter, frame, coloring);
    }
}

#[derive(Default)]
pub struct LegendResponse {
    pub rect: Option<Rect>,
    pub tabs: Vec<(TrailColoring, Rect)>,
    pub clicked: Option<(TrailColoring, Rect)>,
}

fn legend_row(ui: &mut egui::Ui, label: &str, color: Color32, mark: Option<TrailMark>) {
    let (rect, _response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 21.0), egui::Sense::hover());
    let from = pos2(rect.left() + 2.0, rect.center().y);
    let to = pos2(rect.left() + 24.0, rect.center().y);
    if let Some(mark) = mark {
        paint_trail_tube(
            ui.painter(),
            &[from, to],
            TrailSalience::Context.width(),
            color,
            mark,
        );
    } else {
        let _swatch = ui.painter().line_segment(
            [from, to],
            Stroke::new(TrailSalience::Context.width(), color),
        );
    }
    ui.painter().text(
        pos2(rect.left() + 33.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(13.0),
        chrome::TEXT,
    );
}

impl RouteOverlay {
    pub fn candidates(graph: &TrailGraph, routes: &[Route], identities: &[usize]) -> Self {
        assert_eq!(routes.len(), identities.len());
        let mut edges = candidate_chains(graph, routes, identities);
        weave_cadence(graph.vertices.len(), &mut edges);
        Self {
            field: TrailField::overlay(&edges),
        }
    }

    pub fn saved(trail: &SavedTrail) -> Self {
        let mut edges = saved_chains(trail);
        weave_cadence(trail.legs.len() + 1, &mut edges);
        Self {
            field: TrailField::overlay(&edges),
        }
    }

    pub fn paint(&mut self, painter: &Painter, frame: MapFramePlan, coloring: TrailColoring) {
        self.field.paint_colored(painter, frame, coloring);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Crown {
    occurrence: usize,
    slot: usize,
}

#[derive(Clone, Copy)]
struct OverlayStyle {
    color: Color32,
    trail_class: TrailClass,
    standing: TrailStanding,
    terrain: Terrain,
    mark: TrailMark,
    access: Access,
}

impl OverlayStyle {
    fn same_visual_law(self, other: Self) -> bool {
        self.mark == other.mark
            && (self.standing == TrailStanding::Informal)
                == (other.standing == TrailStanding::Informal)
            && self.terrain == other.terrain
            && TrailSalience::Selected.access_color(self.color, self.access)
                == TrailSalience::Selected.access_color(other.color, other.access)
    }
}

struct OverlayDraft {
    endpoints: [usize; 2],
    points: Vec<[f64; 2]>,
    style: OverlayStyle,
}

fn candidate_crown(routes: &[Route]) -> BTreeMap<EdgeId, Crown> {
    let mut crown = BTreeMap::new();
    let mut occurrence = 0;
    for (slot, route) in routes.iter().enumerate() {
        for edge in &route.edges {
            crown.insert(*edge, Crown { occurrence, slot });
            occurrence += 1;
        }
    }
    crown
}

fn candidate_chains(graph: &TrailGraph, routes: &[Route], identities: &[usize]) -> Vec<WorldEdge> {
    let crown = candidate_crown(routes);
    let mut degree = vec![0_u16; graph.vertices.len()];
    for edge_id in crown.keys() {
        let edge = &graph.edges[edge_id.0];
        degree[edge.a.0] = degree[edge.a.0].saturating_add(1);
        degree[edge.b.0] = degree[edge.b.0].saturating_add(1);
    }
    let mut chains = Vec::new();
    let mut occurrence = 0;
    for (slot, route) in routes.iter().enumerate() {
        let color = candidate_color(identities[slot], false);
        let mut at = route.start;
        let mut draft = None::<OverlayDraft>;
        for edge_id in &route.edges {
            let edge = &graph.edges[edge_id.0];
            let next = edge
                .traverse(at)
                .expect("candidate edge must remain traversable");
            let owner = crown[edge_id];
            if owner.slot == slot && owner.occurrence == occurrence {
                let style = OverlayStyle {
                    color,
                    trail_class: edge.attr.trail_class,
                    standing: edge.attr.standing,
                    terrain: edge.attr.terrain,
                    mark: trail_mark(
                        edge.attr.trail_class,
                        edge.attr.standing,
                        edge.attr.marking,
                        edge.attr.terrain,
                        edge.attr.surface.as_deref(),
                    ),
                    access: edge.attr.access,
                };
                let points = edge
                    .oriented_geometry(at)
                    .points
                    .iter()
                    .copied()
                    .map(world_from_coord)
                    .collect::<Vec<_>>();
                if let Some(run) = &mut draft
                    && run.endpoints[1] == at.0
                    && degree[at.0] == 2
                    && run.style.same_visual_law(style)
                {
                    run.points.extend(points.into_iter().skip(1));
                    run.endpoints[1] = next.0;
                } else {
                    seal_overlay(&mut draft, &mut chains);
                    draft = Some(OverlayDraft {
                        endpoints: [at.0, next.0],
                        points,
                        style,
                    });
                }
            } else {
                seal_overlay(&mut draft, &mut chains);
            }
            at = next;
            occurrence += 1;
        }
        seal_overlay(&mut draft, &mut chains);
    }
    chains
}

fn saved_chains(trail: &SavedTrail) -> Vec<WorldEdge> {
    let last = trail.legs.len().saturating_sub(1);
    let mut draft = None::<OverlayDraft>;
    let mut chains = Vec::new();
    for (slot, leg) in trail.legs.iter().enumerate() {
        let endpoint = if slot == last && trail.metrics.shape == RouteShape::Loop {
            0
        } else {
            slot + 1
        };
        let style = OverlayStyle {
            color: SELECTED_TRAIL_COLOR,
            trail_class: leg.trail_class,
            standing: leg.standing,
            terrain: leg.terrain,
            mark: trail_mark(
                leg.trail_class,
                leg.standing,
                leg.marking,
                leg.terrain,
                leg.surface.as_deref(),
            ),
            access: leg.access,
        };
        let points = leg
            .geometry
            .points
            .iter()
            .copied()
            .map(world_from_coord)
            .collect::<Vec<_>>();
        if let Some(run) = &mut draft
            && run.endpoints[1] == slot
            && run.style.same_visual_law(style)
            && run
                .points
                .last()
                .zip(points.first())
                .is_some_and(|(left, right)| same_world(*left, *right))
        {
            run.points.extend(points.into_iter().skip(1));
            run.endpoints[1] = endpoint;
        } else {
            seal_overlay(&mut draft, &mut chains);
            draft = Some(OverlayDraft {
                endpoints: [slot, endpoint],
                points,
                style,
            });
        }
    }
    seal_overlay(&mut draft, &mut chains);
    chains
}

fn seal_overlay(draft: &mut Option<OverlayDraft>, chains: &mut Vec<WorldEdge>) {
    let Some(draft) = draft.take() else {
        return;
    };
    chains.push(WorldEdge {
        endpoints: draft.endpoints,
        length_world: world_polyline_length(&draft.points),
        points: draft.points,
        lineage: None,
        color: draft.style.color,
        trail_class: draft.style.trail_class,
        standing: draft.style.standing,
        terrain: draft.style.terrain,
        mark: draft.style.mark,
        access: draft.style.access,
    });
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
    coloring: TrailColoring,
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
        annex_selected_stroke(
            &mut strokes,
            SelectedStroke {
                length_world: world_polyline_length(&world),
                points: world
                    .into_iter()
                    .map(|world| screen_at(view, rect, world))
                    .collect(),
                color: trail_hue(
                    coloring,
                    color,
                    edge.attr.standing,
                    edge.attr.terrain,
                    TrailSalience::Selected,
                    edge.attr.access,
                ),
                mark: trail_mark(
                    edge.attr.trail_class,
                    edge.attr.standing,
                    edge.attr.marking,
                    edge.attr.terrain,
                    edge.attr.surface.as_deref(),
                ),
            },
        );
        at = edge
            .traverse(at)
            .expect("validated route edge must be traversable");
    }
    paint_selected_strokes(painter, &strokes, view);
}

pub fn paint_edict(
    painter: &Painter,
    graph: &TrailGraph,
    edge: EdgeId,
    disposition: EdgeDisposition,
    view: Viewport,
    rect: Rect,
) {
    if disposition == EdgeDisposition::Free {
        return;
    }
    let points = graph.edges[edge.0]
        .geometry
        .points
        .iter()
        .copied()
        .map(world_from_coord)
        .map(|world| screen_at(view, rect, world))
        .collect::<Vec<_>>();
    let Some(anchor) = polyline_midpoint(&points) else {
        return;
    };
    match disposition {
        EdgeDisposition::Required => forge::pin(painter, anchor, false),
        EdgeDisposition::Forbidden => paint_exclusion(painter, anchor),
        EdgeDisposition::Free => unreachable!("free edicts return before projection"),
    }
}

fn paint_exclusion(painter: &Painter, anchor: Pos2) {
    const ARM: f32 = 11.0;
    let slash = [
        [anchor + vec2(-ARM, -ARM), anchor + vec2(ARM, ARM)],
        [anchor + vec2(-ARM, ARM), anchor + vec2(ARM, -ARM)],
    ];
    for stroke in [
        Stroke::new(6.0_f32, Color32::from_black_alpha(205)),
        Stroke::new(3.2_f32, Color32::from_rgb(232, 48, 45)),
    ] {
        for arm in slash {
            painter.line_segment(arm, stroke);
        }
    }
}

fn polyline_midpoint(points: &[Pos2]) -> Option<Pos2> {
    let length = points
        .windows(2)
        .map(|pair| pair[0].distance(pair[1]))
        .sum::<f32>();
    let target = length * 0.5;
    let mut traversed = 0.0;
    for pair in points.windows(2) {
        let span = pair[0].distance(pair[1]);
        if span > f32::EPSILON && traversed + span >= target {
            return Some(pair[0].lerp(pair[1], (target - traversed) / span));
        }
        traversed += span;
    }
    points.first().copied()
}

struct SelectedStroke {
    points: Vec<Pos2>,
    length_world: f64,
    color: Color32,
    mark: TrailMark,
}

fn annex_selected_stroke(strokes: &mut Vec<SelectedStroke>, stroke: SelectedStroke) {
    if let Some(tail) = strokes.last_mut()
        && tail.color == stroke.color
        && tail.mark == stroke.mark
        && tail
            .points
            .last()
            .zip(stroke.points.first())
            .is_some_and(|(left, right)| left.distance(*right) <= 0.01)
    {
        tail.length_world += stroke.length_world;
        tail.points.extend(stroke.points.into_iter().skip(1));
    } else {
        strokes.push(stroke);
    }
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
    for endpoint in [points[0], points[points.len() - 1]] {
        let _cap = painter.circle_filled(endpoint, width * 0.5, color);
    }
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
        for endpoint in [points[0], points[points.len() - 1]] {
            let _cap = painter.circle_filled(endpoint, core.width * 0.5, core.color);
        }
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

pub const fn formality_color(informal: bool, salience: TrailSalience) -> Color32 {
    match (informal, salience) {
        (false, TrailSalience::Context) => Color32::from_rgb(213, 180, 104),
        (true, TrailSalience::Context) => Color32::from_rgb(205, 133, 190),
        (false, TrailSalience::Selected) => Color32::from_rgb(255, 168, 39),
        (true, TrailSalience::Selected) => Color32::from_rgb(247, 62, 181),
    }
}

/// Terrain hues avoid green, which disappears against park fill. Context
/// colors are shared by the map, legend, and elevation ribbon.
pub const fn terrain_color(terrain: Terrain, salience: TrailSalience) -> Color32 {
    match (terrain, salience) {
        (Terrain::Unknown, TrailSalience::Context) => Color32::from_rgb(154, 140, 123),
        (Terrain::Trail, TrailSalience::Context) => Color32::from_rgb(219, 178, 85),
        (Terrain::Forest, TrailSalience::Context) => Color32::from_rgb(150, 110, 169),
        (Terrain::Alpine, TrailSalience::Context) => Color32::from_rgb(104, 165, 195),
        (Terrain::Talus, TrailSalience::Context) => Color32::from_rgb(191, 139, 81),
        (Terrain::Scramble, TrailSalience::Context) => Color32::from_rgb(202, 83, 62),
        (Terrain::Pavement, TrailSalience::Context) => Color32::from_rgb(142, 145, 151),
        (Terrain::Road, TrailSalience::Context) => Color32::from_rgb(158, 112, 73),
        (Terrain::Water, TrailSalience::Context) => Color32::from_rgb(60, 137, 179),
        (Terrain::Unknown, TrailSalience::Selected) => Color32::from_rgb(227, 165, 88),
        (Terrain::Trail, TrailSalience::Selected) => Color32::from_rgb(255, 180, 37),
        (Terrain::Forest, TrailSalience::Selected) => Color32::from_rgb(207, 88, 232),
        (Terrain::Alpine, TrailSalience::Selected) => Color32::from_rgb(67, 174, 234),
        (Terrain::Talus, TrailSalience::Selected) => Color32::from_rgb(242, 126, 31),
        (Terrain::Scramble, TrailSalience::Selected) => Color32::from_rgb(246, 64, 42),
        (Terrain::Pavement, TrailSalience::Selected) => Color32::from_rgb(197, 202, 213),
        (Terrain::Road, TrailSalience::Selected) => Color32::from_rgb(224, 109, 48),
        (Terrain::Water, TrailSalience::Selected) => Color32::from_rgb(31, 143, 224),
    }
}

pub fn trail_hue(
    coloring: TrailColoring,
    class_color: Color32,
    standing: TrailStanding,
    terrain: Terrain,
    salience: TrailSalience,
    access: Access,
) -> Color32 {
    let projected = match coloring {
        TrailColoring::Class => class_color,
        TrailColoring::Formality => formality_color(standing == TrailStanding::Informal, salience),
        TrailColoring::Terrain => terrain_color(terrain, salience),
    };
    salience.access_color(projected, access)
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

fn same_world(left: [f64; 2], right: [f64; 2]) -> bool {
    (left[0] - right[0]).abs() <= 1.0e-12 && (left[1] - right[1]).abs() <= 1.0e-12
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
    use trailgen_core::{ConstraintVerdict, GraphBuilder, RouteMetrics, VertexId, io::geojson};

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
    fn cartographic_settle_does_not_hold_labels_past_one_tenth_second() {
        assert!(CARTOGRAPHIC_SETTLE <= Duration::from_millis(100));
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
    fn edict_mark_anchors_at_arc_length_midpoint() {
        let points = [pos2(0.0, 0.0), pos2(90.0, 0.0), pos2(90.0, 10.0)];

        assert!(
            polyline_midpoint(&points)
                .is_some_and(|midpoint| midpoint.distance(pos2(50.0, 0.0)) < 1.0e-4)
        );
        assert_eq!(polyline_midpoint(&[pos2(7.0, 9.0)]), Some(pos2(7.0, 9.0)));
        assert_eq!(polyline_midpoint(&[]), None);
    }

    #[test]
    fn candidate_crown_keeps_only_the_topmost_copy_of_shared_support() {
        let routes = [route([0, 1]), route([1, 2])];
        let crown = candidate_crown(&routes);

        assert_eq!(
            crown[&EdgeId(0)],
            Crown {
                occurrence: 0,
                slot: 0
            }
        );
        assert_eq!(
            crown[&EdgeId(1)],
            Crown {
                occurrence: 2,
                slot: 1
            }
        );
        assert_eq!(
            crown[&EdgeId(2)],
            Crown {
                occurrence: 3,
                slot: 1
            }
        );
    }

    #[test]
    fn candidate_chains_join_degree_two_supports() {
        let graph = GraphBuilder::default()
            .build(
                &geojson::network_from_str(
                    r#"{
                        "type": "FeatureCollection",
                        "features": [
                            {
                                "type": "Feature",
                                "properties": {
                                    "id": "a",
                                    "source": "fixture",
                                    "terrain": "trail",
                                    "access": "open",
                                    "confidence": 1.0
                                },
                                "geometry": {
                                    "type": "LineString",
                                    "coordinates": [[-74.0, 41.0], [-73.999, 41.0]]
                                }
                            },
                            {
                                "type": "Feature",
                                "properties": {
                                    "id": "b",
                                    "source": "fixture",
                                    "terrain": "trail",
                                    "access": "open",
                                    "confidence": 1.0
                                },
                                "geometry": {
                                    "type": "LineString",
                                    "coordinates": [[-73.999, 41.0], [-73.998, 41.0]]
                                }
                            }
                        ]
                    }"#,
                )
                .expect("fixture network must parse"),
            )
            .expect("fixture graph must build");
        let chains = candidate_chains(&graph, &[route([0, 1])], &[0]);

        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].endpoints, [0, 2]);
        assert_eq!(chains[0].points.len(), 3);

        let mut saved = SavedTrail::capture(&graph, &route([0, 1])).expect("route can be saved");
        saved.metrics.shape = RouteShape::Open;
        let saved = saved_chains(&saved);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].endpoints, [0, 2]);
        assert_eq!(saved[0].points.len(), 3);
    }

    #[test]
    fn privileged_cpu_strokes_fuse_only_across_one_visual_law() {
        let stroke = |points, mark| SelectedStroke {
            points,
            length_world: 1.0,
            color: SELECTED_TRAIL_COLOR,
            mark,
        };
        let mut strokes = Vec::new();
        annex_selected_stroke(
            &mut strokes,
            stroke(vec![pos2(0.0, 0.0), pos2(1.0, 0.0)], TrailMark::Solid),
        );
        annex_selected_stroke(
            &mut strokes,
            stroke(vec![pos2(1.0, 0.0), pos2(2.0, 0.0)], TrailMark::Solid),
        );
        annex_selected_stroke(
            &mut strokes,
            stroke(vec![pos2(2.0, 0.0), pos2(3.0, 0.0)], TrailMark::Dashed),
        );

        assert_eq!(strokes.len(), 2);
        assert_eq!(strokes[0].points.len(), 3);
        assert!((strokes[0].length_world - 2.0).abs() < f64::EPSILON);
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
    fn color_projections_preserve_class_identity_and_access_alarm() {
        let class = trail_class_color(TrailClass::Path);
        let open = |coloring| {
            trail_hue(
                coloring,
                class,
                TrailStanding::Informal,
                Terrain::Talus,
                TrailSalience::Context,
                Access::Open,
            )
        };
        assert_eq!(open(TrailColoring::Class), class);
        assert_eq!(
            open(TrailColoring::Formality),
            formality_color(true, TrailSalience::Context)
        );
        assert_eq!(
            open(TrailColoring::Terrain),
            terrain_color(Terrain::Talus, TrailSalience::Context)
        );

        let blocked = TrailSalience::Selected.access_color(class, Access::Private);
        for coloring in TrailColoring::ALL {
            assert_eq!(
                trail_hue(
                    coloring,
                    class,
                    TrailStanding::Informal,
                    Terrain::Talus,
                    TrailSalience::Selected,
                    Access::Private,
                ),
                blocked,
            );
        }
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
            (0..32)
                .map(|identity| candidate_color(identity, false))
                .all(|color| chroma(color) >= 70)
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
    fn candidate_palette_is_unbounded_perceptual_and_never_green() {
        let colors = (0..512)
            .map(|identity| candidate_color(identity, false))
            .collect::<Vec<_>>();
        let unique = colors
            .iter()
            .map(Color32::to_tuple)
            .collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), colors.len());
        assert!(colors.iter().all(|color| {
            let [red, green, blue, _] = color.to_array();
            !(f32::from(green) > f32::from(red) * 1.15 && f32::from(green) > f32::from(blue) * 1.15)
        }));
        assert_eq!(
            candidate_color(37, true),
            SELECTED_TRAIL_COLOR,
            "selection owns one emphatic identity"
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
            standing: TrailStanding::Established,
            terrain: Terrain::Trail,
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
