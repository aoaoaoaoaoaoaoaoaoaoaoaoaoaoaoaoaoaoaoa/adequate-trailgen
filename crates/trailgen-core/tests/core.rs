use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

use protobuf::{Message, MessageField};
use trailgen_core::alltrails::{
    ALLTRAILS_POLICY_VERIFIED_ON, AllTrailsBridge, AllTrailsExchange, AllTrailsRequest,
    BridgeStatus, ManualAllTrailsBridge, RouteExchangeFormat, TrailgenExchangeAction,
};
use trailgen_core::io::{
    csv, geojson, gpx, json_route, kml, kmz, osm, report, shapefile as shp_io,
};
use trailgen_core::source::{
    SourceCoverageStatus, SourceKind, adapter_registry, classify_path, discovery_recommendations,
    source_coverage, summarize_source_coverage,
};
use trailgen_core::{
    Access, ArcAsciiGrid, CrossingKind, DailyTimeWindow, EdgeId, EdgeTravel, ElevationSampler,
    ExactLoopSolver, GeoTiffDem, LoopMilpFormulation, MilpSelectedArc, MonthDay, PlanningDate,
    PlanningMoment, PlanningTime, RasterCrs, Route, RouteMetrics, RouteShape, SearchParams,
    SeasonalWindow, SolverKind, VertexId, VrtDem, Weekday, WeekdaySet,
};
use trailgen_core::{
    Coord, DifficultyWeights, EnrichmentConfig, GraphBuilder, LineString, LoopConstraints,
    LoopHunter, OverlayGeometry, PlaneElevation, Provenance, SeedRoute, SegmentDraft, Terrain,
    TerrainMultipliers, TrailGraph, apply_access_overlays, apply_access_overlays_at,
    apply_context_overlays, apply_terrain_overlays, enrich_graph, rank_routes,
    route_edges_from_selected_arcs, route_edges_from_solution,
};

const WGS84_PRJ: &str = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["Degree",0.0174532925199433]]"#;
const WEB_MERCATOR_PRJ: &str = r#"PROJCS["WGS 84 / Pseudo-Mercator",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PROJECTION["Mercator_1SP"],UNIT["metre",1],AUTHORITY["EPSG","3857"]]"#;
const UTM_PRJ: &str = r#"PROJCS["WGS 84 / UTM zone 13N",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PROJECTION["Transverse_Mercator"],UNIT["metre",1],AUTHORITY["EPSG","32613"]]"#;
const NAD83_UTM_PRJ: &str = r#"PROJCS["NAD83 / UTM zone 13N",GEOGCS["NAD83",DATUM["North_American_Datum_1983",SPHEROID["GRS 1980",6378137,298.257222101]],PROJECTION["Transverse_Mercator"],UNIT["metre",1],AUTHORITY["EPSG","26913"]]"#;
const NAD83_LAMBERT_PRJ: &str = r#"PROJCS["NAD83 / Colorado Central",GEOGCS["NAD83",DATUM["North_American_Datum_1983",SPHEROID["GRS 1980",6378137,298.257222101]],PROJECTION["Lambert_Conformal_Conic"],UNIT["metre",1],AUTHORITY["EPSG","32154"]]"#;
const WEB_MERCATOR_0_01_LON_M: f64 = 1_113.194_907_932_735_8;

#[test]
fn builder_splits_crossing_lines() {
    let drafts = vec![
        SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.0, 0.5), Coord::new(1.0, 0.5)]).unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("a"),
        },
        SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.5, 0.0), Coord::new(0.5, 1.0)]).unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("b"),
        },
    ];
    let graph = GraphBuilder::default().build(&drafts).unwrap();
    assert_eq!(graph.edges.len(), 4);
    assert!(
        graph
            .vertices
            .iter()
            .any(|v| graph.adjacency[v.id.0].len() == 4)
    );
}

#[test]
fn graph_adjacency_respects_one_way_travel() {
    let graph = GraphBuilder::default()
        .build(&[SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)]).unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Forward,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("one-way"),
        }])
        .unwrap();
    let edge = graph.edges[0].id;
    let a = graph.edges[0].a;
    let b = graph.edges[0].b;

    assert_eq!(graph.adjacency[a.0], vec![edge]);
    assert!(graph.adjacency[b.0].is_empty());
    assert_eq!(graph.walk_edges(a, &[edge]), Some(b));
    assert_eq!(graph.walk_edges(b, &[edge]), None);
}

#[test]
fn builder_snaps_near_miss_endpoint_with_provenance() {
    let drafts = near_miss_drafts();
    let graph = GraphBuilder {
        snap_tolerance_m: 8.0,
        ..GraphBuilder::default()
    }
    .build(&drafts)
    .unwrap();

    let junction = graph
        .vertices
        .iter()
        .find(|v| {
            (v.coord.lon - 0.5).abs() <= 1.0e-9
                && v.coord.lat.abs() <= 1.0e-9
                && graph.adjacency[v.id.0].len() == 3
        })
        .expect("snapped T junction");
    assert_eq!(graph.adjacency[junction.id.0].len(), 3);
    assert!(graph.edges.iter().any(|edge| {
        edge.attr.confidence <= 0.74
            && edge.attr.provenance.iter().any(|p| {
                p.source == "graph-builder" && p.layer.as_deref() == Some("near-miss-snap")
            })
    }));
}

#[test]
fn builder_leaves_near_miss_disconnected_outside_tolerance() {
    let drafts = near_miss_drafts();
    let graph = GraphBuilder {
        snap_tolerance_m: 1.0,
        ..GraphBuilder::default()
    }
    .build(&drafts)
    .unwrap();

    assert_eq!(graph.edges.len(), 2);
    assert!(
        !graph
            .vertices
            .iter()
            .any(|v| graph.adjacency[v.id.0].len() == 3)
    );
    assert!(!graph.edges.iter().any(|edge| {
        edge.attr
            .provenance
            .iter()
            .any(|p| p.source == "graph-builder")
    }));
}

fn near_miss_drafts() -> Vec<SegmentDraft> {
    vec![
        SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0)]).unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("trunk"),
        },
        SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.5, 0.00005), Coord::new(0.5, 0.01)])
                .unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("spur"),
        },
    ]
}

#[test]
fn difficulty_penalizes_rough_uncertain_closed_edges() {
    let smooth = SegmentDraft {
        geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)]).unwrap(),
        terrain: Terrain::Trail,
        terrain_confidence: None,
        surface: Some("dirt".to_owned()),
        access: Access::Open,
        travel: EdgeTravel::Both,
        road_exposure: 0.0,
        confidence: 1.0,
        provenance: Provenance::fixture("smooth"),
    };
    let savage = SegmentDraft {
        terrain: Terrain::Scramble,
        terrain_confidence: None,
        surface: Some("dirt".to_owned()),
        access: Access::Closed,
        confidence: 0.25,
        provenance: Provenance::fixture("savage"),
        ..smooth.clone()
    };
    let uncertain = SegmentDraft {
        terrain: Terrain::Unknown,
        terrain_confidence: None,
        surface: Some("dirt".to_owned()),
        access: Access::Open,
        confidence: 0.9,
        provenance: Provenance::fixture("uncertain"),
        ..smooth.clone()
    };
    let graph = GraphBuilder {
        weights: DifficultyWeights::default(),
        ..GraphBuilder::default()
    }
    .build(&[smooth, savage, uncertain])
    .unwrap();
    assert!(graph.edges[1].attr.difficulty > graph.edges[0].attr.difficulty + 100.0);
    assert!(graph.edges.iter().all(|edge| {
        (edge.attr.difficulty - edge.attr.difficulty_breakdown.total()).abs() <= 1.0e-9
    }));
    assert!(graph.edges[1].attr.difficulty_breakdown.access > 900.0);
    assert_eq!(graph.edges[0].attr.surface.as_deref(), Some("dirt"));
    assert!(graph.edges[1].attr.difficulty_breakdown.technical > 0.0);
    assert!(
        graph.edges[1].attr.difficulty_breakdown.technical
            > graph.edges[0].attr.difficulty_breakdown.technical
    );
    assert!(graph.edges[2].attr.difficulty_breakdown.navigation > 0.0);
    assert!(
        graph.edges[2].attr.difficulty_breakdown.navigation
            > graph.edges[0].attr.difficulty_breakdown.navigation
    );
}

#[test]
fn terrain_multipliers_are_configurable_and_defaulted() {
    let mut weights = DifficultyWeights {
        terrain_multipliers: TerrainMultipliers {
            talus: 3.0,
            ..TerrainMultipliers::default()
        },
        ..DifficultyWeights::default()
    };
    let draft = SegmentDraft {
        geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)]).unwrap(),
        terrain: Terrain::Talus,
        terrain_confidence: None,
        surface: None,
        access: Access::Open,
        travel: EdgeTravel::Both,
        road_exposure: 0.0,
        confidence: 1.0,
        provenance: Provenance::fixture("talus"),
    };
    let graph = GraphBuilder {
        weights,
        ..GraphBuilder::default()
    }
    .build(&[draft])
    .unwrap();
    let edge = &graph.edges[0];
    assert!(edge.attr.difficulty_breakdown.terrain > edge.attr.difficulty_breakdown.distance);

    weights = serde_json::from_str::<DifficultyWeights>(r#"{"terrain_multipliers":{"talus":2.0}}"#)
        .unwrap();
    assert!((weights.terrain_multiplier(Terrain::Talus) - 2.0).abs() <= f64::EPSILON);
    assert!(
        (weights.terrain_multiplier(Terrain::Scramble)
            - DifficultyWeights::default().terrain_multiplier(Terrain::Scramble))
        .abs()
            <= f64::EPSILON
    );
}

#[test]
fn geojson_network_preserves_surface_tags() {
    let drafts = geojson::network_from_str(
        r#"{"type":"FeatureCollection","features":[{
            "type":"Feature",
            "properties":{"surface":"asphalt","source":"fixture","id":"surface-edge"},
            "geometry":{"type":"LineString","coordinates":[[0.0,0.0],[0.01,0.0]]}
        }]}"#,
    )
    .unwrap();
    let graph = GraphBuilder::default().build(&drafts).unwrap();
    let edge = &graph.edges[0];

    assert_eq!(edge.attr.surface.as_deref(), Some("asphalt"));
    assert_eq!(edge.attr.terrain, Terrain::Pavement);
    assert!((edge.attr.terrain_confidence - 0.82).abs() <= f64::EPSILON);
    assert!(geojson::graph_to_geojson(&graph)["features"][0]["properties"]["surface"] == "asphalt");
    let report = report::render(
        &graph,
        &[Route::from_edges(
            "surface",
            &graph,
            edge.a,
            vec![edge.id],
            &LoopConstraints {
                min_distance_m: 0.0,
                max_distance_m: 10_000.0,
                allowed_shapes: vec![RouteShape::Open],
                ..LoopConstraints::default()
            },
        )],
    );
    assert!(report.contains("surface asphalt"));
}

#[test]
fn geojson_network_normalizes_one_way_tags() {
    let drafts = geojson::network_from_str(
        r#"{"type":"FeatureCollection","features":[{
            "type":"Feature",
            "properties":{"oneway":"-1","source":"fixture","id":"reverse-edge"},
            "geometry":{"type":"LineString","coordinates":[[0.0,0.0],[0.01,0.0]]}
        }]}"#,
    )
    .unwrap();

    assert_eq!(drafts[0].travel, EdgeTravel::Backward);
}

#[test]
fn osm_xml_network_normalizes_walkable_ways() {
    let drafts = osm::network_from_str(
        r#"<osm version="0.6">
  <node id="1" lat="40.0" lon="-105.0"/>
  <node id="2" lat="40.0" lon="-104.99"/>
  <node id="3" lat="40.01" lon="-104.99"/>
  <node id="4" lat="40.01" lon="-105.0"/>
  <way id="10">
    <nd ref="1"/><nd ref="2"/>
    <tag k="highway" v="path"/>
    <tag k="surface" v="asphalt"/>
    <tag k="foot" v="designated"/>
    <tag k="oneway:foot" v="yes"/>
  </way>
  <way id="11">
    <nd ref="2"/><nd ref="3"/>
    <tag k="highway" v="track"/>
    <tag k="access" v="private"/>
  </way>
  <way id="12">
    <nd ref="3"/><nd ref="4"/>
    <tag k="highway" v="motorway"/>
  </way>
</osm>"#,
    )
    .unwrap();

    assert_eq!(drafts.len(), 2);
    assert_eq!(drafts[0].terrain, Terrain::Pavement);
    assert_eq!(drafts[0].terrain_confidence, Some(0.86));
    assert_eq!(drafts[0].surface.as_deref(), Some("asphalt"));
    assert_eq!(drafts[0].access, Access::Open);
    assert_eq!(drafts[0].travel, EdgeTravel::Forward);
    assert_eq!(drafts[0].provenance.source, "osm-xml");
    assert_eq!(drafts[0].provenance.source_id.as_deref(), Some("10"));
    assert_eq!(drafts[1].terrain, Terrain::Road);
    assert_eq!(drafts[1].terrain_confidence, Some(0.62));
    assert_eq!(drafts[1].access, Access::Private);
    assert!((drafts[1].road_exposure - 1.0).abs() <= f64::EPSILON);

    let graph = GraphBuilder::default().build(&drafts).unwrap();
    assert_eq!(graph.edges.len(), 2);
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.attr.provenance[0].source == "osm-xml")
    );
}

#[test]
fn osm_xml_network_rejects_missing_way_nodes() {
    let error = osm::network_from_str(
        r#"<osm version="0.6">
  <node id="1" lat="40.0" lon="-105.0"/>
  <way id="10"><nd ref="1"/><nd ref="missing"/><tag k="highway" v="path"/></way>
</osm>"#,
    )
    .unwrap_err();
    assert!(format!("{error}").contains("references missing node missing"));
}

#[test]
fn osm_xml_network_preserves_hiking_route_relation_evidence() {
    let drafts = osm::network_from_str(
        r#"<osm version="0.6">
  <node id="1" lat="40.0" lon="-105.0"/>
  <node id="2" lat="40.0" lon="-104.99"/>
  <way id="10">
    <nd ref="1"/><nd ref="2"/>
    <tag k="highway" v="path"/>
  </way>
  <relation id="20">
    <member type="way" ref="10" role=""/>
    <tag k="type" v="route"/>
    <tag k="route" v="hiking"/>
    <tag k="name" v="Ridge Route"/>
  </relation>
  <relation id="30">
    <member type="way" ref="10" role="from"/>
    <member type="way" ref="10" role="to"/>
    <tag k="type" v="restriction"/>
    <tag k="restriction" v="no_right_turn"/>
  </relation>
</osm>"#,
    )
    .unwrap();

    assert_eq!(drafts.len(), 1);
    assert_eq!(
        drafts[0].provenance.layer.as_deref(),
        Some("way+route-relation+turn-restriction")
    );
    let source_id = drafts[0].provenance.source_id.as_deref().unwrap();
    assert!(source_id.contains("way 10; route relations 20:ridge route"));
    assert!(source_id.contains("turn restrictions 30:from:no_right_turn,30:to:no_right_turn"));
    assert!(drafts[0].confidence >= 0.82);

    let graph = GraphBuilder::default().build(&drafts).unwrap();
    assert_eq!(
        graph.edges[0].attr.provenance[0].layer.as_deref(),
        Some("way+route-relation+turn-restriction")
    );
}

#[test]
fn osm_pbf_network_normalizes_walkable_ways() {
    let drafts = osm::network_from_pbf_reader(Cursor::new(tiny_osm_pbf())).unwrap();

    assert_eq!(drafts.len(), 2);
    assert_eq!(drafts[0].terrain, Terrain::Trail);
    assert_eq!(drafts[0].terrain_confidence, Some(0.68));
    assert_eq!(drafts[0].surface.as_deref(), Some("gravel"));
    assert_eq!(drafts[0].travel, EdgeTravel::Forward);
    assert_eq!(drafts[0].provenance.source, "osm-pbf");
    assert_eq!(
        drafts[0].provenance.layer.as_deref(),
        Some("way+route-relation+turn-restriction")
    );
    let source_id = drafts[0].provenance.source_id.as_deref().unwrap();
    assert!(source_id.contains("way 10; route relations 20:ridge route"));
    assert!(source_id.contains("turn restrictions 30:from:no_right_turn,30:to:no_right_turn"));
    assert!(drafts[0].confidence >= 0.82);
    assert_eq!(drafts[1].terrain, Terrain::Road);
    assert_eq!(drafts[1].terrain_confidence, Some(0.62));
    assert_eq!(drafts[1].access, Access::Private);
    assert!((drafts[1].road_exposure - 1.0).abs() <= f64::EPSILON);

    let graph = GraphBuilder::default().build(&drafts).unwrap();
    assert_eq!(graph.edges.len(), 2);
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.attr.provenance[0].source == "osm-pbf")
    );
}

