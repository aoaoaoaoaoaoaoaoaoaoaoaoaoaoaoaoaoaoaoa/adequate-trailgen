use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, bounded};
use egui::Context;
use std::{path::PathBuf, thread};
use trailgen_data::{Event as EngineEvent, Summary, Surveyor};

pub enum Event {
    Progress(EngineEvent),
    Ready(Summary),
    Fault(String),
}

pub struct TrailData {
    pub events: Receiver<Event>,
    _thread: thread::JoinHandle<()>,
}

impl TrailData {
    pub fn spawn(ctx: Context, project: PathBuf, place: String, radius_km: f64) -> Result<Self> {
        let (events_tx, events) = bounded(32);
        let thread = thread::Builder::new()
            .name("trail-data-surveyor".to_owned())
            .spawn(move || {
                let result = Surveyor::default().survey(&project, &place, radius_km, |event| {
                    if matches!(&event, trailgen_data::Event::Ready(_)) {
                        return;
                    }
                    let _sent = events_tx.try_send(Event::Progress(event));
                    ctx.request_repaint();
                });
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
