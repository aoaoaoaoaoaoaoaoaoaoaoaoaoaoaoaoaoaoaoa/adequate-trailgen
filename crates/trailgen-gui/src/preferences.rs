use crate::persistence;
use anyhow::{Context as _, Result, ensure};
use eternalist_apps::{ScribeOutcome, SettledScribe};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
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

/// A debounced, background-committed XDG preference ledger.
pub struct PreferenceLedger {
    live: Values,
    scribe: SettledScribe<Values>,
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
        let worker_path = path;
        let scribe = SettledScribe::spawn(
            "trailgen-preference-scribe",
            ctx,
            SETTLE,
            move |values: Values| save(&worker_path, &values),
        )?;
        Ok(Self {
            live,
            scribe,
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
        self.scribe.mark();
        true
    }

    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.scribe.deadline()
    }

    pub fn service_deadline_reached(&mut self, now: Instant) -> bool {
        if self
            .scribe
            .deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            let values = self.live.clone();
            if let Err(error) = self.scribe.tend(now, || values) {
                self.alarm = Some(format!("Could not save preferences: {error:#}"));
                return true;
            }
        }
        false
    }

    pub fn take_alarm(&mut self) -> Option<String> {
        if let Some(ScribeOutcome::Fault { message, .. }) = self.scribe.take_outcome() {
            self.alarm = Some(format!("Could not save preferences: {message}"));
        }
        self.alarm.take()
    }
}

impl Drop for PreferenceLedger {
    fn drop(&mut self) {
        if let Err(err) = self.scribe.flush(self.live.clone()) {
            eprintln!("could not save trailgen preferences: {err:#}");
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
    persistence::replace(path, body.as_bytes())
        .with_context(|| format!("replace preferences {}", path.display()))
}
