use crate::{lexicon::ExplainedText, lexicon::Glosses, preferences::BasePace};
use trailgen_core::RouteMetrics;

#[must_use]
pub fn metrics_summary(metrics: &RouteMetrics, pace: BasePace) -> ExplainedText {
    let head = format!(
        "{:.2} KM · LOAD {:.1} FGJW KM · MOVING {} · QUALITY {:.0}",
        metrics.distance_m / 1_000.0,
        metrics.lower_limb_load_km,
        moving_time(metrics.moving_time_s, pace),
        metrics.quality
    );
    let text = if metrics.elevation_fraction >= 0.8 {
        format!(
            "{head} · ASCENT {:.0} M · DESCENT {:.0} M",
            metrics.ascent_m, metrics.descent_m
        )
    } else {
        format!("{head} · ELEVATION UNAVAILABLE")
    };
    ExplainedText::forge(text, Glosses::ROUTE_METRICS)
}

#[must_use]
pub fn tile_measurements(metrics: &RouteMetrics, pace: BasePace) -> ExplainedText {
    let text = if metrics.elevation_fraction >= 0.8 {
        format!(
            "{:.1} KM   {}   ASCENT {:.0} M",
            metrics.distance_m / 1_000.0,
            moving_time(metrics.moving_time_s, pace),
            metrics.ascent_m,
        )
    } else {
        format!(
            "{:.1} KM   {}   NO ELEVATION",
            metrics.distance_m / 1_000.0,
            moving_time(metrics.moving_time_s, pace)
        )
    };
    ExplainedText::forge(text, Glosses::MOVING_TIME)
}

#[must_use]
pub fn load_badge(metrics: &RouteMetrics) -> ExplainedText {
    ExplainedText::forge(
        format!("LOAD {:.0} FGJW KM", metrics.lower_limb_load_km),
        Glosses::FGJW,
    )
}

#[must_use]
pub fn library_measurements(metrics: &RouteMetrics) -> String {
    if metrics.elevation_fraction >= 0.8 {
        format!(
            "{:.1} KM · +{:.0} M",
            metrics.distance_m / 1_000.0,
            metrics.ascent_m
        )
    } else {
        format!("{:.1} KM", metrics.distance_m / 1_000.0)
    }
}

#[must_use]
pub fn library_load(metrics: &RouteMetrics) -> ExplainedText {
    ExplainedText::forge(
        format!("{:.0} FGJW KM", metrics.lower_limb_load_km),
        Glosses::FGJW,
    )
}

fn moving_time(population_seconds: f64, pace: BasePace) -> String {
    let minutes = (pace.moving_time_s(population_seconds).max(0.0) / 60.0).round();
    format!("{:.0}:{:02.0}", (minutes / 60.0).floor(), minutes % 60.0)
}
