use crate::{
    ProjectIntent,
    app::{Action as TrailAction, ReloadFrame, TrailApp, forge_water},
    basemap::Source as BasemapSource,
    chrome,
    commands::{self, Context as CommandContext, Edict},
    habitat::{Habitat, ProjectPlace, create_project},
    live_area::{self, RegionHandles, RegionScribe, ResizeEvent, ScribeEvent},
    map::{self, Viewport},
    preferences::{BASE_PACE_SETTING, MAX_BASE_PACE_KMH, MIN_BASE_PACE_KMH, Preferences},
    slate::Slate,
    trail_data::{
        Event as TrailDataEvent, Mutation as TrailDataMutation, TrailData, progress_status,
    },
    vector_field::VectorField,
};
use anyhow::{Context as _, Result, ensure};
use brass_poolrooms::water::{Frame as WaterFrame, Surface};
use egui::{Color32, RichText, Stroke, vec2};
use eternalist_apps::{
    ApplicationHeader, Inspector, LivingWait, ScribeOutcome, SettledScribe,
    command_guide::CommandGuide,
    commands::{CommandDispatch, CommandStatus},
    configuration::ConfigurationLedger,
    panel_navigation::PanelNavigator,
    responsiveness::{Drain, DrainBudget},
    settings::{SettingsFile, SettingsSheet},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use trailgen_contract::Target;
use trailgen_core::Coord;
use trailgen_data::SurveyRegion;

const EVENT_DRAIN: DrainBudget = DrainBudget::new(64, Duration::from_millis(3));
const CONFIGURATION_SETTLE: Duration = Duration::from_millis(400);

pub struct Workbench {
    mode: WorkbenchMode,
    transition: Option<WorkbenchTransition>,
    configuration: ConfigurationLedger<Preferences>,
    settings: SettingsSheet,
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
    ) -> Result<Self> {
        let configuration = ConfigurationLedger::raise(
            "trailgen-configuration-scribe",
            ctx,
            habitat.preferences_path(),
            CONFIGURATION_SETTLE,
        )?;
        let mut settings = SettingsSheet::default();
        if configuration.fault().is_some() {
            settings.require_attention(ctx);
        }
        let mode = 'mode: {
            let candidate = match intent {
                ProjectIntent::Open(root) => Some(root),
                ProjectIntent::Resume => match habitat.resume() {
                    Ok(candidate) => candidate,
                    Err(err) => {
                        break 'mode WorkbenchMode::Projects(Box::new(ProjectDeck::new(
                            habitat,
                            offline,
                            None,
                            Some(format!("could not read the previous project: {err:#}")),
                            None,
                        )));
                    }
                },
            };
            let Some(root) = candidate else {
                break 'mode WorkbenchMode::Projects(Box::new(ProjectDeck::new(
                    habitat, offline, None, None, None,
                )));
            };
            match open_project(ctx, &habitat, &root, offline) {
                Ok(workspace) => WorkbenchMode::Project {
                    workspace,
                    habitat,
                    offline,
                },
                Err(err) => WorkbenchMode::Projects(Box::new(ProjectDeck::new(
                    habitat,
                    offline,
                    Some(&root),
                    Some(format!("could not open that project: {err:#}")),
                    None,
                ))),
            }
        };
        Ok(Self::still(mode, configuration, settings))
    }

    pub fn pulse(&mut self, ui: &mut egui::Ui) {
        if self.configuration.absorb() {
            self.mode
                .configuration_changed(self.configuration.live().base_pace());
        }
        if self.configuration.fault().is_some() {
            self.settings.require_attention(ui.ctx());
        }
        let settings_invoked = self.settings.take_shortcut(ui.ctx());
        if settings_invoked && self.settings.is_open() {
            self.reload_configuration();
        }
        let settings_attention = self.configuration.fault().is_some();
        let configuration = &mut self.configuration;
        let settings = &mut self.settings;
        self.transition = match &mut self.mode {
            WorkbenchMode::Project {
                workspace,
                habitat,
                offline,
            } => match workspace.pulse(ui, configuration, settings, settings_attention) {
                None => None,
                Some(WorkspaceAction::Projects) => Some(WorkbenchTransition::Projects),
                Some(WorkspaceAction::Reload) => {
                    let root = workspace.root().to_owned();
                    let frame = workspace.reload_frame();
                    match open_project(ui.ctx(), habitat, &root, *offline) {
                        Ok(mut workspace) => {
                            workspace.restore_reload_frame(frame);
                            Some(WorkbenchTransition::Project {
                                workspace,
                                habitat: habitat.clone(),
                                offline: *offline,
                            })
                        }
                        Err(err) => {
                            workspace.set_fault(format!("could not open this project: {err:#}"));
                            None
                        }
                    }
                }
            },
            WorkbenchMode::Projects(deck) => {
                deck.pulse(ui, settings, settings_attention)
                    .map(|workspace| WorkbenchTransition::Project {
                        workspace,
                        habitat: deck.habitat.clone(),
                        offline: deck.offline,
                    })
            }
            WorkbenchMode::Limbo => unreachable!("workbench transition escaped its pulse"),
        };
        self.show_settings(ui.ctx());
    }

    pub fn window_title(&self) -> String {
        match &self.mode {
            WorkbenchMode::Project { workspace, .. } => workspace.window_title(),
            WorkbenchMode::Projects(_) => "trailgen · trail projects".to_owned(),
            WorkbenchMode::Limbo => "trailgen".to_owned(),
        }
    }

    pub fn settle(&mut self) -> bool {
        let changed = self.transition.is_some();
        if changed {
            self.commit_transition();
        }
        changed
    }

    pub fn next_service_deadline(&self, now: Instant) -> Option<Instant> {
        let mode = match &self.mode {
            WorkbenchMode::Project { workspace, .. } => workspace.service_deadline(now),
            WorkbenchMode::Projects(_) | WorkbenchMode::Limbo => None,
        };
        mode.into_iter().chain(self.configuration.deadline()).min()
    }

    pub fn service_reached(&mut self, now: Instant) -> bool {
        let changed = self.configuration.service_deadline_reached(now);
        if changed {
            self.mode
                .configuration_changed(self.configuration.live().base_pace());
        }
        let mode_changed = match &mut self.mode {
            WorkbenchMode::Project { workspace, .. } => workspace.service_deadline_reached(now),
            WorkbenchMode::Projects(_) | WorkbenchMode::Limbo => false,
        };
        changed | mode_changed
    }

    const fn still(
        mode: WorkbenchMode,
        configuration: ConfigurationLedger<Preferences>,
        settings: SettingsSheet,
    ) -> Self {
        Self {
            mode,
            transition: None,
            configuration,
            settings,
        }
    }

    fn reload_configuration(&mut self) {
        if self.configuration.fault().is_some() || self.configuration.settled() {
            let _requested = self.configuration.request_reload();
        }
    }

    fn show_settings(&mut self, ctx: &egui::Context) {
        let path = self.configuration.path().to_owned();
        let fault = self.configuration.fault().map(ToString::to_string);
        let mut base_pace = self.configuration.live().base_pace().kmh();
        let file = fault.as_deref().map_or_else(
            || SettingsFile::ready(&path),
            |message| SettingsFile::fault(&path, message),
        );
        let file = file
            .reloading(self.configuration.reload_pending())
            .reloadable(self.configuration.fault().is_some() || self.configuration.settled());
        let mut changed = false;
        let response = self
            .settings
            .show(ctx, self.mode.water_mut(), file, |settings| {
                settings.section("CALIBRATION");
                changed |= settings.number(
                    BASE_PACE_SETTING,
                    &mut base_pace,
                    MIN_BASE_PACE_KMH..=MAX_BASE_PACE_KMH,
                    0.1,
                    1,
                );
            });
        if changed
            && self
                .configuration
                .revise(|preferences| preferences.set_base_pace(base_pace))
                .is_ok()
        {
            self.mode
                .configuration_changed(self.configuration.live().base_pace());
        }
        if response.reload_requested() {
            self.reload_configuration();
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
        let mut state = match &self.mode {
            WorkbenchMode::Project { workspace, .. } => match workspace {
                ProjectWorkspace::Trail(app) => app.witness_state(text_edit_focused),
                ProjectWorkspace::Survey(project) => project.witness_state(text_edit_focused),
            },
            WorkbenchMode::Projects(deck) => {
                let mut state = crate::witness::State::empty(
                    trailgen_contract::Workspace::Projects,
                    trailgen_contract::View::Projects,
                    text_edit_focused,
                );
                state.guide_open = deck.guide.is_open();
                state
            }
            WorkbenchMode::Limbo => unreachable!("workbench transition escaped its witness"),
        };
        state.base_pace_kmh = Some(self.configuration.live().base_pace().kmh());
        state.settings = crate::witness::Settings {
            open: self.settings.is_open(),
            fault: self.configuration.fault().is_some(),
            settled: self.configuration.settled(),
        };
        state
    }
}

