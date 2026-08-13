use crate::{
    geo::LineString,
    model::{Edge, EdgeAttr, Terrain, VertexId, WayKind},
};
use serde::{Deserialize, Serialize};

/// Population traversal estimates in the direction of an edge's geometry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TraversalEstimate {
    /// Mass-normalized lower-limb joint work expressed as equivalent
    /// kilometers on flat gravel.
    pub lower_limb_load_km: f64,
    /// Population moving-time estimate from the Wood et al. walking-speed GLM.
    pub moving_time_s: f64,
}

/// Estimates for both legal geometric directions of one physical edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EdgeTraversal {
    pub forward: TraversalEstimate,
    pub reverse: TraversalEstimate,
}

impl EdgeTraversal {
    #[must_use]
    pub const fn departing(self, edge: &Edge, from: VertexId) -> TraversalEstimate {
        if from.0 == edge.a.0 {
            self.forward
        } else {
            debug_assert!(from.0 == edge.b.0);
            self.reverse
        }
    }

    #[must_use]
    pub fn valid(self) -> bool {
        [self.forward, self.reverse].into_iter().all(|estimate| {
            estimate.lower_limb_load_km.is_finite()
                && estimate.lower_limb_load_km >= 0.0
                && estimate.moving_time_s.is_finite()
                && estimate.moving_time_s >= 0.0
        })
    }
}

/// The fixed, population-level Nuckols–Voloshina load and Wood time model.
///
/// This is deliberately not a bag of user-tunable coefficients. Its numbers
/// name published population priors; route quality, access, navigation, risk,
/// and personal capacity belong to other domains.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HikingModel;

impl HikingModel {
    /// Level-ground speed of the Wood population model for an ordinary path.
    #[must_use]
    pub fn reference_flat_speed_kmh() -> f64 {
        1.580_f64.exp()
    }

    #[must_use]
    pub fn estimate(self, geometry: &LineString, attr: &EdgeAttr) -> EdgeTraversal {
        self.estimate_leg(
            geometry,
            attr.way_kind,
            attr.terrain,
            attr.surface.as_deref(),
            attr.hill_slope_deg,
        )
    }

    #[must_use]
    pub fn estimate_leg(
        self,
        geometry: &LineString,
        way_kind: WayKind,
        terrain: Terrain,
        surface: Option<&str>,
        hill_slope_deg: Option<f64>,
    ) -> EdgeTraversal {
        let roughness = roughness_factor(terrain, surface);
        let wood = wood_coefficients(way_kind, terrain, surface);
        let mut forward = TraversalEstimate::default();
        let mut reverse = TraversalEstimate::default();

        for segment in geometry.points.windows(2) {
            let meters = segment[0].haversine_m(segment[1]);
            if meters <= f64::EPSILON {
                continue;
            }
            let grade = segment[0]
                .ele
                .zip(segment[1].ele)
                .map_or(0.0, |(a, b)| (b - a) / meters);
            let kilometers = meters / 1_000.0;
            forward.lower_limb_load_km = (kilometers * joint_work_factor(grade))
                .mul_add(roughness, forward.lower_limb_load_km);
            reverse.lower_limb_load_km = (kilometers * joint_work_factor(-grade))
                .mul_add(roughness, reverse.lower_limb_load_km);

            let hill_slope_deg = hill_slope_deg
                .unwrap_or_else(|| grade.atan().abs().to_degrees())
                .max(grade.atan().abs().to_degrees());
            forward.moving_time_s = (kilometers
                / wood.speed_kmh(hill_slope_deg, grade.atan().to_degrees()))
            .mul_add(3_600.0, forward.moving_time_s);
            reverse.moving_time_s = (kilometers
                / wood.speed_kmh(hill_slope_deg, (-grade).atan().to_degrees()))
            .mul_add(3_600.0, reverse.moving_time_s);
        }

        EdgeTraversal { forward, reverse }
    }

    pub fn apply(self, edge: &mut Edge) {
        edge.attr.traversal = self.estimate(&edge.geometry, &edge.attr);
    }
}

/// Nuckols et al. total positive-plus-absolute-negative lower-limb joint-power
/// ratio, shape-preserved between measured walking grades and clamped beyond
/// the measured −15%…+15% domain.
#[must_use]
pub fn joint_work_factor(grade: f64) -> f64 {
    const DESCENT_GRADE: [f64; 3] = [-0.15, -0.10, 0.0];
    const DESCENT_FACTOR: [f64; 3] = [1.69, 1.24, 1.00];
    const ASCENT_GRADE: [f64; 3] = [0.0, 0.10, 0.15];
    const ASCENT_FACTOR: [f64; 3] = [1.00, 1.18, 1.57];

    if grade <= DESCENT_GRADE[0] {
        return DESCENT_FACTOR[0];
    }
    if grade >= ASCENT_GRADE[2] {
        return ASCENT_FACTOR[2];
    }
    if grade < 0.0 {
        monotone_cubic(DESCENT_GRADE, DESCENT_FACTOR, grade)
    } else {
        monotone_cubic(ASCENT_GRADE, ASCENT_FACTOR, grade)
    }
}

