use crate::{
    cadence, chrome, forge,
    library::SavedTrail,
    palette::{ColorCycle, CycleLaw, Span, TRAIL_HIGHLIGHT_HUES},
    trail_map::TrailField,
};
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
    Access, Coord, EdgeDisposition, EdgeId, Route, RouteShape, Terrain, TrailMarking,
    TrailStanding, WalkGraph, WayKind, WayRealm,
};

const TILE_EDGE: f64 = 256.0;
const CARTOGRAPHIC_SETTLE: Duration = Duration::from_millis(90);
pub const EARTH_CIRCUMFERENCE_M: f64 = 40_075_016.686;
const FIT_PADDING: f32 = 44.0;

pub const MAP_GROUND_SRGB: [u8; 3] = [196, 194, 176];
pub const MAP_GROUND: Color32 =
    Color32::from_rgb(MAP_GROUND_SRGB[0], MAP_GROUND_SRGB[1], MAP_GROUND_SRGB[2]);
pub const ROAD_SRGB: [u8; 3] = [150, 152, 151];
pub const ROAD_COLOR: Color32 = Color32::from_rgb(ROAD_SRGB[0], ROAD_SRGB[1], ROAD_SRGB[2]);
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
        const LAW: CycleLaw = CycleLaw::new(
            Span::new(0.64, 0.805),
            Span::new(0.17, 0.206),
            &TRAIL_HIGHLIGHT_HUES,
            0.0,
            0.72,
            0.17,
        );
        static PALETTE: OnceLock<Mutex<ColorCycle>> = OnceLock::new();
        PALETTE
            .get_or_init(|| Mutex::new(ColorCycle::new(LAW)))
            .lock()
            .expect("candidate palette lock poisoned")
            .color(ordinal)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrailSalience {
    Context,
    Selected,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TrailClass {
    Trail,
    Walkway,
    Track,
    Road,
    Bushwhack,
}

impl TrailClass {
    pub const fn color(self) -> Color32 {
        match self {
            Self::Trail => Color32::from_rgb(198, 137, 91),
            Self::Walkway => Color32::from_rgb(176, 151, 198),
            Self::Track => Color32::from_rgb(181, 122, 104),
            Self::Road => ROAD_COLOR,
            Self::Bushwhack => Color32::from_rgb(205, 145, 173),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Trail => "TRAIL",
            Self::Walkway => "WALKWAY",
            Self::Track => "TRACK",
            Self::Road => "ROAD",
            Self::Bushwhack => "BUSHWHACK",
        }
    }
}

pub fn trail_class(kind: WayKind, realm: WayRealm) -> Option<TrailClass> {
    match kind {
        WayKind::Sidewalk | WayKind::Crossing => None,
        WayKind::Bushwhack => Some(TrailClass::Bushwhack),
        WayKind::Track => Some(TrailClass::Track),
        WayKind::ServiceRoad | WayKind::Roadway => Some(TrailClass::Road),
        WayKind::PedestrianStreet | WayKind::Cycleway => Some(TrailClass::Walkway),
        WayKind::Footway | WayKind::Steps if realm != WayRealm::Recreational => {
            Some(TrailClass::Walkway)
        }
        WayKind::Unknown if realm == WayRealm::Urban => Some(TrailClass::Walkway),
        WayKind::Unknown
        | WayKind::Path
        | WayKind::Footway
        | WayKind::Steps
        | WayKind::Bridleway => Some(TrailClass::Trail),
    }
}

fn trail_color(kind: WayKind, realm: WayRealm) -> Color32 {
    match kind {
        WayKind::Sidewalk => Color32::from_rgb(166, 170, 171),
        WayKind::Crossing => Color32::from_rgb(218, 111, 151),
        _ => trail_class(kind, realm).map_or_else(|| unreachable!(), TrailClass::color),
    }
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
    pub const MAX_ZOOM: f64 = 18.5;
    pub const WORLD: Self = Self {
        center: [0.5, 0.5],
        zoom: 2.0,
    };

    pub fn normalize(&mut self) {
        self.center[0] = self.center[0].rem_euclid(1.0);
        self.center[1] = self.center[1].clamp(0.0, 1.0);
        self.zoom = self.zoom.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
    }

    pub fn preserve_visible_extent(mut self, prior: Rect, next: Rect) -> Self {
        if !prior.is_positive() || !next.is_positive() {
            return self;
        }
        let contraction = (next.width() / prior.width())
            .min(next.height() / prior.height())
            .min(1.0);
        if contraction.is_normal() {
            self.zoom =
                (self.zoom + f64::from(contraction).log2()).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        }
        self
    }

    pub fn fit_graph(graph: &WalkGraph, rect: Rect) -> Self {
        fit_coords(graph.vertices.iter().map(|vertex| vertex.coord), rect)
    }

    pub fn fit_route(graph: &WalkGraph, route: &Route, rect: Rect) -> Self {
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
    sidewalks: TrailField,
    crossing_diagnostics: TrailField,
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
    pub class: Option<TrailClass>,
    pub stepped: bool,
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
    pub fn forge(graph: &WalkGraph) -> Self {
        let mut edges = Vec::new();
        let mut sidewalks = Vec::new();
        let mut crossing_diagnostics = Vec::new();
        let mut classes = BTreeSet::new();
        for edge in &graph.edges {
            let stratum = context_stratum(edge.attr.way_kind);
            let points = edge
                .geometry
                .points
                .iter()
                .copied()
                .map(world_from_coord)
                .collect::<Vec<_>>();
            let class = trail_class(edge.attr.way_kind, edge.attr.realm);
            if stratum == ContextStratum::Colored {
                classes.extend(class);
            }
            let world = WorldEdge {
                endpoints: [edge.a.0, edge.b.0],
                length_world: world_polyline_length(&points),
                points,
                lineage: None,
                color: trail_color(edge.attr.way_kind, edge.attr.realm),
                class,
                stepped: edge.attr.way_kind == WayKind::Steps,
                standing: edge.attr.standing,
                terrain: edge.attr.terrain,
                mark: trail_mark(
                    edge.attr.way_kind,
                    edge.attr.standing,
                    edge.attr.marking,
                    edge.attr.terrain,
                    edge.attr.surface.as_deref(),
                ),
                access: edge.attr.access,
            };
            match stratum {
                ContextStratum::Colored => edges.push(world),
                ContextStratum::Sidewalk => sidewalks.push(world),
                ContextStratum::CrossingDiagnostic => crossing_diagnostics.push(world),
            }
        }
        weave_cadence(graph.vertices.len(), &mut edges);
        weave_cadence(graph.vertices.len(), &mut crossing_diagnostics);
        let classes = classes.into_iter().collect();
        let terrains = edges
            .iter()
            .map(|edge| edge.terrain)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let field = TrailField::forge(&edges);
        let sidewalks = TrailField::sidewalks(&sidewalks);
        let crossing_diagnostics = TrailField::crossing_diagnostics(&crossing_diagnostics);
        Self {
            classes,
            terrains,
            field,
            sidewalks,
            crossing_diagnostics,
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
                        match current {
                            TrailColoring::Class => {
                                for class in self.classes.iter().copied() {
                                    legend_row(ui, class.label(), class.color(), None, false);
                                }
                            }
                            TrailColoring::Formality => {
                                legend_row(
                                    ui,
                                    "FORMAL",
                                    formality_color(false, TrailSalience::Context),
                                    None,
                                    false,
                                );
                                legend_row(
                                    ui,
                                    "INFORMAL",
                                    formality_color(true, TrailSalience::Context),
                                    None,
                                    false,
                                );
                            }
                            TrailColoring::Terrain => {
                                for terrain in self.terrains.iter().copied() {
                                    legend_row(
                                        ui,
                                        terrain_label(terrain),
                                        terrain_color(terrain, TrailSalience::Context),
                                        None,
                                        false,
                                    );
                                }
                            }
                        }
                        ui.add_space(4.0);
                        let _heading = ui.label(chrome::eyebrow("SURFACE / WAYFINDING"));
                        for mark in TrailMark::ALL {
                            legend_row(
                                ui,
                                mark.label(),
                                TrailClass::Trail.color(),
                                Some(mark),
                                false,
                            );
                        }
                        legend_row(
                            ui,
                            "STEPS",
                            TrailClass::Trail.color(),
                            Some(TrailMark::Solid),
                            true,
                        );
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
        self.sidewalks.paint_colored(painter, frame, coloring);
        self.crossing_diagnostics
            .paint_colored(painter, frame, coloring);
        self.field.paint_colored(painter, frame, coloring);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContextStratum {
    Colored,
    Sidewalk,
    CrossingDiagnostic,
}

const fn context_stratum(kind: WayKind) -> ContextStratum {
    match kind {
        WayKind::Sidewalk => ContextStratum::Sidewalk,
        WayKind::Crossing => ContextStratum::CrossingDiagnostic,
        _ => ContextStratum::Colored,
    }
}

#[derive(Default)]
pub struct LegendResponse {
    pub rect: Option<Rect>,
    pub tabs: Vec<(TrailColoring, Rect)>,
    pub clicked: Option<(TrailColoring, Rect)>,
}

fn legend_row(
    ui: &mut egui::Ui,
    label: &str,
    color: Color32,
    mark: Option<TrailMark>,
    stepped: bool,
) {
    let (rect, _response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 21.0), egui::Sense::hover());
    let from = pos2(rect.left() + 2.0, rect.center().y);
    let to = pos2(rect.left() + 24.0, rect.center().y);
    if let Some(mark) = mark {
        paint_trail_tube_style(
            ui.painter(),
            &[from, to],
            TrailSalience::Context.width(),
            color,
            mark,
            stepped,
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
    pub fn candidates(graph: &WalkGraph, routes: &[Route], identities: &[usize]) -> Self {
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

    pub fn paint_hued(
        &mut self,
        painter: &Painter,
        frame: MapFramePlan,
        hue: Color32,
        opacity: f32,
    ) {
        self.field.paint_hued(painter, frame, hue, opacity);
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
    stepped: bool,
    standing: TrailStanding,
    terrain: Terrain,
    mark: TrailMark,
    access: Access,
}

impl OverlayStyle {
    fn same_visual_law(self, other: Self) -> bool {
        self.mark == other.mark
            && self.stepped == other.stepped
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

fn candidate_chains(graph: &WalkGraph, routes: &[Route], identities: &[usize]) -> Vec<WorldEdge> {
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
                    stepped: edge.attr.way_kind == WayKind::Steps,
                    standing: edge.attr.standing,
                    terrain: edge.attr.terrain,
                    mark: trail_mark(
                        edge.attr.way_kind,
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
            stepped: leg.way_kind == WayKind::Steps,
            standing: leg.standing,
            terrain: leg.terrain,
            mark: trail_mark(
                leg.way_kind,
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
        class: None,
        stepped: draft.style.stepped,
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
        .filter(|(_, edge)| edge.mark.patterned() || edge.stepped)
    {
        for endpoint in edge.endpoints {
            adjacency[endpoint].push(edge_id);
        }
    }

    for mark in TrailMark::ALL {
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
    graph: &WalkGraph,
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
                    edge.attr.way_kind,
                    edge.attr.standing,
                    edge.attr.marking,
                    edge.attr.terrain,
                    edge.attr.surface.as_deref(),
                ),
                stepped: edge.attr.way_kind == WayKind::Steps,
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
    graph: &WalkGraph,
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
        EdgeDisposition::Required => forge::reticle(painter, anchor),
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
    stepped: bool,
}

fn annex_selected_stroke(strokes: &mut Vec<SelectedStroke>, stroke: SelectedStroke) {
    if let Some(tail) = strokes.last_mut()
        && tail.color == stroke.color
        && tail.mark == stroke.mark
        && tail.stepped == stroke.stepped
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
    let core_width = trail_core_width(width);
    let scale = world_pixels(view);
    let lattice = cadence::WorldLevel::at_zoom(view.zoom);
    let cells_per_world = lattice.cells_per_world();
    let cell_points = (scale / cells_per_world) as f32;
    let mut datum_world = 0.0;
    for stroke in strokes {
        let pattern = trail_lattice_pattern(stroke.mark, core_width, cell_points);
        let phase = (datum_world * cells_per_world).rem_euclid(2.0) as f32 * cell_points;
        let _length = paint_trail_tube_pattern(
            painter,
            &stroke.points,
            width,
            TubeStyle {
                color: stroke.color,
                mark: stroke.mark,
                step_spacing: stroke.stepped.then_some(cell_points),
            },
            pattern,
            phase,
        );
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

fn paint_trail_tube_style(
    painter: &Painter,
    points: &[Pos2],
    width: f32,
    color: Color32,
    mark: TrailMark,
    stepped: bool,
) {
    let _length = paint_trail_tube_pattern(
        painter,
        points,
        width,
        TubeStyle {
            color,
            mark,
            step_spacing: stepped.then_some(width * 1.35),
        },
        trail_pattern(mark, width, trail_core_width(width)),
        0.0,
    );
}

pub fn paint_trail_tube_at(
    painter: &Painter,
    points: &[Pos2],
    width: f32,
    color: Color32,
    mark: TrailMark,
    datum: f32,
    stepped: bool,
) -> f32 {
    let core_width = trail_core_width(width);
    paint_trail_tube_pattern(
        painter,
        points,
        width,
        TubeStyle {
            color,
            mark,
            step_spacing: stepped.then_some(width * 1.35),
        },
        trail_pattern(mark, width, core_width),
        datum,
    )
}

#[derive(Clone, Copy)]
struct TubeStyle {
    color: Color32,
    mark: TrailMark,
    step_spacing: Option<f32>,
}

fn paint_trail_tube_pattern(
    painter: &Painter,
    points: &[Pos2],
    width: f32,
    style: TubeStyle,
    pattern: Option<cadence::Pattern>,
    datum: f32,
) -> f32 {
    if points.len() < 2 {
        return 0.0;
    }
    let length = cadence::polyline_length(points);
    let _tube = painter.add(Shape::line(
        points.to_vec(),
        Stroke::new(width, style.color),
    ));
    for endpoint in [points[0], points[points.len() - 1]] {
        let _cap = painter.circle_filled(endpoint, width * 0.5, style.color);
    }
    if let Some(pattern) = pattern {
        let mut shapes = Vec::new();
        pattern.tessellate(
            points.iter().copied(),
            trail_core(style.mark, width).expect("patterned trails own a core"),
            datum,
            f32::INFINITY,
            &mut shapes,
        );
        painter.extend(shapes);
    }
    if let Some(step_spacing) = style.step_spacing {
        let mut hatches = Vec::new();
        cadence::crossbars(
            points,
            Stroke::new(1.15_f32, Color32::from_black_alpha(205)),
            width * 0.92,
            step_spacing,
            datum,
            &mut hatches,
        );
        painter.extend(hatches);
    }
    length
}

pub fn trail_core_width(width: f32) -> f32 {
    (width * 0.30).max(1.2)
}

fn trail_core(mark: TrailMark, width: f32) -> Option<Stroke> {
    mark.patterned().then(|| {
        Stroke::new(
            trail_core_width(width),
            Color32::from_rgb(20, 19, 17).gamma_multiply(0.5),
        )
    })
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
    class: WayKind,
    standing: TrailStanding,
    marking: TrailMarking,
    terrain: Terrain,
    surface: Option<&str>,
) -> TrailMark {
    if class == WayKind::Steps {
        return TrailMark::Solid;
    }
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
        WayKind::Unknown | WayKind::Path | WayKind::Steps | WayKind::Bridleway
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
    chrome::ForgePin::new(anchor)
        .size(chrome::MechanismSize::Medium)
        .paint(painter, seized);
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
    fn mercator_round_trip_is_tight() {
        let coord = Coord::new(-74.102, 41.221);
        let round_trip = world_to_coord(world_from_coord(coord));
        assert!((round_trip.lon - coord.lon).abs() < 1.0e-10);
        assert!((round_trip.lat - coord.lat).abs() < 1.0e-10);
    }

    #[test]
    fn panel_contraction_preserves_the_entire_prior_geographic_extent() {
        let viewport = Viewport {
            center: [0.5, 0.5],
            zoom: 12.0,
        };
        let prior = Rect::from_min_size(Pos2::ZERO, vec2(1_000.0, 800.0));
        let contracted = Rect::from_min_size(Pos2::ZERO, vec2(1_000.0, 600.0));
        let preserved = viewport.preserve_visible_extent(prior, contracted);

        assert!((world_pixels(preserved) / world_pixels(viewport) - 0.75).abs() < 1.0e-12);
        assert_eq!(
            preserved.preserve_visible_extent(contracted, prior),
            preserved,
            "panel removal must reveal more map rather than crop the retained extent"
        );
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
}
