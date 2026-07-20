use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Debug, Write as _};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use trailgen_core::alltrails::alltrails_plans;
use trailgen_core::io::route_file::RouteFile;
use trailgen_core::io::{csv, geojson, gpx, json_route, kml, kmz, osm, report, shapefile as shp};
use trailgen_core::model::TerrainEvidence;
use trailgen_core::source::{
    GeoBounds, SourceCandidate, SourceCoverage, SourceCoverageStatus, SourceCoverageSummary,
    SourceFingerprint, SourceKind, SourceManifest, SourcePriority, adapter_registry, classify_path,
    discovery_recommendations, source_coverage, summarize_source_coverage,
};
use trailgen_core::{
    Access, AccessOverlay, AccessWindow, ArcAsciiGrid, ConstraintAudit, Coord, CrossingKind,
    DifficultyBreakdown, DifficultyWeights, EdgeAttr, EdgeId, EdgeTravel, ElevationMosaic,
    EnrichmentConfig, GeoTiffDem, GradeDistribution, GraphBuilder, JunctionPolicy,
    LOW_CONFIDENCE_THRESHOLD, LineString, LoopConstraints, LoopMilpFormulation, PlanningDate,
    PlanningMoment, PlanningTime, Provenance, RasterDem, Route, RouteMetrics, RouteShape,
    RouteSnapStats, SearchParams, SeedRoute, SegmentDraft, SolverKind, Terrain, TrailGraph,
    VertexId, VrtDem, apply_access_overlays, apply_access_overlays_at, apply_context_overlays,
    apply_terrain_overlays, artifact_key, enrich_graph, rank_routes, route_edges_from_solution,
};

#[derive(Parser)]
#[command(name = "trailgen", version)]
#[command(about = "Native workbench and deterministic forge for constrained hiking trails.")]
#[command(after_help = "Run without a command to resume the current or managed project.")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
#[allow(
    clippy::large_enum_variant,
    reason = "CLI command values are parsed once; boxing clap fields would only launder cold-start bytes into ceremony."
)]
enum Cmd {
    /// Open a materialized project in the native trail workbench.
    Gui {
        /// Project directory containing trailgen.toml and cache/graph.json; omit to resume.
        project: Option<PathBuf>,
        /// Suppress the project-local Protomaps vector basemap.
        #[arg(long)]
        offline: bool,
    },
    /// Create a project directory and initial trailgen.toml.
    Init {
        /// Project directory to create or update.
        project: PathBuf,
        /// Human-readable project name.
        #[arg(long)]
        name: String,
        /// Area of interest as west,south,east,north lon/lat bounds.
        #[arg(long, allow_hyphen_values = true, value_parser = parse_bounds)]
        bbox: Option<GeoBounds>,
    },
    /// Build a routable graph from one or more network or route files.
    Build {
        /// Project directory containing trailgen.toml.
        project: PathBuf,
        /// `GeoJSON`, OSM XML/PBF, shapefile, `GPX`, `KML`/`KMZ`, `CSV`, or route `JSON` source; repeat to merge sources.
        #[arg(long, required = true)]
        source: Vec<PathBuf>,
        /// Override and persist graph near-miss snap tolerance in meters.
        #[arg(long, value_parser = parse_positive_f64)]
        snap_tolerance_m: Option<f64>,
    },
    /// Print graph terrain, access, provenance, confidence, direction, turn-ban, crossing, and seed statistics.
    Stats {
        /// Project directory containing cache/graph.json.
        project: PathBuf,
    },
    /// Scan sources/ and write the source recommendation and coverage manifest.
    Discover {
        /// Project directory containing trailgen.toml.
        project: PathBuf,
        /// Override discovery AOI as west,south,east,north lon/lat bounds for this pass.
        #[arg(long, allow_hyphen_values = true, value_parser = parse_bounds)]
        bbox: Option<GeoBounds>,
    },
    /// Print concrete next source-acquisition actions from sources/manifest.json.
    SourcePlan {
        /// Project directory containing sources/manifest.json.
        project: PathBuf,
        /// Restrict the plan to one source class; repeatable.
        #[arg(long = "kind", value_parser = parse_source_kind)]
        kind: Vec<SourceKind>,
        /// Include already satisfied source classes instead of only gaps.
        #[arg(long)]
        all: bool,
    },
    /// Copy or download a source artifact into project/sources and fingerprint it.
    CacheSource {
        /// Project directory containing trailgen.toml.
        project: PathBuf,
        /// Local path, file:// URI, http:// URL, or https:// URL to cache.
        #[arg(long)]
        input: String,
        /// Relative output path under project/sources.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Source class when the filename is ambiguous.
        #[arg(long, value_parser = parse_source_kind)]
        kind: Option<SourceKind>,
        /// Adapter id when the filename cannot imply the normalizer.
        #[arg(long)]
        adapter: Option<String>,
    },
    /// Fetch bbox-scoped OSM XML from Overpass and register it under sources/.
    AcquireOsm {
        /// Project directory containing trailgen.toml.
        project: PathBuf,
        /// OSM way family to acquire and register.
        #[arg(long, value_enum)]
        profile: OsmAcquireProfile,
        /// Override the project AOI as west,south,east,north lon/lat bounds.
        #[arg(long, allow_hyphen_values = true, value_parser = parse_bounds)]
        bbox: Option<GeoBounds>,
        /// Relative output path under project/sources; defaults from --profile.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Overpass interpreter endpoint.
        #[arg(long, default_value = DEFAULT_OVERPASS_ENDPOINT)]
        endpoint: String,
        /// Overpass server/query timeout in seconds.
        #[arg(long, default_value_t = 180)]
        timeout_s: u64,
        /// Print the Overpass QL query and do not contact the endpoint.
        #[arg(long)]
        print_query: bool,
    },
    /// Recompute source fingerprints and fail on missing or drifted inputs.
    VerifySources {
        /// Project directory containing sources/manifest.json.
        project: PathBuf,
    },
    /// Verify generated artifacts, source snapshots, run metadata, routes, and native solver replay.
    VerifyGeneration {
        /// Project directory containing routes/generated.manifest.json.
        project: PathBuf,
    },
    /// Verify fingerprints and fail unless source coverage satisfies a planning gate.
    VetSources {
        /// Project directory containing sources/manifest.json.
        project: PathBuf,
        /// Coverage gate: required, or required plus recommended.
        #[arg(long, value_enum, default_value = "required")]
        level: SourceGateLevel,
        /// Additional source class that must be satisfied; repeatable.
        #[arg(long = "require", value_parser = parse_source_kind)]
        require: Vec<SourceKind>,
    },
    /// Build and enrich cache/graph.json from sources/manifest.json.
    Assemble {
        /// Project directory containing trailgen.toml and sources/manifest.json.
        project: PathBuf,
        /// Planning date used while applying access/closure overlays.
        #[arg(long, value_parser = parse_planning_date)]
        date: Option<PlanningDate>,
        /// Planning local time used while applying hourly access/closure overlays.
        #[arg(long, value_parser = parse_planning_time)]
        time: Option<PlanningTime>,
        /// Confidence assigned to DEM samples during manifest elevation application.
        #[arg(long, default_value_t = 0.80)]
        elevation_confidence: f64,
    },
    /// Generate ranked candidate routes from a trailhead/start coordinate.
    Generate {
        /// Project directory with a cached graph.
        project: PathBuf,
        /// Requested trailhead/start coordinate as lon,lat.
        #[arg(long, allow_hyphen_values = true)]
        start: String,
        /// Minimum route distance in kilometers for this run.
        #[arg(long, default_value_t = 35.0)]
        min_km: f64,
        /// Maximum route distance in kilometers for this run.
        #[arg(long, default_value_t = 50.0)]
        max_km: f64,
        /// Maximum number of candidates to emit.
        #[arg(long, default_value_t = 6)]
        count: usize,
        /// Recorded random seed for reproducibility and future stochastic solvers.
        #[arg(long, default_value_t = 0)]
        seed: u64,
        /// Maximum allowed trailhead snap distance in meters.
        #[arg(long)]
        max_start_snap_m: Option<f64>,
        /// Solver selection: auto, heuristic, or exact.
        #[arg(long, value_parser = parse_solver_kind)]
        solver: Option<SolverKind>,
        /// Maximum edge count in the outward search for this run.
        #[arg(long, value_parser = parse_positive_usize)]
        max_hops: Option<usize>,
        /// Maximum expanded solver states for this run.
        #[arg(long, value_parser = parse_positive_usize)]
        max_frontier: Option<usize>,
        /// Maximum pre-truncation candidates retained by the solver.
        #[arg(long, value_parser = parse_positive_usize)]
        keep: Option<usize>,
        /// Number of legal return paths tried from each outward heuristic frontier.
        #[arg(long, value_parser = parse_positive_usize)]
        closure_paths: Option<usize>,
        /// Source coverage gate enforced before generation: off, required, or recommended.
        #[arg(long, value_enum)]
        source_gate: Option<GenerationSourceGate>,
        /// Planning date used to materialize dated access/closure overlays.
        #[arg(long, value_parser = parse_planning_date)]
        date: Option<PlanningDate>,
        /// Planning local time used to materialize hourly access/closure overlays.
        #[arg(long, value_parser = parse_planning_time)]
        time: Option<PlanningTime>,
        /// Minimum scalar route difficulty.
        #[arg(long)]
        min_difficulty: Option<f64>,
        /// Maximum scalar route difficulty.
        #[arg(long)]
        max_difficulty: Option<f64>,
        /// Minimum route ascent in meters.
        #[arg(long)]
        min_ascent_m: Option<f64>,
        /// Maximum route ascent in meters.
        #[arg(long)]
        max_ascent_m: Option<f64>,
        /// Minimum route descent in meters.
        #[arg(long)]
        min_descent_m: Option<f64>,
        /// Maximum route descent in meters.
        #[arg(long)]
        max_descent_m: Option<f64>,
        /// Maximum road or pavement distance fraction in [0,1].
        #[arg(long)]
        max_road_fraction: Option<f64>,
        /// Maximum low-confidence edge distance fraction in [0,1].
        #[arg(long)]
        max_low_confidence_fraction: Option<f64>,
        /// Maximum restricted/closed/private access distance fraction in [0,1].
        #[arg(long)]
        max_restricted_access_fraction: Option<f64>,
        /// Allowed measured route shape; repeat for multiple shapes.
        #[arg(long = "shape", value_parser = parse_shape)]
        shape: Vec<RouteShape>,
        /// Maximum repeated-edge distance fraction in [0,1].
        #[arg(long)]
        max_repeated_edge_fraction: Option<f64>,
        /// Terrain bucket that must not appear in emitted routes; repeatable.
        #[arg(long = "forbid-terrain", value_parser = parse_terrain)]
        forbidden_terrain: Vec<Terrain>,
        /// One-run polygon/line avoid zone forced closed in the generated graph; repeatable.
        #[arg(long = "forbid-area")]
        forbidden_area: Vec<PathBuf>,
        /// Required minimum terrain fraction as terrain:fraction or terrain=fraction.
        #[arg(long = "min-terrain", value_parser = parse_terrain_fraction)]
        min_terrain: Vec<TerrainFraction>,
        /// Required maximum terrain fraction as terrain:fraction or terrain=fraction.
        #[arg(long = "max-terrain", value_parser = parse_terrain_fraction)]
        max_terrain: Vec<TerrainFraction>,
    },
    /// Write a deterministic LP/MILP loop formulation for the current graph.
    FormulateMilp {
        /// Project directory with a cached graph.
        project: PathBuf,
        /// Requested trailhead/start coordinate as lon,lat.
        #[arg(long, allow_hyphen_values = true)]
        start: String,
        /// Destination .lp file.
        #[arg(long)]
        output: PathBuf,
        /// Minimum route distance in kilometers for this formulation.
        #[arg(long)]
        min_km: Option<f64>,
        /// Maximum route distance in kilometers for this formulation.
        #[arg(long)]
        max_km: Option<f64>,
        /// Maximum allowed trailhead snap distance in meters.
        #[arg(long)]
        max_start_snap_m: Option<f64>,
        /// Planning date used to materialize dated access/closure overlays.
        #[arg(long, value_parser = parse_planning_date)]
        date: Option<PlanningDate>,
        /// Planning local time used to materialize hourly access/closure overlays.
        #[arg(long, value_parser = parse_planning_time)]
        time: Option<PlanningTime>,
    },
    /// Import an external MILP solver incumbent into normal generated route artifacts.
    ImportMilpSolution {
        /// Project directory with a cached graph.
        project: PathBuf,
        /// Requested trailhead/start coordinate used by the formulation, as lon,lat.
        #[arg(long, allow_hyphen_values = true)]
        start: String,
        /// Solver solution text containing selected `z_eN_vFROM_vTO` variables.
        #[arg(long)]
        solution: PathBuf,
        /// Generated candidate name.
        #[arg(long, default_value = "candidate-1")]
        name: String,
        /// Minimum route distance in kilometers for this import audit.
        #[arg(long)]
        min_km: Option<f64>,
        /// Maximum route distance in kilometers for this import audit.
        #[arg(long)]
        max_km: Option<f64>,
        /// Maximum allowed trailhead snap distance in meters.
        #[arg(long)]
        max_start_snap_m: Option<f64>,
        /// Planning date used to materialize dated access/closure overlays.
        #[arg(long, value_parser = parse_planning_date)]
        date: Option<PlanningDate>,
        /// Planning local time used to materialize hourly access/closure overlays.
        #[arg(long, value_parser = parse_planning_time)]
        time: Option<PlanningTime>,
    },
    /// Export one generated candidate route to `GPX`, `GeoJSON`, `CSV`, `KML`, or `KMZ`.
    Export {
        /// Project directory containing routes/generated.routes.json.
        project: PathBuf,
        /// Candidate name such as candidate-1.
        #[arg(long)]
        route: String,
        /// Destination file.
        #[arg(long)]
        output: PathBuf,
        /// Optional Markdown report sidecar for the same selected route.
        #[arg(long)]
        report_output: Option<PathBuf>,
        /// Export format.
        #[arg(long, value_enum)]
        format: ExportFormat,
    },
    /// Render an aggregate or single-route Markdown diagnostic report.
    Report {
        /// Project directory containing generated routes.
        project: PathBuf,
        /// Optional candidate name such as candidate-1.
        #[arg(long)]
        route: Option<String>,
        /// Destination report path; stdout when omitted.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Render a self-contained interactive offline SVG/HTML diagnostic map.
    Map {
        /// Project directory containing a graph and optional generated routes.
        project: PathBuf,
        /// Destination HTML path.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Rate a supplied route file against the current project graph.
    Rate {
        /// Project directory containing cache/graph.json.
        project: PathBuf,
        /// `GPX`, `GeoJSON`, `KML`/`KMZ`, `CSV`, or route `JSON` file to snap and rate.
        #[arg(long)]
        route: PathBuf,
        /// Maximum allowed route snap distance in meters.
        #[arg(long)]
        max_route_snap_m: Option<f64>,
        /// Optional Markdown report path.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Recompute cached edge costs after hand-editing difficulty weights.
    Rerate {
        /// Project directory containing cache/graph.json.
        project: PathBuf,
    },
    /// Fit difficulty weights from a completed route and optional target scalar.
    Calibrate {
        /// Project directory containing cache/graph.json.
        project: PathBuf,
        /// Completed route file to snap and use as calibration evidence.
        #[arg(long)]
        route: PathBuf,
        /// Desired scalar difficulty for the completed route.
        #[arg(long)]
        target_difficulty: f64,
        /// Weight family to scale.
        #[arg(long, value_enum, default_value = "all")]
        family: CalibrationFamily,
        /// Maximum allowed route snap distance in meters.
        #[arg(long)]
        max_route_snap_m: Option<f64>,
        /// Persist the calibrated weights and rerate the cached graph.
        #[arg(long)]
        write: bool,
    },
    /// Archive, snap, and register a user-supplied seed route.
    ImportSeed {
        /// Project directory containing cache/graph.json.
        project: PathBuf,
        /// `GPX`, `GeoJSON`, `KML`/`KMZ`, `CSV`, or route `JSON` file to import.
        #[arg(long)]
        route: PathBuf,
        /// Optional seed name; route metadata or filename is used when omitted.
        #[arg(long)]
        name: Option<String>,
        /// Maximum allowed route snap distance in meters.
        #[arg(long)]
        max_route_snap_m: Option<f64>,
    },
    /// Apply one or more access/closure overlays from a shared graph baseline.
    ApplyAccess {
        /// Project directory containing cache/graph.json.
        project: PathBuf,
        /// Access, ownership, restriction, or closure overlay; repeat to compose sources.
        #[arg(long = "source", required = true)]
        source: Vec<PathBuf>,
        /// Persisted planning date for `active_from`/`active_to` overlay filtering.
        #[arg(long, value_parser = parse_planning_date)]
        date: Option<PlanningDate>,
        /// Persisted local planning time for hourly overlay filtering.
        #[arg(long, value_parser = parse_planning_time)]
        time: Option<PlanningTime>,
    },
    /// Apply terrain, surface, or land-cover overlays and rerate touched edges.
    ApplyTerrain {
        /// Project directory containing cache/graph.json.
        project: PathBuf,
        /// `GeoJSON` or shapefile terrain overlay.
        #[arg(long)]
        source: PathBuf,
    },
    /// Sample a local DEM and recompute edge ascent, descent, grade, and difficulty.
    ApplyElevation {
        /// Project directory containing cache/graph.json.
        project: PathBuf,
        /// Arc/Info ASCII Grid, affine WGS84/NAD83/EPSG:3857/UTM `GeoTIFF`, or simple `VRT` `DEM`.
        #[arg(long)]
        source: PathBuf,
        /// Confidence assigned to sampled elevation evidence.
        #[arg(long, default_value_t = 0.80)]
        confidence: f64,
    },
    /// Apply road and hydrology context linework for crossings and road exposure.
    ApplyContext {
        /// Project directory containing cache/graph.json.
        project: PathBuf,
        /// `GeoJSON`, shapefile, OSM XML, or OSM PBF road/hydrology context layer.
        #[arg(long)]
        source: PathBuf,
    },
    /// Print the documented `AllTrails` import/export bridge status.
    AlltrailsStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProjectConfig {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    area: Option<GeoBounds>,
    #[serde(default = "default_snap_tolerance_m")]
    snap_tolerance_m: f64,
    #[serde(default)]
    enrichment: EnrichmentConfig,
    #[serde(default)]
    difficulty: DifficultyWeights,
    #[serde(default)]
    constraints: LoopConstraints,
    #[serde(default)]
    search: SearchParams,
    #[serde(default = "default_max_start_snap_m")]
    max_start_snap_m: f64,
    #[serde(default = "default_max_route_snap_m")]
    max_route_snap_m: f64,
    #[serde(default)]
    solver: SolverKind,
    #[serde(default)]
    generation_source_gate: GenerationSourceGate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    planning_date: Option<PlanningDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    planning_time: Option<PlanningTime>,
}

impl ProjectConfig {
    fn new(name: String, area: Option<GeoBounds>) -> Self {
        Self {
            name,
            area,
            snap_tolerance_m: 8.0,
            enrichment: EnrichmentConfig::default(),
            difficulty: DifficultyWeights::default(),
            constraints: LoopConstraints::default(),
            search: SearchParams::default(),
            max_start_snap_m: default_max_start_snap_m(),
            max_route_snap_m: default_max_route_snap_m(),
            solver: SolverKind::default(),
            generation_source_gate: GenerationSourceGate::default(),
            planning_date: None,
            planning_time: None,
        }
    }

    fn planning_moment(&self) -> Option<PlanningMoment> {
        (self.planning_date.is_some() || self.planning_time.is_some())
            .then(|| PlanningMoment::new(self.planning_date, self.planning_time))
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.trim().is_empty(),
            "project name must not be empty"
        );
        ensure!(
            self.area.is_none_or(GeoBounds::is_valid),
            "invalid project bbox"
        );
        for (name, value) in [
            ("snap_tolerance_m", self.snap_tolerance_m),
            ("max_start_snap_m", self.max_start_snap_m),
            ("max_route_snap_m", self.max_route_snap_m),
            ("sample_spacing_m", self.enrichment.sample_spacing_m),
            (
                "steep_grade_threshold",
                self.enrichment.steep_grade_threshold,
            ),
        ] {
            ensure!(
                value.is_finite() && value > 0.0,
                "{name} must be finite and positive"
            );
        }
        for (name, value) in difficulty_values(self.difficulty) {
            ensure!(
                value.is_finite() && value >= 0.0,
                "{name} must be finite and nonnegative"
            );
        }
        let c = &self.constraints;
        for (name, value) in [
            ("min_distance_m", c.min_distance_m),
            ("max_distance_m", c.max_distance_m),
            ("min_difficulty", c.min_difficulty),
            ("max_difficulty", c.max_difficulty),
            ("min_ascent_m", c.min_ascent_m),
            ("max_ascent_m", c.max_ascent_m),
            ("min_descent_m", c.min_descent_m),
            ("max_descent_m", c.max_descent_m),
        ] {
            ensure!(
                value.is_finite() && value >= 0.0,
                "{name} must be finite and nonnegative"
            );
        }
        for (name, min, max) in [
            ("distance", c.min_distance_m, c.max_distance_m),
            ("difficulty", c.min_difficulty, c.max_difficulty),
            ("ascent", c.min_ascent_m, c.max_ascent_m),
            ("descent", c.min_descent_m, c.max_descent_m),
        ] {
            ensure!(min <= max, "minimum {name} exceeds maximum {name}");
        }
        for (name, value) in [
            ("max_road_fraction", c.max_road_fraction),
            ("max_low_confidence_fraction", c.max_low_confidence_fraction),
            (
                "max_restricted_access_fraction",
                c.max_restricted_access_fraction,
            ),
            ("max_repeated_edge_fraction", c.max_repeated_edge_fraction),
        ]
        .into_iter()
        .chain(
            c.min_terrain_fraction
                .values()
                .copied()
                .map(|value| ("min_terrain_fraction", value)),
        )
        .chain(
            c.max_terrain_fraction
                .values()
                .copied()
                .map(|value| ("max_terrain_fraction", value)),
        ) {
            ensure!((0.0..=1.0).contains(&value), "{name} must be within 0..=1");
        }
        ensure!(
            !c.allowed_shapes.is_empty(),
            "allowed_shapes must not be empty"
        );
        for (terrain, minimum) in &c.min_terrain_fraction {
            if let Some(maximum) = c.max_terrain_fraction.get(terrain) {
                ensure!(
                    minimum <= maximum,
                    "minimum {terrain:?} fraction exceeds maximum"
                );
            }
        }
        ensure!(
            self.search.max_hops > 0
                && self.search.max_frontier > 0
                && self.search.keep > 0
                && self.search.closure_paths > 0,
            "search limits must be positive"
        );
        Ok(())
    }
}

const fn difficulty_values(weights: DifficultyWeights) -> [(&'static str, f64); 18] {
    let terrain = weights.terrain_multipliers;
    [
        ("distance_per_km", weights.distance_per_km),
        ("ascent_per_m", weights.ascent_per_m),
        ("descent_per_m", weights.descent_per_m),
        ("grade_per_abs_fraction", weights.grade_per_abs_fraction),
        ("terrain.unknown", terrain.unknown),
        ("terrain.trail", terrain.trail),
        ("terrain.forest", terrain.forest),
        ("terrain.alpine", terrain.alpine),
        ("terrain.talus", terrain.talus),
        ("terrain.scramble", terrain.scramble),
        ("terrain.pavement", terrain.pavement),
        ("terrain.road", terrain.road),
        ("terrain.water", terrain.water),
        ("road_penalty", weights.road_penalty),
        ("technical_penalty", weights.technical_penalty),
        ("navigation_penalty", weights.navigation_penalty),
        ("low_confidence_penalty", weights.low_confidence_penalty),
        ("closed_access_penalty", weights.closed_access_penalty),
    ]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ExportFormat {
    Gpx,
    Geojson,
    Csv,
    Kml,
    Kmz,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum OsmAcquireProfile {
    All,
    Trails,
    Roads,
    Hydrology,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum CalibrationFamily {
    All,
    Distance,
    Elevation,
    Ascent,
    Descent,
    Grade,
    Terrain,
    Road,
    Technical,
    Navigation,
    Confidence,
    Access,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SourceGateLevel {
    Required,
    Recommended,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum GenerationSourceGate {
    #[default]
    Off,
    Required,
    Recommended,
}

impl GenerationSourceGate {
    const fn level(self) -> Option<SourceGateLevel> {
        match self {
            Self::Off => None,
            Self::Required => Some(SourceGateLevel::Required),
            Self::Recommended => Some(SourceGateLevel::Recommended),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Required => "required",
            Self::Recommended => "recommended",
        }
    }
}

impl SourceGateLevel {
    const fn admits(self, priority: SourcePriority) -> bool {
        match self {
            Self::Required => matches!(priority, SourcePriority::Required),
            Self::Recommended => {
                matches!(
                    priority,
                    SourcePriority::Required | SourcePriority::Recommended
                )
            }
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Recommended => "recommended",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TerrainFraction {
    terrain: Terrain,
    fraction: f64,
}

const fn default_snap_tolerance_m() -> f64 {
    8.0
}

const fn default_max_start_snap_m() -> f64 {
    500.0
}

const fn default_max_route_snap_m() -> f64 {
    100.0
}

const DEFAULT_OVERPASS_ENDPOINT: &str = "https://overpass-api.de/api/interpreter";
const MAX_OVERPASS_BBOX_DEG2: f64 = 4.0;
const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ARCHIVE_MEMBER_BYTES: u64 = 256 * 1024 * 1024;
const OSM_TRAIL_SELECTORS: &[&str] = &[
    r#"way["highway"~"^(path|footway|track|service|pedestrian|steps|bridleway|unclassified|residential|tertiary|road)$"]"#,
    r#"way["route"~"^(hiking|foot|walking)$"]"#,
    r#"relation["type"="route"]["route"~"^(hiking|foot|walking)$"]"#,
    r#"relation["type"="restriction"]["restriction"]"#,
    r#"relation["type"="restriction"]["restriction:foot"]"#,
];
const OSM_ROAD_SELECTORS: &[&str] = &[
    r#"way["highway"~"^(motorway|trunk|primary|secondary|tertiary|unclassified|residential|living_street|service|track|road)$"]"#,
];
const OSM_HYDROLOGY_SELECTORS: &[&str] =
    &[r#"way["waterway"~"^(stream|river|canal|drain|ditch|brook)$"]"#];

fn main() -> Result<()> {
    let Some(cmd) = Cli::parse().cmd else {
        return trailgen_gui::run(trailgen_gui::ProjectIntent::Resume, false);
    };
    dispatch(cmd)
}

#[allow(
    clippy::too_many_lines,
    reason = "Clap command dispatch is a single declarative cold path; splitting it would scatter the command algebra."
)]
fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Gui { project, offline } => trailgen_gui::run(
            project.map_or(
                trailgen_gui::ProjectIntent::Resume,
                trailgen_gui::ProjectIntent::Open,
            ),
            offline,
        ),
        Cmd::Init {
            project,
            name,
            bbox,
        } => init(&project, name, bbox),
        Cmd::Build {
            project,
            source,
            snap_tolerance_m,
        } => build_many(
            &project,
            source.iter().map(PathBuf::as_path),
            snap_tolerance_m,
        ),
        Cmd::Stats { project } => stats(&project),
        Cmd::Discover { project, bbox } => discover(&project, bbox),
        Cmd::SourcePlan { project, kind, all } => source_plan(&project, &kind, all),
        Cmd::CacheSource {
            project,
            input,
            output,
            kind,
            adapter,
        } => cache_source(
            &project,
            &input,
            output.as_deref(),
            kind,
            adapter.as_deref(),
        ),
        Cmd::AcquireOsm {
            project,
            profile,
            bbox,
            output,
            endpoint,
            timeout_s,
            print_query,
        } => acquire_osm(
            &project,
            &OsmAcquisition {
                profile,
                bbox,
                output,
                endpoint,
                timeout_s,
                print_query,
            },
        ),
        Cmd::VerifySources { project } => verify_sources(&project),
        Cmd::VerifyGeneration { project } => verify_generation(&project),
        Cmd::VetSources {
            project,
            level,
            require,
        } => vet_sources(&project, level, &require),
        Cmd::Assemble {
            project,
            date,
            time,
            elevation_confidence,
        } => assemble_sources(&project, date, time, elevation_confidence),
        Cmd::Generate {
            project,
            start,
            min_km,
            max_km,
            count,
            seed,
            max_start_snap_m,
            solver,
            max_hops,
            max_frontier,
            keep,
            closure_paths,
            source_gate,
            date,
            time,
            min_difficulty,
            max_difficulty,
            min_ascent_m,
            max_ascent_m,
            min_descent_m,
            max_descent_m,
            max_road_fraction,
            max_low_confidence_fraction,
            max_restricted_access_fraction,
            shape,
            max_repeated_edge_fraction,
            forbidden_terrain,
            forbidden_area,
            min_terrain,
            max_terrain,
        } => generate(
            &project,
            &GenerateOptions {
                start,
                min_km,
                max_km,
                count,
                seed,
                max_start_snap_m,
                solver,
                max_hops,
                max_frontier,
                keep,
                closure_paths,
                source_gate,
                date,
                time,
                min_difficulty,
                max_difficulty,
                min_ascent_m,
                max_ascent_m,
                min_descent_m,
                max_descent_m,
                max_road_fraction,
                max_low_confidence_fraction,
                max_restricted_access_fraction,
                shape,
                max_repeated_edge_fraction,
                forbidden_terrain,
                forbidden_area,
                min_terrain,
                max_terrain,
            },
        ),
        Cmd::FormulateMilp {
            project,
            start,
            output,
            min_km,
            max_km,
            max_start_snap_m,
            date,
            time,
        } => formulate_milp(
            &project,
            &MilpFormulationOptions {
                start,
                output,
                min_km,
                max_km,
                max_start_snap_m,
                date,
                time,
            },
        ),
        Cmd::ImportMilpSolution {
            project,
            start,
            solution,
            name,
            min_km,
            max_km,
            max_start_snap_m,
            date,
            time,
        } => import_milp_solution(
            &project,
            &MilpIncumbentOptions {
                start,
                solution,
                name,
                min_km,
                max_km,
                max_start_snap_m,
                date,
                time,
            },
        ),
        Cmd::Export {
            project,
            route,
            output,
            report_output,
            format,
        } => export_route(&project, &route, format, &output, report_output.as_deref()),
        Cmd::Report {
            project,
            route,
            output,
        } => report_generated(&project, route.as_deref(), output.as_deref()),
        Cmd::Map { project, output } => map_html(&project, output.as_deref()),
        Cmd::Rate {
            project,
            route,
            max_route_snap_m,
            output,
        } => rate(&project, &route, max_route_snap_m, output.as_deref()),
        Cmd::Rerate { project } => rerate(&project),
        Cmd::Calibrate {
            project,
            route,
            target_difficulty,
            family,
            max_route_snap_m,
            write,
        } => calibrate(
            &project,
            &route,
            target_difficulty,
            family,
            max_route_snap_m,
            write,
        ),
        Cmd::ImportSeed {
            project,
            route,
            name,
            max_route_snap_m,
        } => import_seed(&project, &route, name, max_route_snap_m),
        Cmd::ApplyAccess {
            project,
            source,
            date,
            time,
        } => apply_access(&project, &source, date, time),
        Cmd::ApplyTerrain { project, source } => apply_terrain(&project, &source),
        Cmd::ApplyElevation {
            project,
            source,
            confidence,
        } => apply_elevation(&project, &source, confidence),
        Cmd::ApplyContext { project, source } => apply_context(&project, &source),
        Cmd::AlltrailsStatus => {
            println!("{}", include_str!("../../../docs/alltrails.md"));
            println!(
                "\nMachine-readable exchange plans:\n{}",
                serde_json::to_string_pretty(&alltrails_plans())?
            );
            Ok(())
        }
    }
}

fn init(project: &Path, name: String, area: Option<GeoBounds>) -> Result<()> {
    ensure!(
        !project.join("trailgen.toml").exists(),
        "{} is already a trailgen project",
        project.display()
    );
    fs::create_dir_all(project).with_context(|| format!("create {}", project.display()))?;
    for subdir in ["cache", "routes", "reports", "sources", "seeds"] {
        fs::create_dir_all(project.join(subdir))
            .with_context(|| format!("create {}", project.join(subdir).display()))?;
    }
    let config = ProjectConfig::new(name, area);
    config.validate()?;
    fs::write(
        project.join("trailgen.toml"),
        toml::to_string_pretty(&config)?,
    )
    .with_context(|| "write trailgen.toml")?;
    println!("initialized {}", project.display());
    Ok(())
}

#[cfg(test)]
fn build(project: &Path, source: &Path) -> Result<()> {
    build_many(project, std::iter::once(source), None)
}

fn build_many<'a>(
    project: &Path,
    sources: impl IntoIterator<Item = &'a Path>,
    snap_tolerance_m: Option<f64>,
) -> Result<()> {
    let sources = sources.into_iter().collect::<Vec<_>>();
    if sources.is_empty() {
        bail!("build requires at least one --source");
    }
    let mut config = load_config(project)?;
    if let Some(snap_tolerance_m) = snap_tolerance_m {
        config.snap_tolerance_m = snap_tolerance_m;
        save_config(project, &config)?;
    }
    let mut drafts = Vec::new();
    let mut candidates = Vec::new();
    let mut adapter_counts = BTreeMap::<&'static str, usize>::new();
    for source in sources {
        let build_source = build_source(source)?;
        *adapter_counts.entry(build_source.adapter_id).or_default() += 1;
        candidates.push(source_candidate(
            source,
            build_source.kind,
            build_source.adapter_id,
            source_fingerprint(source)?,
        ));
        drafts.extend(build_source.drafts);
    }
    let graph = GraphBuilder {
        snap_tolerance_m: config.snap_tolerance_m,
        enrichment: config.enrichment,
        weights: config.difficulty,
    }
    .build(&drafts)
    .with_context(|| "build graph")?;
    save_graph(project, &graph)?;
    register_source_candidates(project, candidates)?;
    println!(
        "built graph from {} source(s) via {}: {} vertices, {} edges",
        adapter_counts.values().sum::<usize>(),
        adapter_counts
            .into_iter()
            .map(|(adapter, count)| format!("{adapter}×{count}"))
            .collect::<Vec<_>>()
            .join(", "),
        graph.vertices.len(),
        graph.edges.len()
    );
    Ok(())
}

fn build_source(source: &Path) -> Result<BuildSource> {
    match source_ext(source).as_deref() {
        Some("geojson") => {
            let raw =
                fs::read_to_string(source).with_context(|| format!("read {}", source.display()))?;
            match geojson::network_from_str(&raw) {
                Ok(drafts) => Ok(BuildSource {
                    drafts,
                    kind: SourceKind::TrailNetwork,
                    adapter_id: "geojson-network",
                }),
                Err(network_error) => Ok(BuildSource {
                    drafts: vec![route_source_draft_from_line(
                        source,
                        geojson::route_line_from_str(&raw).with_context(|| {
                            format!(
                                "parse {} as GeoJSON trail network ({network_error}) or route",
                                source.display()
                            )
                        })?,
                    )],
                    kind: SourceKind::SeedRoute,
                    adapter_id: "geojson-route",
                }),
            }
        }
        Some("json") => json_build_source(source),
        Some("gpx" | "csv" | "kml" | "kmz") => Ok(BuildSource {
            drafts: vec![route_source_draft(source)?],
            kind: SourceKind::SeedRoute,
            adapter_id: route_adapter_id(source),
        }),
        Some("osm") => Ok(BuildSource {
            drafts: osm::network_from_str(
                &fs::read_to_string(source)
                    .with_context(|| format!("read {}", source.display()))?,
            )?,
            kind: SourceKind::TrailNetwork,
            adapter_id: "osm-xml-network",
        }),
        Some("osm.pbf") => Ok(BuildSource {
            drafts: osm::network_from_pbf_reader(
                fs::File::open(source).with_context(|| format!("read {}", source.display()))?,
            )?,
            kind: SourceKind::TrailNetwork,
            adapter_id: "osm-pbf-network",
        }),
        Some("shp") => Ok(BuildSource {
            drafts: shp::network_from_path(source)?,
            kind: SourceKind::TrailNetwork,
            adapter_id: "shapefile-network",
        }),
        Some(ext) => bail!(
            "unsupported build source extension {ext:?}; expected geojson, json, osm, osm.pbf, shp, gpx, csv, kml, or kmz"
        ),
        None => bail!("build source has no extension"),
    }
}

fn json_build_source(source: &Path) -> Result<BuildSource> {
    let raw = fs::read_to_string(source).with_context(|| format!("read {}", source.display()))?;
    match geojson::network_from_str(&raw) {
        Ok(drafts) => Ok(BuildSource {
            drafts,
            kind: SourceKind::TrailNetwork,
            adapter_id: "geojson-network",
        }),
        Err(network_error) => {
            let (line, adapter_id) = json_route_line(&raw).with_context(|| {
                format!(
                    "parse {} as GeoJSON trail network ({network_error}) or JSON route",
                    source.display()
                )
            })?;
            Ok(BuildSource {
                drafts: vec![route_source_draft_from_line(source, line)],
                kind: SourceKind::SeedRoute,
                adapter_id,
            })
        }
    }
}

fn route_source_draft(source: &Path) -> Result<SegmentDraft> {
    let line = load_route_line(source)?;
    Ok(route_source_draft_from_line(source, line))
}

fn route_source_draft_from_line(source: &Path, line: LineString) -> SegmentDraft {
    SegmentDraft {
        junctions: JunctionPolicy::default(),
        turn_ref: None,
        turn_restrictions: Vec::new(),
        geometry: line,
        terrain: Terrain::Unknown,
        terrain_confidence: Some(0.0),
        surface: None,
        access: Access::Unknown,
        travel: EdgeTravel::Both,
        road_exposure: 0.0,
        confidence: 0.65,
        provenance: Provenance {
            source: "route-file".to_owned(),
            layer: Some("route-derived-network".to_owned()),
            source_id: source
                .file_name()
                .and_then(|x| x.to_str())
                .map(str::to_owned),
            license: None,
        },
    }
}

fn route_adapter_id(source: &Path) -> &'static str {
    match source_ext(source).as_deref() {
        Some("kml" | "kmz") => "kml-route",
        Some("geojson") => "geojson-route",
        Some("json") => "json-route",
        Some("csv") => "csv-route",
        _ => "gpx-route",
    }
}

fn stats(project: &Path) -> Result<()> {
    let graph = load_graph(project)?;
    print!("{}", stats_text(&graph));
    Ok(())
}

fn stats_text(graph: &TrailGraph) -> String {
    let mut text = String::new();
    let mut terrain_m = BTreeMap::<Terrain, f64>::new();
    let mut access_m = BTreeMap::<Access, f64>::new();
    let mut source_m = BTreeMap::<String, f64>::new();
    let mut confidence_m = BTreeMap::<ConfidenceBand, f64>::new();
    let mut road_m = 0.0;
    let mut low_conf_m = 0.0;
    let mut restricted_m = 0.0;
    let mut seed_m = 0.0;
    let mut difficulty = 0.0;
    let elevation = graph_elevation_stats(graph);
    for edge in &graph.edges {
        let a = &edge.attr;
        *terrain_m.entry(a.terrain).or_default() += a.length_m;
        *access_m.entry(a.access).or_default() += a.length_m;
        *source_m.entry(primary_source_label(a)).or_default() += a.length_m;
        *confidence_m
            .entry(ConfidenceBand::from_confidence(a.confidence))
            .or_default() += a.length_m;
        road_m = a.length_m.mul_add(
            edge_road_pavement_exposure(a.terrain, a.road_exposure),
            road_m,
        );
        if a.confidence < LOW_CONFIDENCE_THRESHOLD {
            low_conf_m += a.length_m;
        }
        if matches!(
            a.access,
            Access::Restricted | Access::Closed | Access::Private
        ) {
            restricted_m += a.length_m;
        }
        if a.seed_count > 0 {
            seed_m += a.length_m;
        }
        difficulty += a.difficulty;
    }
    let total_m = graph
        .edges
        .iter()
        .map(|edge| edge.attr.length_m)
        .sum::<f64>();
    let _ = writeln!(text, "vertices: {}", graph.vertices.len());
    let _ = writeln!(text, "edges: {}", graph.edges.len());
    let _ = writeln!(
        text,
        "directed-travel edges: {}",
        directed_travel_edge_count(graph)
    );
    let _ = writeln!(text, "turn bans: {}", graph.turn_bans.len());
    let _ = writeln!(text, "edge-km: {:.2}", total_m / 1_000.0);
    let _ = writeln!(
        text,
        "mean difficulty per km: {:.2}",
        difficulty / (total_m / 1_000.0).max(1.0e-9)
    );
    let _ = writeln!(
        text,
        "low-confidence edge-km: {:.2} ({:.1}%)",
        low_conf_m / 1_000.0,
        percent(low_conf_m, total_m)
    );
    let _ = writeln!(
        text,
        "restricted-access edge-km: {:.2} ({:.1}%)",
        restricted_m / 1_000.0,
        percent(restricted_m, total_m)
    );
    let _ = writeln!(
        text,
        "road/pavement edge-km: {:.2} ({:.1}%)",
        road_m / 1_000.0,
        percent(road_m, total_m)
    );
    write_elevation_stats(&mut text, &elevation, total_m);
    let _ = writeln!(
        text,
        "seed-attributed edge-km: {:.2} ({:.1}%)",
        seed_m / 1_000.0,
        percent(seed_m, total_m)
    );
    write_meter_mix(&mut text, "Terrain mix", &terrain_m, total_m);
    write_meter_mix(&mut text, "Access mix", &access_m, total_m);
    write_labeled_meter_mix(&mut text, "Source mix", &source_m, total_m);
    write_labeled_meter_mix(&mut text, "Confidence mix", &confidence_m, total_m);
    write_turn_ban_provenance(&mut text, graph);
    write_crossing_totals(&mut text, graph);
    text
}

fn write_elevation_stats(text: &mut String, elevation: &GraphElevationStats, total_m: f64) {
    let _ = writeln!(
        text,
        "elevation-attributed edge-km: {:.2} ({:.1}%)",
        elevation.attributed_edge_m / 1_000.0,
        percent(elevation.attributed_edge_m, total_m)
    );
    let _ = writeln!(
        text,
        "elevation-sampled grade-km: {:.2} ({:.1}%)",
        elevation.sampled_grade_m / 1_000.0,
        percent(elevation.sampled_grade_m, total_m)
    );
    let _ = writeln!(
        text,
        "graph ascent/descent: {:.0} m / {:.0} m",
        elevation.ascent_m, elevation.descent_m
    );
    let _ = writeln!(
        text,
        "sustained-steep edge-km: {:.2} ({:.1}% of sampled grade)",
        elevation.sustained_steep_m / 1_000.0,
        percent(elevation.sustained_steep_m, elevation.sampled_grade_m)
    );
    write_grade_distribution(text, elevation.grade_distribution_m);
    write_elevation_provenance(text, &elevation.provenance_edges);
}

fn write_grade_distribution(text: &mut String, grade: GradeDistribution) {
    text.push_str("Grade distribution:\n");
    let total = grade.total_m();
    if total <= f64::EPSILON {
        text.push_str("- none\n");
        return;
    }
    for (label, meters) in [
        ("flat <5%", grade.flat_m),
        ("rolling 5–15%", grade.rolling_m),
        ("steep 15–30%", grade.steep_m),
        ("savage ≥30%", grade.savage_m),
    ] {
        let _ = writeln!(
            text,
            "- {label}: {:.2} km ({:.1}%)",
            meters / 1_000.0,
            percent(meters, total)
        );
    }
}

fn write_elevation_provenance(text: &mut String, provenance: &BTreeMap<String, usize>) {
    text.push_str("Elevation provenance:\n");
    if provenance.is_empty() {
        text.push_str("- none\n");
        return;
    }
    for (source, edges) in provenance {
        let _ = writeln!(text, "- {source}: {edges} edge(s)");
    }
}

fn write_turn_ban_provenance(text: &mut String, graph: &TrailGraph) {
    text.push_str("Turn-ban provenance:\n");
    let sources = turn_ban_sources(graph);
    if sources.is_empty() {
        text.push_str("- none\n");
        return;
    }
    for (source, count) in sources {
        let _ = writeln!(text, "- {source}: {count}");
    }
}

fn write_crossing_totals(text: &mut String, graph: &TrailGraph) {
    text.push_str("Crossings:\n");
    let crossings = crossing_totals(graph);
    if crossings.is_empty() {
        text.push_str("- none\n");
        return;
    }
    for (kind, count) in crossings {
        let _ = writeln!(text, "- {kind:?}: {count}");
    }
}

const fn edge_road_pavement_exposure(terrain: Terrain, road_exposure: f64) -> f64 {
    road_exposure
        .clamp(0.0, 1.0)
        .max(if matches!(terrain, Terrain::Pavement | Terrain::Road) {
            1.0
        } else {
            0.0
        })
}

fn edge_has_elevation(a: &EdgeAttr) -> bool {
    !a.elevation_provenance.is_empty()
        || a.ascent_m > 0.0
        || a.descent_m > 0.0
        || a.grade_abs_mean > 0.0
        || a.grade_abs_max > 0.0
        || a.grade_distribution.total_m() > 0.0
}

fn primary_source_label(a: &EdgeAttr) -> String {
    a.provenance
        .first()
        .map_or_else(|| "unknown".to_owned(), provenance_label)
}

fn provenance_label(p: &Provenance) -> String {
    p.source_id
        .as_ref()
        .map_or_else(|| p.source.clone(), |id| format!("{}:{id}", p.source))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum ConfidenceBand {
    Low,
    Medium,
    High,
}

impl ConfidenceBand {
    const fn from_confidence(confidence: f64) -> Self {
        if confidence < LOW_CONFIDENCE_THRESHOLD {
            Self::Low
        } else if confidence < 0.8 {
            Self::Medium
        } else {
            Self::High
        }
    }
}

impl fmt::Display for ConfidenceBand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Low => "low <0.60",
            Self::Medium => "medium 0.60–0.79",
            Self::High => "high ≥0.80",
        })
    }
}

fn write_meter_mix<K: std::fmt::Debug + Ord>(
    text: &mut String,
    title: &str,
    meters_by_key: &BTreeMap<K, f64>,
    total_m: f64,
) {
    let _ = writeln!(text, "{title}:");
    if meters_by_key.is_empty() {
        text.push_str("- none\n");
        return;
    }
    for (key, meters) in meters_by_key {
        let _ = writeln!(
            text,
            "- {key:?}: {:.2} km ({:.1}%)",
            meters / 1_000.0,
            percent(*meters, total_m)
        );
    }
}

fn write_labeled_meter_mix<K: fmt::Display + Ord>(
    text: &mut String,
    title: &str,
    meters_by_key: &BTreeMap<K, f64>,
    total_m: f64,
) {
    let _ = writeln!(text, "{title}:");
    if meters_by_key.is_empty() {
        text.push_str("- none\n");
        return;
    }
    for (key, meters) in meters_by_key {
        let _ = writeln!(
            text,
            "- {key}: {:.2} km ({:.1}%)",
            meters / 1_000.0,
            percent(*meters, total_m)
        );
    }
}

fn percent(part: f64, whole: f64) -> f64 {
    100.0 * part / whole.max(1.0)
}

fn discover(project: &Path, area: Option<GeoBounds>) -> Result<()> {
    let config = load_config(project)?;
    let area = area.or(config.area);
    fs::create_dir_all(project.join("sources"))?;
    let mut discovered = Vec::new();
    for path in source_files(&project.join("sources"))? {
        if let Some(mut candidate) = classify_path(&path) {
            candidate.fingerprint = Some(source_fingerprint(&path)?);
            discovered.push(candidate);
        }
    }
    let discovered_count = discovered.len();
    let mut manifest =
        load_source_manifest(project)?.unwrap_or_else(|| source_manifest(area, Vec::new()));
    merge_source_candidates(&mut manifest.candidates, discovered);
    refresh_source_coverage_for_area(&mut manifest, area);
    write_json(project.join("sources/manifest.json"), &manifest)?;
    write_bytes(
        project.join("sources/discovery.md"),
        render_discovery_report(&manifest),
    )?;
    println!(
        "discovered {} local candidate(s), recommended {} source class(es), evaluated {} source class(es); wrote {} and {}",
        discovered_count,
        manifest.recommendations.len(),
        manifest.coverage.len(),
        project.join("sources/manifest.json").display(),
        project.join("sources/discovery.md").display()
    );
    Ok(())
}

fn source_plan(project: &Path, kinds: &[SourceKind], include_satisfied: bool) -> Result<()> {
    let manifest = load_source_manifest(project)?
        .with_context(|| "read sources/manifest.json; run `trailgen discover` first")?;
    print!(
        "{}",
        render_source_plan(project, &manifest, kinds, include_satisfied)
    );
    Ok(())
}

fn cache_source(
    project: &Path,
    input: &str,
    output: Option<&Path>,
    kind: Option<SourceKind>,
    adapter: Option<&str>,
) -> Result<()> {
    fs::create_dir_all(project.join("sources"))?;
    let path = cached_source_path(project, input, output)?;
    let (kind, adapter_id) = cached_source_kind_adapter(&path, kind, adapter)?;
    let bytes = read_source_input(input)?;
    if looks_like_zip(input, &bytes) {
        extract_source_archive(&bytes, &path)?;
    } else {
        write_bytes(&path, &bytes)?;
        copy_shapefile_sidecars(input, &path)?;
    }
    let fingerprint = source_fingerprint(&path)?;
    let mut candidate = source_candidate(&path, kind, &adapter_id, fingerprint);
    candidate.origin = Some(input.to_owned());
    register_source_candidates(project, vec![candidate])?;
    println!(
        "cached source {} from {} ({} bytes)",
        path.display(),
        input,
        bytes.len()
    );
    Ok(())
}

struct OsmAcquisition {
    profile: OsmAcquireProfile,
    bbox: Option<GeoBounds>,
    output: Option<PathBuf>,
    endpoint: String,
    timeout_s: u64,
    print_query: bool,
}

impl OsmAcquireProfile {
    const fn default_output(self) -> &'static str {
        match self {
            Self::All => "osm-extract.osm",
            Self::Trails => "osm-trails.osm",
            Self::Roads => "roads.osm",
            Self::Hydrology => "hydrology.osm",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Trails => "trails",
            Self::Roads => "roads",
            Self::Hydrology => "hydrology",
        }
    }
}

#[derive(Clone, Copy)]
struct OsmAcquiredClass {
    kind: SourceKind,
    adapter_id: &'static str,
    label: &'static str,
    count: usize,
}

fn acquire_osm(project: &Path, acquisition: &OsmAcquisition) -> Result<()> {
    if acquisition.timeout_s == 0 {
        bail!("--timeout-s must be positive");
    }
    let config = load_config(project)?;
    let area = acquisition.bbox.or(config.area).with_context(
        || "acquire-osm requires --bbox or a project area from `trailgen init --bbox`",
    )?;
    let area_deg2 = (area.east - area.west) * (area.north - area.south);
    if area_deg2 > MAX_OVERPASS_BBOX_DEG2 {
        bail!(
            "Overpass bbox spans {area_deg2:.2} square degrees; limit is {MAX_OVERPASS_BBOX_DEG2:.2}; acquire a regional extract instead"
        );
    }
    let query = overpass_query(acquisition.profile, area, acquisition.timeout_s);
    if acquisition.print_query {
        print!("{query}");
        return Ok(());
    }
    let bytes = read_overpass_xml(&acquisition.endpoint, &query, acquisition.timeout_s)?;
    cache_acquired_osm(project, acquisition, area, &query, &bytes)
}

fn cache_acquired_osm(
    project: &Path,
    acquisition: &OsmAcquisition,
    area: GeoBounds,
    query: &str,
    bytes: &[u8],
) -> Result<()> {
    fs::create_dir_all(project.join("sources"))?;
    let output = acquisition
        .output
        .as_deref()
        .unwrap_or_else(|| Path::new(acquisition.profile.default_output()));
    let path = cached_source_path(project, acquisition.profile.default_output(), Some(output))?;
    if source_ext(&path).as_deref() != Some("osm") {
        bail!("acquire-osm writes OSM XML; --output must end in .osm");
    }
    let normalized =
        std::str::from_utf8(bytes).with_context(|| "Overpass response is not UTF-8 OSM XML")?;
    let acquired = validate_osm_acquisition(acquisition.profile, normalized)?;
    write_bytes(&path, bytes)?;
    write_bytes(path.with_extension("overpassql"), query)?;
    let fingerprint = source_fingerprint(&path)?;
    let origin = format!(
        "overpass:{} profile={} bbox={},{},{},{}",
        acquisition.endpoint,
        acquisition.profile.label(),
        area.west,
        area.south,
        area.east,
        area.north
    );
    let candidates = acquired
        .iter()
        .map(|class| {
            let mut candidate =
                source_candidate(&path, class.kind, class.adapter_id, fingerprint.clone());
            candidate.origin = Some(origin.clone());
            candidate
        })
        .collect::<Vec<_>>();
    register_source_candidates(project, candidates)?;
    println!(
        "acquired OSM {} extract into {}: {}",
        acquisition.profile.label(),
        path.display(),
        acquired
            .iter()
            .map(|class| format!("{} {}", class.count, class.label))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

fn validate_osm_acquisition(
    profile: OsmAcquireProfile,
    raw: &str,
) -> Result<Vec<OsmAcquiredClass>> {
    let drafts = if matches!(profile, OsmAcquireProfile::All | OsmAcquireProfile::Trails) {
        osm::network_from_str(raw)?.len()
    } else {
        0
    };
    let (roads, hydrology) = if matches!(
        profile,
        OsmAcquireProfile::All | OsmAcquireProfile::Roads | OsmAcquireProfile::Hydrology
    ) {
        osm_context_counts(raw)?
    } else {
        (0, 0)
    };
    let mut acquired = Vec::new();
    if drafts > 0 {
        acquired.push(OsmAcquiredClass {
            kind: SourceKind::TrailNetwork,
            adapter_id: "osm-xml-network",
            label: "trail",
            count: drafts,
        });
    }
    if roads > 0 {
        acquired.push(OsmAcquiredClass {
            kind: SourceKind::Road,
            adapter_id: "osm-road-context",
            label: "road",
            count: roads,
        });
    }
    if hydrology > 0 {
        acquired.push(OsmAcquiredClass {
            kind: SourceKind::Hydrology,
            adapter_id: "osm-hydrology-context",
            label: "hydrology",
            count: hydrology,
        });
    }
    acquired.retain(|class| match profile {
        OsmAcquireProfile::All => true,
        OsmAcquireProfile::Trails => class.kind == SourceKind::TrailNetwork,
        OsmAcquireProfile::Roads => class.kind == SourceKind::Road,
        OsmAcquireProfile::Hydrology => class.kind == SourceKind::Hydrology,
    });
    if acquired.is_empty() {
        bail!(
            "Overpass response contained no normalizable {} ways for the current adapter",
            profile.label()
        );
    }
    Ok(acquired)
}

fn osm_context_counts(raw: &str) -> Result<(usize, usize)> {
    let mut roads = 0;
    let mut hydrology = 0;
    for overlay in osm::context_overlays_from_str(raw)? {
        match overlay.kind {
            CrossingKind::Road => roads += 1,
            CrossingKind::Water => hydrology += 1,
        }
    }
    Ok((roads, hydrology))
}

fn read_overpass_xml(endpoint: &str, query: &str, timeout_s: u64) -> Result<Vec<u8>> {
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_s))
        .user_agent(concat!(
            "adequate-trailgen/",
            env!("CARGO_PKG_VERSION"),
            " source-acquisition"
        ))
        .build()
        .with_context(|| "build Overpass HTTP client")?
        .post(endpoint)
        .form(&[("data", query)])
        .send()
        .with_context(|| format!("POST Overpass query to {endpoint}"))?
        .error_for_status()
        .with_context(|| format!("Overpass endpoint {endpoint} returned an error status"))?;
    read_bounded(response, MAX_SOURCE_BYTES, "Overpass response")
}

fn overpass_query(profile: OsmAcquireProfile, area: GeoBounds, timeout_s: u64) -> String {
    let bbox = overpass_bbox(area);
    let selectors = overpass_selectors(profile);
    let mut query = format!("[out:xml][timeout:{timeout_s}];\n(\n");
    for selector in &selectors {
        let _ = writeln!(query, "  {selector}{bbox};");
    }
    query.push_str(");\n(._;>;);\nout body;\n");
    query
}

fn overpass_selectors(profile: OsmAcquireProfile) -> Vec<&'static str> {
    match profile {
        OsmAcquireProfile::All => [
            OSM_TRAIL_SELECTORS,
            OSM_ROAD_SELECTORS,
            OSM_HYDROLOGY_SELECTORS,
        ]
        .concat(),
        OsmAcquireProfile::Trails => OSM_TRAIL_SELECTORS.to_vec(),
        OsmAcquireProfile::Roads => OSM_ROAD_SELECTORS.to_vec(),
        OsmAcquireProfile::Hydrology => OSM_HYDROLOGY_SELECTORS.to_vec(),
    }
}

fn overpass_bbox(area: GeoBounds) -> String {
    format!(
        "({},{},{},{})",
        area.south, area.west, area.north, area.east
    )
}

fn verify_sources(project: &Path) -> Result<()> {
    let manifest = load_source_manifest(project)?.with_context(
        || "read sources/manifest.json; run `trailgen discover` or ingest sources first",
    )?;
    let checked = verify_source_fingerprints(project, &manifest)?;
    println!("verified {checked} source candidate(s)");
    Ok(())
}

fn verify_source_fingerprints(project: &Path, manifest: &SourceManifest) -> Result<usize> {
    let mut failures = Vec::new();
    let mut checked = 0usize;
    for candidate in &manifest.candidates {
        let Some(expected) = &candidate.fingerprint else {
            failures.push(format!("{} lacks fingerprint", candidate.path));
            continue;
        };
        let path = resolve_manifest_source_path(project, &candidate.path);
        match source_fingerprint(&path) {
            Ok(actual) if actual == *expected => checked += 1,
            Ok(actual) => failures.push(format!(
                "{} drifted: expected {} bytes sha256 {}, found {} bytes sha256 {}",
                candidate.path, expected.bytes, expected.sha256, actual.bytes, actual.sha256
            )),
            Err(error) => failures.push(format!("{} unreadable: {error:#}", candidate.path)),
        }
    }
    if !failures.is_empty() {
        bail!("source verification failed:\n{}", failures.join("\n"));
    }
    Ok(checked)
}

fn verify_generation(project: &Path) -> Result<()> {
    let ledger = load_generated_run_ledger(project)?.with_context(
        || "read routes/generated.manifest.json; run `trailgen generate` before verify-generation",
    )?;
    let artifacts = verify_generated_artifact_fingerprints(project, &ledger)?;
    let source_manifest = ledger
        .source_manifest
        .as_ref()
        .with_context(|| "generated manifest lacks source_manifest snapshot")?;
    let sources = verify_source_fingerprints(project, source_manifest)?;
    verify_source_coverage_summary(&ledger, source_manifest)?;
    verify_generated_seed_ledger(project, &ledger)?;
    verify_generated_run_metadata(project, &ledger)?;
    verify_generated_graph_manifest(project, &ledger)?;
    verify_forbidden_area_ledger(project, &ledger)?;
    let routes = verify_generated_route_sequences(project, &ledger)?;
    verify_generated_solver_replay(project, &ledger)?;
    println!(
        "verified generation: {artifacts} artifact(s), {sources} source candidate(s), {routes} route sequence(s)"
    );
    Ok(())
}

fn verify_generated_run_metadata(project: &Path, ledger: &GeneratedRunLedger) -> Result<()> {
    let config = ledger
        .effective_config
        .as_ref()
        .with_context(|| "generated manifest lacks effective_config snapshot")?;
    let graph = load_generated_graph(project)?;
    let mut failures = Vec::new();
    verify_generated_run_identity(ledger, config, &graph, &mut failures);
    verify_generated_start_metadata(ledger, config, &graph, &mut failures);
    if !failures.is_empty() {
        bail!(
            "generated run metadata verification failed:\n{}",
            failures.join("\n")
        );
    }
    Ok(())
}

fn verify_generated_run_identity(
    ledger: &GeneratedRunLedger,
    config: &ProjectConfig,
    graph: &TrailGraph,
    failures: &mut Vec<String>,
) {
    match ledger.schema_version {
        Some(1) => {}
        Some(actual) => failures.push(format!("schema_version mismatch: {actual} != 1")),
        None => failures.push("missing schema_version".to_owned()),
    }
    match ledger.app_version.as_deref() {
        Some(env!("CARGO_PKG_VERSION")) => {}
        Some(actual) => failures.push(format!(
            "app_version mismatch: {actual} != {}",
            env!("CARGO_PKG_VERSION")
        )),
        None => failures.push("missing app_version".to_owned()),
    }
    let requested_solver = ledger.requested_solver.unwrap_or_else(|| {
        failures.push("missing requested_solver".to_owned());
        config.solver
    });
    if requested_solver != config.solver {
        failures.push(format!(
            "requested_solver mismatch: {requested_solver:?} != {:?}",
            config.solver
        ));
    }
    let solver = ledger.solver.as_deref().unwrap_or_else(|| {
        failures.push("missing solver".to_owned());
        ""
    });
    let expected_solver = requested_solver.resolve(graph).label();
    if solver == "milp-incumbent-import" {
        if ledger.random_seed != Some(0) {
            failures.push(format!(
                "random_seed mismatch: {:?} != 0 for MILP incumbent import",
                ledger.random_seed
            ));
        }
    } else {
        if solver != expected_solver {
            failures.push(format!("solver mismatch: {solver} != {expected_solver}"));
        }
        if ledger.random_seed != Some(config.search.seed) {
            failures.push(format!(
                "random_seed mismatch: {:?} != {}",
                ledger.random_seed, config.search.seed
            ));
        }
    }
}

fn verify_generated_start_metadata(
    ledger: &GeneratedRunLedger,
    config: &ProjectConfig,
    graph: &TrailGraph,
    failures: &mut Vec<String>,
) {
    if ledger.requested_start.is_none() {
        failures.push("missing requested_start".to_owned());
    }
    if ledger.snapped_start_vertex.is_none() {
        failures.push("missing snapped_start_vertex".to_owned());
    }
    if ledger.snapped_start_coord.is_none() {
        failures.push("missing snapped_start_coord".to_owned());
    }
    if ledger.start_snap_m.is_none() {
        failures.push("missing start_snap_m".to_owned());
    }
    if let (
        Some(requested_start),
        Some(snapped_start_vertex),
        Some(snapped_start_coord),
        Some(start_snap_m),
    ) = (
        ledger.requested_start,
        ledger.snapped_start_vertex,
        ledger.snapped_start_coord,
        ledger.start_snap_m,
    ) {
        match graph.nearest_vertex_with_distance(requested_start) {
            Some((expected_vertex, expected_m)) => {
                if snapped_start_vertex != expected_vertex {
                    failures.push(format!(
                        "snapped_start_vertex mismatch: {snapped_start_vertex:?} != {expected_vertex:?}"
                    ));
                }
                if let Some(vertex) = graph.vertices.get(snapped_start_vertex.0) {
                    verify_coord(
                        "snapped_start_coord",
                        snapped_start_coord,
                        vertex.coord,
                        failures,
                    );
                } else {
                    failures.push(format!(
                        "snapped_start_vertex {snapped_start_vertex:?} is outside generated graph"
                    ));
                }
                verify_f64("start_snap_m", start_snap_m, expected_m, failures);
                if start_snap_m > config.max_start_snap_m {
                    failures.push(format!(
                        "start_snap_m exceeds max_start_snap_m: {start_snap_m:.12} > {:.12}",
                        config.max_start_snap_m
                    ));
                }
            }
            None => failures.push("generated graph has no vertices".to_owned()),
        }
    }
}

fn verify_generated_artifact_fingerprints(
    project: &Path,
    ledger: &GeneratedRunLedger,
) -> Result<usize> {
    if ledger.artifact_fingerprints.is_empty() {
        bail!("generated manifest lacks artifact_fingerprints; rerun generation with this version");
    }
    let fingerprint_paths = ledger
        .artifact_fingerprints
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<BTreeSet<_>>();
    let missing = ledger
        .artifacts
        .iter()
        .filter(|artifact| artifact.as_str() != "routes/generated.manifest.json")
        .filter(|artifact| !fingerprint_paths.contains(artifact.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "generated manifest lacks fingerprints for artifact(s): {}",
            missing.join(", ")
        );
    }

    let mut failures = Vec::new();
    let mut seen = BTreeSet::new();
    let mut checked = 0usize;
    for entry in &ledger.artifact_fingerprints {
        if entry.path == "routes/generated.manifest.json" {
            failures.push("routes/generated.manifest.json must not fingerprint itself".to_owned());
            continue;
        }
        if !seen.insert(entry.path.as_str()) {
            failures.push(format!("{} has duplicate artifact fingerprint", entry.path));
            continue;
        }
        let path = match resolve_generated_artifact_path(project, &entry.path) {
            Ok(path) => path,
            Err(error) => {
                failures.push(format!("{} invalid artifact path: {error:#}", entry.path));
                continue;
            }
        };
        match source_fingerprint(&path) {
            Ok(actual) if actual == entry.fingerprint => checked += 1,
            Ok(actual) => failures.push(format!(
                "{} drifted: expected {} bytes sha256 {}, found {} bytes sha256 {}",
                entry.path,
                entry.fingerprint.bytes,
                entry.fingerprint.sha256,
                actual.bytes,
                actual.sha256
            )),
            Err(error) => failures.push(format!("{} unreadable: {error:#}", entry.path)),
        }
    }
    if !failures.is_empty() {
        bail!(
            "generated artifact verification failed:\n{}",
            failures.join("\n")
        );
    }
    Ok(checked)
}

fn verify_source_coverage_summary(
    ledger: &GeneratedRunLedger,
    manifest: &SourceManifest,
) -> Result<()> {
    let actual = ledger
        .source_coverage_summary
        .as_ref()
        .with_context(|| "generated manifest lacks source_coverage_summary")?;
    let expected = summarize_source_coverage(&manifest.coverage);
    ensure!(
        actual == &expected,
        "generated source coverage summary verification failed: manifest source_coverage_summary drifted"
    );
    Ok(())
}

fn verify_generated_seed_ledger(project: &Path, ledger: &GeneratedRunLedger) -> Result<()> {
    let actual = seed_ledger_manifest(project)?;
    let expected = ledger
        .seed_ledger
        .as_ref()
        .with_context(|| "generated manifest lacks seed_ledger snapshot")?;
    ensure!(
        &actual == expected,
        "generated seed ledger verification failed: seeds/seeds.json drifted"
    );
    Ok(())
}

fn verify_generated_graph_manifest(project: &Path, ledger: &GeneratedRunLedger) -> Result<()> {
    let actual = ledger
        .graph
        .as_ref()
        .with_context(|| "generated manifest lacks graph summary")?;
    let graph = load_generated_graph(project)?;
    let expected = graph_manifest(&graph);
    let mut failures = Vec::new();
    verify_graph_manifest("graph", actual, &expected, &mut failures);
    if !failures.is_empty() {
        bail!(
            "generated graph verification failed:\n{}",
            failures.join("\n")
        );
    }
    Ok(())
}

fn verify_forbidden_area_ledger(project: &Path, ledger: &GeneratedRunLedger) -> Result<()> {
    if ledger.forbidden_areas.is_empty() {
        return Ok(());
    }
    let config = ledger
        .effective_config
        .as_ref()
        .with_context(|| "generated manifest lacks effective_config snapshot")?;
    let graph = load_generated_graph(project)?;
    let mut failures = Vec::new();
    let mut seen = BTreeSet::new();
    for area in &ledger.forbidden_areas {
        verify_forbidden_area(area, &graph, config, &mut seen, &mut failures);
    }
    if !failures.is_empty() {
        bail!(
            "generated forbidden-area verification failed:\n{}",
            failures.join("\n")
        );
    }
    Ok(())
}

fn verify_forbidden_area(
    area: &ForbiddenAreaLedger,
    graph: &TrailGraph,
    config: &ProjectConfig,
    seen: &mut BTreeSet<String>,
    failures: &mut Vec<String>,
) {
    if !seen.insert(area.path.clone()) {
        failures.push(format!(
            "{} appears twice in forbidden-area ledger",
            area.path
        ));
        return;
    }
    let path = PathBuf::from(&area.path);
    match source_fingerprint(&path) {
        Ok(actual) if actual == area.fingerprint => {}
        Ok(actual) => failures.push(format!(
            "{} forbidden-area source drifted: expected {} bytes sha256 {}, found {} bytes sha256 {}",
            area.path,
            area.fingerprint.bytes,
            area.fingerprint.sha256,
            actual.bytes,
            actual.sha256
        )),
        Err(error) => failures.push(format!(
            "{} forbidden-area source unreadable: {error:#}",
            area.path
        )),
    }
    let expected_adapter = forbidden_area_adapter_id(&path);
    if area.adapter_id != expected_adapter {
        failures.push(format!(
            "{} forbidden-area adapter mismatch: {} != {}",
            area.path, area.adapter_id, expected_adapter
        ));
    }
    let mut overlays = match access_overlays(&path) {
        Ok(overlays) => overlays,
        Err(error) => {
            failures.push(format!(
                "{} forbidden-area parse failed: {error:#}",
                area.path
            ));
            return;
        }
    };
    force_forbidden_area_overlays(&path, &mut overlays);
    if area.overlays != overlays.len() {
        failures.push(format!(
            "{} forbidden-area overlay count mismatch: {} != {}",
            area.path,
            area.overlays,
            overlays.len()
        ));
    }
    let mut graph = graph.clone();
    let touched_edges = apply_access_overlays(&mut graph, &overlays, None, config.difficulty);
    if area.touched_edges != touched_edges {
        failures.push(format!(
            "{} forbidden-area touched edge count mismatch: {} != {}",
            area.path, area.touched_edges, touched_edges
        ));
    }
}

fn verify_graph_manifest(
    label: &str,
    actual: &GraphManifest,
    expected: &GraphManifest,
    failures: &mut Vec<String>,
) {
    for (field, actual, expected) in [
        ("vertices", actual.vertices, expected.vertices),
        ("edges", actual.edges, expected.edges),
        (
            "directed_travel_edges",
            actual.directed_travel_edges,
            expected.directed_travel_edges,
        ),
        (
            "low_confidence_edges",
            actual.low_confidence_edges,
            expected.low_confidence_edges,
        ),
    ] {
        if actual != expected {
            failures.push(format!("{label}.{field} mismatch: {actual} != {expected}"));
        }
    }
    verify_f64(
        &format!("{label}.edge_km"),
        actual.edge_km,
        expected.edge_km,
        failures,
    );
    verify_turn_ban_manifest(
        &format!("{label}.turn_bans"),
        &actual.turn_bans,
        &expected.turn_bans,
        failures,
    );
    verify_graph_elevation_stats(
        &format!("{label}.elevation"),
        &actual.elevation,
        &expected.elevation,
        failures,
    );
    if actual.crossings != expected.crossings {
        failures.push(format!(
            "{label}.crossings mismatch: {:?} != {:?}",
            actual.crossings, expected.crossings
        ));
    }
    verify_f64_map(
        &format!("{label}.terrain_km"),
        &actual.terrain_km,
        &expected.terrain_km,
        failures,
    );
}

fn verify_turn_ban_manifest(
    label: &str,
    actual: &TurnBanManifest,
    expected: &TurnBanManifest,
    failures: &mut Vec<String>,
) {
    if actual.count != expected.count {
        failures.push(format!(
            "{label}.count mismatch: {} != {}",
            actual.count, expected.count
        ));
    }
    if actual.provenance != expected.provenance {
        failures.push(format!(
            "{label}.provenance mismatch: {:?} != {:?}",
            actual.provenance, expected.provenance
        ));
    }
}

fn verify_graph_elevation_stats(
    label: &str,
    actual: &GraphElevationStats,
    expected: &GraphElevationStats,
    failures: &mut Vec<String>,
) {
    for (field, actual, expected) in [
        (
            "attributed_edge_m",
            actual.attributed_edge_m,
            expected.attributed_edge_m,
        ),
        (
            "sampled_grade_m",
            actual.sampled_grade_m,
            expected.sampled_grade_m,
        ),
        ("ascent_m", actual.ascent_m, expected.ascent_m),
        ("descent_m", actual.descent_m, expected.descent_m),
        (
            "sustained_steep_m",
            actual.sustained_steep_m,
            expected.sustained_steep_m,
        ),
    ] {
        verify_f64(&format!("{label}.{field}"), actual, expected, failures);
    }
    verify_grade_distribution(
        &format!("{label}.grade_distribution_m"),
        actual.grade_distribution_m,
        expected.grade_distribution_m,
        failures,
    );
    if actual.provenance_edges != expected.provenance_edges {
        failures.push(format!(
            "{label}.provenance_edges mismatch: {:?} != {:?}",
            actual.provenance_edges, expected.provenance_edges
        ));
    }
}

fn resolve_generated_artifact_path(project: &Path, raw: &str) -> Result<PathBuf> {
    let path = PathBuf::from(raw);
    ensure_safe_relative_project_path(&path)?;
    Ok(project.join(path))
}

fn verify_generated_route_sequences(project: &Path, ledger: &GeneratedRunLedger) -> Result<usize> {
    let graph = load_generated_graph(project)?;
    let routes = load_generated_routes(project)?;
    let config = ledger
        .effective_config
        .as_ref()
        .with_context(|| "generated manifest lacks effective_config snapshot")?;
    let routes_by_name = routes
        .iter()
        .map(|route| (route.name.as_str(), route))
        .collect::<BTreeMap<_, _>>();
    let mut replayed = Vec::new();
    let mut failures = Vec::new();
    let mut seen = BTreeSet::new();
    for route in &routes {
        match verify_route_edge_walk(&graph, route.start, &route.edges) {
            Ok(()) => replayed.push(Route::from_edges(
                route.name.clone(),
                &graph,
                route.start,
                route.edges.clone(),
                &config.constraints,
            )),
            Err(error) => failures.push(format!("{} invalid edge walk: {error:#}", route.name)),
        }
    }
    rank_routes(&mut replayed, &config.constraints);
    let replayed_by_name = replayed
        .iter()
        .map(|route| (route.name.as_str(), route))
        .collect::<BTreeMap<_, _>>();
    for entry in &ledger.routes {
        if !seen.insert(entry.name.as_str()) {
            failures.push(format!(
                "{} appears twice in generation manifest routes",
                entry.name
            ));
            continue;
        }
        let Some(route) = routes_by_name.get(entry.name.as_str()) else {
            failures.push(format!(
                "{} missing from routes/generated.routes.json",
                entry.name
            ));
            continue;
        };
        verify_generated_route_record(
            entry,
            route,
            replayed_by_name.get(entry.name.as_str()).copied(),
            &mut failures,
        );
    }
    for route in &routes {
        if !seen.contains(route.name.as_str()) {
            failures.push(format!(
                "{} exists in routes/generated.routes.json but not in generation manifest",
                route.name
            ));
        }
    }
    if !failures.is_empty() {
        bail!(
            "generated route verification failed:\n{}",
            failures.join("\n")
        );
    }
    Ok(ledger.routes.len())
}

fn verify_generated_solver_replay(project: &Path, ledger: &GeneratedRunLedger) -> Result<()> {
    if ledger.solver.as_deref() == Some("milp-incumbent-import") {
        return Ok(());
    }
    let config = ledger
        .effective_config
        .as_ref()
        .with_context(|| "generated manifest lacks effective_config snapshot")?;
    let start = ledger
        .snapped_start_vertex
        .with_context(|| "generated manifest lacks snapped_start_vertex")?;
    let graph = load_generated_graph(project)?;
    let routes = load_generated_routes(project)?;
    if routes.is_empty() {
        return Ok(());
    }
    let replayed = solve_generation_routes(project, &graph, config, start, routes.len())?;
    let mut failures = Vec::new();
    if replayed.len() != routes.len() {
        failures.push(format!(
            "route count mismatch: persisted {} != replayed {}",
            routes.len(),
            replayed.len()
        ));
    }
    for (index, (persisted, replayed)) in routes.iter().zip(&replayed).enumerate() {
        verify_solver_replayed_route(index, persisted, replayed, &mut failures);
    }
    if !failures.is_empty() {
        bail!(
            "generated solver replay verification failed:\n{}",
            failures.join("\n")
        );
    }
    Ok(())
}

fn verify_solver_replayed_route(
    index: usize,
    persisted: &Route,
    replayed: &Route,
    failures: &mut Vec<String>,
) {
    let label = format!("route[{index}] {}", persisted.name);
    if persisted.name != replayed.name {
        failures.push(format!(
            "{label} name mismatch: {} != {}",
            persisted.name, replayed.name
        ));
    }
    if persisted.start != replayed.start {
        failures.push(format!(
            "{label} start mismatch: {:?} != {:?}",
            persisted.start, replayed.start
        ));
    }
    if persisted.edges != replayed.edges {
        failures.push(format!("{label} edge sequence differs from solver replay"));
    }
    if persisted.pareto_rank != replayed.pareto_rank {
        failures.push(format!(
            "{label} Pareto rank mismatch: {} != {}",
            persisted.pareto_rank, replayed.pareto_rank
        ));
    }
    if persisted.verdict != replayed.verdict {
        failures.push(format!(
            "{label} constraint verdict differs from solver replay"
        ));
    }
    verify_route_metrics(
        &format!("{label} metrics"),
        &persisted.metrics,
        &replayed.metrics,
        failures,
    );
    verify_f64(
        &format!("{label} score"),
        persisted.score,
        replayed.computed_score(),
        failures,
    );
}

fn verify_generated_route_record(
    entry: &RouteManifestEntry,
    route: &Route,
    replayed: Option<&Route>,
    failures: &mut Vec<String>,
) {
    if route.start != entry.start {
        failures.push(format!(
            "{} start mismatch: manifest {:?}, route {:?}",
            entry.name, entry.start, route.start
        ));
    }
    if route.edges != entry.edges {
        failures.push(format!(
            "{} edge sequence differs from manifest",
            entry.name
        ));
    }
    if route.pareto_rank != entry.rank {
        failures.push(format!(
            "{} rank mismatch: manifest {}, route {}",
            entry.name, entry.rank, route.pareto_rank
        ));
    }
    if route.verdict.satisfied != entry.satisfied {
        failures.push(format!(
            "{} satisfied mismatch: manifest {}, route {}",
            entry.name, entry.satisfied, route.verdict.satisfied
        ));
    }
    if route.verdict.violations != entry.violations {
        failures.push(format!(
            "{} violation ledger differs from manifest",
            entry.name
        ));
    }
    if route.verdict.audit != entry.audit {
        failures.push(format!(
            "{} constraint audit differs from manifest",
            entry.name
        ));
    }
    verify_route_metrics(
        &format!("{} manifest metrics", entry.name),
        &entry.metrics,
        &route.metrics,
        failures,
    );
    verify_f64(
        &format!("{} manifest score", entry.name),
        entry.score,
        route.computed_score(),
        failures,
    );
    if let Some(replayed) = replayed {
        verify_route_metrics(
            &format!("{} replayed metrics", entry.name),
            &route.metrics,
            &replayed.metrics,
            failures,
        );
        if route.verdict.satisfied != replayed.verdict.satisfied {
            failures.push(format!(
                "{} replayed satisfied mismatch: route {}, replayed {}",
                entry.name, route.verdict.satisfied, replayed.verdict.satisfied
            ));
        }
        if route.verdict.violations != replayed.verdict.violations {
            failures.push(format!(
                "{} replayed violations differ from effective constraints",
                entry.name
            ));
        }
        if route.verdict.audit != replayed.verdict.audit {
            failures.push(format!(
                "{} replayed constraint audit differs from effective constraints",
                entry.name
            ));
        }
        verify_f64(
            &format!("{} replayed penalty", entry.name),
            route.verdict.penalty,
            replayed.verdict.penalty,
            failures,
        );
        verify_f64(
            &format!("{} replayed score", entry.name),
            route.score,
            replayed.computed_score(),
            failures,
        );
        if route.pareto_rank != replayed.pareto_rank {
            failures.push(format!(
                "{} replayed Pareto rank mismatch: route {}, replayed {}",
                entry.name, route.pareto_rank, replayed.pareto_rank
            ));
        }
    }
}

macro_rules! verify_f64_fields {
    ($label:expr, $actual:expr, $expected:expr, $failures:expr; $($field:ident),+ $(,)?) => {
        $(
            verify_f64(
                &format!("{}.{}", $label, stringify!($field)),
                $actual.$field,
                $expected.$field,
                $failures,
            );
        )+
    };
}

fn verify_route_metrics(
    label: &str,
    actual: &RouteMetrics,
    expected: &RouteMetrics,
    failures: &mut Vec<String>,
) {
    if actual.shape != expected.shape {
        failures.push(format!(
            "{label}.shape mismatch: {:?} != {:?}",
            actual.shape, expected.shape
        ));
    }
    verify_f64_fields!(
        label,
        actual,
        expected,
        failures;
        distance_m,
        ascent_m,
        descent_m,
        difficulty,
        sustained_steep_m,
        road_fraction,
        low_confidence_fraction,
        restricted_access_fraction,
        repeated_edge_fraction,
    );
    verify_difficulty_breakdown(
        &format!("{label}.difficulty_breakdown"),
        actual.difficulty_breakdown,
        expected.difficulty_breakdown,
        failures,
    );
    verify_grade_distribution(
        &format!("{label}.grade_distribution"),
        actual.grade_distribution,
        expected.grade_distribution,
        failures,
    );
    if actual.crossings != expected.crossings {
        failures.push(format!(
            "{label}.crossings mismatch: {:?} != {:?}",
            actual.crossings, expected.crossings
        ));
    }
    verify_f64_map(
        &format!("{label}.access_m"),
        &actual.access_m,
        &expected.access_m,
        failures,
    );
    verify_f64_map(
        &format!("{label}.terrain_m"),
        &actual.terrain_m,
        &expected.terrain_m,
        failures,
    );
}

fn verify_grade_distribution(
    label: &str,
    actual: GradeDistribution,
    expected: GradeDistribution,
    failures: &mut Vec<String>,
) {
    verify_f64_fields!(
        label,
        actual,
        expected,
        failures;
        flat_m,
        rolling_m,
        steep_m,
        savage_m,
    );
}

fn verify_difficulty_breakdown(
    label: &str,
    actual: DifficultyBreakdown,
    expected: DifficultyBreakdown,
    failures: &mut Vec<String>,
) {
    for ((factor, actual), (expected_factor, expected)) in
        actual.factors().into_iter().zip(expected.factors())
    {
        debug_assert_eq!(factor, expected_factor);
        verify_f64(&format!("{label}.{factor:?}"), actual, expected, failures);
    }
}

fn verify_f64_map<K: Ord + Debug>(
    label: &str,
    actual: &BTreeMap<K, f64>,
    expected: &BTreeMap<K, f64>,
    failures: &mut Vec<String>,
) {
    let keys = actual
        .keys()
        .chain(expected.keys())
        .collect::<BTreeSet<_>>();
    for key in keys {
        verify_f64(
            &format!("{label}[{key:?}]"),
            actual.get(key).copied().unwrap_or_default(),
            expected.get(key).copied().unwrap_or_default(),
            failures,
        );
    }
}

fn verify_f64(label: &str, actual: f64, expected: f64, failures: &mut Vec<String>) -> bool {
    if nearly_equal(actual, expected) {
        return true;
    }
    failures.push(format!("{label} mismatch: {actual:.12} != {expected:.12}"));
    false
}

fn verify_coord(label: &str, actual: Coord, expected: Coord, failures: &mut Vec<String>) {
    verify_f64(&format!("{label}.lon"), actual.lon, expected.lon, failures);
    verify_f64(&format!("{label}.lat"), actual.lat, expected.lat, failures);
    match (actual.ele, expected.ele) {
        (Some(actual), Some(expected)) => {
            verify_f64(&format!("{label}.ele"), actual, expected, failures);
        }
        (None, None) => {}
        _ => failures.push(format!(
            "{label}.ele mismatch: {:?} != {:?}",
            actual.ele, expected.ele
        )),
    }
}

fn nearly_equal(a: f64, b: f64) -> bool {
    if a.to_bits() == b.to_bits() {
        return true;
    }
    if !a.is_finite() || !b.is_finite() {
        return false;
    }
    (a - b).abs() <= 1.0e-7_f64.max(a.abs().max(b.abs()) * 1.0e-9)
}

fn verify_route_edge_walk(graph: &TrailGraph, start: VertexId, edges: &[EdgeId]) -> Result<()> {
    if graph
        .vertices
        .get(start.0)
        .is_none_or(|vertex| vertex.id != start)
    {
        bail!("missing start vertex {start:?}");
    }
    let mut at = start;
    for edge_id in edges {
        let edge = graph
            .edges
            .get(edge_id.0)
            .with_context(|| format!("missing edge {edge_id:?}"))?;
        if edge.id != *edge_id {
            bail!(
                "edge index {} contains id {:?}, not {:?}",
                edge_id.0,
                edge.id,
                edge_id
            );
        }
        let Some(next) = edge.traverse(at) else {
            bail!("edge {edge_id:?} is not traversable from {at:?}");
        };
        at = next;
    }
    Ok(())
}

fn vet_sources(project: &Path, level: SourceGateLevel, require: &[SourceKind]) -> Result<()> {
    let mut manifest = load_source_manifest(project)?.with_context(
        || "read sources/manifest.json; run `trailgen discover` or ingest sources first",
    )?;
    let checked = verify_source_fingerprints(project, &manifest)?;
    refresh_source_coverage(&mut manifest);
    let failures = source_gate_failures(&manifest, level, require);
    if !failures.is_empty() {
        bail!(
            "source coverage gate failed after verifying {checked} candidate(s):\n{}",
            failures
                .iter()
                .map(|entry| source_gate_failure_line(entry))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let summary = summarize_source_coverage(&manifest.coverage);
    println!(
        "source coverage gate passed: level={}, explicit={}, verified={}, {}",
        level.label(),
        render_required_source_kinds(require),
        checked,
        source_coverage_summary_line(&summary)
    );
    Ok(())
}

fn enforce_generation_source_gate(project: &Path, gate: GenerationSourceGate) -> Result<()> {
    let Some(level) = gate.level() else {
        return Ok(());
    };
    let mut manifest = load_source_manifest(project)?.with_context(|| {
        format!(
            "generation source gate {gate:?} requires sources/manifest.json; run `trailgen discover` or `trailgen cache-source` first"
        )
    })?;
    let checked = verify_source_fingerprints(project, &manifest)?;
    refresh_source_coverage(&mut manifest);
    let failures = source_gate_failures(&manifest, level, &[]);
    if failures.is_empty() {
        return Ok(());
    }
    bail!(
        "generation source gate {} failed after verifying {checked} candidate(s):\n{}",
        gate.label(),
        failures
            .iter()
            .map(|entry| source_gate_failure_line(entry))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn source_gate_failures<'a>(
    manifest: &'a SourceManifest,
    level: SourceGateLevel,
    require: &[SourceKind],
) -> Vec<&'a SourceCoverage> {
    let explicitly_required = require.iter().copied().collect::<BTreeSet<_>>();
    manifest
        .coverage
        .iter()
        .filter(|entry| {
            entry.status != SourceCoverageStatus::Satisfied
                && (level.admits(entry.priority) || explicitly_required.contains(&entry.kind))
        })
        .collect()
}

fn source_gate_failure_line(entry: &SourceCoverage) -> String {
    format!(
        "- {} priority={} status={}: {}",
        source_kind_arg(entry.kind),
        source_priority_arg(entry.priority),
        source_coverage_status_arg(entry.status),
        entry.message
    )
}

fn source_coverage_summary_line(summary: &SourceCoverageSummary) -> String {
    format!(
        "required {}/{} satisfied, recommended {}/{} satisfied",
        summary.required.satisfied,
        summary.required.total,
        summary.recommended.satisfied,
        summary.recommended.total
    )
}

fn render_required_source_kinds(require: &[SourceKind]) -> String {
    if require.is_empty() {
        "none".to_owned()
    } else {
        require
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(source_kind_arg)
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn assemble_sources(
    project: &Path,
    date: Option<PlanningDate>,
    time: Option<PlanningTime>,
    elevation_confidence: f64,
) -> Result<()> {
    if !(0.0..=1.0).contains(&elevation_confidence) {
        bail!("elevation confidence must be in [0,1]");
    }
    verify_sources(project)?;
    let manifest = load_source_manifest(project)?.with_context(
        || "read sources/manifest.json; run `trailgen discover` or `trailgen cache-source` first",
    )?;
    let candidates = sorted_manifest_candidates(&manifest);
    let trail_networks = manifest_phase_paths(project, &candidates, &[SourceKind::TrailNetwork]);
    let seed_routes = manifest_phase_paths(project, &candidates, &[SourceKind::SeedRoute]);
    let build_sources = if trail_networks.is_empty() {
        &seed_routes
    } else {
        &trail_networks
    };
    if build_sources.is_empty() {
        bail!("assemble requires at least one trail-network candidate or seed-route scaffold");
    }

    build_many(project, build_sources.iter().map(PathBuf::as_path), None)?;
    let elevation_sources = manifest_phase_paths(project, &candidates, &[SourceKind::Elevation]);
    if elevation_sources.len() > 1 {
        apply_elevation_mosaic(project, &elevation_sources, elevation_confidence)
            .with_context(|| "assemble elevation mosaic")?;
    } else {
        for source in elevation_sources {
            apply_elevation(project, &source, elevation_confidence)
                .with_context(|| format!("assemble elevation {}", source.display()))?;
        }
    }
    for source in manifest_phase_paths(project, &candidates, &[SourceKind::Terrain]) {
        apply_terrain(project, &source)
            .with_context(|| format!("assemble terrain {}", source.display()))?;
    }
    for source in manifest_phase_paths(
        project,
        &candidates,
        &[SourceKind::Road, SourceKind::Hydrology],
    ) {
        apply_context(project, &source)
            .with_context(|| format!("assemble context {}", source.display()))?;
    }
    for source in seed_routes {
        import_seed(project, &source, None, None)
            .with_context(|| format!("assemble seed route {}", source.display()))?;
    }
    let access_sources = manifest_phase_paths(
        project,
        &candidates,
        &[SourceKind::Access, SourceKind::Closure],
    );
    if !access_sources.is_empty() {
        apply_access(project, &access_sources, date, time)
            .with_context(|| "assemble access/closure overlays")?;
    }

    let graph = load_graph(project)?;
    println!(
        "assembled graph from manifest: {} vertices, {} edges",
        graph.vertices.len(),
        graph.edges.len()
    );
    Ok(())
}

fn sorted_manifest_candidates(manifest: &SourceManifest) -> Vec<SourceCandidate> {
    let mut candidates = manifest.candidates.clone();
    candidates
        .sort_by(|a, b| (a.kind, &a.path, &a.adapter_id).cmp(&(b.kind, &b.path, &b.adapter_id)));
    candidates
}

fn manifest_phase_paths(
    project: &Path,
    candidates: &[SourceCandidate],
    kinds: &[SourceKind],
) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    for candidate in candidates {
        if !kinds.contains(&candidate.kind) {
            continue;
        }
        let path = resolve_manifest_source_path(project, &candidate.path);
        if seen.insert(path.display().to_string()) {
            paths.push(path);
        }
    }
    paths
}

fn resolve_manifest_source_path(project: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else if project.join(&path).exists() {
        project.join(path)
    } else {
        path
    }
}

struct GenerateOptions {
    start: String,
    min_km: f64,
    max_km: f64,
    count: usize,
    seed: u64,
    max_start_snap_m: Option<f64>,
    solver: Option<SolverKind>,
    max_hops: Option<usize>,
    max_frontier: Option<usize>,
    keep: Option<usize>,
    closure_paths: Option<usize>,
    source_gate: Option<GenerationSourceGate>,
    date: Option<PlanningDate>,
    time: Option<PlanningTime>,
    min_difficulty: Option<f64>,
    max_difficulty: Option<f64>,
    min_ascent_m: Option<f64>,
    max_ascent_m: Option<f64>,
    min_descent_m: Option<f64>,
    max_descent_m: Option<f64>,
    max_road_fraction: Option<f64>,
    max_low_confidence_fraction: Option<f64>,
    max_restricted_access_fraction: Option<f64>,
    shape: Vec<RouteShape>,
    max_repeated_edge_fraction: Option<f64>,
    forbidden_terrain: Vec<Terrain>,
    forbidden_area: Vec<PathBuf>,
    min_terrain: Vec<TerrainFraction>,
    max_terrain: Vec<TerrainFraction>,
}

struct MilpFormulationOptions {
    start: String,
    output: PathBuf,
    min_km: Option<f64>,
    max_km: Option<f64>,
    max_start_snap_m: Option<f64>,
    date: Option<PlanningDate>,
    time: Option<PlanningTime>,
}

struct MilpIncumbentOptions {
    start: String,
    solution: PathBuf,
    name: String,
    min_km: Option<f64>,
    max_km: Option<f64>,
    max_start_snap_m: Option<f64>,
    date: Option<PlanningDate>,
    time: Option<PlanningTime>,
}

#[derive(Clone, Debug, Serialize)]
struct GenerationManifest {
    schema_version: u32,
    app_version: &'static str,
    solver: String,
    requested_solver: SolverKind,
    random_seed: u64,
    requested_start: Coord,
    snapped_start_vertex: VertexId,
    snapped_start_coord: Coord,
    start_snap_m: f64,
    effective_config: ProjectConfig,
    source_manifest: Option<SourceManifest>,
    source_coverage_summary: Option<SourceCoverageSummary>,
    seed_ledger: SeedLedgerManifest,
    forbidden_areas: Vec<ForbiddenAreaManifest>,
    graph: GraphManifest,
    routes: Vec<RouteManifestEntry>,
    artifacts: Vec<String>,
    artifact_fingerprints: Vec<GeneratedArtifactFingerprint>,
}

#[derive(Clone, Debug, Deserialize)]
struct GenerationLedger {
    app_version: String,
    solver: String,
    requested_solver: SolverKind,
    random_seed: u64,
    requested_start: Coord,
    snapped_start_vertex: VertexId,
    snapped_start_coord: Coord,
    start_snap_m: f64,
    #[serde(default)]
    seed_ledger: Option<SeedLedgerManifest>,
    #[serde(default)]
    forbidden_areas: Vec<ForbiddenAreaLedger>,
    #[serde(default)]
    artifacts: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct SeedLedgerManifest {
    present: bool,
    routes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fingerprint: Option<SourceFingerprint>,
}

#[derive(Clone, Debug, Deserialize)]
struct GeneratedRunLedger {
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    app_version: Option<String>,
    #[serde(default)]
    solver: Option<String>,
    #[serde(default)]
    requested_solver: Option<SolverKind>,
    #[serde(default)]
    random_seed: Option<u64>,
    #[serde(default)]
    requested_start: Option<Coord>,
    #[serde(default)]
    snapped_start_vertex: Option<VertexId>,
    #[serde(default)]
    snapped_start_coord: Option<Coord>,
    #[serde(default)]
    start_snap_m: Option<f64>,
    #[serde(default)]
    effective_config: Option<ProjectConfig>,
    #[serde(default)]
    source_manifest: Option<SourceManifest>,
    #[serde(default)]
    source_coverage_summary: Option<SourceCoverageSummary>,
    #[serde(default)]
    seed_ledger: Option<SeedLedgerManifest>,
    #[serde(default)]
    graph: Option<GraphManifest>,
    #[serde(default)]
    forbidden_areas: Vec<ForbiddenAreaLedger>,
    #[serde(default)]
    routes: Vec<RouteManifestEntry>,
    #[serde(default)]
    artifacts: Vec<String>,
    #[serde(default)]
    artifact_fingerprints: Vec<GeneratedArtifactFingerprint>,
}

#[derive(Clone, Debug, Serialize)]
struct ForbiddenAreaManifest {
    path: String,
    adapter_id: String,
    fingerprint: SourceFingerprint,
    overlays: usize,
    touched_edges: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GeneratedArtifactFingerprint {
    path: String,
    fingerprint: SourceFingerprint,
}

#[derive(Clone, Debug, Deserialize)]
struct ForbiddenAreaLedger {
    path: String,
    adapter_id: String,
    fingerprint: SourceFingerprint,
    overlays: usize,
    touched_edges: usize,
}

#[derive(Clone, Copy, Debug)]
struct StartSnap {
    requested: Coord,
    snapped: VertexId,
    snapped_coord: Coord,
    distance_m: f64,
}

struct BuildSource {
    drafts: Vec<SegmentDraft>,
    kind: SourceKind,
    adapter_id: &'static str,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GraphManifest {
    vertices: usize,
    edges: usize,
    edge_km: f64,
    directed_travel_edges: usize,
    turn_bans: TurnBanManifest,
    elevation: GraphElevationStats,
    low_confidence_edges: usize,
    crossings: BTreeMap<CrossingKind, u32>,
    terrain_km: BTreeMap<Terrain, f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct GraphElevationStats {
    attributed_edge_m: f64,
    sampled_grade_m: f64,
    ascent_m: f64,
    descent_m: f64,
    sustained_steep_m: f64,
    grade_distribution_m: GradeDistribution,
    provenance_edges: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TurnBanManifest {
    count: usize,
    provenance: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RouteManifestEntry {
    name: String,
    start: VertexId,
    edges: Vec<EdgeId>,
    score: f64,
    metrics: RouteMetrics,
    satisfied: bool,
    violations: Vec<String>,
    #[serde(default)]
    audit: Vec<ConstraintAudit>,
    rank: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AccessBaseline {
    edges: Vec<EdgeAccessBaseline>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct EdgeAccessBaseline {
    edge: EdgeId,
    access: Access,
    #[serde(default)]
    travel: EdgeTravel,
    access_confidence: f64,
    confidence: f64,
    access_provenance: Vec<Provenance>,
}

impl AccessBaseline {
    fn capture(graph: &TrailGraph) -> Self {
        Self {
            edges: graph
                .edges
                .iter()
                .map(|edge| EdgeAccessBaseline {
                    edge: edge.id,
                    access: edge.attr.access,
                    travel: edge.attr.travel,
                    access_confidence: edge.attr.access_confidence,
                    confidence: edge.attr.confidence,
                    access_provenance: edge.attr.access_provenance.clone(),
                })
                .collect(),
        }
    }

    fn restore(&self, graph: &mut TrailGraph, weights: DifficultyWeights) -> Result<()> {
        if self.edges.len() != graph.edges.len() {
            bail!(
                "access baseline has {} edge(s), cached graph has {}; rebuild graph before changing planning-date access overlays",
                self.edges.len(),
                graph.edges.len()
            );
        }
        for baseline in &self.edges {
            let edge = graph
                .edges
                .get_mut(baseline.edge.0)
                .with_context(|| format!("missing edge {} in access baseline", baseline.edge.0))?;
            if edge.id != baseline.edge {
                bail!(
                    "access baseline edge id mismatch at index {}: cached graph has {:?}",
                    baseline.edge.0,
                    edge.id
                );
            }
            edge.attr.access = baseline.access;
            edge.attr.travel = baseline.travel;
            edge.attr.access_confidence = baseline.access_confidence;
            edge.attr.confidence = baseline.confidence;
            edge.attr
                .access_provenance
                .clone_from(&baseline.access_provenance);
            weights.apply_edge(edge);
        }
        graph.rebuild_adjacency();
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct Calibration {
    family: CalibrationFamily,
    before: f64,
    target: f64,
    selected: f64,
    fixed: f64,
    multiplier: f64,
    weights: DifficultyWeights,
}

#[derive(Serialize)]
struct DifficultySnippet {
    difficulty: DifficultyWeights,
}

fn generate(project: &Path, options: &GenerateOptions) -> Result<()> {
    let mut config = load_config(project)?;
    if let Some(solver) = options.solver {
        config.solver = solver;
    }
    if let Some(date) = options.date {
        config.planning_date = Some(date);
    }
    if let Some(time) = options.time {
        config.planning_time = Some(time);
    }
    if let Some(max_start_snap_m) = options.max_start_snap_m {
        config.max_start_snap_m = max_start_snap_m;
    }
    if let Some(source_gate) = options.source_gate {
        config.generation_source_gate = source_gate;
    }
    config.search.seed = options.seed;
    apply_generate_search_options(&mut config.search, options);
    apply_generate_options(&mut config.constraints, options);
    config.validate()?;
    enforce_generation_source_gate(project, config.generation_source_gate)?;
    let mut graph = materialize_effective_graph(project, &config)?;
    let forbidden_areas =
        apply_forbidden_area_sources(&mut graph, &options.forbidden_area, config.difficulty)?;
    let start_coord = parse_coord(&options.start)?;
    let start = snap_generation_start(&graph, start_coord, config.max_start_snap_m)?;
    let solver = config.solver.resolve(&graph);
    let routes = solve_generation_routes(project, &graph, &config, start.snapped, options.count)?;
    fs::create_dir_all(project.join("routes"))?;
    fs::create_dir_all(project.join("reports"))?;
    let previous_artifacts = previous_generated_artifacts(project)?;
    let (graph, routes) = write_generated_snapshots(project, &graph, &routes)?;
    write_json(
        project.join("routes/generated.geojson"),
        &geojson::routes_to_geojson(&graph, &routes),
    )?;
    let mut manifest = generation_manifest(GenerationManifestInput {
        project,
        options,
        config: &config,
        forbidden_areas: &forbidden_areas,
        graph: &graph,
        start,
        routes: &routes,
        solver_label: solver.label(),
    })?;
    write_generation_manifest(project, &manifest)?;
    write_route_artifacts(project, &graph, &routes)?;
    fs::write(
        project.join("reports/generated.md"),
        render_generated_report(project, &graph, &routes)?,
    )
    .with_context(|| "write generated report")?;
    write_bytes(
        project.join("reports/map.html"),
        render_map_html(&config.name, &graph, &routes)?,
    )?;
    finalize_generation_manifest(project, &mut manifest)?;
    remove_obsolete_generation_artifacts(project, previous_artifacts, &manifest.artifacts)?;
    println!("generated {} route(s)", routes.len());
    Ok(())
}

fn solve_generation_routes(
    project: &Path,
    graph: &TrailGraph,
    config: &ProjectConfig,
    start: VertexId,
    count: usize,
) -> Result<Vec<Route>> {
    let solver = config.solver.resolve(graph);
    let mut routes = solver.solve(config.search, graph, start, &config.constraints, count);
    routes.extend(
        load_seeds(project)?
            .iter()
            .filter_map(|seed| seed.as_route(graph, &config.constraints)),
    );
    let mut seen = BTreeSet::new();
    routes.retain(|route| seen.insert(route.edges.clone()));
    rank_routes(&mut routes, &config.constraints);
    routes.truncate(count);
    Ok(routes)
}

fn write_generated_snapshots(
    project: &Path,
    graph: &TrailGraph,
    routes: &[Route],
) -> Result<(TrailGraph, Vec<Route>)> {
    write_json(project.join("routes/generated.graph.json"), graph)?;
    write_json(project.join("routes/generated.routes.json"), routes)?;
    Ok((
        load_generated_graph(project)?,
        load_generated_routes(project)?,
    ))
}

fn formulate_milp(project: &Path, options: &MilpFormulationOptions) -> Result<()> {
    let mut config = load_config(project)?;
    if let Some(date) = options.date {
        config.planning_date = Some(date);
    }
    if let Some(time) = options.time {
        config.planning_time = Some(time);
    }
    if let Some(max_start_snap_m) = options.max_start_snap_m {
        config.max_start_snap_m = max_start_snap_m;
    }
    if let Some(min_km) = options.min_km {
        config.constraints.min_distance_m = min_km * 1_000.0;
    }
    if let Some(max_km) = options.max_km {
        config.constraints.max_distance_m = max_km * 1_000.0;
    }
    config.validate()?;
    if !config.constraints.allows_shape(RouteShape::Loop) {
        bail!(
            "MILP formulation currently encodes connected simple loops; allow shape loop before formulating"
        );
    }
    let graph = materialize_effective_graph(project, &config)?;
    let start_coord = parse_coord(&options.start)?;
    let start = snap_generation_start(&graph, start_coord, config.max_start_snap_m)?;
    let formulation = LoopMilpFormulation::formulate(&graph, start.snapped, &config.constraints);
    write_bytes(&options.output, formulation.to_lp())?;
    println!(
        "wrote MILP loop formulation with {} binary variable(s), {} row(s), and {} flow bound(s) to {}",
        formulation.binaries.len(),
        formulation.rows.len(),
        formulation.bounds.len(),
        options.output.display()
    );
    Ok(())
}

fn import_milp_solution(project: &Path, options: &MilpIncumbentOptions) -> Result<()> {
    ensure_route_artifact_key(&options.name)?;
    let mut config = load_config(project)?;
    if let Some(date) = options.date {
        config.planning_date = Some(date);
    }
    if let Some(time) = options.time {
        config.planning_time = Some(time);
    }
    if let Some(max_start_snap_m) = options.max_start_snap_m {
        config.max_start_snap_m = max_start_snap_m;
    }
    if let Some(min_km) = options.min_km {
        config.constraints.min_distance_m = min_km * 1_000.0;
    }
    if let Some(max_km) = options.max_km {
        config.constraints.max_distance_m = max_km * 1_000.0;
    }
    config.validate()?;
    if !config.constraints.allows_shape(RouteShape::Loop) {
        bail!("MILP incumbent import expects a connected simple loop; allow shape loop first");
    }
    let graph = materialize_effective_graph(project, &config)?;
    let start_coord = parse_coord(&options.start)?;
    let start = snap_generation_start(&graph, start_coord, config.max_start_snap_m)?;
    let raw = fs::read_to_string(&options.solution)
        .with_context(|| format!("read {}", options.solution.display()))?;
    let edges = route_edges_from_solution(&graph, start.snapped, &raw)
        .with_context(|| format!("import MILP incumbent {}", options.solution.display()))?;
    let mut routes = vec![Route::from_edges(
        options.name.clone(),
        &graph,
        start.snapped,
        edges,
        &config.constraints,
    )];
    rank_routes(&mut routes, &config.constraints);
    fs::create_dir_all(project.join("routes"))?;
    fs::create_dir_all(project.join("reports"))?;
    let previous_artifacts = previous_generated_artifacts(project)?;
    let (graph, routes) = write_generated_snapshots(project, &graph, &routes)?;
    write_json(
        project.join("routes/generated.geojson"),
        &geojson::routes_to_geojson(&graph, &routes),
    )?;
    let incumbent_options = milp_incumbent_generate_options(options, &config);
    let mut manifest = generation_manifest(GenerationManifestInput {
        project,
        options: &incumbent_options,
        config: &config,
        forbidden_areas: &[],
        graph: &graph,
        start,
        routes: &routes,
        solver_label: "milp-incumbent-import",
    })?;
    write_generation_manifest(project, &manifest)?;
    write_route_artifacts(project, &graph, &routes)?;
    fs::write(
        project.join("reports/generated.md"),
        render_generated_report(project, &graph, &routes)?,
    )
    .with_context(|| "write generated report")?;
    write_bytes(
        project.join("reports/map.html"),
        render_map_html(&config.name, &graph, &routes)?,
    )?;
    finalize_generation_manifest(project, &mut manifest)?;
    remove_obsolete_generation_artifacts(project, previous_artifacts, &manifest.artifacts)?;
    println!(
        "imported MILP incumbent {} as {} with {:.2} km and {} edge(s)",
        options.solution.display(),
        routes[0].name,
        routes[0].metrics.distance_m / 1_000.0,
        routes[0].edges.len()
    );
    Ok(())
}

fn milp_incumbent_generate_options(
    options: &MilpIncumbentOptions,
    config: &ProjectConfig,
) -> GenerateOptions {
    GenerateOptions {
        start: options.start.clone(),
        min_km: config.constraints.min_distance_m / 1_000.0,
        max_km: config.constraints.max_distance_m / 1_000.0,
        count: 1,
        seed: 0,
        max_start_snap_m: Some(config.max_start_snap_m),
        solver: None,
        max_hops: None,
        max_frontier: None,
        keep: None,
        closure_paths: None,
        source_gate: None,
        date: config.planning_date,
        time: config.planning_time,
        min_difficulty: None,
        max_difficulty: None,
        min_ascent_m: None,
        max_ascent_m: None,
        min_descent_m: None,
        max_descent_m: None,
        max_road_fraction: None,
        max_low_confidence_fraction: None,
        max_restricted_access_fraction: None,
        shape: vec![RouteShape::Loop],
        max_repeated_edge_fraction: None,
        forbidden_terrain: Vec::new(),
        forbidden_area: Vec::new(),
        min_terrain: Vec::new(),
        max_terrain: Vec::new(),
    }
}

fn write_route_artifacts(project: &Path, graph: &TrailGraph, routes: &[Route]) -> Result<()> {
    for route in routes {
        ensure_route_artifact_key(&route.name)?;
        write_json(
            project.join(format!("routes/{}.geojson", route.name)),
            &geojson::routes_to_geojson(graph, std::slice::from_ref(route)),
        )
        .with_context(|| format!("write GeoJSON for {}", route.name))?;
        fs::write(
            project.join(format!("routes/{}.gpx", route.name)),
            gpx::route_to_gpx(graph, route),
        )
        .with_context(|| format!("write GPX for {}", route.name))?;
        fs::write(
            project.join(format!("routes/{}.csv", route.name)),
            csv::route_to_csv(graph, route),
        )
        .with_context(|| format!("write CSV for {}", route.name))?;
        fs::write(
            project.join(format!("routes/{}.kml", route.name)),
            kml::route_to_kml(graph, route),
        )
        .with_context(|| format!("write KML for {}", route.name))?;
        fs::write(
            project.join(format!("routes/{}.kmz", route.name)),
            kmz::route_to_kmz(graph, route)?,
        )
        .with_context(|| format!("write KMZ for {}", route.name))?;
        write_bytes(
            project.join(format!("reports/{}.md", route.name)),
            render_generated_report_with_title(
                project,
                "Generated Hiking Route",
                graph,
                std::slice::from_ref(route),
            )?,
        )
        .with_context(|| format!("write report for {}", route.name))?;
    }
    Ok(())
}

fn snap_generation_start(
    graph: &TrailGraph,
    requested: Coord,
    max_snap_m: f64,
) -> Result<StartSnap> {
    let (snapped, distance_m) = graph
        .nearest_vertex_with_distance(requested)
        .with_context(|| "graph has no vertices")?;
    if distance_m > max_snap_m {
        bail!(
            "start coordinate is {:.0} m from nearest graph vertex {}, above max_start_snap_m {:.0}; choose a nearer trailhead or raise the limit",
            distance_m,
            snapped.0,
            max_snap_m
        );
    }
    Ok(StartSnap {
        requested,
        snapped,
        snapped_coord: graph.vertices[snapped.0].coord,
        distance_m,
    })
}

fn apply_generate_options(constraints: &mut LoopConstraints, options: &GenerateOptions) {
    constraints.min_distance_m = options.min_km * 1_000.0;
    constraints.max_distance_m = options.max_km * 1_000.0;
    if let Some(min_difficulty) = options.min_difficulty {
        constraints.min_difficulty = min_difficulty;
    }
    if let Some(max_difficulty) = options.max_difficulty {
        constraints.max_difficulty = max_difficulty;
    }
    if let Some(min_ascent_m) = options.min_ascent_m {
        constraints.min_ascent_m = min_ascent_m;
    }
    if let Some(max_ascent_m) = options.max_ascent_m {
        constraints.max_ascent_m = max_ascent_m;
    }
    if let Some(min_descent_m) = options.min_descent_m {
        constraints.min_descent_m = min_descent_m;
    }
    if let Some(max_descent_m) = options.max_descent_m {
        constraints.max_descent_m = max_descent_m;
    }
    if let Some(max_road_fraction) = options.max_road_fraction {
        constraints.max_road_fraction = max_road_fraction;
    }
    if let Some(max_low_confidence_fraction) = options.max_low_confidence_fraction {
        constraints.max_low_confidence_fraction = max_low_confidence_fraction;
    }
    if let Some(max_restricted_access_fraction) = options.max_restricted_access_fraction {
        constraints.max_restricted_access_fraction = max_restricted_access_fraction;
    }
    if !options.shape.is_empty() {
        constraints.allowed_shapes.clone_from(&options.shape);
    }
    if let Some(max_repeated_edge_fraction) = options.max_repeated_edge_fraction {
        constraints.max_repeated_edge_fraction = max_repeated_edge_fraction;
    }
    if !options.forbidden_terrain.is_empty() {
        constraints
            .forbidden_terrain
            .clone_from(&options.forbidden_terrain);
    }
    for TerrainFraction { terrain, fraction } in &options.min_terrain {
        constraints.min_terrain_fraction.insert(*terrain, *fraction);
    }
    for TerrainFraction { terrain, fraction } in &options.max_terrain {
        constraints.max_terrain_fraction.insert(*terrain, *fraction);
    }
}

const fn apply_generate_search_options(search: &mut SearchParams, options: &GenerateOptions) {
    if let Some(max_hops) = options.max_hops {
        search.max_hops = max_hops;
    }
    if let Some(max_frontier) = options.max_frontier {
        search.max_frontier = max_frontier;
    }
    if let Some(keep) = options.keep {
        search.keep = keep;
    }
    if let Some(closure_paths) = options.closure_paths {
        search.closure_paths = closure_paths;
    }
}

fn rate(
    project: &Path,
    route: &Path,
    max_route_snap_m: Option<f64>,
    output: Option<&Path>,
) -> Result<()> {
    let config = load_config(project)?;
    let graph = load_graph(project)?;
    let route = snapped_route(
        &graph,
        route,
        &config.constraints,
        "rated-route",
        max_route_snap_m.unwrap_or(config.max_route_snap_m),
    )?;
    let text = render_project_report(
        project,
        "Rated Hiking Route",
        &graph,
        &[route],
        &config.constraints,
    )?;
    if let Some(output) = output {
        write_bytes(output, text)?;
        println!("wrote rated-route report {}", output.display());
    } else {
        println!("{text}");
    }
    Ok(())
}

fn rerate(project: &Path) -> Result<()> {
    let config = load_config(project)?;
    let count = rerate_cached_graph(project, config.difficulty)?;
    println!("rerated {count} cached edge(s)");
    Ok(())
}

fn calibrate(
    project: &Path,
    route_path: &Path,
    target_difficulty: f64,
    family: CalibrationFamily,
    max_route_snap_m: Option<f64>,
    write: bool,
) -> Result<()> {
    let mut config = load_config(project)?;
    let graph = load_graph(project)?;
    let route = snapped_route(
        &graph,
        route_path,
        &config.constraints,
        "calibration-route",
        max_route_snap_m.unwrap_or(config.max_route_snap_m),
    )?;
    let calibration = calibrate_weights(
        config.difficulty,
        route.metrics.difficulty_breakdown,
        target_difficulty,
        family,
    )?;
    println!("{}", render_calibration(&calibration));
    if write {
        config.difficulty = calibration.weights;
        save_config(project, &config)?;
        let count = rerate_cached_graph(project, config.difficulty)?;
        println!(
            "wrote {}; rerated {count} cached edge(s)",
            project.join("trailgen.toml").display()
        );
    } else {
        println!("dry run; pass --write to update trailgen.toml and rerate cached graph surfaces");
    }
    Ok(())
}

fn snapped_route(
    graph: &TrailGraph,
    route_path: &Path,
    constraints: &LoopConstraints,
    name: &str,
    max_route_snap_m: f64,
) -> Result<Route> {
    let route_file = load_route_file(route_path)?;
    let snap = graph.snap_line_edges_within(&route_file.line, max_route_snap_m);
    ensure_route_snap_accepted(&snap.stats, max_route_snap_m, "route")?;
    let edges = snap.edges;
    let start = graph
        .snapped_line_start(&route_file.line, &edges)
        .with_context(|| "route snapped to edges but no traversable start vertex was found")?;
    ensure!(
        graph.walk_edges(start, &edges).is_some(),
        "route snapped to edges but violates directed travel or turn restrictions"
    );
    Ok(Route::from_edges(
        route_file.metadata.title_or(name),
        graph,
        start,
        edges,
        constraints,
    ))
}

fn ensure_route_snap_accepted(
    snap: &RouteSnapStats,
    max_route_snap_m: f64,
    noun: &str,
) -> Result<()> {
    if snap.snapped_segment_count == 0 {
        bail!(
            "{noun} did not snap to any graph edges within max_route_snap_m {max_route_snap_m:.0}"
        );
    }
    if snap.rejected_segment_count > 0 {
        bail!(
            "{noun} has {} segment(s) farther than max_route_snap_m {:.0}; max observed snap {:.0} m, mean {:.0} m",
            snap.rejected_segment_count,
            max_route_snap_m,
            snap.max_snap_m,
            snap.mean_snap_m
        );
    }
    if snap.disconnected_transition_count > 0 {
        bail!(
            "{noun} has {} snapped transition(s) that cannot be connected within the local match budget",
            snap.disconnected_transition_count
        );
    }
    Ok(())
}

fn calibrate_weights(
    weights: DifficultyWeights,
    breakdown: DifficultyBreakdown,
    target: f64,
    family: CalibrationFamily,
) -> Result<Calibration> {
    if !target.is_finite() || target <= 0.0 {
        bail!("target difficulty must be a positive finite number");
    }
    let before = breakdown.total();
    let selected = family.contribution(breakdown);
    if selected.abs() <= f64::EPSILON {
        bail!("calibration family {family:?} has zero contribution on this route");
    }
    let fixed = before - selected;
    let required = target - fixed;
    let multiplier = required / selected;
    if !multiplier.is_finite() || multiplier <= 0.0 {
        bail!(
            "target {target:.2} is unreachable by positively scaling {family:?}; fixed contribution is {fixed:.2}, selected contribution is {selected:.2}"
        );
    }
    let mut calibrated = weights;
    family.scale_weights(&mut calibrated, multiplier)?;
    Ok(Calibration {
        family,
        before,
        target,
        selected,
        fixed,
        multiplier,
        weights: calibrated,
    })
}

fn render_calibration(calibration: &Calibration) -> String {
    let mut text = String::new();
    let after = calibration
        .selected
        .mul_add(calibration.multiplier, calibration.fixed);
    let _ = writeln!(text, "family: {:?}", calibration.family);
    let _ = writeln!(text, "before difficulty: {:.2}", calibration.before);
    let _ = writeln!(text, "target difficulty: {:.2}", calibration.target);
    let _ = writeln!(
        text,
        "selected/fixed contribution: {:.2} / {:.2}",
        calibration.selected, calibration.fixed
    );
    let _ = writeln!(text, "weight multiplier: {:.6}", calibration.multiplier);
    let _ = writeln!(text, "projected difficulty: {after:.2}");
    let _ = writeln!(text, "\n{}", difficulty_toml(calibration.weights));
    text
}

fn difficulty_toml(difficulty: DifficultyWeights) -> String {
    toml::to_string_pretty(&DifficultySnippet { difficulty })
        .expect("serializing difficulty snippet must not fail")
}

impl CalibrationFamily {
    fn contribution(self, breakdown: DifficultyBreakdown) -> f64 {
        match self {
            Self::All => breakdown.total(),
            Self::Distance => breakdown.distance,
            Self::Elevation => breakdown.ascent + breakdown.descent,
            Self::Ascent => breakdown.ascent,
            Self::Descent => breakdown.descent,
            Self::Grade => breakdown.grade,
            Self::Terrain => breakdown.terrain,
            Self::Road => breakdown.road,
            Self::Technical => breakdown.technical,
            Self::Navigation => breakdown.navigation,
            Self::Confidence => breakdown.confidence,
            Self::Access => breakdown.access,
        }
    }

    fn scale_weights(self, weights: &mut DifficultyWeights, multiplier: f64) -> Result<()> {
        match self {
            Self::All => {
                scale_global_weights(weights, multiplier);
            }
            Self::Distance => weights.distance_per_km *= multiplier,
            Self::Elevation => {
                weights.ascent_per_m *= multiplier;
                weights.descent_per_m *= multiplier;
            }
            Self::Ascent => weights.ascent_per_m *= multiplier,
            Self::Descent => weights.descent_per_m *= multiplier,
            Self::Grade => weights.grade_per_abs_fraction *= multiplier,
            Self::Terrain => scale_terrain_offsets(weights, multiplier),
            Self::Road => weights.road_penalty *= multiplier,
            Self::Technical => weights.technical_penalty *= multiplier,
            Self::Navigation => weights.navigation_penalty *= multiplier,
            Self::Confidence => weights.low_confidence_penalty *= multiplier,
            Self::Access => weights.closed_access_penalty *= multiplier,
        }
        ensure_valid_difficulty_weights(*weights)
    }
}

fn scale_global_weights(weights: &mut DifficultyWeights, multiplier: f64) {
    weights.distance_per_km *= multiplier;
    weights.ascent_per_m *= multiplier;
    weights.descent_per_m *= multiplier;
    weights.grade_per_abs_fraction *= multiplier;
    scale_terrain_offsets(weights, multiplier);
    weights.road_penalty *= multiplier;
    weights.technical_penalty *= multiplier;
    weights.navigation_penalty *= multiplier;
    weights.low_confidence_penalty *= multiplier;
    weights.closed_access_penalty *= multiplier;
}

fn scale_terrain_offsets(weights: &mut DifficultyWeights, multiplier: f64) {
    macro_rules! scale {
        ($field:ident) => {
            weights.terrain_multipliers.$field =
                (weights.terrain_multipliers.$field - 1.0).mul_add(multiplier, 1.0);
        };
    }
    scale!(unknown);
    scale!(trail);
    scale!(forest);
    scale!(alpine);
    scale!(talus);
    scale!(scramble);
    scale!(pavement);
    scale!(road);
    scale!(water);
}

fn ensure_valid_difficulty_weights(weights: DifficultyWeights) -> Result<()> {
    let scalar_weights = [
        ("distance_per_km", weights.distance_per_km),
        ("ascent_per_m", weights.ascent_per_m),
        ("descent_per_m", weights.descent_per_m),
        ("grade_per_abs_fraction", weights.grade_per_abs_fraction),
        ("road_penalty", weights.road_penalty),
        ("technical_penalty", weights.technical_penalty),
        ("navigation_penalty", weights.navigation_penalty),
        ("low_confidence_penalty", weights.low_confidence_penalty),
        ("closed_access_penalty", weights.closed_access_penalty),
        (
            "terrain_multipliers.unknown",
            weights.terrain_multipliers.unknown,
        ),
        (
            "terrain_multipliers.trail",
            weights.terrain_multipliers.trail,
        ),
        (
            "terrain_multipliers.forest",
            weights.terrain_multipliers.forest,
        ),
        (
            "terrain_multipliers.alpine",
            weights.terrain_multipliers.alpine,
        ),
        (
            "terrain_multipliers.talus",
            weights.terrain_multipliers.talus,
        ),
        (
            "terrain_multipliers.scramble",
            weights.terrain_multipliers.scramble,
        ),
        (
            "terrain_multipliers.pavement",
            weights.terrain_multipliers.pavement,
        ),
        ("terrain_multipliers.road", weights.terrain_multipliers.road),
        (
            "terrain_multipliers.water",
            weights.terrain_multipliers.water,
        ),
    ];
    for (name, value) in scalar_weights {
        if !value.is_finite() || value <= 0.0 {
            bail!("calibration produced invalid {name}={value}");
        }
    }
    Ok(())
}

fn export_route(
    project: &Path,
    route_name: &str,
    format: ExportFormat,
    output: &Path,
    report_output: Option<&Path>,
) -> Result<()> {
    let graph = load_generated_graph(project)?;
    let routes = load_generated_routes(project)?;
    let route = select_route(&routes, route_name)?;
    match format {
        ExportFormat::Gpx => write_bytes(output, gpx::route_to_gpx(&graph, route)),
        ExportFormat::Geojson => write_json(
            output,
            &geojson::routes_to_geojson(&graph, &[(*route).clone()]),
        ),
        ExportFormat::Csv => write_bytes(output, csv::route_to_csv(&graph, route)),
        ExportFormat::Kml => write_bytes(output, kml::route_to_kml(&graph, route)),
        ExportFormat::Kmz => write_bytes(output, kmz::route_to_kmz(&graph, route)?),
    }?;
    println!("exported {} to {}", route.name, output.display());
    if let Some(report_output) = report_output {
        write_bytes(
            report_output,
            render_selected_generated_report(project, &graph, route)?,
        )?;
        println!("wrote report {}", report_output.display());
    }
    Ok(())
}

fn report_generated(project: &Path, route_name: Option<&str>, output: Option<&Path>) -> Result<()> {
    let graph = load_generated_graph(project)?;
    let routes = load_generated_routes(project)?;
    let text = if let Some(route_name) = route_name {
        render_selected_generated_report(project, &graph, select_route(&routes, route_name)?)?
    } else {
        render_generated_report(project, &graph, &routes)?
    };
    if let Some(output) = output {
        write_bytes(output, text)?;
        println!("wrote report {}", output.display());
    } else {
        println!("{text}");
    }
    Ok(())
}

fn render_selected_generated_report(
    project: &Path,
    graph: &TrailGraph,
    route: &Route,
) -> Result<String> {
    render_generated_report_with_title(
        project,
        "Generated Hiking Route",
        graph,
        std::slice::from_ref(route),
    )
}

fn render_generated_report(project: &Path, graph: &TrailGraph, routes: &[Route]) -> Result<String> {
    render_generated_report_with_title(project, "Generated Hiking Routes", graph, routes)
}

fn render_generated_report_with_title(
    project: &Path,
    title: &str,
    graph: &TrailGraph,
    routes: &[Route],
) -> Result<String> {
    let constraints = load_generated_constraints(project)?.unwrap_or_else(|| {
        load_config(project)
            .map(|config| config.constraints)
            .unwrap_or_default()
    });
    let source_manifest = match load_generated_source_manifest(project)? {
        Some(manifest) => Some(manifest),
        None => load_source_manifest(project)?,
    };
    let ledger = load_generated_ledger(project)?;
    Ok(render_report(
        title,
        graph,
        routes,
        &constraints,
        ledger.as_ref(),
        source_manifest.as_ref(),
    ))
}

fn map_html(project: &Path, output: Option<&Path>) -> Result<()> {
    let routes = load_generated_routes(project).unwrap_or_default();
    let config = load_config(project)?;
    let graph = if routes.is_empty() {
        materialize_effective_graph(project, &config)?
    } else {
        load_generated_graph(project)?
    };
    let output = output.map_or_else(|| project.join("reports/map.html"), Path::to_path_buf);
    write_bytes(&output, render_map_html(&config.name, &graph, &routes)?)?;
    println!("wrote map {}", output.display());
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "A self-contained offline HTML artifact is clearer as one template than as scattered string shards."
)]
fn render_map_html(project_name: &str, graph: &TrailGraph, routes: &[Route]) -> Result<String> {
    let graph_json = js_json(&geojson::graph_to_geojson(graph))?;
    let routes_json = js_json(&geojson::routes_to_geojson(graph, routes))?;
    let title = html_text(project_name);
    Ok(format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} trailgen map</title>
<style>
:root {{ color-scheme: dark; font-family: ui-sans-serif, system-ui, sans-serif; background: #101318; color: #edf2f7; }}
body {{ margin: 0; display: grid; grid-template-columns: minmax(0, 1fr) 360px; min-height: 100vh; }}
main {{ padding: 16px; }}
aside {{ border-left: 1px solid #2d3748; padding: 16px; background: #171b22; overflow: auto; }}
h1, h2, h3 {{ margin: 0 0 12px; }}
#map {{ width: 100%; height: calc(100vh - 32px); background: #0b0f14; border: 1px solid #2d3748; border-radius: 10px; }}
.edge {{ fill: none; stroke-linecap: round; vector-effect: non-scaling-stroke; }}
.route {{ fill: none; stroke-linecap: round; stroke-linejoin: round; vector-effect: non-scaling-stroke; cursor: pointer; }}
.route:hover {{ stroke-width: 7; }}
.route.selected {{ stroke-width: 8; }}
.route.dimmed {{ opacity: .22; }}
.halo {{ fill: none; stroke: #05070a; stroke-width: 8; opacity: .7; vector-effect: non-scaling-stroke; }}
.halo.dimmed {{ opacity: .18; }}
.closed {{ stroke-dasharray: 6 4; }}
.legend {{ display: grid; grid-template-columns: 18px 1fr; gap: 6px 8px; font-size: 13px; margin: 12px 0 18px; }}
.swatch {{ width: 18px; height: 12px; border-radius: 3px; align-self: center; }}
.route-card {{ border: 1px solid #2d3748; border-radius: 8px; padding: 10px; margin: 10px 0; background: #101318; cursor: pointer; }}
.route-card.selected {{ border-color: #faf089; box-shadow: 0 0 0 1px #faf089 inset; }}
.diag {{ display: grid; grid-template-columns: 1fr auto; gap: 4px 12px; margin: 8px 0; font-size: 13px; }}
.diag dt {{ color: #a0aec0; }}
.diag dd {{ margin: 0; font-variant-numeric: tabular-nums; }}
.controls {{ display: grid; gap: 6px; margin: 8px 0 14px; font-size: 13px; color: #cbd5e0; }}
.controls label {{ display: flex; gap: 8px; align-items: center; }}
.meter {{ height: 8px; border-radius: 999px; background: #2d3748; overflow: hidden; margin: 4px 0 10px; }}
.meter > span {{ display: block; height: 100%; background: #63b3ed; }}
.raw {{ max-height: 320px; overflow: auto; border: 1px solid #2d3748; border-radius: 8px; padding: 8px; background: #0b0f14; }}
.audit {{ margin: 8px 0; padding-left: 18px; font-size: 13px; }}
.audit li {{ margin: 4px 0; }}
.ok {{ color: #68d391; }}
.bad {{ color: #fc8181; }}
code {{ color: #f6ad55; }}
small {{ color: #a0aec0; }}
</style>
</head>
<body>
<main>
<svg id="map" role="img" aria-label="Trail graph and generated route map"></svg>
</main>
<aside>
<h1>{title}</h1>
<p><small>Offline diagnostic map. Graph edges are colored by terrain and faded by confidence; generated routes are drawn thick above the graph.</small></p>
<h2>Terrain</h2>
<div class="legend" id="legend"></div>
<h2>Encoding</h2>
<div class="diag">
<dt>edge color</dt><dd>terrain bucket</dd>
<dt>edge width</dt><dd>scalar difficulty</dd>
<dt>edge opacity</dt><dd>confidence</dd>
<dt>dashed edge</dt><dd>closed access</dd>
</div>
<h2>Routes</h2>
<div class="controls">
<label><input id="dim-unselected" type="checkbox" checked> dim unselected routes after selection</label>
<label><input id="show-raw" type="checkbox" checked> show raw selected properties</label>
</div>
<div id="routes"></div>
<h2>Selected</h2>
<div id="details"><small>Click a route or edge.</small></div>
</aside>
<script>
const graph = {graph_json};
const routes = {routes_json};
const terrainColors = {{
  unknown: '#718096', trail: '#68d391', forest: '#38a169', alpine: '#90cdf4',
  talus: '#b7791f', scramble: '#f56565', pavement: '#a0aec0', road: '#f6ad55', water: '#63b3ed'
}};
const routeColors = ['#f56565', '#f6ad55', '#faf089', '#68d391', '#63b3ed', '#b794f4', '#f687b3'];
const svg = document.getElementById('map');
const details = document.getElementById('details');
const dimUnselected = document.getElementById('dim-unselected');
const showRaw = document.getElementById('show-raw');
const edgeById = Object.fromEntries(graph.features.map(f => [f.properties.edge_id, f.properties]));
let selectedRouteName = null;
const routeHandles = [];
function coords(feature) {{ return feature.geometry && feature.geometry.coordinates || []; }}
function allPoints() {{
  return graph.features.concat(routes.features).flatMap(f => coords(f).map(c => [c[0], c[1]]));
}}
const pts = allPoints();
const xs = pts.map(p => p[0]), ys = pts.map(p => p[1]);
const bounds = pts.length
  ? {{ west: Math.min(...xs), east: Math.max(...xs), south: Math.min(...ys), north: Math.max(...ys) }}
  : {{ west: 0, east: 1, south: 0, north: 1 }};
const W = 1200, H = 850, P = 36;
svg.setAttribute('viewBox', `0 0 ${{W}} ${{H}}`);
function project(c) {{
  const dx = Math.max(bounds.east - bounds.west, 1e-9);
  const dy = Math.max(bounds.north - bounds.south, 1e-9);
  return [P + (c[0] - bounds.west) / dx * (W - 2 * P), H - P - (c[1] - bounds.south) / dy * (H - 2 * P)];
}}
function path(feature) {{ return coords(feature).map((c, i) => `${{i ? 'L' : 'M'}}${{project(c)[0].toFixed(2)}} ${{project(c)[1].toFixed(2)}}`).join(' '); }}
function el(name, attrs) {{
  const node = document.createElementNS('http://www.w3.org/2000/svg', name);
  Object.entries(attrs).forEach(([k, v]) => node.setAttribute(k, v));
  return node;
}}
function pct(x) {{ return `${{(100 * (x || 0)).toFixed(1)}}%`; }}
function km(x) {{ return `${{((x || 0) / 1000).toFixed(2)}} km`; }}
function scalar(x, digits = 1) {{ return Number(x || 0).toFixed(digits); }}
function topEntries(obj, n = 4) {{
  return Object.entries(obj || {{}}).filter(([, v]) => Number(v) > 0).sort((a, b) => Number(b[1]) - Number(a[1])).slice(0, n);
}}
function difficultyText(breakdown) {{
  const xs = topEntries(breakdown, 5);
  return xs.length ? xs.map(([k, v]) => `${{k}} ${{scalar(v)}}`).join(', ') : 'none';
}}
function mixText(obj) {{
  const xs = topEntries(obj, 6);
  return xs.length ? xs.map(([k, v]) => `${{k}} ${{pct(v)}}`).join(', ') : 'none';
}}
function gradeText(obj) {{
  const total = Object.values(obj || {{}}).reduce((a, b) => a + Number(b || 0), 0);
  if (total <= 0) return 'none';
  return [
    ['flat <5%', obj.flat_m],
    ['rolling 5-15%', obj.rolling_m],
    ['steep 15-30%', obj.steep_m],
    ['savage ≥30%', obj.savage_m]
  ].map(([k, v]) => `${{k}} ${{pct(Number(v || 0) / total)}}`).join(', ');
}}
function sourceText(p) {{
  const xs = (p.source_provenance || []).map(x => {{
    const source = x.source || 'unknown';
    const base = x.source_id ? `${{source}}:${{x.source_id}}` : source;
    return x.layer ? `${{base}} (${{x.layer}})` : base;
  }});
  return xs.length ? xs.join(', ') : 'none';
}}
function accessWarningText(e) {{
  const prov = (e.access_provenance || []).map(p => p.source_id ? `${{p.source}}:${{p.source_id}}` : p.source).join(', ') || e.provenance || 'unknown';
  return `edge ${{e.edge_id}} ${{e.access || 'unknown'}} ${{pct(e.access_confidence)}} ${{prov}}`;
}}
function directedTravelText(e) {{
  return `edge ${{e.edge_id}} ${{e.travel || 'unknown'}} ${{km(e.length_m)}} ${{pct(e.confidence)}}`;
}}
function routeText(p) {{
  return `${{p.name}} | rank ${{p.pareto_rank}} | ${{km(p.distance_m)}} | ascent ${{(p.ascent_m || 0).toFixed(0)}} m | difficulty ${{(p.difficulty || 0).toFixed(1)}} | road ${{pct(p.road_fraction)}} | low confidence ${{pct(p.low_confidence_fraction)}} | restricted ${{pct(p.restricted_access_fraction)}}`;
}}
function escapeHtml(x) {{
  return String(x).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');
}}
function rawBlock(kind, p) {{
  if (!showRaw.checked) return '';
  return `<h3>Raw ${{kind}} properties</h3><pre class="raw">${{escapeHtml(JSON.stringify({{kind, ...p}}, null, 2))}}</pre>`;
}}
function metricRow(k, v) {{ return `<dt>${{escapeHtml(k)}}</dt><dd>${{escapeHtml(v)}}</dd>`; }}
function confidenceMeter(x) {{
  const pctValue = Math.max(0, Math.min(100, 100 * Number(x || 0)));
  return `<div class="meter"><span style="width:${{pctValue.toFixed(0)}}%"></span></div>`;
}}
function edgeSummary(p) {{
  return `<h3>Edge ${{p.edge_id}}</h3><dl class="diag">
    ${{metricRow('terrain', p.terrain || 'unknown')}}
    ${{metricRow('surface', p.surface || 'unknown')}}
    ${{metricRow('access', p.access || 'unknown')}}
    ${{metricRow('length', km(p.length_m))}}
    ${{metricRow('ascent/descent', `${{scalar(p.ascent_m, 0)}} m / ${{scalar(p.descent_m, 0)}} m`)}}
    ${{metricRow('max grade', pct(p.grade_abs_max))}}
    ${{metricRow('difficulty', scalar(p.difficulty))}}
    ${{metricRow('confidence', pct(p.confidence))}}
    ${{metricRow('terrain evidence', (p.terrain_evidence || []).slice(0, 2).map(e => `${{e.terrain}} ${{pct(e.confidence)}}: ${{e.rationale}}`).join(' | ') || 'none')}}
    ${{metricRow('difficulty factors', difficultyText(p.difficulty_breakdown))}}
  </dl>${{confidenceMeter(p.confidence)}}${{rawBlock('edge', p)}}`;
}}
function routeDiagnostics(p) {{
  const edges = (p.edges || []).map(id => edgeById[id]).filter(Boolean);
  const dubious = edges.slice().sort((a, b) => Number(a.confidence || 0) - Number(b.confidence || 0)).slice(0, 5);
  const brutal = edges.slice().sort((a, b) => Number(b.difficulty || 0) - Number(a.difficulty || 0)).slice(0, 5);
  const routeDubious = (p.dubious_edges || []).map(e => `edge ${{e.edge_id}} ${{pct(e.confidence)}} ${{e.terrain || 'unknown'}}`);
  const routeLowConfidence = (p.low_confidence_edges || []).map(e => `edge ${{e.edge_id}} ${{pct(e.confidence)}} ${{e.terrain || 'unknown'}}`);
  const routeAccessWarnings = (p.access_warning_edges || []).map(accessWarningText);
  const routeDirectedTravel = (p.directed_travel_edges || []).map(directedTravelText);
  const routeHotspots = (p.difficulty_hotspots || []).map(e => `edge ${{e.edge_id}} ${{e.factor}} ${{scalar(e.value)}} ${{e.terrain || 'unknown'}}`);
  return {{
    accessWarnings: routeAccessWarnings.join(', ') || 'none',
    directedTravel: routeDirectedTravel.join(', ') || 'none',
    lowConfidence: routeLowConfidence.join(', ') || 'none',
    dubious: routeDubious.join(', ') || dubious.map(e => `edge ${{e.edge_id}} ${{pct(e.confidence)}} ${{e.terrain || 'unknown'}}`).join(', ') || 'none',
    brutal: routeHotspots.join(', ') || brutal.map(e => `edge ${{e.edge_id}} ${{scalar(e.difficulty)}} ${{e.terrain || 'unknown'}}`).join(', ') || 'none'
  }};
}}
function constraintAudit(p) {{
  const rows = p.constraint_audit || [];
  if (!rows.length) return '';
  return `<h3>Constraint audit</h3><ul class="audit">${{rows.map(row => `<li class="${{row.satisfied ? 'ok' : 'bad'}}">${{escapeHtml(row.metric)}}: measured ${{escapeHtml(row.measured)}}, requires ${{escapeHtml(row.requirement)}}, margin ${{escapeHtml(row.margin)}}</li>`).join('')}}</ul>`;
}}
function routeSummary(p) {{
  const d = routeDiagnostics(p);
  return `<h3>${{escapeHtml(p.name || 'route')}}</h3><dl class="diag">
    ${{metricRow('constraint verdict', p.satisfied ? 'satisfied' : 'violated')}}
    ${{metricRow('pareto rank', p.pareto_rank)}}
    ${{metricRow('score', scalar(p.score))}}
    ${{metricRow('shape', p.shape || 'unknown')}}
    ${{metricRow('source provenance', sourceText(p))}}
    ${{metricRow('distance', km(p.distance_m))}}
    ${{metricRow('ascent/descent', `${{scalar(p.ascent_m, 0)}} m / ${{scalar(p.descent_m, 0)}} m`)}}
    ${{metricRow('sustained steepness', km(p.sustained_steep_m))}}
    ${{metricRow('grade distribution', gradeText(p.grade_distribution))}}
    ${{metricRow('difficulty', scalar(p.difficulty))}}
    ${{metricRow('difficulty factors', difficultyText(p.difficulty_breakdown))}}
    ${{metricRow('road/pavement', pct(p.road_fraction))}}
    ${{metricRow('low confidence', pct(p.low_confidence_fraction))}}
    ${{metricRow('restricted access', pct(p.restricted_access_fraction))}}
    ${{metricRow('repeated edge', pct(p.repeated_edge_fraction))}}
    ${{metricRow('terrain mix', mixText(p.terrain_fraction))}}
    ${{metricRow('access mix', mixText(p.access_fraction))}}
    ${{metricRow('violations', (p.violations || []).join(' | ') || 'none')}}
    ${{metricRow('access warnings', d.accessWarnings)}}
    ${{metricRow('directed travel constraints', d.directedTravel)}}
    ${{metricRow('low-confidence segments', d.lowConfidence)}}
    ${{metricRow('dubious segments', d.dubious)}}
    ${{metricRow('largest difficulty contributors', d.brutal)}}
  </dl>${{constraintAudit(p)}}${{rawBlock('route', p)}}`;
}}
function refreshRouteSelection() {{
  for (const h of routeHandles) {{
    const selected = selectedRouteName && h.name === selectedRouteName;
    h.route.classList.toggle('selected', selected);
    h.card.classList.toggle('selected', selected);
    const dim = Boolean(selectedRouteName && dimUnselected.checked && !selected);
    h.route.classList.toggle('dimmed', dim);
    h.halo.classList.toggle('dimmed', dim);
  }}
}}
function show(kind, p) {{
  if (kind === 'route') {{
    selectedRouteName = p.name || null;
    refreshRouteSelection();
  }}
  details.innerHTML = kind === 'route' ? routeSummary(p) : edgeSummary(p);
}}
dimUnselected.addEventListener('change', refreshRouteSelection);
showRaw.addEventListener('change', () => {{
  if (selectedRouteName) {{
    const route = routes.features.map(f => f.properties || {{}}).find(p => p.name === selectedRouteName);
    if (route) show('route', route);
  }}
}});
for (const [terrain, color] of Object.entries(terrainColors)) {{
  document.getElementById('legend').append(Object.assign(document.createElement('span'), {{className: 'swatch', style: `background:${{color}}`}}), terrain);
}}
const graphLayer = el('g', {{id: 'graph'}});
svg.append(graphLayer);
for (const f of graph.features) {{
  const p = f.properties || {{}};
  const terrain = p.terrain || 'unknown';
  const edge = el('path', {{
    d: path(f),
    class: `edge ${{p.access === 'closed' ? 'closed' : ''}}`,
    stroke: terrainColors[terrain] || terrainColors.unknown,
    'stroke-width': Math.max(1, Math.min(5, 1 + Math.log10(1 + (p.difficulty || 0)))),
    opacity: Math.max(.18, Math.min(.9, p.confidence || .5))
  }});
  edge.addEventListener('click', () => show('edge', p));
  graphLayer.append(edge);
}}
const routeLayer = el('g', {{id: 'routes-layer'}});
svg.append(routeLayer);
const list = document.getElementById('routes');
routes.features.forEach((f, i) => {{
  const p = f.properties || {{}};
  const color = routeColors[i % routeColors.length];
  const halo = el('path', {{d: path(f), class: 'halo'}});
  routeLayer.append(halo);
  const r = el('path', {{d: path(f), class: 'route', stroke: color, 'stroke-width': 4}});
  r.addEventListener('click', () => show('route', p));
  routeLayer.append(r);
  const card = document.createElement('div');
  card.className = 'route-card';
  card.innerHTML = `<h3 style="color:${{color}}">${{escapeHtml(p.name || '')}}</h3><div class="${{p.satisfied ? 'ok' : 'bad'}}">${{p.satisfied ? 'satisfied' : 'violates constraints'}}</div><small>${{routeText(p)}}</small>`;
  card.addEventListener('click', () => show('route', p));
  routeHandles.push({{name: p.name, route: r, halo, card}});
  list.append(card);
}});
</script>
</body>
</html>
"#
    ))
}

fn js_json(value: &serde_json::Value) -> Result<String> {
    Ok(serde_json::to_string(value)?.replace("</", "<\\/"))
}

fn html_text(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_project_report(
    project: &Path,
    title: &str,
    graph: &TrailGraph,
    routes: &[Route],
    constraints: &LoopConstraints,
) -> Result<String> {
    let source_manifest = load_source_manifest(project)?;
    Ok(render_report(
        title,
        graph,
        routes,
        constraints,
        None,
        source_manifest.as_ref(),
    ))
}

fn render_report(
    title: &str,
    graph: &TrailGraph,
    routes: &[Route],
    constraints: &LoopConstraints,
    ledger: Option<&GenerationLedger>,
    source_manifest: Option<&SourceManifest>,
) -> String {
    let mut text = report::render_titled(title, graph, routes);
    render_generation_ledger_section(&mut text, ledger, graph);
    render_constraints_section(&mut text, constraints);
    render_source_manifest_section(&mut text, source_manifest);
    text
}

fn render_generation_ledger_section(
    text: &mut String,
    ledger: Option<&GenerationLedger>,
    graph: &TrailGraph,
) {
    let Some(ledger) = ledger else {
        return;
    };
    text.push_str("## Generation Ledger\n\n");
    let _ = writeln!(text, "- app version: {}", ledger.app_version);
    let _ = writeln!(
        text,
        "- solver: requested {:?}, concrete {}",
        ledger.requested_solver, ledger.solver
    );
    let _ = writeln!(text, "- random seed: {}", ledger.random_seed);
    let _ = writeln!(
        text,
        "- requested start: {:.6},{:.6}",
        ledger.requested_start.lon, ledger.requested_start.lat
    );
    let _ = writeln!(
        text,
        "- snapped start: vertex {}, {:.6},{:.6} ({:.0} m)",
        ledger.snapped_start_vertex.0,
        ledger.snapped_start_coord.lon,
        ledger.snapped_start_coord.lat,
        ledger.start_snap_m
    );
    render_generation_seed_ledger(text, ledger.seed_ledger.as_ref());
    render_generation_graph_ledger(text, graph);
    let _ = writeln!(text, "- emitted artifacts: {}", ledger.artifacts.len());
    if ledger.forbidden_areas.is_empty() {
        text.push_str("- forbidden areas: none\n");
    } else {
        text.push_str("- forbidden areas:\n");
        for area in &ledger.forbidden_areas {
            let _ = writeln!(
                text,
                "  - {} via {}, {} overlay(s), {} touched edge(s)",
                area.path, area.adapter_id, area.overlays, area.touched_edges
            );
        }
    }
    text.push('\n');
}

fn render_generation_seed_ledger(text: &mut String, seed_ledger: Option<&SeedLedgerManifest>) {
    match seed_ledger {
        Some(SeedLedgerManifest {
            present: true,
            routes,
            fingerprint: Some(fingerprint),
        }) => {
            let _ = writeln!(
                text,
                "- seed ledger: {routes} route(s), {} bytes sha256 {}",
                fingerprint.bytes, fingerprint.sha256
            );
        }
        Some(SeedLedgerManifest { present: false, .. }) => text.push_str("- seed ledger: none\n"),
        Some(SeedLedgerManifest {
            fingerprint: None, ..
        }) => {
            text.push_str("- seed ledger: present without fingerprint\n");
        }
        None => text.push_str("- seed ledger: missing from manifest\n"),
    }
}

fn render_generation_graph_ledger(text: &mut String, graph: &TrailGraph) {
    let elevation = graph_elevation_stats(graph);
    let edge_km = graph
        .edges
        .iter()
        .map(|edge| edge.attr.length_m)
        .sum::<f64>()
        / 1_000.0;
    let _ = writeln!(
        text,
        "- graph: {} vertices, {} edges, {edge_km:.2} km, {} directed-travel edge(s), {} turn ban(s)",
        graph.vertices.len(),
        graph.edges.len(),
        directed_travel_edge_count(graph),
        graph.turn_bans.len()
    );
    let _ = writeln!(
        text,
        "- graph elevation: {:.0} m ascent / {:.0} m descent, {:.2} sampled grade-km, {:.2} sustained-steep km",
        elevation.ascent_m,
        elevation.descent_m,
        elevation.sampled_grade_m / 1_000.0,
        elevation.sustained_steep_m / 1_000.0
    );
    if !graph.turn_bans.is_empty() {
        text.push_str("- turn-ban provenance:\n");
        for (source, count) in turn_ban_sources(graph) {
            let _ = writeln!(text, "  - {source}: {count}");
        }
    }
}

fn render_constraints_section(text: &mut String, constraints: &LoopConstraints) {
    text.push_str("## Constraint Envelope\n\n");
    let _ = writeln!(
        text,
        "- distance: {:.2}–{:.2} km",
        constraints.min_distance_m / 1_000.0,
        constraints.max_distance_m / 1_000.0
    );
    let _ = writeln!(
        text,
        "- scalar difficulty: {:.2}–{:.2}",
        constraints.min_difficulty, constraints.max_difficulty
    );
    let _ = writeln!(
        text,
        "- ascent: {:.0}–{:.0} m",
        constraints.min_ascent_m, constraints.max_ascent_m
    );
    let _ = writeln!(
        text,
        "- descent: {:.0}–{:.0} m",
        constraints.min_descent_m, constraints.max_descent_m
    );
    let _ = writeln!(
        text,
        "- max road/pavement exposure: {:.1}%",
        constraints.max_road_fraction * 100.0
    );
    let _ = writeln!(
        text,
        "- max low-confidence fraction: {:.1}%",
        constraints.max_low_confidence_fraction * 100.0
    );
    let _ = writeln!(
        text,
        "- max restricted-access fraction: {:.1}%",
        constraints.max_restricted_access_fraction * 100.0
    );
    let _ = writeln!(
        text,
        "- max repeated-edge fraction: {:.1}%",
        constraints.max_repeated_edge_fraction * 100.0
    );
    let _ = writeln!(text, "- allowed shapes: {:?}", constraints.allowed_shapes);
    if !constraints.forbidden_terrain.is_empty() {
        let _ = writeln!(
            text,
            "- forbidden terrain: {:?}",
            constraints.forbidden_terrain
        );
    }
    for (terrain, fraction) in &constraints.min_terrain_fraction {
        let _ = writeln!(text, "- min {terrain:?}: {:.1}%", fraction * 100.0);
    }
    for (terrain, fraction) in &constraints.max_terrain_fraction {
        let _ = writeln!(text, "- max {terrain:?}: {:.1}%", fraction * 100.0);
    }
    text.push('\n');
}

fn render_source_manifest_section(text: &mut String, manifest: Option<&SourceManifest>) {
    text.push_str("## Source Manifest\n\n");
    let Some(manifest) = manifest else {
        text.push_str("No source manifest found.\n");
        return;
    };
    render_source_coverage_summary(text, manifest);
    render_source_coverage(text, manifest);
    if manifest.candidates.is_empty() {
        text.push_str("No source candidates recorded.\n");
        return;
    }
    render_source_candidates(text, manifest);
}

fn render_source_coverage_summary(text: &mut String, manifest: &SourceManifest) {
    let summary = summarize_source_coverage(&manifest.coverage);
    let required = if summary.required_complete() {
        "complete"
    } else {
        "incomplete"
    };
    let recommended = if summary.recommended_complete() {
        "complete"
    } else {
        "incomplete"
    };
    let _ = writeln!(
        text,
        "Coverage summary: required {required} ({}/{} satisfied), recommended {recommended} ({}/{} satisfied), optional {}/{} satisfied.",
        summary.required.satisfied,
        summary.required.total,
        summary.recommended.satisfied,
        summary.recommended.total,
        summary.optional.satisfied,
        summary.optional.total
    );
    render_kind_list(text, "Missing required", &summary.missing_required);
    render_kind_list(text, "Missing recommended", &summary.missing_recommended);
    text.push('\n');
}

fn render_kind_list(text: &mut String, label: &str, kinds: &[SourceKind]) {
    if kinds.is_empty() {
        let _ = writeln!(text, "{label}: none");
        return;
    }
    let rendered = kinds
        .iter()
        .map(|kind| format!("{kind:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(text, "{label}: {rendered}");
}

fn render_discovery_report(manifest: &SourceManifest) -> String {
    let mut text = "# Source Discovery\n\n".to_owned();
    render_discovery_area(&mut text, manifest);
    text.push_str("## Coverage\n\n");
    render_source_coverage_summary(&mut text, manifest);
    render_source_coverage(&mut text, manifest);
    text.push_str("## Acquisition Plan\n\n");
    render_acquisition_plan(&mut text, manifest);
    text.push_str("## Cache Command Sketches\n\n");
    render_cache_command_sketches(&mut text, manifest);
    text.push_str("## Local Candidates\n\n");
    render_source_candidates(&mut text, manifest);
    text.push_str("\n## Adapter Registry\n\n");
    render_adapter_registry(&mut text, manifest);
    text
}

fn render_discovery_area(text: &mut String, manifest: &SourceManifest) {
    match manifest
        .recommendations
        .iter()
        .find_map(|recommendation| recommendation.area)
    {
        Some(area) => {
            let _ = writeln!(
                text,
                "AOI: west {:.6}, south {:.6}, east {:.6}, north {:.6}\n",
                area.west, area.south, area.east, area.north
            );
        }
        None => text.push_str("AOI: not set; run `trailgen init --bbox` or `discover --bbox` for region-bound recommendations.\n\n"),
    }
}

fn render_source_coverage(text: &mut String, manifest: &SourceManifest) {
    if manifest.coverage.is_empty() {
        text.push_str("No source coverage ledger recorded.\n\n");
        return;
    }
    text.push_str("Coverage:\n");
    for coverage in &manifest.coverage {
        let candidates = if coverage.candidate_paths.is_empty() {
            "none".to_owned()
        } else {
            coverage.candidate_paths.join(", ")
        };
        let _ = writeln!(
            text,
            "- {:?} ({:?}): {:?}; candidates: {}; {}",
            coverage.kind, coverage.priority, coverage.status, candidates, coverage.message
        );
    }
    text.push('\n');
}

fn render_source_candidates(text: &mut String, manifest: &SourceManifest) {
    if manifest.candidates.is_empty() {
        text.push_str("No source candidates recorded.\n");
        return;
    }
    text.push_str("Candidates:\n");
    for candidate in &manifest.candidates {
        let fingerprint = candidate.fingerprint.as_ref().map_or_else(
            || "unfingerprinted".to_owned(),
            |fingerprint| format!("{} bytes, sha256 {}", fingerprint.bytes, fingerprint.sha256),
        );
        let origin = candidate
            .origin
            .as_ref()
            .map_or_else(String::new, |origin| format!("; origin {origin}"));
        let _ = writeln!(
            text,
            "- {}: {:?} via {}; {fingerprint}{origin}",
            candidate.path, candidate.kind, candidate.adapter_id
        );
    }
}

fn render_source_plan(
    project: &Path,
    manifest: &SourceManifest,
    kinds: &[SourceKind],
    include_satisfied: bool,
) -> String {
    let mut text = "# Source Acquisition Plan\n\n".to_owned();
    let mut emitted = 0usize;
    let project = shell_arg(&project.display().to_string());
    let kind_filter = kinds.iter().copied().collect::<BTreeSet<_>>();
    for recommendation in &manifest.recommendations {
        if !kind_filter.is_empty() && !kind_filter.contains(&recommendation.kind) {
            continue;
        }
        let coverage = manifest
            .coverage
            .iter()
            .find(|coverage| coverage.kind == recommendation.kind);
        let status = coverage.map_or(SourceCoverageStatus::Missing, |coverage| coverage.status);
        if !include_satisfied && status == SourceCoverageStatus::Satisfied {
            continue;
        }
        emitted += 1;
        render_source_plan_item(&mut text, &project, recommendation, status);
    }
    if emitted == 0 {
        text.push_str("No source acquisition actions match the requested filters.\n");
    }
    text
}

fn render_source_plan_item(
    text: &mut String,
    project: &str,
    recommendation: &trailgen_core::source::SourceRecommendation,
    status: SourceCoverageStatus,
) {
    let _ = writeln!(
        text,
        "## {:?} ({:?}, {:?})\n",
        recommendation.kind, recommendation.priority, status
    );
    let adapter_id = recommendation
        .adapter_ids
        .first()
        .map_or("<adapter-id>", String::as_str);
    let output = recommendation
        .suggested_paths
        .first()
        .map_or("source.bin", |path| {
            path.strip_prefix("sources/").unwrap_or(path)
        });
    if let Some(profile) = osm_profile_for_kind(recommendation.kind) {
        let bbox = recommendation.area.map_or_else(
            || " --bbox west,south,east,north".to_owned(),
            |_| String::new(),
        );
        let _ = writeln!(
            text,
            "Direct OSM fallback:\n```sh\ntrailgen acquire-osm {project} --profile {}{} --output {}\n```\n",
            profile.label(),
            bbox,
            profile.default_output()
        );
    }
    let _ = writeln!(
        text,
        "Cache selected artifact:\n```sh\ntrailgen cache-source {project} --input '<artifact-url-or-path>' --output {} --kind {} --adapter {}\n```\n",
        output,
        source_kind_arg(recommendation.kind),
        adapter_id
    );
    text.push_str("Acceptance:\n");
    let _ = writeln!(text, "- {}\n", recommendation.acceptance);
    if !recommendation.acquisition_hints.is_empty() {
        text.push_str("Source surfaces:\n");
        for hint in &recommendation.acquisition_hints {
            let _ = writeln!(
                text,
                "- {}: {} [{}]. {}",
                hint.label,
                hint.url,
                hint.formats.join(", "),
                hint.note
            );
        }
        text.push('\n');
    }
}

fn render_acquisition_plan(text: &mut String, manifest: &SourceManifest) {
    for recommendation in &manifest.recommendations {
        let coverage = manifest
            .coverage
            .iter()
            .find(|coverage| coverage.kind == recommendation.kind);
        let status = coverage.map_or_else(
            || "uncovered".to_owned(),
            |coverage| format!("{:?}", coverage.status),
        );
        let _ = writeln!(
            text,
            "### {:?} ({:?}, {status})\n",
            recommendation.kind, recommendation.priority
        );
        let _ = writeln!(text, "{}\n", recommendation.rationale);
        let _ = writeln!(text, "Acceptance: {}\n", recommendation.acceptance);
        write_string_list(
            text,
            "Suggested cache paths",
            &recommendation.suggested_paths,
        );
        write_string_list(text, "Adapter ids", &recommendation.adapter_ids);
        write_string_list(text, "Search terms", &recommendation.search_terms);
        if recommendation.acquisition_hints.is_empty() {
            text.push_str("Acquisition hints:\n- none\n\n");
        } else {
            text.push_str("Acquisition hints:\n");
            for hint in &recommendation.acquisition_hints {
                let _ = writeln!(
                    text,
                    "- {}: {} [{}]. {}",
                    hint.label,
                    hint.url,
                    hint.formats.join(", "),
                    hint.note
                );
            }
            text.push('\n');
        }
    }
}

fn render_cache_command_sketches(text: &mut String, manifest: &SourceManifest) {
    text.push_str("Run `trailgen source-plan <project>` for a filtered next-action view of this manifest. Replace `<artifact-url-or-path>` with a concrete downloaded artifact or local file selected from the listed source surface; keep the explicit kind and adapter when provider filenames are ambiguous. For OSM-backed trail, road, and hydrology layers, `acquire-osm` can materialize bbox-scoped XML directly from an Overpass endpoint.\n\n");
    if manifest
        .recommendations
        .iter()
        .any(|recommendation| osm_profile_for_kind(recommendation.kind).is_some())
    {
        let bbox = manifest
            .recommendations
            .iter()
            .find_map(|recommendation| recommendation.area)
            .map_or_else(
                || " --bbox west,south,east,north".to_owned(),
                |_| String::new(),
            );
        let _ = writeln!(
            text,
            "Combined OSM/Overpass fallback:\n\n```sh\ntrailgen acquire-osm <project> --profile all{bbox} --output {}\n```\n",
            OsmAcquireProfile::All.default_output()
        );
    }
    for recommendation in &manifest.recommendations {
        let adapter_id = recommendation
            .adapter_ids
            .first()
            .map_or("<adapter-id>", String::as_str);
        let output = recommendation
            .suggested_paths
            .first()
            .map_or("source.bin", |path| {
                path.strip_prefix("sources/").unwrap_or(path)
            });
        let _ = writeln!(
            text,
            "### {:?}\n\n```sh\ntrailgen cache-source <project> --input '<artifact-url-or-path>' --output {} --kind {} --adapter {}\n```\n",
            recommendation.kind,
            output,
            source_kind_arg(recommendation.kind),
            adapter_id
        );
        if let Some(profile) = osm_profile_for_kind(recommendation.kind) {
            let bbox = recommendation.area.map_or_else(
                || " --bbox west,south,east,north".to_owned(),
                |_| String::new(),
            );
            let _ = writeln!(
                text,
                "OSM/Overpass direct acquisition:\n\n```sh\ntrailgen acquire-osm <project> --profile {}{} --output {}\n```\n",
                profile.label(),
                bbox,
                profile.default_output()
            );
        }
        if let Some(hint) = recommendation.acquisition_hints.first() {
            let _ = writeln!(
                text,
                "Primary source surface: {} ({})",
                hint.label, hint.url
            );
            if recommendation.acquisition_hints.len() > 1 {
                let alternates = recommendation
                    .acquisition_hints
                    .iter()
                    .skip(1)
                    .map(|hint| hint.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(text, "Alternate surfaces: {alternates}");
            }
            text.push('\n');
        }
    }
}

const fn osm_profile_for_kind(kind: SourceKind) -> Option<OsmAcquireProfile> {
    match kind {
        SourceKind::TrailNetwork => Some(OsmAcquireProfile::Trails),
        SourceKind::Road => Some(OsmAcquireProfile::Roads),
        SourceKind::Hydrology => Some(OsmAcquireProfile::Hydrology),
        SourceKind::SeedRoute
        | SourceKind::Elevation
        | SourceKind::Terrain
        | SourceKind::Access
        | SourceKind::Closure => None,
    }
}

fn write_string_list(text: &mut String, title: &str, xs: &[String]) {
    let _ = writeln!(text, "{title}:");
    if xs.is_empty() {
        text.push_str("- none\n\n");
        return;
    }
    for x in xs {
        let _ = writeln!(text, "- {x}");
    }
    text.push('\n');
}

fn render_adapter_registry(text: &mut String, manifest: &SourceManifest) {
    for adapter in &manifest.adapters {
        let _ = writeln!(
            text,
            "- {} ({:?}): consumes {}; produces {}; {}",
            adapter.id,
            adapter.kind,
            adapter.consumes.join(", "),
            adapter.produces.join(", "),
            adapter.note
        );
    }
}

fn import_seed(
    project: &Path,
    route: &Path,
    name: Option<String>,
    max_route_snap_m: Option<f64>,
) -> Result<()> {
    let config = load_config(project)?;
    let mut graph = load_graph(project)?;
    let mut seeds = load_seeds(project)?;
    let archived_seed_name = name
        .is_none()
        .then(|| seed_name_for_source(&seeds, route))
        .flatten();
    let route_file = load_route_file(route)?;
    let name = name.or(archived_seed_name).unwrap_or_else(|| {
        route_file
            .metadata
            .title_or(
                route
                    .file_stem()
                    .and_then(|x| x.to_str())
                    .unwrap_or("seed-route"),
            )
            .to_owned()
    });
    let previous_seed = seeds.iter().find(|old| old.name == name).cloned();
    let previous_source_path = previous_seed.as_ref().map(|old| old.source_path.clone());
    let previous_original_source_path = previous_seed
        .as_ref()
        .and_then(|old| old.original_source_path.clone());
    let reimporting_archive = previous_seed
        .as_ref()
        .is_some_and(|old| old.source_path == route.display().to_string());
    let original_source_path = if reimporting_archive {
        previous_original_source_path.unwrap_or_else(|| route.display().to_string())
    } else {
        route.display().to_string()
    };
    let source_format = route
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("unknown")
        .to_ascii_lowercase();
    let mut seed = SeedRoute::snap_with_limit(
        &graph,
        name,
        route.display().to_string(),
        source_format,
        &route_file.line,
        max_route_snap_m.unwrap_or(config.max_route_snap_m),
    );
    seed.metadata = route_file.metadata;
    seed.original_source_path = Some(original_source_path);
    ensure_route_snap_accepted(
        &seed.snap,
        max_route_snap_m.unwrap_or(config.max_route_snap_m),
        "seed route",
    )?;
    let archived_route = archive_seed_route(project, route, &seed.name)?;
    seed.source_path = archived_route.display().to_string();
    graph.apply_seed_hints(&seed);
    for edge in &mut graph.edges {
        config.difficulty.apply_edge(edge);
    }
    save_graph(project, &graph)?;

    seeds.retain(|old| old.name != seed.name);
    seeds.push(seed.clone());
    seeds.sort_by(|a, b| a.name.cmp(&b.name));
    fs::create_dir_all(project.join("seeds"))?;
    write_json(project.join("seeds/seeds.json"), &seeds)?;
    write_json(
        project.join(format!("seeds/{}.json", artifact_key(&seed.name))),
        &seed,
    )?;
    register_source_candidate(project, &archived_route)?;
    if let Some(previous_source_path) =
        previous_source_path.filter(|path| path != &seed.source_path)
    {
        unregister_source_candidate_path(project, &previous_source_path)?;
    }
    println!(
        "imported seed {}: {} point(s), {} snapped edge(s), max snap {:.0} m, closed_loop={}",
        seed.name,
        seed.point_count,
        seed.snapped_edges.len(),
        seed.snap.max_snap_m,
        seed.closed_loop
    );
    Ok(())
}

fn seed_name_for_source(seeds: &[SeedRoute], route: &Path) -> Option<String> {
    seeds
        .iter()
        .find(|seed| same_seed_source(route, &seed.source_path))
        .map(|seed| seed.name.clone())
}

fn same_seed_source(route: &Path, seed_source_path: &str) -> bool {
    let seed_source = PathBuf::from(seed_source_path);
    route
        .canonicalize()
        .ok()
        .zip(seed_source.canonicalize().ok())
        .is_some_and(|(route, seed_source)| route == seed_source)
        || route == seed_source
        || route.display().to_string() == seed_source_path
}

fn archive_seed_route(project: &Path, route: &Path, name: &str) -> Result<PathBuf> {
    let ext = route
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("route")
        .to_ascii_lowercase();
    let archived_route = project.join(format!("seeds/imports/{}.{}", artifact_key(name), ext));
    let same_file = route
        .canonicalize()
        .ok()
        .zip(archived_route.canonicalize().ok())
        .is_some_and(|(route, archived_route)| route == archived_route);
    if !same_file {
        if let Some(parent) = archived_route.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(route, &archived_route).with_context(|| {
            format!(
                "archive seed route {} to {}",
                route.display(),
                archived_route.display()
            )
        })?;
    }
    Ok(archived_route)
}

struct AccessOverlayBundle<'a> {
    source: &'a Path,
    overlays: Vec<AccessOverlay>,
}

fn apply_access(
    project: &Path,
    sources: &[PathBuf],
    date: Option<PlanningDate>,
    time: Option<PlanningTime>,
) -> Result<()> {
    if sources.is_empty() {
        bail!("apply-access requires at least one --source");
    }
    let mut config = load_config(project)?;
    let temporal_override = date.is_some() || time.is_some();
    if let Some(date) = date {
        config.planning_date = Some(date);
    }
    if let Some(time) = time {
        config.planning_time = Some(time);
    }
    let mut graph = load_graph(project)?;
    let bundles = sources
        .iter()
        .map(|source| {
            Ok(AccessOverlayBundle {
                source,
                overlays: access_overlays(source)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let overlays = bundles
        .iter()
        .flat_map(|bundle| bundle.overlays.iter().cloned())
        .collect::<Vec<_>>();
    restore_or_capture_access_baseline(project, &mut graph, &overlays, config.difficulty)?;
    let touched = apply_access_overlays_at(
        &mut graph,
        &overlays,
        config.planning_moment(),
        config.difficulty,
    );
    save_graph(project, &graph)?;
    fs::create_dir_all(project.join("sources"))?;
    write_json(project.join("sources/access-overlays.json"), &overlays)?;
    if temporal_override {
        save_config(project, &config)?;
    }
    register_access_sources(project, &bundles)?;
    println!(
        "applied {} access overlay source(s), {} overlay(s); touched {} edge(s)",
        bundles.len(),
        overlays.len(),
        touched
    );
    Ok(())
}

fn access_overlays(source: &Path) -> Result<Vec<trailgen_core::AccessOverlay>> {
    if source_ext(source).as_deref() == Some("shp") {
        shp::access_overlays_from_path(source).map_err(Into::into)
    } else {
        let raw =
            fs::read_to_string(source).with_context(|| format!("read {}", source.display()))?;
        Ok(geojson::access_overlays_from_str(&raw)
            .with_context(|| "parse access overlay GeoJSON")?)
    }
}

fn restore_or_capture_access_baseline(
    project: &Path,
    graph: &mut TrailGraph,
    overlays: &[AccessOverlay],
    weights: DifficultyWeights,
) -> Result<()> {
    if let Some(baseline) = load_access_baseline(project)? {
        baseline.restore(graph, weights)?;
        return Ok(());
    }
    if graph_contains_access_overlays(graph, overlays) {
        bail!(
            "cached graph already contains access-overlay provenance but sources/access-baseline.json is missing; rebuild the graph, reapply enrichment/context, then rerun apply-access"
        );
    }
    write_json(
        access_baseline_path(project),
        &AccessBaseline::capture(graph),
    )
}

fn materialize_effective_graph(project: &Path, config: &ProjectConfig) -> Result<TrailGraph> {
    let mut graph = load_graph(project)?;
    let overlays = load_stored_access_overlays(project)?;
    if overlays.is_empty() {
        return Ok(graph);
    }
    if let Some(baseline) = load_access_baseline(project)? {
        baseline.restore(&mut graph, config.difficulty)?;
    } else if graph_contains_access_overlays(&graph, &overlays) {
        bail!(
            "cached graph contains access-overlay provenance but sources/access-baseline.json is missing; rebuild graph and rerun apply-access before date-specific generation"
        );
    }
    apply_access_overlays_at(
        &mut graph,
        &overlays,
        config.planning_moment(),
        config.difficulty,
    );
    Ok(graph)
}

fn apply_forbidden_area_sources(
    graph: &mut TrailGraph,
    sources: &[PathBuf],
    weights: DifficultyWeights,
) -> Result<Vec<ForbiddenAreaManifest>> {
    sources
        .iter()
        .map(|source| apply_forbidden_area_source(graph, source, weights))
        .collect()
}

fn apply_forbidden_area_source(
    graph: &mut TrailGraph,
    source: &Path,
    weights: DifficultyWeights,
) -> Result<ForbiddenAreaManifest> {
    let fingerprint = source_fingerprint(source)?;
    let mut overlays = access_overlays(source)
        .with_context(|| format!("load forbidden area overlay {}", source.display()))?;
    if overlays.is_empty() {
        bail!(
            "forbidden area source {} contains no overlays",
            source.display()
        );
    }
    force_forbidden_area_overlays(source, &mut overlays);
    let touched_edges = apply_access_overlays(graph, &overlays, None, weights);
    if touched_edges == 0 {
        bail!(
            "forbidden area source {} touched no graph edges; check CRS, AOI, and overlay geometry",
            source.display()
        );
    }
    Ok(ForbiddenAreaManifest {
        path: source.display().to_string(),
        adapter_id: forbidden_area_adapter_id(source),
        fingerprint,
        overlays: overlays.len(),
        touched_edges,
    })
}

fn force_forbidden_area_overlays(source: &Path, overlays: &mut [AccessOverlay]) {
    for overlay in overlays {
        let name = overlay.name.clone();
        overlay.access = Access::Closed;
        overlay.travel = None;
        overlay.active = AccessWindow::default();
        overlay.confidence = overlay.confidence.max(0.95);
        overlay.provenance = Provenance {
            source: "forbidden-area".to_owned(),
            layer: Some("generate-forbid-area".to_owned()),
            source_id: Some(format!("{}#{name}", source.display())),
            license: None,
        };
    }
}

fn forbidden_area_adapter_id(source: &Path) -> String {
    classify_path(source).map_or_else(
        || {
            if source_ext(source).as_deref() == Some("shp") {
                "shapefile-access-overlay".to_owned()
            } else {
                "geojson-access-overlay".to_owned()
            }
        },
        |candidate| match candidate.kind {
            SourceKind::Access | SourceKind::Closure => candidate.adapter_id,
            _ => {
                if source_ext(source).as_deref() == Some("shp") {
                    "shapefile-access-overlay".to_owned()
                } else {
                    "geojson-access-overlay".to_owned()
                }
            }
        },
    )
}

fn graph_contains_access_overlays(graph: &TrailGraph, overlays: &[AccessOverlay]) -> bool {
    graph.edges.iter().any(|edge| {
        overlays.iter().any(|overlay| {
            edge.attr
                .access_provenance
                .iter()
                .any(|provenance| provenance == &overlay.provenance)
        })
    })
}

fn apply_terrain(project: &Path, source: &Path) -> Result<()> {
    let config = load_config(project)?;
    let mut graph = load_graph(project)?;
    let overlays = terrain_overlays(source)?;
    let touched = apply_terrain_overlays(&mut graph, &overlays, config.difficulty);
    save_graph(project, &graph)?;
    fs::create_dir_all(project.join("sources"))?;
    write_json(project.join("sources/terrain-overlays.json"), &overlays)?;
    let adapter_id = if source_ext(source).as_deref() == Some("shp") {
        "shapefile-terrain-overlay"
    } else {
        "geojson-terrain-overlay"
    };
    register_source_candidate_as(project, source, SourceKind::Terrain, adapter_id)?;
    println!(
        "applied {} terrain overlay(s); touched {} edge(s)",
        overlays.len(),
        touched
    );
    Ok(())
}

fn terrain_overlays(source: &Path) -> Result<Vec<trailgen_core::TerrainOverlay>> {
    if source_ext(source).as_deref() == Some("shp") {
        shp::terrain_overlays_from_path(source).map_err(Into::into)
    } else {
        let raw =
            fs::read_to_string(source).with_context(|| format!("read {}", source.display()))?;
        Ok(geojson::terrain_overlays_from_str(&raw)
            .with_context(|| "parse terrain overlay GeoJSON")?)
    }
}

fn apply_elevation(project: &Path, source: &Path, confidence: f64) -> Result<()> {
    let config = load_config(project)?;
    let mut graph = load_graph(project)?;
    let applied = apply_elevation_source(&mut graph, source, confidence, &config)?;
    save_graph(project, &graph)?;
    fs::create_dir_all(project.join("sources"))?;
    applied.write_metadata(project)?;
    register_source_candidate_as(project, source, SourceKind::Elevation, applied.adapter_id)?;
    println!("{}", applied.message);
    Ok(())
}

struct AppliedElevation {
    adapter_id: &'static str,
    metadata_path: &'static str,
    metadata: ElevationDescriptor,
    message: String,
}

#[derive(Serialize)]
struct ElevationDescriptor {
    adapter_id: &'static str,
    source_path: String,
    width: usize,
    height: usize,
    confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_filename: Option<String>,
}

impl AppliedElevation {
    fn write_metadata(&self, project: &Path) -> Result<()> {
        write_json(project.join(self.metadata_path), &self.metadata)
    }
}

fn apply_elevation_source(
    graph: &mut TrailGraph,
    source: &Path,
    confidence: f64,
    config: &ProjectConfig,
) -> Result<AppliedElevation> {
    match source_ext(source).as_deref() {
        Some("asc") => apply_arc_ascii_elevation(graph, source, confidence, config),
        Some("tif" | "tiff") => apply_geotiff_elevation(graph, source, confidence, config),
        Some("vrt") => apply_vrt_elevation(graph, source, confidence, config),
        Some(ext) => {
            bail!("unsupported elevation extension {ext:?}; expected asc, tif, tiff, or vrt")
        }
        None => bail!("elevation source has no file extension"),
    }
}

struct LoadedElevation {
    dem: RasterDem,
    adapter_id: &'static str,
}

fn load_elevation_dem(source: &Path, confidence: f64) -> Result<LoadedElevation> {
    match source_ext(source).as_deref() {
        Some("asc") => Ok(LoadedElevation {
            dem: RasterDem::ArcAscii(load_arc_ascii_grid(source, confidence)?),
            adapter_id: "arc-ascii-elevation",
        }),
        Some("tif" | "tiff") => Ok(LoadedElevation {
            dem: RasterDem::GeoTiff(load_geotiff_dem(source, confidence)?),
            adapter_id: "geotiff-elevation",
        }),
        Some("vrt") => Ok(LoadedElevation {
            dem: RasterDem::Vrt(load_vrt_dem(source, confidence)?),
            adapter_id: "vrt-elevation",
        }),
        Some(ext) => {
            bail!("unsupported elevation extension {ext:?}; expected asc, tif, tiff, or vrt")
        }
        None => bail!("elevation source has no file extension"),
    }
}

fn apply_elevation_mosaic(project: &Path, sources: &[PathBuf], confidence: f64) -> Result<()> {
    let config = load_config(project)?;
    let mut graph = load_graph(project)?;
    let mut rasters = Vec::new();
    let mut candidates = Vec::new();
    for source in sources {
        let loaded = load_elevation_dem(source, confidence)
            .with_context(|| format!("load elevation source {}", source.display()))?;
        candidates.push(source_candidate(
            source,
            SourceKind::Elevation,
            loaded.adapter_id,
            source_fingerprint(source)?,
        ));
        rasters.push(loaded.dem);
    }
    let mosaic = ElevationMosaic::new(rasters)?;
    enrich_graph(&mut graph, &mosaic, config.enrichment, config.difficulty)
        .with_context(|| "apply elevation mosaic")?;
    save_graph(project, &graph)?;
    write_json(
        project.join("sources/elevation-mosaic.json"),
        &serde_json::json!({
            "kind": "elevation-mosaic",
            "sources": sources,
            "confidence": confidence.clamp(0.0, 1.0),
        }),
    )?;
    register_source_candidates(project, candidates)?;
    println!("applied elevation mosaic from {} source(s)", sources.len());
    Ok(())
}

fn apply_arc_ascii_elevation(
    graph: &mut TrailGraph,
    source: &Path,
    confidence: f64,
    config: &ProjectConfig,
) -> Result<AppliedElevation> {
    let raster = load_arc_ascii_grid(source, confidence)?;
    enrich_graph(graph, &raster, config.enrichment, config.difficulty)
        .with_context(|| "apply Arc/Info ASCII elevation raster")?;
    let (ncols, nrows) = (raster.ncols, raster.nrows);
    Ok(AppliedElevation {
        adapter_id: "arc-ascii-elevation",
        metadata_path: "sources/elevation-arc-ascii.json",
        metadata: ElevationDescriptor {
            adapter_id: "arc-ascii-elevation",
            source_path: source.display().to_string(),
            width: ncols,
            height: nrows,
            confidence: raster.confidence,
            source_filename: None,
        },
        message: format!(
            "applied Arc/Info ASCII elevation grid {}x{} from {}",
            ncols,
            nrows,
            source.display()
        ),
    })
}

fn apply_geotiff_elevation(
    graph: &mut TrailGraph,
    source: &Path,
    confidence: f64,
    config: &ProjectConfig,
) -> Result<AppliedElevation> {
    let raster = load_geotiff_dem(source, confidence)?;
    enrich_graph(graph, &raster, config.enrichment, config.difficulty)
        .with_context(|| "apply GeoTIFF elevation raster")?;
    let (width, height) = (raster.width, raster.height);
    Ok(AppliedElevation {
        adapter_id: "geotiff-elevation",
        metadata_path: "sources/elevation-geotiff.json",
        metadata: ElevationDescriptor {
            adapter_id: "geotiff-elevation",
            source_path: source.display().to_string(),
            width,
            height,
            confidence: raster.confidence,
            source_filename: None,
        },
        message: format!(
            "applied GeoTIFF elevation grid {}x{} from {}",
            width,
            height,
            source.display()
        ),
    })
}

fn apply_vrt_elevation(
    graph: &mut TrailGraph,
    source: &Path,
    confidence: f64,
    config: &ProjectConfig,
) -> Result<AppliedElevation> {
    let raster = load_vrt_dem(source, confidence)?;
    enrich_graph(graph, &raster, config.enrichment, config.difficulty)
        .with_context(|| "apply VRT elevation raster")?;
    let (width, height) = (raster.width, raster.height);
    Ok(AppliedElevation {
        adapter_id: "vrt-elevation",
        metadata_path: "sources/elevation-vrt.json",
        metadata: ElevationDescriptor {
            adapter_id: "vrt-elevation",
            source_path: source.display().to_string(),
            width,
            height,
            confidence: raster.confidence,
            source_filename: Some(raster.source_filename.clone()),
        },
        message: format!(
            "applied VRT elevation grid {}x{} from {}",
            width,
            height,
            source.display()
        ),
    })
}

fn load_arc_ascii_grid(source: &Path, confidence: f64) -> Result<ArcAsciiGrid> {
    let raw = fs::read_to_string(source).with_context(|| format!("read {}", source.display()))?;
    ArcAsciiGrid::parse(
        &raw,
        Provenance {
            source: "arc-ascii-grid".to_owned(),
            layer: Some("arc-ascii".to_owned()),
            source_id: source
                .file_name()
                .and_then(|x| x.to_str())
                .map(str::to_owned),
            license: None,
        },
        confidence,
    )
    .with_context(|| "parse Arc/Info ASCII elevation raster")
}

fn load_geotiff_dem(source: &Path, confidence: f64) -> Result<GeoTiffDem> {
    GeoTiffDem::from_path(
        source,
        Provenance {
            source: "geotiff-dem".to_owned(),
            layer: Some("geotiff".to_owned()),
            source_id: source
                .file_name()
                .and_then(|x| x.to_str())
                .map(str::to_owned),
            license: None,
        },
        confidence,
    )
    .with_context(|| "parse GeoTIFF elevation raster")
}

fn load_vrt_dem(source: &Path, confidence: f64) -> Result<VrtDem> {
    VrtDem::from_path(
        source,
        Provenance {
            source: "vrt-dem".to_owned(),
            layer: Some("vrt".to_owned()),
            source_id: source
                .file_name()
                .and_then(|x| x.to_str())
                .map(str::to_owned),
            license: None,
        },
        confidence,
    )
    .with_context(|| "parse VRT elevation raster")
}

fn apply_context(project: &Path, source: &Path) -> Result<()> {
    let config = load_config(project)?;
    let mut graph = load_graph(project)?;
    let overlays = context_overlays(source)?;
    let crossings = apply_context_overlays(&mut graph, &overlays, config.difficulty);
    save_graph(project, &graph)?;
    fs::create_dir_all(project.join("sources"))?;
    write_json(project.join("sources/context-overlays.json"), &overlays)?;
    register_context_source(project, source, &overlays)?;
    println!(
        "applied {} context overlay(s); inferred {} crossing(s)",
        overlays.len(),
        crossings
    );
    Ok(())
}

fn context_overlays(source: &Path) -> Result<Vec<trailgen_core::ContextOverlay>> {
    match source_ext(source).as_deref() {
        Some("shp") => shp::context_overlays_from_path(source).map_err(Into::into),
        Some("osm") => {
            let raw =
                fs::read_to_string(source).with_context(|| format!("read {}", source.display()))?;
            Ok(osm::context_overlays_from_str(&raw).with_context(|| "parse context OSM XML")?)
        }
        Some("osm.pbf") => Ok(osm::context_overlays_from_pbf_reader(
            fs::File::open(source).with_context(|| format!("read {}", source.display()))?,
        )
        .with_context(|| "parse context OSM PBF")?),
        _ => {
            let raw =
                fs::read_to_string(source).with_context(|| format!("read {}", source.display()))?;
            Ok(
                geojson::context_overlays_from_str(&raw)
                    .with_context(|| "parse context GeoJSON")?,
            )
        }
    }
}

fn load_config(project: &Path) -> Result<ProjectConfig> {
    let raw = fs::read_to_string(project.join("trailgen.toml"))
        .with_context(|| format!("read {}", project.join("trailgen.toml").display()))?;
    let config = toml::from_str::<ProjectConfig>(&raw)?;
    config.validate()?;
    Ok(config)
}

fn save_config(project: &Path, config: &ProjectConfig) -> Result<()> {
    config.validate()?;
    fs::write(
        project.join("trailgen.toml"),
        toml::to_string_pretty(config)?,
    )
    .with_context(|| format!("write {}", project.join("trailgen.toml").display()))
}

fn load_graph(project: &Path) -> Result<TrailGraph> {
    let raw = fs::read_to_string(project.join("cache/graph.json"))
        .with_context(|| "read cache/graph.json; run `trailgen build` first")?;
    load_graph_json(&raw)
}

fn load_generated_graph(project: &Path) -> Result<TrailGraph> {
    let path = project.join("routes/generated.graph.json");
    if path.exists() {
        let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        return load_graph_json(&raw);
    }
    let config = if let Some(config) = load_generated_config(project)? {
        config
    } else {
        load_config(project)?
    };
    materialize_effective_graph(project, &config)
}

fn load_graph_json(raw: &str) -> Result<TrailGraph> {
    Ok(serde_json::from_str(raw)?)
}

fn save_graph(project: &Path, graph: &TrailGraph) -> Result<()> {
    write_json(project.join("cache/graph.json"), graph)?;
    write_json(
        project.join("cache/graph.geojson"),
        &geojson::graph_to_geojson(graph),
    )?;
    write_bytes(project.join("cache/edges.csv"), graph_edges_csv(graph))?;
    write_bytes(
        project.join("cache/vertices.csv"),
        graph_vertices_csv(graph),
    )
}

fn graph_vertices_csv(graph: &TrailGraph) -> String {
    let mut out = String::from("vertex_id,lon,lat,elevation_m,wkt\n");
    for vertex in &graph.vertices {
        let Coord { lon, lat, ele } = vertex.coord;
        writeln!(
            out,
            "{},{lon:.7},{lat:.7},{},{}",
            vertex.id.0,
            csv_f64(ele),
            csv_cell(&point_wkt(vertex.coord))
        )
        .expect("write to string");
    }
    out
}

fn graph_edges_csv(graph: &TrailGraph) -> String {
    let mut out = String::from(
        "edge_id,from_vertex,to_vertex,travel,length_m,ascent_m,descent_m,grade_abs_mean,grade_abs_max,sustained_steep_m,terrain,surface,terrain_confidence,terrain_evidence,access,access_confidence,access_provenance,road_exposure,confidence,difficulty,seed_count,seed_provenance,elevation_provenance,road_crossings,water_crossings,provenance,wkt\n",
    );
    for edge in &graph.edges {
        let crossings = edge_crossing_counts(edge);
        writeln!(
            out,
            "{},{},{},{},{:.3},{:.3},{:.3},{:.6},{:.6},{:.3},{},{},{:.6},{},{},{:.6},{},{:.6},{:.6},{:.6},{},{},{},{},{},{},{}",
            edge.id.0,
            edge.a.0,
            edge.b.0,
            edge_travel_tag(edge.attr.travel),
            edge.attr.length_m,
            edge.attr.ascent_m,
            edge.attr.descent_m,
            edge.attr.grade_abs_mean,
            edge.attr.grade_abs_max,
            edge.attr.sustained_steep_m,
            terrain_tag(edge.attr.terrain),
            csv_cell(edge.attr.surface.as_deref().unwrap_or("")),
            edge.attr.terrain_confidence,
            csv_cell(&terrain_evidence_summary(&edge.attr.terrain_evidence)),
            access_tag(edge.attr.access),
            edge.attr.access_confidence,
            csv_cell(&provenance_summary(&edge.attr.access_provenance)),
            edge.attr.road_exposure,
            edge.attr.confidence,
            edge.attr.difficulty,
            edge.attr.seed_count,
            csv_cell(&provenance_summary(&edge.attr.seed_provenance)),
            csv_cell(&provenance_summary(&edge.attr.elevation_provenance)),
            crossings.road,
            crossings.water,
            csv_cell(&provenance_summary(&edge.attr.provenance)),
            csv_cell(&line_wkt(&edge.geometry))
        )
        .expect("write to string");
    }
    out
}

fn terrain_evidence_summary(evidence: &[TerrainEvidence]) -> String {
    evidence
        .iter()
        .map(|e| {
            let mut s = format!(
                "{}:{:.0}%:{}",
                terrain_tag(e.terrain),
                e.confidence * 100.0,
                e.rationale
            );
            if let Some(provenance) = &e.provenance {
                write!(s, ":{}", provenance_csv_label(provenance)).expect("write to string");
            }
            s
        })
        .collect::<Vec<_>>()
        .join("|")
}

#[derive(Clone, Copy, Debug, Default)]
struct CrossingCounts {
    road: u32,
    water: u32,
}

fn edge_crossing_counts(edge: &trailgen_core::Edge) -> CrossingCounts {
    edge.attr
        .crossings
        .iter()
        .fold(CrossingCounts::default(), |mut counts, crossing| {
            match crossing.kind {
                CrossingKind::Road => counts.road += crossing.count,
                CrossingKind::Water => counts.water += crossing.count,
            }
            counts
        })
}

fn provenance_summary(provenance: &[Provenance]) -> String {
    provenance
        .iter()
        .map(provenance_csv_label)
        .collect::<Vec<_>>()
        .join("|")
}

fn provenance_csv_label(p: &Provenance) -> String {
    let mut s = p.source.clone();
    if let Some(layer) = &p.layer {
        write!(s, ":{layer}").expect("write to string");
    }
    if let Some(source_id) = &p.source_id {
        write!(s, ":{source_id}").expect("write to string");
    }
    s
}

fn line_wkt(line: &LineString) -> String {
    format!(
        "LINESTRING Z ({})",
        line.points
            .iter()
            .map(coord_wkt_tuple)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn point_wkt(coord: Coord) -> String {
    format!("POINT Z ({})", coord_wkt_tuple(&coord))
}

fn coord_wkt_tuple(coord: &Coord) -> String {
    format!(
        "{:.7} {:.7} {:.3}",
        coord.lon,
        coord.lat,
        coord.ele.unwrap_or(0.0)
    )
}

fn csv_cell(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn csv_f64(value: Option<f64>) -> String {
    value.map_or_else(String::new, |x| format!("{x:.3}"))
}

const fn terrain_tag(terrain: Terrain) -> &'static str {
    match terrain {
        Terrain::Unknown => "unknown",
        Terrain::Trail => "trail",
        Terrain::Forest => "forest",
        Terrain::Alpine => "alpine",
        Terrain::Talus => "talus",
        Terrain::Scramble => "scramble",
        Terrain::Pavement => "pavement",
        Terrain::Road => "road",
        Terrain::Water => "water",
    }
}

const fn access_tag(access: Access) -> &'static str {
    match access {
        Access::Unknown => "unknown",
        Access::Open => "open",
        Access::Restricted => "restricted",
        Access::Closed => "closed",
        Access::Private => "private",
    }
}

const fn edge_travel_tag(travel: EdgeTravel) -> &'static str {
    match travel {
        EdgeTravel::Both => "both",
        EdgeTravel::Forward => "forward",
        EdgeTravel::Backward => "backward",
    }
}

fn access_baseline_path(project: &Path) -> PathBuf {
    project.join("sources/access-baseline.json")
}

fn load_access_baseline(project: &Path) -> Result<Option<AccessBaseline>> {
    let path = access_baseline_path(project);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).map(Some).map_err(Into::into)
}

fn load_stored_access_overlays(project: &Path) -> Result<Vec<AccessOverlay>> {
    let path = project.join("sources/access-overlays.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).map_err(Into::into)
}

fn rerate_cached_graph(project: &Path, weights: DifficultyWeights) -> Result<usize> {
    let mut graph = load_graph(project)?;
    for edge in &mut graph.edges {
        weights.apply_edge(edge);
    }
    let count = graph.edges.len();
    save_graph(project, &graph)?;
    Ok(count)
}

fn load_route_line(path: &Path) -> Result<LineString> {
    load_route_file(path).map(|route| route.line)
}

fn load_route_file(path: &Path) -> Result<RouteFile> {
    match source_ext(path).as_deref() {
        Some("kmz") => {
            let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
            Ok(kmz::route_file_from_bytes(&bytes)?)
        }
        Some("csv") => {
            let raw =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            Ok(csv::route_file_from_str(&raw)?)
        }
        Some("gpx") => {
            let raw =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            Ok(gpx::route_file_from_str(&raw)?)
        }
        Some("kml") => {
            let raw =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            Ok(kml::route_file_from_str(&raw)?)
        }
        Some("geojson" | "json") => {
            let raw =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            if source_ext(path).as_deref() == Some("json") {
                Ok(json_route_file(&raw)?.0)
            } else {
                Ok(geojson::route_file_from_str(&raw)?)
            }
        }
        Some(ext) => bail!("unsupported route extension: {ext}"),
        None => bail!("route file has no extension"),
    }
}

fn json_route_line(raw: &str) -> Result<(LineString, &'static str)> {
    json_route_file(raw).map(|(route, adapter)| (route.line, adapter))
}

fn json_route_file(raw: &str) -> Result<(RouteFile, &'static str)> {
    match geojson::route_file_from_str(raw) {
        Ok(route) => Ok((route, "geojson-route")),
        Err(geojson_error) => json_route::route_file_from_str(raw)
            .map(|route| (route, "json-route"))
            .with_context(|| format!("parse JSON route after GeoJSON failed: {geojson_error}")),
    }
}

fn source_ext(path: &Path) -> Option<String> {
    if path
        .file_name()
        .and_then(|x| x.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".osm.pbf"))
    {
        return Some("osm.pbf".to_owned());
    }
    path.extension()
        .and_then(|x| x.to_str())
        .map(str::to_ascii_lowercase)
}

fn load_seeds(project: &Path) -> Result<Vec<SeedRoute>> {
    let path = project.join("seeds/seeds.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(serde_json::from_str(&raw)?)
}

fn load_generated_routes(project: &Path) -> Result<Vec<Route>> {
    let path = project.join("routes/generated.routes.json");
    let raw = fs::read_to_string(&path)
        .with_context(|| "read routes/generated.routes.json; run `trailgen generate` first")?;
    let mut routes = serde_json::from_str::<Vec<Route>>(&raw)?;
    for route in &mut routes {
        route.score = route.computed_score();
    }
    Ok(routes)
}

fn load_generated_constraints(project: &Path) -> Result<Option<LoopConstraints>> {
    Ok(load_generated_config(project)?.map(|config| config.constraints))
}

fn load_generated_config(project: &Path) -> Result<Option<ProjectConfig>> {
    let config: Option<ProjectConfig> = load_generated_manifest_value(project)?
        .and_then(|manifest| manifest.get("effective_config").cloned())
        .map(serde_json::from_value)
        .transpose()?;
    if let Some(config) = &config {
        config.validate()?;
    }
    Ok(config)
}

fn load_generated_source_manifest(project: &Path) -> Result<Option<SourceManifest>> {
    Ok(load_generated_manifest_value(project)?
        .and_then(|manifest| manifest.get("source_manifest").cloned())
        .map(serde_json::from_value)
        .transpose()?)
}

fn load_generated_ledger(project: &Path) -> Result<Option<GenerationLedger>> {
    Ok(load_generated_manifest_value(project)?
        .map(serde_json::from_value)
        .transpose()?)
}

fn load_generated_run_ledger(project: &Path) -> Result<Option<GeneratedRunLedger>> {
    Ok(load_generated_manifest_value(project)?
        .map(serde_json::from_value)
        .transpose()?)
}

fn load_generated_manifest_value(project: &Path) -> Result<Option<serde_json::Value>> {
    let path = project.join("routes/generated.manifest.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(Some(serde_json::from_str(&raw)?))
}

fn select_route<'a>(routes: &'a [Route], name: &str) -> Result<&'a Route> {
    routes
        .iter()
        .find(|route| route.name == name)
        .with_context(|| {
            format!(
                "route {name:?} not found; available routes: {}",
                routes
                    .iter()
                    .map(|route| route.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn load_source_manifest(project: &Path) -> Result<Option<SourceManifest>> {
    let path = project.join("sources/manifest.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(Some(serde_json::from_str(&raw)?))
}

fn source_manifest(area: Option<GeoBounds>, candidates: Vec<SourceCandidate>) -> SourceManifest {
    let adapters = adapter_registry();
    let recommendations = discovery_recommendations(area);
    let coverage = source_coverage(&adapters, &recommendations, &candidates);
    SourceManifest {
        adapters,
        recommendations,
        coverage,
        candidates,
    }
}

fn refresh_source_coverage(manifest: &mut SourceManifest) {
    let area = manifest
        .recommendations
        .iter()
        .find_map(|recommendation| recommendation.area);
    refresh_source_coverage_for_area(manifest, area);
}

fn refresh_source_coverage_for_area(manifest: &mut SourceManifest, area: Option<GeoBounds>) {
    manifest.adapters = adapter_registry();
    manifest.recommendations = discovery_recommendations(area);
    manifest.coverage = source_coverage(
        &manifest.adapters,
        &manifest.recommendations,
        &manifest.candidates,
    );
}

fn register_source_candidate(project: &Path, source: &Path) -> Result<()> {
    let Some(mut candidate) = classify_path(source) else {
        return Ok(());
    };
    candidate.fingerprint = Some(source_fingerprint(source)?);
    register_source_candidates(project, vec![candidate])
}

fn register_source_candidate_as(
    project: &Path,
    source: &Path,
    kind: SourceKind,
    adapter_id: &str,
) -> Result<()> {
    register_source_candidates(
        project,
        vec![source_candidate(
            source,
            kind,
            adapter_id,
            source_fingerprint(source)?,
        )],
    )
}

fn register_access_sources(project: &Path, bundles: &[AccessOverlayBundle<'_>]) -> Result<()> {
    let mut candidates = Vec::new();
    for bundle in bundles {
        let fingerprint = source_fingerprint(bundle.source)?;
        let mut kinds = bundle
            .overlays
            .iter()
            .map(|overlay| access_source_kind(overlay.access))
            .collect::<Vec<_>>();
        kinds.sort_unstable();
        kinds.dedup();
        for kind in kinds {
            candidates.push(source_candidate(
                bundle.source,
                kind,
                &access_adapter_id(bundle.source, kind),
                fingerprint.clone(),
            ));
        }
    }
    register_source_candidates(project, candidates)
}

const fn access_source_kind(access: Access) -> SourceKind {
    match access {
        Access::Closed => SourceKind::Closure,
        Access::Unknown | Access::Open | Access::Restricted | Access::Private => SourceKind::Access,
    }
}

fn access_adapter_id(source: &Path, kind: SourceKind) -> String {
    classify_path(source).map_or_else(
        || fallback_access_adapter_id(source, kind),
        |candidate| {
            if candidate.kind == kind {
                candidate.adapter_id
            } else {
                fallback_access_adapter_id(source, kind)
            }
        },
    )
}

fn fallback_access_adapter_id(source: &Path, kind: SourceKind) -> String {
    match (kind, source_ext(source).as_deref()) {
        (SourceKind::Closure, Some("shp")) => "shapefile-closure-layer".to_owned(),
        (SourceKind::Closure, _) => "geojson-closure-overlay".to_owned(),
        (SourceKind::Access, Some("shp")) => "shapefile-access-overlay".to_owned(),
        (SourceKind::Access, _) => "geojson-access-overlay".to_owned(),
        _ => unreachable!("access overlay registration only handles access and closure"),
    }
}

fn register_context_source(
    project: &Path,
    source: &Path,
    overlays: &[trailgen_core::ContextOverlay],
) -> Result<()> {
    let fingerprint = source_fingerprint(source)?;
    let mut candidates = Vec::new();
    if overlays
        .iter()
        .any(|overlay| overlay.kind == CrossingKind::Road)
    {
        candidates.push(source_candidate(
            source,
            SourceKind::Road,
            &context_adapter_id(source, SourceKind::Road),
            fingerprint.clone(),
        ));
    }
    if overlays
        .iter()
        .any(|overlay| overlay.kind == CrossingKind::Water)
    {
        candidates.push(source_candidate(
            source,
            SourceKind::Hydrology,
            &context_adapter_id(source, SourceKind::Hydrology),
            fingerprint,
        ));
    }
    register_source_candidates(project, candidates)
}

fn context_adapter_id(source: &Path, kind: SourceKind) -> String {
    classify_path(source).map_or_else(
        || fallback_context_adapter_id(source, kind),
        |candidate| {
            if candidate.kind == kind {
                candidate.adapter_id
            } else {
                fallback_context_adapter_id(source, kind)
            }
        },
    )
}

fn fallback_context_adapter_id(source: &Path, kind: SourceKind) -> String {
    match (kind, source_ext(source).as_deref()) {
        (SourceKind::Road, Some("shp")) => "shapefile-road-context".to_owned(),
        (SourceKind::Road, Some("osm" | "osm.pbf")) => "osm-road-context".to_owned(),
        (SourceKind::Road, _) => "geojson-road-context".to_owned(),
        (SourceKind::Hydrology, Some("shp")) => "shapefile-hydrology-context".to_owned(),
        (SourceKind::Hydrology, Some("osm" | "osm.pbf")) => "osm-hydrology-context".to_owned(),
        (SourceKind::Hydrology, _) => "geojson-hydrology-context".to_owned(),
        _ => unreachable!("context overlay registration only handles road and hydrology"),
    }
}

fn source_candidate(
    source: &Path,
    kind: SourceKind,
    adapter_id: &str,
    fingerprint: SourceFingerprint,
) -> SourceCandidate {
    SourceCandidate {
        path: source.display().to_string(),
        kind,
        adapter_id: adapter_id.to_owned(),
        origin: None,
        fingerprint: Some(fingerprint),
    }
}

fn cached_source_kind_adapter(
    source: &Path,
    kind: Option<SourceKind>,
    adapter: Option<&str>,
) -> Result<(SourceKind, String)> {
    let classified = classify_path(source);
    let kind = kind
        .or_else(|| classified.as_ref().map(|candidate| candidate.kind))
        .with_context(|| {
            format!(
                "cannot infer source kind for {}; pass --kind",
                source.display()
            )
        })?;
    let adapter_id = adapter
        .map(str::to_owned)
        .or_else(|| classified.map(|candidate| candidate.adapter_id))
        .with_context(|| {
            format!(
                "cannot infer source adapter for {}; pass --adapter",
                source.display()
            )
        })?;
    ensure_adapter_supports_kind(kind, &adapter_id)?;
    Ok((kind, adapter_id))
}

fn ensure_adapter_supports_kind(kind: SourceKind, adapter_id: &str) -> Result<()> {
    let Some(adapter) = adapter_registry()
        .into_iter()
        .find(|adapter| adapter.id == adapter_id)
    else {
        bail!("unknown source adapter {adapter_id:?}");
    };
    if adapter.kind == kind {
        Ok(())
    } else {
        bail!(
            "adapter {adapter_id:?} has kind {:?}, not {:?}",
            adapter.kind,
            kind
        );
    }
}

fn register_source_candidates(project: &Path, candidates: Vec<SourceCandidate>) -> Result<()> {
    if candidates.is_empty() {
        return Ok(());
    }
    let mut manifest =
        load_source_manifest(project)?.unwrap_or_else(|| source_manifest(None, Vec::new()));
    merge_source_candidates(&mut manifest.candidates, candidates);
    refresh_source_coverage(&mut manifest);
    write_json(project.join("sources/manifest.json"), &manifest)
}

fn merge_source_candidates(into: &mut Vec<SourceCandidate>, candidates: Vec<SourceCandidate>) {
    into.retain(|old| {
        candidates
            .iter()
            .all(|candidate| candidate.path != old.path)
    });
    into.extend(candidates);
    into.sort_by(|a, b| (&a.path, a.kind, &a.adapter_id).cmp(&(&b.path, b.kind, &b.adapter_id)));
}

fn unregister_source_candidate_path(project: &Path, path: &str) -> Result<()> {
    let Some(mut manifest) = load_source_manifest(project)? else {
        return Ok(());
    };
    manifest
        .candidates
        .retain(|candidate| candidate.path != path);
    refresh_source_coverage(&mut manifest);
    write_json(project.join("sources/manifest.json"), &manifest)
}

fn source_fingerprint(path: &Path) -> Result<SourceFingerprint> {
    let members = fingerprint_members(path)?;
    let mut bytes = 0u64;
    let mut hasher = Sha256::new();
    for member in &members {
        if members.len() > 1 {
            hasher.update(
                member
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or("")
                    .as_bytes(),
            );
            hasher.update([0]);
        }
        let mut file = fs::File::open(member)
            .with_context(|| format!("open {} for fingerprinting", member.display()))?;
        let mut chunk = vec![0_u8; 64 * 1024].into_boxed_slice();
        loop {
            let read = file
                .read(&mut chunk)
                .with_context(|| format!("read {} for fingerprinting", member.display()))?;
            if read == 0 {
                break;
            }
            bytes = bytes
                .checked_add(u64::try_from(read).expect("buffer length fits u64"))
                .with_context(|| "source fingerprint byte count overflow")?;
            hasher.update(&chunk[..read]);
        }
        if members.len() > 1 {
            hasher.update([0xff]);
        }
    }
    let digest = hasher.finalize();
    let mut sha256 = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut sha256, "{byte:02x}")?;
    }
    Ok(SourceFingerprint { bytes, sha256 })
}

fn seed_ledger_manifest(project: &Path) -> Result<SeedLedgerManifest> {
    let path = project.join("seeds/seeds.json");
    if !path.exists() {
        return Ok(SeedLedgerManifest {
            present: false,
            routes: 0,
            fingerprint: None,
        });
    }
    Ok(SeedLedgerManifest {
        present: true,
        routes: load_seeds(project)?.len(),
        fingerprint: Some(source_fingerprint(&path)?),
    })
}

fn fingerprint_members(path: &Path) -> Result<Vec<PathBuf>> {
    match source_ext(path).as_deref() {
        Some("shp") => shapefile_fingerprint_members(path),
        Some("vrt") => vrt_fingerprint_members(path),
        _ => Ok(vec![path.to_path_buf()]),
    }
}

fn shapefile_fingerprint_members(path: &Path) -> Result<Vec<PathBuf>> {
    let dbf = path.with_extension("dbf");
    if !dbf.exists() {
        bail!(
            "shapefile source {} is missing mandatory DBF sidecar {}",
            path.display(),
            dbf.display()
        );
    }
    let mut members = vec![path.to_path_buf(), dbf];
    for ext in ["shx", "prj", "cpg"] {
        let sidecar = path.with_extension(ext);
        if sidecar.exists() {
            members.push(sidecar);
        }
    }
    members.sort();
    Ok(members)
}

fn vrt_fingerprint_members(path: &Path) -> Result<Vec<PathBuf>> {
    let mut members = vec![path.to_path_buf()];
    members.extend(VrtDem::referenced_sources(path)?);
    members.sort();
    Ok(members)
}

#[derive(Clone, Copy)]
struct GenerationManifestInput<'a> {
    project: &'a Path,
    options: &'a GenerateOptions,
    config: &'a ProjectConfig,
    forbidden_areas: &'a [ForbiddenAreaManifest],
    graph: &'a TrailGraph,
    start: StartSnap,
    routes: &'a [Route],
    solver_label: &'static str,
}

fn generation_manifest(input: GenerationManifestInput<'_>) -> Result<GenerationManifest> {
    let GenerationManifestInput {
        project,
        options,
        config,
        forbidden_areas,
        graph,
        start,
        routes,
        solver_label,
    } = input;
    let source_manifest = load_source_manifest(project)?;
    let source_coverage_summary = source_manifest
        .as_ref()
        .map(|manifest| summarize_source_coverage(&manifest.coverage));
    Ok(GenerationManifest {
        schema_version: 1,
        app_version: env!("CARGO_PKG_VERSION"),
        solver: solver_label.to_owned(),
        requested_solver: config.solver,
        random_seed: options.seed,
        requested_start: start.requested,
        snapped_start_vertex: start.snapped,
        snapped_start_coord: start.snapped_coord,
        start_snap_m: start.distance_m,
        effective_config: config.clone(),
        source_manifest,
        source_coverage_summary,
        seed_ledger: seed_ledger_manifest(project)?,
        forbidden_areas: forbidden_areas.to_vec(),
        graph: graph_manifest(graph),
        routes: routes.iter().map(route_manifest_entry).collect(),
        artifacts: generation_artifacts(routes),
        artifact_fingerprints: Vec::new(),
    })
}

fn write_generation_manifest(project: &Path, manifest: &GenerationManifest) -> Result<()> {
    write_json(project.join("routes/generated.manifest.json"), manifest)
}

fn finalize_generation_manifest(project: &Path, manifest: &mut GenerationManifest) -> Result<()> {
    manifest.artifact_fingerprints =
        generation_artifact_fingerprints(project, &manifest.artifacts)?;
    write_generation_manifest(project, manifest)
}

fn generation_artifact_fingerprints(
    project: &Path,
    artifacts: &[String],
) -> Result<Vec<GeneratedArtifactFingerprint>> {
    artifacts
        .iter()
        .filter(|artifact| artifact.as_str() != "routes/generated.manifest.json")
        .map(|artifact| {
            let path = project.join(artifact);
            Ok(GeneratedArtifactFingerprint {
                path: artifact.clone(),
                fingerprint: source_fingerprint(&path).with_context(|| {
                    format!("fingerprint generated artifact {}", path.display())
                })?,
            })
        })
        .collect()
}

fn previous_generated_artifacts(project: &Path) -> Result<BTreeSet<String>> {
    Ok(load_generated_run_ledger(project)?
        .map(|ledger| ledger.artifacts.into_iter().collect())
        .unwrap_or_default())
}

fn remove_obsolete_generation_artifacts(
    project: &Path,
    previous: BTreeSet<String>,
    current: &[String],
) -> Result<()> {
    let current = current.iter().map(String::as_str).collect::<BTreeSet<_>>();
    for artifact in previous {
        if artifact == "routes/generated.manifest.json" || current.contains(artifact.as_str()) {
            continue;
        }
        let path = resolve_generated_artifact_path(project, &artifact)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("remove obsolete generated artifact {}", path.display())
                });
            }
        }
    }
    Ok(())
}

fn graph_manifest(graph: &TrailGraph) -> GraphManifest {
    let mut terrain_km = BTreeMap::<Terrain, f64>::new();
    for edge in &graph.edges {
        *terrain_km.entry(edge.attr.terrain).or_default() += edge.attr.length_m / 1_000.0;
    }
    GraphManifest {
        vertices: graph.vertices.len(),
        edges: graph.edges.len(),
        edge_km: graph
            .edges
            .iter()
            .map(|edge| edge.attr.length_m)
            .sum::<f64>()
            / 1_000.0,
        directed_travel_edges: directed_travel_edge_count(graph),
        turn_bans: TurnBanManifest {
            count: graph.turn_bans.len(),
            provenance: turn_ban_sources(graph),
        },
        elevation: graph_elevation_stats(graph),
        low_confidence_edges: graph
            .edges
            .iter()
            .filter(|edge| edge.attr.confidence < LOW_CONFIDENCE_THRESHOLD)
            .count(),
        crossings: crossing_totals(graph),
        terrain_km,
    }
}

fn graph_elevation_stats(graph: &TrailGraph) -> GraphElevationStats {
    let mut stats = GraphElevationStats::default();
    for edge in &graph.edges {
        let a = &edge.attr;
        if edge_has_elevation(a) {
            stats.attributed_edge_m += a.length_m;
        }
        stats.sampled_grade_m += a.grade_distribution.total_m();
        stats.ascent_m += a.ascent_m;
        stats.descent_m += a.descent_m;
        stats.sustained_steep_m += a.sustained_steep_m;
        stats.grade_distribution_m.flat_m += a.grade_distribution.flat_m;
        stats.grade_distribution_m.rolling_m += a.grade_distribution.rolling_m;
        stats.grade_distribution_m.steep_m += a.grade_distribution.steep_m;
        stats.grade_distribution_m.savage_m += a.grade_distribution.savage_m;
        for provenance in &a.elevation_provenance {
            *stats
                .provenance_edges
                .entry(provenance_label(provenance))
                .or_default() += 1;
        }
    }
    stats
}

fn directed_travel_edge_count(graph: &TrailGraph) -> usize {
    graph
        .edges
        .iter()
        .filter(|edge| edge.attr.travel != EdgeTravel::Both)
        .count()
}

fn turn_ban_sources(graph: &TrailGraph) -> BTreeMap<String, usize> {
    graph
        .turn_bans
        .iter()
        .fold(BTreeMap::new(), |mut counts, ban| {
            *counts.entry(provenance_label(&ban.provenance)).or_default() += 1;
            counts
        })
}

fn route_manifest_entry(route: &Route) -> RouteManifestEntry {
    RouteManifestEntry {
        name: route.name.clone(),
        start: route.start,
        edges: route.edges.clone(),
        score: route.computed_score(),
        metrics: route.metrics.clone(),
        satisfied: route.verdict.satisfied,
        violations: route.verdict.violations.clone(),
        audit: route.verdict.audit.clone(),
        rank: route.pareto_rank,
    }
}

fn generation_artifacts(routes: &[Route]) -> Vec<String> {
    let mut artifacts = vec![
        "routes/generated.geojson".to_owned(),
        "routes/generated.graph.json".to_owned(),
        "routes/generated.routes.json".to_owned(),
        "routes/generated.manifest.json".to_owned(),
        "reports/generated.md".to_owned(),
        "reports/map.html".to_owned(),
    ];
    for route in routes {
        artifacts.extend([
            format!("routes/{}.geojson", route.name),
            format!("routes/{}.gpx", route.name),
            format!("routes/{}.csv", route.name),
            format!("routes/{}.kml", route.name),
            format!("routes/{}.kmz", route.name),
            format!("reports/{}.md", route.name),
        ]);
    }
    artifacts
}

fn source_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.exists() {
        return Ok(files);
    }
    for entry in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        ensure!(
            !kind.is_symlink(),
            "source discovery refuses symlink {}",
            path.display()
        );
        if kind.is_dir() {
            files.extend(source_files(&path)?);
        } else if !is_generated_source_artifact(&path) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn cached_source_path(project: &Path, input: &str, output: Option<&Path>) -> Result<PathBuf> {
    let relative = output.map_or_else(|| inferred_source_name(input), PathBuf::from);
    ensure_safe_relative_source_path(&relative)?;
    Ok(project.join("sources").join(relative))
}

fn inferred_source_name(input: &str) -> PathBuf {
    let head = input
        .split(['?', '#'])
        .next()
        .unwrap_or(input)
        .trim_end_matches('/');
    let name = Path::new(head)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("source.bin");
    PathBuf::from(name)
}

fn ensure_safe_relative_source_path(path: &Path) -> Result<()> {
    ensure_safe_relative_project_path(path).map_err(|_| {
        anyhow::anyhow!("source cache output must be a relative path under project/sources")
    })
}

fn ensure_safe_relative_project_path(path: &Path) -> Result<()> {
    if path
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
    {
        Ok(())
    } else {
        bail!("path must be relative and stay inside the project")
    }
}

fn ensure_route_artifact_key(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "route artifact name must be 1..=128 ASCII letters, digits, '-' or '_'"
    );
    Ok(())
}

fn read_source_input(input: &str) -> Result<Vec<u8>> {
    if input.starts_with("http://") || input.starts_with("https://") {
        let response = reqwest::blocking::get(input)
            .with_context(|| format!("GET {input}"))?
            .error_for_status()
            .with_context(|| format!("GET {input} returned an error status"))?;
        read_bounded(
            response,
            MAX_SOURCE_BYTES,
            &format!("response from {input}"),
        )
    } else {
        let path = input.strip_prefix("file://").unwrap_or(input);
        let file = fs::File::open(path).with_context(|| format!("open {path}"))?;
        read_bounded(file, MAX_SOURCE_BYTES, path)
    }
}

fn read_bounded(reader: impl Read, limit: u64, label: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {label}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        bail!("{label} exceeds {limit} byte limit");
    }
    Ok(bytes)
}

fn copy_shapefile_sidecars(input: &str, cached_shp: &Path) -> Result<()> {
    if source_ext(cached_shp).as_deref() != Some("shp") {
        return Ok(());
    }
    let Some(local_shp) = local_input_path(input) else {
        bail!(
            "remote loose shapefile caching requires a bundled .zip archive; cache local .shp/.dbf/.shx files, pass --output name.shp for zipped shapefiles, or normalize to GeoJSON first"
        );
    };
    for ext in ["dbf", "shx", "prj", "cpg"] {
        let src = local_shp.with_extension(ext);
        if !src.exists() && ext == "dbf" {
            bail!(
                "shapefile source {} is missing mandatory DBF sidecar {}",
                local_shp.display(),
                src.display()
            );
        }
        if src.exists() {
            fs::copy(&src, cached_shp.with_extension(ext)).with_context(|| {
                format!(
                    "cache shapefile sidecar {} beside {}",
                    src.display(),
                    cached_shp.display()
                )
            })?;
        }
    }
    Ok(())
}

fn looks_like_zip(input: &str, bytes: &[u8]) -> bool {
    source_input_ext(input).as_deref() == Some("zip") || bytes.starts_with(b"PK\x03\x04")
}

fn source_input_ext(input: &str) -> Option<String> {
    input
        .split(['?', '#'])
        .next()
        .and_then(|head| Path::new(head).extension())
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
}

fn extract_shapefile_archive(bytes: &[u8], cached_shp: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .with_context(|| "read zipped shapefile archive")?;
    let stem = shapefile_archive_stem(&mut archive, cached_shp)?;
    for ext in ["shp", "dbf", "shx", "prj", "cpg"] {
        let output = cached_shp.with_extension(ext);
        match archive_member_bytes(&mut archive, &stem, ext)? {
            Some(member) => write_bytes(output, member)?,
            None if ext == "shp" || ext == "dbf" => bail!(
                "zipped shapefile archive is missing mandatory .{ext} member for stem {stem:?}"
            ),
            None => {}
        }
    }
    Ok(())
}

fn extract_source_archive(bytes: &[u8], cached: &Path) -> Result<()> {
    if source_ext(cached).as_deref() == Some("shp") {
        return extract_shapefile_archive(bytes, cached);
    }
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).with_context(|| "read zipped source archive")?;
    let index = source_archive_member_index(&mut archive, cached)?;
    let mut file = archive.by_index(index)?;
    if file.size() > MAX_ARCHIVE_MEMBER_BYTES {
        bail!(
            "archive member {} is {} bytes; limit is {MAX_ARCHIVE_MEMBER_BYTES}",
            file.name(),
            file.size()
        );
    }
    let name = file.name().to_owned();
    let member = read_bounded(&mut file, MAX_ARCHIVE_MEMBER_BYTES, &name)?;
    write_bytes(cached, member)
}

fn source_archive_member_index(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    cached: &Path,
) -> Result<usize> {
    let ext = source_ext(cached).with_context(|| {
        format!(
            "cached output {} has no extension; pass --output with the desired extracted source name",
            cached.display()
        )
    })?;
    let requested_name = cached
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned);
    let mut matches = Vec::new();
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        if file.is_dir() {
            continue;
        }
        let member = Path::new(file.name());
        if !member
            .extension()
            .and_then(|member_ext| member_ext.to_str())
            .is_some_and(|member_ext| member_ext.eq_ignore_ascii_case(&ext))
        {
            continue;
        }
        let name = member
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| file.name())
            .to_owned();
        if requested_name.as_deref() == Some(name.as_str()) {
            return Ok(i);
        }
        matches.push((i, name));
    }
    match matches.len() {
        0 => bail!(
            "zipped source archive contains no .{ext} member for {}",
            cached.display()
        ),
        1 => Ok(matches[0].0),
        _ => bail!(
            "zipped source archive contains multiple .{ext} members; pass --output with one of: {}",
            matches
                .into_iter()
                .map(|(_, name)| name)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn shapefile_archive_stem(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    cached_shp: &Path,
) -> Result<String> {
    let requested = cached_shp
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_owned);
    let mut stems = BTreeSet::new();
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        let path = Path::new(file.name());
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("shp"))
            && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
        {
            stems.insert(stem.to_owned());
        }
    }
    if let Some(requested) = requested.filter(|requested| stems.contains(requested)) {
        return Ok(requested);
    }
    match stems.len() {
        0 => bail!("zipped shapefile archive contains no .shp member"),
        1 => Ok(stems.into_iter().next().expect("len checked")),
        _ => bail!(
            "zipped shapefile archive contains multiple .shp members; pass --output with one of: {}",
            stems.into_iter().collect::<Vec<_>>().join(", ")
        ),
    }
}

fn archive_member_bytes(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    stem: &str,
    ext: &str,
) -> Result<Option<Vec<u8>>> {
    let mut index = None;
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        let path = Path::new(file.name());
        let matched = path
            .file_stem()
            .and_then(|x| x.to_str())
            .is_some_and(|x| x == stem)
            && path
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case(ext));
        if matched && index.replace(i).is_some() {
            bail!("zipped shapefile archive has multiple .{ext} members for stem {stem:?}");
        }
    }
    let Some(index) = index else {
        return Ok(None);
    };
    let mut file = archive.by_index(index)?;
    if file.size() > MAX_ARCHIVE_MEMBER_BYTES {
        bail!(
            "archive member {} is {} bytes; limit is {MAX_ARCHIVE_MEMBER_BYTES}",
            file.name(),
            file.size()
        );
    }
    let name = file.name().to_owned();
    Ok(Some(read_bounded(
        &mut file,
        MAX_ARCHIVE_MEMBER_BYTES,
        &name,
    )?))
}

fn local_input_path(input: &str) -> Option<PathBuf> {
    if input.starts_with("http://") || input.starts_with("https://") {
        None
    } else {
        Some(PathBuf::from(
            input.strip_prefix("file://").unwrap_or(input),
        ))
    }
}

fn crossing_totals(graph: &TrailGraph) -> BTreeMap<CrossingKind, u32> {
    let mut totals = BTreeMap::new();
    for crossing in graph
        .edges
        .iter()
        .flat_map(|edge| edge.attr.crossings.iter())
    {
        *totals.entry(crossing.kind).or_default() += crossing.count;
    }
    totals
}

fn is_generated_source_artifact(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|x| x.to_str()),
        Some(
            "manifest.json"
                | "discovery.md"
                | "access-overlays.json"
                | "access-baseline.json"
                | "terrain-overlays.json"
                | "context-overlays.json"
                | "elevation-arc-ascii.json"
                | "elevation-geotiff.json"
                | "elevation-vrt.json"
                | "elevation-raster.json"
                | "elevation-mosaic.json"
        )
    )
}

fn parse_coord(raw: &str) -> Result<Coord> {
    let Some((lon, lat)) = raw.split_once(',') else {
        bail!("coordinate must be lon,lat");
    };
    let lon = lon.trim().parse::<f64>()?;
    let lat = lat.trim().parse::<f64>()?;
    ensure!(
        lon.is_finite()
            && lat.is_finite()
            && (-180.0..=180.0).contains(&lon)
            && (-90.0..=90.0).contains(&lat),
        "coordinate must contain finite longitude in -180..=180 and latitude in -90..=90"
    );
    Ok(Coord::new(lon, lat))
}

fn parse_bounds(raw: &str) -> Result<GeoBounds, String> {
    let xs = raw
        .split(',')
        .map(str::trim)
        .map(str::parse::<f64>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let [west, south, east, north] = xs.as_slice() else {
        return Err("bbox must be west,south,east,north".to_owned());
    };
    let bounds = GeoBounds::new(*west, *south, *east, *north);
    if bounds.is_valid() {
        Ok(bounds)
    } else {
        Err("bbox must satisfy -180≤west<east≤180 and -90≤south<north≤90".to_owned())
    }
}

fn parse_source_kind(raw: &str) -> Result<SourceKind, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "trail-network" | "network" | "trails" => Ok(SourceKind::TrailNetwork),
        "seed-route" | "seed" | "route" => Ok(SourceKind::SeedRoute),
        "elevation" | "dem" => Ok(SourceKind::Elevation),
        "terrain" | "surface" | "landcover" | "land-cover" => Ok(SourceKind::Terrain),
        "access" => Ok(SourceKind::Access),
        "closure" | "closures" => Ok(SourceKind::Closure),
        "road" | "roads" => Ok(SourceKind::Road),
        "hydrology" | "water" | "streams" => Ok(SourceKind::Hydrology),
        _ => Err(
            "expected trail-network, seed-route, elevation, terrain, access, closure, road, or hydrology"
                .to_owned(),
        ),
    }
}

const fn source_kind_arg(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::TrailNetwork => "trail-network",
        SourceKind::SeedRoute => "seed-route",
        SourceKind::Elevation => "elevation",
        SourceKind::Terrain => "terrain",
        SourceKind::Access => "access",
        SourceKind::Closure => "closure",
        SourceKind::Road => "road",
        SourceKind::Hydrology => "hydrology",
    }
}

fn shell_arg(raw: &str) -> String {
    if raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':'))
    {
        raw.to_owned()
    } else {
        format!("'{}'", raw.replace('\'', r"'\''"))
    }
}

const fn source_priority_arg(priority: SourcePriority) -> &'static str {
    match priority {
        SourcePriority::Required => "required",
        SourcePriority::Recommended => "recommended",
        SourcePriority::Optional => "optional",
    }
}

const fn source_coverage_status_arg(status: SourceCoverageStatus) -> &'static str {
    match status {
        SourceCoverageStatus::Satisfied => "satisfied",
        SourceCoverageStatus::Missing => "missing",
    }
}

fn parse_shape(raw: &str) -> Result<RouteShape, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "loop" => Ok(RouteShape::Loop),
        "figure-eight" | "figure8" | "fig8" => Ok(RouteShape::FigureEight),
        "out-and-back" | "outback" => Ok(RouteShape::OutAndBack),
        "open" => Ok(RouteShape::Open),
        _ => Err("expected loop, figure-eight, out-and-back, or open".to_owned()),
    }
}

fn parse_solver_kind(raw: &str) -> Result<SolverKind, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(SolverKind::Auto),
        "heuristic" | "loop-hunter" | "loophunter" => Ok(SolverKind::Heuristic),
        "exact" | "exact-enumerator" | "enumerator" => Ok(SolverKind::Exact),
        _ => Err("expected auto, heuristic, or exact".to_owned()),
    }
}

fn parse_positive_usize(raw: &str) -> Result<usize, String> {
    let value = raw
        .parse::<usize>()
        .map_err(|error| format!("expected positive integer: {error}"))?;
    if value == 0 {
        Err("expected positive integer".to_owned())
    } else {
        Ok(value)
    }
}

fn parse_positive_f64(raw: &str) -> Result<f64, String> {
    let value = raw
        .parse::<f64>()
        .map_err(|error| format!("expected positive number: {error}"))?;
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err("expected positive finite number".to_owned())
    }
}

fn parse_planning_date(raw: &str) -> Result<PlanningDate, String> {
    raw.parse()
}

fn parse_planning_time(raw: &str) -> Result<PlanningTime, String> {
    raw.parse()
}

fn parse_terrain(raw: &str) -> Result<Terrain, String> {
    let terrain = Terrain::from_tag(raw);
    if terrain == Terrain::Unknown && !raw.trim().eq_ignore_ascii_case("unknown") {
        Err(format!(
            "unknown terrain {raw:?}; expected one of unknown, trail, forest, alpine, talus, scramble, pavement, road, water"
        ))
    } else {
        Ok(terrain)
    }
}

fn parse_terrain_fraction(raw: &str) -> Result<TerrainFraction, String> {
    let Some((terrain, fraction)) = raw.split_once(':').or_else(|| raw.split_once('=')) else {
        return Err("terrain fraction must be terrain:fraction or terrain=fraction".to_owned());
    };
    let terrain = parse_terrain(terrain)?;
    let fraction = fraction
        .trim()
        .parse::<f64>()
        .map_err(|error| error.to_string())?;
    if (0.0..=1.0).contains(&fraction) {
        Ok(TerrainFraction { terrain, fraction })
    } else {
        Err("terrain fraction must be in [0,1]".to_owned())
    }
}

fn write_json<T: Serialize + ?Sized>(path: impl AsRef<Path>, value: &T) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(value)?)
        .with_context(|| format!("write {}", path.display()))
}

fn write_bytes(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;
    use protobuf::{Message, MessageField};
    use serde_json::Value;
    use trailgen_core::ExactLoopSolver;

    const WGS84_PRJ: &str = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["Degree",0.0174532925199433]]"#;
    const UTM_PRJ: &str = r#"PROJCS["WGS 84 / UTM zone 13N",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PROJECTION["Transverse_Mercator"],UNIT["metre",1],AUTHORITY["EPSG","32613"]]"#;

    fn mini_generate_options() -> GenerateOptions {
        GenerateOptions {
            start: "-105.0000,40.0000".to_owned(),
            min_km: 3.0,
            max_km: 8.0,
            count: 2,
            seed: 0,
            max_start_snap_m: None,
            solver: None,
            max_hops: None,
            max_frontier: None,
            keep: None,
            closure_paths: None,
            source_gate: None,
            date: None,
            time: None,
            min_difficulty: None,
            max_difficulty: None,
            min_ascent_m: None,
            max_ascent_m: None,
            min_descent_m: None,
            max_descent_m: None,
            max_road_fraction: None,
            max_low_confidence_fraction: None,
            max_restricted_access_fraction: None,
            shape: Vec::new(),
            max_repeated_edge_fraction: None,
            forbidden_terrain: Vec::new(),
            forbidden_area: Vec::new(),
            min_terrain: Vec::new(),
            max_terrain: Vec::new(),
        }
    }

    fn forbidden_area_generate_options(forbidden: PathBuf) -> GenerateOptions {
        GenerateOptions {
            solver: Some(SolverKind::Exact),
            max_difficulty: Some(10_000.0),
            forbidden_area: vec![forbidden],
            ..mini_generate_options()
        }
    }

    #[test]
    fn hostile_coordinates_names_and_script_payloads_are_rejected_or_neutered() -> Result<()> {
        for coordinate in ["NaN,40", "-105,inf", "181,40", "-105,91"] {
            assert!(parse_coord(coordinate).is_err(), "accepted {coordinate}");
        }
        for name in ["", "../escape", "route/name", "route.html"] {
            assert!(ensure_route_artifact_key(name).is_err(), "accepted {name}");
        }
        ensure_route_artifact_key("candidate_1-hard")?;

        let mut config = ProjectConfig::new("hostile".to_owned(), None);
        config.constraints.max_distance_m = config.constraints.min_distance_m - 1.0;
        assert!(config.validate().is_err());

        let embedded = js_json(&serde_json::json!({
            "name": "</script><script>alert(1)</script>"
        }))?;
        assert!(!embedded.contains("</script>"));
        assert!(embedded.contains("<\\/script>"));
        Ok(())
    }

    #[test]
    fn cli_schema_is_sound() {
        Cli::command().debug_assert();
        assert!(Cli::try_parse_from(["trailgen"]).unwrap().cmd.is_none());
        assert!(matches!(
            Cli::try_parse_from(["trailgen", "gui", "demo/mini-loop", "--offline"])
                .unwrap()
                .cmd,
            Some(Cmd::Gui { project: Some(project), offline: true })
                if project == Path::new("demo/mini-loop")
        ));
        assert!(matches!(
            Cli::try_parse_from(["trailgen", "gui"]).unwrap().cmd,
            Some(Cmd::Gui {
                project: None,
                offline: false
            })
        ));
    }

    #[test]
    fn generate_options_can_override_terrain_mix_constraints() {
        let mut constraints = LoopConstraints::default();
        apply_generate_options(
            &mut constraints,
            &GenerateOptions {
                max_restricted_access_fraction: Some(0.25),
                forbidden_terrain: vec![Terrain::Pavement, Terrain::Road],
                min_terrain: vec![TerrainFraction {
                    terrain: Terrain::Trail,
                    fraction: 0.65,
                }],
                max_terrain: vec![TerrainFraction {
                    terrain: Terrain::Talus,
                    fraction: 0.10,
                }],
                ..mini_generate_options()
            },
        );

        assert_eq!(
            constraints.forbidden_terrain,
            vec![Terrain::Pavement, Terrain::Road]
        );
        assert_eq!(
            constraints.min_terrain_fraction.get(&Terrain::Trail),
            Some(&0.65)
        );
        assert_eq!(
            constraints.max_terrain_fraction.get(&Terrain::Talus),
            Some(&0.10)
        );
        assert!((constraints.max_restricted_access_fraction - 0.25).abs() <= f64::EPSILON);
        assert_eq!(
            parse_terrain_fraction("scramble=0.25").unwrap(),
            TerrainFraction {
                terrain: Terrain::Scramble,
                fraction: 0.25,
            }
        );
        assert!(parse_terrain_fraction("scramble=1.25").is_err());
    }

    #[test]
    fn graph_stats_report_attributed_exposure() -> Result<()> {
        let turn_source = Provenance {
            source: "fixture-turns".to_owned(),
            layer: Some("turn-restriction".to_owned()),
            source_id: Some("forbidden-corner".to_owned()),
            license: None,
        };
        let graph = GraphBuilder::default().build(&[
            SegmentDraft {
                junctions: JunctionPolicy::default(),
                turn_ref: Some("trail".to_owned()),
                turn_restrictions: vec![trailgen_core::TurnRestrictionDraft {
                    from: "trail".to_owned(),
                    via: Coord::new(0.01, 0.0),
                    to: "restricted-road".to_owned(),
                    rule: trailgen_core::TurnRestrictionRule::No,
                    provenance: turn_source,
                }],
                geometry: LineString::new(vec![
                    Coord::with_ele(0.0, 0.0, 1_000.0),
                    Coord::with_ele(0.01, 0.0, 1_030.0),
                ])
                .unwrap(),
                terrain: Terrain::Trail,
                terrain_confidence: None,
                surface: None,
                access: Access::Open,
                travel: EdgeTravel::Both,
                road_exposure: 0.0,
                confidence: 1.0,
                provenance: Provenance::fixture("trail"),
            },
            SegmentDraft {
                junctions: JunctionPolicy::default(),
                turn_ref: Some("restricted-road".to_owned()),
                turn_restrictions: Vec::new(),
                geometry: LineString::new(vec![Coord::new(0.01, 0.0), Coord::new(0.02, 0.0)])
                    .unwrap(),
                terrain: Terrain::Road,
                terrain_confidence: None,
                surface: Some("gravel".to_owned()),
                access: Access::Restricted,
                travel: EdgeTravel::Both,
                road_exposure: 0.25,
                confidence: 0.5,
                provenance: Provenance::fixture("restricted-road"),
            },
        ])?;

        let text = stats_text(&graph);
        assert!(text.contains("turn bans: 1"));
        assert!(text.contains("Grade distribution:"));
        assert!(text.contains("Elevation provenance:"));
        assert!(text.contains("Turn-ban provenance:"));
        let manifest = graph_manifest(&graph);
        assert_eq!(manifest.directed_travel_edges, 0);
        assert_eq!(manifest.turn_bans.count, 1);
        assert!(manifest.elevation.attributed_edge_m > 1_000.0);
        assert!(manifest.elevation.sampled_grade_m > 1_000.0);
        assert!((manifest.elevation.ascent_m - 30.0).abs() <= f64::EPSILON);
        assert_eq!(
            manifest
                .elevation
                .provenance_edges
                .get("embedded-geometry-elevation"),
            Some(&2)
        );
        assert_eq!(
            manifest
                .turn_bans
                .provenance
                .get("fixture-turns:forbidden-corner"),
            Some(&1)
        );
        Ok(())
    }

    #[test]
    fn graph_save_writes_deterministic_vertex_and_edge_tables() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");

        init(project, "Table Test".to_owned(), None)?;
        build(project, &fixture)?;

        let vertices = fs::read_to_string(project.join("cache/vertices.csv"))?;
        assert!(vertices.starts_with("vertex_id,lon,lat,elevation_m,wkt\n"));
        assert!(vertices.contains("POINT Z ("));

        let edges = fs::read_to_string(project.join("cache/edges.csv"))?;
        assert!(edges.starts_with("edge_id,from_vertex,to_vertex,travel,length_m,"));
        assert!(edges.contains("terrain_confidence,terrain_evidence,access,access_confidence"));
        assert!(edges.contains("access_provenance,road_exposure,confidence,difficulty"));
        assert!(edges.contains("seed_provenance,elevation_provenance"));
        assert!(edges.contains("LINESTRING Z ("));
        assert!(edges.contains("fixture:north"));
        assert!(edges.contains(",trail,"));
        assert!(edges.contains(
            ",0.900000,trail:90%:explicit source terrain tag:fixture:north,open,0.900000,"
        ));
        assert!(edges.contains(",embedded-geometry-elevation,"));

        Ok(())
    }

    #[test]
    fn milp_formulation_command_writes_lp_artifact() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");
        let output = project.join("routes/loop.lp");

        init(project, "MILP Test".to_owned(), None)?;
        build(project, &fixture)?;
        formulate_milp(
            project,
            &MilpFormulationOptions {
                start: "-105.0000,40.0000".to_owned(),
                output: output.clone(),
                min_km: Some(3.0),
                max_km: Some(8.0),
                max_start_snap_m: None,
                date: None,
                time: None,
            },
        )?;

        let lp = fs::read_to_string(output)?;
        assert!(lp.contains("Minimize\n obj:"));
        assert!(lp.contains("flow_start_supplies_visited_vertices"));
        assert!(lp.contains("distance_min:"));
        assert!(lp.contains("distance_max:"));
        assert!(lp.contains("Binary\n"));
        Ok(())
    }

    #[test]
    fn milp_solution_import_writes_generated_route_artifacts() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");
        let solution = project.join("routes/loop.sol");

        init(project, "MILP Import Test".to_owned(), None)?;
        build(project, &fixture)?;
        let graph = load_graph(project)?;
        let start = graph.nearest_vertex(Coord::new(-105.0, 40.0)).unwrap();
        let constraints = LoopConstraints {
            min_distance_m: 3_000.0,
            max_distance_m: 8_000.0,
            ..LoopConstraints::default()
        };
        let route = ExactLoopSolver {
            params: SearchParams::default(),
        }
        .enumerate(&graph, start, &constraints, 1)
        .pop()
        .expect("fixture has an exact loop");
        write_bytes(&solution, milp_solution_from_route(&graph, &route))?;

        import_milp_solution(
            project,
            &MilpIncumbentOptions {
                start: "-105.0000,40.0000".to_owned(),
                solution,
                name: "candidate-1".to_owned(),
                min_km: Some(3.0),
                max_km: Some(8.0),
                max_start_snap_m: None,
                date: None,
                time: None,
            },
        )?;

        let routes: Vec<Route> = serde_json::from_str(&fs::read_to_string(
            project.join("routes/generated.routes.json"),
        )?)?;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].name, "candidate-1");
        assert!(project.join("routes/candidate-1.gpx").exists());
        assert!(project.join("routes/candidate-1.kmz").exists());
        assert!(project.join("reports/candidate-1.md").exists());
        let manifest: Value = serde_json::from_str(&fs::read_to_string(
            project.join("routes/generated.manifest.json"),
        )?)?;
        assert_eq!(manifest["solver"], "milp-incumbent-import");
        assert!(
            manifest["artifacts"]
                .as_array()
                .expect("artifacts")
                .iter()
                .any(|artifact| artifact == "routes/candidate-1.kmz")
        );
        Ok(())
    }

    fn milp_solution_from_route(graph: &TrailGraph, route: &Route) -> String {
        let mut at = route.start;
        let mut out = String::new();
        for edge_id in &route.edges {
            let edge = &graph.edges[edge_id.0];
            let to = edge.traverse(at).expect("test route is traversable");
            let _ = writeln!(out, "z_e{}_v{}_v{} 1", edge_id.0, at.0, to.0);
            at = to;
        }
        out
    }

    #[test]
    fn discovery_ignores_generated_source_metadata() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let sources = tmp.path().join("sources");
        fs::create_dir_all(&sources)?;
        fs::write(sources.join("access-baseline.json"), "{}")?;
        fs::write(sources.join("access-overlays.json"), "{}")?;
        fs::write(sources.join("discovery.md"), "# generated\n")?;
        fs::write(sources.join("elevation-mosaic.json"), "{}")?;
        fs::write(sources.join("elevation-vrt.json"), "{}")?;
        fs::write(sources.join("trails.geojson"), "{}")?;

        let files = source_files(&sources)?;
        assert_eq!(files, vec![sources.join("trails.geojson")]);

        Ok(())
    }

    #[test]
    fn source_plan_renders_filtered_gap_actions() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let area = GeoBounds::new(-105.02, 39.99, -104.98, 40.02);
        init(project, "Source Plan Test".to_owned(), Some(area))?;
        discover(project, None)?;
        let manifest = load_source_manifest(project)?.expect("manifest");
        let plan = render_source_plan(project, &manifest, &[SourceKind::TrailNetwork], false);

        assert!(plan.starts_with("# Source Acquisition Plan"));
        assert!(plan.contains("## TrailNetwork (Required, Missing)"));
        assert!(plan.contains("Direct OSM fallback"));
        assert!(plan.contains("trailgen acquire-osm"));
        assert!(plan.contains("--profile trails --output osm-trails.osm"));
        assert!(plan.contains("--kind trail-network --adapter geojson-network"));
        assert!(plan.contains("NPS official GIS open data"));
        assert!(!plan.contains("## Elevation"));

        fs::write(
            project.join("sources/trails.geojson"),
            include_str!("../../trailgen-core/tests/fixtures/mini_network.geojson"),
        )?;
        discover(project, None)?;
        let manifest = load_source_manifest(project)?.expect("manifest");
        let gap_plan = render_source_plan(project, &manifest, &[SourceKind::TrailNetwork], false);
        assert!(gap_plan.contains("No source acquisition actions match"));
        let full_plan = render_source_plan(project, &manifest, &[SourceKind::TrailNetwork], true);
        assert!(full_plan.contains("## TrailNetwork (Required, Satisfied)"));

        Ok(())
    }

    #[test]
    fn discovery_preserves_registered_ingestion_sources() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");

        init(project, "Discover Preservation Test".to_owned(), None)?;
        discover(project, None)?;
        build(project, &fixture)?;
        discover(project, None)?;

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        let discovery = fs::read_to_string(project.join("sources/discovery.md"))?;
        assert!(discovery.starts_with("# Source Discovery"));
        assert!(discovery.contains("Coverage summary: required incomplete"));
        assert!(discovery.contains("Missing recommended:"));
        assert!(discovery.contains("## Acquisition Plan"));
        assert!(discovery.contains("## Cache Command Sketches"));
        assert!(discovery.contains("NPS official GIS open data"));
        assert!(discovery.contains("Geofabrik OpenStreetMap extracts"));
        assert!(discovery.contains(
            "trailgen cache-source <project> --input '<artifact-url-or-path>' --output trails.geojson --kind trail-network --adapter geojson-network"
        ));
        assert!(discovery.contains(
            "trailgen acquire-osm <project> --profile all --bbox west,south,east,north --output osm-extract.osm"
        ));
        assert!(
            discovery.contains(
                "trailgen acquire-osm <project> --profile trails --bbox west,south,east,north --output osm-trails.osm"
            )
        );
        assert!(
            discovery.contains("trailgen acquire-osm <project> --profile roads --bbox west,south,east,north --output roads.osm")
        );
        assert!(
            discovery
                .contains("--output seeds/completed.gpx --kind seed-route --adapter gpx-route")
        );
        assert!(discovery.contains("## Adapter Registry"));
        assert!(
            manifest["candidates"]
                .as_array()
                .expect("candidates")
                .iter()
                .any(|candidate| {
                    candidate["path"].as_str() == Some(fixture.to_str().unwrap())
                        && candidate["adapter_id"] == "geojson-network"
                })
        );

        Ok(())
    }

    #[test]
    fn source_coverage_gate_enforces_required_recommended_and_explicit_classes() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../trailgen-core/tests/fixtures");
        let network = fixture_dir.join("mini_network.geojson");
        let dem = fixture_dir.join("mini_dem.asc");

        init(project, "Source Gate Test".to_owned(), None)?;
        discover(project, None)?;
        let empty_error = vet_sources(project, SourceGateLevel::Required, &[])
            .expect_err("empty discovery cannot pass required gate");
        assert!(format!("{empty_error:#}").contains("trail-network"));
        assert!(format!("{empty_error:#}").contains("elevation"));

        build(project, &network)?;
        let trail_only_error = vet_sources(project, SourceGateLevel::Required, &[])
            .expect_err("trail-only project cannot pass required gate");
        assert!(format!("{trail_only_error:#}").contains("elevation"));

        apply_elevation(project, &dem, 0.81)?;
        vet_sources(project, SourceGateLevel::Required, &[])?;
        let recommended_error = vet_sources(project, SourceGateLevel::Recommended, &[])
            .expect_err("recommended gate needs terrain/access/context coverage");
        assert!(format!("{recommended_error:#}").contains("terrain"));
        assert!(format!("{recommended_error:#}").contains("hydrology"));

        let seed_error = vet_sources(project, SourceGateLevel::Required, &[SourceKind::SeedRoute])
            .expect_err("explicit seed-route gate needs a seed candidate");
        assert!(format!("{seed_error:#}").contains("seed-route"));

        Ok(())
    }

    #[test]
    fn overpass_queries_are_bbox_scoped_and_parser_aligned() {
        let area = GeoBounds::new(-105.02, 39.99, -104.98, 40.02);
        let bbox = "(39.99,-105.02,40.02,-104.98)";

        let trails = overpass_query(OsmAcquireProfile::Trails, area, 45);
        assert!(trails.starts_with("[out:xml][timeout:45];"));
        assert!(trails.contains(&format!(
            r#"way["highway"~"^(path|footway|track|service|pedestrian|steps|bridleway|unclassified|residential|tertiary|road)$"]{bbox};"#
        )));
        assert!(trails.contains(&format!(r#"way["route"~"^(hiking|foot|walking)$"]{bbox};"#)));
        assert!(trails.contains(&format!(
            r#"relation["type"="route"]["route"~"^(hiking|foot|walking)$"]{bbox};"#
        )));
        assert!(trails.contains(&format!(
            r#"relation["type"="restriction"]["restriction"]{bbox};"#
        )));
        assert!(trails.contains(&format!(
            r#"relation["type"="restriction"]["restriction:foot"]{bbox};"#
        )));
        assert!(trails.ends_with("(._;>;);\nout body;\n"));

        let roads = overpass_query(OsmAcquireProfile::Roads, area, 45);
        assert!(roads.contains("living_street"));
        assert!(roads.contains(bbox));

        let hydrology = overpass_query(OsmAcquireProfile::Hydrology, area, 45);
        assert!(hydrology.contains("waterway"));
        assert!(hydrology.contains("brook"));
        assert!(hydrology.contains(bbox));

        let all = overpass_query(OsmAcquireProfile::All, area, 45);
        assert!(all.contains("living_street"));
        assert!(all.contains("waterway"));
        assert_eq!(
            all.matches(bbox).count(),
            OSM_TRAIL_SELECTORS.len() + OSM_ROAD_SELECTORS.len() + OSM_HYDROLOGY_SELECTORS.len()
        );

        assert_acquired_classes(
            validate_osm_acquisition(OsmAcquireProfile::Trails, tiny_overpass_osm()).unwrap(),
            &[(SourceKind::TrailNetwork, "osm-xml-network", 2)],
        );
        assert_acquired_classes(
            validate_osm_acquisition(OsmAcquireProfile::Roads, tiny_overpass_osm()).unwrap(),
            &[(SourceKind::Road, "osm-road-context", 1)],
        );
        assert_acquired_classes(
            validate_osm_acquisition(OsmAcquireProfile::Hydrology, tiny_overpass_osm()).unwrap(),
            &[(SourceKind::Hydrology, "osm-hydrology-context", 1)],
        );
        assert_acquired_classes(
            validate_osm_acquisition(OsmAcquireProfile::All, tiny_overpass_osm()).unwrap(),
            &[
                (SourceKind::TrailNetwork, "osm-xml-network", 2),
                (SourceKind::Road, "osm-road-context", 1),
                (SourceKind::Hydrology, "osm-hydrology-context", 1),
            ],
        );
        assert!(
            validate_osm_acquisition(OsmAcquireProfile::Hydrology, empty_overpass_osm()).is_err()
        );
    }

    #[test]
    fn osm_acquisition_caches_xml_query_and_profiled_manifest_candidates() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let area = GeoBounds::new(-105.02, 39.99, -104.98, 40.02);
        init(project, "OSM Acquisition Test".to_owned(), Some(area))?;

        for profile in [
            OsmAcquireProfile::Trails,
            OsmAcquireProfile::Roads,
            OsmAcquireProfile::Hydrology,
        ] {
            let acquisition = OsmAcquisition {
                profile,
                bbox: None,
                output: None,
                endpoint: "https://overpass.example.test/api/interpreter".to_owned(),
                timeout_s: 30,
                print_query: false,
            };
            let query = overpass_query(profile, area, acquisition.timeout_s);
            cache_acquired_osm(
                project,
                &acquisition,
                area,
                &query,
                tiny_overpass_osm().as_bytes(),
            )?;
        }
        let all = OsmAcquisition {
            profile: OsmAcquireProfile::All,
            bbox: None,
            output: Some(PathBuf::from("osm-complete.osm")),
            endpoint: "https://overpass.example.test/api/interpreter".to_owned(),
            timeout_s: 30,
            print_query: false,
        };
        let query = overpass_query(all.profile, area, all.timeout_s);
        cache_acquired_osm(project, &all, area, &query, tiny_overpass_osm().as_bytes())?;
        verify_sources(project)?;

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        let candidates = manifest["candidates"].as_array().expect("candidates");
        for (output, kind, adapter, profile) in [
            (
                "osm-trails.osm",
                "trail-network",
                "osm-xml-network",
                "trails",
            ),
            ("roads.osm", "road", "osm-road-context", "roads"),
            (
                "hydrology.osm",
                "hydrology",
                "osm-hydrology-context",
                "hydrology",
            ),
            (
                "osm-complete.osm",
                "trail-network",
                "osm-xml-network",
                "all",
            ),
            ("osm-complete.osm", "road", "osm-road-context", "all"),
            (
                "osm-complete.osm",
                "hydrology",
                "osm-hydrology-context",
                "all",
            ),
        ] {
            assert!(project.join("sources").join(output).exists(), "{output}");
            assert!(
                project
                    .join("sources")
                    .join(output)
                    .with_extension("overpassql")
                    .exists(),
                "{output} query sidecar"
            );
            assert!(candidates.iter().any(|candidate| {
                candidate["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with(output))
                    && candidate["kind"] == kind
                    && candidate["adapter_id"] == adapter
                    && candidate["origin"]
                        .as_str()
                        .is_some_and(|origin| origin.contains(&format!("profile={profile}")))
                    && candidate["fingerprint"]["sha256"]
                        .as_str()
                        .is_some_and(|hash| hash.len() == 64)
            }));
        }

        Ok(())
    }

    #[test]
    fn generation_manifest_captures_reproducibility_contract() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");

        init(
            project,
            "Ledger Test".to_owned(),
            Some(GeoBounds::new(-105.1, 39.9, -104.9, 40.1)),
        )?;
        discover(project, None)?;
        build(project, &fixture)?;
        let options = GenerateOptions {
            seed: 77,
            solver: Some(SolverKind::Exact),
            max_hops: Some(9),
            max_frontier: Some(1_234),
            keep: Some(7),
            closure_paths: Some(3),
            source_gate: None,
            date: Some("2026-05-15".parse().unwrap()),
            time: None,
            forbidden_terrain: vec![Terrain::Road],
            min_terrain: vec![TerrainFraction {
                terrain: Terrain::Trail,
                fraction: 0.50,
            }],
            max_terrain: vec![TerrainFraction {
                terrain: Terrain::Pavement,
                fraction: 0.05,
            }],
            ..mini_generate_options()
        };
        generate(project, &options)?;

        let raw = fs::read_to_string(project.join("routes/generated.manifest.json"))?;
        let manifest: Value = serde_json::from_str(&raw)?;
        assert_generation_run_header(&manifest);
        assert_eq!(manifest["effective_config"]["max_start_snap_m"], 500.0);
        assert_eq!(manifest["effective_config"]["search"]["max_hops"], 9);
        assert_eq!(
            manifest["effective_config"]["search"]["max_frontier"],
            1_234
        );
        assert_eq!(manifest["effective_config"]["search"]["keep"], 7);
        assert_eq!(manifest["effective_config"]["search"]["closure_paths"], 3);
        assert_eq!(manifest["effective_config"]["search"]["seed"], 77);
        assert_eq!(manifest["effective_config"]["planning_date"], "2026-05-15");
        assert_effective_constraints_manifest(&manifest);
        assert!(manifest["source_manifest"]["adapters"].as_array().is_some());
        assert_eq!(
            manifest["source_manifest"]["recommendations"][0]["area"]["west"],
            -105.1
        );
        assert!(
            manifest["source_manifest"]["recommendations"][0]["acquisition_hints"]
                .as_array()
                .is_some_and(|hints| !hints.is_empty())
        );
        assert_source_coverage_manifest(&manifest);
        assert_eq!(manifest["seed_ledger"]["present"], false);
        assert_eq!(manifest["seed_ledger"]["routes"], 0);
        let candidates = manifest["source_manifest"]["candidates"]
            .as_array()
            .expect("source candidates");
        assert!(candidates.iter().any(|candidate| {
            candidate["adapter_id"] == "geojson-network"
                && candidate["fingerprint"]["sha256"]
                    .as_str()
                    .is_some_and(|hash| hash.len() == 64)
                && candidate["fingerprint"]["bytes"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
        }));
        assert_generation_graph_manifest(&manifest);
        assert!(
            manifest["routes"]
                .as_array()
                .is_some_and(|xs| !xs.is_empty())
        );
        assert!(
            manifest["routes"][0]["edges"]
                .as_array()
                .is_some_and(|xs| !xs.is_empty())
        );
        assert!(
            manifest["routes"][0]["audit"]
                .as_array()
                .is_some_and(|xs| !xs.is_empty())
        );
        assert_generation_artifacts_manifest(&manifest);
        let generated_report = fs::read_to_string(project.join("reports/generated.md"))?;
        assert!(generated_report.contains("## Generation Ledger"));

        Ok(())
    }

    fn assert_generation_run_header(manifest: &Value) {
        assert_eq!(manifest["schema_version"], 1);
        assert_eq!(manifest["app_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["solver"], "exact-enumerator");
        assert_eq!(manifest["requested_solver"], "exact");
        assert_eq!(manifest["random_seed"], 77);
        assert_eq!(manifest["snapped_start_coord"]["lon"], -105.0);
        assert!(
            manifest["start_snap_m"]
                .as_f64()
                .is_some_and(|meters| meters <= 1.0)
        );
    }

    fn assert_generation_graph_manifest(manifest: &Value) {
        assert!(manifest["graph"]["edges"].as_u64().is_some_and(|n| n > 0));
        assert_eq!(manifest["graph"]["directed_travel_edges"], 0);
        assert_eq!(manifest["graph"]["turn_bans"]["count"], 0);
        assert!(
            manifest["graph"]["elevation"]["ascent_m"]
                .as_f64()
                .is_some_and(|meters| meters > 0.0)
        );
        assert!(
            manifest["graph"]["elevation"]["sampled_grade_m"]
                .as_f64()
                .is_some_and(|meters| meters > 0.0)
        );
        assert!(
            manifest["graph"]["elevation"]["grade_distribution_m"]["flat_m"]
                .as_f64()
                .is_some()
        );
        assert!(
            manifest["graph"]["turn_bans"]["provenance"]
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        );
    }

    #[test]
    fn generation_source_gate_rejects_missing_required_sources_and_records_policy() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../trailgen-core/tests/fixtures");
        let network = fixture_dir.join("mini_network.geojson");
        let dem = fixture_dir.join("mini_dem.asc");

        init(project, "Generation Source Gate Test".to_owned(), None)?;
        build(project, &network)?;
        let mut options = mini_generate_options();
        options.source_gate = Some(GenerationSourceGate::Required);
        let error = generate(project, &options).expect_err("missing DEM should fail source gate");
        assert!(format!("{error:#}").contains("generation source gate required failed"));
        assert!(format!("{error:#}").contains("elevation"));

        apply_elevation(project, &dem, 0.81)?;
        generate(project, &options)?;
        let manifest: Value = serde_json::from_str(&fs::read_to_string(
            project.join("routes/generated.manifest.json"),
        )?)?;
        assert_eq!(
            manifest["effective_config"]["generation_source_gate"],
            "required"
        );

        Ok(())
    }

    #[test]
    fn generation_verifier_rejects_source_and_artifact_drift() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../trailgen-core/tests/fixtures");
        let network_fixture = fixture_dir.join("mini_network.geojson");
        let dem_fixture = fixture_dir.join("mini_dem.asc");

        init(project, "Generation Verify Test".to_owned(), None)?;
        let network = project.join("sources/trails.geojson");
        let dem = project.join("sources/dem.asc");
        fs::copy(&network_fixture, &network)?;
        fs::copy(&dem_fixture, &dem)?;
        build(project, &network)?;
        apply_elevation(project, &dem, 0.81)?;
        generate(
            project,
            &GenerateOptions {
                source_gate: Some(GenerationSourceGate::Required),
                ..mini_generate_options()
            },
        )?;
        verify_generation(project)?;

        expect_source_drift(project, &network)?;
        expect_seed_ledger_appearance_drift(project)?;
        expect_generated_run_metadata_drift(project)?;
        expect_generated_manifest_drift(
            project,
            |manifest| manifest["effective_config"]["search"]["max_hops"] = serde_json::json!(1),
            "generated solver replay verification failed",
            "route count mismatch",
        )?;
        expect_generated_manifest_drift(
            project,
            |manifest| {
                manifest["source_coverage_summary"]["required"]["satisfied"] =
                    serde_json::json!(42);
            },
            "generated source coverage summary verification failed",
            "source_coverage_summary drifted",
        )?;
        expect_generated_manifest_drift(
            project,
            |manifest| manifest["graph"]["edges"] = serde_json::json!(0),
            "generated graph verification failed",
            "graph.edges mismatch",
        )?;
        expect_generated_manifest_drift(
            project,
            |manifest| manifest["graph"]["elevation"]["sampled_grade_m"] = serde_json::json!(42.0),
            "generated graph verification failed",
            "graph.elevation.sampled_grade_m mismatch",
        )?;
        expect_generated_manifest_drift(
            project,
            |manifest| manifest["routes"][0]["edges"][0] = serde_json::json!(999_999),
            "generated route verification failed",
            "edge sequence differs",
        )?;
        expect_generated_manifest_drift(
            project,
            |manifest| manifest["routes"][0]["metrics"]["distance_m"] = serde_json::json!(42.0),
            "generated route verification failed",
            "manifest metrics.distance_m mismatch",
        )?;
        expect_generated_manifest_drift(
            project,
            |manifest| {
                manifest["routes"][0]["metrics"]["sustained_steep_m"] = serde_json::json!(42.0);
            },
            "generated route verification failed",
            "manifest metrics.sustained_steep_m mismatch",
        )?;
        expect_generated_manifest_drift(
            project,
            |manifest| {
                manifest["routes"][0]["metrics"]["grade_distribution"]["flat_m"] =
                    serde_json::json!(42.0);
            },
            "generated route verification failed",
            "manifest metrics.grade_distribution.flat_m mismatch",
        )?;
        expect_generated_manifest_drift(
            project,
            |manifest| manifest["routes"][0]["audit"][0]["margin"] = serde_json::json!("rotted"),
            "generated route verification failed",
            "constraint audit differs from manifest",
        )?;
        expect_generated_artifact_drift(project)?;

        Ok(())
    }

    fn expect_source_drift(project: &Path, network: &Path) -> Result<()> {
        let original_network = fs::read(network)?;
        fs::write(network, b"{\"type\":\"FeatureCollection\",\"features\":[]}")?;
        let error = verify_generation(project).expect_err("source drift should fail verification");
        assert!(format!("{error:#}").contains("source verification failed"));
        assert!(format!("{error:#}").contains("trails.geojson drifted"));
        fs::write(network, original_network)?;
        verify_generation(project)?;
        Ok(())
    }

    fn expect_seed_ledger_appearance_drift(project: &Path) -> Result<()> {
        let seeds_dir = project.join("seeds");
        fs::create_dir_all(&seeds_dir)?;
        let seed_ledger = seeds_dir.join("seeds.json");
        fs::write(&seed_ledger, "[]")?;
        let error =
            verify_generation(project).expect_err("new seed ledger should fail verification");
        assert!(format!("{error:#}").contains("generated seed ledger verification failed"));
        fs::remove_file(seed_ledger)?;
        verify_generation(project)?;
        Ok(())
    }

    struct ManifestDriftCase {
        mutate: fn(&mut Value),
        detail: &'static str,
    }

    fn expect_generated_run_metadata_drift(project: &Path) -> Result<()> {
        let cases = [
            ManifestDriftCase {
                mutate: |manifest| manifest["schema_version"] = serde_json::json!(2),
                detail: "schema_version mismatch",
            },
            ManifestDriftCase {
                mutate: |manifest| manifest["app_version"] = serde_json::json!("rotted"),
                detail: "app_version mismatch",
            },
            ManifestDriftCase {
                mutate: |manifest| manifest["solver"] = serde_json::json!("loop-hunter"),
                detail: "solver mismatch",
            },
            ManifestDriftCase {
                mutate: |manifest| manifest["random_seed"] = serde_json::json!(42),
                detail: "random_seed mismatch",
            },
            ManifestDriftCase {
                mutate: |manifest| manifest["requested_start"]["lon"] = serde_json::json!(-104.5),
                detail: "snapped_start_vertex mismatch",
            },
            ManifestDriftCase {
                mutate: |manifest| {
                    manifest["snapped_start_coord"]["lat"] = serde_json::json!(40.5);
                },
                detail: "snapped_start_coord.lat mismatch",
            },
            ManifestDriftCase {
                mutate: |manifest| manifest["start_snap_m"] = serde_json::json!(42.0),
                detail: "start_snap_m mismatch",
            },
        ];
        for case in cases {
            expect_generated_manifest_drift(
                project,
                case.mutate,
                "generated run metadata verification failed",
                case.detail,
            )?;
        }
        Ok(())
    }

    fn expect_generated_manifest_drift(
        project: &Path,
        mutate: impl FnOnce(&mut Value),
        headline: &str,
        detail: &str,
    ) -> Result<()> {
        let manifest_path = project.join("routes/generated.manifest.json");
        let original_manifest = fs::read_to_string(&manifest_path)?;
        let mut manifest: Value = serde_json::from_str(&original_manifest)?;
        mutate(&mut manifest);
        write_json(&manifest_path, &manifest)?;
        let error =
            verify_generation(project).expect_err("manifest drift should fail verification");
        assert!(format!("{error:#}").contains(headline));
        assert!(format!("{error:#}").contains(detail));
        fs::write(&manifest_path, original_manifest)?;
        verify_generation(project)?;
        Ok(())
    }

    fn expect_generated_artifact_drift(project: &Path) -> Result<()> {
        let report = project.join("reports/generated.md");
        let mut report_text = fs::read_to_string(&report)?;
        report_text.push_str("\nmanual drift\n");
        fs::write(&report, report_text)?;
        let error =
            verify_generation(project).expect_err("artifact drift should fail verification");
        assert!(format!("{error:#}").contains("generated artifact verification failed"));
        assert!(format!("{error:#}").contains("reports/generated.md drifted"));
        Ok(())
    }

    #[test]
    fn generation_prunes_obsolete_recorded_artifacts() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");

        init(project, "Artifact Prune Test".to_owned(), None)?;
        build(project, &fixture)?;
        let mut options = GenerateOptions {
            solver: Some(SolverKind::Exact),
            max_hops: Some(8),
            ..mini_generate_options()
        };
        generate(project, &options)?;
        assert!(project.join("routes/candidate-2.gpx").exists());
        assert!(project.join("reports/candidate-2.md").exists());
        fs::write(project.join("routes/manual.gpx"), "manual route export")?;

        options.count = 1;
        generate(project, &options)?;
        verify_generation(project)?;
        assert!(project.join("routes/candidate-1.gpx").exists());
        assert!(!project.join("routes/candidate-2.gpx").exists());
        assert!(!project.join("reports/candidate-2.md").exists());
        assert!(project.join("routes/manual.gpx").exists());

        let manifest: Value = serde_json::from_str(&fs::read_to_string(
            project.join("routes/generated.manifest.json"),
        )?)?;
        assert!(
            manifest["artifacts"]
                .as_array()
                .expect("artifacts")
                .iter()
                .all(|artifact| artifact != "routes/candidate-2.gpx")
        );

        Ok(())
    }

    #[test]
    fn generation_rejects_remote_start_without_explicit_snap_override() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");

        init(project, "Start Snap Test".to_owned(), None)?;
        build(project, &fixture)?;
        let mut options = GenerateOptions {
            start: "0.0000,0.0000".to_owned(),
            min_km: 3.0,
            max_km: 8.0,
            count: 1,
            seed: 0,
            max_start_snap_m: None,
            solver: Some(SolverKind::Exact),
            max_hops: None,
            max_frontier: None,
            keep: None,
            closure_paths: None,
            source_gate: None,
            date: None,
            time: None,
            min_difficulty: None,
            max_difficulty: None,
            min_ascent_m: None,
            max_ascent_m: None,
            min_descent_m: None,
            max_descent_m: None,
            max_road_fraction: None,
            max_low_confidence_fraction: None,
            max_restricted_access_fraction: None,
            shape: Vec::new(),
            max_repeated_edge_fraction: None,
            forbidden_terrain: Vec::new(),
            forbidden_area: Vec::new(),
            min_terrain: Vec::new(),
            max_terrain: Vec::new(),
        };

        let error = generate(project, &options).expect_err("remote start should be rejected");
        assert!(format!("{error:#}").contains("above max_start_snap_m 500"));

        options.max_start_snap_m = Some(20_000_000.0);
        generate(project, &options)?;
        let manifest: Value = serde_json::from_str(&fs::read_to_string(
            project.join("routes/generated.manifest.json"),
        )?)?;
        assert_eq!(
            manifest["effective_config"]["max_start_snap_m"],
            20_000_000.0
        );
        assert!(
            manifest["start_snap_m"]
                .as_f64()
                .is_some_and(|meters| meters > 1_000_000.0)
        );
        Ok(())
    }

    #[test]
    fn generation_forbid_area_mutates_only_effective_graph() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../trailgen-core/tests/fixtures");
        let network = fixture_dir.join("mini_network.geojson");
        let forbidden = project.join("closure_overlay.geojson");

        init(project, "Forbidden Area Test".to_owned(), None)?;
        discover(project, None)?;
        build(project, &network)?;
        fs::copy(fixture_dir.join("closure_overlay.geojson"), &forbidden)?;
        generate(project, &forbidden_area_generate_options(forbidden.clone()))?;

        let manifest: Value = serde_json::from_str(&fs::read_to_string(
            project.join("routes/generated.manifest.json"),
        )?)?;
        assert_eq!(
            manifest["forbidden_areas"][0]["path"],
            forbidden.display().to_string()
        );
        assert_eq!(
            manifest["forbidden_areas"][0]["adapter_id"],
            "geojson-closure-overlay"
        );
        assert_eq!(manifest["forbidden_areas"][0]["overlays"], 1);
        assert!(
            manifest["forbidden_areas"][0]["touched_edges"]
                .as_u64()
                .is_some_and(|n| n > 0)
        );
        assert!(
            manifest["forbidden_areas"][0]["fingerprint"]["sha256"]
                .as_str()
                .is_some_and(|hash| hash.len() == 64)
        );
        verify_generation(project)?;

        let original_forbidden = fs::read_to_string(&forbidden)?;
        fs::write(&forbidden, format!("{original_forbidden}\n"))?;
        let error =
            verify_generation(project).expect_err("forbidden area drift should fail verification");
        assert!(format!("{error:#}").contains("generated forbidden-area verification failed"));
        assert!(format!("{error:#}").contains("forbidden-area source drifted"));
        fs::write(&forbidden, original_forbidden)?;
        verify_generation(project)?;

        let cached = load_graph(project)?;
        assert!(cached.edges.iter().all(|edge| {
            edge.attr.access != Access::Closed
                && edge
                    .attr
                    .access_provenance
                    .iter()
                    .all(|p| p.source != "forbidden-area")
        }));

        let generated = load_generated_graph(project)?;
        assert!(generated.edges.iter().any(|edge| {
            edge.attr.access == Access::Closed
                && edge
                    .attr
                    .access_provenance
                    .iter()
                    .any(|p| p.source == "forbidden-area")
        }));

        let sources: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        assert!(
            sources["candidates"]
                .as_array()
                .expect("source candidates")
                .iter()
                .all(|candidate| candidate["path"].as_str()
                    != Some(forbidden.to_str().expect("fixture path must be UTF-8")))
        );
        Ok(())
    }

    #[test]
    fn rating_rejects_remote_route_without_explicit_snap_override() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");
        let route = project.join("remote.csv");

        init(project, "Route Snap Test".to_owned(), None)?;
        build(project, &fixture)?;
        fs::write(
            &route,
            "longitude,latitude,elevation_m\n0.0000,0.0000,0\n0.0000,0.0100,0\n",
        )?;

        let error = rate(project, &route, None, None).expect_err("remote route should be rejected");
        assert!(format!("{error:#}").contains("max_route_snap_m 100"));
        let report = project.join("reports/rated.md");
        rate(project, &route, Some(20_000_000.0), Some(&report))?;
        let report = fs::read_to_string(report)?;
        assert!(report.starts_with("# Rated Hiking Route"));
        assert!(report.contains("rated-route"));
        assert!(report.contains("Difficulty decomposition"));
        assert!(report.contains("Low-confidence segments"));
        assert!(report.contains("Most dubious segments"));
        assert!(report.contains("## Constraint Envelope"));
        assert!(report.contains("## Source Manifest"));
        Ok(())
    }

    fn assert_effective_constraints_manifest(manifest: &Value) {
        let constraints = &manifest["effective_config"]["constraints"];
        assert_eq!(constraints["min_distance_m"], 3_000.0);
        assert_eq!(constraints["forbidden_terrain"][0], "road");
        assert_eq!(constraints["min_terrain_fraction"]["trail"], 0.50);
        assert_eq!(constraints["max_terrain_fraction"]["pavement"], 0.05);
        assert_eq!(constraints["max_restricted_access_fraction"], 0.0);
    }

    fn assert_source_coverage_manifest(manifest: &Value) {
        let coverage = manifest["source_manifest"]["coverage"]
            .as_array()
            .expect("source coverage");
        assert!(
            coverage.iter().any(|entry| {
                entry["kind"] == "trail-network" && entry["status"] == "satisfied"
            })
        );
        assert!(
            coverage
                .iter()
                .any(|entry| { entry["kind"] == "elevation" && entry["status"] == "missing" })
        );
        assert_eq!(
            manifest["source_coverage_summary"]["required"]["satisfied"],
            1
        );
        assert_eq!(
            manifest["source_coverage_summary"]["recommended"]["missing"],
            5
        );
        assert!(
            manifest["source_coverage_summary"]["missing_recommended"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "hydrology"))
        );
    }

    fn assert_generation_artifacts_manifest(manifest: &Value) {
        let artifacts = manifest["artifacts"].as_array().expect("artifacts");
        for artifact in [
            "routes/generated.manifest.json",
            "reports/map.html",
            "routes/candidate-1.geojson",
            "reports/candidate-1.md",
        ] {
            assert!(artifacts.iter().any(|x| x == artifact), "{artifact}");
        }
        let fingerprints = manifest["artifact_fingerprints"]
            .as_array()
            .expect("artifact fingerprints");
        assert_eq!(fingerprints.len(), artifacts.len() - 1);
        assert!(fingerprints.iter().all(|entry| {
            entry["path"] != "routes/generated.manifest.json"
                && entry["fingerprint"]["sha256"]
                    .as_str()
                    .is_some_and(|hash| hash.len() == 64)
                && entry["fingerprint"]["bytes"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
        }));
        assert!(
            fingerprints
                .iter()
                .any(|entry| entry["path"] == "reports/generated.md")
        );
    }

    #[test]
    fn generated_routes_can_be_selected_exported_and_reported() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");

        init(project, "Export Test".to_owned(), None)?;
        build(project, &fixture)?;
        generate(project, &mini_generate_options())?;

        let gpx = project.join("exports/candidate-1.gpx");
        let gpx_report = project.join("exports/candidate-1.md");
        let csv = project.join("exports/candidate-1.csv");
        let geojson = project.join("exports/candidate-1.geojson");
        let md = project.join("reports/candidate-1.md");
        let map = project.join("exports/map.html");
        let generated_candidate_report = fs::read_to_string(&md)?;
        assert!(generated_candidate_report.starts_with("# Generated Hiking Route"));
        assert!(generated_candidate_report.contains("Source provenance:"));
        fs::write(
            project.join("sources/manifest.json"),
            "{live manifest is stale",
        )?;
        export_route(
            project,
            "candidate-1",
            ExportFormat::Gpx,
            &gpx,
            Some(&gpx_report),
        )?;
        export_route(project, "candidate-1", ExportFormat::Csv, &csv, None)?;
        export_route(
            project,
            "candidate-1",
            ExportFormat::Geojson,
            &geojson,
            None,
        )?;
        report_generated(project, Some("candidate-1"), Some(&md))?;
        map_html(project, Some(&map))?;

        assert_selected_route_exports(project, &gpx, &csv, &geojson)?;
        assert_selected_route_reports(&md, &gpx_report)?;
        assert_selected_route_maps(project, &map)?;

        Ok(())
    }

    fn assert_selected_route_exports(
        project: &Path,
        gpx: &Path,
        csv: &Path,
        geojson: &Path,
    ) -> Result<()> {
        assert!(gpx::route_line_from_str(&fs::read_to_string(gpx)?)?.length_m() > 3_000.0);
        let csv_text = fs::read_to_string(csv)?;
        assert!(csv_text.starts_with("# name: candidate-1\n"));
        assert!(csv::route_line_from_str(&csv_text)?.length_m() > 3_000.0);
        let selected_geojson = serde_json::from_str::<Value>(&fs::read_to_string(geojson)?)?;
        assert_eq!(selected_geojson["type"], "FeatureCollection");
        let generated_geojson = serde_json::from_str::<Value>(&fs::read_to_string(
            project.join("routes/candidate-1.geojson"),
        )?)?;
        assert_eq!(
            generated_geojson["features"][0]["properties"]["name"],
            "candidate-1"
        );
        let selected_properties = &selected_geojson["features"][0]["properties"];
        assert!(selected_properties["restricted_access_fraction"].is_number());
        assert!(selected_properties["terrain_fraction"].is_object());
        assert!(selected_properties["difficulty_hotspots"].is_array());
        Ok(())
    }

    fn assert_selected_route_reports(md: &Path, sidecar: &Path) -> Result<()> {
        let report = fs::read_to_string(md)?;
        assert!(report.contains("candidate-1"));
        assert!(report.contains("## Generation Ledger"));
        assert!(report.contains("## Constraint Envelope"));
        assert!(report.contains("## Source Manifest"));
        let sidecar_report = fs::read_to_string(sidecar)?;
        assert!(sidecar_report.starts_with("# Generated Hiking Route"));
        assert!(sidecar_report.contains("candidate-1"));
        Ok(())
    }

    fn assert_selected_route_maps(project: &Path, selected: &Path) -> Result<()> {
        let generated_map = fs::read_to_string(project.join("reports/map.html"))?;
        assert!(generated_map.contains("Offline diagnostic map"));
        assert!(generated_map.contains("const graph = {"));
        assert!(generated_map.contains("escapeHtml(p.name || '')"));
        let selected_map = fs::read_to_string(selected)?;
        assert!(selected_map.contains("Export Test"));
        assert!(selected_map.contains("candidate-1"));
        Ok(())
    }

    #[test]
    fn build_accepts_route_files_as_practical_graph_sources() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let route = tmp.path().join("completed.csv");
        fs::write(
            &route,
            "longitude,latitude,elevation_m\n-105.0,40.0,1600\n-104.995,40.0,1620\n-104.990,40.004,1660\n",
        )?;

        init(project, "Route Build Test".to_owned(), None)?;
        build(project, &route)?;

        let graph = load_graph(project)?;
        assert_eq!(graph.edges.len(), 2);
        assert!(
            graph
                .edges
                .iter()
                .all(|edge| edge.attr.terrain_confidence <= 0.35)
        );
        assert!(graph.edges.iter().all(|edge| {
            edge.attr.provenance.iter().any(|p| {
                p.source == "route-file" && p.layer.as_deref() == Some("route-derived-network")
            })
        }));

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        let candidate = manifest["candidates"]
            .as_array()
            .expect("candidates")
            .iter()
            .find(|candidate| candidate["path"].as_str() == Some(route.to_str().unwrap()))
            .expect("route source candidate");
        assert_eq!(candidate["kind"], "seed-route");
        assert_eq!(candidate["adapter_id"], "csv-route");
        assert!(
            manifest["coverage"]
                .as_array()
                .expect("coverage")
                .iter()
                .any(|entry| entry["kind"] == "trail-network" && entry["status"] == "missing")
        );

        let json_project = tmp.path().join("json-route-project");
        let json_route = tmp.path().join("app-route.json");
        fs::write(
            &json_route,
            r#"{"track":[
                {"latitude":40.0,"longitude":-105.0,"elevation":1600},
                {"lat":40.0,"lng":-104.995,"altitude_m":1620},
                {"lat":40.004,"lng":-104.990,"alt":1660}
            ]}"#,
        )?;
        init(&json_project, "Route JSON Build Test".to_owned(), None)?;
        build(&json_project, &json_route)?;
        let json_manifest: Value = serde_json::from_str(&fs::read_to_string(
            json_project.join("sources/manifest.json"),
        )?)?;
        assert!(
            json_manifest["candidates"]
                .as_array()
                .expect("candidates")
                .iter()
                .any(|candidate| candidate["adapter_id"] == "json-route")
        );

        let geojson_project = tmp.path().join("geojson-route-project");
        let geojson_route = tmp.path().join("route.geojson");
        fs::write(
            &geojson_route,
            r#"{"type":"LineString","coordinates":[[-105.0,40.0,1600],[-104.995,40.0,1620]]}"#,
        )?;
        init(
            &geojson_project,
            "Route GeoJSON Build Test".to_owned(),
            None,
        )?;
        build(&geojson_project, &geojson_route)?;
        let manifest: Value = serde_json::from_str(&fs::read_to_string(
            geojson_project.join("sources/manifest.json"),
        )?)?;
        assert!(
            manifest["candidates"]
                .as_array()
                .expect("candidates")
                .iter()
                .any(|candidate| candidate["adapter_id"] == "geojson-route")
        );

        Ok(())
    }

    #[test]
    fn build_accepts_multiple_network_sources() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");
        let spur = tmp.path().join("agency-spur.geojson");
        fs::write(
            &spur,
            r#"{"type":"FeatureCollection","features":[{
                "type":"Feature",
                "properties":{"id":"agency-spur","source":"agency-two","terrain":"trail","access":"open","confidence":0.88},
                "geometry":{"type":"LineString","coordinates":[[-104.9800,40.0000,1685],[-104.9760,40.0000,1690]]}
            }]}"#,
        )?;

        init(project, "Multi Source Build Test".to_owned(), None)?;
        build_many(project, [fixture.as_path(), spur.as_path()], None)?;

        let graph = load_graph(project)?;
        assert!(graph.edges.iter().any(|edge| {
            edge.attr
                .provenance
                .iter()
                .any(|p| p.source == "agency-two" && p.source_id.as_deref() == Some("agency-spur"))
        }));

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        let candidates = manifest["candidates"].as_array().expect("candidates");
        assert!(candidates.iter().any(|candidate| {
            candidate["path"].as_str() == Some(fixture.to_str().unwrap())
                && candidate["adapter_id"] == "geojson-network"
        }));
        assert!(candidates.iter().any(|candidate| {
            candidate["path"].as_str() == Some(spur.to_str().unwrap())
                && candidate["adapter_id"] == "geojson-network"
        }));
        assert!(
            manifest["coverage"]
                .as_array()
                .expect("coverage")
                .iter()
                .any(|entry| entry["kind"] == "trail-network" && entry["status"] == "satisfied")
        );
        Ok(())
    }

    #[test]
    fn build_snap_tolerance_override_persists_topology_policy() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let source = tmp.path().join("near-miss.geojson");
        fs::write(
            &source,
            r#"{"type":"FeatureCollection","features":[
                {
                    "type":"Feature",
                    "properties":{"id":"main","source":"fixture","terrain":"trail","access":"open","confidence":1.0},
                    "geometry":{"type":"LineString","coordinates":[[-105.0000,40.0000],[-104.9900,40.0000]]}
                },
                {
                    "type":"Feature",
                    "properties":{"id":"spur","source":"fixture","terrain":"trail","access":"open","confidence":1.0},
                    "geometry":{"type":"LineString","coordinates":[[-104.9950,40.00005],[-104.9950,40.0040]]}
                }
            ]}"#,
        )?;

        init(&project, "Snap Override Test".to_owned(), None)?;
        build_many(&project, std::iter::once(source.as_path()), Some(8.0))?;
        let graph = load_graph(&project)?;
        assert!(graph.edges.iter().any(|edge| {
            edge.attr.provenance.iter().any(|p| {
                p.source == "graph-builder"
                    && p.layer.as_deref() == Some("near-miss-snap")
                    && p.source_id.as_deref() == Some("tolerance 8.0 m")
            })
        }));
        assert!((load_config(&project)?.snap_tolerance_m - 8.0).abs() <= f64::EPSILON);

        build_many(&project, std::iter::once(source.as_path()), Some(1.0))?;
        let graph = load_graph(&project)?;
        assert!(!graph.edges.iter().any(|edge| {
            edge.attr
                .provenance
                .iter()
                .any(|p| p.layer.as_deref() == Some("near-miss-snap"))
        }));
        assert!((load_config(&project)?.snap_tolerance_m - 1.0).abs() <= f64::EPSILON);

        Ok(())
    }

    #[test]
    fn build_accepts_osm_xml_network_sources() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let osm = tmp.path().join("osm-trails.osm");
        fs::write(
            &osm,
            r#"<osm version="0.6">
  <node id="1" lat="40.0" lon="-105.0"/>
  <node id="2" lat="40.0" lon="-104.99"/>
  <node id="3" lat="40.01" lon="-104.99"/>
  <way id="trail-10">
    <nd ref="1"/><nd ref="2"/>
    <tag k="highway" v="path"/>
    <tag k="surface" v="dirt"/>
    <tag k="foot" v="designated"/>
  </way>
  <way id="service-11">
    <nd ref="2"/><nd ref="3"/>
    <tag k="highway" v="service"/>
    <tag k="access" v="private"/>
  </way>
</osm>"#,
        )?;

        init(project, "OSM Build Test".to_owned(), None)?;
        build(project, &osm)?;

        let graph = load_graph(project)?;
        assert_eq!(graph.edges.len(), 2);
        assert!(graph.edges.iter().any(|edge| {
            edge.attr
                .provenance
                .iter()
                .any(|p| p.source == "osm-xml" && p.source_id.as_deref() == Some("trail-10"))
        }));
        assert!(graph.edges.iter().any(|edge| {
            edge.attr.access == Access::Private
                && (edge.attr.road_exposure - 1.0).abs() <= f64::EPSILON
        }));

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        let candidate = manifest["candidates"]
            .as_array()
            .expect("candidates")
            .iter()
            .find(|candidate| candidate["path"].as_str() == Some(osm.to_str().unwrap()))
            .expect("OSM source candidate");
        assert_eq!(candidate["kind"], "trail-network");
        assert_eq!(candidate["adapter_id"], "osm-xml-network");
        assert!(
            manifest["coverage"]
                .as_array()
                .expect("coverage")
                .iter()
                .any(|entry| entry["kind"] == "trail-network" && entry["status"] == "satisfied")
        );
        Ok(())
    }

    #[test]
    fn build_accepts_osm_pbf_network_sources() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let pbf = tmp.path().join("osm-trails.osm.pbf");
        fs::write(&pbf, tiny_osm_pbf())?;

        init(project, "OSM PBF Build Test".to_owned(), None)?;
        build(project, &pbf)?;

        let graph = load_graph(project)?;
        assert_eq!(graph.edges.len(), 2);
        assert!(graph.edges.iter().any(|edge| {
            edge.attr
                .provenance
                .iter()
                .any(|p| p.source == "osm-pbf" && p.source_id.as_deref() == Some("10"))
        }));
        assert!(graph.edges.iter().any(|edge| {
            edge.attr.access == Access::Private
                && (edge.attr.road_exposure - 1.0).abs() <= f64::EPSILON
        }));

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        let candidate = manifest["candidates"]
            .as_array()
            .expect("candidates")
            .iter()
            .find(|candidate| candidate["path"].as_str() == Some(pbf.to_str().unwrap()))
            .expect("OSM PBF source candidate");
        assert_eq!(candidate["kind"], "trail-network");
        assert_eq!(candidate["adapter_id"], "osm-pbf-network");
        assert!(
            manifest["coverage"]
                .as_array()
                .expect("coverage")
                .iter()
                .any(|entry| entry["kind"] == "trail-network" && entry["status"] == "satisfied")
        );
        Ok(())
    }

    #[test]
    fn generation_date_materializes_access_from_baseline_snapshot() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");
        let closure = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/closure_overlay.geojson");

        init(project, "Date Access Test".to_owned(), None)?;
        build(project, &fixture)?;
        apply_access(
            project,
            std::slice::from_ref(&closure),
            Some("2026-05-15".parse().unwrap()),
            None,
        )?;
        assert!(
            load_graph(project)?
                .edges
                .iter()
                .any(|edge| edge.attr.access == Access::Closed)
        );

        generate(
            project,
            &GenerateOptions {
                solver: Some(SolverKind::Exact),
                date: Some("2026-07-15".parse().unwrap()),
                ..mini_generate_options()
            },
        )?;

        let generated_graph = load_generated_graph(project)?;
        assert!(
            !generated_graph
                .edges
                .iter()
                .any(|edge| edge.attr.access == Access::Closed)
        );
        let manifest: Value = serde_json::from_str(&fs::read_to_string(
            project.join("routes/generated.manifest.json"),
        )?)?;
        assert_eq!(manifest["effective_config"]["planning_date"], "2026-07-15");
        assert!(
            manifest["artifacts"]
                .as_array()
                .is_some_and(|xs| { xs.iter().any(|x| x == "routes/generated.graph.json") })
        );

        Ok(())
    }

    #[test]
    fn generation_time_materializes_hourly_access_from_baseline_snapshot() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");
        let closure = project.join("hourly-closure.geojson");
        fs::write(
            &closure,
            r#"{
              "type": "FeatureCollection",
              "features": [{
                "type": "Feature",
                "properties": {
                  "id": "midday-reservation",
                  "source": "fixture-closure",
                  "access": "closed",
                  "active_from": "2026-07-01",
                  "active_to": "2026-07-31",
                  "time_from": "08:00",
                  "time_to": "17:00"
                },
                "geometry": {
                  "type": "Polygon",
                  "coordinates": [[
                    [-105.0000, 40.0020],
                    [-104.9900, 40.0020],
                    [-104.9900, 40.0100],
                    [-105.0000, 40.0100],
                    [-105.0000, 40.0020]
                  ]]
                }
              }]
            }"#,
        )?;

        init(project, "Hourly Access Test".to_owned(), None)?;
        build(project, &fixture)?;
        apply_access(
            project,
            std::slice::from_ref(&closure),
            Some("2026-07-06".parse().unwrap()),
            Some("12:00".parse().unwrap()),
        )?;
        assert!(
            load_graph(project)?
                .edges
                .iter()
                .any(|edge| edge.attr.access == Access::Closed)
        );

        generate(
            project,
            &GenerateOptions {
                solver: Some(SolverKind::Exact),
                date: Some("2026-07-06".parse().unwrap()),
                time: Some("18:00".parse().unwrap()),
                ..mini_generate_options()
            },
        )?;

        let generated_graph = load_generated_graph(project)?;
        assert!(
            !generated_graph
                .edges
                .iter()
                .any(|edge| edge.attr.access == Access::Closed)
        );
        let manifest: Value = serde_json::from_str(&fs::read_to_string(
            project.join("routes/generated.manifest.json"),
        )?)?;
        assert_eq!(manifest["effective_config"]["planning_date"], "2026-07-06");
        assert_eq!(manifest["effective_config"]["planning_time"], "18:00");

        Ok(())
    }

    #[test]
    fn access_and_closure_sources_compose_from_one_baseline() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../trailgen-core/tests/fixtures");
        let network = fixture_dir.join("mini_network.geojson");
        let access = fixture_dir.join("access_overlay.geojson");
        let closure = fixture_dir.join("closure_overlay.geojson");

        init(project, "Access Composition Test".to_owned(), None)?;
        discover(project, None)?;
        build(project, &network)?;
        apply_access(
            project,
            &[access, closure],
            Some("2026-05-15".parse().unwrap()),
            None,
        )?;
        let graph = load_graph(project)?;
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.attr.access == Access::Restricted)
        );
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.attr.access == Access::Closed)
        );

        generate(
            project,
            &GenerateOptions {
                solver: Some(SolverKind::Exact),
                date: Some("2026-07-15".parse().unwrap()),
                max_restricted_access_fraction: Some(1.0),
                ..mini_generate_options()
            },
        )?;

        let generated_graph = load_generated_graph(project)?;
        assert!(
            generated_graph
                .edges
                .iter()
                .any(|edge| edge.attr.access == Access::Restricted)
        );
        assert!(
            !generated_graph
                .edges
                .iter()
                .any(|edge| edge.attr.access == Access::Closed)
        );
        let manifest: Value = serde_json::from_str(&fs::read_to_string(
            project.join("routes/generated.manifest.json"),
        )?)?;
        let coverage = manifest["source_manifest"]["coverage"]
            .as_array()
            .expect("coverage");
        assert!(
            coverage
                .iter()
                .any(|entry| { entry["kind"] == "access" && entry["status"] == "satisfied" })
        );
        assert!(
            coverage
                .iter()
                .any(|entry| { entry["kind"] == "closure" && entry["status"] == "satisfied" })
        );
        let stored_overlays: Value = serde_json::from_str(&fs::read_to_string(
            project.join("sources/access-overlays.json"),
        )?)?;
        assert_eq!(
            stored_overlays.as_array().expect("stored overlays").len(),
            2
        );

        Ok(())
    }

    #[test]
    fn calibration_write_updates_config_and_rerates_cached_graph() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");

        init(project, "Calibration Test".to_owned(), None)?;
        build(project, &fixture)?;
        generate(
            project,
            &GenerateOptions {
                count: 1,
                max_difficulty: Some(10_000.0),
                ..mini_generate_options()
            },
        )?;

        let route_path = project.join("routes/candidate-1.gpx");
        let before_config = load_config(project)?;
        let before_weight = before_config.difficulty.distance_per_km;
        let before_graph = load_graph(project)?;
        let before_route = snapped_route(
            &before_graph,
            &route_path,
            &before_config.constraints,
            "rated",
            before_config.max_route_snap_m,
        )?;
        let target = before_route.metrics.difficulty * 1.25;
        calibrate(
            project,
            &route_path,
            target,
            CalibrationFamily::All,
            None,
            true,
        )?;
        let config = load_config(project)?;
        assert!(config.difficulty.distance_per_km > before_weight);

        let graph = load_graph(project)?;
        let rated = snapped_route(
            &graph,
            &route_path,
            &config.constraints,
            "rated",
            config.max_route_snap_m,
        )?;
        assert!(
            (rated.metrics.difficulty - target).abs() <= 1.0e-6,
            "rated {} target {}",
            rated.metrics.difficulty,
            target
        );

        Ok(())
    }

    #[test]
    fn calibration_rejects_zero_contribution_family() {
        let error = calibrate_weights(
            DifficultyWeights::default(),
            DifficultyBreakdown {
                distance: 10.0,
                ..DifficultyBreakdown::default()
            },
            20.0,
            CalibrationFamily::Grade,
        )
        .expect_err("zero selected contribution should fail");

        assert!(format!("{error:#}").contains("zero contribution"));
    }

    #[test]
    fn source_verification_detects_drift() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let source = project.join("sources/network.geojson");

        init(project, "Verification Test".to_owned(), None)?;
        fs::write(
            &source,
            include_str!("../../trailgen-core/tests/fixtures/mini_network.geojson"),
        )?;
        build(project, &source)?;
        verify_sources(project)?;

        fs::write(
            &source,
            format!(
                "{}\n",
                include_str!("../../trailgen-core/tests/fixtures/mini_network.geojson")
            ),
        )?;
        let error = verify_sources(project).expect_err("source drift should fail verification");
        assert!(format!("{error:#}").contains("source verification failed"));
        assert!(format!("{error:#}").contains("drifted"));

        Ok(())
    }

    #[test]
    fn assemble_realizes_discovered_manifest_into_attributed_graph() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let sources = project.join("sources");
        let seed_sources = sources.join("seeds");
        fs::create_dir_all(&seed_sources)?;
        init(&project, "Assemble Test".to_owned(), None)?;
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../trailgen-core/tests/fixtures");
        fs::copy(
            fixtures.join("mini_network.geojson"),
            sources.join("network.geojson"),
        )?;
        fs::copy(fixtures.join("mini_dem.asc"), sources.join("dem.asc"))?;
        fs::copy(
            fixtures.join("terrain_overlay.geojson"),
            sources.join("terrain.geojson"),
        )?;
        fs::copy(
            fixtures.join("context_overlay.geojson"),
            sources.join("roads.geojson"),
        )?;
        fs::copy(
            fixtures.join("access_overlay.geojson"),
            sources.join("access.geojson"),
        )?;
        fs::copy(
            fixtures.join("closure_overlay.geojson"),
            sources.join("closure.geojson"),
        )?;
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../demo/mini-loop/routes/candidate-1.gpx"),
            seed_sources.join("completed.gpx"),
        )?;

        discover(&project, None)?;
        assemble_sources(&project, None, None, 0.82)?;

        let graph = load_graph(&project)?;
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| !edge.attr.elevation_provenance.is_empty())
        );
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.attr.terrain == Terrain::Talus)
        );
        assert!(graph.edges.iter().any(|edge| {
            edge.attr
                .crossings
                .iter()
                .any(|crossing| crossing.kind == CrossingKind::Road)
        }));
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.attr.access == Access::Closed)
        );
        assert!(project.join("seeds/seeds.json").exists());

        Ok(())
    }

    #[test]
    fn assemble_preserves_archived_seed_identity() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");
        let route = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../demo/mini-loop/routes/candidate-1.gpx");

        init(&project, "Seed Idempotence Test".to_owned(), None)?;
        build(&project, &fixture)?;
        import_seed(&project, &route, Some("Known Good Loop".to_owned()), None)?;

        let seeds_once = load_seeds(&project)?;
        assert_eq!(seeds_once.len(), 1);
        assert_eq!(seeds_once[0].name, "Known Good Loop");
        let seed_key = artifact_key("Known Good Loop");
        assert!(
            project
                .join(format!("seeds/imports/{seed_key}.gpx"))
                .exists()
        );
        assert!(!project.join("seeds/imports/candidate-1.gpx").exists());

        assemble_sources(&project, None, None, 0.82)?;

        let seeds_twice = load_seeds(&project)?;
        assert_eq!(seeds_twice.len(), 1);
        assert_eq!(seeds_twice[0].name, "Known Good Loop");
        assert!(
            load_graph(&project)?
                .edges
                .iter()
                .any(|edge| edge.attr.seed_count > 0)
        );
        assert!(
            project
                .join(format!("seeds/imports/{seed_key}.gpx"))
                .exists()
        );
        assert!(!project.join("seeds/imports/candidate-1.gpx").exists());
        assert!(!project.join("seeds/candidate-1.json").exists());

        Ok(())
    }

    #[test]
    fn assemble_applies_multiple_dems_as_one_mosaic() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let sources = project.join("sources");
        fs::create_dir_all(&sources)?;
        init(&project, "Mosaic Test".to_owned(), None)?;
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../trailgen-core/tests/fixtures");
        fs::copy(
            fixtures.join("mini_network.geojson"),
            sources.join("network.geojson"),
        )?;
        fs::write(
            sources.join("dem-west.asc"),
            "ncols 3\nnrows 3\nxllcorner -105.02\nyllcorner 39.99\ncellsize 0.01\nNODATA_value -9999\n1700 1710 1720\n1600 1610 1620\n1500 1510 1520\n",
        )?;
        fs::write(
            sources.join("dem-east.asc"),
            "ncols 3\nnrows 3\nxllcorner -104.99\nyllcorner 39.99\ncellsize 0.01\nNODATA_value -9999\n1900 1910 1920\n1800 1810 1820\n1700 1710 1720\n",
        )?;

        discover(&project, None)?;
        assemble_sources(&project, None, None, 0.83)?;

        let graph = load_graph(&project)?;
        let source_ids = graph
            .edges
            .iter()
            .flat_map(|edge| edge.attr.elevation_provenance.iter())
            .filter_map(|p| p.source_id.as_deref())
            .collect::<BTreeSet<_>>();
        assert!(source_ids.contains("dem-west.asc"));
        assert!(source_ids.contains("dem-east.asc"));
        assert!(project.join("sources/elevation-mosaic.json").exists());

        Ok(())
    }

    #[test]
    fn cache_source_copies_bytes_under_sources_and_records_origin() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let source = tmp.path().join("raw-network.geojson");
        fs::write(
            &source,
            include_str!("../../trailgen-core/tests/fixtures/mini_network.geojson"),
        )?;

        init(&project, "Cache Test".to_owned(), None)?;
        cache_source(
            &project,
            source.to_str().expect("utf-8 temp path"),
            Some(Path::new("cached/network.geojson")),
            None,
            None,
        )?;
        verify_sources(&project)?;

        let cached = project.join("sources/cached/network.geojson");
        assert_eq!(fs::read(&cached)?, fs::read(&source)?);
        assert!(cached_source_path(&project, "x", Some(Path::new("../x"))).is_err());

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        let candidate = manifest["candidates"]
            .as_array()
            .expect("candidates")
            .iter()
            .find(|candidate| {
                candidate["path"]
                    .as_str()
                    .is_some_and(|p| p.ends_with("cached/network.geojson"))
            })
            .expect("cached candidate");
        assert_eq!(candidate["kind"], "trail-network");
        assert_eq!(candidate["adapter_id"], "geojson-network");
        assert_eq!(candidate["origin"], source.display().to_string());
        assert!(
            candidate["fingerprint"]["sha256"]
                .as_str()
                .is_some_and(|hash| hash.len() == 64)
        );

        Ok(())
    }

    #[test]
    fn cache_source_requires_adapter_for_ambiguous_kind() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let source = tmp.path().join("raw-route");
        fs::write(&source, "-105.0,40.0,1600\n-105.0,40.01,1700\n")?;

        init(&project, "Ambiguous Cache Test".to_owned(), None)?;
        let error = cache_source(
            &project,
            source.to_str().expect("utf-8 temp path"),
            Some(Path::new("cached/route")),
            Some(SourceKind::SeedRoute),
            None,
        )
        .expect_err("ambiguous seed route adapter should fail");
        assert!(format!("{error:#}").contains("pass --adapter"));
        assert!(!project.join("sources/cached/route").exists());

        cache_source(
            &project,
            source.to_str().expect("utf-8 temp path"),
            Some(Path::new("cached/route")),
            Some(SourceKind::SeedRoute),
            Some("csv-route"),
        )?;
        verify_sources(&project)?;

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        assert!(
            manifest["candidates"]
                .as_array()
                .expect("candidates")
                .iter()
                .any(|candidate| candidate["adapter_id"] == "csv-route")
        );

        Ok(())
    }

    #[test]
    fn shapefile_source_verification_covers_dbf_sidecar() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let source = tmp.path().join("trails.shp");
        write_test_shapefile(&source, "trail-1");

        init(&project, "Shapefile Cache Test".to_owned(), None)?;
        cache_source(
            &project,
            source.to_str().expect("utf-8 temp path"),
            Some(Path::new("cached/trails.shp")),
            None,
            None,
        )?;
        verify_sources(&project)?;

        let cached = project.join("sources/cached/trails.shp");
        assert!(cached.exists());
        assert!(cached.with_extension("dbf").exists());
        assert!(cached.with_extension("shx").exists());

        write_test_shapefile(&cached, "trail-2");
        let error = verify_sources(&project).expect_err("DBF drift should fail verification");
        assert!(format!("{error:#}").contains("drifted"));
        Ok(())
    }

    #[test]
    fn cache_source_extracts_zipped_shapefile_bundle() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let source = tmp.path().join("agency-roads.shp");
        let archive = tmp.path().join("agency-roads.zip");
        write_test_shapefile(&source, "road-1");
        fs::write(source.with_extension("prj"), WGS84_PRJ)?;
        write_shapefile_zip(&archive, &source)?;

        init(&project, "Zip Shapefile Cache Test".to_owned(), None)?;
        cache_source(
            &project,
            archive.to_str().expect("utf-8 temp path"),
            Some(Path::new("cached/roads.shp")),
            Some(SourceKind::Road),
            Some("shapefile-road-context"),
        )?;
        verify_sources(&project)?;

        let cached = project.join("sources/cached/roads.shp");
        assert!(cached.exists());
        assert!(cached.with_extension("dbf").exists());
        assert!(cached.with_extension("shx").exists());
        assert!(cached.with_extension("prj").exists());

        fs::write(cached.with_extension("prj"), UTM_PRJ)?;
        let error = verify_sources(&project).expect_err("PRJ drift should fail verification");
        assert!(format!("{error:#}").contains("drifted"));
        Ok(())
    }

    #[test]
    fn cache_source_extracts_requested_zipped_geojson_member() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let archive = tmp.path().join("agency-trails.zip");
        write_zip_members(
            &archive,
            &[(
                "exports/network.geojson",
                include_bytes!("../../trailgen-core/tests/fixtures/mini_network.geojson"),
            )],
        )?;

        init(&project, "Zip GeoJSON Cache Test".to_owned(), None)?;
        cache_source(
            &project,
            archive.to_str().expect("utf-8 temp path"),
            Some(Path::new("cached/network.geojson")),
            None,
            None,
        )?;
        verify_sources(&project)?;

        let cached = project.join("sources/cached/network.geojson");
        assert_eq!(
            fs::read_to_string(cached)?,
            include_str!("../../trailgen-core/tests/fixtures/mini_network.geojson")
        );
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        assert!(
            manifest["candidates"]
                .as_array()
                .expect("candidates")
                .iter()
                .any(|candidate| {
                    candidate["path"]
                        .as_str()
                        .is_some_and(|path| path.ends_with("cached/network.geojson"))
                        && candidate["adapter_id"] == "geojson-network"
                })
        );

        Ok(())
    }

    #[test]
    fn cache_source_rejects_ambiguous_zipped_non_shapefile_members() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let archive = tmp.path().join("agency-trails.zip");
        write_zip_members(
            &archive,
            &[
                (
                    "north.geojson",
                    include_bytes!("../../trailgen-core/tests/fixtures/mini_network.geojson"),
                ),
                (
                    "south.geojson",
                    include_bytes!("../../trailgen-core/tests/fixtures/terrain_overlay.geojson"),
                ),
            ],
        )?;

        init(&project, "Ambiguous Zip Cache Test".to_owned(), None)?;
        let error = cache_source(
            &project,
            archive.to_str().expect("utf-8 temp path"),
            Some(Path::new("cached/network.geojson")),
            None,
            None,
        )
        .expect_err("ambiguous geojson archive should fail");
        assert!(format!("{error:#}").contains("multiple .geojson members"));
        assert!(format!("{error:#}").contains("north.geojson"));
        assert!(format!("{error:#}").contains("south.geojson"));

        cache_source(
            &project,
            archive.to_str().expect("utf-8 temp path"),
            Some(Path::new("cached/north.geojson")),
            None,
            None,
        )?;
        verify_sources(&project)?;

        Ok(())
    }

    #[test]
    fn geotiff_elevation_application_registers_source() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");
        let dem = tmp.path().join("dem.tif");
        write_cli_geotiff_dem(&dem);

        init(&project, "GeoTIFF Elevation Test".to_owned(), None)?;
        build(&project, &fixture)?;
        apply_elevation(&project, &dem, 0.84)?;

        let metadata: Value = serde_json::from_str(&fs::read_to_string(
            project.join("sources/elevation-geotiff.json"),
        )?)?;
        assert_eq!(metadata["width"], 3);
        assert_eq!(metadata["height"], 3);
        assert_eq!(metadata["confidence"], 0.84);
        assert!(metadata.get("values").is_none());
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        assert!(
            manifest["candidates"]
                .as_array()
                .expect("candidates")
                .iter()
                .any(|candidate| {
                    candidate["kind"] == "elevation"
                        && candidate["adapter_id"] == "geotiff-elevation"
                        && candidate["fingerprint"]["sha256"]
                            .as_str()
                            .is_some_and(|hash| hash.len() == 64)
                })
        );
        Ok(())
    }

    #[test]
    fn vrt_elevation_application_hashes_referenced_source() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");
        let dem = tmp.path().join("dem.tif");
        let vrt = tmp.path().join("dem.vrt");
        write_cli_geotiff_dem(&dem);
        write_cli_vrt_dem(&vrt, "dem.tif");

        init(&project, "VRT Elevation Test".to_owned(), None)?;
        build(&project, &fixture)?;
        apply_elevation(&project, &vrt, 0.82)?;

        let metadata: Value = serde_json::from_str(&fs::read_to_string(
            project.join("sources/elevation-vrt.json"),
        )?)?;
        assert_eq!(metadata["width"], 3);
        assert_eq!(metadata["height"], 3);
        assert_eq!(metadata["confidence"], 0.82);
        assert!(
            metadata["source_filename"]
                .as_str()
                .is_some_and(|source| source.ends_with("dem.tif"))
        );
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        let candidate = manifest["candidates"]
            .as_array()
            .expect("candidates")
            .iter()
            .find(|candidate| candidate["adapter_id"] == "vrt-elevation")
            .expect("VRT elevation candidate");
        assert_eq!(candidate["kind"], "elevation");
        assert!(
            candidate["fingerprint"]["bytes"]
                .as_u64()
                .is_some_and(|bytes| bytes > fs::metadata(&vrt).unwrap().len())
        );
        verify_sources(&project)?;
        write_cli_geotiff_dem(&dem);
        fs::write(
            &dem,
            fs::read(&dem)?
                .into_iter()
                .chain([0_u8])
                .collect::<Vec<_>>(),
        )?;
        let error = verify_sources(&project).expect_err("VRT source drift should fail");
        assert!(format!("{error:#}").contains("drifted"));
        Ok(())
    }

    #[test]
    fn shapefile_apply_commands_register_true_adapter_ids() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");
        let closure = tmp.path().join("raptor-closure.shp");
        let terrain = tmp.path().join("talus-terrain.shp");
        let roads = tmp.path().join("roads.shp");
        write_status_polygon_shapefile(&closure, "closed");
        write_terrain_polygon_shapefile(&terrain);
        write_context_line_shapefile(&roads, "road");

        init(&project, "Shapefile Apply Test".to_owned(), None)?;
        build(&project, &fixture)?;
        apply_access(&project, std::slice::from_ref(&closure), None, None)?;
        apply_terrain(&project, &terrain)?;
        apply_context(&project, &roads)?;

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        let candidates = manifest["candidates"].as_array().expect("candidates");
        assert!(candidates.iter().any(|candidate| {
            candidate["kind"] == "closure" && candidate["adapter_id"] == "shapefile-closure-layer"
        }));
        assert!(candidates.iter().any(|candidate| {
            candidate["kind"] == "terrain" && candidate["adapter_id"] == "shapefile-terrain-overlay"
        }));
        assert!(candidates.iter().any(|candidate| {
            candidate["kind"] == "road" && candidate["adapter_id"] == "shapefile-road-context"
        }));
        Ok(())
    }

    #[test]
    fn osm_pbf_context_registers_road_and_hydrology_sources() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let network = tmp.path().join("crossing-trail.geojson");
        let context = tmp.path().join("roads-streams.osm.pbf");
        fs::write(
            &network,
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"id":"trail","terrain":"trail"},"geometry":{"type":"LineString","coordinates":[[-105.01,40.005],[-104.98,40.005]]}}]}"#,
        )?;
        fs::write(&context, tiny_osm_pbf())?;

        init(&project, "OSM PBF Context Test".to_owned(), None)?;
        build(&project, &network)?;
        apply_context(&project, &context)?;

        let graph = load_graph(&project)?;
        assert!(graph.edges.iter().any(|edge| {
            edge.attr
                .crossings
                .iter()
                .any(|crossing| crossing.kind == CrossingKind::Road)
        }));
        assert!(graph.edges.iter().any(|edge| {
            edge.attr
                .crossings
                .iter()
                .any(|crossing| crossing.kind == CrossingKind::Water)
        }));

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        let candidates = manifest["candidates"].as_array().expect("candidates");
        assert!(candidates.iter().any(|candidate| {
            candidate["kind"] == "road" && candidate["adapter_id"] == "osm-road-context"
        }));
        assert!(candidates.iter().any(|candidate| {
            candidate["kind"] == "hydrology" && candidate["adapter_id"] == "osm-hydrology-context"
        }));
        Ok(())
    }

    fn write_test_shapefile(path: &Path, id: &str) {
        let table = ::shapefile::dbase::TableWriterBuilder::new()
            .add_character_field("id".try_into().unwrap(), 32)
            .add_character_field("terrain".try_into().unwrap(), 16);
        let mut writer = ::shapefile::Writer::from_path(path, table).unwrap();
        let line = ::shapefile::Polyline::new(vec![
            ::shapefile::Point::new(0.0, 0.0),
            ::shapefile::Point::new(0.01, 0.0),
        ]);
        let mut record = ::shapefile::dbase::Record::default();
        record.insert("id".to_owned(), id.to_owned().into());
        record.insert("terrain".to_owned(), "trail".to_owned().into());
        writer.write_shape_and_record(&line, &record).unwrap();
    }

    fn write_shapefile_zip(archive: &Path, source: &Path) -> Result<()> {
        use std::io::Write as _;
        let file = fs::File::create(archive)?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        let stem = source.file_stem().and_then(|x| x.to_str()).expect("stem");
        for ext in ["shp", "dbf", "shx", "prj"] {
            let path = source.with_extension(ext);
            zip.start_file(format!("nested/{stem}.{ext}"), options)?;
            zip.write_all(&fs::read(path)?)?;
        }
        zip.finish()?;
        Ok(())
    }

    fn write_zip_members(archive: &Path, members: &[(&str, &[u8])]) -> Result<()> {
        use std::io::Write as _;
        let file = fs::File::create(archive)?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in members {
            zip.start_file(*name, options)?;
            zip.write_all(bytes)?;
        }
        zip.finish()?;
        Ok(())
    }

    fn tiny_overpass_osm() -> &'static str {
        r#"<osm version="0.6" generator="adequate-trailgen-test">
  <node id="1" lat="40.0000" lon="-105.0000"/>
  <node id="2" lat="40.0010" lon="-105.0000"/>
  <node id="3" lat="40.0005" lon="-105.0010"/>
  <node id="4" lat="40.0005" lon="-104.9990"/>
  <node id="5" lat="40.0020" lon="-105.0010"/>
  <node id="6" lat="40.0020" lon="-104.9990"/>
  <way id="10">
    <nd ref="1"/>
    <nd ref="2"/>
    <tag k="highway" v="path"/>
    <tag k="name" v="Trail One"/>
  </way>
  <way id="11">
    <nd ref="3"/>
    <nd ref="4"/>
    <tag k="highway" v="service"/>
    <tag k="name" v="Road One"/>
  </way>
  <way id="12">
    <nd ref="5"/>
    <nd ref="6"/>
    <tag k="waterway" v="stream"/>
    <tag k="name" v="Stream One"/>
  </way>
  <relation id="20">
    <member type="way" ref="10" role=""/>
    <tag k="type" v="route"/>
    <tag k="route" v="hiking"/>
    <tag k="name" v="Trail One Route"/>
  </relation>
</osm>"#
    }

    fn empty_overpass_osm() -> &'static str {
        r#"<osm version="0.6" generator="adequate-trailgen-test"></osm>"#
    }

    fn assert_acquired_classes(
        mut actual: Vec<OsmAcquiredClass>,
        expected: &[(SourceKind, &'static str, usize)],
    ) {
        actual.sort_by_key(|class| (class.kind, class.adapter_id));
        let actual = actual
            .into_iter()
            .map(|class| (class.kind, class.adapter_id, class.count))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    fn tiny_osm_pbf() -> Vec<u8> {
        use osmpbfreader::osmformat::{PrimitiveBlock, PrimitiveGroup, StringTable};

        let mut table = StringTable::new();
        table.s = [
            "",
            "highway",
            "path",
            "surface",
            "gravel",
            "oneway:foot",
            "yes",
            "access",
            "private",
            "track",
            "motorway",
            "waterway",
            "stream",
        ]
        .into_iter()
        .map(|s| s.as_bytes().to_vec())
        .collect();

        let mut group = PrimitiveGroup::new();
        group.nodes = vec![
            pbf_node(1, 400_000_000, -1_050_000_000),
            pbf_node(2, 400_000_000, -1_049_900_000),
            pbf_node(3, 400_100_000, -1_049_900_000),
            pbf_node(4, 400_100_000, -1_050_000_000),
        ];
        group.ways = vec![
            pbf_way(10, &[(1, 2), (3, 4), (5, 6)], &[1, 1]),
            pbf_way(11, &[(1, 9), (7, 8)], &[2, 1]),
            pbf_way(12, &[(1, 10)], &[3, 1]),
            pbf_way(13, &[(11, 12)], &[4, -3]),
        ];

        let mut block = PrimitiveBlock::new();
        block.stringtable = MessageField::some(table);
        block.primitivegroup.push(group);
        pbf_file_block("OSMData", block.write_to_bytes().unwrap())
    }

    fn pbf_node(id: i64, lat: i64, lon: i64) -> osmpbfreader::osmformat::Node {
        let mut node = osmpbfreader::osmformat::Node::new();
        node.set_id(id);
        node.set_lat(lat);
        node.set_lon(lon);
        node
    }

    fn pbf_way(id: i64, tags: &[(u32, u32)], refs: &[i64]) -> osmpbfreader::osmformat::Way {
        let mut way = osmpbfreader::osmformat::Way::new();
        way.set_id(id);
        way.keys = tags.iter().map(|(key, _)| *key).collect();
        way.vals = tags.iter().map(|(_, value)| *value).collect();
        way.refs = refs.to_vec();
        way
    }

    fn pbf_file_block(kind: &str, raw: Vec<u8>) -> Vec<u8> {
        use osmpbfreader::fileformat::{Blob, BlobHeader};

        let mut blob = Blob::new();
        let raw_len = raw.len();
        blob.set_raw(raw);
        blob.set_raw_size(i32::try_from(raw_len).unwrap());
        let blob = blob.write_to_bytes().unwrap();

        let mut header = BlobHeader::new();
        header.set_type(kind.to_owned());
        header.set_datasize(i32::try_from(blob.len()).unwrap());
        let header = header.write_to_bytes().unwrap();

        let mut out = Vec::new();
        out.extend(u32::try_from(header.len()).unwrap().to_be_bytes());
        out.extend(header);
        out.extend(blob);
        out
    }

    fn write_cli_geotiff_dem(path: &Path) {
        use tiff::encoder::{TiffEncoder, colortype};
        use tiff::tags::Tag;

        let file = fs::File::create(path).unwrap();
        let mut tiff = TiffEncoder::new(file).unwrap();
        let mut image = tiff.new_image::<colortype::GrayI16>(3, 3).unwrap();
        image
            .encoder()
            .write_tag(Tag::ModelPixelScaleTag, &[0.01_f64, 0.01, 0.0][..])
            .unwrap();
        image
            .encoder()
            .write_tag(
                Tag::ModelTiepointTag,
                &[0.0_f64, 0.0, 0.0, -105.01, 40.02, 0.0][..],
            )
            .unwrap();
        image
            .encoder()
            .write_tag(
                Tag::GeoKeyDirectoryTag,
                &[
                    1_u16, 1, 0, 3, 1024, 0, 1, 2, 2048, 0, 1, 4326, 2054, 0, 1, 9102,
                ][..],
            )
            .unwrap();
        image
            .write_data(&[
                1_500_i16, 1_510, 1_520, 1_590, 1_600, 1_610, 1_700, 1_710, 1_720,
            ])
            .unwrap();
    }

    fn write_cli_vrt_dem(path: &Path, source: &str) {
        fs::write(
            path,
            format!(
                r#"<VRTDataset rasterXSize="3" rasterYSize="3">
  <GeoTransform>-105.01, 0.01, 0.0, 40.02, 0.0, -0.01</GeoTransform>
  <VRTRasterBand dataType="Int16" band="1">
    <NoDataValue>-32768</NoDataValue>
    <SimpleSource>
      <SourceFilename relativeToVRT="1">{source}</SourceFilename>
      <SourceBand>1</SourceBand>
      <SrcRect xOff="0" yOff="0" xSize="3" ySize="3"/>
      <DstRect xOff="0" yOff="0" xSize="3" ySize="3"/>
    </SimpleSource>
  </VRTRasterBand>
</VRTDataset>"#
            ),
        )
        .unwrap();
    }

    fn write_status_polygon_shapefile(path: &Path, status: &str) {
        let table = ::shapefile::dbase::TableWriterBuilder::new()
            .add_character_field("name".try_into().unwrap(), 32)
            .add_character_field("status".try_into().unwrap(), 16);
        let mut writer = ::shapefile::Writer::from_path(path, table).unwrap();
        let mut record = ::shapefile::dbase::Record::default();
        record.insert("name".to_owned(), "closure-1".to_owned().into());
        record.insert("status".to_owned(), status.to_owned().into());
        writer
            .write_shape_and_record(&fixture_polygon(), &record)
            .unwrap();
    }

    fn write_terrain_polygon_shapefile(path: &Path) {
        let table = ::shapefile::dbase::TableWriterBuilder::new()
            .add_character_field("name".try_into().unwrap(), 32)
            .add_character_field("terrain".try_into().unwrap(), 16)
            .add_character_field("surface".try_into().unwrap(), 16);
        let mut writer = ::shapefile::Writer::from_path(path, table).unwrap();
        let mut record = ::shapefile::dbase::Record::default();
        record.insert("name".to_owned(), "talus-1".to_owned().into());
        record.insert("terrain".to_owned(), "talus".to_owned().into());
        record.insert("surface".to_owned(), "scree".to_owned().into());
        writer
            .write_shape_and_record(&fixture_polygon(), &record)
            .unwrap();
    }

    fn write_context_line_shapefile(path: &Path, kind: &str) {
        let table = ::shapefile::dbase::TableWriterBuilder::new()
            .add_character_field("name".try_into().unwrap(), 32)
            .add_character_field("kind".try_into().unwrap(), 16);
        let mut writer = ::shapefile::Writer::from_path(path, table).unwrap();
        let line = ::shapefile::Polyline::new(vec![
            ::shapefile::Point::new(-104.995, 39.999),
            ::shapefile::Point::new(-104.995, 40.006),
        ]);
        let mut record = ::shapefile::dbase::Record::default();
        record.insert("name".to_owned(), "road-1".to_owned().into());
        record.insert("kind".to_owned(), kind.to_owned().into());
        writer.write_shape_and_record(&line, &record).unwrap();
    }

    fn fixture_polygon() -> ::shapefile::Polygon {
        ::shapefile::Polygon::with_rings(vec![::shapefile::PolygonRing::Outer(vec![
            ::shapefile::Point::new(-105.001, 39.999),
            ::shapefile::Point::new(-104.990, 39.999),
            ::shapefile::Point::new(-104.990, 40.006),
            ::shapefile::Point::new(-105.001, 40.006),
            ::shapefile::Point::new(-105.001, 39.999),
        ])])
    }

    #[test]
    fn imported_seed_is_archived_before_generated_outputs_can_drift() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");

        init(project, "Seed Archive Test".to_owned(), None)?;
        build(project, &fixture)?;
        generate(project, &mini_generate_options())?;
        import_seed(
            project,
            &project.join("routes/candidate-1.gpx"),
            Some("Known Good Loop".to_owned()),
            None,
        )?;
        generate(project, &mini_generate_options())?;
        verify_sources(project)?;
        verify_generation(project)?;

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        let candidates = manifest["candidates"].as_array().expect("candidates");
        let seed_key = artifact_key("Known Good Loop");
        assert!(candidates.iter().any(|candidate| {
            candidate["path"]
                .as_str()
                .is_some_and(|path| path.contains(&format!("seeds/imports/{seed_key}.gpx")))
        }));
        assert!(!candidates.iter().any(|candidate| {
            candidate["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("routes/candidate-1.gpx"))
        }));
        let seeds: Value = serde_json::from_str(&fs::read_to_string(
            project.join(format!("seeds/{seed_key}.json")),
        )?)?;
        assert!(
            seeds["source_path"]
                .as_str()
                .is_some_and(|path| path.ends_with(&format!("seeds/imports/{seed_key}.gpx")))
        );
        assert!(
            seeds["original_source_path"]
                .as_str()
                .is_some_and(|path| path.ends_with("routes/candidate-1.gpx"))
        );
        assert_eq!(seeds["metadata"]["title"], "candidate-1");
        let generation_manifest: Value = serde_json::from_str(&fs::read_to_string(
            project.join("routes/generated.manifest.json"),
        )?)?;
        assert_eq!(generation_manifest["seed_ledger"]["present"], true);
        assert_eq!(generation_manifest["seed_ledger"]["routes"], 1);
        assert!(
            generation_manifest["seed_ledger"]["fingerprint"]["sha256"]
                .as_str()
                .is_some_and(|hash| hash.len() == 64)
        );
        let generated_report = fs::read_to_string(project.join("reports/generated.md"))?;
        assert!(generated_report.contains("- seed ledger: 1 route(s)"));
        expect_seed_ledger_content_drift(project)?;

        Ok(())
    }

    #[test]
    fn rejected_seed_leaves_no_archive_or_ledger_debris() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path().join("project");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");
        let remote = tmp.path().join("remote.csv");
        fs::write(&remote, "longitude,latitude\n0,0\n0.01,0.01\n")?;

        init(&project, "Rejected Seed Test".to_owned(), None)?;
        build(&project, &fixture)?;
        let error = import_seed(&project, &remote, Some("remote".to_owned()), None)
            .expect_err("remote seed should not snap");
        assert!(format!("{error:#}").contains("seed route"));
        assert!(!project.join("seeds/imports/remote.csv").exists());
        assert!(!project.join("seeds/remote.json").exists());
        assert!(!project.join("seeds/seeds.json").exists());
        Ok(())
    }

    fn expect_seed_ledger_content_drift(project: &Path) -> Result<()> {
        let seed_ledger = project.join("seeds/seeds.json");
        let original = fs::read_to_string(&seed_ledger)?;
        fs::write(&seed_ledger, format!("{original}\n"))?;
        let error = verify_generation(project)
            .expect_err("seed ledger byte drift should fail verification");
        assert!(format!("{error:#}").contains("generated seed ledger verification failed"));
        fs::write(seed_ledger, original)?;
        verify_generation(project)?;
        Ok(())
    }
}
