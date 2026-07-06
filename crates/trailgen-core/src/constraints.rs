use crate::model::Terrain;
use crate::route::{RouteMetrics, RouteShape};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopConstraints {
    #[serde(default = "default_min_distance_m")]
    pub min_distance_m: f64,
    #[serde(default = "default_max_distance_m")]
    pub max_distance_m: f64,
    #[serde(default)]
    pub min_difficulty: f64,
    #[serde(default = "default_max_difficulty")]
    pub max_difficulty: f64,
    #[serde(default)]
    pub min_ascent_m: f64,
    #[serde(default = "default_max_elevation_m")]
    pub max_ascent_m: f64,
    #[serde(default)]
    pub min_descent_m: f64,
    #[serde(default = "default_max_elevation_m")]
    pub max_descent_m: f64,
    #[serde(default = "default_max_road_fraction")]
    pub max_road_fraction: f64,
    #[serde(default = "default_max_low_confidence_fraction")]
    pub max_low_confidence_fraction: f64,
    #[serde(default)]
    pub max_restricted_access_fraction: f64,
    #[serde(default)]
    pub max_repeated_edge_fraction: f64,
    #[serde(default = "default_allowed_shapes")]
    pub allowed_shapes: Vec<RouteShape>,
    #[serde(default)]
    pub forbidden_terrain: Vec<Terrain>,
    #[serde(default)]
    pub min_terrain_fraction: BTreeMap<Terrain, f64>,
    #[serde(default)]
    pub max_terrain_fraction: BTreeMap<Terrain, f64>,
}

impl Default for LoopConstraints {
    fn default() -> Self {
        Self {
            min_distance_m: 35_000.0,
            max_distance_m: 50_000.0,
            min_difficulty: 0.0,
            max_difficulty: 90.0,
            min_ascent_m: 0.0,
            max_ascent_m: 3_000.0,
            min_descent_m: 0.0,
            max_descent_m: 3_000.0,
            max_road_fraction: 0.12,
            max_low_confidence_fraction: 0.20,
            max_restricted_access_fraction: 0.0,
            max_repeated_edge_fraction: 0.0,
            allowed_shapes: default_allowed_shapes(),
            forbidden_terrain: Vec::new(),
            min_terrain_fraction: BTreeMap::new(),
            max_terrain_fraction: BTreeMap::new(),
        }
    }
}

impl LoopConstraints {
    #[must_use]
    pub fn judge(&self, metrics: &RouteMetrics) -> ConstraintVerdict {
        let mut violations = Vec::new();
        self.append_distance_violations(metrics, &mut violations);
        self.append_difficulty_violations(metrics, &mut violations);
        self.append_elevation_violations(metrics, &mut violations);
        self.append_fraction_violations(metrics, &mut violations);
        self.append_shape_violations(metrics, &mut violations);
        self.append_terrain_violations(metrics, &mut violations);
        ConstraintVerdict {
            satisfied: violations.is_empty(),
            violations,
            audit: self.audit(metrics),
            penalty: self.penalty(metrics),
        }
    }

    #[must_use]
    pub fn audit(&self, metrics: &RouteMetrics) -> Vec<ConstraintAudit> {
        let mut audit = self.core_audit(metrics);
        self.append_terrain_audit(metrics, &mut audit);
        audit
    }

    fn core_audit(&self, metrics: &RouteMetrics) -> Vec<ConstraintAudit> {
        vec![
            min_check(
                "minimum distance",
                metrics.distance_m / 1_000.0,
                self.min_distance_m / 1_000.0,
                "km",
                2,
            ),
            max_check(
                "maximum distance",
                metrics.distance_m / 1_000.0,
                self.max_distance_m / 1_000.0,
                "km",
                2,
            ),
            min_check(
                "minimum difficulty",
                metrics.difficulty,
                self.min_difficulty,
                "",
                2,
            ),
            max_check(
                "maximum difficulty",
                metrics.difficulty,
                self.max_difficulty,
                "",
                2,
            ),
            min_check(
                "minimum ascent",
                metrics.ascent_m,
                self.min_ascent_m,
                "m",
                0,
            ),
            max_check(
                "maximum ascent",
                metrics.ascent_m,
                self.max_ascent_m,
                "m",
                0,
            ),
            min_check(
                "minimum descent",
                metrics.descent_m,
                self.min_descent_m,
                "m",
                0,
            ),
            max_check(
                "maximum descent",
                metrics.descent_m,
                self.max_descent_m,
                "m",
                0,
            ),
            max_check(
                "maximum road/pavement exposure",
                metrics.road_fraction * 100.0,
                self.max_road_fraction * 100.0,
                "%",
                1,
            ),
            max_check(
                "maximum low-confidence exposure",
                metrics.low_confidence_fraction * 100.0,
                self.max_low_confidence_fraction * 100.0,
                "%",
                1,
            ),
            max_check(
                "maximum restricted-access exposure",
                metrics.restricted_access_fraction * 100.0,
                self.max_restricted_access_fraction * 100.0,
                "%",
                1,
            ),
            max_check(
                "maximum repeated-edge exposure",
                metrics.repeated_edge_fraction * 100.0,
                self.max_repeated_edge_fraction * 100.0,
                "%",
                1,
            ),
            shape_check(metrics.shape, &self.allowed_shapes),
        ]
    }

