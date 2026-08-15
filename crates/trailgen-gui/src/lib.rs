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
mod commands;
mod export;
mod forge;
mod gallery;
mod habitat;
mod host;
mod lexicon;
mod library;
mod live_area;
mod map;
mod palette;
mod persistence;
mod portfolio;
mod preferences;
mod profile;
mod project;
mod projects;
mod readout;
mod relief;
mod search_boundary;
mod slate;
mod trail_data;
mod trail_map;
mod vector_field;
mod vector_map;
mod witness;

use anyhow::Result;
use std::path::PathBuf;

pub use export::{SavedTrailListing, export_saved_gpx, saved_trails};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectIntent {
    Resume,
    Open(PathBuf),
}

/// Resume or explicitly open a trailgen project in the native workbench.
pub fn run(intent: ProjectIntent, offline: bool) -> Result<()> {
    let trace = eternalist_apps::TraceGuard::arm()?;
    let ctx = egui::Context::default();
    brass_poolrooms::chrome::install(&ctx);
    let result = host::run(ctx, intent, offline);
    trace.flush();
    result
}
