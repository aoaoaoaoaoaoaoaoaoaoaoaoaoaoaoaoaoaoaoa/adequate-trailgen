use crate::{
    gallery::{self, CandidateSort},
    map::{self, ALLTRAILS_GREEN, Atlas, CANDIDATE_COLORS, Viewport},
    profile::ElevationProfile,
    project::{Project, SearchEvent, SearchForge, SearchRequest},
    tile::{self, Basemap, TileKey},
};
use anyhow::Result;
use dwemer_poolrooms::{
    chrome,
    water::{Frame as WaterFrame, WaterTable, Wetness},
};
use egui::{Color32, RichText, Stroke, TextureHandle, TextureOptions, pos2, vec2};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
    time::Duration,
};
use trailgen_core::{
    Coord, Edge, Route, RouteShape, SearchParams, SolverKind, Terrain, TrailGraph, VertexId,
};

const TILE_CAPACITY: usize = 512;
const PROFILE_HEIGHT: f32 = 178.0;
const GALLERY_HEIGHT: f32 = 190.0;
const TOOLBAR_HEIGHT: f32 = 38.0;
const TERRAIN_ALL: [Terrain; 9] = [
    Terrain::Unknown,
    Terrain::Trail,
    Terrain::Forest,
    Terrain::Alpine,
    Terrain::Talus,
    Terrain::Scramble,
    Terrain::Pavement,
    Terrain::Road,
    Terrain::Water,
];
const SHAPES: [(RouteShape, &str); 4] = [
    (RouteShape::Loop, "LOOP"),
    (RouteShape::FigureEight, "FIGURE 8"),
    (RouteShape::OutAndBack, "OUT + BACK"),
    (RouteShape::Open, "OPEN"),
];

