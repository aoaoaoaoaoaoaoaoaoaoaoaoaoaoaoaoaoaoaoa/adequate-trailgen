//! Provider-agnostic trail graph, scoring, routing, and route I/O.

pub mod alltrails;
pub mod builder;
pub mod constraints;
pub mod crs;
pub mod difficulty;
pub mod enrich;
pub mod geo;
pub mod io;
pub mod model;
pub mod optimizer;
pub mod overlay;
pub mod raster;
pub mod route;
pub mod seed;
pub mod source;
pub mod units;

pub use builder::{GraphBuilder, SegmentDraft};
pub use constraints::{ConstraintVerdict, LoopConstraints};
pub use difficulty::{
    DifficultyBreakdown, DifficultyFactor, DifficultyWeights, TerrainMultipliers,
};
pub use enrich::{
    ElevationSample, ElevationSampler, EmbeddedElevation, EnrichmentConfig, PlaneElevation,
    enrich_graph,
};
pub use geo::{Coord, LineString};
pub use model::{
    Access, CrossingEvidence, CrossingKind, Edge, EdgeAttr, EdgeId, EdgeTravel, GradeDistribution,
    Provenance, RouteSnapStats, Terrain, TrailGraph, Vertex, VertexId,
};
pub use optimizer::{ExactLoopSolver, LoopHunter, RouteSolver, SearchParams, SolverKind};
pub use overlay::{
    AccessOverlay, AccessWindow, ContextOverlay, MonthDay, OverlayGeometry, PlanningDate,
    SeasonalWindow, TerrainOverlay, apply_access_overlays, apply_context_overlays,
    apply_terrain_overlays,
};
pub use raster::{ArcAsciiGrid, GeoTiffDem, RasterCrs, RasterTransform, VrtDem};
pub use route::{
    LOW_CONFIDENCE_THRESHOLD, Route, RouteMetrics, RouteShape, is_restricted_access, rank_routes,
};
pub use seed::{SeedRoute, slug};

#[derive(Debug, thiserror::Error)]
pub enum TrailgenError {
    #[error("invalid geometry: {0}")]
    InvalidGeometry(String),
    #[error("invalid data: {0}")]
    InvalidData(String),
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("xml error: {0}")]
    Xml(String),
}

pub type Result<T> = std::result::Result<T, TrailgenError>;
