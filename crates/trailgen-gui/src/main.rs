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
use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "trailgen-gui", about = "Native constrained-trail workbench")]
struct Args {
    /// Project directory containing trailgen.toml and cache/graph.json.
    #[arg(default_value = ".")]
    project: PathBuf,
    /// Suppress network-backed USGS topographic tiles.
    #[arg(long)]
    offline: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let ctx = egui::Context::default();
    dwemer_poolrooms::chrome::install(&ctx);
    let app = app::TrailApp::open(&ctx, &args.project, args.offline)?;
    boiler::run(ctx, app)
}