pub struct TrailApp {
    root: std::path::PathBuf,
    name: String,
    graph: Arc<TrailGraph>,
    atlas: Atlas,
    forge: SearchForge,
    constraints: trailgen_core::LoopConstraints,
    params: SearchParams,
    solver: SolverKind,
    count: usize,
    start: VertexId,
    requested_start: Coord,
    routes: Vec<Route>,
    profiles: Vec<Option<ElevationProfile>>,
    selected: Option<usize>,
    sort: CandidateSort,
    view: ViewMode,
    viewport: Viewport,
    fit: Fit,
    serial: u64,
    forge_phase: ForgePhase,
    basemap: Option<Basemap>,
    tiles: TileBank,
    tile_inflight: HashSet<TileKey>,
    tile_faults: HashSet<TileKey>,
    layers: Layers,
    water: WaterTable,
    status: String,
    basemap_status: String,
    map_rect: egui::Rect,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Fit {
    #[default]
    Graph,
    Route(usize),
    None,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ViewMode {
    #[default]
    Atlas,
    Focus,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ForgePhase {
    #[default]
    Idle,
    Striking,
}

#[derive(Clone, Copy, Debug)]
struct Layers {
    topo: bool,
    network: bool,
    terrain: bool,
}

impl TrailApp {
    pub fn open(ctx: &egui::Context, root: &Path, offline: bool) -> Result<Self> {
        let Project {
            root,
            graph,
            routes,
            config,
            start,
            requested_start,
        } = Project::open(root)?;
        let forge = SearchForge::spawn(ctx.clone(), Arc::clone(&graph))?;
        let basemap = (!offline).then(|| Basemap::spawn(ctx)).transpose()?;
        let selected = (!routes.is_empty()).then_some(0);
        let count = routes.len().clamp(6, 12);
        let profiles = profiles(&graph, &routes);
        let atlas = Atlas::forge(&graph);
        let water = forge_water();
        let status = if routes.is_empty() {
            "choose a trailhead, tune the bounds, and strike FIND TRAILS".to_owned()
        } else {
            format!("loaded {} measured candidate(s)", routes.len())
        };
        Ok(Self {
            root,
            name: config.name,
            graph,
            atlas,
            forge,
            constraints: config.constraints,
            params: config.search,
            solver: config.solver,
            count,
            start,
            requested_start,
            routes,
            profiles,
            selected,
            sort: CandidateSort::default(),
            view: ViewMode::Atlas,
            viewport: Viewport {
                center: map::world_from_coord(requested_start),
                zoom: 13.0,
            },
            fit: Fit::Graph,
            serial: 0,
            forge_phase: ForgePhase::Idle,
            basemap,
            tiles: TileBank::new(TILE_CAPACITY),
            tile_inflight: HashSet::new(),
            tile_faults: HashSet::new(),
            layers: Layers {
                topo: !offline,
                network: true,
                terrain: true,
            },
            water,
            status,
            basemap_status: if offline {
                "TOPOGRAPHY OFFLINE".to_owned()
            } else {
                "USGS NATIONAL MAP".to_owned()
            },
            map_rect: egui::Rect::ZERO,
        })
    }

    pub fn pulse(&mut self, ui: &mut egui::Ui) {
        self.absorb_events(ui.ctx());
        self.take_keys(ui.ctx());
        let _left = egui::Panel::left("trail-inspector")
            .resizable(false)
            .exact_size(chrome::INSPECTOR_WIDTH)
            .show_inside(ui, |ui| {
                let scroll = egui::ScrollArea::vertical()
                    .id_salt("trail-inspector-scroll")
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(ui.spacing().item_spacing.x);
                        self.inspector(ui);
                    });
                self.water.heave(ui.ctx(), scroll.state.offset.y);
            });
        let _center = egui::CentralPanel::default().show_inside(ui, |ui| self.arena(ui));
    }

    pub fn water_frame(
        &mut self,
        ctx: &egui::Context,
        pixels_per_point: f32,
        tooltip_rects: &[egui::Rect],
    ) -> WaterFrame {
        self.water.frame(ctx, pixels_per_point, tooltip_rects, None)
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        let _project = ui.label(chrome::eyebrow("TRAIL FORGE"));
        let _name = ui.label(chrome::title(self.name.to_ascii_uppercase()));
        ui.add_space(3.0);

        self.section(ui, "strike", "find trails", true, Self::strike_panel);
        self.section(ui, "trailhead", "trailhead", true, Self::trailhead_panel);
        self.section(ui, "bounds", "route bounds", true, Self::bounds_panel);
        self.section(ui, "terrain", "terrain law", false, Self::terrain_panel);
        self.section(ui, "engine", "search engine", false, Self::engine_panel);
        self.section(ui, "layers", "map layers", false, Self::layers_panel);
        if self.selected_route().is_some() {
            self.section(ui, "active", "active trail", true, Self::active_panel);
        }
        self.section(ui, "status", "status", true, Self::status_panel);
    }

    fn section(
        &mut self,
        ui: &mut egui::Ui,
        id: &'static str,
        title: &'static str,
        open: bool,
        body: fn(&mut Self, &mut egui::Ui),
    ) {
        let wake = chrome::section(ui, id, title, open, |ui| body(self, ui));
        self.water.fold(wake);
    }

    fn strike_panel(&mut self, ui: &mut egui::Ui) {
        let striking = self.forge_phase == ForgePhase::Striking;
        let label = if striking {
            "⌁  FORGING…"
        } else {
            "⌖  FIND TRAILS"
        };
        let response = ui.add_enabled(
            !striking,
            chrome::glyph_button(label, striking).min_size(vec2(ui.available_width(), 34.0)),
        );
        chrome::tension(ui, &response);
        if response.hovered() {
            self.water.hover("find-trails", response.rect);
        }
        if response.clicked() {
            self.water.thwack(response.rect, 0.7);
            self.strike();
        }
        ui.add_space(3.0);
        let snapped = self.graph.vertices[self.start.0].coord;
        let _summary = ui.label(chrome::muted(format!(
            "V{} · {:.5}, {:.5}\n{:.1}–{:.1} km · {} candidate(s)",
            self.start.0,
            snapped.lon,
            snapped.lat,
            self.constraints.min_distance_m / 1_000.0,
            self.constraints.max_distance_m / 1_000.0,
            self.count
        )));
    }

    fn trailhead_panel(&mut self, ui: &mut egui::Ui) {
        let mut lon = self.requested_start.lon;
        let mut lat = self.requested_start.lat;
        let lon_response = scalar_row(ui, "LONGITUDE", &mut lon, -180.0..=180.0, 0.000_1);
        let lat_response = scalar_row(ui, "LATITUDE", &mut lat, -85.0..=85.0, 0.000_1);
        if lon_response.changed() || lat_response.changed() {
            self.requested_start = Coord::new(lon, lat);
        }
        let snap = ui.add_sized(
            [ui.available_width(), 24.0],
            chrome::glyph_button("⌖  SNAP TO NETWORK", false),
        );
        chrome::tension(ui, &snap);
        if snap.clicked() {
            self.snap_start(self.requested_start, snap.rect);
        }
        let fit = ui.add_sized(
            [ui.available_width(), 24.0],
            chrome::glyph_button("□  FIT NETWORK", false),
        );
        chrome::tension(ui, &fit);
        if fit.clicked() {
            self.fit = Fit::Graph;
            self.water.click(fit.rect);
        }
        let _note = chrome::note(ui, "click the map to strike a trailhead; drag to pan");
    }

    fn bounds_panel(&mut self, ui: &mut egui::Ui) {
        let mut min_km = self.constraints.min_distance_m / 1_000.0;
        let mut max_km = self.constraints.max_distance_m / 1_000.0;
        if range_row(ui, "DISTANCE · KM", &mut min_km, &mut max_km, 0.1).changed() {
            self.constraints.min_distance_m = min_km * 1_000.0;
            self.constraints.max_distance_m = max_km * 1_000.0;
        }
        let mut min_ascent = self.constraints.min_ascent_m;
        let mut max_ascent = self.constraints.max_ascent_m;
        if range_row(ui, "ASCENT · M", &mut min_ascent, &mut max_ascent, 10.0).changed() {
            self.constraints.min_ascent_m = min_ascent;
            self.constraints.max_ascent_m = max_ascent;
        }
        let mut min_descent = self.constraints.min_descent_m;
        let mut max_descent = self.constraints.max_descent_m;
        if range_row(ui, "DESCENT · M", &mut min_descent, &mut max_descent, 10.0).changed() {
            self.constraints.min_descent_m = min_descent;
            self.constraints.max_descent_m = max_descent;
        }
        let mut min_difficulty = self.constraints.min_difficulty;
        let mut max_difficulty = self.constraints.max_difficulty;
        if range_row(
            ui,
            "DIFFICULTY",
            &mut min_difficulty,
            &mut max_difficulty,
            0.5,
        )
        .changed()
        {
            self.constraints.min_difficulty = min_difficulty;
            self.constraints.max_difficulty = max_difficulty;
        }
        ui.add_space(3.0);
        let _shape = ui.label(chrome::eyebrow("SHAPE"));
        let mut changed = false;
        let _chips = ui.horizontal_wrapped(|ui| {
            for (shape, label) in SHAPES {
                let allowed = self.constraints.allowed_shapes.contains(&shape);
                let response = chrome::glyph(ui, label, allowed);
                if response.clicked() {
                    if allowed {
                        self.constraints
                            .allowed_shapes
                            .retain(|item| *item != shape);
                    } else {
                        self.constraints.allowed_shapes.push(shape);
                    }
                    changed = true;
                }
            }
        });
        if changed {
            self.water.bump(ui.min_rect());
        }
        ui.add_space(3.0);
        fraction_row(
            ui,
            "ROAD / PAVEMENT",
            &mut self.constraints.max_road_fraction,
        );
        fraction_row(
            ui,
            "LOW CONFIDENCE",
            &mut self.constraints.max_low_confidence_fraction,
        );
        fraction_row(
            ui,
            "RESTRICTED",
            &mut self.constraints.max_restricted_access_fraction,
        );
        fraction_row(
            ui,
            "REPEATED EDGE",
            &mut self.constraints.max_repeated_edge_fraction,
        );
    }

    fn terrain_panel(&mut self, ui: &mut egui::Ui) {
        let _note = chrome::note(ui, "lit terrain is admissible; dark terrain is forbidden");
        let mut toggled = None;
        let _chips = ui.horizontal_wrapped(|ui| {
            for terrain in TERRAIN_ALL {
                let allowed = !self.constraints.forbidden_terrain.contains(&terrain);
                let response = chrome::glyph(ui, map::terrain_label(terrain), allowed);
                if response.clicked() {
                    toggled = Some((terrain, response.rect));
                }
            }
        });
        if let Some((terrain, rect)) = toggled {
            if self.constraints.forbidden_terrain.contains(&terrain) {
                self.constraints
                    .forbidden_terrain
                    .retain(|item| *item != terrain);
            } else {
                self.constraints.forbidden_terrain.push(terrain);
                self.constraints.forbidden_terrain.sort();
            }
            self.water.select(rect);
        }
        ui.add_space(5.0);
        let _mix = ui.label(chrome::eyebrow("DISTANCE MIX · MIN / MAX"));
        for terrain in TERRAIN_ALL {
            let mut minimum = self
                .constraints
                .min_terrain_fraction
                .get(&terrain)
                .copied()
                .unwrap_or(0.0);
            let mut maximum = self
                .constraints
                .max_terrain_fraction
                .get(&terrain)
                .copied()
                .unwrap_or(1.0);
            if terrain_range_row(ui, terrain, &mut minimum, &mut maximum).changed() {
                replace_terrain_bound(
                    &mut self.constraints.min_terrain_fraction,
                    terrain,
                    minimum,
                    0.0,
                );
                replace_terrain_bound(
                    &mut self.constraints.max_terrain_fraction,
                    terrain,
                    maximum,
                    1.0,
                );
            }
        }
    }

    fn engine_panel(&mut self, ui: &mut egui::Ui) {
        let _solver = ui.horizontal_wrapped(|ui| {
            let _label = ui.label(chrome::eyebrow("SOLVER"));
            for solver in [SolverKind::Auto, SolverKind::Heuristic, SolverKind::Exact] {
                let response = chrome::glyph(
                    ui,
                    solver.label().to_ascii_uppercase(),
                    self.solver == solver,
                );
                if response.clicked() && self.solver != solver {
                    self.solver = solver;
                    self.water.select(response.rect);
                }
            }
        });
        usize_row(ui, "CANDIDATES", &mut self.count, 1..=32);
        usize_row(ui, "MAX HOPS", &mut self.params.max_hops, 2..=512);
        usize_row(
            ui,
            "FRONTIER",
            &mut self.params.max_frontier,
            1_000..=5_000_000,
        );
        usize_row(ui, "KEEP", &mut self.params.keep, 1..=256);
        usize_row(ui, "CLOSURES", &mut self.params.closure_paths, 1..=32);
        let _seed = scalar_row_u64(ui, "SEED", &mut self.params.seed);
    }

    fn layers_panel(&mut self, ui: &mut egui::Ui) {
        layer_toggle(ui, &mut self.layers.topo, "TOPOGRAPHIC BASE");
        layer_toggle(ui, &mut self.layers.network, "TRAIL NETWORK");
        layer_toggle(ui, &mut self.layers.terrain, "TERRAIN CENTERLINE");
        ui.add_space(4.0);
        for terrain in TERRAIN_ALL {
            let _row = ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
                let _swatch = ui
                    .painter()
                    .rect_filled(dot, 0.0, map::terrain_color(terrain));
                let _label = ui.label(chrome::muted(map::terrain_label(terrain)));
            });
        }
    }

    fn active_panel(&mut self, ui: &mut egui::Ui) {
        let Some(route) = self.selected_route() else {
            return;
        };
        let metrics = &route.metrics;
        let _name = ui.label(chrome::section_title(route.name.to_ascii_uppercase()));
        metric_pair(
            ui,
            "DISTANCE",
            format!("{:.2} km", metrics.distance_m / 1_000.0),
        );
        metric_pair(ui, "ASCENT", format!("{:.0} m", metrics.ascent_m));
        metric_pair(ui, "DESCENT", format!("{:.0} m", metrics.descent_m));
        metric_pair(ui, "DIFFICULTY", format!("{:.2}", metrics.difficulty));
        metric_pair(
            ui,
            "SHAPE",
            format!("{:?}", metrics.shape).to_ascii_uppercase(),
        );
        metric_pair(ui, "PARETO", format!("RANK {}", route.pareto_rank));
        ui.add_space(4.0);
        let _terrain = ui.label(chrome::eyebrow("TERRAIN BY DISTANCE"));
        for (terrain, meters) in &metrics.terrain_m {
            let fraction = *meters / metrics.distance_m.max(1.0);
            distribution_bar(
                ui,
                map::terrain_label(*terrain),
                fraction,
                map::terrain_color(*terrain),
            );
        }
        ui.add_space(4.0);
        let _grade = ui.label(chrome::eyebrow("GRADE BY DISTANCE"));
        let grade = metrics.grade_distribution;
        for (label, meters, color) in [
            ("FLAT < 5%", grade.flat_m, ALLTRAILS_GREEN),
            (
                "ROLLING 5–15%",
                grade.rolling_m,
                Color32::from_rgb(211, 178, 78),
            ),
            (
                "STEEP 15–30%",
                grade.steep_m,
                Color32::from_rgb(218, 124, 65),
            ),
            (
                "SAVAGE > 30%",
                grade.savage_m,
                Color32::from_rgb(205, 73, 58),
            ),
        ] {
            distribution_bar(ui, label, meters / metrics.distance_m.max(1.0), color);
        }
        ui.add_space(4.0);
        metric_pair(ui, "ROAD", percent(metrics.road_fraction));
        metric_pair(ui, "LOW CONF", percent(metrics.low_confidence_fraction));
        metric_pair(
            ui,
            "RESTRICTED",
            percent(metrics.restricted_access_fraction),
        );
        metric_pair(ui, "REPEATED", percent(metrics.repeated_edge_fraction));
        let crossings = metrics.crossings.values().sum::<u32>();
        metric_pair(ui, "CROSSINGS", crossings.to_string());
        if route.verdict.satisfied {
            let _fit = ui.label(RichText::new("✓ ALL BOUNDS SATISFIED").color(ALLTRAILS_GREEN));
        } else {
            ui.add_space(3.0);
            for violation in &route.verdict.violations {
                let _violation = chrome::note(ui, format!("× {violation}"));
            }
        }
        ui.add_space(5.0);
        let _audit = ui.label(chrome::eyebrow("BOUND AUDIT"));
        for check in &route.verdict.audit {
            let mark = if check.satisfied { "✓" } else { "×" };
            let color = if check.satisfied {
                chrome::MUTED
            } else {
                Color32::from_rgb(208, 116, 72)
            };
            let response = ui.label(
                RichText::new(format!(
                    "{mark} {} · {} · {}",
                    check.metric.to_ascii_uppercase(),
                    check.measured,
                    check.margin
                ))
                .monospace()
                .small()
                .color(color),
            );
            let _tooltip = response.on_hover_text(format!("REQUIREMENT · {}", check.requirement));
        }
    }

    fn status_panel(&mut self, ui: &mut egui::Ui) {
        let _status = ui.label(chrome::muted(&self.status));
        ui.add_space(3.0);
        for line in [
            format!(
                "GRAPH · {} V / {} E",
                self.graph.vertices.len(),
                self.graph.edges.len()
            ),
            format!("BASE · {}", self.basemap_status),
            format!("PROJECT · {}", self.root.display()),
        ] {
            let _line = chrome::note(ui, line);
        }
    }

    fn arena(&mut self, ui: &mut egui::Ui) {
        let _toolbar = egui::Panel::top("trail-toolbar")
            .exact_size(TOOLBAR_HEIGHT)
            .show_inside(ui, |ui| self.toolbar(ui));
        if self.view == ViewMode::Focus {
            if self
                .selected
                .is_some_and(|slot| self.profiles.get(slot).is_some_and(Option::is_some))
            {
                let _profile = egui::Panel::bottom("trail-profile")
                    .exact_size(PROFILE_HEIGHT)
                    .show_inside(ui, |ui| self.profile(ui));
            }
        } else {
            let _gallery = egui::Panel::bottom("candidate-gallery")
                .exact_size(GALLERY_HEIGHT)
                .show_inside(ui, |ui| self.gallery(ui));
        }
        let _map = egui::CentralPanel::default().show_inside(ui, |ui| self.map(ui));
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(5.0);
        if self.view == ViewMode::Focus {
            let mut action = None;
            let _row = ui.horizontal(|ui| {
                let back = chrome::glyph(ui, "← ATLAS", false);
                if back.clicked() {
                    action = Some(FocusAction::Close(back.rect));
                }
                let previous = chrome::glyph_enabled(ui, self.routes.len() > 1, "◀", false);
                if previous.clicked() {
                    action = Some(FocusAction::Step(-1, previous.rect));
                }
                let next = chrome::glyph_enabled(ui, self.routes.len() > 1, "▶", false);
                if next.clicked() {
                    action = Some(FocusAction::Step(1, next.rect));
                }
                if let Some(route) = self.selected_route() {
                    let _name = ui.label(chrome::section_title(route.name.to_ascii_uppercase()));
                    let _metrics = ui.label(chrome::muted(format!(
                        "{:.2} KM · ↗ {:.0} M · ◇ {:.2}",
                        route.metrics.distance_m / 1_000.0,
                        route.metrics.ascent_m,
                        route.metrics.difficulty
                    )));
                }
            });
            if let Some(action) = action {
                match action {
                    FocusAction::Close(rect) => {
                        self.view = ViewMode::Atlas;
                        self.fit = Fit::Graph;
                        self.water.click(rect);
                    }
                    FocusAction::Step(delta, rect) => {
                        self.step_candidate(delta);
                        self.water
                            .lever(rect, if delta.is_negative() { -1.0 } else { 1.0 });
                    }
                }
            }
        } else {
            let mut chosen = None;
            let _row = ui.horizontal_wrapped(|ui| {
                let _label = ui.label(chrome::eyebrow("SORT"));
                for sort in CandidateSort::ALL {
                    let response = chrome::glyph(ui, sort.label(), self.sort == sort);
                    if response.clicked() && self.sort != sort {
                        chosen = Some((sort, response.rect));
                    }
                }
                let _count = ui.label(chrome::muted(format!("{} TRAILS", self.routes.len())));
            });
            if let Some((sort, rect)) = chosen {
                self.sort = sort;
                self.water.select(rect);
            }
        }
    }

    fn gallery(&mut self, ui: &mut egui::Ui) {
        if self.routes.is_empty() {
            let rect = ui.available_rect_before_wrap();
            let _empty = ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                if self.forge_phase == ForgePhase::Striking {
                    "THE FORGE IS WALKING THE GRAPH"
                } else {
                    "NO CANDIDATES · STRIKE FIND TRAILS"
                },
                egui::FontId::monospace(13.0),
                chrome::MUTED,
            );
            if self.forge_phase == ForgePhase::Striking {
                self.water.show_loading(ui.ctx(), rect);
            } else {
                self.water.hide_loading();
            }
            return;
        }
        self.water.hide_loading();
        let order = gallery::order(&self.routes, self.sort);
        let mut opened = None;
        let scroll = egui::ScrollArea::horizontal()
            .id_salt("candidate-plate-rack")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                let _rack = ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    for (ordinal, slot) in order.iter().copied().enumerate() {
                        let response = gallery::tile(
                            ui,
                            &self.graph,
                            &self.routes[slot],
                            ordinal,
                            self.selected == Some(slot),
                        );
                        if response.hovered() {
                            self.water.hover(("candidate", slot), response.rect);
                        }
                        if response.clicked() {
                            opened = Some((slot, response.rect));
                        }
                    }
                    ui.add_space(6.0);
                });
            });
        self.water.heave(ui.ctx(), scroll.state.offset.x);
        if let Some((slot, rect)) = opened {
            self.selected = Some(slot);
            self.view = ViewMode::Focus;
            self.fit = Fit::Route(slot);
            self.water.click(rect);
        }
    }

    fn profile(&mut self, ui: &mut egui::Ui) {
        let Some(slot) = self.selected else {
            return;
        };
        ui.add_space(5.0);
        let _label = ui.label(chrome::eyebrow("ELEVATION · TERRAIN · ABSOLUTE GRADE"));
        if let Some(profile) = self.profiles.get(slot).and_then(Option::as_ref) {
            let response = profile.show(ui, ui.available_height() - 3.0);
            chrome::shallow_tension(ui, &response);
            if response.hovered() {
                self.water.hover(("profile", slot), response.rect);
            }
        }
    }

    fn map(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        self.map_rect = rect;
        self.water.begin_surface(rect);
        self.apply_fit(rect);
        let before = self.viewport;
        let moved = map::navigate(&mut self.viewport, ui, &response, rect);
        if moved {
            self.fit = Fit::None;
            if response.dragged() {
                self.water
                    .drag(rect, ui.input(|input| input.pointer.delta().y));
            } else {
                self.water.bump(rect);
            }
        }
        let painter = ui.painter_at(rect);
        let _ground = painter.rect_filled(rect, 0.0, map::MAP_GROUND);
        self.paint_basemap(&painter, rect);
        if self.layers.network {
            self.atlas.paint_network(&painter, self.viewport, rect);
        }
        self.paint_candidates(&painter, rect);
        map::paint_start(&painter, &self.graph, self.start, self.viewport, rect);
        map::paint_scale(&painter, self.viewport, rect);
        let _edge = painter.rect_stroke(
            rect.shrink(0.5),
            0.0,
            Stroke::new(1.0_f32, chrome::EDGE_STRONG),
            egui::StrokeKind::Inside,
        );
        self.paint_map_header(&painter, rect);
        if let Some(pointer) = response.hover_pos() {
            self.paint_hovered_leg(&painter, rect, pointer);
        }
        if response.clicked()
            && self.view == ViewMode::Atlas
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let coord = map::coord_at(self.viewport, rect, pointer);
            self.snap_start(
                coord,
                egui::Rect::from_center_size(pointer, vec2(18.0, 18.0)),
            );
        }
        if before != self.viewport {
            ui.ctx().request_repaint();
        }
    }

    fn paint_basemap(&mut self, painter: &egui::Painter, rect: egui::Rect) {
        if !self.layers.topo || self.basemap.is_none() {
            return;
        }
        let cover = tile::cover(self.viewport, rect);
        for stratum in &cover.strata {
            if stratum.intent.demands() {
                for placement in &stratum.placements {
                    self.demand_tile(placement.key);
                }
            }
        }
        if let Some(coherent) = cover.finest_ready(|key| self.tiles.contains(key)) {
            for placement in &coherent.placements {
                self.paint_tile(painter, *placement);
            }
        } else {
            for stratum in cover
                .strata
                .iter()
                .filter(|stratum| stratum.intent.presents())
            {
                for placement in &stratum.placements {
                    self.paint_tile(painter, *placement);
                }
            }
        }
    }

    fn paint_candidates(&self, painter: &egui::Painter, rect: egui::Rect) {
        if self.routes.is_empty() {
            return;
        }
        let order = gallery::order(&self.routes, self.sort);
        if self.view == ViewMode::Focus {
            if let Some(slot) = self.selected {
                map::paint_route(
                    painter,
                    &self.graph,
                    &self.routes[slot],
                    self.viewport,
                    rect,
                    ALLTRAILS_GREEN,
                    self.layers.terrain,
                );
            }
            return;
        }
        for (ordinal, slot) in order.iter().copied().enumerate() {
            if self.selected == Some(slot) {
                continue;
            }
            map::paint_route(
                painter,
                &self.graph,
                &self.routes[slot],
                self.viewport,
                rect,
                CANDIDATE_COLORS[ordinal % CANDIDATE_COLORS.len()].gamma_multiply(0.82),
                false,
            );
        }
        if let Some(slot) = self.selected {
            map::paint_route(
                painter,
                &self.graph,
                &self.routes[slot],
                self.viewport,
                rect,
                ALLTRAILS_GREEN,
                self.layers.terrain,
            );
        }
    }

    fn paint_hovered_leg(&self, canvas: &egui::Painter, rect: egui::Rect, pointer: egui::Pos2) {
        let Some(route) = self.selected_route() else {
            return;
        };
        let Some(edge_id) =
            map::hovered_route_edge(&self.graph, route, self.viewport, rect, pointer)
        else {
            return;
        };
        let edge = &self.graph.edges[edge_id.0];
        paint_edge_plate(canvas, rect, edge);
    }

    fn paint_map_header(&self, painter: &egui::Painter, rect: egui::Rect) {
        let text = if self.view == ViewMode::Focus {
            self.selected_route().map_or_else(
                || "TRAIL FOCUS".to_owned(),
                |route| format!("TRAIL FOCUS · {}", route.name.to_ascii_uppercase()),
            )
        } else {
            "CANDIDATE ATLAS · CLICK MAP TO MOVE TRAILHEAD".to_owned()
        };
        let galley = painter.layout_no_wrap(text, egui::FontId::monospace(11.0), chrome::TEXT);
        let plate = egui::Rect::from_min_size(
            rect.left_top() + vec2(12.0, 12.0),
            galley.size() + vec2(14.0, 8.0),
        );
        let _fill = painter.rect_filled(plate, 1.0, chrome::SURFACE.gamma_multiply(0.92));
        let _stroke = painter.rect_stroke(
            plate,
            1.0,
            Stroke::new(1.0_f32, chrome::EDGE),
            egui::StrokeKind::Inside,
        );
        painter.galley(plate.min + vec2(7.0, 4.0), galley, chrome::TEXT);
    }

    fn strike(&mut self) {
        self.serial = self.serial.saturating_add(1);
        self.params.keep = self.params.keep.max(self.count);
        let request = SearchRequest {
            serial: self.serial,
            start: self.start,
            constraints: self.constraints.clone(),
            params: self.params,
            solver: self.solver,
            count: self.count,
        };
        match self.forge.strike(request) {
            Ok(()) => {
                self.forge_phase = ForgePhase::Striking;
                self.status = format!(
                    "forging {} candidate(s) from vertex {}…",
                    self.count, self.start.0
                );
            }
            Err(err) => self.status = format!("cannot strike forge: {err:#}"),
        }
    }

    fn absorb_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.forge.events.try_recv() {
            match event {
                SearchEvent::Found {
                    serial,
                    routes,
                    elapsed,
                    solver,
                } if serial == self.serial => {
                    self.forge_phase = ForgePhase::Idle;
                    self.status = if routes.is_empty() {
                        format!(
                            "{} found no candidates in {}",
                            solver.label(),
                            duration(elapsed)
                        )
                    } else {
                        format!(
                            "{} forged {} candidate(s) in {}",
                            solver.label(),
                            routes.len(),
                            duration(elapsed)
                        )
                    };
                    self.install_routes(routes);
                    if self.map_rect.is_positive() {
                        self.water.thwack(self.map_rect, 0.8);
                    }
                    ctx.request_repaint();
                }
                SearchEvent::Found { .. } => {}
            }
        }
        let Some(basemap) = &self.basemap else {
            return;
        };
        while let Ok(event) = basemap.events.try_recv() {
            match event {
                tile::Event::Loaded { key, size, rgba } => {
                    let _inflight = self.tile_inflight.remove(&key);
                    let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
                    let texture = ctx.load_texture(
                        format!("usgs-{}-{}-{}", key.zoom, key.x, key.y),
                        image,
                        TextureOptions::LINEAR,
                    );
                    self.tiles.insert(key, texture);
                }
                tile::Event::Fault { key, message } => {
                    let _inflight = self.tile_inflight.remove(&key);
                    let _fault = self.tile_faults.insert(key);
                    self.basemap_status = format!("USGS FAULT · {message}");
                }
            }
        }
    }

    fn install_routes(&mut self, routes: Vec<Route>) {
        self.profiles = profiles(&self.graph, &routes);
        self.routes = routes;
        self.selected = (!self.routes.is_empty()).then_some(0);
        self.view = ViewMode::Atlas;
        self.fit = if self.routes.is_empty() {
            Fit::Graph
        } else {
            Fit::Route(0)
        };
    }

    fn snap_start(&mut self, requested: Coord, strike: egui::Rect) {
        let Some((start, distance_m)) = self.graph.nearest_vertex_with_distance(requested) else {
            "graph has no trailhead vertices".clone_into(&mut self.status);
            return;
        };
        self.start = start;
        self.requested_start = requested;
        self.status = format!(
            "trailhead snapped {:.0} m to vertex {} · {:.5}, {:.5}",
            distance_m,
            start.0,
            self.graph.vertices[start.0].coord.lon,
            self.graph.vertices[start.0].coord.lat
        );
        self.water.click(strike);
    }

    fn apply_fit(&mut self, rect: egui::Rect) {
        self.viewport = match self.fit {
            Fit::Graph => Viewport::fit_graph(&self.graph, rect),
            Fit::Route(slot) if slot < self.routes.len() => {
                Viewport::fit_route(&self.graph, &self.routes[slot], rect)
            }
            Fit::Route(_) | Fit::None => return,
        };
        self.fit = Fit::None;
    }

    fn take_keys(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() {
            return;
        }
        let escape =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if escape && self.view == ViewMode::Focus {
            self.view = ViewMode::Atlas;
            self.fit = Fit::Graph;
        }
        if self.view == ViewMode::Atlas {
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)) {
            self.step_candidate(-1);
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)) {
            self.step_candidate(1);
        }
    }

    fn step_candidate(&mut self, delta: isize) {
        let order = gallery::order(&self.routes, self.sort);
        let Some(selected) = self.selected else {
            return;
        };
        let current = order.iter().position(|slot| *slot == selected).unwrap_or(0);
        let next = (current.cast_signed() + delta)
            .rem_euclid(order.len().cast_signed())
            .cast_unsigned();
        self.selected = Some(order[next]);
        self.fit = Fit::Route(order[next]);
    }

    fn selected_route(&self) -> Option<&Route> {
        self.selected.and_then(|slot| self.routes.get(slot))
    }

    fn demand_tile(&mut self, key: TileKey) {
        let Some(basemap) = &self.basemap else {
            return;
        };
        if !self.tiles.contains(key)
            && !self.tile_inflight.contains(&key)
            && !self.tile_faults.contains(&key)
            && basemap.request(key)
        {
            let _fresh = self.tile_inflight.insert(key);
        }
    }

    fn paint_tile(&mut self, painter: &egui::Painter, placement: tile::Placement) {
        if let Some(texture) = self.tiles.get(placement.key) {
            let _tile = painter.image(
                texture.id(),
                placement.rect.expand(0.35),
                egui::Rect::from_min_max(egui::Pos2::ZERO, pos2(1.0, 1.0)),
                Color32::from_gray(172),
            );
        }
    }
}

