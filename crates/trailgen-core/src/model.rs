use crate::difficulty::DifficultyBreakdown;
use crate::geo::{Coord, LineString};
use crate::{Result, TrailgenError};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};
use std::ops::AddAssign;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VertexId(pub usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EdgeId(pub usize);

/// The source-authored structural kind of a routable way. This is orthogonal
/// to terrain and surface: a forest footway is still a footway.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum TrailClass {
    #[default]
    Unknown,
    Path,
    Footway,
    Track,
    Service,
    Pedestrian,
    Steps,
    Bridleway,
    Road,
}

impl TrailClass {
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "path" | "trail" | "singletrack" => Self::Path,
            "footway" | "sidewalk" => Self::Footway,
            "track" => Self::Track,
            "service" | "service-road" => Self::Service,
            "pedestrian" | "pedestrian-way" => Self::Pedestrian,
            "steps" | "stairs" => Self::Steps,
            "bridleway" => Self::Bridleway,
            "road" | "unclassified" | "residential" | "tertiary" => Self::Road,
            _ => Self::Unknown,
        }
    }

    #[must_use]
    pub const fn road_like(self) -> bool {
        matches!(self, Self::Track | Self::Service | Self::Road)
    }

    #[must_use]
    pub const fn requires_foot_evidence(self) -> bool {
        matches!(self, Self::Service | Self::Road)
    }
}

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
    #[serde(default)]
    pub trail_class: TrailClass,
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

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TrailGraph {
    pub vertices: Vec<Vertex>,
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub turn_bans: Vec<TurnBan>,
    #[serde(skip_serializing)]
    pub adjacency: Vec<Vec<EdgeId>>,
}

impl<'de> Deserialize<'de> for TrailGraph {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct StoredGraph {
            vertices: Vec<Vertex>,
            edges: Vec<Edge>,
            #[serde(default)]
            turn_bans: Vec<TurnBan>,
        }

