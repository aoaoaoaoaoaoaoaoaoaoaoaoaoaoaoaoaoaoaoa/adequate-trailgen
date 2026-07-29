use crate::{
    ProjectIntent,
    app::{Action as TrailAction, TrailApp, forge_water},
    basemap::Source as BasemapSource,
    chrome,
    habitat::{Habitat, ProjectPlace, create_project},
    live_area::{self, RegionScribe, ScribeEvent},
    map::{self, Viewport},
    slate::Slate,
    trail_data::{
        Event as TrailDataEvent, Mutation as TrailDataMutation, TrailData, progress_status,
    },
    vector_field::VectorField,
};
use anyhow::{Context as _, Result, ensure};
use dwemer_poolrooms::water::{Frame as WaterFrame, Surface};
use egui::{Color32, RichText, Stroke, vec2};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use trailgen_core::Coord;
use trailgen_data::SurveyRegion;

pub struct Workbench {
    mode: WorkbenchMode,
    transition: Option<WorkbenchTransition>,
}

enum WorkbenchMode {
    Project {
        workspace: ProjectWorkspace,
        habitat: Habitat,
        offline: bool,
    },
    Projects(Box<ProjectDeck>),
    Limbo,
}

enum WorkbenchTransition {
    Projects,
    Project {
        workspace: ProjectWorkspace,
        habitat: Habitat,
        offline: bool,
    },
}

enum ProjectWorkspace {
    Trail(Box<TrailApp>),
    Survey(Box<SurveyWorkbench>),
}

#[derive(Clone, Copy)]
enum WorkspaceAction {
    Projects,
    Reload,
}

impl Workbench {
    pub fn launch(
        ctx: &egui::Context,
        habitat: Habitat,
        intent: ProjectIntent,
        offline: bool,
    ) -> Self {
        let candidate = match intent {
            ProjectIntent::Open(root) => Some(root),
            ProjectIntent::Resume => match habitat.resume() {
                Ok(candidate) => candidate,
                Err(err) => {
                    return Self::still(WorkbenchMode::Projects(Box::new(ProjectDeck::new(
                        habitat,
                        offline,
                        None,
                        Some(format!("could not read the previous project: {err:#}")),
                        None,
                    ))));
                }
            },
        };
        let Some(root) = candidate else {
            return Self::still(WorkbenchMode::Projects(Box::new(ProjectDeck::new(
                habitat, offline, None, None, None,
            ))));
        };
        match open_project(ctx, &habitat, &root, offline) {
            Ok(workspace) => Self::still(WorkbenchMode::Project {
                workspace,
                habitat,
                offline,
            }),
            Err(err) => Self::still(WorkbenchMode::Projects(Box::new(ProjectDeck::new(
                habitat,
                offline,
                Some(&root),
                Some(format!("could not open that project: {err:#}")),
                None,
            )))),
        }
    }

    pub fn pulse(&mut self, ui: &mut egui::Ui) {
        self.transition = match &mut self.mode {
            WorkbenchMode::Project {
                workspace,
                habitat,
                offline,
            } => match workspace.pulse(ui) {
                None => None,
                Some(WorkspaceAction::Projects) => Some(WorkbenchTransition::Projects),
                Some(WorkspaceAction::Reload) => {
                    let root = workspace.root().to_owned();
                    match open_project(ui.ctx(), habitat, &root, *offline) {
                        Ok(workspace) => Some(WorkbenchTransition::Project {
                            workspace,
                            habitat: habitat.clone(),
                            offline: *offline,
                        }),
                        Err(err) => {
                            workspace.set_fault(format!("could not open this project: {err:#}"));
                            None
                        }
                    }
                }
            },
            WorkbenchMode::Projects(deck) => {
                deck.pulse(ui)
                    .map(|workspace| WorkbenchTransition::Project {
                        workspace,
                        habitat: deck.habitat.clone(),
                        offline: deck.offline,
                    })
            }
            WorkbenchMode::Limbo => unreachable!("workbench transition escaped its pulse"),
        };
    }

