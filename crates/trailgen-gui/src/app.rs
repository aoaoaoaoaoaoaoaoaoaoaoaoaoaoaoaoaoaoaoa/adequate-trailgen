use crate::{
    basemap::{self, Basemap, Source as BasemapSource, TileKey, VectorTile},
    gallery::{self, CandidateSort},
    map::{self, ALLTRAILS_GREEN, Atlas, CANDIDATE_COLORS, Viewport},
    profile::ElevationProfile,
    project::{Project, SearchEvent, SearchForge, SearchRequest},
    slate::{LayerSlate, Slate},
    vector_map::VectorPaint,
};
use anyhow::Result;
use dwemer_poolrooms::{
    chrome,
    water::{Domain, Frame as WaterFrame, Surface, Wetness},
};
use egui::{Color32, RichText, Stroke, pos2, vec2};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use trailgen_core::{
    Coord, Edge, Route, RouteShape, SearchParams, SolverKind, Terrain, TrailGraph, VertexId,
};

const VECTOR_CEILING: usize = 512 * 1_048_576;
const PROFILE_HEIGHT: f32 = 178.0;
const GALLERY_HEIGHT: f32 = 190.0;
const TOOLBAR_HEIGHT: f32 = 38.0;
const STATE_SETTLE: Duration = Duration::from_millis(400);
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
    tiles: VectorBank,
    presented_basemap: Arc<[Arc<VectorTile>]>,
    tile_inflight: HashSet<TileKey>,
    tile_faults: HashSet<TileKey>,
    layers: Layers,
    basemap_preference: bool,
    shutters: BTreeMap<String, bool>,
    inspector_scroll: f32,
    slate_path: PathBuf,
    committed_slate: Slate,
    observed_slate: Slate,
    slate_dirty: Option<Instant>,
    water: Surface,
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

#[derive(Clone, Copy, Debug, PartialEq)]
struct Layers {
    basemap: bool,
    network: bool,
    terrain: bool,
}