pub fn forge_water() -> WaterTable {
    let mut water = WaterTable::new(Wetness::Wet);
    let (chemistry, agitation) = water.laboratory_mut();
    chemistry.refract_px = 0.22;
    chemistry.meniscus_px = 0.42;
    chemistry.ior_spread = 0.09;
    chemistry.bulge_px = 1.8;
    chemistry.source_gain = 12.0;
    agitation.enter_impulse = 0.14;
    agitation.exit_impulse = 0.08;
    agitation.click_impulse = 0.34;
    agitation.scroll_coupling = 0.0035;
    agitation.pond_impulse = 0.24;
    water
}

enum FocusAction {
    Close(egui::Rect),
    Step(isize, egui::Rect),
}

struct TileBank {
    capacity: usize,
    clock: u64,
    entries: HashMap<TileKey, (TextureHandle, u64)>,
}

impl TileBank {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            clock: 0,
            entries: HashMap::new(),
        }
    }

    fn contains(&self, key: TileKey) -> bool {
        self.entries.contains_key(&key)
    }

    fn get(&mut self, key: TileKey) -> Option<&TextureHandle> {
        self.clock = self.clock.saturating_add(1);
        self.entries.get_mut(&key).map(|(texture, age)| {
            *age = self.clock;
            &*texture
        })
    }

    fn insert(&mut self, key: TileKey, texture: TextureHandle) {
        self.clock = self.clock.saturating_add(1);
        let _prior = self.entries.insert(key, (texture, self.clock));
        while self.entries.len() > self.capacity {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, age))| *age)
                .map(|(key, _)| *key)
            else {
                break;
            };
            let _evicted = self.entries.remove(&victim);
        }
    }
}