    fn append_terrain_audit(&self, metrics: &RouteMetrics, audit: &mut Vec<ConstraintAudit>) {
        let terrain_fraction = metrics.terrain_percentages();
        audit.extend(self.forbidden_terrain.iter().map(|terrain| {
            let fraction = terrain_fraction.get(terrain).copied().unwrap_or_default() * 100.0;
            ConstraintAudit {
                metric: format!("forbidden terrain {terrain:?}"),
                measured: percent(fraction, 1),
                requirement: "must be absent".to_owned(),
                margin: if fraction <= f64::EPSILON {
                    "absent".to_owned()
                } else {
                    format!("violates by {}", percent(fraction, 1))
                },
                satisfied: fraction <= f64::EPSILON,
            }
        }));
        audit.extend(self.min_terrain_fraction.iter().map(|(terrain, minimum)| {
            min_check(
                &format!("minimum terrain {terrain:?}"),
                terrain_fraction.get(terrain).copied().unwrap_or_default() * 100.0,
                minimum * 100.0,
                "%",
                1,
            )
        }));
        audit.extend(self.max_terrain_fraction.iter().map(|(terrain, maximum)| {
            max_check(
                &format!("maximum terrain {terrain:?}"),
                terrain_fraction.get(terrain).copied().unwrap_or_default() * 100.0,
                maximum * 100.0,
                "%",
                1,
            )
        }));
    }

    fn append_distance_violations(&self, metrics: &RouteMetrics, violations: &mut Vec<String>) {
        push_violation(
            violations,
            metrics.distance_m < self.min_distance_m,
            format!(
                "distance {:.2} km below minimum {:.2} km",
                metrics.distance_m / 1_000.0,
                self.min_distance_m / 1_000.0
            ),
        );
        push_violation(
            violations,
            metrics.distance_m > self.max_distance_m,
            format!(
                "distance {:.2} km above maximum {:.2} km",
                metrics.distance_m / 1_000.0,
                self.max_distance_m / 1_000.0
            ),
        );
    }

    fn append_difficulty_violations(&self, metrics: &RouteMetrics, violations: &mut Vec<String>) {
        push_violation(
            violations,
            metrics.difficulty < self.min_difficulty,
            format!(
                "difficulty {:.2} below minimum {:.2}",
                metrics.difficulty, self.min_difficulty
            ),
        );
        push_violation(
            violations,
            metrics.difficulty > self.max_difficulty,
            format!(
                "difficulty {:.2} above maximum {:.2}",
                metrics.difficulty, self.max_difficulty
            ),
        );
    }

    fn append_elevation_violations(&self, metrics: &RouteMetrics, violations: &mut Vec<String>) {
        push_violation(
            violations,
            metrics.ascent_m < self.min_ascent_m,
            format!(
                "ascent {:.0} m below minimum {:.0} m",
                metrics.ascent_m, self.min_ascent_m
            ),
        );
        push_violation(
            violations,
            metrics.ascent_m > self.max_ascent_m,
            format!(
                "ascent {:.0} m above maximum {:.0} m",
                metrics.ascent_m, self.max_ascent_m
            ),
        );
        push_violation(
            violations,
            metrics.descent_m < self.min_descent_m,
            format!(
                "descent {:.0} m below minimum {:.0} m",
                metrics.descent_m, self.min_descent_m
            ),
        );
        push_violation(
            violations,
            metrics.descent_m > self.max_descent_m,
            format!(
                "descent {:.0} m above maximum {:.0} m",
                metrics.descent_m, self.max_descent_m
            ),
        );
    }

