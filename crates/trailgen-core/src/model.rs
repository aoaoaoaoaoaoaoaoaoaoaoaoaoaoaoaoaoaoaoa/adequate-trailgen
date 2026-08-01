use crate::geo::{Coord, LineString};
use crate::hiking::{EdgeTraversal, HikingModel, TraversalEstimate};
use crate::{Result, TrailgenError};
use rstar::{AABB, RTree, RTreeObject};
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

/// The source-authored structural kind of a routable segment. This is
/// orthogonal to terrain and surface: a forest footway is still a footway.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum WayKind {
    #[default]
    Unknown,
    Path,
    Footway,
    Sidewalk,
    Crossing,
    Track,
    #[serde(alias = "service")]
    ServiceRoad,
    PedestrianStreet,
    Steps,
    Bridleway,
    /// A deliberate route across ground where no path is asserted to exist.
    /// Physical ground cover remains represented by [`Terrain`].
    Bushwhack,
    #[serde(alias = "road")]
    Roadway,
    /// Bicycle-priority infrastructure whose pedestrian authority is carried
    /// independently by [`Access`].
    Cycleway,
}

/// The routing projection in which a way participates. Manual routing admits
/// every value; Finder admits only recreation and its bounded connectors.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum WayRealm {
    #[default]
    Recreational,
    Connector,
    Urban,
}

impl WayRealm {
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "connector" | "recreational-connector" => Self::Connector,
            "urban" | "pedestrian" | "circulation" => Self::Urban,
            _ => Self::Recreational,
        }
    }

    #[must_use]
    pub const fn admitted_by_finder(self) -> bool {
        matches!(self, Self::Recreational | Self::Connector)
    }
}

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum GeometryClaim {
    #[default]
    Surveyed,
    CenterlineProxy,
}

impl GeometryClaim {
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "centerline-proxy" | "road-centerline-proxy" | "proxy" => Self::CenterlineProxy,
            _ => Self::Surveyed,
        }
    }
}

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum CrossingControl {
    #[default]
    None,
    Uncontrolled,
    Marked,
    Signals,
    GradeSeparated,
}

impl CrossingControl {
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "uncontrolled" | "unmarked" | "no" => Self::Uncontrolled,
            "marked" | "zebra" | "uncontrolled-marked" => Self::Marked,
            "signals" | "signal" | "traffic-signals" => Self::Signals,
            "grade-separated" | "bridge" | "tunnel" => Self::GradeSeparated,
            _ => Self::None,
        }
    }
}

/// How much institutional and physical reality a trail line presently claims.
/// Access is independent: an established trail may be closed, while an
/// informal path may still cross public land.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum TrailStanding {
    #[default]
    Unknown,
    Established,
    Unmaintained,
    Informal,
    Historical,
}

/// Whether the path supplies deliberate wayfinding marks. This is independent
/// of physical condition, institutional standing, and permission to travel.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum TrailMarking {
    #[default]
    Unknown,
    Marked,
    Unmarked,
}

impl TrailMarking {
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "marked" | "marked trail" | "blazed" | "yes" | "symbols" | "poles" | "cairns" => {
                Self::Marked
            }
            "unmarked" | "unmarked trail" | "unblazed" | "no" | "none" => Self::Unmarked,
            _ => Self::Unknown,
        }
    }
}

impl TrailStanding {
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "established" | "maintained" | "official" | "current" => Self::Established,
            "unmaintained" | "disused" | "overgrown" => Self::Unmaintained,
            "informal" | "social" | "social-trail" | "desire-path" => Self::Informal,
            "historical" | "abandoned" | "removed" => Self::Historical,
            _ => Self::Unknown,
        }
    }
}