impl ProjectWorkspace {
    fn pulse(
        &mut self,
        ui: &mut egui::Ui,
        configuration: &mut ConfigurationLedger<Preferences>,
        settings: &mut SettingsSheet,
        settings_attention: bool,
    ) -> Option<WorkspaceAction> {
        match self {
            Self::Trail(app) => app
                .pulse(ui, configuration, settings, settings_attention)
                .map(|action| match action {
                    TrailAction::Projects => WorkspaceAction::Projects,
                    TrailAction::Reload => WorkspaceAction::Reload,
                }),
            Self::Survey(project) => project.pulse(ui, settings, settings_attention),
        }
    }

    fn water_mut(&mut self) -> &mut Surface {
        match self {
            Self::Trail(app) => app.water_mut(),
            Self::Survey(project) => &mut project.water,
        }
    }

    fn configuration_changed(&mut self, base_pace: crate::preferences::BasePace) {
        if let Self::Trail(app) = self {
            app.set_base_pace(base_pace);
        }
    }

    fn root(&self) -> &Path {
        match self {
            Self::Trail(app) => app.root(),
            Self::Survey(project) => &project.root,
        }
    }

    fn set_fault(&mut self, fault: String) {
        match self {
            Self::Survey(project) => project.fault = Some(fault),
            Self::Trail(_) => {}
        }
    }

