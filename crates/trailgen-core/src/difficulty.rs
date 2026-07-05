use crate::model::{Access, Edge, Terrain};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::AddAssign;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DifficultyWeights {
    #[serde(default = "default_distance_per_km")]
    pub distance_per_km: f64,
    #[serde(default = "default_ascent_per_m")]
    pub ascent_per_m: f64,
    #[serde(default = "default_descent_per_m")]
    pub descent_per_m: f64,
    #[serde(default = "default_grade_per_abs_fraction")]
    pub grade_per_abs_fraction: f64,
    #[serde(default)]
    pub terrain_multipliers: TerrainMultipliers,
    #[serde(default = "default_road_penalty")]
    pub road_penalty: f64,
    #[serde(default = "default_low_confidence_penalty")]
    pub low_confidence_penalty: f64,
    #[serde(default = "default_closed_access_penalty")]
    pub closed_access_penalty: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerrainMultipliers {
    #[serde(default = "default_unknown_terrain_multiplier")]
    pub unknown: f64,
    #[serde(default = "default_trail_terrain_multiplier")]
    pub trail: f64,
    #[serde(default = "default_forest_terrain_multiplier")]
    pub forest: f64,
    #[serde(default = "default_alpine_terrain_multiplier")]
    pub alpine: f64,
    #[serde(default = "default_talus_terrain_multiplier")]
    pub talus: f64,
    #[serde(default = "default_scramble_terrain_multiplier")]
    pub scramble: f64,
    #[serde(default = "default_pavement_terrain_multiplier")]
    pub pavement: f64,
    #[serde(default = "default_road_terrain_multiplier")]
    pub road: f64,
    #[serde(default = "default_water_terrain_multiplier")]
    pub water: f64,
}

impl Default for DifficultyWeights {
    fn default() -> Self {
        Self {
            distance_per_km: default_distance_per_km(),
            ascent_per_m: default_ascent_per_m(),
            descent_per_m: default_descent_per_m(),
            grade_per_abs_fraction: default_grade_per_abs_fraction(),
            terrain_multipliers: TerrainMultipliers::default(),
            road_penalty: default_road_penalty(),
            low_confidence_penalty: default_low_confidence_penalty(),
            closed_access_penalty: default_closed_access_penalty(),
        }
    }
}

impl Default for TerrainMultipliers {
    fn default() -> Self {
        Self {
            unknown: default_unknown_terrain_multiplier(),
            trail: default_trail_terrain_multiplier(),
            forest: default_forest_terrain_multiplier(),
            alpine: default_alpine_terrain_multiplier(),
            talus: default_talus_terrain_multiplier(),
            scramble: default_scramble_terrain_multiplier(),
            pavement: default_pavement_terrain_multiplier(),
            road: default_road_terrain_multiplier(),
            water: default_water_terrain_multiplier(),
        }
    }
}

impl DifficultyWeights {
    #[must_use]
    pub const fn terrain_multiplier(self, terrain: Terrain) -> f64 {
        self.terrain_multipliers.multiplier(terrain)
    }

    #[must_use]
    pub fn rate_edge(self, edge: &Edge) -> DifficultyBreakdown {
        let a = &edge.attr;
        let distance = a.length_m / 1_000.0 * self.distance_per_km;
        let ascent = a.ascent_m * self.ascent_per_m;
        let descent = a.descent_m * self.descent_per_m;
        let grade = a.grade_abs_mean * self.grade_per_abs_fraction;
        let terrain = distance * (self.terrain_multiplier(a.terrain) - 1.0);
        let road = a.road_exposure.clamp(0.0, 1.0) * self.road_penalty * distance;
        let confidence = (1.0 - a.confidence.clamp(0.0, 1.0)) * self.low_confidence_penalty;
        let access = match a.access {
            Access::Closed | Access::Private => self.closed_access_penalty,
            Access::Restricted => self.closed_access_penalty * 0.05,
            Access::Unknown | Access::Open => 0.0,
        };
        DifficultyBreakdown {
            distance,
            ascent,
            descent,
            grade,
            terrain,
            road,
            confidence,
            access,
        }
    }

    pub fn apply_edge(self, edge: &mut Edge) {
        let breakdown = self.rate_edge(edge);
        edge.attr.difficulty_breakdown = breakdown;
        edge.attr.difficulty = breakdown.total();
    }
}

impl TerrainMultipliers {
    #[must_use]
    pub const fn multiplier(self, terrain: Terrain) -> f64 {
        match terrain {
            Terrain::Unknown => self.unknown,
            Terrain::Trail => self.trail,
            Terrain::Forest => self.forest,
            Terrain::Alpine => self.alpine,
            Terrain::Talus => self.talus,
            Terrain::Scramble => self.scramble,
            Terrain::Pavement => self.pavement,
            Terrain::Road => self.road,
            Terrain::Water => self.water,
        }
    }
}

const fn default_unknown_terrain_multiplier() -> f64 {
    1.15
}

const fn default_distance_per_km() -> f64 {
    1.0
}

const fn default_ascent_per_m() -> f64 {
    0.012
}

const fn default_descent_per_m() -> f64 {
    0.003
}

const fn default_grade_per_abs_fraction() -> f64 {
    2.4
}

const fn default_road_penalty() -> f64 {
    2.0
}

const fn default_low_confidence_penalty() -> f64 {
    1.5
}

const fn default_closed_access_penalty() -> f64 {
    1_000.0
}

const fn default_trail_terrain_multiplier() -> f64 {
    1.0
}

const fn default_forest_terrain_multiplier() -> f64 {
    1.0
}

const fn default_alpine_terrain_multiplier() -> f64 {
    1.18
}

const fn default_talus_terrain_multiplier() -> f64 {
    1.65
}

const fn default_scramble_terrain_multiplier() -> f64 {
    2.1
}

const fn default_pavement_terrain_multiplier() -> f64 {
    0.82
}

const fn default_road_terrain_multiplier() -> f64 {
    0.9
}

const fn default_water_terrain_multiplier() -> f64 {
    2.5
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DifficultyBreakdown {
    pub distance: f64,
    pub ascent: f64,
    pub descent: f64,
    pub grade: f64,
    pub terrain: f64,
    pub road: f64,
    pub confidence: f64,
    pub access: f64,
}

impl DifficultyBreakdown {
    #[must_use]
    pub fn total(self) -> f64 {
        self.distance
            + self.ascent
            + self.descent
            + self.grade
            + self.terrain
            + self.road
            + self.confidence
            + self.access
    }

    #[must_use]
    pub const fn factors(self) -> [(DifficultyFactor, f64); 8] {
        [
            (DifficultyFactor::Distance, self.distance),
            (DifficultyFactor::Ascent, self.ascent),
            (DifficultyFactor::Descent, self.descent),
            (DifficultyFactor::Grade, self.grade),
            (DifficultyFactor::Terrain, self.terrain),
            (DifficultyFactor::Road, self.road),
            (DifficultyFactor::Confidence, self.confidence),
            (DifficultyFactor::Access, self.access),
        ]
    }
}

impl AddAssign for DifficultyBreakdown {
    fn add_assign(&mut self, rhs: Self) {
        self.distance += rhs.distance;
        self.ascent += rhs.ascent;
        self.descent += rhs.descent;
        self.grade += rhs.grade;
        self.terrain += rhs.terrain;
        self.road += rhs.road;
        self.confidence += rhs.confidence;
        self.access += rhs.access;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DifficultyFactor {
    Distance,
    Ascent,
    Descent,
    Grade,
    Terrain,
    Road,
    Confidence,
    Access,
}

impl fmt::Display for DifficultyFactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Distance => "distance",
            Self::Ascent => "ascent",
            Self::Descent => "descent",
            Self::Grade => "grade",
            Self::Terrain => "terrain",
            Self::Road => "road",
            Self::Confidence => "confidence",
            Self::Access => "access",
        };
        f.write_str(label)
    }
}
