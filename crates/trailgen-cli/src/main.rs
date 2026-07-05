use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use trailgen_core::alltrails::{AllTrailsBridge, ManualAllTrailsBridge};
use trailgen_core::io::{csv, geojson, gpx, kml, kmz, report};
use trailgen_core::source::{
    GeoBounds, SourceCandidate, SourceFingerprint, SourceKind, SourceManifest, adapter_registry,
    classify_path, discovery_recommendations, source_coverage,
};
use trailgen_core::{
    Access, ArcAsciiGrid, Coord, CrossingKind, DifficultyBreakdown, DifficultyWeights, EdgeId,
    EnrichmentConfig, GraphBuilder, LineString, LoopConstraints, LoopHunter, Provenance, Route,
    RouteMetrics, RouteShape, SearchParams, SeedRoute, SegmentDraft, Terrain, TrailGraph, VertexId,
    apply_access_overlays, apply_context_overlays, apply_terrain_overlays, enrich_graph,
    rank_routes, slug,
};

#[derive(Parser)]
#[command(name = "trailgen")]
#[command(about = "Design constrained long-hike loops over normalized trail graphs.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
#[allow(
    clippy::large_enum_variant,
    reason = "CLI command values are parsed once; boxing clap fields would only launder cold-start bytes into ceremony."
)]
enum Cmd {
    Init {
        project: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long, allow_hyphen_values = true, value_parser = parse_bounds)]
        bbox: Option<GeoBounds>,
    },
    Build {
        project: PathBuf,
        #[arg(long)]
        source: PathBuf,
    },
    Stats {
        project: PathBuf,
    },
    Discover {
        project: PathBuf,
        #[arg(long, allow_hyphen_values = true, value_parser = parse_bounds)]
        bbox: Option<GeoBounds>,
    },
    CacheSource {
        project: PathBuf,
        #[arg(long)]
        input: String,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long, value_parser = parse_source_kind)]
        kind: Option<SourceKind>,
        #[arg(long)]
        adapter: Option<String>,
    },
    VerifySources {
        project: PathBuf,
    },
    Generate {
        project: PathBuf,
        #[arg(long, allow_hyphen_values = true)]
        start: String,
        #[arg(long, default_value_t = 35.0)]
        min_km: f64,
        #[arg(long, default_value_t = 50.0)]
        max_km: f64,
        #[arg(long, default_value_t = 6)]
        count: usize,
        #[arg(long, default_value_t = 0)]
        seed: u64,
        #[arg(long)]
        min_difficulty: Option<f64>,
        #[arg(long)]
        max_difficulty: Option<f64>,
        #[arg(long)]
        min_ascent_m: Option<f64>,
        #[arg(long)]
        max_ascent_m: Option<f64>,
        #[arg(long)]
        min_descent_m: Option<f64>,
        #[arg(long)]
        max_descent_m: Option<f64>,
        #[arg(long)]
        max_road_fraction: Option<f64>,
        #[arg(long)]
        max_low_confidence_fraction: Option<f64>,
        #[arg(long)]
        max_restricted_access_fraction: Option<f64>,
        #[arg(long = "shape", value_parser = parse_shape)]
        shape: Vec<RouteShape>,
        #[arg(long)]
        max_repeated_edge_fraction: Option<f64>,
        #[arg(long = "forbid-terrain", value_parser = parse_terrain)]
        forbidden_terrain: Vec<Terrain>,
        #[arg(long = "min-terrain", value_parser = parse_terrain_fraction)]
        min_terrain: Vec<TerrainFraction>,
        #[arg(long = "max-terrain", value_parser = parse_terrain_fraction)]
        max_terrain: Vec<TerrainFraction>,
    },
    Export {
        project: PathBuf,
        #[arg(long)]
        route: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_enum)]
        format: ExportFormat,
    },
    Report {
        project: PathBuf,
        #[arg(long)]
        route: Option<String>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Rate {
        project: PathBuf,
        #[arg(long)]
        route: PathBuf,
    },
    Rerate {
        project: PathBuf,
    },
    Calibrate {
        project: PathBuf,
        #[arg(long)]
        route: PathBuf,
        #[arg(long)]
        target_difficulty: f64,
        #[arg(long, value_enum, default_value = "all")]
        family: CalibrationFamily,
        #[arg(long)]
        write: bool,
    },
    ImportSeed {
        project: PathBuf,
        #[arg(long)]
        route: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    ApplyAccess {
        project: PathBuf,
        #[arg(long)]
        source: PathBuf,
    },
    ApplyTerrain {
        project: PathBuf,
        #[arg(long)]
        source: PathBuf,
    },
    ApplyElevation {
        project: PathBuf,
        #[arg(long)]
        source: PathBuf,
        #[arg(long, default_value_t = 0.80)]
        confidence: f64,
    },
    ApplyContext {
        project: PathBuf,
        #[arg(long)]
        source: PathBuf,
    },
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
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ExportFormat {
    Gpx,
    Geojson,
    Kml,
    Kmz,
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

#[derive(Clone, Copy, Debug, PartialEq)]
struct TerrainFraction {
    terrain: Terrain,
    fraction: f64,
}

const fn default_snap_tolerance_m() -> f64 {
    8.0
}

#[allow(
    clippy::too_many_lines,
    reason = "Clap command dispatch is a single declarative cold path; splitting it would scatter the command algebra."
)]
fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Init {
            project,
            name,
            bbox,
        } => init(&project, name, bbox),
        Cmd::Build { project, source } => build(&project, &source),
        Cmd::Stats { project } => stats(&project),
        Cmd::Discover { project, bbox } => discover(&project, bbox),
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
        Cmd::VerifySources { project } => verify_sources(&project),
        Cmd::Generate {
            project,
            start,
            min_km,
            max_km,
            count,
            seed,
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
                min_terrain,
                max_terrain,
            },
        ),
        Cmd::Export {
            project,
            route,
            output,
            format,
        } => export_route(&project, &route, format, &output),
        Cmd::Report {
            project,
            route,
            output,
        } => report_generated(&project, route.as_deref(), output.as_deref()),
        Cmd::Rate { project, route } => rate(&project, &route),
        Cmd::Rerate { project } => rerate(&project),
        Cmd::Calibrate {
            project,
            route,
            target_difficulty,
            family,
            write,
        } => calibrate(&project, &route, target_difficulty, family, write),
        Cmd::ImportSeed {
            project,
            route,
            name,
        } => import_seed(&project, &route, name),
        Cmd::ApplyAccess { project, source } => apply_access(&project, &source),
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
                "\nMachine-readable bridge capabilities:\n{}",
                serde_json::to_string_pretty(&ManualAllTrailsBridge.capabilities())?
            );
            Ok(())
        }
    }
}

