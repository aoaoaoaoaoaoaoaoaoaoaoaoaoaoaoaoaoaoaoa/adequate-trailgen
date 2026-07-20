use crate::{
    ProjectIntent,
    app::{TrailApp, forge_water},
    habitat::{Habitat, ProjectPlace, forge_sample},
};
use anyhow::{Result, ensure};
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
    Trail {
        app: Box<TrailApp>,
        habitat: Habitat,
        offline: bool,
    },
    Projects(Box<ProjectDeck>),
    Limbo,
}

enum WorkbenchTransition {
    Projects,
    Trail {
        app: Box<TrailApp>,
        habitat: Habitat,
        offline: bool,
    },
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
        match open(ctx, &habitat, &root, offline) {
            Ok(app) => Self::still(WorkbenchMode::Trail {
                app: Box::new(app),
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
            WorkbenchMode::Trail {
                app,
                habitat: _,
                offline: _,
            } => app.pulse(ui).then_some(WorkbenchTransition::Projects),
            WorkbenchMode::Projects(deck) => deck.pulse(ui).map(|app| WorkbenchTransition::Trail {
                app,
                habitat: deck.habitat.clone(),
                offline: deck.offline,
            }),
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
            Some(WorkbenchTransition::Trail {
                app,
                habitat,
                offline,
            }) => {
                self.mode = WorkbenchMode::Trail {
                    app,
                    habitat,
                    offline,
                };
            }
            None => {}
        }
    }

    fn open_project_deck(&mut self) {
        let displaced = std::mem::replace(&mut self.mode, WorkbenchMode::Limbo);
        let WorkbenchMode::Trail {
            app,
            habitat,
            offline,
        } = displaced
        else {
            unreachable!("only a trail workbench can request the project deck");
        };
        let root = app.root().to_owned();
        self.mode = WorkbenchMode::Projects(Box::new(ProjectDeck::new(
            habitat,
            offline,
            Some(&root),
            None,
            Some(app),
        )));
    }

    pub fn water_frame(
        &mut self,
        ctx: &egui::Context,
        pixels_per_point: f32,
        tooltip_rects: &[egui::Rect],
    ) -> WaterFrame {
        match &mut self.mode {
            WorkbenchMode::Trail { app, .. } => {
                app.water_frame(ctx, pixels_per_point, tooltip_rects)
            }
            WorkbenchMode::Projects(deck) => {
                deck.water.frame(ctx, pixels_per_point, tooltip_rects, None)
            }
            WorkbenchMode::Limbo => unreachable!("workbench transition escaped its frame"),
        }
    }
}

pub struct ProjectDeck {
    habitat: Habitat,
    offline: bool,
    root: String,
    fault: Option<String>,
    return_app: Option<Box<TrailApp>>,
    known: Vec<ProjectPlace>,
    water: Surface,
}

impl ProjectDeck {
    fn new(
        habitat: Habitat,
        offline: bool,
        proposed: Option<&Path>,
        mut fault: Option<String>,
        return_app: Option<Box<TrailApp>>,
    ) -> Self {
        let root = proposed
            .or_else(|| habitat.library_root())
            .map_or_else(String::new, |root| root.to_string_lossy().into_owned());
        let known = habitat.known_projects().unwrap_or_else(|err| {
            fault = Some(format!("could not inspect the project library: {err:#}"));
            Vec::new()
        });
        Self {
            habitat,
            offline,
            root,
            fault,
            return_app,
            known,
            water: forge_water(),
        }
    }