    fn reload_frame(&self) -> ReloadFrame {
        match self {
            Self::Trail(app) => app.reload_frame(),
            Self::Survey(project) => ReloadFrame::browse(project.viewport),
        }
    }

    fn restore_reload_frame(&mut self, frame: ReloadFrame) {
        match self {
            Self::Trail(app) => app.restore_reload_frame(frame),
            Self::Survey(project) => project.viewport = frame.viewport(),
        }
    }

    fn window_title(&self) -> String {
        match self {
            Self::Trail(app) => app.window_title(),
            Self::Survey(project) => format!("{} · trailgen", project.name),
        }
    }

    fn service_deadline(&self, now: Instant) -> Option<Instant> {
        match self {
            Self::Trail(app) => app.service_deadline(now),
            Self::Survey(project) => project.service_deadline(),
        }
    }

    fn service_deadline_reached(&mut self, now: Instant) -> bool {
        match self {
            Self::Trail(app) => app.service_deadline_reached(now),
            Self::Survey(project) => project.service_deadline_reached(now),
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
            Self::Survey(project) => project.water_frame(ctx, pixels_per_point, tooltip_rects),
        }
    }
}

impl WorkbenchMode {
    fn water_mut(&mut self) -> &mut Surface {
        match self {
            Self::Project { workspace, .. } => workspace.water_mut(),
            Self::Projects(deck) => &mut deck.water,
            Self::Limbo => unreachable!("workbench transition escaped its water"),
        }
    }

    fn configuration_changed(&mut self, base_pace: crate::preferences::BasePace) {
        match self {
            Self::Project { workspace, .. } => workspace.configuration_changed(base_pace),
            Self::Projects(deck) => {
                if let Some(workspace) = &mut deck.return_workspace {
                    workspace.configuration_changed(base_pace);
                }
            }
            Self::Limbo => {}
        }
    }
}

const STATE_SETTLE: Duration = Duration::from_millis(400);

