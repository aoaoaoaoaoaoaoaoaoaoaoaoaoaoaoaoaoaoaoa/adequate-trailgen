use crate::{
    library::{SavedTrail, TrailId},
    project::Project,
};
use anyhow::{Context as _, Result, bail};
use crossbeam_channel::{Receiver, Sender, bounded};
use eternalist_apps::NativeWake;
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    thread,
};
use trailgen_core::io::{
    gpx::route_file_to_gpx,
    route_file::{RouteFile, RouteFileMetadata, metrics_summary},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedTrailListing {
    pub id: String,
    pub name: String,
}

/// Enumerate the only trails eligible for product export: durable Library entries.
pub fn saved_trails(project: &Path) -> Result<Vec<SavedTrailListing>> {
    Ok(Project::open(project)?
        .library
        .trails()
        .iter()
        .map(|trail| SavedTrailListing {
            id: trail.id.as_str().to_owned(),
            name: trail.name.clone(),
        })
        .collect())
}

/// Export one durable Library trail, selected by exact identity or unique name.
pub fn export_saved_gpx(project: &Path, selector: &str, destination: &Path) -> Result<()> {
    let project = Project::open(project)?;
    let identities = project
        .library
        .trails()
        .iter()
        .filter(|trail| trail.id.as_str() == selector)
        .collect::<Vec<_>>();
    let matches = if identities.is_empty() {
        project
            .library
            .trails()
            .iter()
            .filter(|trail| trail.name == selector)
            .collect::<Vec<_>>()
    } else {
        identities
    };
    let [trail] = matches.as_slice() else {
        if matches.is_empty() {
            bail!("saved trail `{selector}` does not exist");
        }
        bail!("saved trail name `{selector}` is ambiguous; select its identity");
    };
    write_saved_gpx(trail, destination)
}

pub fn suggested_filename(name: &str) -> String {
    let mut stem = String::new();
    let mut separated = false;
    for character in name.trim().chars() {
        if character.is_alphanumeric() {
            for lower in character.to_lowercase() {
                stem.push(lower);
            }
            separated = false;
        } else if !stem.is_empty() && !separated {
            stem.push('-');
            separated = true;
        }
    }
    while stem.ends_with('-') {
        let _ = stem.pop();
    }
    if stem.is_empty() {
        "trail.gpx".to_owned()
    } else {
        format!("{stem}.gpx")
    }
}

fn write_saved_gpx(trail: &SavedTrail, destination: &Path) -> Result<()> {
    let route = RouteFile::new(
        trail.geometry(),
        RouteFileMetadata {
            title: Some(trail.name.clone()),
            description: Some(metrics_summary(&trail.metrics)),
            recorded_at: None,
            activity_type: Some("hiking".to_owned()),
        },
    );
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("prepare export beside {}", destination.display()))?;
    temporary
        .write_all(route_file_to_gpx(&route).as_bytes())
        .with_context(|| format!("write {}", destination.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("sync {}", destination.display()))?;
    let _file = temporary
        .persist(destination)
        .with_context(|| format!("replace {}", destination.display()))?;
    Ok(())
}

pub struct ExportJob {
    pub trail: SavedTrail,
    pub destination: PathBuf,
}

pub enum ExportEvent {
    Written { id: TrailId, destination: PathBuf },
    Fault(String),
}

pub struct ExportForge {
    command: Sender<ExportJob>,
    events: Receiver<ExportEvent>,
    _thread: thread::JoinHandle<()>,
}

impl ExportForge {
    pub fn spawn(ctx: &egui::Context) -> Result<Self> {
        let (command, jobs) = bounded::<ExportJob>(1);
        let (publish, events) = bounded(1);
        let wake = NativeWake::from_context(ctx);
        let thread = thread::Builder::new()
            .name("saved-trail-exporter".to_owned())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    let id = job.trail.id.clone();
                    let event = match write_saved_gpx(&job.trail, &job.destination) {
                        Ok(()) => ExportEvent::Written {
                            id,
                            destination: job.destination,
                        },
                        Err(error) => ExportEvent::Fault(format!("{error:#}")),
                    };
                    if publish.send(event).is_err() {
                        break;
                    }
                    let _woken = wake.request_foreground_repaint();
                }
            })
            .context("spawn saved-trail exporter")?;
        Ok(Self {
            command,
            events,
            _thread: thread,
        })
    }

    pub fn strike(&self, job: ExportJob) -> Result<()> {
        self.command
            .try_send(job)
            .context("saved-trail exporter is busy")
    }

    pub const fn events(&self) -> &Receiver<ExportEvent> {
        &self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Library;
    use trailgen_core::{
        GraphBuilder, LoopConstraints, SearchParams, SolverKind, VertexId, io::geojson,
    };

    #[test]
    fn application_service_exports_only_the_saved_library() -> Result<()> {
        let project = tempfile::tempdir()?;
        std::fs::write(
            project.path().join("trailgen.toml"),
            "name = 'Export Test'\n",
        )?;
        let graph = GraphBuilder::default().build(&geojson::network_from_str(include_str!(
            "../../trailgen-core/tests/fixtures/mini_network.geojson"
        ))?)?;
        let mut route = SolverKind::Exact
            .solve(
                SearchParams::default(),
                &graph,
                VertexId(0),
                &LoopConstraints {
                    min_distance_m: 0.0,
                    max_distance_m: 20_000.0,
                    ..LoopConstraints::default()
                },
                1,
            )
            .into_iter()
            .next()
            .context("fixture must contain a loop")?;
        route.name = "Devil & Path".to_owned();
        let mut library = Library::default();
        let id = library.promote(&graph, &route)?;
        library.save(project.path())?;

        assert_eq!(
            saved_trails(project.path())?,
            vec![SavedTrailListing {
                id: id.as_str().to_owned(),
                name: route.name.clone(),
            }]
        );
        let output = project.path().join("handoff.gpx");
        export_saved_gpx(project.path(), &route.name, &output)?;
        export_saved_gpx(project.path(), id.as_str(), &output)?;
        let parsed =
            trailgen_core::io::gpx::route_file_from_str(&std::fs::read_to_string(output)?)?;
        assert_eq!(parsed.metadata.title.as_deref(), Some(route.name.as_str()));
        let expected = route.geometry(&graph);
        assert_eq!(parsed.line.points.len(), expected.points.len());
        assert!(
            parsed
                .line
                .points
                .iter()
                .zip(&expected.points)
                .all(|(actual, expected)| actual.haversine_m(*expected) <= 0.02
                    && actual
                        .ele
                        .zip(expected.ele)
                        .is_none_or(|(actual, expected)| (actual - expected).abs() <= 0.01))
        );
        assert!(
            parsed
                .metadata
                .description
                .is_some_and(|description| description.starts_with("shape "))
        );
        Ok(())
    }
}