    fn pulse(&mut self, ui: &mut egui::Ui) -> Option<Box<TrailApp>> {
        let mut action = self.return_app.as_ref().and_then(|_| {
            ui.ctx()
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
                .then_some(ProjectAction::Back)
        });
        let _center = egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.add_space(((ui.available_height() - 610.0) * 0.42).max(18.0));
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
            self.project_chooser(ui, action);
            self.known_projects(ui, action);
            self.sample_chooser(ui, action);
            self.footnotes(ui);
        });
    }

    fn heading(&self, ui: &mut egui::Ui) {
        let _eyebrow = ui.label(chrome::eyebrow("PROJECT DECK"));
        let title = if self.return_app.is_some() {
            "SWITCH TRAIL PROJECT"
        } else {
            "CHOOSE A TRAIL PROJECT"
        };
        let _title = ui.label(chrome::title(title));
        ui.add_space(7.0);
        let _copy = ui.add(
            egui::Label::new(
                RichText::new(
                    "OPEN A READY PROJECT FOLDER CONTAINING TRAILGEN.TOML AND A BUILT GRAPH. TRAILGEN REMEMBERS ONLY A PROJECT YOU CHOOSE; THE COLORADO SAMPLE IS NEVER CREATED OR OPENED AUTOMATICALLY.",
                )
                .color(chrome::MUTED),
            )
            .wrap(),
        );
        ui.add_space(14.0);
    }

    fn project_chooser(&mut self, ui: &mut egui::Ui, action: &mut Option<ProjectAction>) {
        let _label = ui.label(chrome::eyebrow("PROJECT FOLDER"));
        let _path = ui.horizontal(|ui| {
            let edit = ui.add_sized(
                [552.0, 28.0],
                egui::TextEdit::singleline(&mut self.root)
                    .hint_text("folder containing trailgen.toml")
                    .text_color(chrome::TEXT),
            );
            chrome::tension(ui, &edit);
            if edit.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                *action = Some(ProjectAction::Open(PathBuf::from(self.root.trim())));
            }
            let browse = ui.add_sized([142.0, 28.0], chrome::glyph_button("□  BROWSE…", false));
            chrome::tension(ui, &browse);
            if browse.clicked()
                && let Some(root) = self.pick_folder()
            {
                self.root = root.to_string_lossy().into_owned();
                self.fault = None;
            }
        });
        ui.add_space(6.0);
        let _actions = ui.horizontal(|ui| {
            let open = ui.add_enabled(
                !self.root.trim().is_empty(),
                chrome::glyph_button("↗  OPEN PROJECT", true).min_size(vec2(210.0, 34.0)),
            );
            let open = open.on_disabled_hover_text("Choose a project folder first.");
            chrome::tension(ui, &open);
            if open.clicked() {
                *action = Some(ProjectAction::Open(PathBuf::from(self.root.trim())));
            }
            if self.return_app.is_some() {
                let back =
                    ui.add(chrome::glyph_button("←  BACK", false).min_size(vec2(150.0, 34.0)));
                chrome::tension(ui, &back);
                if back.clicked() {
                    *action = Some(ProjectAction::Back);
                }
            }
        });
        if let Some(fault) = &self.fault {
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

    fn sample_chooser(&self, ui: &mut egui::Ui, action: &mut Option<ProjectAction>) {
        ui.add_space(16.0);
        let _sample = ui.label(chrome::eyebrow("OPTIONAL SAMPLE · COLORADO"));
        let _sample_row = ui.horizontal(|ui| {
            let _note = ui.add_sized(
                [470.0, 32.0],
                egui::Label::new(chrome::muted(
                    "A tiny synthetic loop for learning the controls. It is not your project.",
                ))
                .wrap(),
            );
            let sample = ui.add_enabled_ui(self.habitat.sample_root().is_some(), |ui| {
                ui.add_sized(
                    [224.0, 30.0],
                    chrome::glyph_button("△  OPEN COLORADO SAMPLE", false),
                )
            });
            let sample = sample
                .inner
                .on_disabled_hover_text("The operating system exposes no Documents directory.");
            chrome::tension(ui, &sample);
            if sample.clicked() {
                *action = Some(ProjectAction::Sample);
            }
        });
    }

    fn footnotes(&self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        let library = self.habitat.library_root().map_or_else(
            || "OS DOCUMENTS DIRECTORY UNAVAILABLE · BROWSE TO A PROJECT".to_owned(),
            |root| format!("CONVENTIONAL LIBRARY · {}", root.display()),
        );
        let _library = chrome::note(ui, library);
        let _shortcut = chrome::note(ui, "CTRL+O OPENS THIS DECK FROM THE WORKBENCH");
    }

    fn pick_folder(&self) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new().set_title("Open Trailgen Project");
        let proposed = Path::new(self.root.trim());
        if proposed.is_dir() {
            dialog = dialog.set_directory(proposed);
        } else if let Some(library) = self.habitat.library_root() {
            dialog = dialog.set_directory(library);
        }
        dialog.pick_folder()
    }

    fn attempt(&mut self, ctx: &egui::Context, action: ProjectAction) -> Option<Box<TrailApp>> {
        if matches!(action, ProjectAction::Back) {
            return self.return_app.take();
        }
        let result = (|| {
            let root = match action {
                ProjectAction::Open(root) => {
                    ensure!(!root.as_os_str().is_empty(), "choose a project folder");
                    root
                }
                ProjectAction::Sample => {
                    forge_sample(&self.habitat.sample_root().ok_or_else(|| {
                        anyhow::anyhow!("the operating system exposes no Documents directory")
                    })?)?
                }
                ProjectAction::Back => unreachable!("back handled before opening a project"),
            };
            open(ctx, &self.habitat, &root, self.offline)
        })();
        match result {
            Ok(app) => Some(Box::new(app)),
            Err(err) => {
                self.fault = Some(format!("{err:#}"));
                None
            }
        }
    }
}

enum ProjectAction {
    Open(PathBuf),
    Sample,
    Back,
}

fn open(ctx: &egui::Context, habitat: &Habitat, root: &Path, offline: bool) -> Result<TrailApp> {
    let app = TrailApp::open(ctx, root, offline, habitat.slate_path(root))?;
    if let Err(err) = habitat.remember(root) {
        eprintln!("could not remember project: {err:#}");
    }
    Ok(app)
}