    pub fn settle(&mut self) -> bool {
        let changed = self.transition.is_some();
        if changed {
            self.commit_transition();
        }
        changed
    }

    const fn still(mode: WorkbenchMode) -> Self {
        Self {
            mode,
            transition: None,
        }
    }

    fn commit_transition(&mut self) {
        match self.transition.take() {
            Some(WorkbenchTransition::Projects) => self.open_project_deck(),
            Some(WorkbenchTransition::Project {
                workspace,
                habitat,
                offline,
            }) => {
                self.mode = WorkbenchMode::Project {
                    workspace,
                    habitat,
                    offline,
                };
            }
            None => {}
        }
    }

    fn open_project_deck(&mut self) {
        let displaced = std::mem::replace(&mut self.mode, WorkbenchMode::Limbo);
        let WorkbenchMode::Project {
            workspace,
            habitat,
            offline,
        } = displaced
        else {
            unreachable!("only an open project can request the project deck");
        };
        let root = workspace.root().to_owned();
        self.mode = WorkbenchMode::Projects(Box::new(ProjectDeck::new(
            habitat,
            offline,
            Some(&root),
            None,
            Some(workspace),
        )));
    }

    pub fn water_frame(
        &mut self,
        ctx: &egui::Context,
        pixels_per_point: f32,
        tooltip_rects: &[egui::Rect],
    ) -> WaterFrame {
        match &mut self.mode {
            WorkbenchMode::Project { workspace, .. } => {
                workspace.water_frame(ctx, pixels_per_point, tooltip_rects)
            }
            WorkbenchMode::Projects(deck) => {
                deck.water.frame(ctx, pixels_per_point, tooltip_rects, None)
            }
            WorkbenchMode::Limbo => unreachable!("workbench transition escaped its frame"),
        }
    }

    #[cfg(feature = "egui-test")]
    pub(crate) fn witness_state(&self, text_edit_focused: bool) -> crate::witness::State {
        match &self.mode {
            WorkbenchMode::Project { workspace, .. } => match workspace {
                ProjectWorkspace::Trail(app) => app.witness_state(text_edit_focused),
                ProjectWorkspace::Survey(_) => {
                    crate::witness::State::empty("survey", "browse", text_edit_focused)
                }
            },
            WorkbenchMode::Projects(_) => {
                crate::witness::State::empty("projects", "projects", text_edit_focused)
            }
            WorkbenchMode::Limbo => {
                crate::witness::State::empty("limbo", "transition", text_edit_focused)
            }
        }
    }
}

impl ProjectWorkspace {
    fn pulse(&mut self, ui: &mut egui::Ui) -> Option<WorkspaceAction> {
        match self {
            Self::Trail(app) => app.pulse(ui).map(|action| match action {
                TrailAction::Projects => WorkspaceAction::Projects,
                TrailAction::Reload => WorkspaceAction::Reload,
            }),
            Self::Survey(project) => project.pulse(ui),
        }
    }

    fn root(&self) -> &Path {
        match self {
            Self::Trail(app) => app.root(),
            Self::Survey(project) => &project.root,
        }
    }

    fn set_fault(&mut self, fault: String) {
        if let Self::Survey(project) = self {
            project.fault = Some(fault);
        }
    }

    fn water_frame(
        &mut self,
        ctx: &egui::Context,
        pixels_per_point: f32,
        tooltip_rects: &[egui::Rect],
    ) -> WaterFrame {
        match self {
            Self::Trail(app) => app.water_frame(ctx, pixels_per_point, tooltip_rects),
            Self::Survey(project) => {
                project
                    .water
                    .frame(ctx, pixels_per_point, tooltip_rects, None)
            }
        }
    }
}

const STATE_SETTLE: Duration = Duration::from_millis(400);

