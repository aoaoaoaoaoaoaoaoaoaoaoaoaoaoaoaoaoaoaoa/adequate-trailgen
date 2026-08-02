use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use trailgen_gui::ProjectIntent;

#[derive(Debug, Parser)]
#[command(
    name = "trailgen",
    version,
    about = "Native trail-design workbench with a narrow debug shell"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Open the native workbench.
    Gui {
        /// Project to open; omit to resume the last workbench.
        project: Option<PathBuf>,
        /// Forbid network-backed trail and civic-area acquisition.
        #[arg(long)]
        offline: bool,
    },
    /// Enumerate the trails durably saved in a project Library.
    Saved { project: PathBuf },
    /// Export one saved Library trail as GPX.
    Export {
        project: PathBuf,
        /// Exact saved-trail identity or unique name.
        #[arg(long)]
        trail: String,
        /// Destination GPX file.
        #[arg(long)]
        output: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        None => trailgen_gui::run(ProjectIntent::Resume, false),
        Some(Command::Gui { project, offline }) => trailgen_gui::run(
            project.map_or(ProjectIntent::Resume, ProjectIntent::Open),
            offline,
        ),
        Some(Command::Saved { project }) => {
            for trail in trailgen_gui::saved_trails(&project)? {
                println!("{}\t{}", trail.id, trail.name);
            }
            Ok(())
        }
        Some(Command::Export {
            project,
            trail,
            output,
        }) => trailgen_gui::export_saved_gpx(&project, &trail, &output),
    }
}
