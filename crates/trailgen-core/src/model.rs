use crate::difficulty::DifficultyBreakdown;
use crate::geo::{Coord, LineString};
use serde::{Deserialize, Serialize};
use std::ops::AddAssign;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VertexId(pub usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EdgeId(pub usize);

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Terrain {
    #[default]
    Unknown,
    Trail,
    Forest,
    Alpine,
    Talus,
    Scramble,
    Pavement,
    Road,
    Water,
}

impl Terrain {
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "trail" | "path" | "singletrack" => Self::Trail,
            "forest" | "woods" => Self::Forest,
            "alpine" | "tundra" => Self::Alpine,
            "talus" | "scree" | "boulder" => Self::Talus,
            "scramble" | "technical" => Self::Scramble,
            "pavement" | "paved" | "asphalt" => Self::Pavement,
            "road" | "service-road" | "gravel-road" => Self::Road,
            "water" | "ford" => Self::Water,
            _ => Self::Unknown,
        }
    }

    #[must_use]
    pub fn from_landcover_tag(tag: &str) -> Self {
        let terrain = Self::from_tag(tag);
        if terrain != Self::Unknown {
            return terrain;
        }
        if let Ok(code) = tag.trim().parse::<u16>() {
            return Self::from_nlcd_code(code);
        }
        match canonical_tag(tag).as_str() {
            "openwater" | "emergentherbaceouswetlands" | "wetlands" => Self::Water,
            "perennialicesnow"
            | "shrubscrub"
            | "grasslandherbaceous"
            | "sedgeherbaceous"
            | "lichens"
            | "moss"
            | "pasturehay"
            | "cultivatedcrops" => Self::Alpine,
            "developedopenspace"
            | "developedlowintensity"
            | "developedmediumintensity"
            | "developedhighintensity"
            | "developed"
            | "urban" => Self::Pavement,
            "barrenland" | "rocksandclay" | "barren" | "bedrock" | "rock" => Self::Talus,
            "deciduousforest" | "evergreenforest" | "mixedforest" | "forestland"
            | "woodywetlands" => Self::Forest,
            _ => Self::Unknown,
        }
    }

    const fn from_nlcd_code(code: u16) -> Self {
        match code {
            11 | 95 => Self::Water,
            12 | 52 | 71 | 72 | 73 | 74 | 81 | 82 => Self::Alpine,
            21..=24 => Self::Pavement,
            31 => Self::Talus,
            41..=43 | 90 => Self::Forest,
            _ => Self::Unknown,
        }
    }
}

fn canonical_tag(tag: &str) -> String {
    tag.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Access {
    #[default]
    Unknown,
    Open,
    Restricted,
    Closed,
    Private,
}

impl Access {
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "open" | "yes" | "permissive" => Self::Open,
            "restricted" | "permit" => Self::Restricted,
            "closed" | "no" => Self::Closed,
            "private" => Self::Private,
            _ => Self::Unknown,
        }
    }
}

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum EdgeTravel {
    #[default]
    Both,
    Forward,
    Backward,
}