#[test]
fn osm_pbf_context_overlays_normalize_roads_and_waterways() {
    let overlays = osm::context_overlays_from_pbf_reader(Cursor::new(tiny_osm_pbf())).unwrap();
    assert!(overlays.iter().any(|overlay| {
        overlay.kind == CrossingKind::Road
            && overlay.provenance.source == "osm-road-context"
            && overlay.provenance.source_id.as_deref() == Some("11")
    }));
    assert!(overlays.iter().any(|overlay| {
        overlay.kind == CrossingKind::Water
            && overlay.provenance.source == "osm-hydrology-context"
            && overlay.provenance.source_id.as_deref() == Some("13")
    }));
}

#[test]
fn road_fraction_counts_road_and_pavement_terrain() {
    let graph = GraphBuilder::default()
        .build(&[SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)]).unwrap(),
            terrain: Terrain::Pavement,
            terrain_confidence: None,
            surface: Some("asphalt".to_owned()),
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("paved"),
        }])
        .unwrap();
    let route = Route::from_edges(
        "pavement",
        &graph,
        graph.edges[0].a,
        vec![graph.edges[0].id],
        &LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 10_000.0,
            max_difficulty: 10_000.0,
            allowed_shapes: vec![RouteShape::Open],
            ..LoopConstraints::default()
        },
    );

    assert!((route.metrics.road_fraction - 1.0).abs() <= f64::EPSILON);
    assert!(
        route
            .verdict
            .violations
            .iter()
            .any(|v| v == "road/pavement fraction 100.0% above maximum 12.0%")
    );
    assert!(route.verdict.audit.iter().any(|row| {
        row.metric == "maximum road/pavement exposure"
            && row.measured == "100.0%"
            && row.requirement == "≤ 12.0%"
            && row.margin == "-88.0%"
            && !row.satisfied
    }));
}

#[test]
fn pareto_ranking_preserves_tradeoffs_and_demotes_dominated_routes() {
    let constraints = LoopConstraints {
        min_distance_m: 3_000.0,
        max_distance_m: 8_000.0,
        max_repeated_edge_fraction: 1.0,
        ..LoopConstraints::default()
    };
    let mut routes = vec![
        synthetic_rank_route("hard-roadish", &constraints, 4_000.0, 20.0, 0.05),
        synthetic_rank_route("clean-hard", &constraints, 4_000.0, 10.0, 0.0),
        synthetic_rank_route("easy-roadish", &constraints, 4_000.0, 5.0, 0.05),
    ];

    rank_routes(&mut routes, &constraints);

    assert_eq!(routes[0].name, "easy-roadish");
    assert_eq!(routes[0].pareto_rank, 1);
    assert_eq!(routes[1].name, "clean-hard");
    assert_eq!(routes[1].pareto_rank, 1);
    assert_eq!(routes[2].name, "hard-roadish");
    assert_eq!(routes[2].pareto_rank, 2);
}

fn synthetic_rank_route(
    name: &str,
    constraints: &LoopConstraints,
    distance_m: f64,
    difficulty: f64,
    road_fraction: f64,
) -> Route {
    let metrics = RouteMetrics {
        shape: RouteShape::Loop,
        distance_m,
        difficulty,
        road_fraction,
        terrain_m: BTreeMap::from([(Terrain::Trail, distance_m)]),
        ..RouteMetrics::default()
    };
    let verdict = constraints.judge(&metrics);
    Route {
        name: name.to_owned(),
        start: VertexId(0),
        edges: vec![EdgeId(0)],
        pareto_rank: 0,
        metrics,
        score: 0.0,
        verdict,
    }
}

#[test]
fn closure_overlay_closes_edges_and_records_provenance() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    let overlays =
        geojson::access_overlays_from_str(include_str!("fixtures/closure_overlay.geojson"))
            .unwrap();
    let touched = apply_access_overlays(&mut graph, &overlays, None, DifficultyWeights::default());
    assert!(touched > 0);
    let closed = graph
        .edges
        .iter()
        .filter(|edge| edge.attr.access == Access::Closed)
        .collect::<Vec<_>>();
    assert!(!closed.is_empty());
    assert!(closed.iter().all(|edge| edge.attr.difficulty > 1_000.0));
    assert!(closed.iter().all(|edge| {
        edge.attr
            .access_provenance
            .iter()
            .any(|p| p.source == "fixture-closure")
    }));
}

#[test]
fn dated_access_overlay_only_bites_inside_active_window() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let overlays = geojson::access_overlays_from_str(
        r#"{
          "type": "FeatureCollection",
          "features": [{
            "type": "Feature",
            "properties": {
              "id": "nesting-closure",
              "source": "fixture-closure",
              "access": "closed",
              "active_from": "2026-03-01",
              "active_to": "2026-06-30"
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
    )
    .unwrap();

    let mut spring = GraphBuilder::default().build(&drafts).unwrap();
    let mut summer = GraphBuilder::default().build(&drafts).unwrap();
    let spring_hits = apply_access_overlays(
        &mut spring,
        &overlays,
        Some(PlanningDate::new(2026, 5, 1).unwrap()),
        DifficultyWeights::default(),
    );
    let summer_hits = apply_access_overlays(
        &mut summer,
        &overlays,
        Some(PlanningDate::new(2026, 7, 1).unwrap()),
        DifficultyWeights::default(),
    );

    assert!(spring_hits > 0);
    assert_eq!(summer_hits, 0);
    assert!(
        spring
            .edges
            .iter()
            .any(|edge| edge.attr.access == Access::Closed)
    );
    assert!(
        !summer
            .edges
            .iter()
            .any(|edge| edge.attr.access == Access::Closed)
    );
}

#[test]
fn seasonal_access_overlay_recurs_across_years() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let overlays = geojson::access_overlays_from_str(
        r#"{
          "type": "FeatureCollection",
          "features": [{
            "type": "Feature",
            "properties": {
              "id": "elk-calving-closure",
              "source": "fixture-closure",
              "access": "closed",
              "seasonal_from": "04-15",
              "seasonal_to": "06-30"
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
    )
    .unwrap();

    let mut spring = GraphBuilder::default().build(&drafts).unwrap();
    let mut winter = GraphBuilder::default().build(&drafts).unwrap();
    let spring_hits = apply_access_overlays(
        &mut spring,
        &overlays,
        Some(PlanningDate::new(2027, 5, 1).unwrap()),
        DifficultyWeights::default(),
    );
    let winter_hits = apply_access_overlays(
        &mut winter,
        &overlays,
        Some(PlanningDate::new(2027, 1, 1).unwrap()),
        DifficultyWeights::default(),
    );

    assert!(spring_hits > 0);
    assert_eq!(winter_hits, 0);
    assert!(
        spring
            .edges
            .iter()
            .any(|edge| edge.attr.access == Access::Closed)
    );
    assert!(
        !winter
            .edges
            .iter()
            .any(|edge| edge.attr.access == Access::Closed)
    );
}

#[test]
fn seasonal_access_window_wraps_winter_year_boundary() {
    let winter = SeasonalWindow::new(
        MonthDay::new(11, 15).unwrap(),
        MonthDay::new(3, 31).unwrap(),
    );

    assert!(winter.contains(PlanningDate::new(2027, 12, 1).unwrap()));
    assert!(winter.contains(PlanningDate::new(2028, 2, 1).unwrap()));
    assert!(!winter.contains(PlanningDate::new(2028, 7, 1).unwrap()));
}

#[test]
fn planning_dates_derive_weekdays() {
    assert_eq!(
        PlanningDate::new(2026, 7, 4).unwrap().weekday(),
        Weekday::Saturday
    );
    assert_eq!(
        PlanningDate::new(2026, 7, 6).unwrap().weekday(),
        Weekday::Monday
    );
}

#[test]
fn weekday_sets_parse_shortcuts_and_ranges() {
    let workweek = "mon-fri".parse::<WeekdaySet>().unwrap();
    assert!(workweek.contains(Weekday::Monday));
    assert!(workweek.contains(Weekday::Friday));
    assert!(!workweek.contains(Weekday::Sunday));

    let weekend = "weekends".parse::<WeekdaySet>().unwrap();
    assert!(weekend.contains(Weekday::Saturday));
    assert!(weekend.contains(Weekday::Sunday));
    assert!(!weekend.contains(Weekday::Wednesday));
}

#[test]
fn daily_time_window_parses_and_wraps_midnight() {
    let daytime = DailyTimeWindow::new(
        "08:00".parse::<PlanningTime>().unwrap(),
        "17:30".parse::<PlanningTime>().unwrap(),
    );
    assert!(daytime.contains("12:00".parse().unwrap()));
    assert!(!daytime.contains("18:00".parse().unwrap()));

    let overnight = DailyTimeWindow::new(
        "22:00".parse::<PlanningTime>().unwrap(),
        "05:00".parse::<PlanningTime>().unwrap(),
    );
    assert!(overnight.contains("23:30".parse().unwrap()));
    assert!(overnight.contains("04:30".parse().unwrap()));
    assert!(!overnight.contains("12:00".parse().unwrap()));
}

#[test]
fn hourly_access_overlay_only_bites_inside_daily_window() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let overlays = geojson::access_overlays_from_str(
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
              "time_to": "17:30"
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
    )
    .unwrap();

    let date = PlanningDate::new(2026, 7, 6).unwrap();
    let mut midday = GraphBuilder::default().build(&drafts).unwrap();
    let mut evening = GraphBuilder::default().build(&drafts).unwrap();
    let mut time_only_evening = GraphBuilder::default().build(&drafts).unwrap();
    let midday_hits = apply_access_overlays_at(
        &mut midday,
        &overlays,
        Some(PlanningMoment::new(
            Some(date),
            Some("12:00".parse::<PlanningTime>().unwrap()),
        )),
        DifficultyWeights::default(),
    );
    let evening_hits = apply_access_overlays_at(
        &mut evening,
        &overlays,
        Some(PlanningMoment::new(
            Some(date),
            Some("18:00".parse::<PlanningTime>().unwrap()),
        )),
        DifficultyWeights::default(),
    );
    let time_only_evening_hits = apply_access_overlays_at(
        &mut time_only_evening,
        &overlays,
        Some(PlanningMoment::new(
            None,
            Some("18:00".parse::<PlanningTime>().unwrap()),
        )),
        DifficultyWeights::default(),
    );

    assert!(midday_hits > 0);
    assert_eq!(evening_hits, 0);
    assert_eq!(time_only_evening_hits, 0);
    assert!(
        midday
            .edges
            .iter()
            .any(|edge| edge.attr.access == Access::Closed)
    );
    assert!(
        !evening
            .edges
            .iter()
            .any(|edge| edge.attr.access == Access::Closed)
    );
}

#[test]
fn reservation_required_overlay_normalizes_to_timed_restriction() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let overlays = geojson::access_overlays_from_str(
        r#"{
          "type": "FeatureCollection",
          "features": [{
            "type": "Feature",
            "properties": {
              "id": "timed-entry-permit",
              "source": "fixture-permit-system",
              "reservation_required": "yes",
              "active_from": "2026-07-01",
              "active_to": "2026-07-31",
              "time_from": "08:00",
              "time_to": "17:30"
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
    )
    .unwrap();

    assert_eq!(overlays[0].access, Access::Restricted);
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    let hits = apply_access_overlays_at(
        &mut graph,
        &overlays,
        Some(PlanningMoment::new(
            Some(PlanningDate::new(2026, 7, 6).unwrap()),
            Some("12:00".parse().unwrap()),
        )),
        DifficultyWeights::default(),
    );

    assert!(hits > 0);
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.attr.access == Access::Restricted)
    );
    assert!(graph.edges.iter().any(|edge| {
        edge.attr.access_provenance.iter().any(|p| {
            p.source == "fixture-permit-system"
                && p.source_id.as_deref() == Some("timed-entry-permit")
        })
    }));
}

#[test]
fn weekday_access_overlay_only_bites_on_listed_days() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let overlays = geojson::access_overlays_from_str(
        r#"{
          "type": "FeatureCollection",
          "features": [{
            "type": "Feature",
            "properties": {
              "id": "weekend-maintenance",
              "source": "fixture-closure",
              "access": "closed",
              "weekdays": ["sat", "sun"]
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
    )
    .unwrap();

    let mut saturday = GraphBuilder::default().build(&drafts).unwrap();
    let mut monday = GraphBuilder::default().build(&drafts).unwrap();
    let saturday_hits = apply_access_overlays(
        &mut saturday,
        &overlays,
        Some(PlanningDate::new(2026, 7, 4).unwrap()),
        DifficultyWeights::default(),
    );
    let monday_hits = apply_access_overlays(
        &mut monday,
        &overlays,
        Some(PlanningDate::new(2026, 7, 6).unwrap()),
        DifficultyWeights::default(),
    );

    assert!(saturday_hits > 0);
    assert_eq!(monday_hits, 0);
    assert!(
        saturday
            .edges
            .iter()
            .any(|edge| edge.attr.access == Access::Closed)
    );
    assert!(
        !monday
            .edges
            .iter()
            .any(|edge| edge.attr.access == Access::Closed)
    );
}

#[test]
fn multiline_access_overlay_hits_each_line_without_flattening() {
    let mut graph = GraphBuilder::default()
        .build(&[
            SegmentDraft {
                geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)])
                    .unwrap(),
                terrain: Terrain::Trail,
                terrain_confidence: None,
                surface: None,
                access: Access::Open,
                travel: EdgeTravel::Both,
                road_exposure: 0.0,
                confidence: 1.0,
                provenance: Provenance::fixture("lower"),
            },
            SegmentDraft {
                geometry: LineString::new(vec![Coord::new(0.0, 0.01), Coord::new(0.01, 0.01)])
                    .unwrap(),
                terrain: Terrain::Trail,
                terrain_confidence: None,
                surface: None,
                access: Access::Open,
                travel: EdgeTravel::Both,
                road_exposure: 0.0,
                confidence: 1.0,
                provenance: Provenance::fixture("upper"),
            },
        ])
        .unwrap();
    let overlays = geojson::access_overlays_from_str(
        r#"{"type":"FeatureCollection","features":[{
            "type":"Feature",
            "properties":{"status":"closed","name":"two-corridors","confidence":0.7},
            "geometry":{"type":"MultiLineString","coordinates":[
                [[0.005,-0.001],[0.005,0.001]],
                [[0.005,0.009],[0.005,0.011]]
            ]}
        }]}"#,
    )
    .unwrap();

    assert!(matches!(
        &overlays[0].geometry,
        OverlayGeometry::MultiLine(lines) if lines.len() == 2
    ));
    let touched = apply_access_overlays(&mut graph, &overlays, None, DifficultyWeights::default());

    assert_eq!(touched, 2);
    assert!(
        graph
            .edges
            .iter()
            .all(|edge| edge.attr.access == Access::Closed)
    );
}

#[test]
fn access_restrictions_are_hard_route_constraints() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    let overlays =
        geojson::access_overlays_from_str(include_str!("fixtures/closure_overlay.geojson"))
            .unwrap();
    apply_access_overlays(&mut graph, &overlays, None, DifficultyWeights::default());
    let edge = graph
        .edges
        .iter()
        .find(|edge| edge.attr.access == Access::Closed)
        .expect("fixture closure should touch an edge");
    let route = Route::from_edges(
        "closed-segment",
        &graph,
        edge.a,
        vec![edge.id],
        &LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 10_000.0,
            max_difficulty: 10_000.0,
            allowed_shapes: vec![RouteShape::Open],
            ..LoopConstraints::default()
        },
    );

    assert!((route.metrics.restricted_access_fraction - 1.0).abs() <= f64::EPSILON);
    assert!(
        route
            .metrics
            .access_percentages()
            .get(&Access::Closed)
            .is_some_and(|fraction| (*fraction - 1.0).abs() <= f64::EPSILON)
    );
    assert!(!route.verdict.satisfied);
    assert!(
        route
            .verdict
            .violations
            .iter()
            .any(|v| v == "restricted-access fraction 100.0% above maximum 0.0%")
    );
}

