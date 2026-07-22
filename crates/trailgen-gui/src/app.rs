use crate::{
    basemap::Source as BasemapSource,
    gallery::{self, TrailSort},
    library::{FamilyId, Library, SavedTrail, SearchRecipe, TrailId, Trailhead},
    live_area::{self, RegionScribe, ScribeEvent},
    map::{self, Atlas, SELECTED_TRAIL_COLOR, Viewport},
    profile::ElevationProfile,
    project::{Project, SearchEvent, SearchForge, SearchRequest},
    relief::Relief,
    slate::{GalleryDeck, Slate},
    trail_data::{
        Event as TrailDataEvent, Mutation as TrailDataMutation, TrailData, progress_status,
    },
    vector_field::VectorField,
};
use anyhow::{Context as _, Result};
use dwemer_poolrooms::{
    chrome,
    water::{Domain, Frame as WaterFrame, Surface, Wetness},
};
use egui::{Color32, RichText, Stroke, vec2};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use trailgen_core::{
    Coord, LoopConstraints, Route, RouteMetrics, RouteShape, SearchParams, SolverKind,
    SupportPoint, Trail, TrailGraph, TrailRealization, TrailStanding,
};
use trailgen_data::SurveyRegion;

const PROFILE_HEIGHT: f32 = 178.0;
const GALLERY_HEIGHT: f32 = 190.0;
const TOOLBAR_HEIGHT: f32 = 44.0;
const STATE_SETTLE: Duration = Duration::from_millis(400);
const CANDIDATE_COUNT: usize = 12;
const TRAILHEAD_SNAP_M: f64 = 500.0;
const UNDO_DEPTH: usize = 128;
const SHAPES: [(RouteShape, &str); 3] = [
    (RouteShape::Loop, "LOOP"),
    (RouteShape::OutAndBack, "OUT + BACK"),
    (RouteShape::Open, "POINT TO POINT"),
];

pub struct TrailApp {
    root: PathBuf,
    name: String,
    graph: Arc<TrailGraph>,
    atlas: Atlas,
    forge: SearchForge,
    defaults: LoopConstraints,
    params: SearchParams,
    solver: SolverKind,
    library: Library,
    committed_library: Library,
    library_dirty: Option<Instant>,
    active_family: Option<FamilyId>,
    family_name: String,
    candidates: BTreeMap<FamilyId, CandidateRun>,
    focus: Option<Focus>,
    sort: TrailSort,
    gallery: GalleryDeck,
    viewport: Viewport,
    focus_frame: FocusFrame,
    fit: Fit,
    serial: u64,
    forge_phase: ForgePhase,
    placing_trailhead: bool,
    editor: Option<TrailEditor>,
    vector: VectorField,
    relief: Relief,
    regions: Vec<SurveyRegion>,
    corpus: Option<TrailData>,
    scribe: RegionScribe,
    offline: bool,
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

struct CandidateRun {
    routes: Vec<Route>,
    designs: Vec<Option<Trail>>,
    profiles: Vec<Option<ElevationProfile>>,
}

struct TrailEditor {
    name: String,
    origin: EditorOrigin,
    return_focus: Option<Focus>,
    shape: RouteShape,
    support_points: Vec<SupportPoint>,
    realization: Option<TrailRealization>,
    profile: Option<ElevationProfile>,
    fault: Option<String>,
    undo: VecDeque<TrailSketch>,
    redo: VecDeque<TrailSketch>,
    drag: Option<PinDrag>,
}

#[derive(Clone, PartialEq)]
struct TrailSketch {
    support_points: Vec<SupportPoint>,
}

struct PinDrag {
    slot: usize,
    before: TrailSketch,
    grab: egui::Vec2,
}

impl TrailEditor {
    fn sketch(&self) -> TrailSketch {
        TrailSketch {
            support_points: self.support_points.clone(),
        }
    }

    fn push(history: &mut VecDeque<TrailSketch>, sketch: TrailSketch) {
        if history.len() == UNDO_DEPTH {
            let _oldest = history.pop_front();
        }
        history.push_back(sketch);
    }

    fn commit(&mut self, sketch: TrailSketch) {
        Self::push(&mut self.undo, sketch);
        self.redo.clear();
    }

    fn checkpoint(&mut self) {
        self.finish_drag();
        self.commit(self.sketch());
    }

    fn finish_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        if drag.before != self.sketch() {
            self.commit(drag.before);
        }
    }

    fn undo(&mut self) -> bool {
        let current = self.sketch();
        let target = self
            .drag
            .take()
            .and_then(|drag| (drag.before != current).then_some(drag.before))
            .or_else(|| self.undo.pop_back());
        let Some(target) = target else {
            return false;
        };
        Self::push(&mut self.redo, current);
        self.restore(target);
        true
    }

    fn redo(&mut self) -> bool {
        self.finish_drag();
        let Some(target) = self.redo.pop_back() else {
            return false;
        };
        let current = self.sketch();
        Self::push(&mut self.undo, current);
        self.restore(target);
        true
    }

    fn restore(&mut self, target: TrailSketch) {
        self.support_points = target.support_points;
    }
}