struct SurveyWorkbench {
    root: PathBuf,
    name: String,
    regions: Vec<SurveyRegion>,
    corpus: Option<TrailData>,
    corpus_status: String,
    offline: bool,
    fault: Option<String>,
    vector: VectorField,
    viewport: Viewport,
    cartography: map::CartographicClock,
    scale_bar: map::ScaleBar,
    fit_regions: bool,
    scribe: RegionScribe,
    slate_path: PathBuf,
    committed_slate: Slate,
    observed_slate: Slate,
    slate_dirty: Option<Instant>,
    water: Surface,
}

impl SurveyWorkbench {
    fn new(
        ctx: &egui::Context,
        place: ProjectPlace,
        offline: bool,
        slate_path: PathBuf,
    ) -> Result<Self> {
        let config = trailgen_data::project_config(&place.root)?;
        let slate = Slate::load(&slate_path, &place.root);
        let fit_regions = slate.viewport.is_none() && !config.regions.is_empty();
        let viewport = slate.viewport.unwrap_or_else(|| Viewport {
            center: map::world_from_coord(Coord::new(-98.5, 39.5)),
            zoom: 4.2,
        });
        let vector = VectorField::raise(ctx, BasemapSource::bootstrap()?, offline, None)?;
        let cartography = map::CartographicClock::new(viewport);
        let mut project = Self {
            root: place.root,
            name: place.name,
            regions: config.regions,
            corpus: None,
            corpus_status: if offline {
                "Go online to download trails.".to_owned()
            } else {
                "Draw a map area to download its trails.".to_owned()
            },
            offline,
            fault: None,
            vector,
            viewport,
            cartography,
            scale_bar: map::ScaleBar::default(),
            fit_regions,
            scribe: RegionScribe::default(),
            slate_path,
            committed_slate: slate.clone(),
            observed_slate: slate,
            slate_dirty: None,
            water: forge_water(),
        };
        if !offline && !project.regions.is_empty() {
            project.strike(ctx, TrailDataMutation::Refresh)?;
        }
        Ok(project)
    }

