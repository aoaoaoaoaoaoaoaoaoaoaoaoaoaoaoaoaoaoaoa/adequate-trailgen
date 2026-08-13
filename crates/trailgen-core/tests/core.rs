use std::io::Cursor;

use protobuf::{Message, MessageField};
use trailgen_core::io::{geojson, gpx, osm, shapefile as shp_io};
use trailgen_core::{
    Access, EdgeId, EdgeTravel, ElevationSample, ElevationSampler, ExactLoopSolver, GeoTiffDem,
    MonthDay, PlanningDate, RasterCrs, Route, RouteMetrics, RouteShape, SeasonalWindow, VrtDem,
};
use trailgen_core::{
    Coord, CrossingControl, EnrichmentConfig, GeometryClaim, GraphBuilder, JunctionPolicy,
    LineString, LoopConstraints, LoopHunter, PlaneElevation, Provenance, SegmentDraft, Terrain,
    TrailMarking, TrailStanding, TurnRestrictionDraft, TurnRestrictionRule, WalkGraph, WayKind,
    WayRealm, apply_access_overlays, apply_terrain_overlays, enrich_graph,
};

const WEB_MERCATOR_PRJ: &str = r#"PROJCS["WGS 84 / Pseudo-Mercator",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PROJECTION["Mercator_1SP"],UNIT["metre",1],AUTHORITY["EPSG","3857"]]"#;
const UTM_PRJ: &str = r#"PROJCS["WGS 84 / UTM zone 13N",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PROJECTION["Transverse_Mercator"],UNIT["metre",1],AUTHORITY["EPSG","32613"]]"#;
const NAD83_UTM_PRJ: &str = r#"PROJCS["NAD83 / UTM zone 13N",GEOGCS["NAD83",DATUM["North_American_Datum_1983",SPHEROID["GRS 1980",6378137,298.257222101]],PROJECTION["Transverse_Mercator"],UNIT["metre",1],AUTHORITY["EPSG","26913"]]"#;
const ESRI_NAD83_UTM_PRJ: &str = r#"PROJCS["NAD_1983_UTM_Zone_13N",GEOGCS["GCS_North_American_1983",DATUM["D_North_American_1983",SPHEROID["GRS_1980",6378137,298.257222101]],PROJECTION["Transverse_Mercator"],UNIT["Meter",1]]"#;
const NAD83_LAMBERT_PRJ: &str = r#"PROJCS["NAD83 / Colorado Central",GEOGCS["NAD83",DATUM["North_American_Datum_1983",SPHEROID["GRS 1980",6378137,298.257222101]],PROJECTION["Lambert_Conformal_Conic"],UNIT["metre",1],AUTHORITY["EPSG","32154"]]"#;
const WEB_MERCATOR_0_01_LON_M: f64 = 1_113.194_907_932_735_8;

