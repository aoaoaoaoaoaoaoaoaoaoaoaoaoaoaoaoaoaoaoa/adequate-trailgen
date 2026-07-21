use crate::{
    basemap::Source as BasemapSource,
    gallery::{self, CandidateSort},
    live_area::{self, RegionScribe, ScribeEvent},
    map::{self, ALLTRAILS_GREEN, Atlas, CANDIDATE_COLORS, Viewport},
    profile::ElevationProfile,
    project::{Project, SearchEvent, SearchForge, SearchRequest},
    slate::{LayerSlate, SearchDraft, Slate},
    trail_data::{Event as TrailDataEvent, Mutation as TrailDataMutation, TrailData},
    vector_field::VectorField,
};
use anyhow::Result;
use dwemer_poolrooms::{
    chrome,
    water::{Domain, Frame as WaterFrame, Surface, Wetness},
};
use egui::{Color32, RichText, Stroke, pos2, vec2};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use trailgen_core::{
    Coord, Edge, Route, RouteShape, SearchParams, SolverKind, Terrain, TrailGraph, VertexId,
};
use trailgen_data::SurveyRegion;

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
    project_search: SearchDraft,
    saved_routes: Vec<Route>,
    saved_routes_visible: bool,
    routes: Vec<Route>,
    route_origin: Option<CandidateOrigin>,
    profiles: Vec<Option<ElevationProfile>>,
    selected: Option<usize>,
    sort: CandidateSort,
    view: ViewMode,
    viewport: Viewport,
    fit: Fit,
    serial: u64,
    forge_phase: ForgePhase,
    vector: VectorField,
    regions: Vec<SurveyRegion>,
    corpus: Option<TrailData>,
    scribe: RegionScribe,
    offline: bool,
    layers: Layers,
    shutters: BTreeMap<String, bool>,
    inspector_scroll: f32,
    slate_path: PathBuf,
    committed_slate: Slate,
    observed_slate: Slate,
    slate_dirty: Option<Instant>,
    water: Surface,
    status: String,
    trail_data_status: Option<String>,
    map_rect: egui::Rect,
    workspace_signal: Option<Action>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Projects,
    Reload,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandidateOrigin {
    Saved,
    Search,
}

impl CandidateOrigin {
    const fn label(self) -> &'static str {
        match self {
            Self::Saved => "SAVED",
            Self::Search => "RESULTS",
        }
    }
}

struct LoadedCandidates {
    saved: Vec<Route>,
    visible: Vec<Route>,
    profiles: Vec<Option<ElevationProfile>>,
    selected: Option<usize>,
    origin: Option<CandidateOrigin>,
    count: usize,
    status: String,
}

impl LoadedCandidates {
    fn raise(graph: &TrailGraph, saved: Vec<Route>, slate: &Slate) -> Self {
        let visible = if slate.saved_routes_visible {
            saved.clone()
        } else {
            Vec::new()
        };
        let selected = slate
            .selected
            .filter(|slot| *slot < visible.len())
            .or_else(|| (!visible.is_empty()).then_some(0));
        let origin = (!visible.is_empty()).then_some(CandidateOrigin::Saved);
        let status = if !slate.saved_routes_visible && !saved.is_empty() {
            "saved candidates are hidden; restore them above the atlas or find new trails"
                .to_owned()
        } else if visible.is_empty() {
            "choose a trailhead, tune the bounds, and strike FIND TRAILS".to_owned()
        } else {
            format!("loaded {} measured candidate(s)", visible.len())
        };
        Self {
            count: saved.len().clamp(6, 12),
            profiles: profiles(graph, &visible),
            saved,
            visible,
            selected,
            origin,
            status,
        }
    }
}

struct LoadedSearch {
    project: SearchDraft,
    draft: SearchDraft,
    start: VertexId,
}

struct LoadedCorpus {
    regions: Vec<SurveyRegion>,
    task: Option<TrailData>,
    status: Option<String>,
}