    fn pulse(&mut self, ui: &mut egui::Ui) -> Option<WorkspaceAction> {
        self.vector.absorb();
        let shortcut = ui
            .ctx()
            .input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::O));
        let mut action = shortcut.then_some(WorkspaceAction::Projects);
        if ui
            .ctx()
            .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.scribe.disarm();
        }
        self.absorb_corpus(&mut action);
        let _left = egui::Panel::left("survey-inspector")
            .resizable(false)
            .exact_size(chrome::INSPECTOR_WIDTH)
            .show_inside(ui, |ui| self.inspector(ui, &mut action));
        let _center = egui::CentralPanel::default().show_inside(ui, |ui| self.arena(ui));
        self.tend_slate(ui.ctx());
        action
    }

    fn inspector(&mut self, ui: &mut egui::Ui, action: &mut Option<WorkspaceAction>) {
        ui.add_space(ui.spacing().item_spacing.x);
        let _name = ui.label(chrome::title(self.name.to_ascii_uppercase()));
        ui.add_space(3.0);
        let projects = ui.add_sized(
            [ui.available_width(), 27.0],
            chrome::command_button("PROJECTS · CTRL+O", false),
        );
        chrome::tension(ui, &projects);
        if projects.clicked() {
            *action = Some(WorkspaceAction::Projects);
            self.water.click(projects.rect);
        }
        ui.add_space(14.0);
        let _label = ui.label(chrome::section_title("MAP AREAS"));
        let selecting = self.scribe.active();
        let select = ui.add_enabled(
            !self.offline && self.corpus.is_none(),
            chrome::command_button(
                if selecting {
                    "CANCEL DRAWING"
                } else {
                    "ADD MAP AREA"
                },
                selecting,
            )
            .min_size(vec2(ui.available_width(), 34.0)),
        );
        chrome::tension(ui, &select);
        if select.clicked() {
            if selecting {
                self.scribe.disarm();
            } else {
                self.scribe.arm();
            }
            self.water.click(select.rect);
        }
        ui.add_space(7.0);
        let _count = chrome::note(ui, format!("{} DOWNLOADED AREA(S)", self.regions.len()));
        let mut excision = None;
        for (slot, region) in self.regions.iter().enumerate() {
            let _region = ui.horizontal(|ui| {
                let _area = ui.label(chrome::muted(format!("AREA {slot:02}")));
                let remove = ui
                    .add_enabled(
                        self.corpus.is_none(),
                        chrome::command_button("REMOVE", false).min_size(vec2(60.0, 24.0)),
                    )
                    .on_hover_text("Remove this downloaded area and update trails.");
                if remove.clicked() {
                    excision = Some((region.id.clone(), remove.rect));
                }
            });
        }
        if let Some((id, rect)) = excision {
            if let Err(err) = self.strike(ui.ctx(), TrailDataMutation::Remove(id)) {
                self.fault = Some(format!("{err:#}"));
            } else {
                "Removing that map area…".clone_into(&mut self.corpus_status);
                self.water.click(rect);
            }
        }
        if !self.regions.is_empty() {
            ui.add_space(6.0);
            let refresh = ui.add_enabled(
                !self.offline && self.corpus.is_none(),
                chrome::command_button("REFRESH TRAILS", false)
                    .min_size(vec2(ui.available_width(), 27.0)),
            );
            chrome::tension(ui, &refresh);
            if refresh.clicked()
                && let Err(err) = self.strike(ui.ctx(), TrailDataMutation::Refresh)
            {
                self.fault = Some(format!("{err:#}"));
            }
        }
        if let Some(fault) = &self.fault {
            fault_label(ui, fault);
        }
    }

    fn arena(&mut self, ui: &mut egui::Ui) {
        let _counsel = egui::Panel::bottom("survey-counsel")
            .exact_size(52.0)
            .show_inside(ui, |ui| self.counsel(ui));
        let _map = egui::CentralPanel::default().show_inside(ui, |ui| self.map(ui));
    }

    fn counsel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        let _row = ui.horizontal(|ui| {
            let message = if self.corpus.is_some() {
                &self.corpus_status
            } else if self.scribe.active() {
                "Drag a rectangle across the map to download its trails. Esc cancels."
            } else if self.offline && !self.vector.has_presented_tiles() {
                "Go online once to load the map and download trails."
            } else if self.offline {
                "The cached map is available offline. Go online to download trails."
            } else if self.regions.is_empty() {
                "Use Add Map Area to choose where Trailgen should download trails."
            } else {
                "Add another map area or refresh the downloaded trails."
            };
            let _message = ui.add(
                egui::Label::new(RichText::new(message).monospace().color(chrome::TEXT)).wrap(),
            );
            if self.corpus.is_none() && !self.scribe.active() {
                let select = ui.add_enabled(
                    !self.offline,
                    chrome::command_button("ADD MAP AREA", true).min_size(vec2(164.0, 29.0)),
                );
                chrome::tension(ui, &select);
                if select.clicked() {
                    self.scribe.arm();
                    self.water.click(select.rect);
                }
            }
        });
    }

    fn map(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        if self.fit_regions {
            self.viewport = map::fit_coords(
                self.regions.iter().flat_map(|region| {
                    let bounds = region.bounds;
                    [
                        Coord::new(bounds.west, bounds.south),
                        Coord::new(bounds.east, bounds.north),
                    ]
                }),
                rect,
            );
            self.fit_regions = false;
        }
        self.water
            .begin(dwemer_poolrooms::water::Domain::shelf(rect));
        if map::navigate_with(
            &mut self.viewport,
            ui,
            &response,
            rect,
            !self.scribe.active(),
        ) {
            if response.dragged() {
                self.water
                    .drag(rect, ui.input(|input| input.pointer.delta().y));
            } else {
                self.water.bump(rect);
            }
        }
        let event = self.scribe.interact(self.viewport, ui, &response, rect);
        let painter = ui.painter_at(rect);
        let frame = map::MapFramePlan::forge(self.viewport, rect);
        let cartography = self.cartography.observe(self.viewport, ui.ctx());
        painter.rect_filled(rect, 0.0, map::MAP_GROUND);
        self.vector.paint_base(&painter, frame, cartography);
        live_area::paint(
            &painter,
            self.viewport,
            rect,
            &self.regions,
            self.scribe.preview(self.viewport, rect),
        );
        self.vector
            .paint_annotations(&painter, frame, cartography, 0, Vec::new);
        self.scale_bar.paint(&painter, self.viewport, rect);
        painter.rect_stroke(
            rect.shrink(0.5),
            0.0,
            Stroke::new(1.0_f32, chrome::EDGE_STRONG),
            egui::StrokeKind::Inside,
        );
        match event {
            ScribeEvent::None => {}
            ScribeEvent::Fault(fault) => self.fault = Some(fault.to_owned()),
            ScribeEvent::Committed(bounds) => {
                if self.offline {
                    self.fault = Some("map areas cannot be downloaded while offline".to_owned());
                } else if let Err(err) = trailgen_data::validate_region(bounds) {
                    self.fault = Some(format!("{err:#}"));
                    self.scribe.arm();
                } else {
                    let region = SurveyRegion::new(bounds)
                        .expect("validated bounds must forge a survey region");
                    if self.regions.iter().any(|known| known.id == region.id) {
                        self.fault = Some("that map area is already downloaded".to_owned());
                    } else if let Err(err) = self.strike(ui.ctx(), TrailDataMutation::Add(bounds)) {
                        self.fault = Some(format!("{err:#}"));
                        self.scribe.arm();
                    } else {
                        self.regions.push(region);
                    }
                }
            }
        }
    }

    fn strike(&mut self, ctx: &egui::Context, mutation: TrailDataMutation) -> Result<()> {
        self.fault = None;
        "Updating trails…".clone_into(&mut self.corpus_status);
        self.corpus = Some(TrailData::spawn(ctx.clone(), self.root.clone(), mutation)?);
        Ok(())
    }

    fn absorb_corpus(&mut self, action: &mut Option<WorkspaceAction>) {
        let Some(corpus) = &self.corpus else {
            return;
        };
        let mut finished = false;
        while let Ok(event) = corpus.events.try_recv() {
            match event {
                TrailDataEvent::Progress(event) => self.corpus_status = progress_status(&event),
                TrailDataEvent::Ready(Some(summary)) => {
                    self.corpus_status =
                        format!("Trail data ready in {} map area(s).", summary.regions.len());
                    self.regions = summary.regions;
                    self.fault = None;
                    *action = Some(WorkspaceAction::Reload);
                    finished = true;
                }
                TrailDataEvent::Ready(None) => {
                    self.regions.clear();
                    "No map areas downloaded.".clone_into(&mut self.corpus_status);
                    self.fault = None;
                    finished = true;
                }
                TrailDataEvent::Fault(fault) => {
                    "Trail update failed.".clone_into(&mut self.corpus_status);
                    self.fault = Some(fault);
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

    fn snapshot(&self) -> Slate {
        let mut slate = self.observed_slate.clone();
        slate.project.clone_from(&self.root);
        slate.viewport = Some(self.viewport);
        slate
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
                self.fault = Some(format!("could not save workbench state: {err:#}"));
                self.slate_dirty = Some(Instant::now());
                ctx.request_repaint_after(STATE_SETTLE);
            }
        }
    }
}

impl Drop for SurveyWorkbench {
    fn drop(&mut self) {
        let current = self.snapshot();
        if current != self.committed_slate
            && let Err(err) = current.save(&self.slate_path)
        {
            eprintln!("could not save survey workbench state: {err:#}");
        }
    }
}

pub struct ProjectDeck {
    habitat: Habitat,
    offline: bool,
    new_name: String,
    new_parent: String,
    open_root: String,
    fault: Option<String>,
    return_workspace: Option<ProjectWorkspace>,
    known: Vec<ProjectPlace>,
    water: Surface,
}

impl ProjectDeck {
    fn new(
        habitat: Habitat,
        offline: bool,
        proposed: Option<&Path>,
        mut fault: Option<String>,
        return_workspace: Option<ProjectWorkspace>,
    ) -> Self {
        let new_parent = habitat
            .library_root()
            .map_or_else(String::new, |root| root.to_string_lossy().into_owned());
        let known = habitat.known_projects().unwrap_or_else(|err| {
            fault = Some(format!("could not inspect the project library: {err:#}"));
            Vec::new()
        });
        Self {
            habitat,
            offline,
            new_name: String::new(),
            new_parent,
            open_root: proposed
                .map_or_else(String::new, |root| root.to_string_lossy().into_owned()),
            fault,
            return_workspace,
            known,
            water: forge_water(),
        }
    }

    fn pulse(&mut self, ui: &mut egui::Ui) -> Option<ProjectWorkspace> {
        let mut action = self.return_workspace.as_ref().and_then(|_| {
            ui.ctx()
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
                .then_some(ProjectAction::Back)
        });
        let _center = egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.add_space(((ui.available_height() - 650.0) * 0.42).max(14.0));
            let _row = ui.horizontal(|ui| {
                ui.add_space(((ui.available_width() - 760.0) * 0.5).max(12.0));
                let _plate = egui::Frame::new()
                    .fill(chrome::SURFACE)
                    .stroke(Stroke::new(1.0_f32, chrome::EDGE_STRONG))
                    .corner_radius(2)
                    .inner_margin(egui::Margin::same(24))
                    .show(ui, |ui| self.plate(ui, &mut action));
            });
        });
        action.and_then(|action| self.attempt(ui.ctx(), action))
    }

    fn plate(&mut self, ui: &mut egui::Ui, action: &mut Option<ProjectAction>) {
        let _column = ui.vertical(|ui| {
            ui.set_width(710.0);
            self.heading(ui);
            self.new_project(ui, action);
            self.open_project(ui, action);
            self.known_projects(ui, action);
            self.footnotes(ui);
        });
    }

    fn heading(&self, ui: &mut egui::Ui) {
        let _eyebrow = ui.label(chrome::eyebrow("PROJECT DECK"));
        let title = if self.return_workspace.is_some() {
            "SWITCH TRAIL PROJECT"
        } else {
            "TRAIL PROJECTS"
        };
        let _title = ui.label(chrome::title(title));
        ui.add_space(7.0);
        let _copy = ui.add(
            egui::Label::new(
                RichText::new("CREATE A TRAIL PROJECT OR OPEN ONE ALREADY ON DISK.")
                    .color(chrome::MUTED),
            )
            .wrap(),
        );
        ui.add_space(14.0);
    }

    fn new_project(&mut self, ui: &mut egui::Ui, action: &mut Option<ProjectAction>) {
        let _label = ui.label(chrome::eyebrow("NEW PROJECT"));
        ui.add_space(3.0);
        let name = ui.add_sized(
            [ui.available_width(), 28.0],
            egui::TextEdit::singleline(&mut self.new_name)
                .hint_text("project name · Harriman loops")
                .text_color(chrome::TEXT),
        );
        chrome::tension(ui, &name);
        ui.add_space(5.0);
        let _parent = ui.horizontal(|ui| {
            let edit = ui.add_sized(
                [552.0, 28.0],
                egui::TextEdit::singleline(&mut self.new_parent)
                    .hint_text("parent folder")
                    .text_color(chrome::TEXT),
            );
            chrome::tension(ui, &edit);
            let browse = ui.add_sized([142.0, 28.0], chrome::command_button("BROWSE…", false));
            chrome::tension(ui, &browse);
            if browse.clicked()
                && let Some(parent) = self.pick_parent()
            {
                self.new_parent = parent.to_string_lossy().into_owned();
                self.fault = None;
            }
        });
        ui.add_space(6.0);
        let target = self.new_project_root();
        let ready = target.is_some();
        let create = ui.add_enabled(
            ready,
            chrome::command_button("CREATE PROJECT", true).min_size(vec2(245.0, 34.0)),
        );
        let create =
            create.on_disabled_hover_text("Enter a project name and choose a parent folder.");
        chrome::tension(ui, &create);
        if create.clicked()
            || (ready && name.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)))
        {
            *action = Some(ProjectAction::Create {
                root: target.expect("enabled project creation has a target"),
                name: self.new_name.trim().to_owned(),
            });
        }
        if let Some(target) = self.new_project_root() {
            let _target = chrome::note(ui, format!("NEW FOLDER · {}", target.display()));
        }
    }

    fn open_project(&mut self, ui: &mut egui::Ui, action: &mut Option<ProjectAction>) {
        ui.add_space(16.0);
        let _label = ui.label(chrome::eyebrow("OPEN EXISTING PROJECT"));
        ui.add_space(3.0);
        let _path = ui.horizontal(|ui| {
            let edit = ui.add_sized(
                [552.0, 28.0],
                egui::TextEdit::singleline(&mut self.open_root)
                    .hint_text("folder containing trailgen.toml")
                    .text_color(chrome::TEXT),
            );
            chrome::tension(ui, &edit);
            if edit.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                *action = Some(ProjectAction::Open(PathBuf::from(self.open_root.trim())));
            }
            let browse = ui.add_sized([142.0, 28.0], chrome::command_button("BROWSE…", false));
            chrome::tension(ui, &browse);
            if browse.clicked()
                && let Some(root) = self.pick_project()
            {
                self.open_root = root.to_string_lossy().into_owned();
                self.fault = None;
            }
        });
        ui.add_space(6.0);
        let _actions = ui.horizontal(|ui| {
            let open = ui.add_enabled(
                !self.open_root.trim().is_empty(),
                chrome::command_button("OPEN PROJECT", true).min_size(vec2(210.0, 34.0)),
            );
            let open = open.on_disabled_hover_text("Choose a project folder first.");
            chrome::tension(ui, &open);
            if open.clicked() {
                *action = Some(ProjectAction::Open(PathBuf::from(self.open_root.trim())));
            }
            if self.return_workspace.is_some() {
                let back =
                    ui.add(chrome::command_button("←  BACK", false).min_size(vec2(150.0, 34.0)));
                chrome::tension(ui, &back);
                if back.clicked() {
                    *action = Some(ProjectAction::Back);
                }
            }
        });
        if let Some(fault) = &self.fault {
            fault_label(ui, fault);
        }
    }

    fn known_projects(&self, ui: &mut egui::Ui, action: &mut Option<ProjectAction>) {
        if self.known.is_empty() {
            return;
        }
        ui.add_space(16.0);
        let _known = ui.label(chrome::eyebrow("KNOWN PROJECTS"));
        ui.add_space(3.0);
        for project in &self.known {
            let response = ui
                .add_sized(
                    [ui.available_width(), 31.0],
                    chrome::command_button(project.name.to_ascii_uppercase(), false),
                )
                .on_hover_text(project.root.display().to_string());
            chrome::tension(ui, &response);
            if response.clicked() {
                *action = Some(ProjectAction::Open(project.root.clone()));
            }
        }
    }

    fn footnotes(&self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        let library = self.habitat.library_root().map_or_else(
            || "OS DOCUMENTS DIRECTORY UNAVAILABLE · CHOOSE A PARENT FOLDER".to_owned(),
            |root| format!("CONVENTIONAL LIBRARY · {}", root.display()),
        );
        let _library = chrome::note(ui, library);
        let _shortcut = chrome::note(ui, "CTRL+O OPENS THIS DECK FROM A PROJECT");
    }

    fn new_project_root(&self) -> Option<PathBuf> {
        let parent = self.new_parent.trim();
        let slug = project_slug(self.new_name.trim())?;
        (!parent.is_empty()).then(|| Path::new(parent).join(slug))
    }

    fn pick_parent(&self) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new().set_title("Choose New Project Parent");
        if let Some(parent) = nearest_existing(Path::new(self.new_parent.trim())) {
            dialog = dialog.set_directory(parent);
        }
        dialog.pick_folder()
    }

    fn pick_project(&self) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new().set_title("Open Trailgen Project");
        let proposed = Path::new(self.open_root.trim());
        if let Some(parent) = nearest_existing(proposed) {
            dialog = dialog.set_directory(parent);
        } else if let Some(library) = self.habitat.library_root()
            && let Some(parent) = nearest_existing(library)
        {
            dialog = dialog.set_directory(parent);
        }
        dialog.pick_folder()
    }

    fn attempt(&mut self, ctx: &egui::Context, action: ProjectAction) -> Option<ProjectWorkspace> {
        if matches!(action, ProjectAction::Back) {
            return self.return_workspace.take();
        }
        let result = (|| {
            let root = match action {
                ProjectAction::Create { root, name } => create_project(&root, &name)?,
                ProjectAction::Open(root) => {
                    ensure!(!root.as_os_str().is_empty(), "choose a project folder");
                    root
                }
                ProjectAction::Back => unreachable!("back handled before opening a project"),
            };
            open_project(ctx, &self.habitat, &root, self.offline)
        })();
        match result {
            Ok(workspace) => Some(workspace),
            Err(err) => {
                self.fault = Some(format!("{err:#}"));
                None
            }
        }
    }
}