#[test]
fn builder_splits_crossing_lines() {
    let drafts = vec![
        SegmentDraft {
            junctions: JunctionPolicy::default(),
            turn_ref: None,
            junction_keys: None,
            turn_restrictions: Vec::new(),
            geometry: LineString::new(vec![Coord::new(0.0, 0.5), Coord::new(1.0, 0.5)]).unwrap(),
            way_kind: WayKind::default(),
            realm: WayRealm::default(),
            geometry_claim: GeometryClaim::default(),
            crossing_control: CrossingControl::default(),
            standing: TrailStanding::Unknown,
            marking: TrailMarking::default(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: vec![Provenance::fixture("a")],
        },
        SegmentDraft {
            junctions: JunctionPolicy::default(),
            turn_ref: None,
            junction_keys: None,
            turn_restrictions: Vec::new(),
            geometry: LineString::new(vec![Coord::new(0.5, 0.0), Coord::new(0.5, 1.0)]).unwrap(),
            way_kind: WayKind::default(),
            realm: WayRealm::default(),
            geometry_claim: GeometryClaim::default(),
            crossing_control: CrossingControl::default(),
            standing: TrailStanding::Unknown,
            marking: TrailMarking::default(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: vec![Provenance::fixture("b")],
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
            junctions: JunctionPolicy::default(),
            turn_ref: None,
            junction_keys: None,
            turn_restrictions: Vec::new(),
            geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)]).unwrap(),
            way_kind: WayKind::default(),
            realm: WayRealm::default(),
            geometry_claim: GeometryClaim::default(),
            crossing_control: CrossingControl::default(),
            standing: TrailStanding::Unknown,
            marking: TrailMarking::default(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Forward,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: vec![Provenance::fixture("one-way")],
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
fn graph_deserialization_rebuilds_derived_adjacency_and_rejects_corruption() {
    let graph = GraphBuilder::default()
        .build(&geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap())
        .unwrap();
    let mut json = serde_json::to_value(&graph).unwrap();
    assert!(json.get("adjacency").is_none());
    let restored: WalkGraph = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(restored.adjacency, graph.adjacency);

    json["edges"][0]["a"] = serde_json::json!(usize::MAX);
    assert!(serde_json::from_value::<WalkGraph>(json).is_err());
}

#[test]
fn solvers_respect_directed_turn_bans() {
    let graph = directed_turn_ban_graph();
    let constraints = LoopConstraints {
        min_distance_m: 0.0,
        max_distance_m: 10_000.0,
        max_lower_limb_load_km: 10_000.0,
        ..LoopConstraints::default()
    };

    assert_eq!(graph.turn_bans.len(), 2);
    assert!(
        graph
            .walk_edges(graph.edges[0].a, &[EdgeId(0), EdgeId(1), EdgeId(2)])
            .is_none()
    );
    assert!(
        ExactLoopSolver::default()
            .enumerate(&graph, graph.edges[0].a, &constraints, 4)
            .is_empty()
    );
    assert!(
        LoopHunter::default()
            .hunt(&graph, graph.edges[0].a, &constraints, 4)
            .is_empty()
    );
}

fn directed_turn_ban_graph() -> WalkGraph {
    let start = Coord::new(0.0, 0.0);
    let via = Coord::new(0.01, 0.0);
    let crown = Coord::new(0.01, 0.01);
    let turn_source = Provenance {
        source: "fixture-turns".to_owned(),
        layer: Some("turn-restriction".to_owned()),
        source_id: Some("forbidden-corner".to_owned()),
        license: None,
    };
    GraphBuilder::default()
        .build(&[
            SegmentDraft {
                junctions: JunctionPolicy::default(),
                turn_ref: Some("out".to_owned()),
                junction_keys: None,
                turn_restrictions: vec![
                    TurnRestrictionDraft {
                        from: "out".to_owned(),
                        via,
                        via_key: None,
                        to: "north".to_owned(),
                        rule: TurnRestrictionRule::No,
                        provenance: turn_source.clone(),
                    },
                    TurnRestrictionDraft {
                        from: "north".to_owned(),
                        via,
                        via_key: None,
                        to: "out".to_owned(),
                        rule: TurnRestrictionRule::No,
                        provenance: turn_source,
                    },
                ],
                geometry: LineString::new(vec![start, via]).unwrap(),
                way_kind: WayKind::default(),
                realm: WayRealm::default(),
                geometry_claim: GeometryClaim::default(),
                crossing_control: CrossingControl::default(),
                standing: TrailStanding::Unknown,
                marking: TrailMarking::default(),
                terrain: Terrain::Trail,
                terrain_confidence: None,
                surface: None,
                access: Access::Open,
                travel: EdgeTravel::Both,
                road_exposure: 0.0,
                confidence: 1.0,
                provenance: vec![Provenance::fixture("out")],
            },
            SegmentDraft {
                junctions: JunctionPolicy::default(),
                turn_ref: Some("north".to_owned()),
                junction_keys: None,
                turn_restrictions: Vec::new(),
                geometry: LineString::new(vec![via, crown]).unwrap(),
                way_kind: WayKind::default(),
                realm: WayRealm::default(),
                geometry_claim: GeometryClaim::default(),
                crossing_control: CrossingControl::default(),
                standing: TrailStanding::Unknown,
                marking: TrailMarking::default(),
                terrain: Terrain::Trail,
                terrain_confidence: None,
                surface: None,
                access: Access::Open,
                travel: EdgeTravel::Both,
                road_exposure: 0.0,
                confidence: 1.0,
                provenance: vec![Provenance::fixture("north")],
            },
            SegmentDraft {
                junctions: JunctionPolicy::default(),
                turn_ref: Some("return".to_owned()),
                junction_keys: None,
                turn_restrictions: Vec::new(),
                geometry: LineString::new(vec![crown, start]).unwrap(),
                way_kind: WayKind::default(),
                realm: WayRealm::default(),
                geometry_claim: GeometryClaim::default(),
                crossing_control: CrossingControl::default(),
                standing: TrailStanding::Unknown,
                marking: TrailMarking::default(),
                terrain: Terrain::Trail,
                terrain_confidence: None,
                surface: None,
                access: Access::Open,
                travel: EdgeTravel::Both,
                road_exposure: 0.0,
                confidence: 1.0,
                provenance: vec![Provenance::fixture("return")],
            },
        ])
        .unwrap()
}

#[test]
fn builder_repairs_near_miss_explicit_endpoints_without_inventing_crossings() {
    let mut drafts = near_miss_drafts();
    for draft in &mut drafts {
        draft.junctions = JunctionPolicy::ExplicitEndpoints;
    }

    let graph = GraphBuilder::default().build(&drafts).unwrap();

    assert!(
        graph
            .vertices
            .iter()
            .any(|vertex| graph.adjacency[vertex.id.0].len() == 3)
    );
    assert!(graph.edges.iter().any(|edge| {
        edge.attr.provenance.iter().any(|provenance| {
            provenance.source == "graph-builder"
                && provenance.layer.as_deref() == Some("near-miss-snap")
        })
    }));
}

#[test]
fn builder_never_quantizes_explicit_nodes_into_a_junction() {
    let mut drafts = near_miss_drafts();
    drafts[0].geometry =
        LineString::new(vec![Coord::new(-74.0, 41.0), Coord::new(-73.999, 41.0)]).unwrap();
    drafts[1].geometry = LineString::new(vec![
        Coord::new(-73.999, 41.000_05),
        Coord::new(-73.998, 41.000_05),
    ])
    .unwrap();
    for draft in &mut drafts {
        draft.junctions = JunctionPolicy::ExplicitNodes;
    }

    let graph = GraphBuilder::default().build(&drafts).unwrap();

    assert_eq!(graph.vertices.len(), 4);
    assert!(
        graph
            .vertices
            .iter()
            .all(|vertex| graph.adjacency[vertex.id.0].len() == 1)
    );
}

fn near_miss_drafts() -> Vec<SegmentDraft> {
    vec![
        SegmentDraft {
            junctions: JunctionPolicy::default(),
            turn_ref: None,
            junction_keys: None,
            turn_restrictions: Vec::new(),
            geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0)]).unwrap(),
            way_kind: WayKind::default(),
            realm: WayRealm::default(),
            geometry_claim: GeometryClaim::default(),
            crossing_control: CrossingControl::default(),
            standing: TrailStanding::Unknown,
            marking: TrailMarking::default(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: vec![Provenance::fixture("trunk")],
        },
        SegmentDraft {
            junctions: JunctionPolicy::default(),
            turn_ref: None,
            junction_keys: None,
            turn_restrictions: Vec::new(),
            geometry: LineString::new(vec![Coord::new(0.5, 0.00005), Coord::new(0.5, 0.01)])
                .unwrap(),
            way_kind: WayKind::default(),
            realm: WayRealm::default(),
            geometry_claim: GeometryClaim::default(),
            crossing_control: CrossingControl::default(),
            standing: TrailStanding::Unknown,
            marking: TrailMarking::default(),
            terrain: Terrain::Trail,
            terrain_confidence: None,
            surface: None,
            access: Access::Open,
            travel: EdgeTravel::Both,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: vec![Provenance::fixture("spur")],
        },
    ]
}

#[test]
fn lower_limb_load_is_directional_and_invariant_under_subdivision() {
    let draft = |points| SegmentDraft {
        junctions: JunctionPolicy::default(),
        turn_ref: None,
        junction_keys: None,
        turn_restrictions: Vec::new(),
        geometry: LineString::new(points).unwrap(),
        way_kind: WayKind::default(),
        realm: WayRealm::default(),
        geometry_claim: GeometryClaim::default(),
        crossing_control: CrossingControl::default(),
        standing: TrailStanding::Unknown,
        marking: TrailMarking::default(),
        terrain: Terrain::Trail,
        terrain_confidence: Some(0.6),
        surface: Some("gravel".to_owned()),
        access: Access::Restricted,
        travel: EdgeTravel::Both,
        road_exposure: 0.2,
        confidence: 0.4,
        provenance: vec![Provenance::fixture("segmentation")],
    };
    let coarse = GraphBuilder::default()
        .build(&[draft(vec![
            Coord::with_ele(0.0, 0.0, 1_000.0),
            Coord::with_ele(0.02, 0.0, 1_100.0),
        ])])
        .unwrap();
    let fine = GraphBuilder::default()
        .build(&[draft(
            (0..=20)
                .map(|i| {
                    let t = f64::from(i) / 20.0;
                    Coord::with_ele(0.02 * t, 0.0, 100.0f64.mul_add(t, 1_000.0))
                })
                .collect(),
        )])
        .unwrap();
    let traversal = |graph: &WalkGraph| {
        graph
            .edges
            .iter()
            .fold(Default::default(), |mut total: [f64; 2], edge| {
                total[0] += edge.attr.traversal.forward.lower_limb_load_km;
                total[1] += edge.attr.traversal.reverse.lower_limb_load_km;
                total
            })
    };
    let coarse_load = traversal(&coarse);
    let fine_load = traversal(&fine);

    assert!((coarse_load[0] - fine_load[0]).abs() <= 1.0e-9);
    assert!((coarse_load[1] - fine_load[1]).abs() <= 1.0e-9);
    assert!(coarse_load[1] > coarse_load[0]);
}

#[test]
fn osm_identity_keeps_coincident_distinct_nodes_disconnected() {
    let drafts = osm::network_from_str(
        r#"<osm version="0.6">
  <node id="1" lat="40.0" lon="-105.01"/>
  <node id="2" lat="40.0" lon="-105.00"/>
  <node id="3" lat="40.0" lon="-105.00"/>
  <node id="4" lat="40.0" lon="-104.99"/>
  <way id="10"><nd ref="1"/><nd ref="2"/><tag k="highway" v="path"/></way>
  <way id="11"><nd ref="3"/><nd ref="4"/><tag k="highway" v="path"/></way>
</osm>"#,
    )
    .unwrap();

    let graph = GraphBuilder::default().build(&drafts).unwrap();

    assert_eq!(graph.vertices.len(), 4);
    assert_eq!(graph.edges.len(), 2);
    assert_eq!(
        graph
            .vertices
            .iter()
            .filter(|vertex| vertex.coord == Coord::new(-105.0, 40.0))
            .count(),
        2
    );
    assert!(graph.adjacency.iter().all(|fanout| fanout.len() == 1));
}

#[test]
fn osm_pedestrian_facilities_preserve_independent_urban_semantics() {
    let drafts = osm::network_from_str(
        r#"<osm version="0.6">
  <node id="1" lat="40.00" lon="-105.00"/><node id="2" lat="40.00" lon="-104.99"/>
  <node id="3" lat="40.01" lon="-105.00"/><node id="4" lat="40.01" lon="-104.99"/>
  <node id="5" lat="40.02" lon="-105.00"/><node id="6" lat="40.02" lon="-104.99"/>
  <node id="7" lat="40.03" lon="-105.00"><tag k="highway" v="crossing"/><tag k="crossing" v="traffic_signals"/></node><node id="8" lat="40.03" lon="-104.99"/>
  <node id="9" lat="40.04" lon="-105.00"/><node id="10" lat="40.04" lon="-104.99"/>
  <way id="10"><nd ref="1"/><nd ref="2"/><tag k="highway" v="residential"/><tag k="sidewalk" v="both"/><tag k="oneway" v="yes"/></way>
  <way id="11"><nd ref="3"/><nd ref="4"/><tag k="highway" v="residential"/><tag k="sidewalk" v="separate"/></way>
  <way id="12"><nd ref="5"/><nd ref="6"/><tag k="highway" v="footway"/><tag k="footway" v="sidewalk"/></way>
  <way id="13"><nd ref="7"/><nd ref="8"/><tag k="highway" v="footway"/><tag k="footway" v="crossing"/></way>
  <way id="14"><nd ref="9"/><nd ref="10"/><tag k="highway" v="residential"/><tag k="foot" v="use_sidepath"/></way>
</osm>"#,
    )
    .unwrap();

    assert_eq!(drafts.len(), 4);
    assert_eq!(drafts[0].way_kind, WayKind::Sidewalk);
    assert_eq!(drafts[0].geometry_claim, GeometryClaim::CenterlineProxy);
    assert_eq!(drafts[0].realm, WayRealm::Urban);
    assert_eq!(drafts[0].terrain, Terrain::Pavement);
    assert_eq!(drafts[0].travel, EdgeTravel::Both);
    assert!((drafts[0].road_exposure - 0.25).abs() <= f64::EPSILON);

    assert_eq!(drafts[1].way_kind, WayKind::Roadway);
    assert_eq!(drafts[1].geometry_claim, GeometryClaim::Surveyed);
    assert_eq!(drafts[1].realm, WayRealm::Urban);
    assert_eq!(drafts[2].way_kind, WayKind::Sidewalk);
    assert_eq!(drafts[2].geometry_claim, GeometryClaim::Surveyed);
    assert_eq!(drafts[2].realm, WayRealm::Urban);
    assert_eq!(drafts[3].way_kind, WayKind::Crossing);
    assert_eq!(drafts[3].crossing_control, CrossingControl::Signals);
    assert_eq!(drafts[3].realm, WayRealm::Urban);
}

#[test]
fn osm_grade_separation_blocks_near_miss_junction_repair() {
    let drafts = osm::network_from_str(
        r#"<osm version="0.6">
  <node id="1" lat="41.00000" lon="-74.00100"/>
  <node id="2" lat="41.00000" lon="-74.00000"/>
  <node id="3" lat="41.00005" lon="-74.00000"/>
  <node id="4" lat="41.00005" lon="-73.99900"/>
  <way id="10"><nd ref="1"/><nd ref="2"/><tag k="highway" v="path"/></way>
  <way id="11"><nd ref="3"/><nd ref="4"/><tag k="highway" v="path"/><tag k="bridge" v="yes"/></way>
</osm>"#,
    )
    .unwrap();

    assert_eq!(drafts[1].junctions, JunctionPolicy::GradeSeparatedEndpoints);
    let graph = GraphBuilder::default().build(&drafts).unwrap();
    assert_eq!(graph.vertices.len(), 4);
    assert!(
        graph
            .vertices
            .iter()
            .all(|vertex| graph.adjacency[vertex.id.0].len() == 1)
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
    assert_eq!(drafts[0].provenance[0].source, "osm-pbf");
    assert_eq!(
        drafts[0].provenance[0].layer.as_deref(),
        Some("way+route-relation+turn-restriction")
    );
    let source_id = drafts[0].provenance[0].source_id.as_deref().unwrap();
    assert!(source_id.contains("way 10; route relations 20:ridge route"));
    assert!(source_id.contains("turn restrictions 30:from:no_right_turn"));
    assert!(drafts[0].confidence >= 0.82);
    assert_eq!(
        drafts
            .iter()
            .map(|draft| draft.turn_restrictions.len())
            .sum::<usize>(),
        1
    );
    assert_eq!(drafts[1].terrain, Terrain::Road);
    assert_eq!(drafts[1].terrain_confidence, Some(0.62));
    assert_eq!(drafts[1].access, Access::Private);
    assert!((drafts[1].road_exposure - 1.0).abs() <= f64::EPSILON);

    let graph = GraphBuilder::default().build(&drafts).unwrap();
    assert_eq!(graph.edges.len(), 2);
    assert_eq!(graph.turn_bans.len(), 1);
    assert!(
        graph
            .walk_edges(graph.edges[0].a, &[graph.edges[0].id, graph.edges[1].id])
            .is_none()
    );
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.attr.provenance[0].source == "osm-pbf")
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
fn access_restrictions_are_hard_route_constraints() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    let overlays =
        geojson::access_overlays_from_str(include_str!("fixtures/closure_overlay.geojson"))
            .unwrap();
    apply_access_overlays(&mut graph, &overlays, None);
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
            max_lower_limb_load_km: 10_000.0,
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
fn terrain_overlays_override_edges_with_provenance_and_physical_estimates() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    let overlays =
        geojson::terrain_overlays_from_str(include_str!("fixtures/terrain_overlay.geojson"))
            .unwrap();
    let touched = apply_terrain_overlays(&mut graph, &overlays);
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
    assert!(overlaid.iter().all(|edge| edge.attr.traversal.valid()
        && edge.attr.traversal.forward.lower_limb_load_km > 0.0
        && edge.attr.traversal.forward.moving_time_s > 0.0));
    let evidence_count = graph
        .edges
        .iter()
        .flat_map(|edge| edge.attr.terrain_evidence.iter())
        .filter(|e| e.rationale == "terrain overlay")
        .count();
    let _ = apply_terrain_overlays(&mut graph, &overlays);
    let evidence_count_again = graph
        .edges
        .iter()
        .flat_map(|edge| edge.attr.terrain_evidence.iter())
        .filter(|e| e.rationale == "terrain overlay")
        .count();
    assert_eq!(evidence_count, evidence_count_again);
}

#[test]
fn physical_load_and_moving_time_windows_are_independent_constraints() {
    let metrics = RouteMetrics {
        shape: RouteShape::Loop,
        distance_m: 8_000.0,
        lower_limb_load_km: 10.0,
        moving_time_s: 2.0 * 3_600.0,
        ..RouteMetrics::default()
    };
    let mut constraints = LoopConstraints::default();
    assert!(constraints.judge(&metrics).satisfied);

    constraints.max_lower_limb_load_km = 9.0;
    constraints.min_moving_time_s = 3.0 * 3_600.0;
    let verdict = constraints.judge(&metrics);
    assert!(!verdict.satisfied);
    assert!(
        verdict
            .violations
            .iter()
            .any(|violation| violation.starts_with("lower-limb load "))
    );
    assert!(
        verdict
            .violations
            .iter()
            .any(|violation| violation.starts_with("moving time "))
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
    assert!(xml.contains("<desc>score "));
    assert!(xml.contains("constraints "));
    let line = gpx::route_line_from_str(&xml).unwrap();
    assert!(line.length_m() > 3_000.0);
    let file = gpx::route_file_from_str(&xml).unwrap();
    assert_eq!(file.metadata.title.as_deref(), Some(route.name.as_str()));
    assert_eq!(file.metadata.activity_type.as_deref(), Some("hiking"));
}

#[test]
fn route_coverage_ignores_a_closer_disconnected_shadow_line() {
    let segment = |name, points| SegmentDraft {
        geometry: LineString::new(points).unwrap(),
        junctions: JunctionPolicy::ExplicitNodes,
        turn_ref: None,
        junction_keys: None,
        turn_restrictions: Vec::new(),
        way_kind: WayKind::Path,
        realm: WayRealm::default(),
        geometry_claim: GeometryClaim::default(),
        crossing_control: CrossingControl::default(),
        standing: TrailStanding::Established,
        marking: TrailMarking::default(),
        terrain: Terrain::Trail,
        terrain_confidence: Some(0.8),
        surface: None,
        access: Access::Open,
        travel: EdgeTravel::Both,
        road_exposure: 0.0,
        confidence: 0.8,
        provenance: vec![Provenance::fixture(name)],
    };
    let graph = GraphBuilder::default()
        .build(&[
            segment(
                "through",
                vec![Coord::new(0.0, 0.0), Coord::new(0.003, 0.0)],
            ),
            segment(
                "shadow",
                vec![Coord::new(0.001, 0.000_01), Coord::new(0.002, 0.000_01)],
            ),
        ])
        .unwrap();
    let route = LineString::new(vec![
        Coord::new(0.0, 0.0),
        Coord::new(0.000_8, 0.0),
        Coord::new(0.001, 0.000_01),
        Coord::new(0.002, 0.000_01),
        Coord::new(0.002_2, 0.0),
        Coord::new(0.003, 0.0),
    ])
    .unwrap();

    let coverage = graph.trace_coverage(&route, 20.0);

    assert!(coverage.gaps.is_empty());
    assert_eq!(coverage.stats.disconnected_transition_count, 0);
    assert_eq!(coverage.edges, vec![EdgeId(0)]);
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
    assert_colorado_segment(&drafts);

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
    assert_colorado_segment(&drafts);

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
    assert_colorado_segment(&drafts);

    let nad83_utm = tmp.path().join("nad83-utm-trails.shp");
    write_network_shapefile_points(
        &nad83_utm,
        ::shapefile::Point::new(nad83_utm_start.0, nad83_utm_start.1),
        ::shapefile::Point::new(nad83_utm_end.0, nad83_utm_end.1),
    );
    std::fs::write(nad83_utm.with_extension("prj"), NAD83_UTM_PRJ).unwrap();
    let drafts = shp_io::network_from_path(&nad83_utm).unwrap();
    assert_colorado_segment(&drafts);

    std::fs::write(nad83_utm.with_extension("prj"), ESRI_NAD83_UTM_PRJ).unwrap();
    let drafts = shp_io::network_from_path(&nad83_utm).unwrap();
    assert_colorado_segment(&drafts);

    let unsupported = tmp.path().join("nad83-lambert-trails.shp");
    write_network_shapefile(&unsupported);
    std::fs::write(unsupported.with_extension("prj"), NAD83_LAMBERT_PRJ).unwrap();
    let error = shp_io::network_from_path(&unsupported).unwrap_err();
    assert!(format!("{error}").contains("unsupported projected CRS"));
}

fn assert_colorado_segment(drafts: &[SegmentDraft]) {
    assert!((drafts[0].geometry.points[0].lon + 104.995).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[0].lat - 40.005).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[1].lon + 104.985).abs() < 1.0e-7);
    assert!((drafts[0].geometry.points[1].lat - 40.005).abs() < 1.0e-7);
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
        .add_character_field("asset".try_into().unwrap(), 20)
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
    record.insert("asset".to_owned(), "Unmarked Trail".to_owned().into());
    record.insert("source".to_owned(), "agency".to_owned().into());
    record.insert("confidence".to_owned(), 0.91.into());
    writer.write_shape_and_record(&line, &record).unwrap();
}