impl LoadedCorpus {
    fn raise(ctx: &egui::Context, root: &Path, offline: bool) -> Result<Self> {
        let regions = trailgen_data::project_config(root)?.regions;
        let indexed = trailgen_data::indexed_summary(root)?;
        let stale = !regions.is_empty() && indexed.is_none();
        let task = if !offline && stale {
            Some(TrailData::spawn(
                ctx.clone(),
                root.to_owned(),
                TrailDataMutation::Refresh,
            )?)
        } else {
            None
        };
        let status = indexed.as_ref().map(trail_data_status).or_else(|| {
            stale.then(|| {
                if offline {
                    "TRAIL DATA · RECONCILIATION NEEDED · OFFLINE".to_owned()
                } else {
                    "RECONCILING LIVE TRAIL AREA".to_owned()
                }
            })
        });
        Ok(Self {
            regions,
            task,
            status,
        })
    }
}

impl LoadedSearch {
    fn raise(graph: &TrailGraph, project: SearchDraft, saved: Option<&SearchDraft>) -> Self {
        let draft = saved
            .filter(|draft| {
                usable_coord(draft.requested_start) && draft.start.0 < graph.vertices.len()
            })
            .cloned()
            .unwrap_or_else(|| project.clone());
        let start = draft.start;
        Self {
            project,
            draft,
            start,
        }
    }
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
            start: project_start,
            requested_start: project_requested_start,
        } = Project::open(root)?;
        let slate = Slate::load(&slate_path, &root);
        let forge = SearchForge::spawn(ctx.clone(), Arc::clone(&graph))?;
        let corpus = LoadedCorpus::raise(ctx, &root, offline)?;
        let vector = spawn_vector_field(ctx, &root, &graph, &corpus.regions, offline)?;
        let candidates = LoadedCandidates::raise(&graph, routes, &slate);
        let search = LoadedSearch::raise(
            &graph,
            SearchDraft {
                constraints: config.constraints.clone(),
                params: config.search,
                solver: config.solver,
                count: candidates.count,
                requested_start: project_requested_start,
                start: project_start,
            },
            slate.search.as_ref(),
        );
        let atlas = Atlas::forge(&graph);
        let water = forge_water();
        let restored_viewport = slate.viewport;
        let viewport = restored_viewport.unwrap_or_else(|| Viewport {
            center: map::world_from_coord(search.draft.requested_start),
            zoom: 13.0,
        });
        let layers = Layers {
            basemap: slate.layers.basemap,
            network: slate.layers.network,
            terrain: slate.layers.terrain,
        };
        let mut app = Self {
            root,
            name: config.name,
            graph,
            atlas,
            forge,
            constraints: search.draft.constraints.clone(),
            params: search.draft.params,
            solver: search.draft.solver,
            count: search.draft.count,
            start: search.start,
            requested_start: search.draft.requested_start,
            project_search: search.project,
            saved_routes: candidates.saved,
            saved_routes_visible: slate.saved_routes_visible,
            routes: candidates.visible,
            route_origin: candidates.origin,
            profiles: candidates.profiles,
            selected: candidates.selected,
            sort: slate.sort,
            view: if slate.focus && candidates.selected.is_some() {
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
            vector,
            regions: corpus.regions,
            corpus: corpus.task,
            scribe: RegionScribe::default(),
            offline,
            layers,
            shutters: slate.shutters.clone(),
            inspector_scroll: slate.inspector_scroll,
            slate_path,
            committed_slate: slate.clone(),
            observed_slate: slate,
            slate_dirty: None,
            water,
            status: candidates.status,
            trail_data_status: corpus.status,
            map_rect: egui::Rect::ZERO,
            workspace_signal: None,
        };
        app.observed_slate = app.snapshot();
        Ok(app)
    }

    pub fn pulse(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        self.absorb_events(ui.ctx());
        self.absorb_corpus();
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
        self.workspace_signal.take()
    }

    pub fn root(&self) -> &Path {
        &self.root
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

        let project = ui.add_sized(
            [ui.available_width(), 27.0],
            chrome::glyph_button("▦  PROJECTS · CTRL+O", false),
        );
        chrome::tension(ui, &project);
        let project =
            project.on_hover_text(format!("Switch projects\nCurrent: {}", self.root.display()));
        if project.clicked() {
            self.workspace_signal = Some(Action::Projects);
            self.water.click(project.rect);
        }
        ui.add_space(3.0);

        self.section(ui, "regions", "live trail area", true, Self::region_panel);
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

    fn region_panel(&mut self, ui: &mut egui::Ui) {
        let selecting = self.scribe.active();
        let select = ui.add_enabled(
            !self.offline && self.corpus.is_none(),
            chrome::glyph_button(
                if selecting {
                    "×  CANCEL REGION"
                } else {
                    "▣  SELECT REGION"
                },
                selecting,
            )
            .min_size(vec2(ui.available_width(), 27.0)),
        );
        chrome::tension(ui, &select);
        if select.clicked() {
            if selecting {
                self.scribe.disarm();
            } else {
                self.scribe.arm();
                self.view = ViewMode::Atlas;
            }
            self.water.click(select.rect);
        }
        let _count = chrome::note(
            ui,
            format!("{} RECTANGLE(S) · UNION INDEX", self.regions.len()),
        );
        let mut excision = None;
        for (slot, region) in self.regions.iter().enumerate() {
            let _row = ui.horizontal(|ui| {
                let _label = ui.label(chrome::muted(format!("REGION {:02}", slot + 1)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let remove = ui
                        .add_enabled(
                            self.corpus.is_none(),
                            chrome::glyph_button("×", false).min_size(vec2(22.0, 22.0)),
                        )
                        .on_hover_text("Excise this rectangle and rebuild the trail union.");
                    if remove.clicked() {
                        excision = Some((region.id.clone(), remove.rect));
                    }
                });
            });
        }
        if let Some((id, rect)) = excision {
            match self.strike_corpus(ui.ctx(), TrailDataMutation::Remove(id)) {
                Ok(()) => self.water.click(rect),
                Err(err) => self.status = format!("cannot excise region: {err:#}"),
            }
        }
        if !self.regions.is_empty() {
            let refresh = ui.add_enabled(
                !self.offline && self.corpus.is_none(),
                chrome::glyph_button("↻  REFRESH TRAIL CORPUS", false)
                    .min_size(vec2(ui.available_width(), 24.0)),
            );
            chrome::tension(ui, &refresh);
            if refresh.clicked() {
                match self.strike_corpus(ui.ctx(), TrailDataMutation::Refresh) {
                    Ok(()) => self.water.click(refresh.rect),
                    Err(err) => self.status = format!("cannot refresh trail corpus: {err:#}"),
                }
            }
        }
        let _note = chrome::note(ui, "BRONZE FRAMES ARE LIVE · SHADED GROUND IS DEAD");
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
        let validation = self
            .search_request(self.serial.saturating_add(1))
            .validate(&self.graph)
            .err()
            .map(|err| err.to_string());
        let label = if striking {
            "⌁  FORGING…"
        } else {
            "⌖  FIND TRAILS · CTRL+ENTER"
        };
        let response = ui.add_enabled(
            !striking && validation.is_none(),
            chrome::glyph_button(label, striking).min_size(vec2(ui.available_width(), 34.0)),
        );
        let response = match &validation {
            Some(problem) => response.on_disabled_hover_text(problem),
            None => response,
        };
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
        if let Some(problem) = validation {
            let _problem = ui.label(
                RichText::new(format!("FIX PARAMETERS · {problem}"))
                    .monospace()
                    .small()
                    .color(Color32::from_rgb(203, 113, 91)),
            );
        }
        if self.search_draft() != self.project_search {
            ui.add_space(3.0);
            let reset = ui.add_sized(
                [ui.available_width(), 24.0],
                chrome::glyph_button("↶  RESET PROJECT DEFAULTS", false),
            );
            chrome::tension(ui, &reset);
            if reset.clicked() {
                self.restore_project_search();
                self.water.click(reset.rect);
            }
        }
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
        let _basemap = ui.add_enabled_ui(self.vector.available(), |ui| {
            layer_toggle(ui, &mut self.layers.basemap, "VECTOR BASEMAP");
        });
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
        for line in self.trail_data_status.iter().cloned().chain([
            format!(
                "GRAPH · {} V / {} E",
                self.graph.vertices.len(),
                self.graph.edges.len()
            ),
            format!("BASE · {}", self.vector.status()),
            format!("PROJECT · {}", self.root.display()),
        ]) {
            let _line = chrome::note(ui, line);
        }
    }

    fn arena(&mut self, ui: &mut egui::Ui) {
        let _toolbar = egui::Panel::top("trail-toolbar")
            .exact_size(TOOLBAR_HEIGHT)
            .show_inside(ui, |ui| self.toolbar(ui));
        let _counsel = egui::Panel::bottom("trail-counsel")
            .exact_size(42.0)
            .show_inside(ui, |ui| self.counsel(ui));
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

    fn counsel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(5.0);
        let _row = ui.horizontal(|ui| {
            let message = if self.corpus.is_some() {
                self.trail_data_status
                    .as_deref()
                    .unwrap_or("RECONCILING LIVE TRAIL AREA")
            } else if self.scribe.active() {
                "DRAG ACROSS THE MAP TO ADD A FETCH RECTANGLE · ESC CANCELS"
            } else {
                &self.status
            };
            let _message = ui.add(
                egui::Label::new(RichText::new(message).monospace().color(chrome::TEXT)).wrap(),
            );
            if self.corpus.is_none() && !self.scribe.active() {
                let select = ui.add_enabled(
                    !self.offline,
                    chrome::glyph_button("▣  ADD REGION", false).min_size(vec2(130.0, 27.0)),
                );
                chrome::tension(ui, &select);
                if select.clicked() {
                    self.scribe.arm();
                    self.view = ViewMode::Atlas;
                    self.water.click(select.rect);
                }
            }
        });
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
            let mut candidates = None;
            let _row = ui.horizontal_wrapped(|ui| {
                let _label = ui.label(chrome::eyebrow("SORT"));
                for sort in CandidateSort::ALL {
                    let response = chrome::glyph(ui, sort.label(), self.sort == sort);
                    if response.clicked() && self.sort != sort {
                        chosen = Some((sort, response.rect));
                    }
                }
                let origin = self
                    .route_origin
                    .map_or("CANDIDATES", CandidateOrigin::label);
                let _count = ui.label(chrome::muted(format!("{} {origin}", self.routes.len())));
                if self.routes.is_empty() {
                    if !self.saved_routes.is_empty() && !self.saved_routes_visible {
                        let restore = chrome::glyph(ui, "↶ RESTORE SAVED", false);
                        if restore.clicked() {
                            candidates = Some((CandidateAction::Restore, restore.rect));
                        }
                    }
                } else {
                    let clear = chrome::glyph(ui, "× CLEAR", false).on_hover_text(
                        "Remove candidates from this workbench. Project files remain untouched.",
                    );
                    if clear.clicked() {
                        candidates = Some((CandidateAction::Clear, clear.rect));
                    }
                }
            });
            if let Some((sort, rect)) = chosen {
                self.sort = sort;
                self.water.select(rect);
            }
            if let Some((action, rect)) = candidates {
                match action {
                    CandidateAction::Clear => self.clear_candidates(),
                    CandidateAction::Restore => self.restore_saved_candidates(),
                }
                self.water.click(rect);
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
                } else if !self.saved_routes.is_empty() && !self.saved_routes_visible {
                    "SAVED CANDIDATES HIDDEN · RESTORE ABOVE OR FIND NEW TRAILS"
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
        let moved = map::navigate_with(
            &mut self.viewport,
            ui,
            &response,
            rect,
            !self.scribe.active(),
        );
        if moved {
            self.fit = Fit::None;
            if response.dragged() {
                self.water
                    .drag(rect, ui.input(|input| input.pointer.delta().y));
            } else {
                self.water.bump(rect);
            }
        }
        let scribe_event = self.scribe.interact(self.viewport, ui, &response, rect);
        let painter = ui.painter_at(rect);
        let _ground = painter.rect_filled(rect, 0.0, map::MAP_GROUND);
        if self.layers.basemap {
            self.vector.paint(&painter, self.viewport, rect);
        }
        if self.layers.network {
            self.atlas.paint_network(&painter, self.viewport, rect);
        }
        if !self.regions.is_empty() || self.scribe.active() {
            live_area::paint(
                &painter,
                self.viewport,
                rect,
                &self.regions,
                self.scribe.preview(self.viewport, rect),
            );
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
            && !self.scribe.active()
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
        match scribe_event {
            ScribeEvent::None => {}
            ScribeEvent::Fault(fault) => fault.clone_into(&mut self.status),
            ScribeEvent::Committed(bounds) => {
                if let Err(err) = trailgen_data::validate_region(bounds) {
                    self.status = format!("invalid survey region: {err:#}");
                    self.scribe.arm();
                } else {
                    let region = SurveyRegion::new(bounds)
                        .expect("validated bounds must forge a survey region");
                    if self.regions.iter().any(|known| known.id == region.id) {
                        "that survey region is already live".clone_into(&mut self.status);
                    } else if let Err(err) =
                        self.strike_corpus(ui.ctx(), TrailDataMutation::Add(bounds))
                    {
                        self.status = format!("cannot add survey region: {err:#}");
                        self.scribe.arm();
                    } else {
                        self.regions.push(region);
                    }
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
        let text = if self.scribe.active() {
            "SELECT LIVE REGION · DRAG A RECTANGLE".to_owned()
        } else if self.view == ViewMode::Focus {
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
        if self.layers.basemap && self.vector.has_presented_tiles() {
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
        let request = self.search_request(self.serial);
        match self.forge.strike(request) {
            Ok(()) => {
                self.forge_phase = ForgePhase::Striking;
                self.status = format!(
                    "searching for up to {} matching route(s) from vertex {}…",
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
                    let fits = routes
                        .iter()
                        .filter(|route| route.verdict.satisfied)
                        .count();
                    self.status = if routes.is_empty() {
                        format!(
                            "{} found no routes in {}",
                            solver.label(),
                            duration(elapsed)
                        )
                    } else if fits == 0 {
                        format!(
                            "{} found no exact match in {}; showing {} nearest alternatives",
                            solver.label(),
                            duration(elapsed),
                            routes.len()
                        )
                    } else if fits == routes.len() {
                        format!(
                            "{} found {fits} matching route(s) in {}",
                            solver.label(),
                            duration(elapsed)
                        )
                    } else {
                        format!(
                            "{} found {fits} matches + {} near misses in {}",
                            solver.label(),
                            routes.len() - fits,
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
        self.vector.absorb();
    }

    fn strike_corpus(&mut self, ctx: &egui::Context, mutation: TrailDataMutation) -> Result<()> {
        anyhow::ensure!(
            self.corpus.is_none(),
            "trail corpus mutation already running"
        );
        self.corpus = Some(TrailData::spawn(ctx.clone(), self.root.clone(), mutation)?);
        self.trail_data_status = Some("RECONCILING LIVE TRAIL AREA".to_owned());
        Ok(())
    }

    fn absorb_corpus(&mut self) {
        let Some(corpus) = &self.corpus else {
            return;
        };
        let mut finished = false;
        while let Ok(event) = corpus.events.try_recv() {
            match event {
                TrailDataEvent::Progress(event) => {
                    self.trail_data_status = Some(event.status());
                }
                TrailDataEvent::Ready(Some(summary)) => {
                    self.regions = summary.regions;
                    self.trail_data_status = Some(format!(
                        "READY · {} REGION(S) · {} TRAIL SEGMENTS",
                        self.regions.len(),
                        summary.inventory.trail_segments
                    ));
                    self.workspace_signal = Some(Action::Reload);
                    finished = true;
                }
                TrailDataEvent::Ready(None) => {
                    self.regions.clear();
                    self.trail_data_status = Some("NO LIVE REGIONS".to_owned());
                    self.workspace_signal = Some(Action::Reload);
                    finished = true;
                }
                TrailDataEvent::Fault(fault) => {
                    self.status = format!("trail corpus reconciliation failed: {fault}");
                    self.trail_data_status = Some("TRAIL DATA · RECONCILIATION FAILED".to_owned());
                    if let Ok(config) = trailgen_data::project_config(&self.root) {
                        self.regions = config.regions;
                    }
                    finished = true;
                }
            }
        }
        if finished {
            self.corpus = None;
        }
    }

    fn install_routes(&mut self, routes: Vec<Route>) {
        self.profiles = profiles(&self.graph, &routes);
        self.routes = routes;
        self.route_origin = (!self.routes.is_empty()).then_some(CandidateOrigin::Search);
        self.saved_routes_visible = false;
        self.selected = (!self.routes.is_empty()).then_some(0);
        self.view = ViewMode::Atlas;
        self.fit = if self.routes.is_empty() {
            Fit::Graph
        } else {
            Fit::Route(0)
        };
    }

    fn clear_candidates(&mut self) {
        let count = self.routes.len();
        self.routes.clear();
        self.profiles.clear();
        self.route_origin = None;
        self.saved_routes_visible = false;
        self.selected = None;
        self.view = ViewMode::Atlas;
        self.fit = Fit::Graph;
        self.status =
            format!("cleared {count} candidate(s) from the workbench; project files are untouched");
    }

    fn restore_saved_candidates(&mut self) {
        self.routes.clone_from(&self.saved_routes);
        self.profiles = profiles(&self.graph, &self.routes);
        self.route_origin = (!self.routes.is_empty()).then_some(CandidateOrigin::Saved);
        self.saved_routes_visible = true;
        self.selected = (!self.routes.is_empty()).then_some(0);
        self.view = ViewMode::Atlas;
        self.fit = if self.routes.is_empty() {
            Fit::Graph
        } else {
            Fit::Route(0)
        };
        self.status = format!("restored {} saved candidate(s)", self.routes.len());
    }

    fn search_request(&self, serial: u64) -> SearchRequest {
        let mut params = self.params;
        params.keep = params.keep.max(self.count);
        SearchRequest {
            serial,
            start: self.start,
            constraints: self.constraints.clone(),
            params,
            solver: self.solver,
            count: self.count,
        }
    }

    fn search_draft(&self) -> SearchDraft {
        SearchDraft {
            constraints: self.constraints.clone(),
            params: self.params,
            solver: self.solver,
            count: self.count,
            requested_start: self.requested_start,
            start: self.start,
        }
    }

    fn restore_project_search(&mut self) {
        let project = self.project_search.clone();
        self.constraints = project.constraints;
        self.params = project.params;
        self.solver = project.solver;
        self.count = project.count;
        self.requested_start = project.requested_start;
        self.start = project.start;
        "restored the project's search defaults".clone_into(&mut self.status);
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
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::O)) {
            self.workspace_signal = Some(Action::Projects);
            return;
        }
        if ctx.text_edit_focused() {
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)) {
            if self.forge_phase != ForgePhase::Striking {
                match self
                    .search_request(self.serial.saturating_add(1))
                    .validate(&self.graph)
                {
                    Ok(()) => self.strike(),
                    Err(err) => self.status = format!("cannot find trails: {err}"),
                }
            }
            return;
        }
        let escape =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if escape && self.scribe.active() {
            self.scribe.disarm();
            return;
        }
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

    fn snapshot(&self) -> Slate {
        Slate {
            project: self.root.clone(),
            viewport: Some(self.viewport),
            shutters: self.shutters.clone(),
            inspector_scroll: self.inspector_scroll,
            sort: self.sort,
            selected: self.selected,
            focus: self.view == ViewMode::Focus,
            saved_routes_visible: self.saved_routes_visible,
            search: Some(self.search_draft()),
            layers: LayerSlate {
                basemap: self.layers.basemap,
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

fn trail_data_status(summary: &trailgen_data::Summary) -> String {
    format!(
        "TRAIL DATA · OSM / OVERPASS · {} REGION(S) · {} SEGMENTS",
        summary.regions.len(),
        summary.inventory.trail_segments
    )
}

fn spawn_vector_field(
    ctx: &egui::Context,
    root: &Path,
    graph: &TrailGraph,
    regions: &[SurveyRegion],
    offline: bool,
) -> Result<VectorField> {
    let bounds = regions
        .iter()
        .map(|region| region.bounds)
        .collect::<Vec<_>>();
    let source = BasemapSource::project(root, graph, &bounds)?;
    VectorField::raise(ctx, source, offline)
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

enum CandidateAction {
    Clear,
    Restore,
}

fn usable_coord(coord: Coord) -> bool {
    coord.lon.is_finite()
        && coord.lat.is_finite()
        && (-180.0..=180.0).contains(&coord.lon)
        && (-85.0..=85.0).contains(&coord.lat)
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
