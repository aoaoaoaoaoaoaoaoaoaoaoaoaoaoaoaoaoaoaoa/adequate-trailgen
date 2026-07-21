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
    #[serde(default = "default_technical_penalty")]
    pub technical_penalty: f64,
    #[serde(default = "default_navigation_penalty")]
    pub navigation_penalty: f64,
    #[serde(default = "default_bushwhack_penalty")]
    pub bushwhack_penalty: f64,
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
            technical_penalty: default_technical_penalty(),
            navigation_penalty: default_navigation_penalty(),
            bushwhack_penalty: default_bushwhack_penalty(),
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
        let distance_km = a.length_m / 1_000.0;
        let distance = distance_km * self.distance_per_km;
        let ascent = a.ascent_m * self.ascent_per_m;
        let descent = a.descent_m * self.descent_per_m;
        let grade = a.grade_abs_mean * self.grade_per_abs_fraction * distance_km;
        let terrain = distance_km * (self.terrain_multiplier(a.terrain) - 1.0);
        let road = a.road_exposure.clamp(0.0, 1.0) * self.road_penalty * distance_km;
        let technical = technical_pressure(edge) * self.technical_penalty * distance_km;
        let navigation = navigation_pressure(edge) * self.navigation_penalty * distance_km;
        let bushwhack = f64::from(a.trail_class.pathless()) * self.bushwhack_penalty * distance_km;
        let confidence =
            (1.0 - a.confidence.clamp(0.0, 1.0)) * self.low_confidence_penalty * distance_km;
        let access = match a.access {
            Access::Closed | Access::Private => self.closed_access_penalty * distance_km,
            Access::Restricted => self.closed_access_penalty * 0.05 * distance_km,
            Access::Unknown | Access::Open => 0.0,
        };
        DifficultyBreakdown {
            distance,
            ascent,
            descent,
            grade,
            terrain,
            road,
            technical,
            navigation,
            bushwhack,
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

const fn default_technical_penalty() -> f64 {
    1.4
}

const fn default_navigation_penalty() -> f64 {
    0.8
}

const fn default_bushwhack_penalty() -> f64 {
    3.0
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
    #[serde(default)]
    pub technical: f64,
    #[serde(default)]
    pub navigation: f64,
    #[serde(default)]
    pub bushwhack: f64,
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
            + self.technical
            + self.navigation
            + self.bushwhack
            + self.confidence
            + self.access
    }

    #[must_use]
    pub const fn factors(self) -> [(DifficultyFactor, f64); 11] {
        [
            (DifficultyFactor::Distance, self.distance),
            (DifficultyFactor::Ascent, self.ascent),
            (DifficultyFactor::Descent, self.descent),
            (DifficultyFactor::Grade, self.grade),
            (DifficultyFactor::Terrain, self.terrain),
            (DifficultyFactor::Road, self.road),
            (DifficultyFactor::Technical, self.technical),
            (DifficultyFactor::Navigation, self.navigation),
            (DifficultyFactor::Bushwhack, self.bushwhack),
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
        self.technical += rhs.technical;
        self.navigation += rhs.navigation;
        self.bushwhack += rhs.bushwhack;
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
    Technical,
    Navigation,
    Bushwhack,
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
            Self::Technical => "technical",
            Self::Navigation => "navigation",
            Self::Bushwhack => "bushwhack",
            Self::Confidence => "confidence",
            Self::Access => "access",
        };
        f.write_str(label)
    }
}

fn technical_pressure(edge: &Edge) -> f64 {
    let terrain = match edge.attr.terrain {
        Terrain::Unknown | Terrain::Trail | Terrain::Forest | Terrain::Pavement | Terrain::Road => {
            0.0
        }
        Terrain::Alpine => 0.25,
        Terrain::Talus => 0.85,
        Terrain::Scramble => 1.40,
        Terrain::Water => 1.00,
    };
    let grade = edge.attr.grade_distribution;
    let total = grade.total_m().max(edge.attr.length_m).max(1.0);
    let steep = grade.steep_m / total;
    let savage = grade.savage_m / total;
    terrain + steep.mul_add(0.35, savage)
}

fn navigation_pressure(edge: &Edge) -> f64 {
    let unknown = f64::from(edge.attr.terrain == Terrain::Unknown) * 0.80;
    let weak_terrain_evidence = (1.0 - edge.attr.terrain_confidence.clamp(0.0, 1.0)) * 0.40;
    let crossing_complexity = edge
        .attr
        .crossings
        .iter()
        .map(|x| match x.kind {
            crate::model::CrossingKind::Road => f64::from(x.count) * 0.05,
            crate::model::CrossingKind::Water => f64::from(x.count) * 0.20,
        })
        .sum::<f64>()
        .min(1.0);
    unknown + weak_terrain_evidence + crossing_complexity
}