fn profiles(graph: &TrailGraph, routes: &[Route]) -> Vec<Option<ElevationProfile>> {
    routes
        .iter()
        .map(|route| ElevationProfile::forge(graph, route))
        .collect()
}

fn scalar_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    speed: f64,
) -> egui::Response {
    ui.horizontal(|ui| {
        let _label = ui.label(chrome::eyebrow(label));
        ui.add(
            egui::DragValue::new(value)
                .range(range)
                .speed(speed)
                .max_decimals(5),
        )
    })
    .inner
}

fn scalar_row_u64(ui: &mut egui::Ui, label: &str, value: &mut u64) -> egui::Response {
    ui.horizontal(|ui| {
        let _label = ui.label(chrome::eyebrow(label));
        ui.add(egui::DragValue::new(value).speed(1.0))
    })
    .inner
}

fn range_row(
    ui: &mut egui::Ui,
    label: &str,
    minimum: &mut f64,
    maximum: &mut f64,
    speed: f64,
) -> egui::Response {
    ui.vertical(|ui| {
        let _label = ui.label(chrome::eyebrow(label));
        ui.horizontal(|ui| {
            let lo = ui.add(
                egui::DragValue::new(minimum)
                    .prefix("MIN ")
                    .range(0.0..=1_000_000.0)
                    .speed(speed)
                    .max_decimals(2),
            );
            let hi = ui.add(
                egui::DragValue::new(maximum)
                    .prefix("MAX ")
                    .range(0.0..=1_000_000.0)
                    .speed(speed)
                    .max_decimals(2),
            );
            lo.union(hi)
        })
        .inner
    })
    .inner
}

