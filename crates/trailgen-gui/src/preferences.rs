use eternalist_apps::configuration::Configuration;
use eternalist_apps::settings::SettingSpec;
use serde::{Deserialize, Serialize};
use trailgen_core::HikingModel;

const DEFAULT_BASE_PACE_KMH: f64 = 5.0;
pub const MIN_BASE_PACE_KMH: f64 = 0.5;
pub const MAX_BASE_PACE_KMH: f64 = 15.0;
pub const BASE_PACE_SETTING: SettingSpec = SettingSpec::new(
    "base_pace_kmh",
    "BASE PACE",
    "Calibrate moving-time estimates in kilometres per hour.",
);

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
pub struct Preferences {
    base_pace_kmh: f64,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            base_pace_kmh: DEFAULT_BASE_PACE_KMH,
        }
    }
}

impl Preferences {
    #[must_use]
    pub fn base_pace(&self) -> BasePace {
        BasePace::forge(self.base_pace_kmh).expect("live preferences must remain valid")
    }

    pub fn set_base_pace(&mut self, kmh: f64) {
        self.base_pace_kmh = (kmh * 10.0).round() / 10.0;
    }
}

impl Configuration for Preferences {
    fn validate(&self) -> std::result::Result<(), String> {
        BasePace::forge(self.base_pace_kmh).map_or_else(
            || {
                Err(format!(
                    "base_pace_kmh must be between {MIN_BASE_PACE_KMH} and {MAX_BASE_PACE_KMH}"
                ))
            },
            |_| Ok(()),
        )
    }
}