struct SurveyWorkbench {
    root: PathBuf,
    name: String,
    regions: Vec<SurveyRegion>,
    region_names: BTreeMap<String, String>,
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
    area_handles: RegionHandles,
    guide: CommandGuide,
    panels: PanelNavigator,
    observed_slate: Slate,
    state_scribe: SettledScribe<Slate>,
    water: Surface,
    living_wait: LivingWait,
    map_rect: egui::Rect,
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
        let scribe_path = slate_path;
        let state_scribe = SettledScribe::spawn(
            "trailgen-survey-state-scribe",
            ctx,
            STATE_SETTLE,
            move |slate: Slate| slate.save(&scribe_path),
        )?;
        let mut project = Self {
            root: place.root,
            name: place.name,
            regions: config.regions,
            region_names: config.region_names,
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
            area_handles: RegionHandles::default(),
            guide: CommandGuide::default(),
            panels: PanelNavigator::default(),
            observed_slate: slate,
            state_scribe,
            water: forge_water(),
            living_wait: LivingWait::default(),
            map_rect: egui::Rect::ZERO,
        };
        if !offline && !project.regions.is_empty() {
            project.strike(ctx, TrailDataMutation::Refresh)?;
        }
        Ok(project)
    }

    fn pulse(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut SettingsSheet,
        settings_attention: bool,
    ) -> Option<WorkspaceAction> {
        let mut drain = EVENT_DRAIN.arm();
        self.absorb_persistence();
        self.vector.absorb(ui.ctx());
        let guide_invoked = self.guide.take_shortcuts(ui.ctx());
        let mut action = None;
        if !guide_invoked
            && !self.guide.is_open()
            && let Some(dispatch) =
                commands::canon().route(ui.ctx(), &[CommandContext::Survey], |edict| {
                    self.edict_status(edict)
                })
        {
            self.apply_edict(ui.ctx(), dispatch, &mut action);
        }
        if !guide_invoked
            && !self.guide.is_open()
            && ui.ctx().memory(|memory| memory.top_modal_layer().is_none())
            && ui
                .ctx()
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.scribe.disarm();
            self.area_handles.cancel();
        }
        self.absorb_corpus(ui.ctx(), &mut drain, &mut action);
        let mut panels = std::mem::take(&mut self.panels);
        let inspector = Inspector::new("survey-inspector").show(ui, |ui| {
            self.inspector(ui, &mut panels, &mut action, settings, settings_attention);
        });
        self.panels = panels;
        inspector.agitate(&mut self.water);
        let _center = egui::CentralPanel::default().show(ui, |ui| self.arena(ui));
        self.command_guide(ui);
        self.observe_persistence();
        action
    }

    const fn edict_status(&self, edict: Edict) -> CommandStatus<'static> {
        match edict {
            Edict::DrawMapArea if self.scribe.active() => {
                CommandStatus::Disabled("a map-area drawing gesture is already armed")
            }
            Edict::DrawMapArea if self.offline => {
                CommandStatus::Disabled("map areas cannot be downloaded while offline")
            }
            Edict::RefreshMapAreas if self.regions.is_empty() => CommandStatus::Hidden,
            Edict::RefreshMapAreas if self.offline => {
                CommandStatus::Disabled("map areas cannot be refreshed while offline")
            }
            Edict::DrawMapArea | Edict::RefreshMapAreas if self.corpus.is_some() => {
                CommandStatus::Disabled("wait for the current trail update to finish")
            }
            Edict::CreateProject
            | Edict::OpenProject
            | Edict::FindTrails
            | Edict::StopSearch
            | Edict::ToggleFinder
            | Edict::BeginManual
            | Edict::UndoSearchEdit
            | Edict::RedoSearchEdit
            | Edict::EditTrail
            | Edict::SaveCandidate
            | Edict::RenameFocused
            | Edict::DiscardTrailEdit
            | Edict::UndoTrailEdit
            | Edict::RedoTrailEdit
            | Edict::SaveTrail
            | Edict::RenameEditor => CommandStatus::Hidden,
            Edict::OpenProjects | Edict::DrawMapArea | Edict::RefreshMapAreas => {
                CommandStatus::Enabled
            }
        }
    }

    fn apply_edict(
        &mut self,
        ctx: &egui::Context,
        dispatch: CommandDispatch<'_, Edict>,
        action: &mut Option<WorkspaceAction>,
    ) {
        let edict = match dispatch {
            CommandDispatch::Invoke(edict) => edict,
            CommandDispatch::Refused { reason, .. } => {
                self.fault = Some(format!("Unavailable: {reason}."));
                return;
            }
        };
        match edict {
            Edict::OpenProjects => *action = Some(WorkspaceAction::Projects),
            Edict::DrawMapArea => {
                self.area_handles.cancel();
                self.scribe.arm();
            }
            Edict::RefreshMapAreas => {
                if let Err(error) = self.strike(ctx, TrailDataMutation::Refresh) {
                    self.fault = Some(format!("{error:#}"));
                }
            }
            _ => self.fault = Some("That command belongs to another Trailgen workspace.".into()),
        }
    }

    fn command_guide(&mut self, ui: &egui::Ui) {
        let mut guide = std::mem::take(&mut self.guide);
        guide.show(
            ui.ctx(),
            commands::canon(),
            &[CommandContext::Survey],
            commands::scope_name,
            |edict| self.edict_status(edict),
            &commands::SURVEY_IDIOMS,
        );
        if let Some(rect) = guide.rect() {
            crate::witness::rect(ui.ctx(), Target::CommandGuide, rect);
        }
        self.guide = guide;
    }

    fn inspector(
        &mut self,
        ui: &mut egui::Ui,
        navigator: &mut PanelNavigator,
        action: &mut Option<WorkspaceAction>,
        settings: &mut SettingsSheet,
        settings_attention: bool,
    ) {
        let header = ApplicationHeader::new("TRAILGEN")
            .settings_attention(settings_attention)
            .show(ui, &mut self.guide, settings, &mut self.water);
        crate::witness::response(ui, Target::Help, &header.help);
        ui.add_space(5.0);
        let mut panels = navigator.frame(ui.ctx());
        let projects = panels.section(ui, "projects", "projects", true, |ui| {
            let _name = ui.label(chrome::eyebrow(self.name.to_ascii_uppercase()));
            ui.add_space(4.0);
            let spec = commands::canon().spec(Edict::OpenProjects);
            let projects = ui
                .add(
                    chrome::command_spec_button(ui, spec, false)
                        .min_size(vec2(ui.available_width(), 27.0)),
                )
                .on_hover_text(format!(
                    "{} · {}",
                    spec.detail(),
                    commands::canon().shortcuts(Edict::OpenProjects)[0].label(ui.ctx())
                ));
            chrome::tension(ui, &projects);
            if chrome::exact_activation(ui, &projects) {
                self.apply_edict(
                    ui.ctx(),
                    CommandDispatch::Invoke(Edict::OpenProjects),
                    action,
                );
                self.water.click(projects.rect);
            }
        });
        crate::witness::response(ui, Target::Panel("projects"), &projects.header);
        self.water.fold(projects.wake);
        let section = panels.section(ui, "areas", "map areas", true, |ui| {
            self.area_panel(ui);
        });
        crate::witness::response(ui, Target::Panel("areas"), &section.header);
        self.water.fold(section.wake);
    }

    fn area_panel(&mut self, ui: &mut egui::Ui) {
        let selecting = self.scribe.active();
        let draw_spec = commands::canon().spec(Edict::DrawMapArea);
        let select = ui.add_enabled(
            !self.offline && self.corpus.is_none(),
            if selecting {
                chrome::command_button("CANCEL DRAWING", true)
            } else {
                chrome::command_spec_button(ui, draw_spec, false)
            }
            .min_size(vec2(ui.available_width(), 34.0)),
        );
        let select = if selecting {
            select.on_hover_text("Cancel drawing this map area · Esc")
        } else {
            select
        };
        crate::witness::anchor(ui, Target::SurveyAddArea, select.rect);
        chrome::tension(ui, &select);
        if chrome::exact_activation(ui, &select) {
            if selecting {
                self.scribe.disarm();
            } else {
                let mut action = None;
                self.apply_edict(
                    ui.ctx(),
                    CommandDispatch::Invoke(Edict::DrawMapArea),
                    &mut action,
                );
            }
            self.water.click(select.rect);
        }
        ui.add_space(7.0);
        let _count = chrome::note(ui, format!("{} DOWNLOADED AREA(S)", self.regions.len()));
        let mut excision = None;
        for (slot, region) in self.regions.iter().enumerate() {
            let _region = ui.horizontal(|ui| {
                let name = self.region_names.get(&region.id).map_or_else(
                    || format!("AREA {slot:02}"),
                    |name| name.to_ascii_uppercase(),
                );
                let _area = ui.label(chrome::muted(name));
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
            let spec = commands::canon().spec(Edict::RefreshMapAreas);
            let refresh = ui.add_enabled(
                !self.offline && self.corpus.is_none(),
                chrome::command_spec_button(ui, spec, false)
                    .min_size(vec2(ui.available_width(), 27.0)),
            );
            chrome::tension(ui, &refresh);
            if chrome::exact_activation(ui, &refresh) {
                let mut action = None;
                self.apply_edict(
                    ui.ctx(),
                    CommandDispatch::Invoke(Edict::RefreshMapAreas),
                    &mut action,
                );
            }
        }
        if let Some(fault) = &self.fault {
            fault_label(ui, fault);
        }
    }

    fn arena(&mut self, ui: &mut egui::Ui) {
        let _counsel = egui::Panel::bottom("survey-counsel")
            .exact_size(52.0)
            .show(ui, |ui| self.counsel(ui));
        let _map = egui::CentralPanel::default().show(ui, |ui| self.map(ui));
    }

    fn counsel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        let _row = ui.horizontal(|ui| {
            let waiting = self.corpus.is_some();
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
            let message = ui.add(
                egui::Label::new(RichText::new(message).monospace().color(chrome::TEXT)).wrap(),
            );
            if waiting {
                let rect = message.rect.expand(5.0);
                self.living_wait.claim(rect);
                crate::witness::anchor(ui, Target::TrailDataWait, rect);
            }
            if self.corpus.is_none() && !self.scribe.active() {
                let select = ui.add_enabled(
                    !self.offline,
                    chrome::command_spec_button(
                        ui,
                        commands::canon().spec(Edict::DrawMapArea),
                        true,
                    )
                    .min_size(vec2(164.0, 29.0)),
                );
                chrome::tension(ui, &select);
                if chrome::exact_activation(ui, &select) {
                    let mut action = None;
                    self.apply_edict(
                        ui.ctx(),
                        CommandDispatch::Invoke(Edict::DrawMapArea),
                        &mut action,
                    );
                    self.water.click(select.rect);
                }
            }
        });
    }

    fn water_frame(
        &mut self,
        ctx: &egui::Context,
        pixels_per_point: f32,
        tooltip_rects: &[egui::Rect],
    ) -> WaterFrame {
        self.living_wait.compose(ctx, &mut self.water);
        self.water.frame(ctx, pixels_per_point, tooltip_rects, None)
    }

    fn map(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        crate::witness::anchor(ui, Target::SurveyMap, response.rect);
        self.map_rect = rect;
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
            .begin(brass_poolrooms::water::Domain::shelf(rect));
        let handles_enabled = self.corpus.is_none() && !self.scribe.active();
        let resize_event =
            self.area_handles
                .interact(self.viewport, ui, rect, &self.regions, handles_enabled);
        if map::navigate_with(
            &mut self.viewport,
            ui,
            &response,
            rect,
            !self.scribe.active() && !self.area_handles.captured(),
            true,
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
            live_area::Scene {
                viewport: self.viewport,
                canvas: rect,
                regions: &self.regions,
                names: &self.region_names,
                preview: self.scribe.preview(self.viewport, rect),
                adjustment: self.area_handles.preview(),
                handles: handles_enabled,
            },
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
        self.handle_resize(ui.ctx(), resize_event);
    }

    fn handle_resize(&mut self, ctx: &egui::Context, event: ResizeEvent) {
        match event {
            ResizeEvent::None => {}
            ResizeEvent::Fault(fault) => self.fault = Some(fault.to_owned()),
            ResizeEvent::Committed { id, before, bounds } => {
                let Some(slot) = self.regions.iter().position(|region| region.id == id) else {
                    self.fault = Some("that map area no longer exists".to_owned());
                    return;
                };
                debug_assert_eq!(self.regions[slot].bounds, before);
                let replacement = match SurveyRegion::new(bounds) {
                    Ok(replacement) => replacement,
                    Err(err) => {
                        self.fault = Some(format!("that map area cannot be resized: {err:#}"));
                        return;
                    }
                };
                if self
                    .regions
                    .iter()
                    .enumerate()
                    .any(|(known, region)| known != slot && region.id == replacement.id)
                {
                    self.fault = Some("that resize would duplicate another map area".to_owned());
                    return;
                }
                if let Err(err) = self.strike(
                    ctx,
                    TrailDataMutation::Replace {
                        id: id.clone(),
                        bounds,
                    },
                ) {
                    self.fault = Some(format!("could not resize that map area: {err:#}"));
                    return;
                }
                self.regions[slot] = replacement.clone();
                if let Some(name) = self.region_names.remove(&id) {
                    let _prior = self.region_names.insert(replacement.id, name);
                }
                "Updating trails for the resized map area…".clone_into(&mut self.corpus_status);
            }
        }
    }

    fn strike(&mut self, ctx: &egui::Context, mutation: TrailDataMutation) -> Result<()> {
        self.fault = None;
        "Updating trails…".clone_into(&mut self.corpus_status);
        self.corpus = Some(TrailData::spawn(ctx, self.root.clone(), mutation)?);
        Ok(())
    }

    #[cfg(feature = "egui-test")]
    fn witness_state(&self, text_edit_focused: bool) -> crate::witness::State {
        let mut state = crate::witness::State::empty(
            trailgen_contract::Workspace::Survey,
            trailgen_contract::View::Browse,
            text_edit_focused,
        );
        state.guide_open = self.guide.is_open();
        state.map = self.map_rect.is_positive().then(|| {
            crate::witness::MapState::forge(
                self.map_rect,
                self.viewport.center,
                map::world_pixels(self.viewport),
                trailgen_contract::TrailColoring::Class,
                self.vector.presented_tile_count(),
                None,
            )
        });
        state.areas = Some(crate::witness::AreaState {
            regions: self.regions.len(),
            drawing: self.scribe.active(),
            resizing: self
                .area_handles
                .resizing()
                .map(|(slot, corner)| crate::witness::AreaResizeState { slot, corner }),
        });
        state.civic = None;
        state.survey = Some(crate::witness::SurveyState {
            acquiring: self.corpus.is_some(),
        });
        state
    }

    fn absorb_corpus(
        &mut self,
        ctx: &egui::Context,
        drain: &mut Drain,
        action: &mut Option<WorkspaceAction>,
    ) {
        let Some(corpus) = &self.corpus else {
            return;
        };
        let mut finished = false;
        while let Some(event) = drain.receive(&corpus.events) {
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
                    self.region_names.clear();
                    "No map areas downloaded.".clone_into(&mut self.corpus_status);
                    self.fault = None;
                    finished = true;
                }
                TrailDataEvent::Fault(fault) => {
                    "Trail update failed.".clone_into(&mut self.corpus_status);
                    self.fault = Some(fault);
                    if let Ok(config) = trailgen_data::project_config(&self.root) {
                        self.regions = config.regions;
                        self.region_names = config.region_names;
                    }
                    finished = true;
                }
            }
        }
        if finished {
            self.corpus = None;
        } else if !corpus.events.is_empty() {
            ctx.request_repaint();
        }
    }

    fn snapshot(&self) -> Slate {
        let mut slate = self.observed_slate.clone();
        slate.project.clone_from(&self.root);
        slate.viewport = Some(self.viewport);
        slate
    }

    fn observe_persistence(&mut self) {
        let current = self.snapshot();
        if current != self.observed_slate {
            self.observed_slate = current;
            self.state_scribe.mark();
        }
    }

    fn service_deadline(&self) -> Option<Instant> {
        self.state_scribe
            .deadline()
            .into_iter()
            .chain(self.vector.service_deadline())
            .min()
    }

    fn service_deadline_reached(&mut self, now: Instant) -> bool {
        let mut changed = self.vector.service_deadline_reached(now);
        if self
            .state_scribe
            .deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            let slate = self.observed_slate.clone();
            match self.state_scribe.tend(now, || slate) {
                Ok(Some(_sequence)) => {}
                Ok(None) => {}
                Err(error) => {
                    self.fault = Some(format!("could not submit workbench state: {error:#}"));
                    changed = true;
                }
            }
        }
        changed
    }

    fn absorb_persistence(&mut self) {
        if let Some(ScribeOutcome::Fault { message, .. }) = self.state_scribe.take_outcome() {
            self.fault = Some(format!("could not save workbench state: {message}"));
        }
    }
}