    fn append_fraction_violations(&self, metrics: &RouteMetrics, violations: &mut Vec<String>) {
        push_violation(
            violations,
            metrics.road_fraction > self.max_road_fraction,
            format!(
                "road/pavement fraction {:.1}% above maximum {:.1}%",
                metrics.road_fraction * 100.0,
                self.max_road_fraction * 100.0
            ),
        );
        push_violation(
            violations,
            metrics.low_confidence_fraction > self.max_low_confidence_fraction,
            format!(
                "low-confidence fraction {:.1}% above maximum {:.1}%",
                metrics.low_confidence_fraction * 100.0,
                self.max_low_confidence_fraction * 100.0
            ),
        );
        push_violation(
            violations,
            metrics.repeated_edge_fraction > self.max_repeated_edge_fraction,
            format!(
                "repeated-edge fraction {:.1}% above maximum {:.1}%",
                metrics.repeated_edge_fraction * 100.0,
                self.max_repeated_edge_fraction * 100.0
            ),
        );
        push_violation(
            violations,
            metrics.restricted_access_fraction > self.max_restricted_access_fraction,
            format!(
                "restricted-access fraction {:.1}% above maximum {:.1}%",
                metrics.restricted_access_fraction * 100.0,
                self.max_restricted_access_fraction * 100.0
            ),
        );
    }

    fn append_shape_violations(&self, metrics: &RouteMetrics, violations: &mut Vec<String>) {
        push_violation(
            violations,
            !self.allows_shape(metrics.shape),
            format!(
                "route shape {:?} is not in allowed shapes {:?}",
                metrics.shape, self.allowed_shapes
            ),
        );
    }

    fn append_terrain_violations(&self, metrics: &RouteMetrics, violations: &mut Vec<String>) {
        let terrain_fraction = metrics.terrain_percentages();
        for terrain in &self.forbidden_terrain {
            let fraction = terrain_fraction.get(terrain).copied().unwrap_or_default();
            push_violation(
                violations,
                fraction > 0.0,
                format!(
                    "forbidden terrain {terrain:?} present at {:.1}%",
                    fraction * 100.0
                ),
            );
        }
        for (terrain, minimum) in &self.min_terrain_fraction {
            let fraction = terrain_fraction.get(terrain).copied().unwrap_or_default();
            push_violation(
                violations,
                fraction < *minimum,
                format!(
                    "terrain {terrain:?} fraction {:.1}% below minimum {:.1}%",
                    fraction * 100.0,
                    minimum * 100.0
                ),
            );
        }
        for (terrain, maximum) in &self.max_terrain_fraction {
            let fraction = terrain_fraction.get(terrain).copied().unwrap_or_default();
            push_violation(
                violations,
                fraction > *maximum,
                format!(
                    "terrain {terrain:?} fraction {:.1}% above maximum {:.1}%",
                    fraction * 100.0,
                    maximum * 100.0
                ),
            );
        }
    }

    #[must_use]
    pub fn allows_shape(&self, shape: RouteShape) -> bool {
        self.allowed_shapes.contains(&shape)
    }