fn monotone_cubic(abscissa: [f64; 3], ordinate: [f64; 3], at: f64) -> f64 {
    let width = [abscissa[1] - abscissa[0], abscissa[2] - abscissa[1]];
    let secant = [
        (ordinate[1] - ordinate[0]) / width[0],
        (ordinate[2] - ordinate[1]) / width[1],
    ];
    let tangent = [
        endpoint_slope(width[0], width[1], secant[0], secant[1]),
        interior_slope(width[0], width[1], secant[0], secant[1]),
        endpoint_slope(width[1], width[0], secant[1], secant[0]),
    ];
    let slot = usize::from(at >= abscissa[1]);
    let progress = (at - abscissa[slot]) / width[slot];
    let square = progress * progress;
    let cube = square * progress;
    let basis00 = 2.0_f64.mul_add(cube, (-3.0_f64).mul_add(square, 1.0));
    let basis10 = (-2.0_f64).mul_add(square, cube + progress);
    let basis01 = 3.0_f64.mul_add(square, -2.0 * cube);
    let basis11 = cube - square;
    let value = (basis10 * width[slot]).mul_add(tangent[slot], basis00 * ordinate[slot]);
    let value = basis01.mul_add(ordinate[slot + 1], value);
    (basis11 * width[slot]).mul_add(tangent[slot + 1], value)
}

fn endpoint_slope(width0: f64, width1: f64, secant0: f64, secant1: f64) -> f64 {
    let numerator = 2.0_f64
        .mul_add(width0, width1)
        .mul_add(secant0, -(width0 * secant1));
    let candidate = numerator / (width0 + width1);
    if candidate.signum() != secant0.signum() {
        0.0
    } else if secant0.signum() != secant1.signum() && candidate.abs() > 3.0 * secant0.abs() {
        3.0 * secant0
    } else {
        candidate
    }
}

fn interior_slope(width0: f64, width1: f64, secant0: f64, secant1: f64) -> f64 {
    if secant0 == 0.0 || secant1 == 0.0 || secant0.signum() != secant1.signum() {
        return 0.0;
    }
    let weight0 = 2.0_f64.mul_add(width1, width0);
    let weight1 = 2.0_f64.mul_add(width0, width1);
    (weight0 + weight1) / (weight0 / secant0 + weight1 / secant1)
}

fn roughness_factor(terrain: Terrain, surface: Option<&str>) -> f64 {
    let surface = surface
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if matches!(
        surface.as_str(),
        "asphalt"
            | "concrete"
            | "concreteplates"
            | "paved"
            | "pavingstones"
            | "compacted"
            | "finegravel"
            | "gravel"
    ) || matches!(terrain, Terrain::Pavement | Terrain::Road)
    {
        1.0
    } else {
        // Voloshina et al. measured a 28% increase in positive knee work on
        // modest manufactured unevenness. More severe terrain retains this
        // anchor rather than acquiring invented ratio-scale multipliers.
        1.28
    }
}

#[derive(Clone, Copy)]
struct WoodCoefficients {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
}

impl WoodCoefficients {
    fn speed_kmh(self, hill_slope_deg: f64, walking_slope_deg: f64) -> f64 {
        self.b
            .mul_add(
                hill_slope_deg.clamp(0.0, 40.0),
                self.c.mul_add(
                    walking_slope_deg.clamp(-40.0, 40.0),
                    self.d
                        .mul_add(walking_slope_deg.clamp(-40.0, 40.0).powi(2), self.a),
                ),
            )
            .exp()
            .max(0.2)
    }
}

fn wood_coefficients(
    way_kind: WayKind,
    terrain: Terrain,
    surface: Option<&str>,
) -> WoodCoefficients {
    let paved = matches!(terrain, Terrain::Pavement)
        || surface.is_some_and(|surface| {
            matches!(
                surface.trim().to_ascii_lowercase().as_str(),
                "asphalt" | "concrete" | "paved" | "paving_stones"
            )
        });
    if paved {
        return WoodCoefficients {
            a: 1.580,
            b: -0.003_89,
            c: -0.007_26,
            d: -0.002_18,
        };
    }
    if way_kind != WayKind::Bushwhack {
        return WoodCoefficients {
            a: 1.580,
            b: -0.003_89,
            c: -0.009_65,
            d: -0.002_48,
        };
    }
    let a = match terrain {
        Terrain::Forest | Terrain::Water => 1.443,
        Terrain::Unknown => 1.536,
        Terrain::Trail
        | Terrain::Alpine
        | Terrain::Talus
        | Terrain::Scramble
        | Terrain::Pavement
        | Terrain::Road => 1.580,
    };
    WoodCoefficients {
        a,
        b: -0.007_31,
        c: -0.009_65,
        d: -0.001_87,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joint_work_curve_obeys_its_empirical_envelope() {
        for (grade, expected) in [
            (-0.15, 1.69),
            (-0.10, 1.24),
            (0.0, 1.00),
            (0.10, 1.18),
            (0.15, 1.57),
        ] {
            assert!((joint_work_factor(grade) - expected).abs() <= 1.0e-12);
        }
        let descent = (-150..=0)
            .map(|slot| joint_work_factor(f64::from(slot) / 1_000.0))
            .collect::<Vec<_>>();
        assert!(descent.windows(2).all(|pair| pair[0] >= pair[1]));
        let ascent = (0..=150)
            .map(|slot| joint_work_factor(f64::from(slot) / 1_000.0))
            .collect::<Vec<_>>();
        assert!(ascent.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!((joint_work_factor(-0.50) - 1.69).abs() <= f64::EPSILON);
        assert!((joint_work_factor(0.50) - 1.57).abs() <= f64::EPSILON);
    }
}