fn fraction_row(ui: &mut egui::Ui, label: &str, fraction: &mut f64) {
    let mut percent = *fraction * 100.0;
    let response = ui.add(
        egui::Slider::new(&mut percent, 0.0..=100.0)
            .text(label)
            .suffix("%")
            .step_by(0.5),
    );
    chrome::shallow_tension(ui, &response);
    if response.changed() {
        *fraction = percent / 100.0;
    }
}

fn terrain_range_row(
    ui: &mut egui::Ui,
    terrain: Terrain,
    minimum: &mut f64,
    maximum: &mut f64,
) -> egui::Response {
    let mut minimum_percent = *minimum * 100.0;
    let mut maximum_percent = *maximum * 100.0;
    let response = ui
        .horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(vec2(7.0, 7.0), egui::Sense::hover());
            let _swatch = ui
                .painter()
                .rect_filled(dot, 0.0, map::terrain_color(terrain));
            let _label = ui.add_sized(
                [58.0, 16.0],
                egui::Label::new(chrome::muted(map::terrain_label(terrain))),
            );
            let low = ui.add(
                egui::DragValue::new(&mut minimum_percent)
                    .prefix("≥")
                    .suffix("%")
                    .range(0.0..=100.0)
                    .speed(0.5)
                    .max_decimals(1),
            );
            let high = ui.add(
                egui::DragValue::new(&mut maximum_percent)
                    .prefix("≤")
                    .suffix("%")
                    .range(0.0..=100.0)
                    .speed(0.5)
                    .max_decimals(1),
            );
            low.union(high)
        })
        .inner;
    if response.changed() {
        *minimum = minimum_percent / 100.0;
        *maximum = maximum_percent / 100.0;
    }
    response
}

