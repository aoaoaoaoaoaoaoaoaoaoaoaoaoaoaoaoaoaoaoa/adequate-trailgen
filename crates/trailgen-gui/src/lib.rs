#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "screen, tile, and GPU coordinates are deliberately narrowed after viewport bounds"
)]

macro_rules! product_phase {
    ($name:literal, $body:expr) => {{
        let _phase = tracing::info_span!(target: "eternalist::product", $name).entered();
        $body
    }};
}

mod annotation;
mod app;
mod basemap;
mod cadence;
mod chrome;
mod civic_area;
mod forge;
mod gallery;
mod habitat;
mod host;
mod library;
mod live_area;
mod map;
mod portfolio;
mod profile;
mod project;
mod projects;
mod relief;
mod search_boundary;
mod shortcut_help;
mod slate;
mod trail_data;
mod trail_map;
mod vector_field;
mod vector_map;
mod witness;

use anyhow::Result;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectIntent {
    Resume,
    Open(PathBuf),
}

/// Resume or explicitly open a trailgen project in the native workbench.
pub fn run(intent: ProjectIntent, offline: bool) -> Result<()> {
    let trace = trailgen_shell::TraceGuard::arm()?;
    let bootstrap =
        tracing::info_span!(target: "eternalist::startup", "application.bootstrap").entered();
    let habitat = habitat::Habitat::discover()?;
    let ctx = egui::Context::default();
    dwemer_poolrooms::chrome::install(&ctx);
    let app = projects::Workbench::launch(&ctx, habitat, intent, offline);
    drop(bootstrap);
    let result = host::run(ctx, app);
    trace.flush();
    result
}