fn enrich_with_north_plane(graph: &mut WalkGraph, north_gain_m_per_degree: f64, confidence: f64) {
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
    )
    .unwrap();
}

#[test]
fn partial_elevation_sampling_does_not_invent_flat_grade() {
    let draft = SegmentDraft {
        junctions: JunctionPolicy::default(),
        turn_ref: None,
        junction_keys: None,
        turn_restrictions: Vec::new(),
        geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.0, 0.01)]).unwrap(),
        way_kind: WayKind::default(),
        realm: WayRealm::default(),
        geometry_claim: GeometryClaim::default(),
        crossing_control: CrossingControl::default(),
        standing: TrailStanding::Unknown,
        marking: TrailMarking::default(),
        terrain: Terrain::Unknown,
        terrain_confidence: None,
        surface: None,
        access: Access::Open,
        travel: EdgeTravel::Both,
        road_exposure: 0.0,
        confidence: 0.9,
        provenance: vec![Provenance::fixture("partial-dem")],
    };
    let mut graph = GraphBuilder::default().build(&[draft]).unwrap();
    enrich_with_north_plane(&mut graph, 10_000.0, 0.90);
    assert!(
        graph.edges[0]
            .geometry
            .points
            .iter()
            .all(|point| point.ele.is_some())
    );

    enrich_graph(
        &mut graph,
        &PartialNorthPlane {
            max_lat: 0.005,
            north_gain_m_per_degree: 10_000.0,
        },
        EnrichmentConfig {
            sample_spacing_m: 125.0,
            steep_grade_threshold: 0.05,
        },
    )
    .unwrap();

    let edge = &graph.edges[0];
    let graded_m = edge.attr.grade_distribution.total_m();
    assert!(
        edge.geometry
            .points
            .last()
            .is_some_and(|point| point.ele.is_none())
    );
    assert!(graded_m > edge.attr.length_m * 0.40);
    assert!(graded_m < edge.attr.length_m * 0.65);
    assert!((0.085..=0.095).contains(&edge.attr.grade_abs_mean));
    assert!(edge.attr.sustained_steep_m > graded_m - 1.0);
    assert!((0.45..=0.65).contains(&edge.attr.confidence));
    assert!(
        edge.attr
            .elevation_provenance
            .iter()
            .any(|p| p.source == "partial-plane-elevation")
    );
}

