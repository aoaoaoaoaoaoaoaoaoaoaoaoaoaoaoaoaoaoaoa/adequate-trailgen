use crate::{
    ProjectIntent,
    app::{TrailApp, forge_water},
    habitat::{Habitat, ProjectPlace, create_project},
};
use anyhow::{Context as _, Result, ensure};
use dwemer_poolrooms::{
    chrome,
    water::{Frame as WaterFrame, Surface},
};
use egui::{Color32, RichText, Stroke, vec2};
use std::path::{Path, PathBuf};

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
    Empty(Box<EmptyProject>),
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
}

impl ProjectWorkspace {
    fn pulse(&mut self, ui: &mut egui::Ui) -> Option<WorkspaceAction> {
        match self {
            Self::Trail(app) => app.pulse(ui).then_some(WorkspaceAction::Projects),
            Self::Empty(project) => project.pulse(ui),
        }
    }

    fn root(&self) -> &Path {
        match self {
            Self::Trail(app) => app.root(),
            Self::Empty(project) => &project.root,
        }
    }

    fn set_fault(&mut self, fault: String) {
        if let Self::Empty(project) = self {
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
            Self::Empty(project) => project
                .water
                .frame(ctx, pixels_per_point, tooltip_rects, None),
        }
    }
}

struct EmptyProject {
    root: PathBuf,
    name: String,
    fault: Option<String>,
    water: Surface,
}

impl EmptyProject {
    fn new(place: ProjectPlace) -> Self {
        Self {
            root: place.root,
            name: place.name,
            fault: None,
            water: forge_water(),
        }
    }

    fn pulse(&mut self, ui: &mut egui::Ui) -> Option<WorkspaceAction> {
        let shortcut = ui
            .ctx()
            .input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::O));
        let mut action = shortcut.then_some(WorkspaceAction::Projects);
        let _center = egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.add_space(((ui.available_height() - 390.0) * 0.45).max(18.0));
            let _row = ui.horizontal(|ui| {
                ui.add_space(((ui.available_width() - 710.0) * 0.5).max(12.0));
                let _plate = egui::Frame::new()
                    .fill(chrome::SURFACE)
                    .stroke(Stroke::new(1.0_f32, chrome::EDGE_STRONG))
                    .corner_radius(2)
                    .inner_margin(egui::Margin::same(24))
                    .show(ui, |ui| self.plate(ui, &mut action));
            });
        });
        action
    }

    fn plate(&mut self, ui: &mut egui::Ui, action: &mut Option<WorkspaceAction>) {
        let _column = ui.vertical(|ui| {
            ui.set_width(660.0);
            let _eyebrow = ui.label(chrome::eyebrow("EMPTY PROJECT"));
            let _title = ui.label(chrome::title(self.name.to_ascii_uppercase()));
            ui.add_space(8.0);
            let _copy = ui.add(
                egui::Label::new(
                    RichText::new("ADD TRAIL DATA BEFORE SEARCHING FOR ROUTES.")
                        .color(chrome::MUTED),
                )
                .wrap(),
            );
            ui.add_space(16.0);
            let _command_label = ui.label(chrome::eyebrow("DEVELOPMENT COMMAND"));
            let command = format!(
                "trailgen build \"{}\" --source /path/to/trails.geojson",
                self.root.display()
            );
            let _command = ui.add(
                egui::Label::new(
                    RichText::new(command)
                        .monospace()
                        .size(11.0)
                        .color(chrome::TEXT),
                )
                .wrap(),
            );
            ui.add_space(16.0);
            let _actions = ui.horizontal(|ui| {
                let refresh = ui.add(
                    chrome::glyph_button("↻  REFRESH PROJECT", true).min_size(vec2(205.0, 34.0)),
                );
                chrome::tension(ui, &refresh);
                if refresh.clicked() {
                    self.fault = None;
                    *action = Some(WorkspaceAction::Reload);
                }
                let projects = ui.add(
                    chrome::glyph_button("▦  PROJECTS · CTRL+O", false).min_size(vec2(230.0, 34.0)),
                );
                chrome::tension(ui, &projects);
                if projects.clicked() {
                    *action = Some(WorkspaceAction::Projects);
                }
            });
            if let Some(fault) = &self.fault {
                fault_label(ui, fault);
            }
            ui.add_space(14.0);
            let _path = chrome::note(ui, format!("PROJECT FOLDER · {}", self.root.display()));
        });
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
                RichText::new("CREATE A BLANK PROJECT OR OPEN ONE ALREADY ON DISK.")
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
                .hint_text("project name")
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
            let browse = ui.add_sized([142.0, 28.0], chrome::glyph_button("□  BROWSE…", false));
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
            chrome::glyph_button("＋  CREATE BLANK PROJECT", true).min_size(vec2(245.0, 34.0)),
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
            let browse = ui.add_sized([142.0, 28.0], chrome::glyph_button("□  BROWSE…", false));
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
                chrome::glyph_button("↗  OPEN PROJECT", true).min_size(vec2(210.0, 34.0)),
            );
            let open = open.on_disabled_hover_text("Choose a project folder first.");
            chrome::tension(ui, &open);
            if open.clicked() {
                *action = Some(ProjectAction::Open(PathBuf::from(self.open_root.trim())));
            }
            if self.return_workspace.is_some() {
                let back =
                    ui.add(chrome::glyph_button("←  BACK", false).min_size(vec2(150.0, 34.0)));
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
                    chrome::glyph_button(
                        format!("◇  {}", project.name.to_ascii_uppercase()),
                        false,
                    ),
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
    let workspace = if has_graph {
        ProjectWorkspace::Trail(Box::new(TrailApp::open(
            ctx,
            &root,
            offline,
            habitat.slate_path(&root),
        )?))
    } else {
        ProjectWorkspace::Empty(Box::new(EmptyProject::new(place)))
    };
    if let Err(err) = habitat.remember(&root) {
        eprintln!("could not remember project: {err:#}");
    }
    Ok(workspace)
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
}
