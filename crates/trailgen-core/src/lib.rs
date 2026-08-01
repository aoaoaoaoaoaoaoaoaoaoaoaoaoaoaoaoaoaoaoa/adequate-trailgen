//! Provider-agnostic trail graph, scoring, routing, and route I/O.

pub mod alltrails;
pub mod builder;
pub mod cache;
pub mod conflate;
pub mod constraints;
pub mod crs;
pub mod enrich;
pub mod geo;
pub mod hiking;
pub mod io;
pub mod milp;
pub mod model;
pub mod optimizer;
pub mod overlay;
pub mod raster;
pub mod route;
pub mod routing;
pub mod seed;
pub mod source;
pub mod trail;

pub use builder::{
    DEFAULT_SNAP_TOLERANCE_M, GraphBuilder, JunctionKey, JunctionPolicy, SegmentDraft,
    TurnRestrictionDraft, TurnRestrictionRule,
};
pub use cache::{GRAPH_CACHE, decode_graph, encode_graph};
pub use conflate::{
    ConflatedNetwork, ConflationDecision, ConflationPolicy, ConflationReport, ConflationStats,
    NetworkStratum, conflate,
};
pub use constraints::{
    ConstraintAudit, ConstraintVerdict, DEFAULT_MAX_DISTANCE_M, DEFAULT_MIN_DISTANCE_M,
    LoopConstraints,
};
pub use enrich::{
    ElevationMosaic, ElevationSample, ElevationSampler, EmbeddedElevation, EnrichmentConfig,
    PlaneElevation, enrich_graph,
};
pub use geo::{Coord, LineString};
pub use hiking::{EdgeTraversal, HikingModel, TraversalEstimate, joint_work_factor};
pub use milp::{
    LinearRow, LinearSense, LinearTerm, LoopMilpFormulation, MilpIncumbentError, MilpSelectedArc,
    VariableBound, route_edges_from_selected_arcs, route_edges_from_solution,
    selected_arcs_from_solution,
};
pub use model::{
    Access, CoverageGap, CoverageGapKind, CrossingControl, CrossingEvidence, CrossingKind, Edge,
    EdgeAttr, EdgeId, EdgeIndex, EdgeProjection, EdgeTravel, GeometryClaim, GradeDistribution,
    Provenance, RouteCoverage, RouteSnapStats, Terrain, TrailMarking, TrailStanding, TurnBan,
    Vertex, VertexId, WalkGraph, WayKind, WayRealm,
};
pub use optimizer::{
    EdgeDisposition, EdgeEdicts, ExactLoopSolver, LoopHunter, RouteSolver, SearchMonitor,
    SearchParams, SearchProgress, SearchScope, SearchStage, SolverKind,
};
pub use overlay::{
    AccessOverlay, AccessWindow, ContextOverlay, DailyTimeWindow, MonthDay, OverlayGeometry,
    PlanningDate, PlanningMoment, PlanningTime, SeasonalWindow, TerrainOverlay, Weekday,
    WeekdaySet, apply_access_overlays, apply_access_overlays_at, apply_context_overlays,
    apply_terrain_overlays,
};
pub use raster::{ArcAsciiGrid, GeoTiffDem, RasterCrs, RasterDem, RasterTransform, VrtDem};
pub use route::{
    LOW_CONFIDENCE_THRESHOLD, Route, RouteMetrics, RouteShape, is_restricted_access, rank_routes,
};
pub use routing::{RouteRequest, RoutingWorkspace, WalkRealmIndex, WalkRouter};
pub use seed::{SeedRoute, artifact_key};
pub use trail::{
    DEFAULT_ROAD_AVERSION, RoutingLaw, SupportBinding, SupportInsertion, SupportPoint, Trail,
    TrailRealization, TrailReversal,
};

#[derive(Debug, thiserror::Error)]
pub enum TrailgenError {
    #[error("invalid geometry: {0}")]
    InvalidGeometry(String),
    #[error("invalid data: {0}")]
    InvalidData(String),
    #[error("support points realize {actual:?}, not {expected:?}")]
    ShapeMismatch {
        actual: RouteShape,
        expected: RouteShape,
    },
    #[error("this loop uses a one-way segment")]
    OneWayReversal,
    #[error("the reversed walk violates a turn restriction")]
    TurnRestrictedReversal,
    #[error("the reversed walk cannot be encoded as support points")]
    UnrepresentableReversal,
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("xml error: {0}")]
    Xml(String),
}

pub type Result<T> = std::result::Result<T, TrailgenError>;