fn init(project: &Path, name: String, area: Option<GeoBounds>) -> Result<()> {
    fs::create_dir_all(project).with_context(|| format!("create {}", project.display()))?;
    for subdir in ["cache", "routes", "reports", "sources", "seeds"] {
        fs::create_dir_all(project.join(subdir))
            .with_context(|| format!("create {}", project.join(subdir).display()))?;
    }
    let config = ProjectConfig::new(name, area);
    fs::write(
        project.join("trailgen.toml"),
        toml::to_string_pretty(&config)?,
    )
    .with_context(|| "write trailgen.toml")?;
    println!("initialized {}", project.display());
    Ok(())
}

fn build(project: &Path, source: &Path) -> Result<()> {
    let config = load_config(project)?;
    let build_source = build_source(source)?;
    let graph = GraphBuilder {
        snap_tolerance_m: config.snap_tolerance_m,
        enrichment: config.enrichment,
        weights: config.difficulty,
    }
    .build(&build_source.drafts)
    .with_context(|| "build graph")?;
    write_json(project.join("cache/graph.json"), &graph)?;
    write_json(
        project.join("cache/graph.geojson"),
        &geojson::graph_to_geojson(&graph),
    )?;
    register_source_candidate_as(project, source, build_source.kind, build_source.adapter_id)?;
    println!(
        "built graph from {}: {} vertices, {} edges",
        build_source.adapter_id,
        graph.vertices.len(),
        graph.edges.len()
    );
    Ok(())
}