fn replace_terrain_bound(
    bounds: &mut std::collections::BTreeMap<Terrain, f64>,
    terrain: Terrain,
    value: f64,
    identity: f64,
) {
    if (value - identity).abs() <= f64::EPSILON {
        let _removed = bounds.remove(&terrain);
    } else {
        let _prior = bounds.insert(terrain, value);
    }
}

fn usize_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut usize,
    range: std::ops::RangeInclusive<usize>,
) {
    let _row = ui.horizontal(|ui| {
        let _label = ui.label(chrome::eyebrow(label));
        let _value = ui.add(egui::DragValue::new(value).range(range).speed(1.0));
    });
}

fn layer_toggle(ui: &mut egui::Ui, value: &mut bool, label: &str) {
    let response = ui.checkbox(value, label);
    chrome::tension(ui, &response);
}

fn metric_pair(ui: &mut egui::Ui, label: &str, value: String) {
    let _row = ui.horizontal(|ui| {
        let _label = ui.label(chrome::eyebrow(label));
        let _value = ui.label(value);
    });
}

fn distribution_bar(ui: &mut egui::Ui, label: &str, fraction: f64, color: Color32) {
    let _label = ui.horizontal(|ui| {
        let _name = ui.label(chrome::muted(label));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let _value = ui.label(chrome::muted(percent(fraction)));
        });
    });
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), egui::Sense::hover());
    let _well = ui.painter().rect_filled(rect, 0.0, chrome::CONTROL);
    let fill = egui::Rect::from_min_max(
        rect.min,
        pos2(
            egui::lerp(rect.left()..=rect.right(), fraction.clamp(0.0, 1.0) as f32),
            rect.bottom(),
        ),
    );
    let _fill = ui.painter().rect_filled(fill, 0.0, color);
}