#[test]
fn route_geojson_exports_full_diagnostics() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    let overlays =
        geojson::access_overlays_from_str(include_str!("fixtures/closure_overlay.geojson"))
            .unwrap();
    apply_access_overlays(&mut graph, &overlays, None, DifficultyWeights::default());
    let closed_edge = graph
        .edges
        .iter()
        .find(|edge| edge.attr.access == Access::Closed)
        .map(|edge| (edge.id, edge.a))
        .expect("fixture closure should touch an edge");
    graph.edges[closed_edge.0.0].attr.confidence = 0.42;
    let mut route = Route::from_edges(
        "closed-segment",
        &graph,
        closed_edge.1,
        vec![closed_edge.0],
        &LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 10_000.0,
            max_difficulty: 10_000.0,
            allowed_shapes: vec![RouteShape::Open],
            ..LoopConstraints::default()
        },
    );
    let expected_score = route.computed_score();
    route.score = 0.0;

    let gj = geojson::routes_to_geojson(&graph, &[route.clone()]);
    let properties = &gj["features"][0]["properties"];
    let report = report::render(&graph, &[route]);

    assert!(
        properties["score"]
            .as_f64()
            .is_some_and(|score| (score - expected_score).abs() <= 1.0e-9 && score > 0.0)
    );
    assert!(report.contains(&format!("- score: {expected_score:.2}")));
    assert!(report.contains("Constraint audit:"));
    assert_eq!(properties["restricted_access_fraction"], 1.0);
    assert_eq!(properties["access_fraction"]["closed"], 1.0);
    assert!(
        properties["constraint_audit"]
            .as_array()
            .is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["metric"] == "maximum restricted-access exposure"
                        && row["measured"] == "100.0%"
                        && row["requirement"] == "≤ 0.0%"
                        && row["satisfied"] == false
                })
            })
    );
    assert!(properties["terrain_fraction"].is_object());
    assert!(properties["terrain_m"].is_object());
    assert!(properties["access_m"].is_object());
    assert!(
        properties["constraint_penalty"]
            .as_f64()
            .is_some_and(|x| x > 0.0)
    );
    assert_eq!(properties["edge_count"], 1);
    assert_eq!(
        properties["difficulty_hotspots"][0]["edge_id"],
        closed_edge.0.0
    );
    assert_eq!(
        properties["access_warning_edges"][0]["edge_id"],
        closed_edge.0.0
    );
    assert_eq!(properties["access_warning_edges"][0]["access"], "closed");
    assert!(properties["access_warning_edges"][0]["access_provenance"].is_array());
    assert_eq!(
        properties["low_confidence_edges"][0]["edge_id"],
        closed_edge.0.0
    );
    assert_eq!(properties["low_confidence_edges"][0]["confidence"], 0.42);
    assert!(properties["low_confidence_edges"][0]["terrain_evidence"].is_array());
    assert!(properties["low_confidence_edges"][0]["difficulty"].is_number());
    assert!(properties["low_confidence_edges"][0]["difficulty_breakdown"].is_object());
    assert!(properties["low_confidence_edges"][0]["elevation_provenance"].is_array());
    assert!(properties["low_confidence_edges"][0]["source_provenance"].is_array());
    assert!(properties["low_confidence_edges"][0]["road_exposure"].is_number());
    assert_eq!(properties["dubious_edges"][0]["edge_id"], closed_edge.0.0);
    assert!(
        properties["source_provenance"]
            .as_array()
            .is_some_and(|xs| !xs.is_empty())
    );
}

#[test]
fn context_overlays_infer_road_and_water_crossings() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    let overlays =
        geojson::context_overlays_from_str(include_str!("fixtures/context_overlay.geojson"))
            .unwrap();
    let crossings = apply_context_overlays(&mut graph, &overlays, DifficultyWeights::default());
    assert!(crossings >= 4);
    let once = graph
        .edges
        .iter()
        .flat_map(|edge| edge.attr.crossings.iter())
        .map(|x| x.count)
        .sum::<u32>();
    let _ = apply_context_overlays(&mut graph, &overlays, DifficultyWeights::default());
    let twice = graph
        .edges
        .iter()
        .flat_map(|edge| edge.attr.crossings.iter())
        .map(|x| x.count)
        .sum::<u32>();
    assert_eq!(once, twice);
    assert!(graph.edges.iter().any(|edge| {
        edge.attr
            .crossings
            .iter()
            .any(|x| x.kind == CrossingKind::Road && x.count > 0)
    }));
    assert!(graph.edges.iter().any(|edge| {
        edge.attr
            .crossings
            .iter()
            .any(|x| x.kind == CrossingKind::Water && x.count > 0)
    }));
    assert!(graph.edges.iter().any(|edge| edge.attr.road_exposure > 0.0));
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.attr.difficulty_breakdown.road > 0.0)
    );

    let start = graph.nearest_vertex(Coord::new(-105.0, 40.0)).unwrap();
    let route = LoopHunter::default()
        .hunt(
            &graph,
            start,
            &LoopConstraints {
                min_distance_m: 3_000.0,
                max_distance_m: 8_000.0,
                max_difficulty: 10_000.0,
                ..LoopConstraints::default()
            },
            1,
        )
        .into_iter()
        .next()
        .unwrap();
    assert!(
        route
            .metrics
            .crossings
            .get(&CrossingKind::Road)
            .copied()
            .unwrap_or_default()
            > 0
    );
    assert!(
        route
            .metrics
            .crossings
            .get(&CrossingKind::Water)
            .copied()
            .unwrap_or_default()
            > 0
    );
    let rendered = report::render(&graph, &[route]);
    assert!(rendered.contains("Crossings:"));
    assert!(rendered.contains("Road:"));
    assert!(rendered.contains("Water:"));
}

#[test]
fn osm_context_overlays_infer_road_and_water_crossings() {
    let mut graph = GraphBuilder::default()
        .build(&[SegmentDraft {
            geometry: LineString::new(vec![
                Coord::new(-105.005, 39.995),
                Coord::new(-105.005, 40.015),
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
        }])
        .unwrap();
    let overlays = osm::context_overlays_from_str(
        r#"<osm version="0.6">
  <node id="1" lat="40.0" lon="-105.01"/>
  <node id="2" lat="40.0" lon="-105.0"/>
  <node id="3" lat="40.01" lon="-105.01"/>
  <node id="4" lat="40.01" lon="-105.0"/>
  <way id="20"><nd ref="1"/><nd ref="2"/><tag k="highway" v="service"/><tag k="name" v="Park Road"/></way>
  <way id="21"><nd ref="3"/><nd ref="4"/><tag k="waterway" v="stream"/><tag k="name" v="Cold Creek"/></way>
</osm>"#,
    )
    .unwrap();

    assert_eq!(overlays.len(), 2);
    let crossings = apply_context_overlays(&mut graph, &overlays, DifficultyWeights::default());
    assert_eq!(crossings, 2);
    let edge = &graph.edges[0];
    assert!(edge.attr.road_exposure > 0.0);
    assert!(edge.attr.crossings.iter().any(|crossing| {
        crossing.kind == CrossingKind::Road && crossing.provenance.source == "osm-road-context"
    }));
    assert!(edge.attr.crossings.iter().any(|crossing| {
        crossing.kind == CrossingKind::Water
            && crossing.provenance.source == "osm-hydrology-context"
    }));
}

#[test]
fn multiline_context_overlay_does_not_invent_joiner_crossings() {
    let mut graph = GraphBuilder::default()
        .build(&[SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.5, 0.4), Coord::new(0.5, 0.6)]).unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("trail"),
        }])
        .unwrap();
    let overlays = geojson::context_overlays_from_str(
        r#"{"type":"FeatureCollection","features":[{
            "type":"Feature",
            "properties":{"kind":"road","name":"disconnected-roads"},
            "geometry":{"type":"MultiLineString","coordinates":[
                [[0.0,0.0],[0.4,0.0]],
                [[0.6,1.0],[1.0,1.0]]
            ]}
        }]}"#,
    )
    .unwrap();

    assert_eq!(overlays.len(), 2);
    let crossings = apply_context_overlays(&mut graph, &overlays, DifficultyWeights::default());

    assert_eq!(crossings, 0);
    assert!(graph.edges[0].attr.crossings.is_empty());
}

#[test]
fn terrain_overlays_override_edges_with_provenance_and_rerating() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    let overlays =
        geojson::terrain_overlays_from_str(include_str!("fixtures/terrain_overlay.geojson"))
            .unwrap();
    let touched = apply_terrain_overlays(&mut graph, &overlays, DifficultyWeights::default());
    assert!(touched > 0);
    let overlaid = graph
        .edges
        .iter()
        .filter(|edge| {
            edge.attr.terrain_evidence.iter().any(|e| {
                e.terrain == Terrain::Talus
                    && e.rationale == "terrain overlay"
                    && e.provenance
                        .as_ref()
                        .is_some_and(|p| p.source == "fixture-landcover")
            })
        })
        .collect::<Vec<_>>();
    assert!(!overlaid.is_empty());
    assert!(
        overlaid
            .iter()
            .all(|edge| edge.attr.terrain == Terrain::Talus)
    );
    assert!(
        overlaid
            .iter()
            .all(|edge| edge.attr.terrain_confidence >= 0.87)
    );
    assert!(overlaid.iter().all(|edge| {
        (edge.attr.difficulty - edge.attr.difficulty_breakdown.total()).abs() <= 1.0e-9
    }));
    let overlaid_edge = overlaid[0];
    let rendered = report::render(
        &graph,
        &[Route::from_edges(
            "overlaid",
            &graph,
            overlaid_edge.a,
            vec![overlaid_edge.id],
            &LoopConstraints {
                min_distance_m: 0.0,
                max_distance_m: 10_000.0,
                max_difficulty: 10_000.0,
                allowed_shapes: vec![RouteShape::Open],
                ..LoopConstraints::default()
            },
        )],
    );
    assert!(rendered.contains("current Talus"));
    assert!(rendered.contains("terrain overlay (fixture-landcover:"));
    let evidence_count = graph
        .edges
        .iter()
        .flat_map(|edge| edge.attr.terrain_evidence.iter())
        .filter(|e| e.rationale == "terrain overlay")
        .count();
    let _ = apply_terrain_overlays(&mut graph, &overlays, DifficultyWeights::default());
    let evidence_count_again = graph
        .edges
        .iter()
        .flat_map(|edge| edge.attr.terrain_evidence.iter())
        .filter(|e| e.rationale == "terrain overlay")
        .count();
    assert_eq!(evidence_count, evidence_count_again);
}

#[test]
fn terrain_overlays_normalize_landcover_codes_and_names() {
    let overlays = geojson::terrain_overlays_from_str(
        r#"{"type":"FeatureCollection","features":[
          {
            "type":"Feature",
            "properties":{"name":"nlcd-barren","nlcd":31,"source":"nlcd-fixture"},
            "geometry":{"type":"Polygon","coordinates":[[
              [-105.001,39.999],[-104.999,39.999],[-104.999,40.001],[-105.001,40.001],[-105.001,39.999]
            ]]}
          },
          {
            "type":"Feature",
            "properties":{"name":"nlcd-forest","landcover":"Deciduous Forest"},
            "geometry":{"type":"Polygon","coordinates":[[
              [-105.001,40.009],[-104.999,40.009],[-104.999,40.011],[-105.001,40.011],[-105.001,40.009]
            ]]}
          }
        ]}"#,
    )
    .unwrap();

    assert_eq!(overlays[0].terrain, Terrain::Talus);
    assert_eq!(overlays[1].terrain, Terrain::Forest);
    assert_eq!(
        Terrain::from_landcover_tag("Developed, Medium Intensity"),
        Terrain::Pavement
    );
    assert_eq!(Terrain::from_landcover_tag("95"), Terrain::Water);
}

#[test]
fn fixture_generates_nontrivial_loops() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let graph = GraphBuilder::default().build(&drafts).unwrap();
    let start = graph.nearest_vertex(Coord::new(-105.0, 40.0)).unwrap();
    let routes = LoopHunter {
        params: SearchParams {
            max_hops: 10,
            max_frontier: 10_000,
            keep: 8,
            ..SearchParams::default()
        },
    }
    .hunt(
        &graph,
        start,
        &LoopConstraints {
            min_distance_m: 3_000.0,
            max_distance_m: 8_000.0,
            max_difficulty: 80.0,
            ..LoopConstraints::default()
        },
        4,
    );
    assert!(routes.len() >= 2);
    assert!(routes.iter().any(|r| r.verdict.satisfied));
    assert!(routes.iter().all(|r| r.metrics.shape == RouteShape::Loop));
    assert!(routes.iter().all(|r| r.score.is_finite()));
    assert!(routes.windows(2).all(|w| w[0].score <= w[1].score));
    assert!(routes.iter().all(|r| {
        (r.metrics.difficulty - r.metrics.difficulty_breakdown.total()).abs() <= 1.0e-9
    }));
}

#[test]
fn report_explains_difficulty_decomposition() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    let start = graph.nearest_vertex(Coord::new(-105.0, 40.0)).unwrap();
    let route = LoopHunter::default()
        .hunt(
            &graph,
            start,
            &LoopConstraints {
                min_distance_m: 3_000.0,
                max_distance_m: 8_000.0,
                max_difficulty: 10_000.0,
                ..LoopConstraints::default()
            },
            1,
        )
        .into_iter()
        .next()
        .unwrap();
    let low_confidence_edge = route.edges[0];
    graph.edges[low_confidence_edge.0].attr.confidence = 0.42;
    let rendered = report::render(&graph, &[route]);
    assert!(rendered.contains("- score:"));
    assert!(rendered.contains("Route sequence:"));
    assert!(rendered.contains("- start vertex:"));
    assert!(rendered.contains("- edge ids:"));
    assert!(rendered.contains("- vertex ids:"));
    assert!(rendered.contains("Difficulty decomposition:"));
    assert!(rendered.contains("- distance:"));
    assert!(rendered.contains("- ascent:"));
    assert!(rendered.contains("Largest difficulty contributors:"));
    assert!(rendered.contains("Low-confidence segments:"));
    assert!(rendered.contains("confidence 0.42"));
    assert!(rendered.contains("terrain evidence"));
    assert!(rendered.contains("elevation sources"));
    assert!(rendered.contains("Source provenance:"));
    assert!(rendered.contains("fixture:"));
    assert!(rendered.contains("edge "));
    assert!(rendered.contains("grade bins flat"));
}

#[test]
fn shape_constraints_reject_and_allow_out_and_back_routes() {
    let graph = GraphBuilder::default()
        .build(&[simple_path_draft()])
        .unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let route = Route::from_edges(
        "out-and-back",
        &graph,
        start,
        vec![
            graph.edges[0].id,
            graph.edges[1].id,
            graph.edges[1].id,
            graph.edges[0].id,
        ],
        &LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 10_000.0,
            max_difficulty: 10_000.0,
            ..LoopConstraints::default()
        },
    );
    assert_eq!(route.metrics.shape, RouteShape::OutAndBack);
    assert!((route.metrics.repeated_edge_fraction - 0.5).abs() <= f64::EPSILON);
    assert!((route.metrics.ascent_m - route.metrics.descent_m).abs() <= f64::EPSILON);
    assert!(!route.verdict.satisfied);
    assert!(
        route
            .verdict
            .violations
            .iter()
            .any(|v| v.contains("route shape OutAndBack"))
    );

    let allowed = Route::from_edges(
        "out-and-back",
        &graph,
        start,
        route.edges,
        &LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 10_000.0,
            max_difficulty: 10_000.0,
            max_repeated_edge_fraction: 1.0,
            allowed_shapes: vec![RouteShape::OutAndBack],
            ..LoopConstraints::default()
        },
    );
    assert!(allowed.verdict.satisfied);
}

#[test]
fn repeated_edge_fraction_is_distance_weighted() {
    let graph = GraphBuilder::default()
        .build(&[SegmentDraft {
            geometry: LineString::new(vec![
                Coord::new(0.0, 0.0),
                Coord::new(0.02, 0.0),
                Coord::new(0.021, 0.0),
            ])
            .unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("skewed-path"),
        }])
        .unwrap();
    let long = graph.edges[0].id;
    let short = graph.edges[1].id;
    let route = Route::from_edges(
        "short-repeat",
        &graph,
        graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap(),
        vec![long, short, short],
        &LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 10_000.0,
            max_difficulty: 10_000.0,
            allowed_shapes: vec![RouteShape::Open],
            ..LoopConstraints::default()
        },
    );

    let expected = graph.edges[short.0].attr.length_m / route.metrics.distance_m;
    assert!((route.metrics.repeated_edge_fraction - expected).abs() <= 1.0e-12);
    assert!(route.metrics.repeated_edge_fraction < 0.1);
}

#[test]
fn loop_hunter_emits_out_and_back_when_shape_allows_repeated_edges() {
    let graph = GraphBuilder::default()
        .build(&[simple_path_draft()])
        .unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let routes = LoopHunter {
        params: SearchParams {
            max_hops: 4,
            max_frontier: 100,
            keep: 8,
            ..SearchParams::default()
        },
    }
    .hunt(
        &graph,
        start,
        &LoopConstraints {
            min_distance_m: 100.0,
            max_distance_m: 10_000.0,
            max_difficulty: 10_000.0,
            max_repeated_edge_fraction: 1.0,
            allowed_shapes: vec![RouteShape::OutAndBack],
            ..LoopConstraints::default()
        },
        4,
    );
    assert!(
        routes
            .iter()
            .any(|r| r.metrics.shape == RouteShape::OutAndBack)
    );
    assert!(
        routes
            .iter()
            .all(|r| r.metrics.shape == RouteShape::OutAndBack)
    );
    assert!(routes.iter().any(|r| r.verdict.satisfied));
}

#[test]
fn loop_hunter_rejects_directionally_impossible_out_and_back() {
    let graph = GraphBuilder::default()
        .build(&[SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)]).unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Forward,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("one-way-spur"),
        }])
        .unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let routes = LoopHunter {
        params: SearchParams {
            max_hops: 1,
            max_frontier: 10,
            keep: 4,
            ..SearchParams::default()
        },
    }
    .hunt(
        &graph,
        start,
        &LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 10_000.0,
            max_difficulty: 10_000.0,
            max_repeated_edge_fraction: 1.0,
            allowed_shapes: vec![RouteShape::OutAndBack],
            ..LoopConstraints::default()
        },
        4,
    );

    assert!(routes.is_empty());
}