impl TrailApp {
    pub fn open(
        ctx: &egui::Context,
        root: &Path,
        offline: bool,
        slate_path: PathBuf,
    ) -> Result<Self> {
        let Project {
            root,
            graph,
            routes,
            config,
            start,
            requested_start,
        } = Project::open(root)?;
        let slate = Slate::load(&slate_path, &root);
        let forge = SearchForge::spawn(ctx.clone(), Arc::clone(&graph))?;
        let basemap = if offline {
            None
        } else {
            let source = BasemapSource::project(&root, &graph)?;
            Some(Basemap::spawn(ctx.clone(), source)?)
        };
        let selected = slate
            .selected
            .filter(|slot| *slot < routes.len())
            .or_else(|| (!routes.is_empty()).then_some(0));
        let count = routes.len().clamp(6, 12);
        let profiles = profiles(&graph, &routes);
        let atlas = Atlas::forge(&graph);
        let water = forge_water();
        let status = if routes.is_empty() {
            "choose a trailhead, tune the bounds, and strike FIND TRAILS".to_owned()
        } else {
            format!("loaded {} measured candidate(s)", routes.len())
        };
        let restored_viewport = slate.viewport;
        let viewport = restored_viewport.unwrap_or_else(|| Viewport {
            center: map::world_from_coord(requested_start),
            zoom: 13.0,
        });
        let layers = Layers {
            basemap: !offline && slate.layers.basemap,
            network: slate.layers.network,
            terrain: slate.layers.terrain,
        };
        let mut app = Self {
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
            sort: slate.sort,
            view: if slate.focus && selected.is_some() {
                ViewMode::Focus
            } else {
                ViewMode::Atlas
            },
            viewport,
            fit: if restored_viewport.is_some() {
                Fit::None
            } else {
                Fit::Graph
            },
            serial: 0,
            forge_phase: ForgePhase::Idle,
            basemap,
            tiles: VectorBank::new(VECTOR_CEILING),
            presented_basemap: Arc::from([]),
            tile_inflight: HashSet::new(),
            tile_faults: HashSet::new(),
            layers,
            basemap_preference: slate.layers.basemap,
            shutters: slate.shutters.clone(),
            inspector_scroll: slate.inspector_scroll,
            slate_path,
            committed_slate: slate.clone(),
            observed_slate: slate,
            slate_dirty: None,
            water,
            status,
            basemap_status: if offline {
                "VECTOR MAP OFFLINE".to_owned()
            } else {
                "PROTOMAPS · PREPARING PROJECT CUT".to_owned()
            },
            map_rect: egui::Rect::ZERO,
        };
        app.observed_slate = app.snapshot();
        Ok(app)
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
                    .vertical_scroll_offset(self.inspector_scroll)
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(ui.spacing().item_spacing.x);
                        self.inspector(ui);
                    });
                self.inspector_scroll = scroll.state.offset.y.max(0.0);
                self.water.heave(ui.ctx(), scroll.state.offset.y);
            });
        let _center = egui::CentralPanel::default().show_inside(ui, |ui| self.arena(ui));
        self.tend_slate(ui.ctx());
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
        let open = self.shutters.get(id).copied().unwrap_or(open);
        let wake = chrome::section(ui, id, title, open, |ui| body(self, ui));
        if let Some(wake) = wake.as_ref() {
            let _prior = self
                .shutters
                .insert(id.to_owned(), matches!(wake.flux, chrome::FoldFlux::Open));
        }
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
        layer_toggle(ui, &mut self.layers.basemap, "VECTOR BASEMAP");
        if self.basemap.is_some() {
            self.basemap_preference = self.layers.basemap;
        }
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
        self.water.begin(Domain::shelf(rect));
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
        if !self.layers.basemap || self.basemap.is_none() {
            return;
        }
        let cover = basemap::cover(self.viewport, rect);
        self.demand_cover(&cover);
        let coherent = cover
            .finest_ready(|key| self.tiles.contains(key))
            .map(|stratum| stratum.keys.clone());
        if let Some(keys) = coherent
            && (keys.len() != self.presented_basemap.len()
                || keys
                    .iter()
                    .zip(self.presented_basemap.iter())
                    .any(|(key, tile)| *key != tile.key))
        {
            self.presented_basemap = keys
                .into_iter()
                .filter_map(|key| self.tiles.get(key).cloned())
                .collect();
        }
        if !self.presented_basemap.is_empty() {
            let _basemap = painter.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                VectorPaint {
                    tiles: Arc::clone(&self.presented_basemap),
                    center_world: self.viewport.center,
                    world_points: map::world_pixels(self.viewport) as f32,
                    viewport_points: [rect.width(), rect.height()],
                    view_zoom: self.viewport.zoom as f32,
                    apparition_span: basemap::APPARITION_SPAN,
                },
            ));
        }
        self.paint_labels(painter, rect);
    }

    fn paint_labels(&self, painter: &egui::Painter, rect: egui::Rect) {
        let mut candidates = self
            .presented_basemap
            .iter()
            .flat_map(|tile| tile.labels.iter())
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|label| label.rank);
        let mut occupied = Vec::<egui::Rect>::new();
        for label in candidates {
            let maturity = basemap::apparition(self.viewport.zoom as f32, label.onset_zoom);
            if maturity <= 0.01 {
                continue;
            }
            let anchor = map::screen_at(self.viewport, rect, label.world);
            let size = label.size * 0.12_f32.mul_add(maturity, 0.88);
            let width = label.text.chars().count() as f32 * size * 0.58;
            let footprint =
                egui::Rect::from_center_size(anchor, vec2(width.max(size), size * 1.25))
                    .expand(2.0);
            if !rect.contains_rect(footprint)
                || occupied.iter().any(|prior| prior.intersects(footprint))
            {
                continue;
            }
            occupied.push(footprint);
            if occupied.len() >= 180 {
                break;
            }
            let font = egui::FontId::proportional(size);
            let halo = Color32::from_white_alpha((75.0 * maturity) as u8);
            for offset in [
                vec2(-1.0, 0.0),
                vec2(1.0, 0.0),
                vec2(0.0, -1.0),
                vec2(0.0, 1.0),
            ] {
                let _halo = painter.text(
                    anchor + offset,
                    egui::Align2::CENTER_CENTER,
                    label.text.as_ref(),
                    font.clone(),
                    halo,
                );
            }
            let _label = painter.text(
                anchor,
                egui::Align2::CENTER_CENTER,
                label.text.as_ref(),
                font,
                Color32::from_black_alpha((225.0 * maturity) as u8),
            );
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
        if self.layers.basemap && !self.presented_basemap.is_empty() {
            let attribution = painter.layout_no_wrap(
                "PROTOMAPS · © OPENSTREETMAP".to_owned(),
                egui::FontId::monospace(9.5),
                Color32::from_black_alpha(190),
            );
            let plate = egui::Rect::from_min_size(
                rect.right_bottom() - attribution.size() - vec2(16.0, 13.0),
                attribution.size() + vec2(10.0, 6.0),
            );
            let _ground = painter.rect_filled(plate, 1.0, Color32::from_white_alpha(150));
            painter.galley(
                plate.min + vec2(5.0, 3.0),
                attribution,
                Color32::from_black_alpha(190),
            );
        }
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
                basemap::Event::Forging { complete, total } => {
                    self.basemap_status = format!(
                        "FORGING Z{} PROJECT CUT · {complete}/{total} · {:.0}%",
                        basemap::MAX_SOURCE_ZOOM,
                        complete as f64 * 100.0 / total.max(1) as f64
                    );
                }
                basemap::Event::Ready { source_zoom } => {
                    self.basemap_status = format!("PROTOMAPS Z{source_zoom} · © OPENSTREETMAP");
                }
                basemap::Event::Ranging { total } => {
                    self.basemap_status = format!("PROTOMAPS · RANGING {total} VECTOR TILE(S)");
                }
                basemap::Event::Relinquished(keys) => {
                    for key in keys {
                        let _inflight = self.tile_inflight.remove(&key);
                    }
                }
                basemap::Event::Loaded(tile) => {
                    let key = tile.key;
                    let _inflight = self.tile_inflight.remove(&key);
                    self.basemap_status = format!(
                        "PROTOMAPS · {} · {} KB · {} µs MAP + {} µs CUT",
                        tile.timing.source.label(),
                        tile.timing.bytes / 1024,
                        tile.timing.archive_us,
                        tile.timing.decode_us
                    );
                    self.tiles.insert(tile);
                }
                basemap::Event::Missing(key) => {
                    let _inflight = self.tile_inflight.remove(&key);
                    let _fault = self.tile_faults.insert(key);
                }
                basemap::Event::Fault { key, message } => {
                    if let Some(key) = key {
                        let _inflight = self.tile_inflight.remove(&key);
                        let _fault = self.tile_faults.insert(key);
                    }
                    self.basemap_status = format!("BASEMAP UNAVAILABLE · {message}");
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

    fn demand_cover(&mut self, cover: &basemap::Cover) {
        if let Some(fallback) = cover.strata.first() {
            for &key in &fallback.keys {
                self.demand_tile(key);
            }
        }
        for stratum in &cover.strata {
            if stratum.intent.demands() {
                for &key in &stratum.keys {
                    self.demand_tile(key);
                }
            }
        }
        for stratum in cover.strata.iter().rev() {
            if stratum.intent == basemap::Intent::Retained {
                for &key in &stratum.keys {
                    self.demand_tile(key);
                }
            }
        }
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

    fn snapshot(&self) -> Slate {
        Slate {
            project: self.root.clone(),
            viewport: Some(self.viewport),
            shutters: self.shutters.clone(),
            inspector_scroll: self.inspector_scroll,
            sort: self.sort,
            selected: self.selected,
            focus: self.view == ViewMode::Focus,
            layers: LayerSlate {
                basemap: self.basemap_preference,
                network: self.layers.network,
                terrain: self.layers.terrain,
            },
        }
    }

    fn tend_slate(&mut self, ctx: &egui::Context) {
        let current = self.snapshot();
        if current != self.observed_slate {
            self.observed_slate = current;
            self.slate_dirty = Some(Instant::now());
        }
        if self.observed_slate == self.committed_slate {
            self.slate_dirty = None;
            return;
        }
        let dirty = self.slate_dirty.get_or_insert_with(Instant::now);
        let settled = dirty.elapsed();
        if settled < STATE_SETTLE {
            ctx.request_repaint_after(STATE_SETTLE.saturating_sub(settled));
            return;
        }
        match self.observed_slate.save(&self.slate_path) {
            Ok(()) => {
                self.committed_slate.clone_from(&self.observed_slate);
                self.slate_dirty = None;
            }
            Err(err) => {
                self.status = format!("state save failed: {err:#}");
                self.slate_dirty = Some(Instant::now());
                ctx.request_repaint_after(STATE_SETTLE);
            }
        }
    }
}

impl Drop for TrailApp {
    fn drop(&mut self) {
        let current = self.snapshot();
        if current != self.committed_slate
            && let Err(err) = current.save(&self.slate_path)
        {
            eprintln!("could not save trailgen workbench state: {err:#}");
        }
    }
}

pub fn forge_water() -> Surface {
    let mut water = Surface::new(Wetness::Wet);
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

struct VectorBank {
    ceiling: usize,
    bytes: usize,
    epoch: u64,
    tiles: HashMap<TileKey, VectorEntry>,
    order: VecDeque<(TileKey, u64)>,
}

struct VectorEntry {
    tile: Arc<VectorTile>,
    bytes: usize,
    touched: u64,
}

impl VectorBank {
    fn new(ceiling: usize) -> Self {
        Self {
            ceiling,
            bytes: 0,
            epoch: 0,
            tiles: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn contains(&self, key: TileKey) -> bool {
        self.tiles.contains_key(&key)
    }

    fn get(&mut self, key: TileKey) -> Option<&Arc<VectorTile>> {
        self.epoch = self.epoch.saturating_add(1);
        let entry = self.tiles.get_mut(&key)?;
        entry.touched = self.epoch;
        self.order.push_back((key, self.epoch));
        Some(&entry.tile)
    }

    fn insert(&mut self, tile: Arc<VectorTile>) {
        let key = tile.key;
        let bytes = tile.resident_bytes();
        self.epoch = self.epoch.saturating_add(1);
        let fresh = VectorEntry {
            tile,
            bytes,
            touched: self.epoch,
        };
        self.order.push_back((key, self.epoch));
        if let Some(prior) = self.tiles.insert(key, fresh) {
            self.bytes = self.bytes.saturating_sub(prior.bytes);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        while self.bytes > self.ceiling && self.tiles.len() > 1 {
            let Some((victim, epoch)) = self.order.pop_front() else {
                break;
            };
            if self
                .tiles
                .get(&victim)
                .is_none_or(|entry| entry.touched != epoch)
            {
                continue;
            }
            let Some(victim) = self.tiles.remove(&victim) else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(victim.bytes);
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
