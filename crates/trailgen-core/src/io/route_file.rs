use crate::geo::LineString;
use crate::route::Route;
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
        "score {:.2}; pareto rank {}; shape {:?}; distance {:.2} km; ascent/descent {:.0}/{:.0} m; difficulty {:.2}; road {:.1}%; low-confidence {:.1}%; restricted-access {:.1}%; repeated-edge {:.1}%; constraints {verdict}",
        route.computed_score(),
        route.pareto_rank,
        route.metrics.shape,
        route.metrics.distance_m / 1_000.0,
        route.metrics.ascent_m,
        route.metrics.descent_m,
        route.metrics.difficulty,
        route.metrics.road_fraction * 100.0,
        route.metrics.low_confidence_fraction * 100.0,
        route.metrics.restricted_access_fraction * 100.0,
        route.metrics.repeated_edge_fraction * 100.0,
    );
    if !route.verdict.violations.is_empty() {
        let _ = write!(s, "; violations {}", route.verdict.violations.join(" | "));
    }
    s
}