#[test]
fn loop_hunter_closes_sparse_frontier_with_shortest_return_path() {
    let graph = GraphBuilder::default().build(&square_drafts()).unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let routes = LoopHunter {
        params: SearchParams {
            max_hops: 1,
            max_frontier: 20,
            keep: 4,
            ..SearchParams::default()
        },
    }
    .hunt(
        &graph,
        start,
        &LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 10_000.0,
            max_difficulty: 10_000.0,
            allowed_shapes: vec![RouteShape::Loop],
            ..LoopConstraints::default()
        },
        4,
    );

    assert!(routes.iter().any(|r| {
        r.metrics.shape == RouteShape::Loop && r.edges.len() == 4 && r.verdict.satisfied
    }));
}

#[test]
fn loop_hunter_tries_alternate_return_paths_when_shortest_closure_violates_constraints() {
    let graph = GraphBuilder::default()
        .build(&closure_trap_drafts())
        .unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let constraints = LoopConstraints {
        min_distance_m: 0.0,
        max_distance_m: 8_000.0,
        max_difficulty: 10_000.0,
        max_road_fraction: 0.10,
        allowed_shapes: vec![RouteShape::Loop],
        ..LoopConstraints::default()
    };
    let hunt = |closure_paths| {
        LoopHunter {
            params: SearchParams {
                max_hops: 1,
                max_frontier: 20,
                keep: 8,
                closure_paths,
                ..SearchParams::default()
            },
        }
        .hunt(&graph, start, &constraints, 8)
    };

    assert!(!hunt(1).iter().any(|route| route.verdict.satisfied));
    assert!(hunt(2).iter().any(|route| {
        route.verdict.satisfied
            && route.metrics.road_fraction <= constraints.max_road_fraction
            && route.edges.len() > 2
    }));
}

#[test]
fn loop_hunter_seed_diversifies_sparse_frontier_order() {
    let graph = GraphBuilder::default().build(&bowtie_drafts()).unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let constraints = LoopConstraints {
        min_distance_m: 0.0,
        max_distance_m: 10_000.0,
        max_difficulty: 10_000.0,
        allowed_shapes: vec![RouteShape::Loop],
        ..LoopConstraints::default()
    };
    let hunt = |seed| {
        LoopHunter {
            params: SearchParams {
                max_hops: 1,
                max_frontier: 2,
                keep: 1,
                closure_paths: 1,
                seed,
            },
        }
        .hunt(&graph, start, &constraints, 1)
        .into_iter()
        .next()
        .expect("tight seeded frontier should still close one bowtie lobe")
        .edges
    };

    assert_eq!(hunt(11), hunt(11));
    let signatures = (0..32).map(hunt).collect::<BTreeSet<_>>();
    assert!(
        signatures.len() > 1,
        "seeded sparse frontier should choose more than one symmetric lobe: {signatures:?}"
    );
}

#[test]
fn exact_solver_enumerates_only_fully_bounded_loops() {
    let graph = GraphBuilder::default().build(&square_drafts()).unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let constraints = LoopConstraints {
        min_distance_m: 0.0,
        max_distance_m: 10_000.0,
        max_difficulty: 10_000.0,
        allowed_shapes: vec![RouteShape::Loop],
        ..LoopConstraints::default()
    };

    assert!(
        ExactLoopSolver {
            params: SearchParams {
                max_hops: 3,
                max_frontier: 100,
                keep: 4,
                ..SearchParams::default()
            },
        }
        .enumerate(&graph, start, &constraints, 4)
        .is_empty()
    );
    let routes = ExactLoopSolver {
        params: SearchParams {
            max_hops: 4,
            max_frontier: 100,
            keep: 4,
            ..SearchParams::default()
        },
    }
    .enumerate(&graph, start, &constraints, 4);

    assert!(routes.iter().any(|r| {
        r.metrics.shape == RouteShape::Loop && r.edges.len() == 4 && r.verdict.satisfied
    }));
}

#[test]
fn exact_solver_accepts_two_edge_multiedge_loops() {
    let graph = GraphBuilder::default()
        .build(&[
            SegmentDraft {
                geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)])
                    .unwrap(),
                terrain: Terrain::Trail,
                terrain_confidence: None,
                surface: None,
                access: Access::Open,
                travel: EdgeTravel::Both,
                road_exposure: 0.0,
                confidence: 1.0,
                provenance: Provenance::fixture("out"),
            },
            SegmentDraft {
                geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)])
                    .unwrap(),
                terrain: Terrain::Trail,
                terrain_confidence: None,
                surface: None,
                access: Access::Open,
                travel: EdgeTravel::Both,
                road_exposure: 0.0,
                confidence: 1.0,
                provenance: Provenance::fixture("back"),
            },
        ])
        .unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let routes = ExactLoopSolver {
        params: SearchParams {
            max_hops: 2,
            max_frontier: 100,
            keep: 4,
            ..SearchParams::default()
        },
    }
    .enumerate(
        &graph,
        start,
        &LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 10_000.0,
            max_difficulty: 10_000.0,
            allowed_shapes: vec![RouteShape::Loop],
            ..LoopConstraints::default()
        },
        4,
    );

    assert!(routes.iter().any(|r| {
        r.metrics.shape == RouteShape::Loop
            && r.edges.len() == 2
            && r.metrics.repeated_edge_fraction == 0.0
    }));
}

#[test]
fn solvers_collapse_reversed_equivalent_edge_sets() {
    let graph = GraphBuilder::default()
        .build(&[
            SegmentDraft {
                geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)])
                    .unwrap(),
                terrain: Terrain::Trail,
                terrain_confidence: None,
                surface: None,
                access: Access::Open,
                travel: EdgeTravel::Both,
                road_exposure: 0.0,
                confidence: 1.0,
                provenance: Provenance::fixture("braid-a"),
            },
            SegmentDraft {
                geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)])
                    .unwrap(),
                terrain: Terrain::Trail,
                terrain_confidence: None,
                surface: None,
                access: Access::Open,
                travel: EdgeTravel::Both,
                road_exposure: 0.0,
                confidence: 1.0,
                provenance: Provenance::fixture("braid-b"),
            },
        ])
        .unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let routes = ExactLoopSolver {
        params: SearchParams {
            max_hops: 2,
            max_frontier: 100,
            keep: 8,
            ..SearchParams::default()
        },
    }
    .enumerate(
        &graph,
        start,
        &LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 10_000.0,
            max_difficulty: 10_000.0,
            allowed_shapes: vec![RouteShape::Loop],
            ..LoopConstraints::default()
        },
        8,
    );

    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].metrics.shape, RouteShape::Loop);
    assert_eq!(routes[0].edges.len(), 2);
    assert!(routes[0].metrics.repeated_edge_fraction <= f64::EPSILON);
}

#[test]
fn directed_travel_diagnostics_are_exported() {
    let graph = GraphBuilder::default()
        .build(&[
            SegmentDraft {
                geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)])
                    .unwrap(),
                terrain: Terrain::Trail,
                terrain_confidence: None,
                surface: None,
                access: Access::Open,
                travel: EdgeTravel::Forward,
                road_exposure: 0.0,
                confidence: 0.95,
                provenance: Provenance::fixture("forward"),
            },
            SegmentDraft {
                geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)])
                    .unwrap(),
                terrain: Terrain::Trail,
                terrain_confidence: None,
                surface: None,
                access: Access::Open,
                travel: EdgeTravel::Backward,
                road_exposure: 0.0,
                confidence: 0.9,
                provenance: Provenance::fixture("backward"),
            },
        ])
        .unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let route = ExactLoopSolver {
        params: SearchParams {
            max_hops: 2,
            max_frontier: 100,
            keep: 4,
            ..SearchParams::default()
        },
    }
    .enumerate(
        &graph,
        start,
        &LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 10_000.0,
            max_difficulty: 10_000.0,
            allowed_shapes: vec![RouteShape::Loop],
            ..LoopConstraints::default()
        },
        4,
    )
    .into_iter()
    .find(|route| route.metrics.shape == RouteShape::Loop && route.edges.len() == 2)
    .unwrap();

    let geojson = geojson::routes_to_geojson(&graph, std::slice::from_ref(&route));
    let directed = geojson["features"][0]["properties"]["directed_travel_edges"]
        .as_array()
        .unwrap();
    assert_eq!(directed.len(), 2);
    assert!(directed.iter().any(|edge| edge["travel"] == "forward"));
    assert!(directed.iter().any(|edge| edge["travel"] == "backward"));

    let text = report::render_titled("Routes", &graph, std::slice::from_ref(&route));
    assert!(text.contains("Directed travel constraints:"));
    assert!(text.contains("Forward"));
    assert!(text.contains("Backward"));
}

#[test]
fn temporal_direction_overlay_constrains_route_generation() {
    let drafts = [
        SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)]).unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("out"),
        },
        SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)]).unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("back"),
        },
    ];
    let constraints = LoopConstraints {
        min_distance_m: 0.0,
        max_distance_m: 10_000.0,
        max_difficulty: 10_000.0,
        allowed_shapes: vec![RouteShape::Loop],
        ..LoopConstraints::default()
    };
    let solver = ExactLoopSolver {
        params: SearchParams {
            max_hops: 2,
            max_frontier: 100,
            keep: 4,
            ..SearchParams::default()
        },
    };
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    assert!(!solver.enumerate(&graph, start, &constraints, 4).is_empty());

    let overlays = geojson::access_overlays_from_str(
        r#"{
          "type": "FeatureCollection",
          "features": [{
            "type": "Feature",
            "properties": {
              "id": "clockwise-only",
              "source": "fixture-direction",
              "access": "open",
              "travel": "forward",
              "seasonal_from": "06-01",
              "seasonal_to": "09-30"
            },
            "geometry": {
              "type": "Polygon",
              "coordinates": [[
                [-0.001, -0.001],
                [0.011, -0.001],
                [0.011, 0.001],
                [-0.001, 0.001],
                [-0.001, -0.001]
              ]]
            }
          }]
        }"#,
    )
    .unwrap();
    let touched = apply_access_overlays(
        &mut graph,
        &overlays,
        Some(PlanningDate::new(2026, 7, 1).unwrap()),
        DifficultyWeights::default(),
    );

    assert_eq!(touched, 2);
    assert!(
        graph
            .edges
            .iter()
            .all(|edge| edge.attr.travel == EdgeTravel::Forward)
    );
    assert!(solver.enumerate(&graph, start, &constraints, 4).is_empty());
}

#[test]
fn milp_formulation_exports_connected_loop_model() {
    let graph = GraphBuilder::default().build(&square_drafts()).unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let constraints = LoopConstraints {
        min_distance_m: 0.0,
        max_distance_m: 10_000.0,
        min_terrain_fraction: BTreeMap::from([(Terrain::Trail, 0.50)]),
        forbidden_terrain: vec![Terrain::Road],
        ..LoopConstraints::default()
    };
    let formulation = LoopMilpFormulation::formulate(&graph, start, &constraints);
    let lp = formulation.to_lp();

    assert!(formulation.binaries.iter().any(|var| var == "y_v0"));
    assert!(
        formulation
            .binaries
            .iter()
            .any(|var| var.starts_with("z_e0_v"))
    );
    assert!(
        formulation
            .rows
            .iter()
            .any(|row| row.name == "flow_start_supplies_visited_vertices")
    );
    assert!(
        formulation
            .rows
            .iter()
            .any(|row| row.name == "distance_min")
    );
    assert!(
        formulation
            .rows
            .iter()
            .any(|row| row.name == "road_fraction_max")
    );
    assert!(lp.contains("Minimize\n obj:"));
    assert!(lp.contains("Subject To\n"));
    assert!(lp.contains("force_start: + 1 y_v0 = 1"));
    assert!(lp.contains("Binary\n"));
    assert!(lp.ends_with("End\n"));
}

#[test]
fn milp_formulation_respects_one_way_arc_feasibility() {
    let graph = GraphBuilder::default()
        .build(&[SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)]).unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Forward,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("one-way"),
        }])
        .unwrap();
    let formulation = LoopMilpFormulation::formulate(
        &graph,
        graph.vertices[0].id,
        &LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 10_000.0,
            ..LoopConstraints::default()
        },
    );

    assert!(formulation.binaries.iter().any(|var| var == "z_e0_v0_v1"));
    assert!(formulation.binaries.iter().all(|var| var != "z_e0_v1_v0"));
}

#[test]
fn milp_incumbent_solution_reconstructs_directed_start_loop() {
    let graph = GraphBuilder::default().build(&square_drafts()).unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let edges = [0, 1, 2, 3]
        .into_iter()
        .map(|i| &graph.edges[i])
        .collect::<Vec<_>>();
    let raw = format!(
        "# HiGHS-style and value-first assignments are both accepted\n\
         z_e{}_v{}_v{} 1\n\
         1 z_e{}_v{}_v{}\n\
         z_e{}_v{}_v{} = 0\n\
         z_e{}_v{}_v{} 1\n\
         z_e{}_v{}_v{} 1\n",
        edges[0].id.0,
        edges[0].a.0,
        edges[0].b.0,
        edges[1].id.0,
        edges[1].a.0,
        edges[1].b.0,
        edges[1].id.0,
        edges[1].b.0,
        edges[1].a.0,
        edges[2].id.0,
        edges[2].a.0,
        edges[2].b.0,
        edges[3].id.0,
        edges[3].a.0,
        edges[3].b.0
    );

    assert_eq!(
        route_edges_from_solution(&graph, start, &raw).unwrap(),
        vec![EdgeId(0), EdgeId(1), EdgeId(2), EdgeId(3)]
    );
}

#[test]
fn milp_incumbent_rejects_disconnected_subtours() {
    let mut drafts = square_drafts();
    let detached_a = Coord::new(1.0, 1.0);
    let detached_b = Coord::new(1.01, 1.0);
    drafts.extend([
        SegmentDraft {
            geometry: LineString::new(vec![detached_a, detached_b]).unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Forward,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("detached-out"),
        },
        SegmentDraft {
            geometry: LineString::new(vec![detached_b, detached_a]).unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Forward,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("detached-back"),
        },
    ]);
    let graph = GraphBuilder::default().build(&drafts).unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let selected = graph
        .edges
        .iter()
        .map(|edge| MilpSelectedArc {
            edge: edge.id,
            from: edge.a,
            to: edge.b,
        })
        .collect::<Vec<_>>();

    let err = route_edges_from_selected_arcs(&graph, start, selected)
        .expect_err("disconnected incumbent must fail");

    assert!(
        err.to_string()
            .contains("outside the reconstructed start loop")
    );
}

#[test]
fn auto_solver_uses_exact_backend_only_for_small_graphs() {
    let graph = GraphBuilder::default().build(&square_drafts()).unwrap();
    assert_eq!(SolverKind::Auto.resolve(&graph), SolverKind::Exact);

    let drafts = (0..40)
        .map(|i| SegmentDraft {
            geometry: LineString::new(vec![
                Coord::new(f64::from(i) * 0.01, 0.0),
                Coord::new(f64::from(i + 1) * 0.01, 0.0),
            ])
            .unwrap(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("large"),
        })
        .collect::<Vec<_>>();
    let large = GraphBuilder::default().build(&drafts).unwrap();
    assert_eq!(SolverKind::Auto.resolve(&large), SolverKind::Heuristic);
}

#[test]
fn loop_hunter_builds_figure_eights_when_shape_allows_two_lobes() {
    let graph = GraphBuilder::default().build(&bowtie_drafts()).unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let routes = LoopHunter {
        params: SearchParams {
            max_hops: 8,
            max_frontier: 1_000,
            keep: 8,
            ..SearchParams::default()
        },
    }
    .hunt(
        &graph,
        start,
        &LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 10_000.0,
            max_difficulty: 10_000.0,
            allowed_shapes: vec![RouteShape::FigureEight],
            ..LoopConstraints::default()
        },
        4,
    );

    assert!(!routes.is_empty());
    assert!(
        routes
            .iter()
            .all(|r| r.metrics.shape == RouteShape::FigureEight)
    );
    assert!(routes.iter().any(|r| r.verdict.satisfied));
}

#[test]
fn terrain_mix_constraints_reject_forbidden_surface() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let graph = GraphBuilder::default().build(&drafts).unwrap();
    let start = graph.nearest_vertex(Coord::new(-105.0, 40.0)).unwrap();
    let unconstrained = LoopHunter::default()
        .hunt(
            &graph,
            start,
            &LoopConstraints {
                min_distance_m: 3_000.0,
                max_distance_m: 8_000.0,
                ..LoopConstraints::default()
            },
            1,
        )
        .into_iter()
        .next()
        .unwrap();
    let forbidden = *unconstrained
        .metrics
        .terrain_m
        .keys()
        .next()
        .expect("fixture route has terrain");
    let route = trailgen_core::Route::from_edges(
        "forbidden-terrain-route",
        &graph,
        start,
        unconstrained.edges,
        &LoopConstraints {
            min_distance_m: 3_000.0,
            max_distance_m: 8_000.0,
            forbidden_terrain: vec![forbidden],
            ..LoopConstraints::default()
        },
    );
    assert!(!route.verdict.satisfied);
    assert!(
        route
            .verdict
            .violations
            .iter()
            .any(|v| v.contains(&format!("forbidden terrain {forbidden:?}")))
    );
}