impl WayKind {
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "path" | "trail" | "singletrack" => Self::Path,
            "footway" => Self::Footway,
            "sidewalk" => Self::Sidewalk,
            "crossing" => Self::Crossing,
            "track" => Self::Track,
            "service" | "service-road" => Self::ServiceRoad,
            "pedestrian" | "pedestrian-way" => Self::PedestrianStreet,
            "steps" | "stairs" => Self::Steps,
            "bridleway" => Self::Bridleway,
            "bushwhack" | "bushwhacking" | "off-trail" | "offtrail" | "cross-country" => {
                Self::Bushwhack
            }
            "road" | "living_street" | "residential" | "unclassified" | "tertiary"
            | "secondary" | "primary" => Self::Roadway,
            "cycleway" | "cycle-way" => Self::Cycleway,
            _ => Self::Unknown,
        }
    }

    #[must_use]
    pub const fn road_like(self) -> bool {
        matches!(self, Self::Track | Self::ServiceRoad | Self::Roadway)
    }

    #[must_use]
    pub const fn pathless(self) -> bool {
        matches!(self, Self::Bushwhack)
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

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub junction: Option<crate::JunctionKey>,
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
    /// Mean magnitude of the surrounding terrain slope in degrees. This is
    /// distinct from grade along the trail and may be absent without a DEM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hill_slope_deg: Option<f64>,
    #[serde(default)]
    pub way_kind: WayKind,
    #[serde(default)]
    pub realm: WayRealm,
    #[serde(default)]
    pub geometry_claim: GeometryClaim,
    #[serde(default)]
    pub crossing_control: CrossingControl,
    #[serde(default)]
    pub standing: TrailStanding,
    #[serde(default)]
    pub marking: TrailMarking,
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
    pub traversal: EdgeTraversal,
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeProjection {
    pub edge: EdgeId,
    pub coord: Coord,
    pub progress_m: f64,
    pub distance_m: f64,
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

    #[must_use]
    pub const fn traversal_from(&self, from: VertexId) -> TraversalEstimate {
        self.attr.traversal.departing(self, from)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WalkGraph {
    pub vertices: Vec<Vertex>,
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub turn_bans: Vec<TurnBan>,
    #[serde(skip_serializing)]
    pub adjacency: Vec<Vec<EdgeId>>,
}

impl<'de> Deserialize<'de> for WalkGraph {
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
        for edge in &mut graph.edges {
            HikingModel.apply(edge);
        }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CoverageGapKind {
    BeyondNetwork,
    DisconnectedNetwork,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoverageGap {
    pub kind: CoverageGapKind,
    pub first_route_segment: usize,
    pub last_route_segment: usize,
    pub geometry: LineString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearest_edge: Option<EdgeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearest_distance_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_edge: Option<EdgeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_edge: Option<EdgeId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouteCoverage {
    pub edges: Vec<EdgeId>,
    pub stats: RouteSnapStats,
    pub gaps: Vec<CoverageGap>,
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
    route_segment: usize,
}

#[derive(Clone, Copy, Debug)]
struct DisconnectedSpan {
    from_segment: usize,
    to_segment: usize,
    before: EdgeId,
    after: EdgeId,
}

#[derive(Clone, Copy)]
struct EdgeEnvelope {
    edge: EdgeId,
    bounds: AABB<[f64; 2]>,
}

#[derive(Clone)]
pub struct EdgeIndex {
    tree: RTree<EdgeEnvelope>,
}

impl EdgeIndex {
    #[must_use]
    pub fn forge(graph: &WalkGraph) -> Self {
        Self {
            tree: edge_spatial_index(graph.edges.iter()),
        }
    }

    #[must_use]
    pub fn forge_allowed(graph: &WalkGraph, allowed: &[bool]) -> Self {
        assert_eq!(allowed.len(), graph.edges.len());
        Self {
            tree: edge_spatial_index(
                graph
                    .edges
                    .iter()
                    .zip(allowed)
                    .filter_map(|(edge, allowed)| allowed.then_some(edge)),
            ),
        }
    }

    #[must_use]
    pub fn project(&self, graph: &WalkGraph, coord: Coord) -> Option<EdgeProjection> {
        let (edge, distance_m) = indexed_nearest_edge(&graph.edges, &self.tree, coord)?;
        let (_, progress_m, coord) = line_projection(&graph.edges[edge.0].geometry, coord)?;
        Some(EdgeProjection {
            edge,
            coord,
            progress_m,
            distance_m,
        })
    }

    /// Returns the nearest legal-looking geometric anchors in deterministic
    /// order. Selection policy belongs to the caller; the spatial index only
    /// supplies a bounded candidate set.
    #[must_use]
    pub fn candidates(
        &self,
        graph: &WalkGraph,
        coord: Coord,
        max_distance_m: f64,
        limit: usize,
    ) -> Vec<EdgeProjection> {
        if !max_distance_m.is_finite() || max_distance_m < 0.0 || limit == 0 {
            return Vec::new();
        }
        let latitude_radius = max_distance_m / 110_540.0;
        let longitude_radius =
            max_distance_m / (111_320.0 * coord.lat.to_radians().cos().abs().max(0.01));
        let neighborhood = AABB::from_corners(
            [coord.lon - longitude_radius, coord.lat - latitude_radius],
            [coord.lon + longitude_radius, coord.lat + latitude_radius],
        );
        let mut candidates = self
            .tree
            .locate_in_envelope_intersecting(&neighborhood)
            .filter_map(|candidate| {
                let (distance_m, progress_m, anchor) =
                    line_projection(&graph.edges[candidate.edge.0].geometry, coord)?;
                (distance_m <= max_distance_m).then_some(EdgeProjection {
                    edge: candidate.edge,
                    coord: anchor,
                    progress_m,
                    distance_m,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.distance_m
                .total_cmp(&right.distance_m)
                .then_with(|| {
                    edge_anchor_rank(&graph.edges[left.edge.0])
                        .cmp(&edge_anchor_rank(&graph.edges[right.edge.0]))
                })
                .then_with(|| left.edge.cmp(&right.edge))
                .then_with(|| left.progress_m.total_cmp(&right.progress_m))
        });
        candidates.truncate(limit);
        candidates
    }
}

impl RTreeObject for EdgeEnvelope {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        self.bounds
    }
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

impl WalkGraph {
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
                || !edge.attr.traversal.valid()
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
        self.trace_coverage(line, f64::INFINITY).edges
    }

    #[must_use]
    pub fn trace_coverage(&self, line: &crate::geo::LineString, max_snap_m: f64) -> RouteCoverage {
        let edge_index = edge_spatial_index(self.edges.iter());
        let mut anchors = Vec::new();
        let mut gaps = Vec::new();
        let mut segment_count = 0usize;
        let mut snapped_segment_count = 0usize;
        let mut rejected_segment_count = 0usize;
        let mut max_observed_m = 0.0f64;
        let mut sum_observed_m = 0.0f64;
        let mut observed_count = 0.0f64;
        for (route_segment, w) in line.points.windows(2).enumerate() {
            segment_count += 1;
            let mid = w[0].lerp(w[1], 0.5);
            let Some((edge_id, snap_m)) = indexed_nearest_edge(&self.edges, &edge_index, mid)
            else {
                rejected_segment_count += 1;
                gaps.push(CoverageGap {
                    kind: CoverageGapKind::BeyondNetwork,
                    first_route_segment: route_segment,
                    last_route_segment: route_segment,
                    geometry: LineString::unchecked(w.to_vec()),
                    nearest_edge: None,
                    nearest_distance_m: None,
                    before_edge: None,
                    after_edge: None,
                });
                continue;
            };
            max_observed_m = max_observed_m.max(snap_m);
            sum_observed_m += snap_m;
            observed_count += 1.0;
            if snap_m > max_snap_m {
                rejected_segment_count += 1;
                gaps.push(CoverageGap {
                    kind: CoverageGapKind::BeyondNetwork,
                    first_route_segment: route_segment,
                    last_route_segment: route_segment,
                    geometry: LineString::unchecked(w.to_vec()),
                    nearest_edge: Some(edge_id),
                    nearest_distance_m: Some(snap_m),
                    before_edge: None,
                    after_edge: None,
                });
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
                let start_progress_m = line_progress_m(&edge.geometry, w[0]);
                let end_progress_m = line_progress_m(&edge.geometry, w[1]);
                anchors.push(SnapAnchor {
                    edge: edge_id,
                    entry: if end_progress_m >= start_progress_m {
                        edge.a
                    } else {
                        edge.b
                    },
                    budget_m: observation_m.mul_add(3.0, snap_allowance_m).max(50.0),
                    route_segment,
                });
            }
        }
        self.collapse_shadow_anchors(&mut anchors, line, max_snap_m);
        let (edges, disconnected) = self.connect_snap_anchors(&anchors);
        gaps.extend(disconnected.iter().map(|span| CoverageGap {
            kind: CoverageGapKind::DisconnectedNetwork,
            first_route_segment: span.from_segment.min(span.to_segment),
            last_route_segment: span.from_segment.max(span.to_segment),
            geometry: coverage_geometry(
                line,
                span.from_segment.min(span.to_segment),
                span.from_segment.max(span.to_segment),
            ),
            nearest_edge: None,
            nearest_distance_m: None,
            before_edge: Some(span.before),
            after_edge: Some(span.after),
        }));
        RouteCoverage {
            edges,
            stats: coverage_stats(
                segment_count,
                snapped_segment_count,
                rejected_segment_count,
                disconnected.len(),
                max_observed_m,
                sum_observed_m,
                observed_count,
            ),
            gaps: coalesce_coverage_gaps(gaps, line),
        }
    }

    fn collapse_shadow_anchors(
        &self,
        anchors: &mut Vec<SnapAnchor>,
        line: &crate::geo::LineString,
        max_snap_m: f64,
    ) {
        if !max_snap_m.is_finite() {
            return;
        }
        let mut first = 0;
        while first + 2 < anchors.len() {
            let shadowed = ((first + 2)..anchors.len()).rev().find(|&last| {
                anchors[first].edge == anchors[last].edge
                    && anchors[first].entry == anchors[last].entry
                    && route_span_fits_edge(
                        &self.edges[anchors[first].edge.0],
                        line,
                        anchors[first].route_segment,
                        anchors[last].route_segment,
                        max_snap_m,
                    )
            });
            if let Some(last) = shadowed {
                anchors.drain(first + 1..=last);
            } else {
                first += 1;
            }
        }
    }

    fn connect_snap_anchors(&self, anchors: &[SnapAnchor]) -> (Vec<EdgeId>, Vec<DisconnectedSpan>) {
        let Some(first_anchor) = anchors.first().copied() else {
            return (Vec::new(), Vec::new());
        };
        let first_id = first_anchor.edge;
        let Some(first) = self.edges.get(first_id.0) else {
            return (
                Vec::new(),
                vec![DisconnectedSpan {
                    from_segment: first_anchor.route_segment,
                    to_segment: first_anchor.route_segment,
                    before: first_id,
                    after: first_id,
                }],
            );
        };
        let start = first_anchor.entry;
        if first.traverse(start).is_none() {
            return (
                Vec::new(),
                vec![DisconnectedSpan {
                    from_segment: first_anchor.route_segment,
                    to_segment: first_anchor.route_segment,
                    before: first_id,
                    after: first_id,
                }],
            );
        }
        let mut edges = vec![first_id];
        let mut at = first
            .traverse(start)
            .expect("start was filtered as traversable");
        let mut previous = first_id;
        let mut previous_anchor = first_anchor;
        let mut disconnected = Vec::new();
        for anchor in &anchors[1..] {
            let target = anchor.edge;
            if at == anchor.entry
                && self.turn_allowed(Some(previous), at, target)
                && let Some(next) = self.edges[target.0].traverse(at)
            {
                edges.push(target);
                previous = target;
                at = next;
                previous_anchor = *anchor;
                continue;
            }
            let Some(connector) =
                self.shortest_connector(at, previous, target, anchor.entry, anchor.budget_m)
            else {
                self.record_disconnection(&mut disconnected, previous_anchor, *anchor);
                if let Some(next) = self.edges[target.0].traverse(anchor.entry) {
                    edges.clear();
                    edges.push(target);
                    at = next;
                    previous = target;
                    previous_anchor = *anchor;
                    continue;
                }
                previous_anchor = *anchor;
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
                self.record_disconnection(&mut disconnected, previous_anchor, *anchor);
            }
            previous_anchor = *anchor;
        }
        (edges, disconnected)
    }

    fn record_disconnection(
        &self,
        disconnected: &mut Vec<DisconnectedSpan>,
        before: SnapAnchor,
        after: SnapAnchor,
    ) {
        if !self.edges_connect_locally(before.edge, after.edge, before.budget_m.max(after.budget_m))
        {
            disconnected.push(DisconnectedSpan {
                from_segment: before.route_segment,
                to_segment: after.route_segment,
                before: before.edge,
                after: after.edge,
            });
        }
    }

    fn edges_connect_locally(&self, before: EdgeId, after: EdgeId, budget_m: f64) -> bool {
        let before_edge = &self.edges[before.0];
        let after_edge = &self.edges[after.0];
        [before_edge.a, before_edge.b]
            .into_iter()
            .filter_map(|entry| before_edge.traverse(entry))
            .any(|at| {
                [after_edge.a, after_edge.b]
                    .into_iter()
                    .filter(|entry| after_edge.traverse(*entry).is_some())
                    .any(|entry| {
                        (at == entry && self.turn_allowed(Some(before), at, after))
                            || self
                                .shortest_connector(at, before, after, entry, budget_m)
                                .is_some()
                    })
            })
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
        self.project_onto_edge(coord)
            .map(|projection| (projection.edge, projection.distance_m))
    }

    #[must_use]
    pub fn project_onto_edge(&self, coord: Coord) -> Option<EdgeProjection> {
        self.edges
            .iter()
            .filter_map(|edge| {
                line_projection(&edge.geometry, coord).map(|(distance_m, progress_m, coord)| {
                    EdgeProjection {
                        edge: edge.id,
                        coord,
                        progress_m,
                        distance_m,
                    }
                })
            })
            .min_by(|left, right| left.distance_m.total_cmp(&right.distance_m))
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

fn coverage_geometry(line: &LineString, first: usize, last: usize) -> LineString {
    let start = first.min(line.points.len().saturating_sub(2));
    let end = last
        .saturating_add(1)
        .min(line.points.len().saturating_sub(1));
    LineString::unchecked(line.points[start..=end.max(start + 1)].to_vec())
}

fn coalesce_coverage_gaps(mut gaps: Vec<CoverageGap>, line: &LineString) -> Vec<CoverageGap> {
    gaps.sort_by_key(|gap| (gap.first_route_segment, gap.last_route_segment));
    let mut merged = Vec::<CoverageGap>::new();
    for gap in gaps {
        let Some(previous) = merged.last_mut() else {
            merged.push(gap);
            continue;
        };
        if previous.kind != gap.kind
            || gap.first_route_segment > previous.last_route_segment.saturating_add(1)
        {
            merged.push(gap);
            continue;
        }
        previous.last_route_segment = previous.last_route_segment.max(gap.last_route_segment);
        previous.geometry = coverage_geometry(
            line,
            previous.first_route_segment,
            previous.last_route_segment,
        );
        if previous.nearest_edge != gap.nearest_edge {
            previous.nearest_edge = None;
        }
        if previous.kind == CoverageGapKind::DisconnectedNetwork {
            previous.after_edge = gap.after_edge;
        } else {
            if previous.before_edge != gap.before_edge {
                previous.before_edge = None;
            }
            if previous.after_edge != gap.after_edge {
                previous.after_edge = None;
            }
        }
        previous.nearest_distance_m = match (previous.nearest_distance_m, gap.nearest_distance_m) {
            (Some(left), Some(right)) => Some(left.max(right)),
            _ => None,
        };
    }
    merged
}

fn valid_coord(coord: Coord) -> bool {
    coord.lon.is_finite()
        && coord.lat.is_finite()
        && (-180.0..=180.0).contains(&coord.lon)
        && (-90.0..=90.0).contains(&coord.lat)
        && coord.ele.is_none_or(f64::is_finite)
}

fn edge_distance_m(edge: &Edge, coord: Coord) -> f64 {
    line_projection(&edge.geometry, coord).map_or(f64::INFINITY, |projection| projection.0)
}

fn edge_spatial_index<'a>(edges: impl IntoIterator<Item = &'a Edge>) -> RTree<EdgeEnvelope> {
    RTree::bulk_load(
        edges
            .into_iter()
            .map(|edge| {
                let (west, south, east, north) = edge.geometry.points.iter().fold(
                    (
                        f64::INFINITY,
                        f64::INFINITY,
                        f64::NEG_INFINITY,
                        f64::NEG_INFINITY,
                    ),
                    |(west, south, east, north), point| {
                        (
                            west.min(point.lon),
                            south.min(point.lat),
                            east.max(point.lon),
                            north.max(point.lat),
                        )
                    },
                );
                EdgeEnvelope {
                    edge: edge.id,
                    bounds: AABB::from_corners([west, south], [east, north]),
                }
            })
            .collect(),
    )
}

fn coverage_stats(
    segment_count: usize,
    snapped_segment_count: usize,
    rejected_segment_count: usize,
    disconnected_transition_count: usize,
    max_snap_m: f64,
    sum_snap_m: f64,
    observed_count: f64,
) -> RouteSnapStats {
    RouteSnapStats {
        segment_count,
        snapped_segment_count,
        rejected_segment_count,
        disconnected_transition_count,
        max_snap_m,
        mean_snap_m: if observed_count <= f64::EPSILON {
            0.0
        } else {
            sum_snap_m / observed_count
        },
    }
}

fn indexed_nearest_edge(
    edges: &[Edge],
    index: &RTree<EdgeEnvelope>,
    coord: Coord,
) -> Option<(EdgeId, f64)> {
    for radius_m in std::iter::successors(Some(32.0), |radius| Some(radius * 4.0)).take(11) {
        let latitude_radius = radius_m / 110_540.0;
        let longitude_radius =
            radius_m / (111_320.0 * coord.lat.to_radians().cos().abs().max(0.01));
        let neighborhood = AABB::from_corners(
            [coord.lon - longitude_radius, coord.lat - latitude_radius],
            [coord.lon + longitude_radius, coord.lat + latitude_radius],
        );
        let nearest = index
            .locate_in_envelope_intersecting(&neighborhood)
            .map(|candidate| {
                let distance_m = edge_distance_m(&edges[candidate.edge.0], coord);
                (candidate.edge, distance_m)
            })
            .min_by(|left, right| {
                left.1
                    .total_cmp(&right.1)
                    .then_with(|| {
                        edge_anchor_rank(&edges[left.0.0]).cmp(&edge_anchor_rank(&edges[right.0.0]))
                    })
                    .then_with(|| left.0.cmp(&right.0))
            });
        if nearest.is_some_and(|(_, distance_m)| distance_m <= radius_m) {
            return nearest;
        }
    }
    edges
        .iter()
        .map(|edge| (edge.id, edge_distance_m(edge, coord)))
        .min_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then_with(|| {
                    edge_anchor_rank(&edges[left.0.0]).cmp(&edge_anchor_rank(&edges[right.0.0]))
                })
                .then_with(|| left.0.cmp(&right.0))
        })
}

const fn edge_anchor_rank(edge: &Edge) -> (u8, u8, u8) {
    let access = match edge.attr.access {
        Access::Open => 0,
        Access::Unknown | Access::Restricted => 1,
        Access::Closed | Access::Private => 2,
    };
    let geometry = match edge.attr.geometry_claim {
        GeometryClaim::Surveyed => 0,
        GeometryClaim::CenterlineProxy => 1,
    };
    let function = match edge.attr.way_kind {
        WayKind::Path
        | WayKind::Footway
        | WayKind::Sidewalk
        | WayKind::Crossing
        | WayKind::PedestrianStreet
        | WayKind::Steps
        | WayKind::Bridleway
        | WayKind::Bushwhack
        | WayKind::Cycleway => 0,
        WayKind::Track => 1,
        WayKind::ServiceRoad => 2,
        WayKind::Roadway | WayKind::Unknown => 3,
    };
    (access, geometry, function)
}

fn route_span_fits_edge(
    edge: &Edge,
    line: &LineString,
    first_segment: usize,
    last_segment: usize,
    max_snap_m: f64,
) -> bool {
    line.points
        .windows(2)
        .enumerate()
        .skip(first_segment)
        .take(last_segment.saturating_sub(first_segment) + 1)
        .all(|(_, segment)| edge_distance_m(edge, segment[0].lerp(segment[1], 0.5)) <= max_snap_m)
}

fn line_progress_m(line: &LineString, point: Coord) -> f64 {
    line_projection(line, point).map_or(0.0, |projection| projection.1)
}

pub(crate) fn line_projection(line: &LineString, point: Coord) -> Option<(f64, f64, Coord)> {
    let mut traversed_m = 0.0;
    let mut nearest = None::<(f64, f64, Coord)>;
    for segment in line.points.windows(2) {
        let length_m = segment[0].haversine_m(segment[1]);
        let (distance_m, interpolation) = segment_projection(segment[0], segment[1], point);
        if nearest.is_none_or(|nearest| distance_m < nearest.0) {
            nearest = Some((
                distance_m,
                length_m.mul_add(interpolation, traversed_m),
                segment[0].lerp(segment[1], interpolation),
            ));
        }
        traversed_m += length_m;
    }
    nearest
}

fn segment_projection(head: Coord, tail: Coord, point: Coord) -> (f64, f64) {
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
        return (head_x.hypot(head_y), 0.0);
    }
    let interpolation = (-(head_x * span_x + head_y * span_y) / span_len2).clamp(0.0, 1.0);
    (
        (head_x + span_x * interpolation).hypot(head_y + span_y * interpolation),
        interpolation,
    )
}
