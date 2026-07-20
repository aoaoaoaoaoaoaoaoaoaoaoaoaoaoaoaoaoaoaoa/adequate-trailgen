#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "screen, tile, and GPU coordinates are deliberately narrowed after viewport bounds"
)]

mod app;
mod boiler;
mod gallery;
mod map;
mod profile;
mod project;
mod tile;

use anyhow::Result;
use std::path::Path;

/// Open a materialized trailgen project in the native workbench.
pub fn run(project: &Path, offline: bool) -> Result<()> {
    let ctx = egui::Context::default();
    dwemer_poolrooms::chrome::install(&ctx);
    let app = app::TrailApp::open(&ctx, project, offline)?;
    boiler::run(ctx, app)
}