    #[must_use]
    pub fn penalty(&self, m: &RouteMetrics) -> f64 {
        let distance_under =
            ((self.min_distance_m - m.distance_m) / self.min_distance_m.max(1.0)).max(0.0);
        let distance_over =
            ((m.distance_m - self.max_distance_m) / self.max_distance_m.max(1.0)).max(0.0);
        let difficulty_under =
            ((self.min_difficulty - m.difficulty) / self.min_difficulty.max(1.0)).max(0.0);
        let difficulty_over =
            ((m.difficulty - self.max_difficulty) / self.max_difficulty.max(1.0)).max(0.0);
        let ascent_under = ((self.min_ascent_m - m.ascent_m) / self.min_ascent_m.max(1.0)).max(0.0);
        let ascent_over = ((m.ascent_m - self.max_ascent_m) / self.max_ascent_m.max(1.0)).max(0.0);
        let descent_under =
            ((self.min_descent_m - m.descent_m) / self.min_descent_m.max(1.0)).max(0.0);
        let descent_over =
            ((m.descent_m - self.max_descent_m) / self.max_descent_m.max(1.0)).max(0.0);
        let road_over = ((m.road_fraction - self.max_road_fraction)
            / self.max_road_fraction.max(0.01))
        .max(0.0);
        let low_conf_over = ((m.low_confidence_fraction - self.max_low_confidence_fraction)
            / self.max_low_confidence_fraction.max(0.01))
        .max(0.0);
        let restricted_access_over = ((m.restricted_access_fraction
            - self.max_restricted_access_fraction)
            / self.max_restricted_access_fraction.max(0.01))
        .max(0.0);
        let repeated_over = ((m.repeated_edge_fraction - self.max_repeated_edge_fraction)
            / self.max_repeated_edge_fraction.max(0.01))
        .max(0.0);
        let shape = if self.allows_shape(m.shape) { 0.0 } else { 4.0 };
        let terrain_fraction = m.terrain_percentages();
        let forbidden = self
            .forbidden_terrain
            .iter()
            .map(|terrain| terrain_fraction.get(terrain).copied().unwrap_or_default() * 4.0)
            .sum::<f64>();
        let terrain_under = self
            .min_terrain_fraction
            .iter()
            .map(|(terrain, minimum)| {
                ((minimum - terrain_fraction.get(terrain).copied().unwrap_or_default())
                    / minimum.max(0.01))
                .max(0.0)
            })
            .sum::<f64>();
        let terrain_over = self
            .max_terrain_fraction
            .iter()
            .map(|(terrain, maximum)| {
                ((terrain_fraction.get(terrain).copied().unwrap_or_default() - maximum)
                    / maximum.max(0.01))
                .max(0.0)
            })
            .sum::<f64>();
        100.0
            * (distance_under
                + distance_over
                + difficulty_under
                + difficulty_over
                + ascent_under
                + ascent_over
                + descent_under
                + descent_over
                + road_over
                + low_conf_over
                + restricted_access_over
                + repeated_over
                + shape
                + forbidden
                + terrain_under
                + terrain_over)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConstraintAudit {
    pub metric: String,
    pub measured: String,
    pub requirement: String,
    pub margin: String,
    pub satisfied: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConstraintVerdict {
    pub satisfied: bool,
    pub violations: Vec<String>,
    #[serde(default)]
    pub audit: Vec<ConstraintAudit>,
    pub penalty: f64,
}

fn min_check(
    metric: &str,
    value: f64,
    minimum: f64,
    unit: &str,
    decimals: usize,
) -> ConstraintAudit {
    ConstraintAudit {
        metric: metric.to_owned(),
        measured: measure(value, unit, decimals),
        requirement: format!("≥ {}", measure(minimum, unit, decimals)),
        margin: signed_measure(value - minimum, unit, decimals),
        satisfied: value >= minimum,
    }
}

fn max_check(
    metric: &str,
    value: f64,
    maximum: f64,
    unit: &str,
    decimals: usize,
) -> ConstraintAudit {
    ConstraintAudit {
        metric: metric.to_owned(),
        measured: measure(value, unit, decimals),
        requirement: format!("≤ {}", measure(maximum, unit, decimals)),
        margin: signed_measure(maximum - value, unit, decimals),
        satisfied: value <= maximum,
    }
}

fn shape_check(shape: RouteShape, allowed: &[RouteShape]) -> ConstraintAudit {
    let satisfied = allowed.contains(&shape);
    ConstraintAudit {
        metric: "allowed shape".to_owned(),
        measured: format!("{shape:?}"),
        requirement: format!("one of {allowed:?}"),
        margin: if satisfied { "allowed" } else { "disallowed" }.to_owned(),
        satisfied,
    }
}

fn measure(value: f64, unit: &str, decimals: usize) -> String {
    let value = format!("{value:.decimals$}");
    if unit.is_empty() {
        value
    } else if unit == "%" {
        format!("{value}%")
    } else {
        format!("{value} {unit}")
    }
}

fn signed_measure(value: f64, unit: &str, decimals: usize) -> String {
    let value = format!("{value:+.decimals$}");
    if unit.is_empty() {
        value
    } else if unit == "%" {
        format!("{value}%")
    } else {
        format!("{value} {unit}")
    }
}

fn percent(value: f64, decimals: usize) -> String {
    format!("{value:.decimals$}%")
}

fn push_violation(xs: &mut Vec<String>, bad: bool, msg: String) {
    if bad {
        xs.push(msg);
    }
}

fn default_allowed_shapes() -> Vec<RouteShape> {
    vec![RouteShape::Loop]
}

const fn default_min_distance_m() -> f64 {
    35_000.0
}

const fn default_max_distance_m() -> f64 {
    50_000.0
}

const fn default_max_difficulty() -> f64 {
    90.0
}

const fn default_max_elevation_m() -> f64 {
    3_000.0
}

const fn default_max_road_fraction() -> f64 {
    0.12
}

const fn default_max_low_confidence_fraction() -> f64 {
    0.20
}
