use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, bounded};
use egui::Context;
use std::{path::PathBuf, thread};
use trailgen_core::source::GeoBounds;
use trailgen_data::{Event as EngineEvent, Summary, Surveyor};

pub fn progress_status(event: &EngineEvent) -> String {
    match event {
        EngineEvent::Locating => "Finding the map area…".to_owned(),
        EngineEvent::Located(_) => "Map area found.".to_owned(),
        EngineEvent::Ranging { .. } => "Downloading trails…".to_owned(),
        EngineEvent::Downloaded { .. } => "Trail download complete.".to_owned(),
        EngineEvent::Elevating { complete, total } => {
            format!("Downloading topography… {complete}/{total}")
        }
        EngineEvent::Indexing => "Preparing trails…".to_owned(),
        EngineEvent::Ready(summary) => {
            format!("Trail data ready in {} map area(s).", summary.regions.len())
        }
    }
}

pub enum Mutation {
    Add(GeoBounds),
    Remove(String),
    Replace { id: String, bounds: GeoBounds },
    Refresh,
}

pub enum Event {
    Progress(EngineEvent),
    Ready(Option<Summary>),
    Fault(String),
}

pub struct TrailData {
    pub events: Receiver<Event>,
    _thread: thread::JoinHandle<()>,
}

impl TrailData {
    pub fn spawn(ctx: Context, project: PathBuf, mutation: Mutation) -> Result<Self> {
        let (events_tx, events) = bounded(32);
        let thread = thread::Builder::new()
            .name("trail-corpus-mutation".to_owned())
            .spawn(move || {
                let surveyor = Surveyor::default();
                let mut progress = |event| {
                    if matches!(&event, trailgen_data::Event::Ready(_)) {
                        return;
                    }
                    let _sent = events_tx.try_send(Event::Progress(event));
                    ctx.request_repaint();
                };
                let result = match mutation {
                    Mutation::Add(bounds) => surveyor
                        .add_region(&project, bounds, &mut progress)
                        .map(Some),
                    Mutation::Remove(id) => surveyor.remove_region(&project, &id, &mut progress),
                    Mutation::Replace { id, bounds } => surveyor
                        .replace_region(&project, &id, bounds, &mut progress)
                        .map(Some),
                    Mutation::Refresh => surveyor.refresh(&project, &mut progress),
                };
                let event = match result {
                    Ok(summary) => Event::Ready(summary),
                    Err(err) => Event::Fault(format!("{err:#}")),
                };
                let _sent = events_tx.send(event);
                ctx.request_repaint();
            })
            .context("spawn trail-data surveyor")?;
        Ok(Self {
            events,
            _thread: thread,
        })
    }
}