fn paint_edge_plate(painter: &egui::Painter, map: egui::Rect, edge: &Edge) {
    let surface = edge.attr.surface.as_deref().unwrap_or("unclassified");
    let text = format!(
        "{} · {}\n{:.0} M · GRADE {:.1}% / MAX {:.1}%\n↗ {:.0} M · ↘ {:.0} M · CONF {:.0}%\nACCESS {:?} · ROAD {:.0}%",
        map::terrain_label(edge.attr.terrain),
        surface.to_ascii_uppercase(),
        edge.attr.length_m,
        edge.attr.grade_abs_mean * 100.0,
        edge.attr.grade_abs_max * 100.0,
        edge.attr.ascent_m,
        edge.attr.descent_m,
        edge.attr.confidence * 100.0,
        edge.attr.access,
        edge.attr.road_exposure * 100.0,
    );
    let galley = painter.layout(text, egui::FontId::monospace(10.5), chrome::TEXT, 270.0);
    let plate = egui::Rect::from_min_size(
        pos2(map.right() - galley.size().x - 30.0, map.top() + 12.0),
        galley.size() + vec2(16.0, 12.0),
    );
    let _fill = painter.rect_filled(plate, 1.0, chrome::SURFACE.gamma_multiply(0.96));
    let _edge = painter.rect_stroke(
        plate,
        1.0,
        Stroke::new(1.0_f32, chrome::EDGE_STRONG),
        egui::StrokeKind::Inside,
    );
    painter.galley(plate.min + vec2(8.0, 6.0), galley, chrome::TEXT);
}

fn percent(fraction: f64) -> String {
    format!("{:.1}%", fraction * 100.0)
}

fn duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{:.2} s", duration.as_secs_f64())
    } else {
        format!("{} ms", duration.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentages_are_user_facing() {
        assert_eq!(percent(0.125), "12.5%");
    }
}
