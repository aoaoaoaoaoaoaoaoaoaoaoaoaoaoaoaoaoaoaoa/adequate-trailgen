use anyhow::{Context as _, Result, ensure};
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
use trailgen_core::HikingModel;

const SETTLE: Duration = Duration::from_millis(400);
const DEFAULT_BASE_PACE_KMH: f64 = 5.0;
const MIN_BASE_PACE_KMH: f64 = 0.5;
const MAX_BASE_PACE_KMH: f64 = 15.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BasePace(f64);

impl Default for BasePace {
    fn default() -> Self {
        Self(DEFAULT_BASE_PACE_KMH)
    }
}

impl BasePace {
    #[must_use]
    pub const fn kmh(self) -> f64 {
        self.0
    }

    #[must_use]
    pub fn forge(kmh: f64) -> Option<Self> {
        (kmh.is_finite() && (MIN_BASE_PACE_KMH..=MAX_BASE_PACE_KMH).contains(&kmh))
            .then_some(Self(kmh))
    }

    /// Project a population Wood estimate into the user's clock.
    #[must_use]
    pub fn moving_time_s(self, population_seconds: f64) -> f64 {
        population_seconds * HikingModel::reference_flat_speed_kmh() / self.0
    }

    /// Convert a user-facing duration back into the population clock used by
    /// the graph's precomputed traversal estimates.
    #[must_use]
    pub fn population_time_s(self, personal_seconds: f64) -> f64 {
        personal_seconds * self.0 / HikingModel::reference_flat_speed_kmh()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Values {
    base_pace_kmh: f64,
}

impl Default for Values {
    fn default() -> Self {
        Self {
            base_pace_kmh: DEFAULT_BASE_PACE_KMH,
        }
    }
}

impl Values {
    fn validate(&self) -> Result<()> {
        ensure!(
            BasePace::forge(self.base_pace_kmh).is_some(),
            "base_pace_kmh must be between {MIN_BASE_PACE_KMH} and {MAX_BASE_PACE_KMH}"
        );
        Ok(())
    }

    fn base_pace(&self) -> BasePace {
        BasePace::forge(self.base_pace_kmh).expect("live preferences must remain valid")
    }
}

enum Command {
    Commit { revision: u64, values: Values },
    Finish(Values),
}

struct Receipt {
    revision: u64,
    values: Values,
    fault: Option<String>,
}

/// A debounced, background-committed XDG preference ledger.
pub struct PreferenceLedger {
    path: PathBuf,
    live: Values,
    committed: Values,
    dirty: Option<Instant>,
    in_flight: Option<u64>,
    revision: u64,
    command: Sender<Command>,
    receipts: Receiver<Receipt>,
    worker: Option<thread::JoinHandle<()>>,
    alarm: Option<String>,
}

impl PreferenceLedger {
    pub fn raise(ctx: &egui::Context, path: PathBuf) -> Result<Self> {
        let (live, alarm) = match load(&path) {
            Ok(values) => (values, None),
            Err(err) => (
                Values::default(),
                Some(format!("Preferences reset to defaults: {err:#}")),
            ),
        };
        let (command, commands) = unbounded();
        let (publish, receipts) = unbounded();
        let worker_path = path.clone();
        let worker_ctx = ctx.clone();
        let worker = thread::Builder::new()
            .name("preference-committer".to_owned())
            .spawn(move || {
                while let Ok(command) = commands.recv() {
                    match command {
                        Command::Commit { revision, values } => {
                            let fault = save(&worker_path, &values)
                                .err()
                                .map(|err| format!("{err:#}"));
                            if publish
                                .send(Receipt {
                                    revision,
                                    values,
                                    fault,
                                })
                                .is_err()
                            {
                                break;
                            }
                            worker_ctx.request_repaint();
                        }
                        Command::Finish(values) => {
                            if let Err(err) = save(&worker_path, &values) {
                                eprintln!("could not save trailgen preferences: {err:#}");
                            }
                            break;
                        }
                    }
                }
            })
            .context("spawn preference committer")?;
        Ok(Self {
            path,
            committed: live.clone(),
            live,
            dirty: None,
            in_flight: None,
            revision: 0,
            command,
            receipts,
            worker: Some(worker),
            alarm,
        })
    }

    #[must_use]
    pub fn base_pace(&self) -> BasePace {
        self.live.base_pace()
    }

    pub fn revise_base_pace(&mut self, kmh: f64) -> bool {
        let Some(pace) = BasePace::forge(kmh) else {
            return false;
        };
        if pace == self.base_pace() {
            return false;
        }
        self.live.base_pace_kmh = pace.kmh();
        self.dirty = Some(Instant::now());
        true
    }

    pub fn tend(&mut self, ctx: &egui::Context) -> Option<String> {
        while let Ok(receipt) = self.receipts.try_recv() {
            if self.in_flight == Some(receipt.revision) {
                self.in_flight = None;
            }
            if let Some(fault) = receipt.fault {
                self.alarm = Some(format!("Could not save preferences: {fault}"));
                self.dirty = Some(Instant::now());
            } else {
                self.committed = receipt.values;
            }
        }
        if self.live == self.committed {
            self.dirty = None;
        } else if self.in_flight.is_none() {
            let dirty = self.dirty.get_or_insert_with(Instant::now);
            let settled = dirty.elapsed();
            if settled < SETTLE {
                ctx.request_repaint_after(SETTLE.saturating_sub(settled));
            } else {
                self.revision = self.revision.saturating_add(1);
                let revision = self.revision;
                let values = self.live.clone();
                if self
                    .command
                    .send(Command::Commit { revision, values })
                    .is_ok()
                {
                    self.in_flight = Some(revision);
                    self.dirty = None;
                } else {
                    self.alarm = Some("Could not save preferences: committer stopped".to_owned());
                    self.dirty = Some(Instant::now());
                }
            }
        }
        self.alarm.take()
    }
}

impl Drop for PreferenceLedger {
    fn drop(&mut self) {
        if self
            .command
            .send(Command::Finish(self.live.clone()))
            .is_err()
            && let Err(err) = save(&self.path, &self.live)
        {
            eprintln!("could not save trailgen preferences: {err:#}");
        }
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            eprintln!("trailgen preference committer panicked");
        }
    }
}

fn load(path: &Path) -> Result<Values> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Values::default()),
        Err(err) => {
            return Err(err).with_context(|| format!("read preferences {}", path.display()));
        }
    };
    let values = toml::from_str::<Values>(&text)
        .with_context(|| format!("parse preferences {}", path.display()))?;
    values.validate()?;
    Ok(values)
}