        let stored = StoredGraph::deserialize(deserializer)?;
        let mut graph = Self {
            vertices: stored.vertices,
            edges: stored.edges,
            turn_bans: stored.turn_bans,
            adjacency: Vec::new(),
        };
        graph.validate().map_err(serde::de::Error::custom)?;
        graph.rebuild_adjacency();
        Ok(graph)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteSnapStats {
    pub segment_count: usize,
    pub snapped_segment_count: usize,
    pub rejected_segment_count: usize,
    #[serde(default)]
    pub disconnected_transition_count: usize,
    pub max_snap_m: f64,
    pub mean_snap_m: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LineSnap {
    pub edges: Vec<EdgeId>,
    pub stats: RouteSnapStats,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct WalkState {
    at: VertexId,
    previous: Option<EdgeId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WalkFrontier {
    cost_m: f64,
    state: WalkState,
}

#[derive(Clone, Copy, Debug)]
struct SnapAnchor {
    edge: EdgeId,
    entry: VertexId,
    budget_m: f64,
}

impl Eq for WalkFrontier {}

impl Ord for WalkFrontier {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost_m
            .total_cmp(&self.cost_m)
            .then_with(|| other.state.cmp(&self.state))
    }
}

impl PartialOrd for WalkFrontier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
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

    pub fn validate(&self) -> Result<()> {
        for (index, vertex) in self.vertices.iter().enumerate() {
            if vertex.id.0 != index {
                return Err(TrailgenError::InvalidData(format!(
                    "vertex slot {index} contains id {}",
                    vertex.id.0
                )));
            }
            if !valid_coord(vertex.coord) {
                return Err(TrailgenError::InvalidData(format!(
                    "vertex {index} has invalid coordinate"
                )));
            }
        }
        for (index, edge) in self.edges.iter().enumerate() {
            if edge.id.0 != index {
                return Err(TrailgenError::InvalidData(format!(
                    "edge slot {index} contains id {}",
                    edge.id.0
                )));
            }
            if edge.a == edge.b
                || edge.a.0 >= self.vertices.len()
                || edge.b.0 >= self.vertices.len()
            {
                return Err(TrailgenError::InvalidData(format!(
                    "edge {index} has invalid endpoints {} and {}",
                    edge.a.0, edge.b.0
                )));
            }
            if edge.geometry.points.len() < 2
                || edge
                    .geometry
                    .points
                    .iter()
                    .copied()
                    .any(|coord| !valid_coord(coord))
                || !edge.attr.length_m.is_finite()
                || edge.attr.length_m <= 0.0
            {
                return Err(TrailgenError::InvalidData(format!(
                    "edge {index} has invalid geometry or length"
                )));
            }
        }
        for ban in &self.turn_bans {
            if ban.via.0 >= self.vertices.len()
                || ban.from.0 >= self.edges.len()
                || ban.to.0 >= self.edges.len()
                || self.edges[ban.from.0]
                    .other(ban.via)
                    .is_none_or(|_| self.edges[ban.to.0].other(ban.via).is_none())
            {
                return Err(TrailgenError::InvalidData(format!(
                    "turn ban e{}→v{}→e{} does not reference incident graph members",
                    ban.from.0, ban.via.0, ban.to.0
                )));
            }
        }
        Ok(())
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
        let mut anchors = Vec::new();
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
            if anchors
                .last()
                .is_none_or(|last: &SnapAnchor| last.edge != edge_id)
            {
                let observation_m = w[0].haversine_m(w[1]);
                let snap_allowance_m = if max_snap_m.is_finite() {
                    max_snap_m * 2.0
                } else {
                    500.0
                };
                let edge = &self.edges[edge_id.0];
                let forward_m = self.vertices[edge.a.0].coord.haversine_m(w[0])
                    + self.vertices[edge.b.0].coord.haversine_m(w[1]);
                let backward_m = self.vertices[edge.b.0].coord.haversine_m(w[0])
                    + self.vertices[edge.a.0].coord.haversine_m(w[1]);
                anchors.push(SnapAnchor {
                    edge: edge_id,
                    entry: if forward_m <= backward_m {
                        edge.a
                    } else {
                        edge.b
                    },
                    budget_m: observation_m.mul_add(3.0, snap_allowance_m).max(50.0),
                });
            }
        }
        let (edges, disconnected_transition_count) = self.connect_snap_anchors(&anchors);
        LineSnap {
            edges,
            stats: RouteSnapStats {
                segment_count,
                snapped_segment_count,
                rejected_segment_count,
                disconnected_transition_count,
                max_snap_m: max_observed_m,
                mean_snap_m: if observed_count <= f64::EPSILON {
                    0.0
                } else {
                    sum_observed_m / observed_count
                },
            },
        }
    }

    fn connect_snap_anchors(&self, anchors: &[SnapAnchor]) -> (Vec<EdgeId>, usize) {
        let Some(first_anchor) = anchors.first().copied() else {
            return (Vec::new(), 0);
        };
        let first_id = first_anchor.edge;
        let Some(first) = self.edges.get(first_id.0) else {
            return (Vec::new(), 1);
        };
        let start = first_anchor.entry;
        if first.traverse(start).is_none() {
            return (Vec::new(), 1);
        }
        let mut edges = vec![first_id];
        let mut at = first
            .traverse(start)
            .expect("start was filtered as traversable");
        let mut previous = first_id;
        let mut disconnected = 0;
        for anchor in &anchors[1..] {
            let target = anchor.edge;
            if at == anchor.entry
                && self.turn_allowed(Some(previous), at, target)
                && let Some(next) = self.edges[target.0].traverse(at)
            {
                edges.push(target);
                previous = target;
                at = next;
                continue;
            }
            let Some(connector) =
                self.shortest_connector(at, previous, target, anchor.entry, anchor.budget_m)
            else {
                disconnected += 1;
                if let Some(next) = self.edges[target.0].traverse(anchor.entry) {
                    edges.clear();
                    edges.push(target);
                    at = next;
                    previous = target;
                    continue;
                }
                continue;
            };
            for edge_id in connector {
                at = self.edges[edge_id.0]
                    .traverse(at)
                    .expect("connector reconstruction preserves direction");
                previous = edge_id;
                edges.push(edge_id);
            }
            if at == anchor.entry
                && self.turn_allowed(Some(previous), at, target)
                && let Some(next) = self.edges[target.0].traverse(at)
            {
                edges.push(target);
                previous = target;
                at = next;
            } else {
                disconnected += 1;
            }
        }
        (edges, disconnected)
    }

    fn shortest_connector(
        &self,
        from: VertexId,
        previous: EdgeId,
        target: EdgeId,
        target_entry: VertexId,
        max_distance_m: f64,
    ) -> Option<Vec<EdgeId>> {
        let origin = WalkState {
            at: from,
            previous: Some(previous),
        };
        let mut frontier = BinaryHeap::from([WalkFrontier {
            cost_m: 0.0,
            state: origin,
        }]);
        let mut distance = BTreeMap::from([(origin, 0.0)]);
        let mut predecessor = BTreeMap::<WalkState, (WalkState, EdgeId)>::new();
        while let Some(WalkFrontier { cost_m, state }) = frontier.pop() {
            if cost_m > max_distance_m
                || distance
                    .get(&state)
                    .is_some_and(|best| cost_m > *best + f64::EPSILON)
            {
                continue;
            }
            if state.at == target_entry
                && self.turn_allowed(state.previous, state.at, target)
                && self.edges[target.0].traverse(state.at).is_some()
            {
                let mut path = Vec::new();
                let mut cursor = state;
                while cursor != origin {
                    let (prior, edge) = predecessor.get(&cursor).copied()?;
                    path.push(edge);
                    cursor = prior;
                }
                path.reverse();
                return Some(path);
            }
            for edge_id in self.adjacency.get(state.at.0)?.iter().copied() {
                if edge_id == target || !self.turn_allowed(state.previous, state.at, edge_id) {
                    continue;
                }
                let edge = self.edges.get(edge_id.0)?;
                let Some(to) = edge.traverse(state.at) else {
                    continue;
                };
                let next_cost = cost_m + edge.attr.length_m;
                if next_cost > max_distance_m {
                    continue;
                }
                let next = WalkState {
                    at: to,
                    previous: Some(edge_id),
                };
                if distance.get(&next).is_none_or(|best| next_cost < *best) {
                    distance.insert(next, next_cost);
                    predecessor.insert(next, (state, edge_id));
                    frontier.push(WalkFrontier {
                        cost_m: next_cost,
                        state: next,
                    });
                }
            }
        }
        None
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
        [first.a, first.b]
            .into_iter()
            .filter(|start| self.walk_edges(*start, edges).is_some())
            .min_by(|a, b| {
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

fn valid_coord(coord: Coord) -> bool {
    coord.lon.is_finite()
        && coord.lat.is_finite()
        && (-180.0..=180.0).contains(&coord.lon)
        && (-90.0..=90.0).contains(&coord.lat)
        && coord.ele.is_none_or(f64::is_finite)
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
