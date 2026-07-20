use crate::{
    ProjectIntent,
    app::{TrailApp, forge_water},
    habitat::{Habitat, forge_starter},
};
use anyhow::{Result, ensure};
use dwemer_poolrooms::{
    chrome,
    water::{Frame as WaterFrame, WaterTable},
};
use egui::{Color32, RichText, Stroke, vec2};
use std::path::{Path, PathBuf};

pub enum Workbench {
    Trail(Box<TrailApp>),
    Genesis(Box<Genesis>),
}

impl Workbench {
    pub fn launch(
        ctx: &egui::Context,
        habitat: Habitat,
        intent: ProjectIntent,
        offline: bool,
    ) -> Result<Self> {
        match intent {
            ProjectIntent::Open(root) => {
                open(ctx, &habitat, &root, offline).map(|app| Self::Trail(Box::new(app)))
            }
            ProjectIntent::Resume => match habitat
                .resume()
                .and_then(|root| open(ctx, &habitat, &root, offline))
            {
                Ok(app) => Ok(Self::Trail(Box::new(app))),
                Err(err) => Ok(Self::Genesis(Box::new(Genesis::new(
                    habitat, offline, &err,
                )))),
            },
        }
    }

    pub fn pulse(&mut self, ui: &mut egui::Ui) {
        let ascension = match self {
            Self::Trail(app) => {
                app.pulse(ui);
                None
            }
            Self::Genesis(genesis) => genesis.pulse(ui),
        };
        if let Some(app) = ascension {
            *self = Self::Trail(Box::new(app));
        }
    }

    pub fn water_frame(
        &mut self,
        ctx: &egui::Context,
        pixels_per_point: f32,
        tooltip_rects: &[egui::Rect],
    ) -> WaterFrame {
        match self {
            Self::Trail(app) => app.water_frame(ctx, pixels_per_point, tooltip_rects),
            Self::Genesis(genesis) => {
                genesis
                    .water
                    .frame(ctx, pixels_per_point, tooltip_rects, None)
            }
        }
    }
}

pub struct Genesis {
    habitat: Habitat,
    offline: bool,
    root: String,
    fault: String,
    water: WaterTable,
}

impl Genesis {
    fn new(habitat: Habitat, offline: bool, fault: &anyhow::Error) -> Self {
        let root = habitat
            .starter_root()
            .map_or_else(String::new, |root| root.to_string_lossy().into_owned());
        Self {
            habitat,
            offline,
            root,
            fault: format!("{fault:#}"),
            water: forge_water(),
        }
    }

    fn pulse(&mut self, ui: &mut egui::Ui) -> Option<TrailApp> {
        let mut ascension = None;
        let _center = egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.add_space(((ui.available_height() - 330.0) * 0.42).max(24.0));
            let _row = ui.horizontal(|ui| {
                ui.add_space(((ui.available_width() - 660.0) * 0.5).max(12.0));
                let _plate = egui::Frame::new()
                    .fill(chrome::SURFACE)
                    .stroke(Stroke::new(1.0_f32, chrome::EDGE_STRONG))
                    .corner_radius(2)
                    .inner_margin(egui::Margin::same(24))
                    .show(ui, |ui| self.plate(ui, &mut ascension));
            });
        });
        ascension
    }

    fn plate(&mut self, ui: &mut egui::Ui, ascension: &mut Option<TrailApp>) {
        let _column = ui.vertical(|ui| {
            ui.set_width(610.0);
            let _eyebrow = ui.label(chrome::eyebrow("PROJECT TERRITORY"));
            let _title = ui.label(chrome::title("ESTABLISH A TRAIL FORGE"));
            ui.add_space(7.0);
            let _copy = ui.add(
                egui::Label::new(
                    RichText::new(
                        "TRAILGEN COULD NOT RESUME A PROJECT. CHOOSE AN EXISTING ROOT OR FORGE THE BUNDLED STARTER LOOP.",
                    )
                    .color(chrome::MUTED),
                )
                .wrap(),
            );
            ui.add_space(12.0);
            let _label = ui.label(chrome::eyebrow("PROJECT ROOT"));
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.root)
                    .desired_width(f32::INFINITY)
                    .text_color(chrome::TEXT),
            );
            chrome::tension(ui, &edit);
            ui.add_space(5.0);
            let _fault = ui.add(
                egui::Label::new(
                    RichText::new(self.fault.to_ascii_uppercase())
                        .size(11.0)
                        .color(Color32::from_rgb(203, 113, 91)),
                )
                .wrap(),
            );
            ui.add_space(12.0);
            let _actions = ui.horizontal(|ui| {
                let forge = ui.add(
                    chrome::glyph_button("✦  FORGE STARTER", true)
                        .min_size(vec2(205.0, 34.0)),
                );
                chrome::tension(ui, &forge);
                if forge.clicked() {
                    *ascension = self.attempt(ui.ctx(), GenesisAction::Forge);
                }
                let open = ui.add(
                    chrome::glyph_button("□  OPEN EXISTING", false)
                        .min_size(vec2(205.0, 34.0)),
                );
                chrome::tension(ui, &open);
                if open.clicked() {
                    *ascension = self.attempt(ui.ctx(), GenesisAction::Open);
                }
            });
        });
    }

    fn attempt(&mut self, ctx: &egui::Context, action: GenesisAction) -> Option<TrailApp> {
        let result = (|| {
            let raw = self.root.trim();
            ensure!(!raw.is_empty(), "project root must not be empty");
            let root = PathBuf::from(raw);
            let root = match action {
                GenesisAction::Forge => forge_starter(&root)?,
                GenesisAction::Open => root,
            };
            open(ctx, &self.habitat, &root, self.offline)
        })();
        match result {
            Ok(app) => Some(app),
            Err(err) => {
                self.fault = format!("{err:#}");
                None
            }
        }
    }
}

#[derive(Clone, Copy)]
enum GenesisAction {
    Forge,
    Open,
}

fn open(ctx: &egui::Context, habitat: &Habitat, root: &Path, offline: bool) -> Result<TrailApp> {
    let app = TrailApp::open(ctx, root, offline)?;
    if let Err(err) = habitat.remember(root) {
        eprintln!("could not remember project: {err:#}");
    }
    Ok(app)
}