#[derive(Clone)]
enum EditorOrigin {
    New(Option<FamilyId>),
    Candidate(FamilyId),
    Saved(TrailId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Focus {
    Candidate { family: FamilyId, slot: usize },
    Saved(TrailId),
}

enum FocusAction {
    Close(egui::Rect),
    Step(isize, egui::Rect),
    Save(egui::Rect),
    Edit(egui::Rect),
    Delete(egui::Rect),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FocusFrame {
    return_to: Option<Viewport>,
}

impl FocusFrame {
    fn push(&mut self, viewport: Viewport) {
        let _ = self.return_to.get_or_insert(viewport);
    }

    const fn pop(&mut self) -> Option<Viewport> {
        self.return_to.take()
    }

    fn base(self, viewport: Viewport) -> Viewport {
        self.return_to.unwrap_or(viewport)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum Fit {
    #[default]
    Graph,
    Candidate {
        family: FamilyId,
        slot: usize,
    },
    Saved(TrailId),
    None,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ForgePhase {
    #[default]
    Idle,
    Striking {
        serial: u64,
        family: FamilyId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Projects,
    Reload,
}

struct LoadedCorpus {
    regions: Vec<SurveyRegion>,
    task: Option<TrailData>,
    status: Option<String>,
}

impl LoadedCorpus {
    fn raise(
        ctx: &egui::Context,
        root: &Path,
        offline: bool,
        config: trailgen_data::TrailDataConfig,
        indexed: Option<&trailgen_data::Summary>,
    ) -> Result<Self> {
        let regions = config.regions;
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
        let status = indexed
            .map(|summary| format!("Trail data ready in {} map area(s).", summary.regions.len()))
            .or_else(|| {
                stale.then(|| {
                    if offline {
                        "Trail data needs an online refresh.".to_owned()
                    } else {
                        "Updating trails…".to_owned()
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

impl TrailApp {
    pub fn open(
        ctx: &egui::Context,
        root: &Path,
        offline: bool,
        slate_path: PathBuf,
        trail_data: trailgen_data::TrailDataConfig,
        indexed: Option<&trailgen_data::Summary>,
    ) -> Result<Self> {
        let Project {
            root,
            graph,
            config,
            library,
        } = Project::open(root)?;
        let slate = Slate::load(&slate_path, &root);
        let active_family = slate
            .active_family
            .filter(|id| library.family(*id).is_some())
            .or_else(|| library.families().first().map(|family| family.id));
        let family_name = active_family
            .and_then(|id| library.family(id))
            .map_or_else(String::new, |family| family.name.to_string());
        let corpus = LoadedCorpus::raise(ctx, &root, offline, trail_data, indexed)?;
        let vector = spawn_vector_field(ctx, &root, Arc::clone(&graph), &corpus.regions, offline)?;
        let relief = Relief::raise(ctx, &root)?;
        let restored_viewport = slate.viewport;
        let viewport = restored_viewport.unwrap_or(Viewport {
            center: [0.5, 0.5],
            zoom: 2.0,
        });
        let forge = SearchForge::spawn(ctx.clone(), Arc::clone(&graph))?;
        let atlas = Atlas::forge(&graph);
        let status = if library.families().is_empty() {
            "Create a trail family to begin."
        } else if active_family
            .and_then(|id| library.family(id))
            .and_then(|family| family.search.trailhead)
            .is_some()
        {
            "Choose Find trails to search from this trailhead."
        } else {
            "Place a trailhead on the map, then find trails."
        }
        .to_owned();
        let committed_library = library.clone();
        let gallery = if active_family.is_none() {
            GalleryDeck::Library
        } else {
            slate.gallery
        };
        let mut app = Self {
            root,
            name: config.name,
            graph,
            atlas,
            forge,
            defaults: config.constraints,
            params: config.search,
            solver: config.solver,
            library,
            committed_library,
            library_dirty: None,
            active_family,
            family_name,
            candidates: BTreeMap::new(),
            focus: None,
            sort: slate.sort,
            gallery,
            viewport,
            focus_frame: FocusFrame::default(),
            fit: if restored_viewport.is_some() {
                Fit::None
            } else {
                Fit::Graph
            },
            serial: 0,
            forge_phase: ForgePhase::Idle,
            placing_trailhead: false,
            editor: None,
            vector,
            relief,
            regions: corpus.regions,
            corpus: corpus.task,
            scribe: RegionScribe::default(),
            offline,
            shutters: slate.shutters.clone(),
            inspector_scroll: slate.inspector_scroll,
            slate_path,
            committed_slate: slate.clone(),
            observed_slate: slate,
            slate_dirty: None,
            water: forge_water(),
            status,
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
        self.tend_library(ui.ctx());
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
        let projects = ui
            .add_enabled_ui(self.editor.is_none(), |ui| {
                ui.add_sized(
                    [ui.available_width(), 27.0],
                    chrome::glyph_button("PROJECTS · CTRL+O", false),
                )
            })
            .inner
            .on_disabled_hover_text("Finish or cancel the manual trail first.");
        chrome::tension(ui, &projects);
        if projects.clicked() {
            self.workspace_signal = Some(Action::Projects);
            self.water.click(projects.rect);
        }
        ui.add_space(3.0);
        self.section(ui, "library", "trail library", true, Self::library_panel);
        if matches!(self.focus, Some(Focus::Saved(_))) {
            self.section(
                ui,
                "memberships",
                "trail families",
                true,
                Self::membership_panel,
            );
        }
        let search_title = if self.editor.is_some() {
            "trail editor"
        } else {
            "find trails"
        };
        self.section(ui, "search", search_title, true, Self::search_panel);
        self.section(ui, "areas", "map areas", true, Self::area_panel);
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

    fn library_panel(&mut self, ui: &mut egui::Ui) {
        if self.editor.is_some() {
            let _help = chrome::note(ui, "FINISH OR CANCEL THE MANUAL TRAIL TO CHANGE FAMILIES");
            return;
        }
        let create = ui.add(
            chrome::glyph_button("NEW FAMILY", false).min_size(vec2(ui.available_width(), 27.0)),
        );
        chrome::tension(ui, &create);
        if create.clicked() {
            self.commit_family_name();
            let id = self.library.add_family(&self.defaults);
            self.select_family(Some(id));
            self.flush_library();
            self.water.click(create.rect);
        }
        ui.add_space(4.0);

        let loose = self.library.loose_trails().count();
        let unfiled = ui.add_sized(
            [ui.available_width(), 25.0],
            chrome::glyph_button(
                format!("UNFILED                                      {loose}"),
                self.active_family.is_none(),
            ),
        );
        chrome::tension(ui, &unfiled);
        if unfiled.clicked() {
            self.commit_family_name();
            self.select_family(None);
            self.gallery = GalleryDeck::Library;
            self.water.select(unfiled.rect);
        }

        let families = self
            .library
            .families()
            .iter()
            .map(|family| (family.id, family.name.to_string(), family.trails.len()))
            .collect::<Vec<_>>();
        let mut select = None;
        let mut remove = None;
        for (id, name, count) in families {
            let _row = ui.horizontal(|ui| {
                let width = (ui.available_width() - 58.0).max(30.0);
                let family = ui.add_sized(
                    [width, 25.0],
                    chrome::glyph_button(
                        format!("{}    {count}", name.to_ascii_uppercase()),
                        self.active_family == Some(id),
                    ),
                );
                chrome::tension(ui, &family);
                if family.clicked() {
                    select = Some((id, family.rect));
                }
                let excise = ui
                    .add(chrome::glyph_button("DELETE", false).min_size(vec2(54.0, 23.0)))
                    .on_hover_text("Delete this family. Its trails remain in Unfiled.");
                if excise.clicked() {
                    remove = Some((id, excise.rect));
                }
            });
        }
        if let Some((id, rect)) = select {
            self.commit_family_name();
            self.select_family(Some(id));
            self.water.select(rect);
        }
        if let Some((id, rect)) = remove
            && self.library.remove_family(id)
        {
            self.candidates.remove(&id);
            if self.active_family == Some(id) {
                self.select_family(self.library.families().first().map(|family| family.id));
            }
            if matches!(self.focus, Some(Focus::Candidate { family, .. }) if family == id) {
                self.leave_focus();
            }
            self.flush_library();
            "Family deleted. Its trails are now Unfiled.".clone_into(&mut self.status);
            self.water.click(rect);
        }

        if self.active_family.is_some() {
            ui.add_space(5.0);
            let rename = ui.add(
                egui::TextEdit::singleline(&mut self.family_name)
                    .hint_text("family name")
                    .desired_width(ui.available_width()),
            );
            chrome::tension(ui, &rename);
            if rename.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                self.commit_family_name();
            }
        }
    }

    fn membership_panel(&mut self, ui: &mut egui::Ui) {
        let Some(Focus::Saved(id)) = self.focus.clone() else {
            return;
        };
        let memberships = self
            .library
            .families()
            .iter()
            .map(|family| {
                (
                    family.id,
                    family.name.to_string(),
                    self.library.contains(family.id, &id),
                )
            })
            .collect::<Vec<_>>();
        for (family, name, member) in memberships {
            let response = ui.add(
                chrome::glyph_button(name.to_ascii_uppercase(), member)
                    .min_size(vec2(ui.available_width(), 25.0)),
            );
            chrome::tension(ui, &response);
            if response.clicked() && self.library.toggle_membership(family, &id) {
                self.flush_library();
                self.water.select(response.rect);
            }
        }
    }

    fn search_panel(&mut self, ui: &mut egui::Ui) {
        if self.editor.is_some() {
            self.editor_panel(ui);
            return;
        }
        let manual = ui.add(
            chrome::glyph_button("DRAW A TRAIL", false).min_size(vec2(ui.available_width(), 30.0)),
        );
        chrome::tension(ui, &manual);
        if manual.clicked() {
            self.begin_editor(EditorOrigin::New(self.active_family), None);
            self.water.click(manual.rect);
            return;
        }
        ui.add_space(6.0);
        let Some(family_id) = self.active_family else {
            let _note = chrome::note(ui, "CREATE OR SELECT A FAMILY TO SEARCH");
            return;
        };
        let Some(mut recipe) = self
            .library
            .family(family_id)
            .map(|family| family.search.clone())
        else {
            return;
        };
        let original = recipe.clone();

        self.trailhead_editor(ui, &mut recipe);
        let recipe_changed = self.search_recipe_editor(ui, &mut recipe);

        if (recipe_changed || recipe != original)
            && let Some(family) = self.library.family_mut(family_id)
        {
            family.search = recipe;
            self.mark_library_dirty();
        }

        ui.add_space(6.0);
        let validation = self
            .search_request(family_id, self.serial.saturating_add(1))
            .and_then(|request| request.validate(&self.graph))
            .err()
            .map(|err| err.to_string());
        let striking = matches!(self.forge_phase, ForgePhase::Striking { .. });
        let find = ui.add_enabled(
            !striking && validation.is_none(),
            chrome::glyph_button(
                if striking {
                    "FINDING TRAILS…"
                } else {
                    "FIND TRAILS"
                },
                !striking && validation.is_none(),
            )
            .min_size(vec2(ui.available_width(), 36.0)),
        );
        let find = match validation {
            Some(fault) => find.on_disabled_hover_text(fault),
            None => find,
        };
        chrome::tension(ui, &find);
        if find.clicked() {
            self.strike(family_id);
            self.water.thwack(find.rect, 0.7);
        }
    }

    fn trailhead_editor(&mut self, ui: &mut egui::Ui, recipe: &mut SearchRecipe) {
        let _trailhead = ui.label(chrome::eyebrow("TRAILHEAD"));
        let _trailhead_row = ui.horizontal(|ui| {
            let placing = self.placing_trailhead;
            let place = ui.add(
                chrome::glyph_button(
                    if placing {
                        "CANCEL"
                    } else if recipe.trailhead.is_some() {
                        "MOVE ON MAP"
                    } else {
                        "PLACE ON MAP"
                    },
                    placing,
                )
                .min_size(vec2(
                    if recipe.trailhead.is_some() {
                        139.0
                    } else {
                        184.0
                    },
                    27.0,
                )),
            );
            chrome::tension(ui, &place);
            if place.clicked() {
                self.placing_trailhead = !placing;
                if self.placing_trailhead {
                    self.scribe.disarm();
                    self.leave_focus();
                }
                self.water.click(place.rect);
            }
            if recipe.trailhead.is_some() {
                let clear = ui.add(chrome::glyph_button("CLEAR", false).min_size(vec2(48.0, 27.0)));
                if clear.clicked() {
                    recipe.trailhead = None;
                    self.placing_trailhead = false;
                    self.water.click(clear.rect);
                }
            }
        });
        if recipe.trailhead.is_some() {
            let _set = chrome::note(ui, "TRAILHEAD SET");
        }
    }

    fn search_recipe_editor(&mut self, ui: &mut egui::Ui, recipe: &mut SearchRecipe) -> bool {
        ui.add_space(5.0);
        let distance_changed =
            distance_range(ui, &mut recipe.distance_m.min, &mut recipe.distance_m.max);
        let climb_changed = measure_range(
            ui,
            "CLIMB · M",
            &mut recipe.climb_m.min,
            &mut recipe.climb_m.max,
            10.0,
        );
        let _difficulty = ui.label(chrome::eyebrow("DIFFICULTY"));
        let difficulty_changed = ui
            .add(
                egui::Slider::new(&mut recipe.difficulty, 0.0..=100.0)
                    .show_value(true)
                    .integer(),
            )
            .changed();
        let _shape = ui.label(chrome::eyebrow("SHAPE"));
        let mut shape_changed = false;
        let _shapes = ui.horizontal_wrapped(|ui| {
            for (shape, label) in SHAPES {
                let response = chrome::glyph(ui, label, recipe.shape == shape);
                if response.clicked() && recipe.shape != shape {
                    recipe.shape = shape;
                    shape_changed = true;
                    self.water.select(response.rect);
                }
            }
        });
        distance_changed || climb_changed || difficulty_changed || shape_changed
    }

    fn editor_panel(&mut self, ui: &mut egui::Ui) {
        let Some(editor) = &self.editor else {
            return;
        };
        let count = editor.support_points.len();
        let ready = editor.realization.is_some();
        let fault = editor.fault.clone();
        let _mode = chrome::note(ui, format!("{count} SUPPORT POINT(S)"));
        ui.add_space(5.0);
        let _help = chrome::note(
            ui,
            if count == 0 {
                "CLICK A TRAIL TO PLACE THE TRAILHEAD"
            } else {
                "CLICK TO ADD · DRAG A PIN TO MOVE"
            },
        );
        if let Some(fault) = fault {
            let _fault = ui.colored_label(chrome::HOT, chrome::muted(fault));
        }
        ui.add_space(5.0);
        let _undo_row = ui.horizontal(|ui| {
            let can_undo = self
                .editor
                .as_ref()
                .is_some_and(|editor| !editor.undo.is_empty());
            let can_redo = self
                .editor
                .as_ref()
                .is_some_and(|editor| !editor.redo.is_empty());
            let undo = ui.add_enabled(
                can_undo,
                chrome::glyph_button("UNDO · CTRL+Z", false).min_size(vec2(112.0, 27.0)),
            );
            if undo.clicked() {
                self.undo_editor();
                self.water.click(undo.rect);
            }
            let redo = ui.add_enabled(
                can_redo,
                chrome::glyph_button("REDO · CTRL+Y", false).min_size(vec2(112.0, 27.0)),
            );
            if redo.clicked() {
                self.redo_editor();
                self.water.click(redo.rect);
            }
        });
        let clear = ui.add_enabled(
            count > 0,
            chrome::glyph_button("CLEAR", false).min_size(vec2(ui.available_width(), 27.0)),
        );
        if clear.clicked() {
            self.remember_editor();
            if let Some(editor) = &mut self.editor {
                editor.support_points.clear();
            }
            self.reforge_editor();
            self.water.click(clear.rect);
        }
        ui.add_space(5.0);
        let save = ui.add_enabled(
            ready,
            chrome::glyph_button("SAVE TRAIL · CTRL+S", ready)
                .min_size(vec2(ui.available_width(), 34.0)),
        );
        chrome::tension(ui, &save);
        if save.clicked() {
            self.save_editor();
            self.water.thwack(save.rect, 0.7);
        }
        let cancel = ui
            .add(chrome::glyph_button("CANCEL", false).min_size(vec2(ui.available_width(), 27.0)));
        if cancel.clicked() {
            self.cancel_editor();
            self.water.click(cancel.rect);
        }
    }

    fn area_panel(&mut self, ui: &mut egui::Ui) {
        let _count = chrome::note(ui, format!("{} DOWNLOADED AREA(S)", self.regions.len()));
        let selecting = self.scribe.active();
        let mutable = self.editor.is_none() && self.corpus.is_none();
        let select = ui.add_enabled(
            !self.offline && mutable,
            chrome::glyph_button(
                if selecting {
                    "CANCEL DRAWING"
                } else {
                    "ADD MAP AREA"
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
                self.placing_trailhead = false;
                self.leave_focus();
            }
            self.water.click(select.rect);
        }

        let mut excision = None;
        for (slot, region) in self.regions.iter().enumerate() {
            let _row = ui.horizontal(|ui| {
                let _label = ui.label(chrome::muted(format!("AREA {:02}", slot + 1)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let remove = ui
                        .add_enabled(
                            mutable,
                            chrome::glyph_button("REMOVE", false).min_size(vec2(58.0, 22.0)),
                        )
                        .on_hover_text("Remove this downloaded area and update trails.");
                    if remove.clicked() {
                        excision = Some((region.id.clone(), remove.rect));
                    }
                });
            });
        }
        if let Some((id, rect)) = excision {
            match self.strike_corpus(ui.ctx(), TrailDataMutation::Remove(id)) {
                Ok(()) => self.water.click(rect),
                Err(err) => self.status = format!("Could not remove that map area: {err:#}"),
            }
        }
        if !self.regions.is_empty() {
            let refresh = ui.add_enabled(
                !self.offline && mutable,
                chrome::glyph_button("REFRESH TRAILS", false)
                    .min_size(vec2(ui.available_width(), 24.0)),
            );
            chrome::tension(ui, &refresh);
            if refresh.clicked() {
                match self.strike_corpus(ui.ctx(), TrailDataMutation::Refresh) {
                    Ok(()) => self.water.click(refresh.rect),
                    Err(err) => self.status = format!("Could not refresh trails: {err:#}"),
                }
            }
        }
    }

    fn arena(&mut self, ui: &mut egui::Ui) {
        let _toolbar = egui::Panel::top("trail-toolbar")
            .exact_size(TOOLBAR_HEIGHT)
            .show_inside(ui, |ui| self.toolbar(ui));
        let _counsel = egui::Panel::bottom("trail-counsel")
            .exact_size(42.0)
            .show_inside(ui, |ui| self.counsel(ui));
        if self.editor.is_some() {
            if self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.profile.is_some())
            {
                let _profile = egui::Panel::bottom("trail-profile")
                    .exact_size(PROFILE_HEIGHT)
                    .show_inside(ui, |ui| self.profile(ui));
            }
        } else if self.focus.is_some() {
            if self.has_profile() {
                let _profile = egui::Panel::bottom("trail-profile")
                    .exact_size(PROFILE_HEIGHT)
                    .show_inside(ui, |ui| self.profile(ui));
            }
        } else {
            let _gallery = egui::Panel::bottom("trail-gallery")
                .exact_size(GALLERY_HEIGHT)
                .show_inside(ui, |ui| self.gallery(ui));
        }
        let _map = egui::CentralPanel::default().show_inside(ui, |ui| self.map(ui));
    }

    fn counsel(&self, ui: &mut egui::Ui) {
        ui.add_space(5.0);
        let _row = ui.horizontal(|ui| {
            let message = if self.corpus.is_some() {
                self.trail_data_status
                    .as_deref()
                    .unwrap_or("Updating trails…")
            } else if self.scribe.active() {
                "Drag a rectangle across the map to download its trails. Esc cancels."
            } else if let Some(editor) = &self.editor {
                if editor.support_points.is_empty() {
                    "Click a trail to place the first support point. Esc cancels."
                } else {
                    "Click to add support points; drag any bronze pin to reshape the trail."
                }
            } else if self.placing_trailhead {
                "Click a trail on the map to place this family's trailhead. Esc cancels."
            } else if self.library.families().is_empty() {
                "Create a trail family to begin."
            } else if self.active_trailhead().is_none() {
                "Place a trailhead on the map, then choose Find trails."
            } else {
                &self.status
            };
            let _message = ui.add(
                egui::Label::new(RichText::new(message).monospace().color(chrome::TEXT)).wrap(),
            );
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        if self.editor.is_some() {
            self.editor_toolbar(ui);
        } else if self.focus.is_some() {
            self.focus_toolbar(ui);
        } else {
            self.gallery_toolbar(ui);
        }
    }

    fn gallery_toolbar(&mut self, ui: &mut egui::Ui) {
        let mut deck = None;
        let mut sort = None;
        let mut clear = None;
        let _row = ui.horizontal(|ui| {
            for candidate in [GalleryDeck::Library, GalleryDeck::Results] {
                let label = match candidate {
                    GalleryDeck::Library => "LIBRARY",
                    GalleryDeck::Results => "RESULTS",
                };
                let response = chrome::glyph(ui, label, self.gallery == candidate);
                if response.clicked() && self.gallery != candidate {
                    deck = Some((candidate, response.rect));
                }
            }
            ui.separator();
            let _label = toolbar_text(ui, "SORT", chrome::MUTED);
            for candidate in TrailSort::ALL {
                let response = chrome::glyph(ui, candidate.label(), self.sort == candidate);
                if response.clicked() && self.sort != candidate {
                    sort = Some((candidate, response.rect));
                }
            }
            if self.gallery == GalleryDeck::Results
                && self
                    .active_family
                    .and_then(|family| self.candidates.get(&family))
                    .is_some_and(|run| !run.routes.is_empty())
            {
                let response = chrome::glyph(ui, "CLEAR RESULTS", false);
                if response.clicked() {
                    clear = Some(response.rect);
                }
            }
        });
        if let Some((deck, rect)) = deck {
            self.gallery = deck;
            self.water.select(rect);
        }
        if let Some((sort, rect)) = sort {
            self.sort = sort;
            self.water.select(rect);
        }
        if let Some(rect) = clear {
            if let Some(family) = self.active_family {
                self.candidates.remove(&family);
            }
            "Search results cleared. Saved trails are untouched.".clone_into(&mut self.status);
            self.water.click(rect);
        }
    }

    fn focus_toolbar(&mut self, ui: &mut egui::Ui) {
        let summary = self.focus_summary();
        let mut action = None;
        let _row = ui.horizontal(|ui| {
            let back = chrome::glyph(ui, "← BACK", false);
            if back.clicked() {
                action = Some(FocusAction::Close(back.rect));
            }
            let previous = chrome::glyph_enabled(ui, self.focus_count() > 1, "◀", false)
                .on_hover_text("Previous trail");
            if previous.clicked() {
                action = Some(FocusAction::Step(-1, previous.rect));
            }
            let next = chrome::glyph_enabled(ui, self.focus_count() > 1, "▶", false)
                .on_hover_text("Next trail");
            if next.clicked() {
                action = Some(FocusAction::Step(1, next.rect));
            }
            if let Some((name, metrics)) = &summary {
                ui.separator();
                let _name = toolbar_text(ui, name.to_ascii_uppercase(), chrome::TEXT);
                let _metrics = toolbar_text(ui, metrics_summary(metrics), chrome::MUTED);
                if let Some(standing) = self
                    .focus_standing()
                    .filter(|standing| *standing != TrailStanding::Established)
                {
                    let _standing = ui.colored_label(
                        map::trail_standing_color(standing),
                        RichText::new(format!(
                            "PATH STATUS · {}",
                            map::trail_standing_label(standing)
                        ))
                        .monospace()
                        .size(10.5),
                    );
                }
            }
            match &self.focus {
                Some(Focus::Candidate { .. }) => {
                    let edit = chrome::glyph_enabled(
                        ui,
                        self.focus_design().is_some(),
                        "EDIT TRAIL",
                        false,
                    )
                    .on_disabled_hover_text("This candidate has no canonical support-point form");
                    if edit.clicked() {
                        action = Some(FocusAction::Edit(edit.rect));
                    }
                    let save = chrome::glyph(ui, "SAVE TRAIL", true);
                    if save.clicked() {
                        action = Some(FocusAction::Save(save.rect));
                    }
                }
                Some(Focus::Saved(_)) => {
                    let edit = chrome::glyph_enabled(
                        ui,
                        self.focus_design().is_some(),
                        "EDIT TRAIL",
                        false,
                    )
                    .on_disabled_hover_text("This legacy trail has no support points");
                    if edit.clicked() {
                        action = Some(FocusAction::Edit(edit.rect));
                    }
                    let delete = chrome::glyph(ui, "DELETE TRAIL", false);
                    if delete.clicked() {
                        action = Some(FocusAction::Delete(delete.rect));
                    }
                }
                None => {}
            }
        });
        self.enact_focus_action(action.as_ref());
    }

    fn editor_toolbar(&self, ui: &mut egui::Ui) {
        let Some(editor) = &self.editor else {
            return;
        };
        let name = editor.name.to_ascii_uppercase();
        let summary = editor
            .realization
            .as_ref()
            .map(|realization| metrics_summary(&realization.route.metrics));
        let _row = ui.horizontal(|ui| {
            let _name = toolbar_text(ui, name, chrome::TEXT);
            if let Some(summary) = summary {
                ui.separator();
                let _summary = toolbar_text(ui, summary, chrome::MUTED);
            }
        });
    }

    fn enact_focus_action(&mut self, action: Option<&FocusAction>) {
        match action {
            Some(FocusAction::Close(rect)) => {
                self.leave_focus();
                self.water.click(*rect);
            }
            Some(FocusAction::Step(delta, rect)) => {
                self.step_focus(*delta);
                self.water
                    .lever(*rect, if delta.is_negative() { -1.0 } else { 1.0 });
            }
            Some(FocusAction::Save(rect)) => {
                self.save_focused_candidate();
                self.water.click(*rect);
            }
            Some(FocusAction::Edit(rect)) => {
                self.edit_focus();
                self.water.click(*rect);
            }
            Some(FocusAction::Delete(rect)) => {
                self.delete_focused_trail();
                self.water.click(*rect);
            }
            None => {}
        }
    }

    fn gallery(&mut self, ui: &mut egui::Ui) {
        match self.gallery {
            GalleryDeck::Library => self.library_gallery(ui),
            GalleryDeck::Results => self.results_gallery(ui),
        }
    }

    fn library_gallery(&mut self, ui: &mut egui::Ui) {
        let trails = self
            .visible_saved_trails()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        if trails.is_empty() {
            gallery_empty(ui, "NO SAVED TRAILS IN THIS FAMILY");
            self.water.hide_loading();
            return;
        }
        let references = trails.iter().collect::<Vec<_>>();
        let order = gallery::order_saved(&references, self.sort);
        let mut opened = None;
        let scroll = egui::ScrollArea::horizontal()
            .id_salt("trail-library-rack")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                let _rack = ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    for slot in order.iter().copied() {
                        let trail = &trails[slot];
                        let response = gallery::saved_tile(ui, trail, false);
                        if response.hovered() {
                            self.water.hover(("saved-trail", slot), response.rect);
                        }
                        if response.clicked() {
                            opened = Some((trail.id.clone(), response.rect));
                        }
                    }
                    ui.add_space(6.0);
                });
            });
        self.water.heave(ui.ctx(), scroll.state.offset.x);
        if let Some((id, rect)) = opened {
            self.enter_focus(Focus::Saved(id));
            self.water.click(rect);
        }
    }

    fn results_gallery(&mut self, ui: &mut egui::Ui) {
        let Some(family) = self.active_family else {
            gallery_empty(ui, "SELECT A FAMILY TO SEE ITS RESULTS");
            return;
        };
        let Some(run) = self.candidates.get(&family) else {
            gallery_empty(
                ui,
                if matches!(self.forge_phase, ForgePhase::Striking { family: active, .. } if active == family)
                {
                    "FINDING TRAILS…"
                } else {
                    "NO RESULTS YET"
                },
            );
            if matches!(self.forge_phase, ForgePhase::Striking { family: active, .. } if active == family)
            {
                self.water
                    .show_loading(ui.ctx(), ui.available_rect_before_wrap());
            }
            return;
        };
        self.water.hide_loading();
        if run.routes.is_empty() {
            gallery_empty(ui, "NO TRAILS MATCHED THIS SEARCH");
            return;
        }
        let order = gallery::order_candidates(&run.routes, self.sort);
        let mut opened = None;
        let scroll = egui::ScrollArea::horizontal()
            .id_salt("trail-results-rack")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                let _rack = ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    for (ordinal, slot) in order.iter().copied().enumerate() {
                        let active = self
                            .focus
                            .as_ref()
                            .is_some_and(|focus| *focus == Focus::Candidate { family, slot });
                        let response = gallery::candidate_tile(
                            ui,
                            &self.graph,
                            &run.routes[slot],
                            ordinal,
                            active,
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
            self.enter_focus(Focus::Candidate { family, slot });
            self.water.click(rect);
        }
    }

    fn profile(&mut self, ui: &mut egui::Ui) {
        ui.add_space(5.0);
        let _label = ui.label(chrome::eyebrow("ELEVATION · TERRAIN · GRADE"));
        let saved_profile = match &self.focus {
            Some(Focus::Saved(id)) => self
                .library
                .trail(id)
                .and_then(ElevationProfile::forge_saved),
            _ => None,
        };
        let profile = if let Some(editor) = &self.editor {
            editor.profile.as_ref()
        } else {
            match &self.focus {
                Some(Focus::Candidate { family, slot }) => self
                    .candidates
                    .get(family)
                    .and_then(|run| run.profiles.get(*slot))
                    .and_then(Option::as_ref),
                Some(Focus::Saved(_)) => saved_profile.as_ref(),
                None => None,
            }
        };
        if let Some(profile) = profile {
            let response = profile.show(ui, ui.available_height() - 3.0);
            chrome::shallow_tension(ui, &response);
            if response.hovered() {
                self.water.hover("trail-profile", response.rect);
            }
        }
    }

    fn map(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        self.map_rect = rect;
        self.water.begin(Domain::shelf(rect));
        self.apply_fit(rect);
        let pointer = response.interact_pointer_pos();
        let support_under_pointer =
            pointer.and_then(|pointer| self.editor_support_at(pointer, rect));
        if ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary)) {
            self.seize_editor_support(pointer, support_under_pointer, rect);
        }
        let editor_dragging = self
            .editor
            .as_ref()
            .and_then(|editor| editor.drag.as_ref())
            .is_some();
        let before = self.viewport;
        let moved = map::navigate_with(
            &mut self.viewport,
            ui,
            &response,
            rect,
            !self.scribe.active() && !editor_dragging,
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
        let canvas = ui.painter_at(rect);
        let _ground = canvas.rect_filled(rect, 0.0, map::MAP_GROUND);
        self.paint_basemap(&canvas, rect);
        self.atlas.paint_network(&canvas, self.viewport, rect);
        if !self.regions.is_empty() || self.scribe.active() {
            live_area::paint(
                &canvas,
                self.viewport,
                rect,
                &self.regions,
                self.scribe.preview(self.viewport, rect),
            );
        }
        self.paint_trails(&canvas, rect);
        if self.editor.is_some() {
            self.paint_support_points(&canvas, rect);
        } else if let Some(trailhead) = self.active_trailhead() {
            map::paint_start(&canvas, trailhead.coord(), self.viewport, rect);
        }
        map::paint_scale(&canvas, self.viewport, rect);
        self.atlas.paint_legend(&canvas, rect);
        let _edge = canvas.rect_stroke(
            rect.shrink(0.5),
            0.0,
            Stroke::new(1.0_f32, chrome::EDGE_STRONG),
            egui::StrokeKind::Inside,
        );
        self.paint_map_header(&canvas, rect);

        if editor_dragging
            && let Some(pointer) = pointer
            && let Some((slot, grab)) = self
                .editor
                .as_ref()
                .and_then(|editor| editor.drag.as_ref())
                .map(|drag| (drag.slot, drag.grab))
        {
            self.place_editor_support(
                map::coord_at(self.viewport, rect, pointer - grab),
                Some(slot),
                false,
            );
        }
        if ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary)) {
            self.finish_editor_drag();
        }
        if response.clicked()
            && self.editor.is_some()
            && support_under_pointer.is_none()
            && let Some(pointer) = pointer
        {
            self.place_editor_support(map::coord_at(self.viewport, rect, pointer), None, true);
        } else if response.clicked()
            && self.placing_trailhead
            && let Some(pointer) = response.interact_pointer_pos()
        {
            self.place_trailhead(map::coord_at(self.viewport, rect, pointer), pointer);
        }
        if before != self.viewport {
            ui.ctx().request_repaint();
        }
        self.handle_scribe(ui.ctx(), &scribe_event);
    }

    fn paint_basemap(&mut self, painter: &egui::Painter, rect: egui::Rect) {
        self.vector.paint_base(painter, self.viewport, rect);
        self.relief.paint(painter, self.viewport, rect);
        self.vector.paint_annotations(
            painter,
            self.viewport,
            rect,
            self.relief.annotations(self.viewport, rect),
        );
    }

    fn paint_trails(&self, painter: &egui::Painter, rect: egui::Rect) {
        if let Some(realization) = self
            .editor
            .as_ref()
            .and_then(|editor| editor.realization.as_ref())
        {
            map::paint_route(
                painter,
                realization.graph(&self.graph),
                &realization.route,
                self.viewport,
                rect,
                map::SELECTED_TRAIL_COLOR,
            );
            return;
        }
        match &self.focus {
            Some(Focus::Candidate { family, slot }) => {
                if let Some(route) = self
                    .candidates
                    .get(family)
                    .and_then(|run| run.routes.get(*slot))
                {
                    map::paint_route(
                        painter,
                        &self.graph,
                        route,
                        self.viewport,
                        rect,
                        SELECTED_TRAIL_COLOR,
                    );
                }
            }
            Some(Focus::Saved(id)) => {
                if let Some(trail) = self.library.trail(id) {
                    map::paint_saved_trail(
                        painter,
                        trail,
                        self.viewport,
                        rect,
                        SELECTED_TRAIL_COLOR,
                    );
                }
            }
            None if self.gallery == GalleryDeck::Results => {
                if let Some(run) = self
                    .active_family
                    .and_then(|family| self.candidates.get(&family))
                {
                    for (ordinal, slot) in gallery::order_candidates(&run.routes, self.sort)
                        .into_iter()
                        .enumerate()
                    {
                        map::paint_route(
                            painter,
                            &self.graph,
                            &run.routes[slot],
                            self.viewport,
                            rect,
                            map::candidate_color(ordinal, false),
                        );
                    }
                }
            }
            None => {
                for trail in self.visible_saved_trails() {
                    map::paint_saved_trail(
                        painter,
                        trail,
                        self.viewport,
                        rect,
                        SELECTED_TRAIL_COLOR,
                    );
                }
            }
        }
    }

    fn seize_editor_support(
        &mut self,
        pointer: Option<egui::Pos2>,
        slot: Option<usize>,
        rect: egui::Rect,
    ) {
        let (Some(pointer), Some(slot), Some(editor)) = (pointer, slot, &mut self.editor) else {
            return;
        };
        let anchor = map::screen_at(
            self.viewport,
            rect,
            map::world_from_coord(editor.support_points[slot].coord()),
        );
        editor.drag = Some(PinDrag {
            slot,
            before: editor.sketch(),
            grab: pointer - anchor,
        });
    }

    fn paint_map_header(&self, painter: &egui::Painter, rect: egui::Rect) {
        let text = if self.scribe.active() {
            "DRAW A MAP AREA".to_owned()
        } else if self.editor.is_some() {
            "MANUAL TRAIL EDITOR".to_owned()
        } else if self.placing_trailhead {
            "CLICK A TRAIL TO PLACE THE TRAILHEAD".to_owned()
        } else if let Some((name, _)) = self.focus_summary() {
            name.to_ascii_uppercase()
        } else {
            match self.gallery {
                GalleryDeck::Library => "TRAIL LIBRARY".to_owned(),
                GalleryDeck::Results => "SEARCH RESULTS".to_owned(),
            }
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
        if self.vector.has_presented_tiles() {
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

    fn handle_scribe(&mut self, ctx: &egui::Context, event: &ScribeEvent) {
        match event {
            ScribeEvent::None => {}
            ScribeEvent::Fault(fault) => {
                self.status.clear();
                self.status.push_str(fault);
            }
            ScribeEvent::Committed(bounds) => {
                if let Err(err) = trailgen_data::validate_region(*bounds) {
                    self.status = format!("That map area cannot be used: {err:#}");
                    self.scribe.arm();
                } else {
                    let region = SurveyRegion::new(*bounds)
                        .expect("validated bounds must forge a survey region");
                    if self.regions.iter().any(|known| known.id == region.id) {
                        "That map area is already downloaded.".clone_into(&mut self.status);
                    } else if let Err(err) =
                        self.strike_corpus(ctx, TrailDataMutation::Add(*bounds))
                    {
                        self.status = format!("Could not add that map area: {err:#}");
                        self.scribe.arm();
                    } else {
                        self.regions.push(region);
                    }
                }
            }
        }
    }

    fn place_trailhead(&mut self, requested: Coord, pointer: egui::Pos2) {
        let Some(family_id) = self.active_family else {
            "Select a trail family first.".clone_into(&mut self.status);
            return;
        };
        let Some((vertex, distance_m)) = self.graph.nearest_vertex_with_distance(requested) else {
            "No downloaded trail is near that point.".clone_into(&mut self.status);
            return;
        };
        if distance_m > TRAILHEAD_SNAP_M {
            "Click closer to a downloaded trail.".clone_into(&mut self.status);
            return;
        }
        let coord = self.graph.vertices[vertex.0].coord;
        let Some(trailhead) = Trailhead::forge(coord) else {
            "That trailhead cannot be used.".clone_into(&mut self.status);
            return;
        };
        if let Some(family) = self.library.family_mut(family_id) {
            family.search.trailhead = Some(trailhead);
        }
        self.placing_trailhead = false;
        self.flush_library();
        self.status = if distance_m < 20.0 {
            "Trailhead placed.".to_owned()
        } else {
            format!("Trailhead placed {distance_m:.0} m from your click.")
        };
        self.water.click(crate::forge::pin_grip(pointer));
    }

    fn editor_support_at(&self, pointer: egui::Pos2, rect: egui::Rect) -> Option<usize> {
        self.editor
            .as_ref()?
            .support_points
            .iter()
            .enumerate()
            .rev()
            .find_map(|(slot, support)| {
                let anchor =
                    map::screen_at(self.viewport, rect, map::world_from_coord(support.coord()));
                crate::forge::pin_grip(anchor)
                    .contains(pointer)
                    .then_some(slot)
            })
    }

    fn paint_support_points(&self, painter: &egui::Painter, rect: egui::Rect) {
        let Some(editor) = &self.editor else {
            return;
        };
        for (slot, support) in editor.support_points.iter().enumerate() {
            let anchor =
                map::screen_at(self.viewport, rect, map::world_from_coord(support.coord()));
            crate::forge::pin(
                painter,
                anchor,
                editor.drag.as_ref().is_some_and(|drag| drag.slot == slot),
            );
            painter.text(
                crate::forge::pin_bulb(anchor),
                egui::Align2::CENTER_CENTER,
                (slot + 1).to_string(),
                egui::FontId::monospace(12.0),
                chrome::TEXT,
            );
        }
    }

    fn place_editor_support(&mut self, requested: Coord, slot: Option<usize>, remember: bool) {
        let Some(projection) = self.graph.project_onto_edge(requested) else {
            return;
        };
        if projection.distance_m > TRAILHEAD_SNAP_M {
            if let Some(editor) = &mut self.editor {
                editor.fault = Some("Move closer to a downloaded trail.".to_owned());
            }
            return;
        }
        let support = SupportPoint::forge(projection.coord)
            .expect("edge projections contain valid coordinates");
        let Some(editor) = &self.editor else {
            return;
        };
        if slot.map_or_else(
            || editor.support_points.last() == Some(&support),
            |slot| editor.support_points.get(slot) == Some(&support),
        ) {
            return;
        }
        if remember {
            self.remember_editor();
        }
        let editor = self.editor.as_mut().expect("editor existence checked");
        if let Some(slot) = slot {
            editor.support_points[slot] = support;
        } else {
            editor.support_points.push(support);
        }
        self.reforge_editor();
    }

    fn remember_editor(&mut self) {
        if let Some(editor) = &mut self.editor {
            editor.checkpoint();
        }
    }

    fn finish_editor_drag(&mut self) {
        if let Some(editor) = &mut self.editor {
            editor.finish_drag();
        }
    }

    fn undo_editor(&mut self) {
        if self.editor.as_mut().is_some_and(TrailEditor::undo) {
            self.reforge_editor();
        }
    }

    fn redo_editor(&mut self) {
        if self.editor.as_mut().is_some_and(TrailEditor::redo) {
            self.reforge_editor();
        }
    }

    fn strike(&mut self, family: FamilyId) {
        self.serial = self.serial.saturating_add(1);
        match self
            .search_request(family, self.serial)
            .and_then(|request| self.forge.strike(request))
        {
            Ok(()) => {
                self.forge_phase = ForgePhase::Striking {
                    serial: self.serial,
                    family,
                };
                self.gallery = GalleryDeck::Results;
                "Finding trails…".clone_into(&mut self.status);
            }
            Err(err) => self.status = format!("Could not start this search: {err:#}"),
        }
    }

    fn search_request(&self, family: FamilyId, serial: u64) -> Result<SearchRequest> {
        let family = self
            .library
            .family(family)
            .context("select a trail family")?;
        let trailhead = family
            .search
            .trailhead
            .context("place a trailhead on the map")?;
        let (start, _) = self
            .graph
            .nearest_vertex_with_distance(trailhead.coord())
            .context("no downloaded trail is near this trailhead")?;
        let mut params = self.params;
        params.keep = params.keep.max(CANDIDATE_COUNT);
        Ok(SearchRequest {
            serial,
            family: family.id,
            start,
            constraints: family.search.constraints(&self.defaults)?,
            params,
            solver: self.solver,
            count: CANDIDATE_COUNT,
        })
    }

    fn absorb_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.forge.events.try_recv() {
            match event {
                SearchEvent::Found {
                    serial,
                    family,
                    routes,
                    elapsed,
                } if self.forge_phase == ForgePhase::Striking { serial, family } => {
                    self.forge_phase = ForgePhase::Idle;
                    let count = routes.len();
                    let designs = routes
                        .iter()
                        .map(|route| self.design_for_candidate(route))
                        .collect();
                    let profiles = routes
                        .iter()
                        .map(|route| ElevationProfile::forge(&self.graph, route))
                        .collect();
                    self.candidates.insert(
                        family,
                        CandidateRun {
                            routes,
                            designs,
                            profiles,
                        },
                    );
                    self.status = if count == 0 {
                        format!("No trails matched in {}.", duration(elapsed))
                    } else {
                        format!("Found {count} trail(s) in {}.", duration(elapsed))
                    };
                    if self.map_rect.is_positive() {
                        self.water.thwack(self.map_rect, 0.8);
                    }
                    ctx.request_repaint();
                }
                SearchEvent::Found { .. } => {}
            }
        }
        self.vector.absorb();
        self.relief.absorb();
    }

    fn strike_corpus(&mut self, ctx: &egui::Context, mutation: TrailDataMutation) -> Result<()> {
        anyhow::ensure!(self.corpus.is_none(), "trail update already running");
        self.corpus = Some(TrailData::spawn(ctx.clone(), self.root.clone(), mutation)?);
        self.trail_data_status = Some("Updating trails…".to_owned());
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
                    self.trail_data_status = Some(progress_status(&event));
                }
                TrailDataEvent::Ready(Some(summary)) => {
                    self.regions = summary.regions;
                    self.trail_data_status = Some(format!(
                        "Trail data ready in {} map area(s).",
                        self.regions.len()
                    ));
                    self.workspace_signal = Some(Action::Reload);
                    finished = true;
                }
                TrailDataEvent::Ready(None) => {
                    self.regions.clear();
                    self.trail_data_status = Some("No map areas downloaded.".to_owned());
                    self.workspace_signal = Some(Action::Reload);
                    finished = true;
                }
                TrailDataEvent::Fault(fault) => {
                    self.status = format!("Trail update failed: {fault}");
                    self.trail_data_status = Some("Trail update failed.".to_owned());
                    if let Ok(config) = trailgen_data::project_config(&self.root) {
                        self.regions = config.regions;
                    }
                    finished = true;
                }
            }
        }
        if finished {
            self.corpus = None;
            self.flush_library();
        }
    }

    fn visible_saved_trails(&self) -> Vec<&SavedTrail> {
        self.active_family.map_or_else(
            || self.library.loose_trails().collect(),
            |family| self.library.family_trails(family).collect(),
        )
    }

    fn design_for_candidate(&self, route: &Route) -> Option<Trail> {
        let trail = Trail::infer(&self.graph, route, self.params.routing)?;
        let realized = trail
            .realize(
                route.name.clone(),
                &self.graph,
                &self.manual_constraints(route.metrics.shape),
                1.0,
            )
            .ok()?;
        (realized.route.edges == route.edges).then_some(trail)
    }

    fn active_trailhead(&self) -> Option<Trailhead> {
        self.active_family
            .and_then(|id| self.library.family(id))
            .and_then(|family| family.search.trailhead)
    }

    fn select_family(&mut self, family: Option<FamilyId>) {
        self.active_family = family.filter(|id| self.library.family(*id).is_some());
        self.family_name = self
            .active_family
            .and_then(|id| self.library.family(id))
            .map_or_else(String::new, |family| family.name.to_string());
        self.placing_trailhead = false;
        self.leave_focus();
    }

    fn commit_family_name(&mut self) {
        let Some(id) = self.active_family else {
            return;
        };
        let old = self
            .library
            .family(id)
            .map(|family| family.name.to_string());
        if old.as_deref() == Some(self.family_name.trim()) {
            return;
        }
        if self.library.rename_family(id, &self.family_name) {
            self.family_name = self
                .library
                .family(id)
                .expect("renamed family remains present")
                .name
                .to_string();
            self.flush_library();
        } else if let Some(old) = old {
            self.family_name = old;
            "Family names must be distinct and contain 1–64 characters."
                .clone_into(&mut self.status);
        }
    }

    fn save_focused_candidate(&mut self) {
        let Some(Focus::Candidate { family, slot }) = self.focus.clone() else {
            return;
        };
        let Some(route) = self
            .candidates
            .get(&family)
            .and_then(|run| run.routes.get(slot))
            .cloned()
        else {
            return;
        };
        let design = self
            .candidates
            .get(&family)
            .and_then(|run| run.designs.get(slot))
            .and_then(Clone::clone);
        let result = if let Some(design) = design {
            let constraints = self.manual_constraints(design.shape);
            design
                .realize(
                    route.name.clone(),
                    &self.graph,
                    &constraints,
                    TRAILHEAD_SNAP_M,
                )
                .map_err(anyhow::Error::from)
                .and_then(|realization| {
                    self.library
                        .promote_realization(Some(family), &self.graph, &realization)
                })
        } else {
            self.library.promote(family, &self.graph, &route)
        };
        match result {
            Ok(id) => {
                self.enter_focus(Focus::Saved(id));
                self.gallery = GalleryDeck::Library;
                self.flush_library();
                "Trail saved to its family.".clone_into(&mut self.status);
            }
            Err(err) => self.status = format!("Could not save this trail: {err:#}"),
        }
    }

    fn focus_design(&self) -> Option<Trail> {
        match &self.focus {
            Some(Focus::Candidate { family, slot }) => self
                .candidates
                .get(family)
                .and_then(|run| run.designs.get(*slot))
                .and_then(Clone::clone),
            Some(Focus::Saved(id)) => self.library.trail(id).and_then(SavedTrail::design),
            None => None,
        }
    }

    fn edit_focus(&mut self) {
        let Some(trail) = self.focus_design() else {
            "This trail has no canonical support-point design.".clone_into(&mut self.status);
            return;
        };
        let Some((name, _)) = self.focus_summary() else {
            return;
        };
        let origin = match self.focus.as_ref() {
            Some(Focus::Candidate { family, .. }) => EditorOrigin::Candidate(*family),
            Some(Focus::Saved(id)) => EditorOrigin::Saved(id.clone()),
            None => return,
        };
        self.begin_editor(origin, Some((name, trail)));
    }

    fn begin_editor(&mut self, origin: EditorOrigin, seed: Option<(String, Trail)>) {
        let return_focus = self.focus.take();
        let (name, shape, support_points) = seed.map_or_else(
            || {
                let shape = self
                    .active_family
                    .and_then(|family| self.library.family(family))
                    .map_or(RouteShape::Open, |family| family.search.shape);
                ("manual trail".to_owned(), shape, Vec::new())
            },
            |(name, trail)| (name, trail.shape, trail.support_points),
        );
        self.scribe.disarm();
        self.placing_trailhead = false;
        self.fit = Fit::None;
        self.editor = Some(TrailEditor {
            name,
            origin,
            return_focus,
            shape,
            support_points,
            realization: None,
            profile: None,
            fault: None,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            drag: None,
        });
        self.reforge_editor();
        "Manual editor ready. Place support points on the map.".clone_into(&mut self.status);
    }

    fn manual_constraints(&self, shape: RouteShape) -> LoopConstraints {
        let mut constraints = self.defaults.clone();
        constraints.min_distance_m = 0.0;
        constraints.max_distance_m = 1.0e9;
        constraints.min_difficulty = 0.0;
        constraints.max_difficulty = 1.0e9;
        constraints.target_difficulty = None;
        constraints.min_ascent_m = 0.0;
        constraints.max_ascent_m = 1.0e9;
        constraints.min_descent_m = 0.0;
        constraints.max_descent_m = 1.0e9;
        constraints.max_road_fraction = 1.0;
        constraints.max_low_confidence_fraction = 1.0;
        constraints.max_repeated_edge_fraction = if shape == RouteShape::OutAndBack {
            1.0
        } else {
            0.0
        };
        constraints.allowed_shapes = vec![shape];
        constraints
    }

    fn reforge_editor(&mut self) {
        let Some(editor) = &self.editor else {
            return;
        };
        let name = editor.name.clone();
        let shape = editor.shape;
        let support_points = editor.support_points.clone();
        if support_points.len() < 2 {
            if let Some(editor) = &mut self.editor {
                editor.realization = None;
                editor.profile = None;
                editor.fault = None;
            }
            return;
        }
        let result = Trail::forge(shape, support_points, self.params.routing).and_then(|trail| {
            trail.realize(
                name,
                &self.graph,
                &self.manual_constraints(shape),
                TRAILHEAD_SNAP_M,
            )
        });
        if let Some(editor) = &mut self.editor {
            match result {
                Ok(realization) => {
                    editor.profile =
                        ElevationProfile::forge(realization.graph(&self.graph), &realization.route);
                    editor.realization = Some(realization);
                    editor.fault = None;
                }
                Err(err) => {
                    editor.realization = None;
                    editor.profile = None;
                    editor.fault = Some(err.to_string());
                }
            }
        }
    }

    fn save_editor(&mut self) {
        let Some(editor) = &self.editor else {
            return;
        };
        let Some(realization) = editor.realization.clone() else {
            return;
        };
        let origin = editor.origin.clone();
        let had_focus = editor.return_focus.is_some();
        let result = match &origin {
            EditorOrigin::New(family) => {
                self.library
                    .promote_realization(*family, &self.graph, &realization)
            }
            EditorOrigin::Candidate(family) => {
                self.library
                    .promote_realization(Some(*family), &self.graph, &realization)
            }
            EditorOrigin::Saved(id) => {
                self.library
                    .replace_realization(id, &self.graph, &realization)
            }
        };
        match result {
            Ok(id) => {
                self.editor = None;
                if !had_focus {
                    self.focus_frame.push(self.viewport);
                }
                self.focus = Some(Focus::Saved(id.clone()));
                self.fit = Fit::Saved(id);
                self.gallery = GalleryDeck::Library;
                self.flush_library();
                "Trail saved.".clone_into(&mut self.status);
            }
            Err(err) => self.status = format!("Could not save this trail: {err:#}"),
        }
    }

    fn cancel_editor(&mut self) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        self.focus = editor.return_focus;
        self.fit = Fit::None;
        "Manual edit cancelled.".clone_into(&mut self.status);
    }

    fn delete_focused_trail(&mut self) {
        let Some(Focus::Saved(id)) = self.focus.clone() else {
            return;
        };
        if self.library.remove_trail(&id) {
            self.leave_focus();
            self.flush_library();
            "Trail deleted from the project.".clone_into(&mut self.status);
        }
    }

    fn focus_summary(&self) -> Option<(String, RouteMetrics)> {
        match &self.focus {
            Some(Focus::Candidate { family, slot }) => self
                .candidates
                .get(family)
                .and_then(|run| run.routes.get(*slot))
                .map(|route| (route.name.clone(), route.metrics.clone())),
            Some(Focus::Saved(id)) => self
                .library
                .trail(id)
                .map(|trail| (trail.name.clone(), trail.metrics.clone())),
            None => None,
        }
    }

    fn focus_standing(&self) -> Option<TrailStanding> {
        match &self.focus {
            Some(Focus::Candidate { family, slot }) => self
                .candidates
                .get(family)
                .and_then(|run| run.routes.get(*slot))
                .and_then(|route| {
                    map::frailest_standing(
                        route
                            .edges
                            .iter()
                            .map(|edge| self.graph.edges[edge.0].attr.standing),
                    )
                }),
            Some(Focus::Saved(id)) => self.library.trail(id).and_then(|trail| {
                map::frailest_standing(trail.legs.iter().map(|leg| leg.standing))
            }),
            None => None,
        }
    }

    fn focus_count(&self) -> usize {
        match &self.focus {
            Some(Focus::Candidate { family, .. }) => self
                .candidates
                .get(family)
                .map_or(0, |run| run.routes.len()),
            Some(Focus::Saved(_)) => self.visible_saved_trails().len(),
            None => 0,
        }
    }

    fn has_profile(&self) -> bool {
        match &self.focus {
            Some(Focus::Candidate { family, slot }) => self
                .candidates
                .get(family)
                .and_then(|run| run.profiles.get(*slot))
                .is_some_and(Option::is_some),
            Some(Focus::Saved(id)) => self
                .library
                .trail(id)
                .and_then(ElevationProfile::forge_saved)
                .is_some(),
            None => false,
        }
    }

    fn step_focus(&mut self, delta: isize) {
        let next = match self.focus.clone() {
            Some(Focus::Candidate { family, slot }) => {
                let Some(run) = self.candidates.get(&family) else {
                    return;
                };
                let order = gallery::order_candidates(&run.routes, self.sort);
                let Some(next) = cyclic_step(&order, slot, delta) else {
                    return;
                };
                Focus::Candidate { family, slot: next }
            }
            Some(Focus::Saved(id)) => {
                let trails = self.visible_saved_trails();
                let order = gallery::order_saved(&trails, self.sort);
                let ids = order
                    .into_iter()
                    .map(|slot| trails[slot].id.clone())
                    .collect::<Vec<_>>();
                let Some(current) = ids.iter().position(|known| known == &id) else {
                    return;
                };
                let next = (current.cast_signed() + delta)
                    .rem_euclid(ids.len().cast_signed())
                    .cast_unsigned();
                Focus::Saved(ids[next].clone())
            }
            None => return,
        };
        self.fit = match &next {
            Focus::Candidate { family, slot } => Fit::Candidate {
                family: *family,
                slot: *slot,
            },
            Focus::Saved(id) => Fit::Saved(id.clone()),
        };
        self.focus = Some(next);
    }

    fn enter_focus(&mut self, focus: Focus) {
        self.focus_frame.push(self.viewport);
        self.fit = match &focus {
            Focus::Candidate { family, slot } => Fit::Candidate {
                family: *family,
                slot: *slot,
            },
            Focus::Saved(id) => Fit::Saved(id.clone()),
        };
        self.focus = Some(focus);
    }

    fn leave_focus(&mut self) {
        self.focus = None;
        if let Some(viewport) = self.focus_frame.pop() {
            self.viewport = viewport;
        }
        self.fit = Fit::None;
    }

    fn apply_fit(&mut self, rect: egui::Rect) {
        let viewport = match &self.fit {
            Fit::Graph => Some(Viewport::fit_graph(&self.graph, rect)),
            Fit::Candidate { family, slot } => self
                .candidates
                .get(family)
                .and_then(|run| run.routes.get(*slot))
                .map(|route| Viewport::fit_route(&self.graph, route, rect)),
            Fit::Saved(id) => self
                .library
                .trail(id)
                .map(|trail| Viewport::fit_saved(trail, rect)),
            Fit::None => None,
        };
        if let Some(viewport) = viewport {
            self.viewport = viewport;
            self.fit = Fit::None;
        }
    }

    fn take_keys(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::O)) {
            self.workspace_signal = Some(Action::Projects);
            return;
        }
        if ctx.text_edit_focused() {
            return;
        }
        let redo = ctx.input_mut(|input| {
            input.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Z)
                || input.consume_key(egui::Modifiers::CTRL, egui::Key::Y)
        });
        if redo && self.editor.is_some() {
            self.redo_editor();
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Z))
            && self.editor.is_some()
        {
            self.undo_editor();
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::S))
            && self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.realization.is_some())
        {
            self.save_editor();
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)) {
            if self.editor.is_none()
                && let Some(family) = self.active_family
                && matches!(self.forge_phase, ForgePhase::Idle)
            {
                self.strike(family);
            }
            return;
        }
        let escape =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if escape && self.editor.is_some() {
            self.cancel_editor();
            return;
        }
        if escape && self.scribe.active() {
            self.scribe.disarm();
            return;
        }
        if escape && self.placing_trailhead {
            self.placing_trailhead = false;
            return;
        }
        if escape && self.focus.is_some() {
            self.leave_focus();
        }
        if self.focus.is_none() {
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)) {
            self.step_focus(-1);
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)) {
            self.step_focus(1);
        }
    }