#[test]
fn elevation_bounds_reject_routes_outside_ascent_descent_window() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let graph = GraphBuilder::default().build(&drafts).unwrap();
    let start = graph.nearest_vertex(Coord::new(-105.0, 40.0)).unwrap();
    let base = LoopHunter::default()
        .hunt(
            &graph,
            start,
            &LoopConstraints {
                min_distance_m: 3_000.0,
                max_distance_m: 8_000.0,
                max_difficulty: 10_000.0,
                ..LoopConstraints::default()
            },
            1,
        )
        .into_iter()
        .next()
        .unwrap();
    let route = Route::from_edges(
        "bad-elevation-window",
        &graph,
        start,
        base.edges,
        &LoopConstraints {
            min_distance_m: 3_000.0,
            max_distance_m: 8_000.0,
            max_difficulty: 10_000.0,
            min_ascent_m: 10_000.0,
            max_ascent_m: 10.0,
            min_descent_m: 10_000.0,
            max_descent_m: 10.0,
            ..LoopConstraints::default()
        },
    );
    assert!(!route.verdict.satisfied);
    for needle in [
        "ascent 100 m below minimum 10000 m",
        "ascent 100 m above maximum 10 m",
        "descent 100 m below minimum 10000 m",
        "descent 100 m above maximum 10 m",
    ] {
        assert!(
            route.verdict.violations.iter().any(|v| v == needle),
            "missing violation {needle:?}: {:?}",
            route.verdict.violations
        );
    }
}

#[test]
fn loop_constraints_deserialize_missing_fields_from_defaults() {
    let constraints = serde_json::from_str::<LoopConstraints>("{}").unwrap();
    assert_eq!(constraints, LoopConstraints::default());
    let constraints =
        serde_json::from_str::<LoopConstraints>(r#"{"max_ascent_m": 1200.0}"#).unwrap();
    assert!((constraints.max_ascent_m - 1_200.0).abs() <= f64::EPSILON);
    assert!(
        (constraints.max_descent_m - LoopConstraints::default().max_descent_m).abs()
            <= f64::EPSILON
    );
}

#[test]
fn gpx_round_trip_reads_exported_route() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let graph = GraphBuilder::default().build(&drafts).unwrap();
    let start = graph.nearest_vertex(Coord::new(-105.0, 40.0)).unwrap();
    let route = LoopHunter::default()
        .hunt(
            &graph,
            start,
            &LoopConstraints {
                min_distance_m: 3_000.0,
                max_distance_m: 8_000.0,
                ..LoopConstraints::default()
            },
            1,
        )
        .into_iter()
        .next()
        .unwrap();
    let xml = gpx::route_to_gpx(&graph, &route);
    let line = gpx::route_line_from_str(&xml).unwrap();
    assert!(line.length_m() > 3_000.0);
}

#[test]
fn kml_round_trip_reads_exported_route() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let graph = GraphBuilder::default().build(&drafts).unwrap();
    let start = graph.nearest_vertex(Coord::new(-105.0, 40.0)).unwrap();
    let route = LoopHunter::default()
        .hunt(
            &graph,
            start,
            &LoopConstraints {
                min_distance_m: 3_000.0,
                max_distance_m: 8_000.0,
                ..LoopConstraints::default()
            },
            1,
        )
        .into_iter()
        .next()
        .unwrap();
    let xml = kml::route_to_kml(&graph, &route);
    let line = kml::route_line_from_str(&xml).unwrap();
    assert!(line.length_m() > 3_000.0);
}

#[test]
fn kmz_round_trip_reads_exported_route() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let graph = GraphBuilder::default().build(&drafts).unwrap();
    let start = graph.nearest_vertex(Coord::new(-105.0, 40.0)).unwrap();
    let route = LoopHunter::default()
        .hunt(
            &graph,
            start,
            &LoopConstraints {
                min_distance_m: 3_000.0,
                max_distance_m: 8_000.0,
                ..LoopConstraints::default()
            },
            1,
        )
        .into_iter()
        .next()
        .unwrap();
    let bytes = kmz::route_to_kmz(&graph, &route).unwrap();
    let line = kmz::route_line_from_bytes(&bytes).unwrap();
    assert!(line.length_m() > 3_000.0);
}

#[test]
fn csv_route_import_reads_headered_lon_lat_ele() {
    let line = csv::route_line_from_str(
        "longitude,latitude,elevation_m\n-105.0,40.0,1600\n-105.0,40.01,1700\n",
    )
    .unwrap();
    assert_eq!(line.points.len(), 2);
    assert_eq!(line.points[1].ele, Some(1700.0));
}

#[test]
fn json_route_import_reads_coordinate_tuples_and_point_objects() {
    let tuples = json_route::route_line_from_str(
        r#"{"coordinates":[[-105.0,40.0,1600],[-105.0,40.01,1700]]}"#,
    )
    .unwrap();
    assert_eq!(tuples.points[0], Coord::with_ele(-105.0, 40.0, 1600.0));

    let points = json_route::route_line_from_str(
        r#"{"track":[
            {"latitude":"40.0","longitude":"-105.0","elevation_m":"1600"},
            {"lat":40.01,"lng":-105.0,"altitude":1700}
        ]}"#,
    )
    .unwrap();
    assert_eq!(points.points.len(), 2);
    assert_eq!(points.points[1].ele, Some(1700.0));
    assert!(points.length_m() > 1_000.0);
}

#[test]
fn route_file_import_preserves_provider_neutral_metadata() {
    let gpx = gpx::route_file_from_str(
        r#"<gpx version="1.1"><metadata><name>Fallback</name></metadata><trk><name>AllTrails Ridge</name><desc>Windy loop</desc><type>hike</type><trkseg><trkpt lat="40.0" lon="-105.0"><time>2026-07-01T12:00:00Z</time></trkpt><trkpt lat="40.01" lon="-105.0"/></trkseg></trk></gpx>"#,
    )
    .unwrap();
    assert_eq!(gpx.metadata.title.as_deref(), Some("AllTrails Ridge"));
    assert_eq!(gpx.metadata.description.as_deref(), Some("Windy loop"));
    assert_eq!(
        gpx.metadata.recorded_at.as_deref(),
        Some("2026-07-01T12:00:00Z")
    );
    assert_eq!(gpx.metadata.activity_type.as_deref(), Some("hike"));

    let kml = kml::route_file_from_str(
        r"<kml><Document><name>Doc</name><Placemark><name>Knife Edge</name><description>exposed</description><TimeStamp><when>2026-07-02</when></TimeStamp><LineString><coordinates>-105.0,40.0,0 -105.0,40.01,0</coordinates></LineString></Placemark></Document></kml>",
    )
    .unwrap();
    assert_eq!(kml.metadata.title.as_deref(), Some("Knife Edge"));
    assert_eq!(kml.metadata.description.as_deref(), Some("exposed"));
    assert_eq!(kml.metadata.recorded_at.as_deref(), Some("2026-07-02"));

    let geojson = geojson::route_file_from_str(
        r#"{"type":"Feature","properties":{"name":"Mesa Loop","description":"sunny","start_time":"2026-07-03","sport":"hiking"},"geometry":{"type":"LineString","coordinates":[[-105.0,40.0],[-105.0,40.01]]}}"#,
    )
    .unwrap();
    assert_eq!(geojson.metadata.title.as_deref(), Some("Mesa Loop"));
    assert_eq!(geojson.metadata.activity_type.as_deref(), Some("hiking"));

    let json = json_route::route_file_from_str(
        r#"{"title":"App Export","activity_type":"hike","created_at":"2026-07-04","coordinates":[[-105.0,40.0],[-105.0,40.01]]}"#,
    )
    .unwrap();
    assert_eq!(json.metadata.title.as_deref(), Some("App Export"));
    assert_eq!(json.metadata.recorded_at.as_deref(), Some("2026-07-04"));

    let csv = csv::route_file_from_str(
        "# title: Spreadsheet Loop\n# activity: hike\nlongitude,latitude\n-105.0,40.0\n-105.0,40.01\n",
    )
    .unwrap();
    assert_eq!(csv.metadata.title.as_deref(), Some("Spreadsheet Loop"));
    assert_eq!(csv.metadata.activity_type.as_deref(), Some("hike"));
}

#[test]
fn csv_round_trip_reads_exported_route() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let graph = GraphBuilder::default().build(&drafts).unwrap();
    let start = graph.nearest_vertex(Coord::new(-105.0, 40.0)).unwrap();
    let route = LoopHunter::default()
        .hunt(
            &graph,
            start,
            &LoopConstraints {
                min_distance_m: 3_000.0,
                max_distance_m: 8_000.0,
                ..LoopConstraints::default()
            },
            1,
        )
        .into_iter()
        .next()
        .unwrap();
    let text = csv::route_to_csv(&graph, &route);
    assert!(text.starts_with("longitude,latitude,elevation_m\n"));
    let line = csv::route_line_from_str(&text).unwrap();
    assert!(line.length_m() > 3_000.0);
}

#[test]
fn seed_routes_snap_and_raise_edge_confidence() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    let line = LineString::new(vec![
        Coord::with_ele(-105.0000, 40.0000, 1660.0),
        Coord::with_ele(-104.9880, 40.0000, 1680.0),
        Coord::with_ele(-104.9880, 40.0120, 1760.0),
        Coord::with_ele(-105.0000, 40.0000, 1660.0),
    ])
    .unwrap();
    let seed = SeedRoute::snap(&graph, "AllTrails Fixture", "fixture.gpx", "gpx", &line);
    assert!(seed.snapped_edges.len() >= 3);
    assert_eq!(seed.snap.rejected_segment_count, 0);
    assert_eq!(seed.snap.segment_count, 3);
    assert!(seed.snap.max_snap_m <= 1.0);
    graph.apply_seed_hints(&seed);
    graph.apply_seed_hints(&seed);
    assert!(
        seed.snapped_edges
            .iter()
            .all(|edge| graph.edges[edge.0].attr.seed_count == 1)
    );
    assert!(
        seed.snapped_edges
            .iter()
            .all(|edge| graph.edges[edge.0].attr.confidence >= 0.82)
    );
}

#[test]
fn route_snap_stats_reject_remote_lines() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let graph = GraphBuilder::default().build(&drafts).unwrap();
    let line = LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.0, 0.01)]).unwrap();
    let snap = graph.snap_line_edges_within(&line, 100.0);
    assert_eq!(snap.stats.segment_count, 1);
    assert_eq!(snap.stats.snapped_segment_count, 0);
    assert_eq!(snap.stats.rejected_segment_count, 1);
    assert!(snap.stats.max_snap_m > 1_000_000.0);
    assert!(snap.edges.is_empty());
}

#[test]
fn source_registry_classifies_local_inputs() {
    let adapters = adapter_registry();
    assert!(adapters.iter().any(|a| a.id == "geojson-network"));
    let gpx = classify_path(std::path::Path::new("sources/alltrails-export.gpx")).unwrap();
    assert_eq!(gpx.kind, SourceKind::SeedRoute);
    let csv = classify_path(std::path::Path::new("sources/alltrails-export.csv")).unwrap();
    assert_eq!(csv.kind, SourceKind::SeedRoute);
    assert_eq!(csv.adapter_id, "csv-route");
    let json = classify_path(std::path::Path::new("sources/app-route.json")).unwrap();
    assert_eq!(json.kind, SourceKind::SeedRoute);
    assert_eq!(json.adapter_id, "json-route");
    let kmz = classify_path(std::path::Path::new("sources/alltrails-export.kmz")).unwrap();
    assert_eq!(kmz.kind, SourceKind::SeedRoute);
    let access = classify_path(std::path::Path::new("sources/seasonal-access.geojson")).unwrap();
    assert_eq!(access.adapter_id, "geojson-access-overlay");
    let closure = classify_path(std::path::Path::new("sources/raptor-closure.geojson")).unwrap();
    assert_eq!(closure.adapter_id, "geojson-closure-overlay");
    let terrain = classify_path(std::path::Path::new("sources/landcover-terrain.geojson")).unwrap();
    assert_eq!(terrain.kind, SourceKind::Terrain);
    assert_eq!(terrain.adapter_id, "geojson-terrain-overlay");
    let road = classify_path(std::path::Path::new("sources/roads.geojson")).unwrap();
    assert_eq!(road.kind, SourceKind::Road);
    assert_eq!(road.adapter_id, "geojson-road-context");
    let water = classify_path(std::path::Path::new("sources/hydrology.geojson")).unwrap();
    assert_eq!(water.kind, SourceKind::Hydrology);
    assert_eq!(water.adapter_id, "geojson-hydrology-context");
    let dem = classify_path(std::path::Path::new("sources/dem.tif")).unwrap();
    assert_eq!(dem.kind, SourceKind::Elevation);
    assert_eq!(dem.adapter_id, "geotiff-elevation");
    let ascii_dem = classify_path(std::path::Path::new("sources/dem.asc")).unwrap();
    assert_eq!(ascii_dem.kind, SourceKind::Elevation);
    assert_eq!(ascii_dem.adapter_id, "arc-ascii-elevation");
    let shp_network = classify_path(std::path::Path::new("sources/trails.shp")).unwrap();
    assert_eq!(shp_network.kind, SourceKind::TrailNetwork);
    assert_eq!(shp_network.adapter_id, "shapefile-network");
    let osm_network = classify_path(std::path::Path::new("sources/osm-trails.osm")).unwrap();
    assert_eq!(osm_network.kind, SourceKind::TrailNetwork);
    assert_eq!(osm_network.adapter_id, "osm-xml-network");
    let osm_pbf_network =
        classify_path(std::path::Path::new("sources/osm-trails.osm.pbf")).unwrap();
    assert_eq!(osm_pbf_network.kind, SourceKind::TrailNetwork);
    assert_eq!(osm_pbf_network.adapter_id, "osm-pbf-network");
    let osm_roads = classify_path(std::path::Path::new("sources/roads.osm.pbf")).unwrap();
    assert_eq!(osm_roads.kind, SourceKind::Road);
    assert_eq!(osm_roads.adapter_id, "osm-road-context");
    let osm_hydrology = classify_path(std::path::Path::new("sources/streams.osm.pbf")).unwrap();
    assert_eq!(osm_hydrology.kind, SourceKind::Hydrology);
    assert_eq!(osm_hydrology.adapter_id, "osm-hydrology-context");
    let shp_access = classify_path(std::path::Path::new("sources/ownership-access.shp")).unwrap();
    assert_eq!(shp_access.kind, SourceKind::Access);
    assert_eq!(shp_access.adapter_id, "shapefile-access-overlay");
    let shp_closure = classify_path(std::path::Path::new("sources/raptor-closure.shp")).unwrap();
    assert_eq!(shp_closure.kind, SourceKind::Closure);
    assert_eq!(shp_closure.adapter_id, "shapefile-closure-layer");
    let shp_terrain = classify_path(std::path::Path::new("sources/landcover-terrain.shp")).unwrap();
    assert_eq!(shp_terrain.kind, SourceKind::Terrain);
    assert_eq!(shp_terrain.adapter_id, "shapefile-terrain-overlay");
    let shp_road = classify_path(std::path::Path::new("sources/roads.shp")).unwrap();
    assert_eq!(shp_road.kind, SourceKind::Road);
    assert_eq!(shp_road.adapter_id, "shapefile-road-context");
    let shp_water = classify_path(std::path::Path::new("sources/streams.shp")).unwrap();
    assert_eq!(shp_water.kind, SourceKind::Hydrology);
    assert_eq!(shp_water.adapter_id, "shapefile-hydrology-context");
}

