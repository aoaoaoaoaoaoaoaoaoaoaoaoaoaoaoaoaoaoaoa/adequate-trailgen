use crate::geo::LineString;
use crate::model::GradeDistribution;
use crate::route::{Route, RouteMetrics};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RouteFileMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_type: Option<String>,
}

impl RouteFileMetadata {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.recorded_at.is_none()
            && self.activity_type.is_none()
    }

    #[must_use]
    pub fn title_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.title.as_deref().unwrap_or(fallback)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouteFile {
    pub line: LineString,
    #[serde(default, skip_serializing_if = "RouteFileMetadata::is_empty")]
    pub metadata: RouteFileMetadata,
}

impl RouteFile {
    #[must_use]
    pub const fn new(line: LineString, metadata: RouteFileMetadata) -> Self {
        Self { line, metadata }
    }
}

#[must_use]
pub fn clean_text(raw: &str) -> Option<String> {
    let s = raw.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

#[must_use]
pub fn export_summary(route: &Route) -> String {
    let verdict = if route.verdict.satisfied {
        "satisfied"
    } else {
        "violated"
    };
    let mut s = format!(
        "score {:.2}; pareto rank {}; {}; constraints {verdict}",
        route.computed_score(),
        route.pareto_rank,
        metrics_summary(&route.metrics),
    );
    if !route.verdict.violations.is_empty() {
        let _ = write!(s, "; violations {}", route.verdict.violations.join(" | "));
    }
    s
}

/// A provider-neutral account of the durable measurements carried by a route.
#[must_use]
pub fn metrics_summary(metrics: &RouteMetrics) -> String {
    format!(
        "shape {:?}; distance {:.2} km; ascent/descent {:.0}/{:.0} m; sustained-steep {:.2} km; grade {}; lower-limb load {:.2} FGJW km; moving time {:.2} h; road {:.1}%; low-confidence {:.1}%; restricted-access {:.1}%; repeated-edge {:.1}%",
        metrics.shape,
        metrics.distance_m / 1_000.0,
        metrics.ascent_m,
        metrics.descent_m,
        metrics.sustained_steep_m / 1_000.0,
        grade_summary(metrics.grade_distribution),
        metrics.lower_limb_load_km,
        metrics.moving_time_s / 3_600.0,
        metrics.road_fraction * 100.0,
        metrics.low_confidence_fraction * 100.0,
        metrics.restricted_access_fraction * 100.0,
        metrics.repeated_edge_fraction * 100.0,
    )
}

fn grade_summary(d: GradeDistribution) -> String {
    let total = d.total_m();
    if total <= f64::EPSILON {
        return "none".to_owned();
    }
    format!(
        "flat {:.1}%, rolling {:.1}%, steep {:.1}%, savage {:.1}%",
        d.flat_m / total * 100.0,
        d.rolling_m / total * 100.0,
        d.steep_m / total * 100.0,
        d.savage_m / total * 100.0,
    )
}