fn save(path: &Path, values: &Values) -> Result<()> {
    let parent = path.parent().context("preferences path has no parent")?;
    crate::habitat::create_private_dir(parent)?;
    let body = toml::to_string_pretty(values).context("serialize preferences")?;
    let temporary = path.with_extension(format!("toml.{}.partial", std::process::id()));
    {
        let mut file = fs::File::create(&temporary)
            .with_context(|| format!("create preference staging file {}", temporary.display()))?;
        file.write_all(body.as_bytes())
            .with_context(|| format!("write preference staging file {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync preference staging file {}", temporary.display()))?;
    }
    fs::rename(&temporary, path).with_context(|| {
        format!(
            "replace preferences {} with {}",
            temporary.display(),
            path.display()
        )
    })?;
    fs::File::open(parent)
        .with_context(|| format!("open preference directory {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("sync preference directory {}", parent.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pace_is_five_kilometers_per_hour() {
        assert!((BasePace::default().kmh() - 5.0).abs() <= f64::EPSILON);
    }

    #[test]
    fn pace_projection_and_search_conversion_are_inverses() {
        let pace = BasePace::forge(6.6).expect("fixture pace must be valid");
        let population = 5.0 * 3_600.0;
        let personal = pace.moving_time_s(population);
        assert!(personal < population);
        assert!((pace.population_time_s(personal) - population).abs() <= 1.0e-9);
    }

    #[test]
    fn preferences_round_trip_and_reject_invalid_paces() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("preferences.toml");
        let values = Values { base_pace_kmh: 6.6 };
        save(&path, &values)?;
        assert_eq!(load(&path)?, values);

        fs::write(&path, "base_pace_kmh = 0.0\n")?;
        assert!(load(&path).is_err());
        Ok(())
    }
}