impl Drop for SurveyWorkbench {
    fn drop(&mut self) {
        let slate = self.snapshot();
        if let Err(error) = self.state_scribe.flush(slate) {
            eprintln!("could not save survey workbench state: {error:#}");
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
    guide: CommandGuide,
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
            guide: CommandGuide::default(),
        }
    }

    fn pulse(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut SettingsSheet,
        settings_attention: bool,
    ) -> Option<ProjectWorkspace> {
        let guide_invoked = self.guide.take_shortcuts(ui.ctx());
        let mut action = if !guide_invoked
            && !self.guide.is_open()
            && let Some(dispatch) =
                commands::canon().route(ui.ctx(), &[CommandContext::Projects], |edict| {
                    self.edict_status(edict)
                }) {
            self.edict_action(dispatch)
        } else {
            None
        };
        if action.is_none() && !guide_invoked && !self.guide.is_open() {
            action = self.return_workspace.as_ref().and_then(|_| {
                ui.ctx()
                    .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
                    .then_some(ProjectAction::Back)
            });
        }
        let _center = egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(((ui.available_height() - 650.0) * 0.42).max(14.0));
            let _row = ui.horizontal(|ui| {
                ui.add_space(((ui.available_width() - 760.0) * 0.5).max(12.0));
                let _plate = egui::Frame::new()
                    .fill(chrome::SURFACE)
                    .stroke(Stroke::new(1.0_f32, chrome::EDGE_STRONG))
                    .corner_radius(2)
                    .inner_margin(egui::Margin::same(24))
                    .show(ui, |ui| {
                        self.plate(ui, &mut action, settings, settings_attention);
                    });
            });
        });
        self.command_guide(ui);
        action.and_then(|action| self.attempt(ui.ctx(), action))
    }

    fn edict_status(&self, edict: Edict) -> CommandStatus<'static> {
        match edict {
            Edict::CreateProject if self.new_project_root().is_some() => CommandStatus::Enabled,
            Edict::CreateProject => {
                CommandStatus::Disabled("enter a project name and choose a parent folder")
            }
            Edict::OpenProject if !self.open_root.trim().is_empty() => CommandStatus::Enabled,
            Edict::OpenProject => CommandStatus::Disabled("choose a project folder first"),
            Edict::OpenProjects
            | Edict::DrawMapArea
            | Edict::RefreshMapAreas
            | Edict::FindTrails
            | Edict::StopSearch
            | Edict::ToggleFinder
            | Edict::BeginManual
            | Edict::UndoSearchEdit
            | Edict::RedoSearchEdit
            | Edict::EditTrail
            | Edict::SaveCandidate
            | Edict::RenameFocused
            | Edict::DiscardTrailEdit
            | Edict::UndoTrailEdit
            | Edict::RedoTrailEdit
            | Edict::SaveTrail
            | Edict::RenameEditor => CommandStatus::Hidden,
        }
    }

    fn edict_action(&mut self, dispatch: CommandDispatch<'_, Edict>) -> Option<ProjectAction> {
        let edict = match dispatch {
            CommandDispatch::Invoke(edict) => edict,
            CommandDispatch::Refused { reason, .. } => {
                self.fault = Some(format!("Unavailable: {reason}."));
                return None;
            }
        };
        match edict {
            Edict::CreateProject => self.new_project_root().map(|root| ProjectAction::Create {
                root,
                name: self.new_name.trim().to_owned(),
            }),
            Edict::OpenProject => Some(ProjectAction::Open(PathBuf::from(self.open_root.trim()))),
            _ => {
                self.fault = Some("That command belongs to another Trailgen workspace.".into());
                None
            }
        }
    }

    fn command_guide(&mut self, ui: &egui::Ui) {
        let mut guide = std::mem::take(&mut self.guide);
        guide.show(
            ui.ctx(),
            commands::canon(),
            &[CommandContext::Projects],
            commands::scope_name,
            |edict| self.edict_status(edict),
            &commands::PROJECT_IDIOMS,
        );
        if let Some(rect) = guide.rect() {
            crate::witness::rect(ui.ctx(), Target::CommandGuide, rect);
        }
        self.guide = guide;
    }

    fn plate(
        &mut self,
        ui: &mut egui::Ui,
        action: &mut Option<ProjectAction>,
        settings: &mut SettingsSheet,
        settings_attention: bool,
    ) {
        let _column = ui.vertical(|ui| {
            ui.set_width(710.0);
            let header = ApplicationHeader::new("TRAILGEN")
                .settings_attention(settings_attention)
                .show(ui, &mut self.guide, settings, &mut self.water);
            crate::witness::response(ui, Target::Help, &header.help);
            ui.add_space(12.0);
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
        crate::witness::anchor(ui, Target::ProjectName, name.rect);
        chrome::tension(ui, &name);
        ui.add_space(5.0);
        let _parent = ui.horizontal(|ui| {
            let edit = ui.add_sized(
                [552.0, 28.0],
                egui::TextEdit::singleline(&mut self.new_parent)
                    .hint_text("parent folder")
                    .text_color(chrome::TEXT),
            );
            crate::witness::anchor(ui, Target::ProjectParent, edit.rect);
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
            chrome::command_spec_button(ui, commands::canon().spec(Edict::CreateProject), true)
                .min_size(vec2(245.0, 34.0)),
        );
        let create = create
            .on_hover_text("Create this project · Enter from the project name")
            .on_disabled_hover_text("Enter a project name and choose a parent folder.");
        crate::witness::anchor(ui, Target::ProjectCreate, create.rect);
        chrome::tension(ui, &create);
        if chrome::exact_activation(ui, &create)
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
            crate::witness::anchor(ui, "projects.open.path", edit.rect);
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
                chrome::command_spec_button(ui, commands::canon().spec(Edict::OpenProject), true)
                    .min_size(vec2(210.0, 34.0)),
            );
            let open = open
                .on_hover_text("Open this project · Enter from the folder field")
                .on_disabled_hover_text("Choose a project folder first.");
            crate::witness::anchor(ui, "projects.open", open.rect);
            chrome::tension(ui, &open);
            if chrome::exact_activation(ui, &open) {
                *action = Some(ProjectAction::Open(PathBuf::from(self.open_root.trim())));
            }
            if self.return_workspace.is_some() {
                let back = ui
                    .add(chrome::command_button("←  BACK", false).min_size(vec2(150.0, 34.0)))
                    .on_hover_text("Return to the open project · Esc");
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
            crate::witness::anchor(
                ui,
                format!("projects.known/{}", project.name),
                response.rect,
            );
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
        let _shortcut = chrome::note(
            ui,
            format!(
                "{} OPENS THIS DECK FROM A PROJECT",
                commands::canon().shortcuts(Edict::OpenProjects)[0].label(ui.ctx())
            )
            .to_ascii_uppercase(),
        );
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
    let place = ProjectPlace::read(&root)?;
    let has_graph = root.join("routes/generated.graph.json").is_file()
        || root.join(trailgen_core::GRAPH_CACHE).is_file();
    let config = trailgen_data::project_config(&root)?;
    let indexed = if config.managed {
        trailgen_data::indexed_summary(&root)?
    } else {
        None
    };
    let trail_ready = trail_workspace_ready(has_graph, config.managed, indexed.is_some());
    let workspace = if trail_ready {
        ProjectWorkspace::Trail(Box::new(TrailApp::raise(
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
    fn an_excised_managed_corpus_cannot_resurrect_a_generated_snapshot() {
        assert!(!trail_workspace_ready(true, true, false));
        assert!(trail_workspace_ready(true, false, false));
        assert!(trail_workspace_ready(true, true, true));
    }
}