    fn mark_library_dirty(&mut self) {
        self.library_dirty = Some(Instant::now());
    }

    fn flush_library(&mut self) {
        match self.library.save(&self.root) {
            Ok(()) => {
                self.committed_library.clone_from(&self.library);
                self.library_dirty = None;
            }
            Err(err) => {
                self.status = format!("Could not save the trail library: {err:#}");
                self.library_dirty = Some(Instant::now());
            }
        }
    }

    fn tend_library(&mut self, ctx: &egui::Context) {
        if self.library == self.committed_library {
            self.library_dirty = None;
            return;
        }
        let dirty = self.library_dirty.get_or_insert_with(Instant::now);
        let settled = dirty.elapsed();
        if settled < STATE_SETTLE {
            ctx.request_repaint_after(STATE_SETTLE.saturating_sub(settled));
        } else {
            self.flush_library();
        }
    }

    fn snapshot(&self) -> Slate {
        Slate {
            project: self.root.clone(),
            viewport: Some(self.focus_frame.base(self.viewport)),
            shutters: self.shutters.clone(),
            inspector_scroll: self.inspector_scroll,
            sort: self.sort,
            active_family: self.active_family,
            gallery: self.gallery,
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
                self.status = format!("Could not save window state: {err:#}");
                self.slate_dirty = Some(Instant::now());
                ctx.request_repaint_after(STATE_SETTLE);
            }
        }
    }
}