fn build_source(source: &Path) -> Result<BuildSource> {
    match source_ext(source).as_deref() {
        Some("geojson" | "json") => {
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
        Some("gpx" | "csv" | "kml" | "kmz") => Ok(BuildSource {
            drafts: vec![route_source_draft(source)?],
            kind: SourceKind::SeedRoute,
            adapter_id: route_adapter_id(source),
        }),
        Some(ext) => bail!(
            "unsupported build source extension {ext:?}; expected geojson, json, gpx, csv, kml, or kmz"
        ),
        None => bail!("build source has no extension"),
    }
}

fn route_source_draft(source: &Path) -> Result<SegmentDraft> {
    let line = load_route_line(source)?;
    Ok(route_source_draft_from_line(source, line))
}

fn route_source_draft_from_line(source: &Path, line: LineString) -> SegmentDraft {
    SegmentDraft {
        geometry: line,
        terrain: Terrain::Unknown,
        surface: None,
        access: Access::Unknown,
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
        Some("geojson" | "json") => "geojson-route",
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
    let mut road_m = 0.0;
    let mut low_conf_m = 0.0;
    let mut restricted_m = 0.0;
    let mut elevation_m = 0.0;
    let mut difficulty = 0.0;
    for edge in &graph.edges {
        let a = &edge.attr;
        *terrain_m.entry(a.terrain).or_default() += a.length_m;
        *access_m.entry(a.access).or_default() += a.length_m;
        road_m = a.length_m.mul_add(
            edge_road_pavement_exposure(a.terrain, a.road_exposure),
            road_m,
        );
        if a.confidence < 0.6 {
            low_conf_m += a.length_m;
        }
        if matches!(
            a.access,
            Access::Restricted | Access::Closed | Access::Private
        ) {
            restricted_m += a.length_m;
        }
        if edge_has_elevation(a) {
            elevation_m += a.length_m;
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
    let _ = writeln!(
        text,
        "elevation-attributed edge-km: {:.2} ({:.1}%)",
        elevation_m / 1_000.0,
        percent(elevation_m, total_m)
    );
    write_meter_mix(&mut text, "Terrain mix", &terrain_m, total_m);
    write_meter_mix(&mut text, "Access mix", &access_m, total_m);
    text.push_str("Crossings:\n");
    let crossings = crossing_totals(graph);
    if crossings.is_empty() {
        text.push_str("- none\n");
    } else {
        for (kind, count) in crossings {
            let _ = writeln!(text, "- {kind:?}: {count}");
        }
    }
    text
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

fn edge_has_elevation(a: &trailgen_core::EdgeAttr) -> bool {
    !a.elevation_provenance.is_empty()
        || a.ascent_m > 0.0
        || a.descent_m > 0.0
        || a.grade_abs_mean > 0.0
        || a.grade_abs_max > 0.0
        || a.grade_distribution.total_m() > 0.0
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

fn percent(part: f64, whole: f64) -> f64 {
    100.0 * part / whole.max(1.0)
}

fn discover(project: &Path, area: Option<GeoBounds>) -> Result<()> {
    let config = load_config(project)?;
    let area = area.or(config.area);
    fs::create_dir_all(project.join("sources"))?;
    let mut candidates = Vec::new();
    for path in source_files(&project.join("sources"))? {
        if let Some(mut candidate) = classify_path(&path) {
            candidate.fingerprint = Some(source_fingerprint(&path)?);
            candidates.push(candidate);
        }
    }
    let manifest = source_manifest(area, candidates);
    write_json(project.join("sources/manifest.json"), &manifest)?;
    println!(
        "discovered {} local candidate(s), recommended {} source class(es), evaluated {} source class(es); wrote {}",
        manifest.candidates.len(),
        manifest.recommendations.len(),
        manifest.coverage.len(),
        project.join("sources/manifest.json").display()
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
    let bytes = read_source_input(input)?;
    write_bytes(&path, &bytes)?;
    let fingerprint = source_fingerprint(&path)?;
    let mut candidate = cached_source_candidate(&path, kind, adapter, fingerprint)?;
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

fn verify_sources(project: &Path) -> Result<()> {
    let manifest = load_source_manifest(project)?.with_context(
        || "read sources/manifest.json; run `trailgen discover` or ingest sources first",
    )?;
    let mut failures = Vec::new();
    let mut checked = 0usize;
    for candidate in &manifest.candidates {
        let Some(expected) = &candidate.fingerprint else {
            failures.push(format!("{} lacks fingerprint", candidate.path));
            continue;
        };
        match source_fingerprint(Path::new(&candidate.path)) {
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
    println!("verified {checked} source candidate(s)");
    Ok(())
}

struct GenerateOptions {
    start: String,
    min_km: f64,
    max_km: f64,
    count: usize,
    seed: u64,
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
    min_terrain: Vec<TerrainFraction>,
    max_terrain: Vec<TerrainFraction>,
}

#[derive(Clone, Debug, Serialize)]
struct GenerationManifest {
    schema_version: u32,
    app_version: &'static str,
    solver: &'static str,
    random_seed: u64,
    requested_start: Coord,
    snapped_start_vertex: VertexId,
    effective_config: ProjectConfig,
    source_manifest: Option<SourceManifest>,
    graph: GraphManifest,
    routes: Vec<RouteManifestEntry>,
    artifacts: Vec<String>,
}

struct BuildSource {
    drafts: Vec<SegmentDraft>,
    kind: SourceKind,
    adapter_id: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct GraphManifest {
    vertices: usize,
    edges: usize,
    edge_km: f64,
    low_confidence_edges: usize,
    crossings: BTreeMap<CrossingKind, u32>,
    terrain_km: BTreeMap<Terrain, f64>,
}

#[derive(Clone, Debug, Serialize)]
struct RouteManifestEntry {
    name: String,
    start: VertexId,
    edges: Vec<EdgeId>,
    score: f64,
    metrics: RouteMetrics,
    satisfied: bool,
    violations: Vec<String>,
    rank: u32,
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
    let graph = load_graph(project)?;
    apply_generate_options(&mut config.constraints, options);
    let start_coord = parse_coord(&options.start)?;
    let start_vertex = graph
        .nearest_vertex(start_coord)
        .with_context(|| "graph has no vertices")?;
    let mut routes = LoopHunter {
        params: config.search,
    }
    .hunt(&graph, start_vertex, &config.constraints, options.count);
    routes.extend(
        load_seeds(project)?
            .iter()
            .filter_map(|seed| seed.as_route(&graph, &config.constraints)),
    );
    let mut seen = std::collections::BTreeSet::new();
    routes.retain(|route| seen.insert(route.edges.clone()));
    rank_routes(&mut routes, &config.constraints);
    routes.truncate(options.count);
    fs::create_dir_all(project.join("routes"))?;
    fs::create_dir_all(project.join("reports"))?;
    write_json(
        project.join("routes/generated.geojson"),
        &geojson::routes_to_geojson(&graph, &routes),
    )?;
    write_json(project.join("routes/generated.routes.json"), &routes)?;
    write_json(
        project.join("routes/generated.manifest.json"),
        &generation_manifest(
            project,
            options,
            &config,
            &graph,
            start_coord,
            start_vertex,
            &routes,
        )?,
    )?;
    for route in &routes {
        fs::write(
            project.join(format!("routes/{}.gpx", route.name)),
            gpx::route_to_gpx(&graph, route),
        )
        .with_context(|| format!("write GPX for {}", route.name))?;
        fs::write(
            project.join(format!("routes/{}.kml", route.name)),
            kml::route_to_kml(&graph, route),
        )
        .with_context(|| format!("write KML for {}", route.name))?;
        fs::write(
            project.join(format!("routes/{}.kmz", route.name)),
            kmz::route_to_kmz(&graph, route)?,
        )
        .with_context(|| format!("write KMZ for {}", route.name))?;
    }
    fs::write(
        project.join("reports/generated.md"),
        render_project_report(project, &graph, &routes, &config.constraints)?,
    )
    .with_context(|| "write generated report")?;
    println!("generated {} route(s)", routes.len());
    Ok(())
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

fn rate(project: &Path, route: &Path) -> Result<()> {
    let config = load_config(project)?;
    let graph = load_graph(project)?;
    let route = snapped_route(&graph, route, &config.constraints, "rated-route")?;
    println!(
        "{}",
        render_project_report(project, &graph, &[route], &config.constraints)?
    );
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
    write: bool,
) -> Result<()> {
    let mut config = load_config(project)?;
    let graph = load_graph(project)?;
    let route = snapped_route(&graph, route_path, &config.constraints, "calibration-route")?;
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
        println!("dry run; pass --write to update trailgen.toml and rerate cache/graph.json");
    }
    Ok(())
}

fn snapped_route(
    graph: &TrailGraph,
    route_path: &Path,
    constraints: &LoopConstraints,
    name: &str,
) -> Result<Route> {
    let line = load_route_line(route_path)?;
    let edges = graph.snap_line_edges(&line);
    if edges.is_empty() {
        bail!("route did not snap to any graph edges");
    }
    let start = graph
        .nearest_vertex(line.start())
        .with_context(|| "graph has no vertices")?;
    Ok(Route::from_edges(name, graph, start, edges, constraints))
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
) -> Result<()> {
    let graph = load_graph(project)?;
    let routes = load_generated_routes(project)?;
    let route = select_route(&routes, route_name)?;
    match format {
        ExportFormat::Gpx => write_bytes(output, gpx::route_to_gpx(&graph, route)),
        ExportFormat::Geojson => write_json(
            output,
            &geojson::routes_to_geojson(&graph, &[(*route).clone()]),
        ),
        ExportFormat::Kml => write_bytes(output, kml::route_to_kml(&graph, route)),
        ExportFormat::Kmz => write_bytes(output, kmz::route_to_kmz(&graph, route)?),
    }?;
    println!("exported {} to {}", route.name, output.display());
    Ok(())
}

fn report_generated(project: &Path, route_name: Option<&str>, output: Option<&Path>) -> Result<()> {
    let graph = load_graph(project)?;
    let routes = load_generated_routes(project)?;
    let selected;
    let report_routes = if let Some(route_name) = route_name {
        selected = vec![select_route(&routes, route_name)?.clone()];
        selected.as_slice()
    } else {
        routes.as_slice()
    };
    let constraints = load_generated_constraints(project)?.unwrap_or_else(|| {
        load_config(project)
            .map(|config| config.constraints)
            .unwrap_or_default()
    });
    let text = render_project_report(project, &graph, report_routes, &constraints)?;
    if let Some(output) = output {
        write_bytes(output, text)?;
        println!("wrote report {}", output.display());
    } else {
        println!("{text}");
    }
    Ok(())
}

fn render_project_report(
    project: &Path,
    graph: &TrailGraph,
    routes: &[Route],
    constraints: &LoopConstraints,
) -> Result<String> {
    let mut text = report::render(graph, routes);
    render_constraints_section(&mut text, constraints);
    render_source_manifest_section(&mut text, load_source_manifest(project)?.as_ref());
    Ok(text)
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
    if !manifest.coverage.is_empty() {
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

fn import_seed(project: &Path, route: &Path, name: Option<String>) -> Result<()> {
    let config = load_config(project)?;
    let mut graph = load_graph(project)?;
    let name = name.unwrap_or_else(|| {
        route
            .file_stem()
            .and_then(|x| x.to_str())
            .unwrap_or("seed-route")
            .to_owned()
    });
    let mut seeds = load_seeds(project)?;
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
    let archived_route = archive_seed_route(project, route, &name)?;
    let line = load_route_line(&archived_route)?;
    let source_format = archived_route
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("unknown")
        .to_ascii_lowercase();
    let mut seed = SeedRoute::snap(
        &graph,
        name,
        archived_route.display().to_string(),
        source_format,
        &line,
    );
    seed.original_source_path = Some(original_source_path);
    if seed.snapped_edges.is_empty() {
        bail!("seed route did not snap to any graph edges");
    }
    graph.apply_seed_hints(&seed);
    for edge in &mut graph.edges {
        config.difficulty.apply_edge(edge);
    }
    write_json(project.join("cache/graph.json"), &graph)?;
    write_json(
        project.join("cache/graph.geojson"),
        &geojson::graph_to_geojson(&graph),
    )?;

    seeds.retain(|old| old.name != seed.name);
    seeds.push(seed.clone());
    seeds.sort_by(|a, b| a.name.cmp(&b.name));
    fs::create_dir_all(project.join("seeds"))?;
    write_json(project.join("seeds/seeds.json"), &seeds)?;
    write_json(
        project.join(format!("seeds/{}.json", slug(&seed.name))),
        &seed,
    )?;
    register_source_candidate(project, &archived_route)?;
    if let Some(previous_source_path) =
        previous_source_path.filter(|path| path != &seed.source_path)
    {
        unregister_source_candidate_path(project, &previous_source_path)?;
    }
    println!(
        "imported seed {}: {} point(s), {} snapped edge(s), closed_loop={}",
        seed.name,
        seed.point_count,
        seed.snapped_edges.len(),
        seed.closed_loop
    );
    Ok(())
}

fn archive_seed_route(project: &Path, route: &Path, name: &str) -> Result<PathBuf> {
    let ext = route
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("route")
        .to_ascii_lowercase();
    let archived_route = project.join(format!("seeds/imports/{}.{}", slug(name), ext));
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

fn apply_access(project: &Path, source: &Path) -> Result<()> {
    let config = load_config(project)?;
    let mut graph = load_graph(project)?;
    let raw = fs::read_to_string(source).with_context(|| format!("read {}", source.display()))?;
    let overlays =
        geojson::access_overlays_from_str(&raw).with_context(|| "parse access overlay GeoJSON")?;
    let touched = apply_access_overlays(&mut graph, &overlays, config.difficulty);
    write_json(project.join("cache/graph.json"), &graph)?;
    write_json(
        project.join("cache/graph.geojson"),
        &geojson::graph_to_geojson(&graph),
    )?;
    fs::create_dir_all(project.join("sources"))?;
    write_json(project.join("sources/access-overlays.json"), &overlays)?;
    register_access_source(project, source)?;
    println!(
        "applied {} access overlay(s); touched {} edge(s)",
        overlays.len(),
        touched
    );
    Ok(())
}

fn apply_terrain(project: &Path, source: &Path) -> Result<()> {
    let config = load_config(project)?;
    let mut graph = load_graph(project)?;
    let raw = fs::read_to_string(source).with_context(|| format!("read {}", source.display()))?;
    let overlays = geojson::terrain_overlays_from_str(&raw)
        .with_context(|| "parse terrain overlay GeoJSON")?;
    let touched = apply_terrain_overlays(&mut graph, &overlays, config.difficulty);
    write_json(project.join("cache/graph.json"), &graph)?;
    write_json(
        project.join("cache/graph.geojson"),
        &geojson::graph_to_geojson(&graph),
    )?;
    fs::create_dir_all(project.join("sources"))?;
    write_json(project.join("sources/terrain-overlays.json"), &overlays)?;
    register_source_candidate_as(
        project,
        source,
        SourceKind::Terrain,
        "geojson-terrain-overlay",
    )?;
    println!(
        "applied {} terrain overlay(s); touched {} edge(s)",
        overlays.len(),
        touched
    );
    Ok(())
}

fn apply_elevation(project: &Path, source: &Path, confidence: f64) -> Result<()> {
    let config = load_config(project)?;
    let mut graph = load_graph(project)?;
    let raw = fs::read_to_string(source).with_context(|| format!("read {}", source.display()))?;
    let raster = ArcAsciiGrid::parse(
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
    .with_context(|| "parse elevation raster")?;
    enrich_graph(&mut graph, &raster, config.enrichment, config.difficulty)
        .with_context(|| "apply elevation raster")?;
    write_json(project.join("cache/graph.json"), &graph)?;
    write_json(
        project.join("cache/graph.geojson"),
        &geojson::graph_to_geojson(&graph),
    )?;
    fs::create_dir_all(project.join("sources"))?;
    write_json(project.join("sources/elevation-arc-ascii.json"), &raster)?;
    register_source_candidate_as(
        project,
        source,
        SourceKind::Elevation,
        "arc-ascii-elevation",
    )?;
    println!(
        "applied Arc/Info ASCII elevation grid {}x{} from {}",
        raster.ncols,
        raster.nrows,
        source.display()
    );
    Ok(())
}

fn apply_context(project: &Path, source: &Path) -> Result<()> {
    let config = load_config(project)?;
    let mut graph = load_graph(project)?;
    let raw = fs::read_to_string(source).with_context(|| format!("read {}", source.display()))?;
    let overlays =
        geojson::context_overlays_from_str(&raw).with_context(|| "parse context GeoJSON")?;
    let crossings = apply_context_overlays(&mut graph, &overlays, config.difficulty);
    write_json(project.join("cache/graph.json"), &graph)?;
    write_json(
        project.join("cache/graph.geojson"),
        &geojson::graph_to_geojson(&graph),
    )?;
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

fn load_config(project: &Path) -> Result<ProjectConfig> {
    let raw = fs::read_to_string(project.join("trailgen.toml"))
        .with_context(|| format!("read {}", project.join("trailgen.toml").display()))?;
    Ok(toml::from_str(&raw)?)
}

fn save_config(project: &Path, config: &ProjectConfig) -> Result<()> {
    fs::write(
        project.join("trailgen.toml"),
        toml::to_string_pretty(config)?,
    )
    .with_context(|| format!("write {}", project.join("trailgen.toml").display()))
}

fn load_graph(project: &Path) -> Result<TrailGraph> {
    let raw = fs::read_to_string(project.join("cache/graph.json"))
        .with_context(|| "read cache/graph.json; run `trailgen build` first")?;
    let mut graph: TrailGraph = serde_json::from_str(&raw)?;
    graph.rebuild_adjacency();
    Ok(graph)
}

fn save_graph(project: &Path, graph: &TrailGraph) -> Result<()> {
    write_json(project.join("cache/graph.json"), graph)?;
    write_json(
        project.join("cache/graph.geojson"),
        &geojson::graph_to_geojson(graph),
    )
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
    match source_ext(path).as_deref() {
        Some("kmz") => {
            let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
            Ok(kmz::route_line_from_bytes(&bytes)?)
        }
        Some("csv") => {
            let raw =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            Ok(csv::route_line_from_str(&raw)?)
        }
        Some("gpx") => {
            let raw =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            Ok(gpx::route_line_from_str(&raw)?)
        }
        Some("kml") => {
            let raw =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            Ok(kml::route_line_from_str(&raw)?)
        }
        Some("geojson" | "json") => {
            let raw =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            Ok(geojson::route_line_from_str(&raw)?)
        }
        Some(ext) => bail!("unsupported route extension: {ext}"),
        None => bail!("route file has no extension"),
    }
}

fn source_ext(path: &Path) -> Option<String> {
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
    let path = project.join("routes/generated.manifest.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let manifest = serde_json::from_str::<serde_json::Value>(&raw)?;
    manifest
        .get("effective_config")
        .and_then(|config| config.get("constraints"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(Into::into)
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

fn register_access_source(project: &Path, source: &Path) -> Result<()> {
    let (kind, adapter_id) = classify_path(source).map_or_else(
        || (SourceKind::Access, "geojson-access-overlay"),
        |candidate| match candidate.kind {
            SourceKind::Closure => (SourceKind::Closure, "geojson-closure-overlay"),
            _ => (SourceKind::Access, "geojson-access-overlay"),
        },
    );
    register_source_candidate_as(project, source, kind, adapter_id)
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
            "geojson-road-context",
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
            "geojson-hydrology-context",
            fingerprint,
        ));
    }
    register_source_candidates(project, candidates)
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

fn cached_source_candidate(
    source: &Path,
    kind: Option<SourceKind>,
    adapter: Option<&str>,
    fingerprint: SourceFingerprint,
) -> Result<SourceCandidate> {
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
        .unwrap_or_else(|| default_adapter_id(kind).to_owned());
    ensure_adapter_supports_kind(kind, &adapter_id)?;
    Ok(source_candidate(source, kind, &adapter_id, fingerprint))
}

const fn default_adapter_id(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::TrailNetwork => "geojson-network",
        SourceKind::SeedRoute => "gpx-route",
        SourceKind::Elevation => "arc-ascii-elevation",
        SourceKind::Terrain => "geojson-terrain-overlay",
        SourceKind::Access => "geojson-access-overlay",
        SourceKind::Closure => "geojson-closure-overlay",
        SourceKind::Road => "geojson-road-context",
        SourceKind::Hydrology => "geojson-hydrology-context",
    }
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
    manifest.candidates.retain(|old| {
        candidates
            .iter()
            .all(|candidate| candidate.path != old.path)
    });
    manifest.candidates.extend(candidates);
    manifest
        .candidates
        .sort_by(|a, b| (&a.path, a.kind, &a.adapter_id).cmp(&(&b.path, b.kind, &b.adapter_id)));
    refresh_source_coverage(&mut manifest);
    write_json(project.join("sources/manifest.json"), &manifest)
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
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let digest = Sha256::digest(&bytes);
    let mut sha256 = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut sha256, "{byte:02x}")?;
    }
    Ok(SourceFingerprint {
        bytes: bytes
            .len()
            .try_into()
            .expect("usize file length must fit in u64"),
        sha256,
    })
}

fn generation_manifest(
    project: &Path,
    options: &GenerateOptions,
    config: &ProjectConfig,
    graph: &TrailGraph,
    requested_start: Coord,
    snapped_start_vertex: VertexId,
    routes: &[Route],
) -> Result<GenerationManifest> {
    Ok(GenerationManifest {
        schema_version: 1,
        app_version: env!("CARGO_PKG_VERSION"),
        solver: "loop-hunter",
        random_seed: options.seed,
        requested_start,
        snapped_start_vertex,
        effective_config: config.clone(),
        source_manifest: load_source_manifest(project)?,
        graph: graph_manifest(graph),
        routes: routes.iter().map(route_manifest_entry).collect(),
        artifacts: generation_artifacts(routes),
    })
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
        low_confidence_edges: graph
            .edges
            .iter()
            .filter(|edge| edge.attr.confidence < 0.6)
            .count(),
        crossings: crossing_totals(graph),
        terrain_km,
    }
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
        rank: route.pareto_rank,
    }
}

fn generation_artifacts(routes: &[Route]) -> Vec<String> {
    let mut artifacts = vec![
        "routes/generated.geojson".to_owned(),
        "routes/generated.routes.json".to_owned(),
        "routes/generated.manifest.json".to_owned(),
        "reports/generated.md".to_owned(),
    ];
    for route in routes {
        artifacts.extend([
            format!("routes/{}.gpx", route.name),
            format!("routes/{}.kml", route.name),
            format!("routes/{}.kmz", route.name),
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
        let path = entry?.path();
        if path.is_dir() {
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
    if path
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
    {
        Ok(())
    } else {
        bail!("source cache output must be a relative path under project/sources")
    }
}

fn read_source_input(input: &str) -> Result<Vec<u8>> {
    if input.starts_with("http://") || input.starts_with("https://") {
        Ok(reqwest::blocking::get(input)
            .with_context(|| format!("GET {input}"))?
            .error_for_status()
            .with_context(|| format!("GET {input} returned an error status"))?
            .bytes()
            .with_context(|| format!("read response body from {input}"))?
            .to_vec())
    } else {
        let path = input.strip_prefix("file://").unwrap_or(input);
        fs::read(path).with_context(|| format!("read {path}"))
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
                | "access-overlays.json"
                | "terrain-overlays.json"
                | "context-overlays.json"
                | "elevation-arc-ascii.json"
                | "elevation-raster.json"
        )
    )
}

fn parse_coord(raw: &str) -> Result<Coord> {
    let Some((lon, lat)) = raw.split_once(',') else {
        bail!("coordinate must be lon,lat");
    };
    Ok(Coord::new(
        lon.trim().parse::<f64>()?,
        lat.trim().parse::<f64>()?,
    ))
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

fn parse_shape(raw: &str) -> Result<RouteShape, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "loop" => Ok(RouteShape::Loop),
        "figure-eight" | "figure8" | "fig8" => Ok(RouteShape::FigureEight),
        "out-and-back" | "outback" => Ok(RouteShape::OutAndBack),
        "open" => Ok(RouteShape::Open),
        _ => Err("expected loop, figure-eight, out-and-back, or open".to_owned()),
    }
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

fn write_json(path: impl AsRef<Path>, value: &impl Serialize) -> Result<()> {
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
    use serde_json::Value;

    #[test]
    fn generate_options_can_override_terrain_mix_constraints() {
        let mut constraints = LoopConstraints::default();
        apply_generate_options(
            &mut constraints,
            &GenerateOptions {
                start: "-105.0000,40.0000".to_owned(),
                min_km: 3.0,
                max_km: 8.0,
                count: 2,
                seed: 0,
                min_difficulty: None,
                max_difficulty: None,
                min_ascent_m: None,
                max_ascent_m: None,
                min_descent_m: None,
                max_descent_m: None,
                max_road_fraction: None,
                max_low_confidence_fraction: None,
                max_restricted_access_fraction: Some(0.25),
                shape: Vec::new(),
                max_repeated_edge_fraction: None,
                forbidden_terrain: vec![Terrain::Pavement, Terrain::Road],
                min_terrain: vec![TerrainFraction {
                    terrain: Terrain::Trail,
                    fraction: 0.65,
                }],
                max_terrain: vec![TerrainFraction {
                    terrain: Terrain::Talus,
                    fraction: 0.10,
                }],
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
        let graph = GraphBuilder::default().build(&[
            SegmentDraft {
                geometry: LineString::new(vec![
                    Coord::with_ele(0.0, 0.0, 1_000.0),
                    Coord::with_ele(0.01, 0.0, 1_030.0),
                ])
                .unwrap(),
                terrain: Terrain::Trail,
                surface: None,
                access: Access::Open,
                road_exposure: 0.0,
                confidence: 1.0,
                provenance: Provenance::fixture("trail"),
            },
            SegmentDraft {
                geometry: LineString::new(vec![Coord::new(0.01, 0.0), Coord::new(0.02, 0.0)])
                    .unwrap(),
                terrain: Terrain::Road,
                surface: Some("gravel".to_owned()),
                access: Access::Restricted,
                road_exposure: 0.25,
                confidence: 0.5,
                provenance: Provenance::fixture("restricted-road"),
            },
        ])?;

        let text = stats_text(&graph);
        assert!(text.contains("mean difficulty per km:"));
        assert!(text.contains("low-confidence edge-km:"));
        assert!(text.contains("restricted-access edge-km:"));
        assert!(text.contains("road/pavement edge-km:"));
        assert!(text.contains("elevation-attributed edge-km:"));
        assert!(text.contains("Terrain mix:"));
        assert!(text.contains("- Trail:"));
        assert!(text.contains("- Road:"));
        assert!(text.contains("Access mix:"));
        assert!(text.contains("- Restricted:"));
        assert!(text.contains("Crossings:"));
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
        generate(
            project,
            &GenerateOptions {
                start: "-105.0000,40.0000".to_owned(),
                min_km: 3.0,
                max_km: 8.0,
                count: 2,
                seed: 77,
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
                forbidden_terrain: vec![Terrain::Road],
                min_terrain: vec![TerrainFraction {
                    terrain: Terrain::Trail,
                    fraction: 0.50,
                }],
                max_terrain: vec![TerrainFraction {
                    terrain: Terrain::Pavement,
                    fraction: 0.05,
                }],
            },
        )?;

        let raw = fs::read_to_string(project.join("routes/generated.manifest.json"))?;
        let manifest: Value = serde_json::from_str(&raw)?;
        assert_eq!(manifest["app_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["solver"], "loop-hunter");
        assert_eq!(manifest["random_seed"], 77);
        assert_effective_constraints_manifest(&manifest);
        assert!(manifest["source_manifest"]["adapters"].as_array().is_some());
        assert_eq!(
            manifest["source_manifest"]["recommendations"][0]["area"]["west"],
            -105.1
        );
        assert_source_coverage_manifest(&manifest);
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
        assert!(manifest["graph"]["edges"].as_u64().is_some_and(|n| n > 0));
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
            manifest["artifacts"]
                .as_array()
                .is_some_and(|xs| xs.iter().any(|x| x == "routes/generated.manifest.json"))
        );

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
    }

    #[test]
    fn generated_routes_can_be_selected_exported_and_reported() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");

        init(project, "Export Test".to_owned(), None)?;
        build(project, &fixture)?;
        generate(
            project,
            &GenerateOptions {
                start: "-105.0000,40.0000".to_owned(),
                min_km: 3.0,
                max_km: 8.0,
                count: 2,
                seed: 0,
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
                min_terrain: Vec::new(),
                max_terrain: Vec::new(),
            },
        )?;

        let gpx = project.join("exports/candidate-1.gpx");
        let geojson = project.join("exports/candidate-1.geojson");
        let md = project.join("reports/candidate-1.md");
        export_route(project, "candidate-1", ExportFormat::Gpx, &gpx)?;
        export_route(project, "candidate-1", ExportFormat::Geojson, &geojson)?;
        report_generated(project, Some("candidate-1"), Some(&md))?;

        assert!(fs::read_to_string(gpx)?.contains("<gpx"));
        assert_eq!(
            serde_json::from_str::<Value>(&fs::read_to_string(geojson)?)?["type"],
            "FeatureCollection"
        );
        let report = fs::read_to_string(md)?;
        assert!(report.contains("candidate-1"));
        assert!(report.contains("## Constraint Envelope"));
        assert!(report.contains("distance: 3.00–8.00 km"));
        assert!(report.contains("scalar difficulty: 0.00–90.00"));
        assert!(report.contains("Difficulty decomposition"));
        assert!(report.contains("Access mix"));
        assert!(report.contains("restricted-access fraction"));
        assert!(report.contains("## Source Manifest"));
        assert!(report.contains("sha256 "));

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
        assert_eq!(candidate["adapter_id"], "gpx-route");
        assert!(
            manifest["coverage"]
                .as_array()
                .expect("coverage")
                .iter()
                .any(|entry| entry["kind"] == "trail-network" && entry["status"] == "missing")
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
                start: "-105.0000,40.0000".to_owned(),
                min_km: 3.0,
                max_km: 8.0,
                count: 1,
                seed: 0,
                min_difficulty: None,
                max_difficulty: Some(10_000.0),
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
                min_terrain: Vec::new(),
                max_terrain: Vec::new(),
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
        )?;
        let target = before_route.metrics.difficulty * 1.25;
        calibrate(project, &route_path, target, CalibrationFamily::All, true)?;
        let config = load_config(project)?;
        assert!(config.difficulty.distance_per_km > before_weight);

        let graph = load_graph(project)?;
        let rated = snapped_route(&graph, &route_path, &config.constraints, "rated")?;
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
    fn imported_seed_is_archived_before_generated_outputs_can_drift() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let project = tmp.path();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trailgen-core/tests/fixtures/mini_network.geojson");

        init(project, "Seed Archive Test".to_owned(), None)?;
        build(project, &fixture)?;
        generate(
            project,
            &GenerateOptions {
                start: "-105.0000,40.0000".to_owned(),
                min_km: 3.0,
                max_km: 8.0,
                count: 2,
                seed: 0,
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
                min_terrain: Vec::new(),
                max_terrain: Vec::new(),
            },
        )?;
        import_seed(
            project,
            &project.join("routes/candidate-1.gpx"),
            Some("Known Good Loop".to_owned()),
        )?;
        generate(
            project,
            &GenerateOptions {
                start: "-105.0000,40.0000".to_owned(),
                min_km: 3.0,
                max_km: 8.0,
                count: 2,
                seed: 0,
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
                min_terrain: Vec::new(),
                max_terrain: Vec::new(),
            },
        )?;
        verify_sources(project)?;

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(project.join("sources/manifest.json"))?)?;
        let candidates = manifest["candidates"].as_array().expect("candidates");
        assert!(candidates.iter().any(|candidate| {
            candidate["path"]
                .as_str()
                .is_some_and(|path| path.contains("seeds/imports/known-good-loop.gpx"))
        }));
        assert!(!candidates.iter().any(|candidate| {
            candidate["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("routes/candidate-1.gpx"))
        }));
        let seeds: Value = serde_json::from_str(&fs::read_to_string(
            project.join("seeds/known-good-loop.json"),
        )?)?;
        assert!(
            seeds["source_path"]
                .as_str()
                .is_some_and(|path| path.ends_with("seeds/imports/known-good-loop.gpx"))
        );
        assert!(
            seeds["original_source_path"]
                .as_str()
                .is_some_and(|path| path.ends_with("routes/candidate-1.gpx"))
        );

        Ok(())
    }
}