enum ProjectAction {
    Create { root: PathBuf, name: String },
    Open(PathBuf),
    Back,
}

fn open_project(
    ctx: &egui::Context,
    habitat: &Habitat,
    root: &Path,
    offline: bool,
) -> Result<ProjectWorkspace> {
    let root = root
        .canonicalize()
        .with_context(|| format!("open project {}", root.display()))?;
    let place = ProjectPlace::read(root.clone())?;
    let has_graph = root.join("routes/generated.graph.json").is_file()
        || root.join("cache/graph.json").is_file();
    let config = trailgen_data::project_config(&root)?;
    let indexed = if config.managed {
        trailgen_data::indexed_summary(&root)?
    } else {
        None
    };
    let trail_ready = trail_workspace_ready(has_graph, config.managed, indexed.is_some());
    let workspace = if trail_ready {
        ProjectWorkspace::Trail(Box::new(TrailApp::open(
            ctx,
            &root,
            offline,
            habitat.slate_path(&root),
            config,
            indexed.as_ref(),
        )?))
    } else {
        ProjectWorkspace::Survey(Box::new(SurveyWorkbench::new(
            ctx,
            place,
            offline,
            habitat.slate_path(&root),
        )?))
    };
    if let Err(err) = habitat.remember(&root) {
        eprintln!("could not remember project: {err:#}");
    }
    Ok(workspace)
}

