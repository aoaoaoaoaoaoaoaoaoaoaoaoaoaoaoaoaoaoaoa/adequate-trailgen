#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "screen, tile, and GPU coordinates are deliberately narrowed after viewport bounds"
)]

mod app;
mod basemap;
mod boiler;
mod gallery;
mod genesis;
mod habitat;
mod map;
mod profile;
mod project;
mod vector_map;

use anyhow::Result;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectIntent {
    Resume,
    Open(PathBuf),
}

/// Resume or explicitly open a trailgen project in the native workbench.
pub fn run(intent: ProjectIntent, offline: bool) -> Result<()> {
    let habitat = habitat::Habitat::discover()?;
    let ctx = egui::Context::default();
    dwemer_poolrooms::chrome::install(&ctx);
    let app = genesis::Workbench::launch(&ctx, habitat, intent, offline)?;
    boiler::run(ctx, app)
}