#[test]
fn shapefile_adapters_normalize_networks_and_overlays() {
    let tmp = tempfile::tempdir().unwrap();
    let network = tmp.path().join("trails.shp");
    let access = tmp.path().join("access.shp");
    let permit = tmp.path().join("permit-access.shp");
    let terrain = tmp.path().join("terrain.shp");
    let nlcd = tmp.path().join("nlcd-landcover.shp");
    let road = tmp.path().join("roads.shp");
    let water = tmp.path().join("streams.shp");
    write_network_shapefile(&network);
    write_access_shapefile(&access);
    write_permit_access_shapefile(&permit);
    write_terrain_shapefile(&terrain);
    write_nlcd_shapefile(&nlcd);
    write_context_shapefile(&road, "road-1", "road");
    write_context_shapefile(&water, "stream-1", "water");

    let drafts = shp_io::network_from_path(&network).unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].terrain, Terrain::Trail);
    assert_eq!(drafts[0].surface.as_deref(), Some("dirt"));
    assert_eq!(drafts[0].access, Access::Open);
    assert_eq!(drafts[0].travel, EdgeTravel::Backward);
    assert_eq!(drafts[0].provenance.source, "agency");
    assert_eq!(drafts[0].provenance.source_id.as_deref(), Some("trail-1"));

    let overlays = shp_io::access_overlays_from_path(&access).unwrap();
    assert_eq!(overlays.len(), 1);
    assert_eq!(overlays[0].access, Access::Closed);
    assert_eq!(overlays[0].travel, Some(EdgeTravel::Forward));
    assert_eq!(overlays[0].name, "closure-1");
    assert!(overlays[0].active_at(Some(PlanningMoment::new(
        Some(PlanningDate::new(2026, 7, 4).unwrap()),
        Some("12:00".parse().unwrap())
    ))));
    assert!(!overlays[0].active_at(Some(PlanningMoment::new(
        Some(PlanningDate::new(2026, 7, 4).unwrap()),
        Some("18:00".parse().unwrap())
    ))));
    assert!(!overlays[0].active_at(Some(PlanningMoment::new(
        Some(PlanningDate::new(2026, 7, 6).unwrap()),
        Some("12:00".parse().unwrap())
    ))));
    assert!(matches!(
        overlays[0].geometry,
        trailgen_core::OverlayGeometry::Polygon(_)
    ));

    let permits = shp_io::access_overlays_from_path(&permit).unwrap();
    assert_eq!(permits.len(), 1);
    assert_eq!(permits[0].access, Access::Restricted);
    assert!(permits[0].active_at(Some(PlanningMoment::new(
        Some(PlanningDate::new(2026, 9, 1).unwrap()),
        Some("09:00".parse().unwrap())
    ))));
    assert!(!permits[0].active_at(Some(PlanningMoment::new(
        Some(PlanningDate::new(2026, 9, 1).unwrap()),
        Some("17:30".parse().unwrap())
    ))));

    let terrain_overlays = shp_io::terrain_overlays_from_path(&terrain).unwrap();
    assert_eq!(terrain_overlays.len(), 1);
    assert_eq!(terrain_overlays[0].terrain, Terrain::Talus);
    assert_eq!(terrain_overlays[0].surface.as_deref(), Some("scree"));
    assert!(matches!(
        terrain_overlays[0].geometry,
        trailgen_core::OverlayGeometry::Polygon(_)
    ));

    let nlcd_overlays = shp_io::terrain_overlays_from_path(&nlcd).unwrap();
    assert_eq!(nlcd_overlays.len(), 1);
    assert_eq!(nlcd_overlays[0].terrain, Terrain::Forest);

    let road_overlays = shp_io::context_overlays_from_path(&road).unwrap();
    assert_eq!(road_overlays.len(), 1);
    assert_eq!(road_overlays[0].kind, CrossingKind::Road);
    assert_eq!(road_overlays[0].provenance.source, "agency-roads");

    let water_overlays = shp_io::context_overlays_from_path(&water).unwrap();
    assert_eq!(water_overlays.len(), 1);
    assert_eq!(water_overlays[0].kind, CrossingKind::Water);
    assert_eq!(water_overlays[0].provenance.source, "agency-hydrology");
}

#[test]
fn vector_adapters_reproject_declared_projected_crs_and_reject_unsupported_projections() {
    let web_mercator_geojson = r#"{
      "type": "FeatureCollection",
      "crs": {"type": "name", "properties": {"name": "EPSG:3857"}},
      "features": [{
        "type": "Feature",
        "properties": {"terrain": "trail"},
        "geometry": {"type": "LineString", "coordinates": [[0,0], [1113.1949079327358,0]]}
      }]
    }"#;
    let drafts = geojson::network_from_str(web_mercator_geojson).unwrap();
    assert!((drafts[0].geometry.points[1].lon - 0.01).abs() < 1.0e-12);
    assert!(drafts[0].geometry.points[1].lat.abs() < 1.0e-12);

    let utm_start =
        trailgen_core::crs::wgs84_to_utm(Coord::new(-104.995, 40.005), 13, true).unwrap();
    let utm_end = trailgen_core::crs::wgs84_to_utm(Coord::new(-104.985, 40.005), 13, true).unwrap();
    let utm_geojson = format!(
        r#"{{
      "type": "FeatureCollection",
      "crs": {{"type": "name", "properties": {{"name": "EPSG:32613"}}}},
      "features": [{{
        "type": "Feature",
        "properties": {{"terrain": "trail"}},
        "geometry": {{"type": "LineString", "coordinates": [[{},{}], [{},{}]]}}
      }}]
    }}"#,
        utm_start.0, utm_start.1, utm_end.0, utm_end.1
    );
    let drafts = geojson::network_from_str(&utm_geojson).unwrap();
    assert!((drafts[0].geometry.points[0].lon + 104.995).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[0].lat - 40.005).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[1].lon + 104.985).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[1].lat - 40.005).abs() < 1.0e-7);

    let nad83_utm = trailgen_core::crs::UtmCrs::from_epsg(26913).unwrap();
    let nad83_utm_start =
        trailgen_core::crs::geographic_to_utm(Coord::new(-104.995, 40.005), nad83_utm).unwrap();
    let nad83_utm_end =
        trailgen_core::crs::geographic_to_utm(Coord::new(-104.985, 40.005), nad83_utm).unwrap();
    let nad83_utm_geojson = format!(
        r#"{{
      "type": "FeatureCollection",
      "crs": {{"type": "name", "properties": {{"name": "EPSG:26913"}}}},
      "features": [{{
        "type": "Feature",
        "properties": {{"terrain": "trail"}},
        "geometry": {{"type": "LineString", "coordinates": [[{},{}], [{},{}]]}}
      }}]
    }}"#,
        nad83_utm_start.0, nad83_utm_start.1, nad83_utm_end.0, nad83_utm_end.1
    );
    let drafts = geojson::network_from_str(&nad83_utm_geojson).unwrap();
    assert!((drafts[0].geometry.points[0].lon + 104.995).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[0].lat - 40.005).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[1].lon + 104.985).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[1].lat - 40.005).abs() < 1.0e-7);

    let unsupported_geojson = r#"{
      "type": "FeatureCollection",
      "crs": {"type": "name", "properties": {"name": "EPSG:32154"}},
      "features": []
    }"#;
    let error = geojson::network_from_str(unsupported_geojson).unwrap_err();
    assert!(format!("{error}").contains("not supported"));

    let tmp = tempfile::tempdir().unwrap();
    let network = tmp.path().join("trails.shp");
    write_network_shapefile_points(
        &network,
        ::shapefile::Point::new(0.0, 0.0),
        ::shapefile::Point::new(WEB_MERCATOR_0_01_LON_M, 0.0),
    );
    std::fs::write(network.with_extension("prj"), WEB_MERCATOR_PRJ).unwrap();
    let drafts = shp_io::network_from_path(&network).unwrap();
    assert!((drafts[0].geometry.points[1].lon - 0.01).abs() < 1.0e-12);

    let utm = tmp.path().join("utm-trails.shp");
    write_network_shapefile_points(
        &utm,
        ::shapefile::Point::new(utm_start.0, utm_start.1),
        ::shapefile::Point::new(utm_end.0, utm_end.1),
    );
    std::fs::write(utm.with_extension("prj"), UTM_PRJ).unwrap();
    let drafts = shp_io::network_from_path(&utm).unwrap();
    assert!((drafts[0].geometry.points[0].lon + 104.995).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[0].lat - 40.005).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[1].lon + 104.985).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[1].lat - 40.005).abs() < 1.0e-7);

    let nad83_utm = tmp.path().join("nad83-utm-trails.shp");
    write_network_shapefile_points(
        &nad83_utm,
        ::shapefile::Point::new(nad83_utm_start.0, nad83_utm_start.1),
        ::shapefile::Point::new(nad83_utm_end.0, nad83_utm_end.1),
    );
    std::fs::write(nad83_utm.with_extension("prj"), NAD83_UTM_PRJ).unwrap();
    let drafts = shp_io::network_from_path(&nad83_utm).unwrap();
    assert!((drafts[0].geometry.points[0].lon + 104.995).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[0].lat - 40.005).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[1].lon + 104.985).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[1].lat - 40.005).abs() < 1.0e-7);

    let unsupported = tmp.path().join("nad83-lambert-trails.shp");
    write_network_shapefile(&unsupported);
    std::fs::write(unsupported.with_extension("prj"), NAD83_LAMBERT_PRJ).unwrap();
    let error = shp_io::network_from_path(&unsupported).unwrap_err();
    assert!(format!("{error}").contains("unsupported projected CRS"));
}

#[test]
fn shapefile_adapter_accepts_wgs84_prj() {
    let tmp = tempfile::tempdir().unwrap();
    let network = tmp.path().join("trails.shp");
    write_network_shapefile(&network);
    std::fs::write(network.with_extension("prj"), WGS84_PRJ).unwrap();

    assert_eq!(shp_io::network_from_path(&network).unwrap().len(), 1);
}

#[test]
fn source_coverage_evaluates_recommendations_against_candidates() {
    let adapters = adapter_registry();
    let recommendations = discovery_recommendations(Some(trailhead_bounds()));
    let trail_recommendation = recommendations
        .iter()
        .find(|entry| entry.kind == SourceKind::TrailNetwork)
        .expect("trail network recommendation");
    assert_eq!(trail_recommendation.area, Some(trailhead_bounds()));
    assert!(trail_recommendation.acquisition_hints.iter().any(|hint| {
        hint.label.contains("NPS") && hint.formats.iter().any(|format| format == "GeoJSON")
    }));
    let elevation_recommendation = recommendations
        .iter()
        .find(|entry| entry.kind == SourceKind::Elevation)
        .expect("elevation recommendation");
    assert!(
        elevation_recommendation
            .acquisition_hints
            .iter()
            .any(|hint| { hint.url.contains("nationalmap.gov") || hint.url.contains("usgs.gov") })
    );
    let candidates = vec![
        classify_path(std::path::Path::new("sources/network.geojson")).unwrap(),
        classify_path(std::path::Path::new("sources/dem.tif")).unwrap(),
    ];
    let coverage = source_coverage(&adapters, &recommendations, &candidates);

    let trail = coverage
        .iter()
        .find(|entry| entry.kind == SourceKind::TrailNetwork)
        .expect("trail network coverage");
    assert_eq!(trail.status, SourceCoverageStatus::Satisfied);
    assert_eq!(trail.candidate_paths, vec!["sources/network.geojson"]);

    let elevation = coverage
        .iter()
        .find(|entry| entry.kind == SourceKind::Elevation)
        .expect("elevation coverage");
    assert_eq!(elevation.status, SourceCoverageStatus::Satisfied);
    assert_eq!(elevation.implemented_adapter_ids, vec!["geotiff-elevation"]);

    let hydrology = coverage
        .iter()
        .find(|entry| entry.kind == SourceKind::Hydrology)
        .expect("hydrology coverage");
    assert_eq!(hydrology.status, SourceCoverageStatus::Missing);
    assert!(hydrology.message.contains("sources/hydrology.geojson"));

    let summary = summarize_source_coverage(&coverage);
    assert!(summary.required_complete());
    assert!(!summary.recommended_complete());
    assert_eq!(summary.required.satisfied, 2);
    assert_eq!(summary.recommended.satisfied, 0);
    assert!(summary.missing_required.is_empty());
    assert!(summary.missing_recommended.contains(&SourceKind::Hydrology));
}

const fn trailhead_bounds() -> trailgen_core::source::GeoBounds {
    trailgen_core::source::GeoBounds::new(-105.02, 39.99, -104.98, 40.02)
}

fn write_network_shapefile(path: &std::path::Path) {
    write_network_shapefile_points(
        path,
        ::shapefile::Point::new(0.0, 0.0),
        ::shapefile::Point::new(0.01, 0.0),
    );
}

fn write_network_shapefile_points(
    path: &std::path::Path,
    a: ::shapefile::Point,
    b: ::shapefile::Point,
) {
    let table = ::shapefile::dbase::TableWriterBuilder::new()
        .add_character_field("id".try_into().unwrap(), 32)
        .add_character_field("terrain".try_into().unwrap(), 16)
        .add_character_field("surface".try_into().unwrap(), 16)
        .add_character_field("access".try_into().unwrap(), 16)
        .add_character_field("direction".try_into().unwrap(), 16)
        .add_character_field("source".try_into().unwrap(), 32)
        .add_numeric_field("confidence".try_into().unwrap(), 5, 2);
    let mut writer = ::shapefile::Writer::from_path(path, table).unwrap();
    let line = ::shapefile::Polyline::new(vec![a, b]);
    let mut record = ::shapefile::dbase::Record::default();
    record.insert("id".to_owned(), "trail-1".to_owned().into());
    record.insert("terrain".to_owned(), "trail".to_owned().into());
    record.insert("surface".to_owned(), "dirt".to_owned().into());
    record.insert("access".to_owned(), "open".to_owned().into());
    record.insert("direction".to_owned(), "backward".to_owned().into());
    record.insert("source".to_owned(), "agency".to_owned().into());
    record.insert("confidence".to_owned(), 0.91.into());
    writer.write_shape_and_record(&line, &record).unwrap();
}

fn write_access_shapefile(path: &std::path::Path) {
    let table = ::shapefile::dbase::TableWriterBuilder::new()
        .add_character_field("name".try_into().unwrap(), 32)
        .add_character_field("status".try_into().unwrap(), 16)
        .add_character_field("travel".try_into().unwrap(), 16)
        .add_character_field("weekdays".try_into().unwrap(), 16)
        .add_character_field("time_from".try_into().unwrap(), 8)
        .add_character_field("time_to".try_into().unwrap(), 8)
        .add_character_field("source".try_into().unwrap(), 32);
    let mut writer = ::shapefile::Writer::from_path(path, table).unwrap();
    let polygon = ::shapefile::Polygon::with_rings(vec![::shapefile::PolygonRing::Outer(vec![
        ::shapefile::Point::new(-0.001, -0.001),
        ::shapefile::Point::new(0.011, -0.001),
        ::shapefile::Point::new(0.011, 0.001),
        ::shapefile::Point::new(-0.001, 0.001),
        ::shapefile::Point::new(-0.001, -0.001),
    ])]);
    let mut record = ::shapefile::dbase::Record::default();
    record.insert("name".to_owned(), "closure-1".to_owned().into());
    record.insert("status".to_owned(), "closed".to_owned().into());
    record.insert("travel".to_owned(), "forward".to_owned().into());
    record.insert("weekdays".to_owned(), "weekends".to_owned().into());
    record.insert("time_from".to_owned(), "08:00".to_owned().into());
    record.insert("time_to".to_owned(), "17:00".to_owned().into());
    record.insert("source".to_owned(), "agency".to_owned().into());
    writer.write_shape_and_record(&polygon, &record).unwrap();
}

fn write_permit_access_shapefile(path: &std::path::Path) {
    let table = ::shapefile::dbase::TableWriterBuilder::new()
        .add_character_field("name".try_into().unwrap(), 32)
        .add_character_field("permit_req".try_into().unwrap(), 8)
        .add_character_field("time_from".try_into().unwrap(), 8)
        .add_character_field("time_to".try_into().unwrap(), 8)
        .add_character_field("source".try_into().unwrap(), 32);
    let mut writer = ::shapefile::Writer::from_path(path, table).unwrap();
    let polygon = ::shapefile::Polygon::with_rings(vec![::shapefile::PolygonRing::Outer(vec![
        ::shapefile::Point::new(-0.001, -0.001),
        ::shapefile::Point::new(0.011, -0.001),
        ::shapefile::Point::new(0.011, 0.001),
        ::shapefile::Point::new(-0.001, 0.001),
        ::shapefile::Point::new(-0.001, -0.001),
    ])]);
    let mut record = ::shapefile::dbase::Record::default();
    record.insert("name".to_owned(), "timed-entry".to_owned().into());
    record.insert("permit_req".to_owned(), "yes".to_owned().into());
    record.insert("time_from".to_owned(), "08:00".to_owned().into());
    record.insert("time_to".to_owned(), "17:00".to_owned().into());
    record.insert("source".to_owned(), "agency-permits".to_owned().into());
    writer.write_shape_and_record(&polygon, &record).unwrap();
}

fn write_terrain_shapefile(path: &std::path::Path) {
    let table = ::shapefile::dbase::TableWriterBuilder::new()
        .add_character_field("name".try_into().unwrap(), 32)
        .add_character_field("terrain".try_into().unwrap(), 16)
        .add_character_field("surface".try_into().unwrap(), 16)
        .add_character_field("source".try_into().unwrap(), 32);
    let mut writer = ::shapefile::Writer::from_path(path, table).unwrap();
    let polygon = ::shapefile::Polygon::with_rings(vec![::shapefile::PolygonRing::Outer(vec![
        ::shapefile::Point::new(-0.001, -0.001),
        ::shapefile::Point::new(0.011, -0.001),
        ::shapefile::Point::new(0.011, 0.001),
        ::shapefile::Point::new(-0.001, 0.001),
        ::shapefile::Point::new(-0.001, -0.001),
    ])]);
    let mut record = ::shapefile::dbase::Record::default();
    record.insert("name".to_owned(), "talus-1".to_owned().into());
    record.insert("terrain".to_owned(), "talus".to_owned().into());
    record.insert("surface".to_owned(), "scree".to_owned().into());
    record.insert("source".to_owned(), "agency-terrain".to_owned().into());
    writer.write_shape_and_record(&polygon, &record).unwrap();
}