struct PartialNorthPlane {
    max_lat: f64,
    north_gain_m_per_degree: f64,
}

impl ElevationSampler for PartialNorthPlane {
    fn sample(&self, coord: Coord) -> Option<ElevationSample> {
        (coord.lat <= self.max_lat).then(|| ElevationSample {
            ele_m: self.north_gain_m_per_degree.mul_add(coord.lat, 1_000.0),
            confidence: 0.90,
            provenance: Provenance {
                source: "partial-plane-elevation".to_owned(),
                layer: Some("fixture".to_owned()),
                source_id: None,
                license: Some("CC0-fixture".to_owned()),
            },
        })
    }
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
        "via",
        "restriction:foot",
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
        pbf_turn_relation(),
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

fn pbf_turn_relation() -> osmpbfreader::osmformat::Relation {
    use osmpbfreader::osmformat::relation::MemberType;

    let mut relation = osmpbfreader::osmformat::Relation::new();
    relation.set_id(30);
    relation.keys = vec![13, 23];
    relation.vals = vec![18, 19];
    relation.roles_sid = vec![20, 22, 21];
    relation.memids = vec![10, -8, 9];
    relation.types = vec![
        MemberType::WAY.into(),
        MemberType::NODE.into(),
        MemberType::WAY.into(),
    ];
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