impl EdgeTravel {
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "both" | "bidirectional" | "two-way" | "twoway" | "2" | "no" | "false" | "0" => {
                Self::Both
            }
            "forward" | "with" | "yes" | "true" | "1" => Self::Forward,
            "backward" | "reverse" | "against" | "-1" => Self::Backward,
            _ => Self::Both,
        }
    }

    #[must_use]
    pub const fn can_depart(self, from: VertexId, a: VertexId, b: VertexId) -> bool {
        match self {
            Self::Both => from.0 == a.0 || from.0 == b.0,
            Self::Forward => from.0 == a.0,
            Self::Backward => from.0 == b.0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

impl Provenance {
    #[must_use]
    pub fn fixture(source_id: impl Into<String>) -> Self {
        Self {
            source: "fixture".to_owned(),
            layer: Some("mini-network".to_owned()),
            source_id: Some(source_id.into()),
            license: Some("CC0-fixture".to_owned()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vertex {
    pub id: VertexId,
    pub coord: crate::geo::Coord,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerrainEvidence {
    pub terrain: Terrain,
    pub confidence: f64,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CrossingKind {
    Road,
    Water,
}

impl CrossingKind {
    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag.trim().to_ascii_lowercase().as_str() {
            "road" | "roads" | "pavement" | "highway" | "street" => Some(Self::Road),
            "water" | "hydrology" | "stream" | "creek" | "river" | "ford" => Some(Self::Water),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CrossingEvidence {
    pub kind: CrossingKind,
    pub count: u32,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TurnBan {
    pub via: VertexId,
    pub from: EdgeId,
    pub to: EdgeId,
    pub provenance: Provenance,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GradeDistribution {
    pub flat_m: f64,
    pub rolling_m: f64,
    pub steep_m: f64,
    pub savage_m: f64,
}

impl GradeDistribution {
    #[must_use]
    pub fn add_segment(mut self, length_m: f64, abs_grade: f64) -> Self {
        if abs_grade < 0.05 {
            self.flat_m += length_m;
        } else if abs_grade < 0.15 {
            self.rolling_m += length_m;
        } else if abs_grade < 0.30 {
            self.steep_m += length_m;
        } else {
            self.savage_m += length_m;
        }
        self
    }

    #[must_use]
    pub fn total_m(self) -> f64 {
        self.flat_m + self.rolling_m + self.steep_m + self.savage_m
    }
}

impl AddAssign for GradeDistribution {
    fn add_assign(&mut self, rhs: Self) {
        self.flat_m += rhs.flat_m;
        self.rolling_m += rhs.rolling_m;
        self.steep_m += rhs.steep_m;
        self.savage_m += rhs.savage_m;
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeAttr {
    pub length_m: f64,
    pub ascent_m: f64,
    pub descent_m: f64,
    pub grade_abs_mean: f64,
    #[serde(default)]
    pub grade_abs_max: f64,
    #[serde(default)]
    pub sustained_steep_m: f64,
    #[serde(default)]
    pub grade_distribution: GradeDistribution,
    pub terrain: Terrain,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(default)]
    pub terrain_confidence: f64,
    #[serde(default)]
    pub terrain_evidence: Vec<TerrainEvidence>,
    pub access: Access,
    #[serde(default)]
    pub travel: EdgeTravel,
    #[serde(default)]
    pub access_confidence: f64,
    #[serde(default)]
    pub access_provenance: Vec<Provenance>,
    #[serde(default)]
    pub crossings: Vec<CrossingEvidence>,
    pub road_exposure: f64,
    pub confidence: f64,
    #[serde(default)]
    pub difficulty_breakdown: DifficultyBreakdown,
    pub difficulty: f64,
    #[serde(default)]
    pub seed_count: u32,
    #[serde(default)]
    pub popularity: f64,
    #[serde(default)]
    pub seed_provenance: Vec<Provenance>,
    #[serde(default)]
    pub elevation_provenance: Vec<Provenance>,
    pub provenance: Vec<Provenance>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: EdgeId,
    pub a: VertexId,
    pub b: VertexId,
    pub geometry: LineString,
    pub attr: EdgeAttr,
}

impl Edge {
    #[must_use]
    pub fn other(&self, v: VertexId) -> Option<VertexId> {
        if v == self.a {
            Some(self.b)
        } else if v == self.b {
            Some(self.a)
        } else {
            None
        }
    }

    #[must_use]
    pub fn traverse(&self, from: VertexId) -> Option<VertexId> {
        if !self.attr.travel.can_depart(from, self.a, self.b) {
            return None;
        }
        self.other(from)
    }

    #[must_use]
    pub fn oriented_geometry(&self, from: VertexId) -> LineString {
        if from == self.a {
            self.geometry.clone()
        } else {
            self.geometry.reversed()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrailGraph {
    pub vertices: Vec<Vertex>,
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub turn_bans: Vec<TurnBan>,
    pub adjacency: Vec<Vec<EdgeId>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteSnapStats {
    pub segment_count: usize,
    pub snapped_segment_count: usize,
    pub rejected_segment_count: usize,
    pub max_snap_m: f64,
    pub mean_snap_m: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LineSnap {
    pub edges: Vec<EdgeId>,
    pub stats: RouteSnapStats,
}

impl TrailGraph {
    #[must_use]
    pub fn new(vertices: Vec<Vertex>, edges: Vec<Edge>) -> Self {
        let mut graph = Self {
            adjacency: vec![Vec::new(); vertices.len()],
            vertices,
            edges,
            turn_bans: Vec::new(),
        };
        graph.rebuild_adjacency();
        graph
    }

    pub fn rebuild_adjacency(&mut self) {
        self.adjacency.clear();
        self.adjacency.resize_with(self.vertices.len(), Vec::new);
        for edge in &self.edges {
            if edge.attr.travel.can_depart(edge.a, edge.a, edge.b) {
                self.adjacency[edge.a.0].push(edge.id);
            }
            if edge.attr.travel.can_depart(edge.b, edge.a, edge.b) {
                self.adjacency[edge.b.0].push(edge.id);
            }
        }
    }

    #[must_use]
    pub fn nearest_vertex(&self, coord: crate::geo::Coord) -> Option<VertexId> {
        self.nearest_vertex_with_distance(coord)
            .map(|(vertex, _)| vertex)
    }

    #[must_use]
    pub fn nearest_vertex_with_distance(
        &self,
        coord: crate::geo::Coord,
    ) -> Option<(VertexId, f64)> {
        self.vertices
            .iter()
            .min_by(|a, b| {
                a.coord
                    .planar_distance2(coord)
                    .total_cmp(&b.coord.planar_distance2(coord))
            })
            .map(|v| (v.id, v.coord.haversine_m(coord)))
    }

    #[must_use]
    pub fn snap_line_edges(&self, line: &crate::geo::LineString) -> Vec<EdgeId> {
        self.snap_line_edges_within(line, f64::INFINITY).edges
    }

    #[must_use]
    pub fn snap_line_edges_within(
        &self,
        line: &crate::geo::LineString,
        max_snap_m: f64,
    ) -> LineSnap {
        let mut edges = Vec::new();
        let mut segment_count = 0usize;
        let mut snapped_segment_count = 0usize;
        let mut rejected_segment_count = 0usize;
        let mut max_observed_m = 0.0f64;
        let mut sum_observed_m = 0.0f64;
        let mut observed_count = 0.0f64;
        for w in line.points.windows(2) {
            segment_count += 1;
            let mid = w[0].lerp(w[1], 0.5);
            let Some((edge_id, snap_m)) = self.nearest_edge_with_distance(mid) else {
                rejected_segment_count += 1;
                continue;
            };
            max_observed_m = max_observed_m.max(snap_m);
            sum_observed_m += snap_m;
            observed_count += 1.0;
            if snap_m > max_snap_m {
                rejected_segment_count += 1;
                continue;
            }
            snapped_segment_count += 1;
            if edges.last().copied() != Some(edge_id) {
                edges.push(edge_id);
            }
        }
        LineSnap {
            edges,
            stats: RouteSnapStats {
                segment_count,
                snapped_segment_count,
                rejected_segment_count,
                max_snap_m: max_observed_m,
                mean_snap_m: if observed_count <= f64::EPSILON {
                    0.0
                } else {
                    sum_observed_m / observed_count
                },
            },
        }
    }

    #[must_use]
    pub fn nearest_edge(&self, coord: crate::geo::Coord) -> Option<EdgeId> {
        self.nearest_edge_with_distance(coord).map(|(edge, _)| edge)
    }

    #[must_use]
    pub fn nearest_edge_with_distance(&self, coord: crate::geo::Coord) -> Option<(EdgeId, f64)> {
        self.edges
            .iter()
            .min_by(|a, b| edge_distance_m(a, coord).total_cmp(&edge_distance_m(b, coord)))
            .map(|e| (e.id, edge_distance_m(e, coord)))
    }

    #[must_use]
    pub fn snapped_line_start(
        &self,
        line: &crate::geo::LineString,
        edges: &[EdgeId],
    ) -> Option<VertexId> {
        let first = self.edges.get(edges.first()?.0)?;
        let line_start = line.start();
        [first.a, first.b].into_iter().min_by(|a, b| {
            self.vertices[a.0]
                .coord
                .planar_distance2(line_start)
                .total_cmp(&self.vertices[b.0].coord.planar_distance2(line_start))
        })
    }

    #[must_use]
    pub fn walk_edges(&self, start: VertexId, edges: &[EdgeId]) -> Option<VertexId> {
        let mut at = start;
        let mut previous = None;
        for edge_id in edges {
            if !self.turn_allowed(previous, at, *edge_id) {
                return None;
            }
            at = self.edges.get(edge_id.0)?.traverse(at)?;
            previous = Some(*edge_id);
        }
        Some(at)
    }

    #[must_use]
    pub fn turn_allowed(&self, from: Option<EdgeId>, via: VertexId, to: EdgeId) -> bool {
        from.is_none_or(|from| {
            !self
                .turn_bans
                .iter()
                .any(|ban| ban.via == via && ban.from == from && ban.to == to)
        })
    }

    pub fn apply_seed_hints(&mut self, seed: &crate::seed::SeedRoute) {
        for edge_id in &seed.snapped_edges {
            let Some(edge) = self.edges.get_mut(edge_id.0) else {
                continue;
            };
            if !edge.attr.seed_provenance.contains(&seed.provenance) {
                edge.attr.seed_count = edge.attr.seed_count.saturating_add(1);
                edge.attr.seed_provenance.push(seed.provenance.clone());
            }
            edge.attr.popularity = f64::from(edge.attr.seed_count).ln_1p();
            edge.attr.confidence = edge.attr.confidence.max(0.82);
        }
    }
}

fn edge_distance_m(edge: &Edge, coord: Coord) -> f64 {
    edge.geometry
        .points
        .windows(2)
        .map(|w| segment_distance_m(w[0], w[1], coord))
        .min_by(f64::total_cmp)
        .unwrap_or(f64::INFINITY)
}

fn segment_distance_m(head: Coord, tail: Coord, point: Coord) -> f64 {
    let latitude_scale = point.lat.to_radians().cos();
    let meters_per_lon = 111_320.0 * latitude_scale;
    let meters_per_lat = 110_540.0;
    let head_x = (head.lon - point.lon) * meters_per_lon;
    let head_y = (head.lat - point.lat) * meters_per_lat;
    let tail_x = (tail.lon - point.lon) * meters_per_lon;
    let tail_y = (tail.lat - point.lat) * meters_per_lat;
    let span_x = tail_x - head_x;
    let span_y = tail_y - head_y;
    let span_len2 = span_x.mul_add(span_x, span_y * span_y);
    if span_len2 <= f64::EPSILON {
        return head_x.hypot(head_y);
    }
    let interpolation = (-(head_x * span_x + head_y * span_y) / span_len2).clamp(0.0, 1.0);
    (head_x + span_x * interpolation).hypot(head_y + span_y * interpolation)
}