const fn trail_workspace_ready(has_graph: bool, managed: bool, indexed: bool) -> bool {
    if managed { indexed } else { has_graph }
}

fn project_slug(name: &str) -> Option<String> {
    let mut slug = String::with_capacity(name.len());
    let mut separated = true;
    for character in name.chars() {
        if character.is_alphanumeric() {
            slug.extend(character.to_lowercase());
            separated = false;
        } else if !separated && matches!(character, ' ' | '-' | '_') {
            slug.push('-');
            separated = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    (!slug.is_empty()).then_some(slug)
}

fn nearest_existing(path: &Path) -> Option<&Path> {
    path.ancestors().find(|ancestor| ancestor.is_dir())
}

fn fault_label(ui: &mut egui::Ui, fault: &str) {
    ui.add_space(8.0);
    let _fault = ui.add(
        egui::Label::new(
            RichText::new(fault.to_ascii_uppercase())
                .size(11.0)
                .color(Color32::from_rgb(203, 113, 91)),
        )
        .wrap(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_collapse_to_safe_stable_folders() {
        assert_eq!(
            project_slug("Harriman West Loop"),
            Some("harriman-west-loop".into())
        );
        assert_eq!(project_slug("  雪 山  "), Some("雪-山".into()));
        assert_eq!(project_slug("../"), None);
    }

    #[test]
    fn an_excised_managed_corpus_cannot_resurrect_a_generated_snapshot() {
        assert!(!trail_workspace_ready(true, true, false));
        assert!(trail_workspace_ready(true, false, false));
        assert!(trail_workspace_ready(true, true, true));
    }
}