fn write_nlcd_shapefile(path: &std::path::Path) {
    let table = ::shapefile::dbase::TableWriterBuilder::new()
        .add_character_field("name".try_into().unwrap(), 32)
        .add_numeric_field("nlcd".try_into().unwrap(), 5, 0)
        .add_character_field("source".try_into().unwrap(), 32);
    let mut writer = ::shapefile::Writer::from_path(path, table).unwrap();
    let polygon = ::shapefile::Polygon::with_rings(vec![::shapefile::PolygonRing::Outer(vec![
        ::shapefile::Point::new(-0.001, -0.001),
        ::shapefile::Point::new(0.011, -0.001),
        ::shapefile::Point::new(0.011, 0.001),
        ::shapefile::Point::new(-0.001, 0.001),
        ::shapefile::Point::new(-0.001, -0.001),
    ])]);
    let mut record = ::shapefile::dbase::Record::default();
    record.insert("name".to_owned(), "nlcd-forest".to_owned().into());
    record.insert("nlcd".to_owned(), 41.0.into());
    record.insert("source".to_owned(), "nlcd-fixture".to_owned().into());
    writer.write_shape_and_record(&polygon, &record).unwrap();
}

fn write_context_shapefile(path: &std::path::Path, name: &str, kind: &str) {
    let table = ::shapefile::dbase::TableWriterBuilder::new()
        .add_character_field("name".try_into().unwrap(), 32)
        .add_character_field("kind".try_into().unwrap(), 16)
        .add_character_field("source".try_into().unwrap(), 32);
    let mut writer = ::shapefile::Writer::from_path(path, table).unwrap();
    let line = ::shapefile::Polyline::new(vec![
        ::shapefile::Point::new(0.005, -0.001),
        ::shapefile::Point::new(0.005, 0.001),
    ]);
    let mut record = ::shapefile::dbase::Record::default();
    record.insert("name".to_owned(), name.to_owned().into());
    record.insert("kind".to_owned(), kind.to_owned().into());
    let source = if kind == "water" {
        "agency-hydrology"
    } else {
        "agency-roads"
    };
    record.insert("source".to_owned(), source.to_owned().into());
    writer.write_shape_and_record(&line, &record).unwrap();
}

#[test]
fn alltrails_bridge_refuses_undocumented_write_api() {
    let caps = ManualAllTrailsBridge.capabilities();
    assert!(
        caps.iter()
            .all(|cap| cap.verified_on == ALLTRAILS_POLICY_VERIFIED_ON)
    );
    assert!(caps.iter().any(|cap| {
        cap.exchange == AllTrailsExchange::ManualUploadCustomRoute
            && cap.status == BridgeStatus::Manual
            && cap.formats.contains(&RouteExchangeFormat::Gpx)
    }));
    assert!(caps.iter().any(|cap| {
        cap.exchange == AllTrailsExchange::ManualUploadActivity
            && cap.status == BridgeStatus::Manual
            && cap.formats.contains(&RouteExchangeFormat::Csv)
    }));
    assert!(caps.iter().any(|cap| {
        cap.exchange == AllTrailsExchange::DirectWriteApi
            && cap.status == BridgeStatus::Undocumented
    }));

    let import_plan = ManualAllTrailsBridge.plan(AllTrailsRequest {
        exchange: AllTrailsExchange::ImportUserExport,
        format: RouteExchangeFormat::Geojson,
    });
    assert_eq!(import_plan.status, BridgeStatus::Supported);
    assert_eq!(
        import_plan.trailgen_action,
        TrailgenExchangeAction::ImportSeed
    );
    assert_eq!(import_plan.verified_on, ALLTRAILS_POLICY_VERIFIED_ON);
    assert!(
        import_plan
            .trailgen_template
            .contains("trailgen import-seed <project> --route alltrails-export.geojson")
    );

    let upload_plan = ManualAllTrailsBridge.plan(AllTrailsRequest {
        exchange: AllTrailsExchange::ManualUploadCustomRoute,
        format: RouteExchangeFormat::Kmz,
    });
    assert_eq!(upload_plan.status, BridgeStatus::Manual);
    assert_eq!(
        upload_plan.trailgen_action,
        TrailgenExchangeAction::ExportGeneratedRoute
    );
    assert_eq!(upload_plan.verified_on, ALLTRAILS_POLICY_VERIFIED_ON);
    assert!(
        upload_plan
            .trailgen_template
            .contains("trailgen export <project> --route candidate-1 --format kmz")
    );

    let direct_write = ManualAllTrailsBridge.plan(AllTrailsRequest {
        exchange: AllTrailsExchange::DirectWriteApi,
        format: RouteExchangeFormat::Gpx,
    });
    assert_eq!(direct_write.status, BridgeStatus::Undocumented);
    assert_eq!(
        direct_write.trailgen_action,
        TrailgenExchangeAction::Unsupported
    );
    assert_eq!(direct_write.verified_on, ALLTRAILS_POLICY_VERIFIED_ON);
}

#[test]
fn elevation_enrichment_densifies_rates_and_infers_terrain() {
    let draft = SegmentDraft {
        geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.0, 0.01)]).unwrap(),
        terrain: Terrain::Unknown,
        terrain_confidence: None,
        surface: None,
        access: Access::Open,
        travel: EdgeTravel::Both,
        road_exposure: 0.0,
        confidence: 0.9,
        provenance: Provenance::fixture("climb"),
    };
    let mut graph = GraphBuilder::default().build(&[draft]).unwrap();
    enrich_with_north_plane(&mut graph, 40_000.0, 0.77);

    let edge = &graph.edges[0];
    assert!(edge.geometry.points.len() > 10);
    assert!(edge.attr.ascent_m > 390.0);
    assert!(edge.attr.grade_abs_max > 0.30);
    assert!(edge.attr.sustained_steep_m > 1_000.0);
    assert!(edge.attr.grade_distribution.savage_m > 1_000.0);
    assert!((edge.attr.grade_distribution.total_m() - edge.attr.length_m).abs() < 1.0);
    assert_eq!(edge.attr.terrain, Terrain::Scramble);
    assert!(
        edge.attr
            .elevation_provenance
            .iter()
            .any(|p| p.source == "synthetic-plane-elevation")
    );
    let terrain_evidence = edge
        .attr
        .terrain_evidence
        .iter()
        .filter(|e| e.terrain == Terrain::Scramble)
        .collect::<Vec<_>>();
    assert_eq!(terrain_evidence.len(), 1);
    assert!((terrain_evidence[0].confidence - 0.52).abs() <= 1.0e-12);
    assert!(
        terrain_evidence[0]
            .rationale
            .contains("inferred from savage sampled grade")
    );
    assert!(terrain_evidence[0].rationale.contains("max grade"));
    assert!(
        terrain_evidence[0]
            .provenance
            .as_ref()
            .is_some_and(|p| p.source == "synthetic-plane-elevation")
    );

    enrich_with_north_plane(&mut graph, 40_000.0, 0.77);
    assert_eq!(
        graph.edges[0]
            .attr
            .terrain_evidence
            .iter()
            .filter(|e| e.terrain == Terrain::Scramble)
            .count(),
        1
    );

    enrich_with_north_plane(&mut graph, 0.0, 0.80);
    let reenriched = &graph.edges[0];
    assert_eq!(reenriched.attr.terrain, Terrain::Trail);
    assert_eq!(
        reenriched
            .attr
            .terrain_evidence
            .iter()
            .filter(|e| e.terrain == Terrain::Scramble)
            .count(),
        0
    );
    assert!(
        reenriched
            .attr
            .terrain_evidence
            .iter()
            .any(|e| e.terrain == Terrain::Trail
                && e.rationale.contains("inferred default hiking surface"))
    );
}

fn enrich_with_north_plane(graph: &mut TrailGraph, north_gain_m_per_degree: f64, confidence: f64) {
    enrich_graph(
        graph,
        &PlaneElevation {
            origin: Coord::new(0.0, 0.0),
            origin_ele_m: 1_000.0,
            east_gain_m_per_degree: 0.0,
            north_gain_m_per_degree,
            confidence,
        },
        EnrichmentConfig {
            sample_spacing_m: 50.0,
            steep_grade_threshold: 0.15,
        },
        DifficultyWeights::default(),
    )
    .unwrap();
}

#[test]
fn arc_ascii_grid_samples_and_reenriches_graph() {
    let raster = ArcAsciiGrid::parse(
        include_str!("fixtures/mini_dem.asc"),
        Provenance {
            source: "fixture-dem".to_owned(),
            layer: Some("arc-ascii".to_owned()),
            source_id: Some("mini_dem.asc".to_owned()),
            license: Some("CC0-fixture".to_owned()),
        },
        0.81,
    )
    .unwrap();
    let sample = raster.sample(Coord::new(-104.995, 40.005)).unwrap();
    assert!((sample.ele_m - 1680.0).abs() <= 1.0e-9);
    assert!(raster.sample(Coord::new(-106.0, 40.0)).is_none());

    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    enrich_graph(
        &mut graph,
        &raster,
        EnrichmentConfig {
            sample_spacing_m: 100.0,
            steep_grade_threshold: 0.10,
        },
        DifficultyWeights::default(),
    )
    .unwrap();
    assert!(graph.edges.iter().any(|edge| {
        edge.attr
            .elevation_provenance
            .iter()
            .any(|p| p.source == "fixture-dem")
    }));
    assert!(graph.edges.iter().any(|edge| edge.attr.grade_abs_max > 0.0));
}

#[test]
fn geotiff_dem_samples_and_reenriches_graph() {
    let tmp = tempfile::tempdir().unwrap();
    let dem = tmp.path().join("mini_dem.tif");
    write_geotiff_dem(&dem);
    let raster = GeoTiffDem::from_path(
        &dem,
        Provenance {
            source: "fixture-geotiff-dem".to_owned(),
            layer: Some("geotiff".to_owned()),
            source_id: Some("mini_dem.tif".to_owned()),
            license: Some("CC0-fixture".to_owned()),
        },
        0.83,
    )
    .unwrap();
    assert_eq!(raster.width, 3);
    assert_eq!(raster.height, 3);
    assert_eq!(raster.crs, RasterCrs::Wgs84Degrees);
    let sample = raster.sample(Coord::new(-104.995, 40.005)).unwrap();
    assert!((sample.ele_m - 1_600.0).abs() <= 1.0e-9);
    assert!(raster.sample(Coord::new(-106.0, 40.0)).is_none());

    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    enrich_graph(
        &mut graph,
        &raster,
        EnrichmentConfig {
            sample_spacing_m: 100.0,
            steep_grade_threshold: 0.10,
        },
        DifficultyWeights::default(),
    )
    .unwrap();
    assert!(graph.edges.iter().any(|edge| {
        edge.attr
            .elevation_provenance
            .iter()
            .any(|p| p.source == "fixture-geotiff-dem")
    }));
    assert!(graph.edges.iter().any(|edge| edge.attr.grade_abs_max > 0.0));
}

#[test]
fn rotated_geotiff_dem_samples_through_model_transformation() {
    let tmp = tempfile::tempdir().unwrap();
    let dem = tmp.path().join("rotated_dem.tif");
    write_rotated_geotiff_dem(&dem);
    let raster = GeoTiffDem::from_path(
        &dem,
        Provenance {
            source: "fixture-rotated-geotiff-dem".to_owned(),
            layer: Some("geotiff".to_owned()),
            source_id: Some("rotated_dem.tif".to_owned()),
            license: Some("CC0-fixture".to_owned()),
        },
        0.82,
    )
    .unwrap();

    assert_eq!(raster.crs, RasterCrs::Wgs84Degrees);
    assert!(raster.transform.is_some());
    let sample = raster.sample(Coord::new(-104.995, 40.005)).unwrap();
    assert!((sample.ele_m - 1_600.0).abs() <= 1.0e-9);
    assert!(raster.sample(Coord::new(-106.0, 40.0)).is_none());
}

#[test]
fn web_mercator_geotiff_dem_samples_and_rejects_other_projected_crs() {
    let tmp = tempfile::tempdir().unwrap();
    let dem = tmp.path().join("web_mercator_dem.tif");
    write_web_mercator_geotiff_dem(&dem, 3857);
    let raster = GeoTiffDem::from_path(
        &dem,
        Provenance {
            source: "fixture-web-mercator-dem".to_owned(),
            layer: Some("geotiff".to_owned()),
            source_id: Some("web_mercator_dem.tif".to_owned()),
            license: Some("CC0-fixture".to_owned()),
        },
        0.79,
    )
    .unwrap();
    assert_eq!(raster.crs, RasterCrs::WebMercatorMeters);
    let sample = raster.sample(Coord::new(-104.995, 40.005)).unwrap();
    assert!((sample.ele_m - 1_600.0).abs() <= 1.0e-9);
    assert!(raster.sample(Coord::new(-106.0, 40.0)).is_none());

    let unsupported = tmp.path().join("lambert_dem.tif");
    write_web_mercator_geotiff_dem(&unsupported, 32154);
    let error = GeoTiffDem::from_path(
        &unsupported,
        Provenance {
            source: "fixture-lambert-dem".to_owned(),
            layer: Some("geotiff".to_owned()),
            source_id: Some("lambert_dem.tif".to_owned()),
            license: Some("CC0-fixture".to_owned()),
        },
        0.79,
    )
    .unwrap_err();
    assert!(format!("{error}").contains("WGS84/NAD83 UTM"));
}

#[test]
fn utm_geotiff_dem_samples_projected_source() {
    let tmp = tempfile::tempdir().unwrap();
    let dem = tmp.path().join("utm_dem.tif");
    write_utm_geotiff_dem(&dem, 32613);
    let raster = GeoTiffDem::from_path(
        &dem,
        Provenance {
            source: "fixture-utm-dem".to_owned(),
            layer: Some("geotiff".to_owned()),
            source_id: Some("utm_dem.tif".to_owned()),
            license: Some("CC0-fixture".to_owned()),
        },
        0.79,
    )
    .unwrap();
    assert_eq!(
        raster.crs,
        RasterCrs::UtmMeters(trailgen_core::crs::UtmCrs::from_epsg(32613).unwrap())
    );
    let sample = raster.sample(Coord::new(-104.995, 40.005)).unwrap();
    assert!((sample.ele_m - 1_600.0).abs() <= 1.0e-9);
    assert!(raster.sample(Coord::new(-106.0, 40.0)).is_none());
}

#[test]
fn nad83_utm_geotiff_dem_samples_projected_source() {
    let tmp = tempfile::tempdir().unwrap();
    let dem = tmp.path().join("nad83_utm_dem.tif");
    write_utm_geotiff_dem(&dem, 26913);
    let raster = GeoTiffDem::from_path(
        &dem,
        Provenance {
            source: "fixture-nad83-utm-dem".to_owned(),
            layer: Some("geotiff".to_owned()),
            source_id: Some("nad83_utm_dem.tif".to_owned()),
            license: Some("CC0-fixture".to_owned()),
        },
        0.79,
    )
    .unwrap();
    assert_eq!(
        raster.crs,
        RasterCrs::UtmMeters(trailgen_core::crs::UtmCrs::from_epsg(26913).unwrap())
    );
    let sample = raster.sample(Coord::new(-104.995, 40.005)).unwrap();
    assert!((sample.ele_m - 1_600.0).abs() <= 1.0e-9);
    assert!(raster.sample(Coord::new(-106.0, 40.0)).is_none());
}

#[test]
fn vrt_dem_wraps_geotiff_source_and_samples() {
    let tmp = tempfile::tempdir().unwrap();
    let tif = tmp.path().join("mini_dem.tif");
    let vrt = tmp.path().join("mini_dem.vrt");
    write_geotiff_dem(&tif);
    write_vrt_dem(&vrt, "mini_dem.tif");

    let raster = VrtDem::from_path(
        &vrt,
        Provenance {
            source: "fixture-vrt-dem".to_owned(),
            layer: Some("vrt".to_owned()),
            source_id: Some("mini_dem.vrt".to_owned()),
            license: None,
        },
        0.76,
    )
    .unwrap();

    assert_eq!(raster.width, 3);
    assert_eq!(raster.height, 3);
    assert_eq!(raster.crs, RasterCrs::Wgs84Degrees);
    assert!(raster.transform.is_some());
    assert!(raster.source_filename.ends_with("mini_dem.tif"));
    assert!(raster.sample(Coord::new(-105.0, 40.01)).is_some_and(
        |sample| sample.ele_m > 1_500.0 && (sample.confidence - 0.76).abs() <= f64::EPSILON
    ));
    assert_eq!(VrtDem::referenced_sources(&vrt).unwrap(), vec![tif]);
}

#[test]
fn rotated_vrt_dem_samples_through_affine_geotransform() {
    let tmp = tempfile::tempdir().unwrap();
    let tif = tmp.path().join("rotated_dem.tif");
    let vrt = tmp.path().join("rotated_dem.vrt");
    write_rotated_geotiff_dem(&tif);
    write_vrt_dem_with_geotransform(
        &vrt,
        "rotated_dem.tif",
        rotated_fixture_geotransform(),
        Some("EPSG:4326"),
    );

    let raster = VrtDem::from_path(
        &vrt,
        Provenance {
            source: "fixture-rotated-vrt-dem".to_owned(),
            layer: Some("vrt".to_owned()),
            source_id: Some("rotated_dem.vrt".to_owned()),
            license: None,
        },
        0.75,
    )
    .unwrap();

    assert_eq!(raster.crs, RasterCrs::Wgs84Degrees);
    assert!(raster.transform.is_some());
    let sample = raster.sample(Coord::new(-104.995, 40.005)).unwrap();
    assert!((sample.ele_m - 1_600.0).abs() <= 1.0e-9);
    assert!(raster.sample(Coord::new(-106.0, 40.0)).is_none());
}