fn toolbar_text(ui: &mut egui::Ui, text: impl Into<String>, color: Color32) -> egui::Response {
    ui.label(
        RichText::new(text.into())
            .monospace()
            .size(10.5)
            .color(color),
    )
}

fn metrics_summary(metrics: &RouteMetrics) -> String {
    let head = format!(
        "{:.2} KM · DIFFICULTY {:.0} · QUALITY {:.0}",
        metrics.distance_m / 1_000.0,
        metrics.difficulty,
        metrics.quality
    );
    if metrics.elevation_fraction >= 0.8 {
        format!(
            "{head} · ASCENT {:.0} M · DESCENT {:.0} M",
            metrics.ascent_m, metrics.descent_m
        )
    } else {
        format!("{head} · ELEVATION UNAVAILABLE")
    }
}

fn distance_range(ui: &mut egui::Ui, floor_m: &mut f64, ceiling_m: &mut f64) -> bool {
    let mut low = *floor_m / 1_000.0;
    let mut high = *ceiling_m / 1_000.0;
    let changed = measure_range(ui, "DISTANCE · KM", &mut low, &mut high, 0.1);
    if changed {
        *floor_m = low * 1_000.0;
        *ceiling_m = high * 1_000.0;
    }
    changed
}

fn measure_range(
    ui: &mut egui::Ui,
    label: &str,
    minimum: &mut f64,
    maximum: &mut f64,
    speed: f64,
) -> bool {
    ui.vertical(|ui| {
        let _label = ui.label(chrome::eyebrow(label));
        ui.horizontal(|ui| {
            let low = ui.add(
                egui::DragValue::new(minimum)
                    .prefix("MIN ")
                    .range(0.0..=1_000_000.0)
                    .speed(speed)
                    .max_decimals(1),
            );
            let high = ui.add(
                egui::DragValue::new(maximum)
                    .prefix("MAX ")
                    .range(0.0..=1_000_000.0)
                    .speed(speed)
                    .max_decimals(1),
            );
            if low.changed() && *minimum > *maximum {
                *maximum = *minimum;
            } else if high.changed() && *maximum < *minimum {
                *minimum = *maximum;
            }
            low.changed() || high.changed()
        })
        .inner
    })
    .inner
}

