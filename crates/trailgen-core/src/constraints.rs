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
            penalty: self.penalty(metrics),
        }
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
                "road fraction {:.1}% above maximum {:.1}%",
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConstraintVerdict {
    pub satisfied: bool,
    pub violations: Vec<String>,
    pub penalty: f64,
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