#[test]
fn web_mercator_vrt_dem_samples_projected_source() {
    let tmp = tempfile::tempdir().unwrap();
    let tif = tmp.path().join("web_mercator_dem.tif");
    let vrt = tmp.path().join("web_mercator_dem.vrt");
    write_web_mercator_geotiff_dem(&tif, 3857);
    let scale = 1_000.0;
    let (x, y) = web_mercator_xy(Coord::new(-104.995, 40.005));
    let origin_x = 1.5_f64.mul_add(-scale, x);
    let origin_y = 1.5_f64.mul_add(scale, y);
    write_vrt_dem_with_geotransform(
        &vrt,
        "web_mercator_dem.tif",
        [origin_x, scale, 0.0, origin_y, 0.0, -scale],
        Some("EPSG:3857"),
    );

    let raster = VrtDem::from_path(
        &vrt,
        Provenance {
            source: "fixture-vrt-dem".to_owned(),
            layer: Some("vrt".to_owned()),
            source_id: Some("web_mercator_dem.vrt".to_owned()),
            license: None,
        },
        0.76,
    )
    .unwrap();
    assert_eq!(raster.crs, RasterCrs::WebMercatorMeters);
    let sample = raster.sample(Coord::new(-104.995, 40.005)).unwrap();
    assert!((sample.ele_m - 1_600.0).abs() <= 1.0e-9);
    assert!(raster.sample(Coord::new(-106.0, 40.0)).is_none());
}

#[test]
fn utm_vrt_dem_samples_projected_source() {
    let tmp = tempfile::tempdir().unwrap();
    let tif = tmp.path().join("utm_dem.tif");
    let vrt = tmp.path().join("utm_dem.vrt");
    write_utm_geotiff_dem(&tif, 32613);
    let [origin_x, scale, _, origin_y, _, dy] = utm_fixture_geotransform(32613);
    write_vrt_dem_with_geotransform(
        &vrt,
        "utm_dem.tif",
        [origin_x, scale, 0.0, origin_y, 0.0, dy],
        Some(UTM_PRJ),
    );

    let raster = VrtDem::from_path(
        &vrt,
        Provenance {
            source: "fixture-vrt-dem".to_owned(),
            layer: Some("vrt".to_owned()),
            source_id: Some("utm_dem.vrt".to_owned()),
            license: None,
        },
        0.76,
    )
    .unwrap();
    assert_eq!(
        raster.crs,
        RasterCrs::UtmMeters(trailgen_core::crs::UtmCrs::from_epsg(32613).unwrap())
    );
    let sample = raster.sample(Coord::new(-104.995, 40.005)).unwrap();
    assert!((sample.ele_m - 1_600.0).abs() <= 1.0e-9);
    assert!(raster.sample(Coord::new(-106.0, 40.0)).is_none());
}

#[test]
fn nad83_utm_vrt_dem_samples_projected_source() {
    let tmp = tempfile::tempdir().unwrap();
    let tif = tmp.path().join("nad83_utm_dem.tif");
    let vrt = tmp.path().join("nad83_utm_dem.vrt");
    write_utm_geotiff_dem(&tif, 26913);
    let [origin_x, scale, _, origin_y, _, dy] = utm_fixture_geotransform(26913);
    write_vrt_dem_with_geotransform(
        &vrt,
        "nad83_utm_dem.tif",
        [origin_x, scale, 0.0, origin_y, 0.0, dy],
        Some(NAD83_UTM_PRJ),
    );

    let raster = VrtDem::from_path(
        &vrt,
        Provenance {
            source: "fixture-vrt-dem".to_owned(),
            layer: Some("vrt".to_owned()),
            source_id: Some("nad83_utm_dem.vrt".to_owned()),
            license: None,
        },
        0.76,
    )
    .unwrap();
    assert_eq!(
        raster.crs,
        RasterCrs::UtmMeters(trailgen_core::crs::UtmCrs::from_epsg(26913).unwrap())
    );
    let sample = raster.sample(Coord::new(-104.995, 40.005)).unwrap();
    assert!((sample.ele_m - 1_600.0).abs() <= 1.0e-9);
    assert!(raster.sample(Coord::new(-106.0, 40.0)).is_none());
}

#[test]
fn vrt_dem_rejects_unsupported_projected_srs_even_when_wkt_mentions_wgs84() {
    let tmp = tempfile::tempdir().unwrap();
    let tif = tmp.path().join("mini_dem.tif");
    let vrt = tmp.path().join("lambert_dem.vrt");
    write_geotiff_dem(&tif);
    write_vrt_dem_with_geotransform(
        &vrt,
        "mini_dem.tif",
        [-105.01, 0.01, 0.0, 40.02, 0.0, -0.01],
        Some(r#"PROJCS["WGS 84 / Lambert",PROJECTION["Lambert_Conformal_Conic"]]"#),
    );

    let error = VrtDem::from_path(
        &vrt,
        Provenance {
            source: "fixture-vrt-dem".to_owned(),
            layer: Some("vrt".to_owned()),
            source_id: Some("lambert_dem.vrt".to_owned()),
            license: None,
        },
        0.76,
    )
    .unwrap_err();
    assert!(format!("{error}").contains("WGS84/NAD83 UTM"));
}

fn write_geotiff_dem(path: &std::path::Path) {
    use tiff::encoder::{TiffEncoder, colortype};
    use tiff::tags::Tag;

    let file = std::fs::File::create(path).unwrap();
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
        .encoder()
        .write_tag(Tag::GdalNodata, "-32768")
        .unwrap();
    image
        .write_data(&[
            1_500_i16, 1_510, 1_520, 1_590, 1_600, 1_610, 1_700, 1_710, 1_720,
        ])
        .unwrap();
}

fn write_rotated_geotiff_dem(path: &std::path::Path) {
    use tiff::encoder::{TiffEncoder, colortype};
    use tiff::tags::Tag;

    let [x0, a, b, y0, c, d] = rotated_fixture_geotransform();

    let file = std::fs::File::create(path).unwrap();
    let mut tiff = TiffEncoder::new(file).unwrap();
    let mut image = tiff.new_image::<colortype::GrayI16>(3, 3).unwrap();
    image
        .encoder()
        .write_tag(
            Tag::ModelTransformationTag,
            &[
                a, b, 0.0, x0, c, d, 0.0, y0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ][..],
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
        .encoder()
        .write_tag(Tag::GdalNodata, "-32768")
        .unwrap();
    image
        .write_data(&[
            1_500_i16, 1_510, 1_520, 1_590, 1_600, 1_610, 1_700, 1_710, 1_720,
        ])
        .unwrap();
}

fn rotated_fixture_geotransform() -> [f64; 6] {
    let center = Coord::new(-104.995, 40.005);
    let a = 0.01;
    let b = 0.001;
    let c = 0.002;
    let d = -0.01;
    [
        center.lon.mul_add(1.0, -1.5_f64.mul_add(a, 1.5 * b)),
        a,
        b,
        center.lat.mul_add(1.0, -1.5_f64.mul_add(c, 1.5 * d)),
        c,
        d,
    ]
}

fn write_web_mercator_geotiff_dem(path: &std::path::Path, projected_epsg: u16) {
    use tiff::encoder::{TiffEncoder, colortype};
    use tiff::tags::Tag;

    let scale = 1_000.0;
    let (x, y) = web_mercator_xy(Coord::new(-104.995, 40.005));
    let origin_x = 1.5_f64.mul_add(-scale, x);
    let origin_y = 1.5_f64.mul_add(scale, y);
    let file = std::fs::File::create(path).unwrap();
    let mut tiff = TiffEncoder::new(file).unwrap();
    let mut image = tiff.new_image::<colortype::GrayI16>(3, 3).unwrap();
    image
        .encoder()
        .write_tag(Tag::ModelPixelScaleTag, &[scale, scale, 0.0][..])
        .unwrap();
    image
        .encoder()
        .write_tag(
            Tag::ModelTiepointTag,
            &[0.0_f64, 0.0, 0.0, origin_x, origin_y, 0.0][..],
        )
        .unwrap();
    image
        .encoder()
        .write_tag(
            Tag::GeoKeyDirectoryTag,
            &[
                1_u16,
                1,
                0,
                3,
                1024,
                0,
                1,
                1,
                3072,
                0,
                1,
                projected_epsg,
                3076,
                0,
                1,
                9001,
            ][..],
        )
        .unwrap();
    image
        .encoder()
        .write_tag(Tag::GdalNodata, "-32768")
        .unwrap();
    image
        .write_data(&[
            1_500_i16, 1_510, 1_520, 1_590, 1_600, 1_610, 1_700, 1_710, 1_720,
        ])
        .unwrap();
}

fn write_utm_geotiff_dem(path: &std::path::Path, projected_epsg: u16) {
    use tiff::encoder::{TiffEncoder, colortype};
    use tiff::tags::Tag;

    let [origin_x, scale, _, origin_y, _, _] = utm_fixture_geotransform(projected_epsg);
    let file = std::fs::File::create(path).unwrap();
    let mut tiff = TiffEncoder::new(file).unwrap();
    let mut image = tiff.new_image::<colortype::GrayI16>(3, 3).unwrap();
    image
        .encoder()
        .write_tag(Tag::ModelPixelScaleTag, &[scale, scale, 0.0][..])
        .unwrap();
    image
        .encoder()
        .write_tag(
            Tag::ModelTiepointTag,
            &[0.0_f64, 0.0, 0.0, origin_x, origin_y, 0.0][..],
        )
        .unwrap();
    image
        .encoder()
        .write_tag(
            Tag::GeoKeyDirectoryTag,
            &[
                1_u16,
                1,
                0,
                3,
                1024,
                0,
                1,
                1,
                3072,
                0,
                1,
                projected_epsg,
                3076,
                0,
                1,
                9001,
            ][..],
        )
        .unwrap();
    image
        .encoder()
        .write_tag(Tag::GdalNodata, "-32768")
        .unwrap();
    image
        .write_data(&[
            1_500_i16, 1_510, 1_520, 1_590, 1_600, 1_610, 1_700, 1_710, 1_720,
        ])
        .unwrap();
}

fn utm_fixture_geotransform(projected_epsg: u16) -> [f64; 6] {
    let scale = 30.0;
    let crs = trailgen_core::crs::UtmCrs::from_epsg(projected_epsg)
        .expect("fixture uses supported UTM CRS");
    let (x, y) = trailgen_core::crs::geographic_to_utm(Coord::new(-104.995, 40.005), crs)
        .expect("fixture coordinate is inside UTM zone 13N");
    [
        1.5_f64.mul_add(-scale, x),
        scale,
        0.0,
        1.5_f64.mul_add(scale, y),
        0.0,
        -scale,
    ]
}

fn web_mercator_xy(coord: Coord) -> (f64, f64) {
    let r = 6_378_137.0;
    let lat = coord.lat.clamp(-85.051_128_78, 85.051_128_78).to_radians();
    (
        r * coord.lon.to_radians(),
        r * (std::f64::consts::FRAC_PI_4 + lat / 2.0).tan().ln(),
    )
}

fn write_vrt_dem(path: &std::path::Path, source: &str) {
    write_vrt_dem_with_geotransform(path, source, [-105.01, 0.01, 0.0, 40.02, 0.0, -0.01], None);
}

fn write_vrt_dem_with_geotransform(
    path: &std::path::Path,
    source: &str,
    geotransform: [f64; 6],
    srs: Option<&str>,
) {
    let [gt0, gt1, gt2, gt3, gt4, gt5] = geotransform;
    let srs = srs.map_or_else(String::new, |srs| format!("  <SRS>{srs}</SRS>\n"));
    std::fs::write(
        path,
        format!(
            r#"<VRTDataset rasterXSize="3" rasterYSize="3">
{srs}  <GeoTransform>{gt0}, {gt1}, {gt2}, {gt3}, {gt4}, {gt5}</GeoTransform>
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

fn simple_path_draft() -> SegmentDraft {
    SegmentDraft {
        geometry: LineString::new(vec![
            Coord::with_ele(0.0, 0.0, 1_000.0),
            Coord::with_ele(0.01, 0.0, 1_020.0),
            Coord::with_ele(0.02, 0.0, 1_060.0),
        ])
        .unwrap(),
        terrain: Terrain::Trail,
        terrain_confidence: None,
        surface: None,
        access: Access::Open,
        travel: EdgeTravel::Both,
        road_exposure: 0.0,
        confidence: 1.0,
        provenance: Provenance::fixture("simple-path"),
    }
}

fn square_drafts() -> Vec<SegmentDraft> {
    let a = Coord::new(0.0, 0.0);
    let b = Coord::new(0.01, 0.0);
    let c = Coord::new(0.01, 0.01);
    let d = Coord::new(0.0, 0.01);
    [
        (a, b, "south"),
        (b, c, "east"),
        (c, d, "north"),
        (d, a, "west"),
    ]
    .into_iter()
    .map(|(from, to, name)| SegmentDraft {
        geometry: LineString::new(vec![from, to]).unwrap(),
        terrain: Terrain::Trail,
        terrain_confidence: None,
        surface: None,
        access: Access::Open,
        travel: EdgeTravel::Both,
        road_exposure: 0.0,
        confidence: 1.0,
        provenance: Provenance::fixture(name),
    })
    .collect()
}

fn closure_trap_drafts() -> Vec<SegmentDraft> {
    let start = Coord::new(0.0, 0.0);
    let fork = Coord::new(0.01, 0.0);
    let north = Coord::new(0.01, 0.01);
    let west = Coord::new(0.0, 0.01);
    [
        (start, fork, Terrain::Trail, EdgeTravel::Forward, "outbound"),
        (
            fork,
            start,
            Terrain::Road,
            EdgeTravel::Forward,
            "short-road-return",
        ),
        (
            fork,
            north,
            Terrain::Trail,
            EdgeTravel::Forward,
            "long-return-east",
        ),
        (
            north,
            west,
            Terrain::Trail,
            EdgeTravel::Forward,
            "long-return-north",
        ),
        (
            west,
            start,
            Terrain::Trail,
            EdgeTravel::Forward,
            "long-return-west",
        ),
    ]
    .into_iter()
    .map(|(from, to, terrain, travel, name)| SegmentDraft {
        geometry: LineString::new(vec![from, to]).unwrap(),
        terrain,
        terrain_confidence: None,
        surface: None,
        access: Access::Open,
        travel,
        road_exposure: if terrain == Terrain::Road { 1.0 } else { 0.0 },
        confidence: 1.0,
        provenance: Provenance::fixture(name),
    })
    .collect()
}

fn bowtie_drafts() -> Vec<SegmentDraft> {
    let waist = Coord::new(0.0, 0.0);
    let east = Coord::new(0.01, 0.0);
    let north = Coord::new(0.005, 0.008);
    let west = Coord::new(-0.01, 0.0);
    let south = Coord::new(-0.005, -0.008);
    [
        (waist, east, "right-stem"),
        (east, north, "right-ridge"),
        (north, waist, "right-return"),
        (waist, west, "left-stem"),
        (west, south, "left-ridge"),
        (south, waist, "left-return"),
    ]
    .into_iter()
    .map(|(from, to, name)| SegmentDraft {
        geometry: LineString::new(vec![from, to]).unwrap(),
        terrain: Terrain::Trail,
        terrain_confidence: None,
        surface: None,
        access: Access::Open,
        travel: EdgeTravel::Both,
        road_exposure: 0.0,
        confidence: 1.0,
        provenance: Provenance::fixture(name),
    })
    .collect()
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
        "type",
        "route",
        "hiking",
        "name",
        "ridge route",
        "restriction",
        "no_right_turn",
        "from",
        "to",
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
    group.relations = vec![
        pbf_relation(20, &[(13, 14), (14, 15), (16, 17)], &[10]),
        pbf_relation_with_roles(30, &[(13, 18), (18, 19)], &[(10, 20), (10, 21)]),
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

fn pbf_relation(
    id: i64,
    tags: &[(u32, u32)],
    way_refs: &[i64],
) -> osmpbfreader::osmformat::Relation {
    let refs = way_refs.iter().map(|way| (*way, 0)).collect::<Vec<_>>();
    pbf_relation_with_roles(id, tags, &refs)
}

fn pbf_relation_with_roles(
    id: i64,
    tags: &[(u32, u32)],
    way_refs: &[(i64, i32)],
) -> osmpbfreader::osmformat::Relation {
    use osmpbfreader::osmformat::relation::MemberType;

    let mut relation = osmpbfreader::osmformat::Relation::new();
    relation.set_id(id);
    relation.keys = tags.iter().map(|(key, _)| *key).collect();
    relation.vals = tags.iter().map(|(_, value)| *value).collect();
    relation.roles_sid = way_refs.iter().map(|(_, role)| *role).collect();
    relation.memids = way_refs
        .iter()
        .scan(0, |last, reference| {
            let delta = reference.0 - *last;
            *last = reference.0;
            Some(delta)
        })
        .collect();
    relation.types = vec![MemberType::WAY.into(); way_refs.len()];
    relation
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