fn gallery_empty(ui: &egui::Ui, message: &str) {
    let _empty = ui.painter().text(
        ui.available_rect_before_wrap().center(),
        egui::Align2::CENTER_CENTER,
        message,
        egui::FontId::monospace(13.0),
        chrome::MUTED,
    );
}

fn cyclic_step(order: &[usize], current: usize, delta: isize) -> Option<usize> {
    let slot = order.iter().position(|slot| *slot == current)?;
    let next = (slot.cast_signed() + delta)
        .rem_euclid(order.len().cast_signed())
        .cast_unsigned();
    Some(order[next])
}

fn duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{:.2} s", duration.as_secs_f64())
    } else {
        format!("{} ms", duration.as_millis())
    }
}

fn spawn_vector_field(
    ctx: &egui::Context,
    root: &Path,
    graph: Arc<TrailGraph>,
    regions: &[SurveyRegion],
    offline: bool,
) -> Result<VectorField> {
    let bounds = regions
        .iter()
        .map(|region| region.bounds)
        .collect::<Vec<_>>();
    let source = BasemapSource::project(root, &graph, &bounds)?;
    VectorField::raise(ctx, source, offline, Some(graph))
}

impl Drop for TrailApp {
    fn drop(&mut self) {
        if self.library != self.committed_library
            && let Err(err) = self.library.save(&self.root)
        {
            eprintln!("could not save trailgen library: {err:#}");
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cyclic_navigation_respects_gallery_order() {
        let order = [4, 1, 7];
        assert_eq!(cyclic_step(&order, 1, 1), Some(7));
        assert_eq!(cyclic_step(&order, 4, -1), Some(7));
        assert_eq!(cyclic_step(&order, 8, 1), None);
    }

    #[test]
    fn focus_frame_restores_the_view_that_opened_it() {
        let base = Viewport {
            center: [0.21, 0.37],
            zoom: 14.5,
        };
        let second_focus = Viewport {
            center: [0.72, 0.81],
            zoom: 19.0,
        };
        let mut frame = FocusFrame::default();

        frame.push(base);
        frame.push(second_focus);

        assert_eq!(frame.base(second_focus), base);
        assert_eq!(frame.pop(), Some(base));
        assert_eq!(frame.pop(), None);
    }

    #[test]
    fn editor_undo_restores_whole_gestures() {
        let first = SupportPoint::forge(Coord::new(-74.0, 41.0)).expect("valid support");
        let second = SupportPoint::forge(Coord::new(-73.99, 41.01)).expect("valid support");
        let mut editor = TrailEditor {
            name: "test".to_owned(),
            origin: EditorOrigin::New(None),
            return_focus: None,
            shape: RouteShape::OutAndBack,
            support_points: vec![first],
            realization: None,
            profile: None,
            fault: None,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            drag: None,
        };

        editor.checkpoint();
        editor.support_points.push(second);
        assert!(editor.undo());
        assert_eq!(editor.support_points, vec![first]);
        assert!(editor.redo());
        assert_eq!(editor.support_points, vec![first, second]);
        assert!(editor.undo());

        editor.drag = Some(PinDrag {
            slot: 0,
            before: editor.sketch(),
            grab: egui::Vec2::ZERO,
        });
        editor.support_points[0] = second;
        editor.finish_drag();
        assert!(editor.undo());
        assert_eq!(editor.support_points, vec![first]);
        assert!(editor.redo());
        assert_eq!(editor.support_points, vec![second]);
    }
}
