use std::collections::BTreeMap;
use trailgen_core::alltrails::{
    AllTrailsBridge, AllTrailsExchange, BridgeStatus, ManualAllTrailsBridge,
};
use trailgen_core::io::{csv, geojson, gpx, kml, kmz, report};
use trailgen_core::source::{
    SourceCoverageStatus, SourceKind, adapter_registry, classify_path, discovery_recommendations,
    source_coverage,
};
use trailgen_core::{
    Access, ArcAsciiGrid, CrossingKind, EdgeId, ElevationSampler, Route, RouteMetrics, RouteShape,
    SearchParams, VertexId,
};
use trailgen_core::{
    Coord, DifficultyWeights, EnrichmentConfig, GraphBuilder, LineString, LoopConstraints,
    LoopHunter, PlaneElevation, Provenance, SeedRoute, SegmentDraft, Terrain, TerrainMultipliers,
    apply_access_overlays, apply_context_overlays, apply_terrain_overlays, enrich_graph,
    rank_routes,
};

#[test]
fn builder_splits_crossing_lines() {
    let drafts = vec![
        SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.0, 0.5), Coord::new(1.0, 0.5)]).unwrap(),
            terrain: Terrain::Trail,
            access: Access::Open,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("a"),
        },
        SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.5, 0.0), Coord::new(0.5, 1.0)]).unwrap(),
            terrain: Terrain::Trail,
            access: Access::Open,
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
            access: Access::Open,
            road_exposure: 0.0,
            confidence: 1.0,
            provenance: Provenance::fixture("trunk"),
        },
        SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.5, 0.00005), Coord::new(0.5, 0.01)])
                .unwrap(),
            terrain: Terrain::Trail,
            access: Access::Open,
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
        access: Access::Open,
        road_exposure: 0.0,
        confidence: 1.0,
        provenance: Provenance::fixture("smooth"),
    };
    let savage = SegmentDraft {
        terrain: Terrain::Scramble,
        access: Access::Closed,
        confidence: 0.25,
        provenance: Provenance::fixture("savage"),
        ..smooth.clone()
    };
    let uncertain = SegmentDraft {
        terrain: Terrain::Unknown,
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
        access: Access::Open,
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
fn road_fraction_counts_road_and_pavement_terrain() {
    let graph = GraphBuilder::default()
        .build(&[SegmentDraft {
            geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)]).unwrap(),
            terrain: Terrain::Pavement,
            access: Access::Open,
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
    let touched = apply_access_overlays(&mut graph, &overlays, DifficultyWeights::default());
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
fn access_restrictions_are_hard_route_constraints() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let mut graph = GraphBuilder::default().build(&drafts).unwrap();
    let overlays =
        geojson::access_overlays_from_str(include_str!("fixtures/closure_overlay.geojson"))
            .unwrap();
    apply_access_overlays(&mut graph, &overlays, DifficultyWeights::default());
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
fn fixture_generates_nontrivial_loops() {
    let drafts = geojson::network_from_str(include_str!("fixtures/mini_network.geojson")).unwrap();
    let graph = GraphBuilder::default().build(&drafts).unwrap();
    let start = graph.nearest_vertex(Coord::new(-105.0, 40.0)).unwrap();
    let routes = LoopHunter {
        params: SearchParams {
            max_hops: 10,
            max_frontier: 10_000,
            keep: 8,
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
    let graph = GraphBuilder::default().build(&drafts).unwrap();
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
    let rendered = report::render(&graph, &[route]);
    assert!(rendered.contains("- score:"));
    assert!(rendered.contains("Difficulty decomposition:"));
    assert!(rendered.contains("- distance:"));
    assert!(rendered.contains("- ascent:"));
    assert!(rendered.contains("Largest difficulty contributors:"));
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
fn loop_hunter_builds_figure_eights_when_shape_allows_two_lobes() {
    let graph = GraphBuilder::default().build(&bowtie_drafts()).unwrap();
    let start = graph.nearest_vertex(Coord::new(0.0, 0.0)).unwrap();
    let routes = LoopHunter {
        params: SearchParams {
            max_hops: 8,
            max_frontier: 1_000,
            keep: 8,
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
fn source_registry_classifies_local_inputs() {
    let adapters = adapter_registry();
    assert!(adapters.iter().any(|a| a.id == "geojson-network"));
    let gpx = classify_path(std::path::Path::new("sources/alltrails-export.gpx")).unwrap();
    assert_eq!(gpx.kind, SourceKind::SeedRoute);
    let csv = classify_path(std::path::Path::new("sources/alltrails-export.csv")).unwrap();
    assert_eq!(csv.kind, SourceKind::SeedRoute);
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
    let ascii_dem = classify_path(std::path::Path::new("sources/dem.asc")).unwrap();
    assert_eq!(ascii_dem.kind, SourceKind::Elevation);
    assert_eq!(ascii_dem.adapter_id, "arc-ascii-elevation");
}

#[test]
fn source_coverage_evaluates_recommendations_against_candidates() {
    let adapters = adapter_registry();
    let recommendations = discovery_recommendations(None);
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
    assert_eq!(elevation.status, SourceCoverageStatus::PlannedAdapterOnly);
    assert_eq!(
        elevation.planned_adapter_ids,
        vec!["geospatial-elevation-raster"]
    );

    let hydrology = coverage
        .iter()
        .find(|entry| entry.kind == SourceKind::Hydrology)
        .expect("hydrology coverage");
    assert_eq!(hydrology.status, SourceCoverageStatus::Missing);
    assert!(hydrology.message.contains("sources/hydrology.geojson"));
}

#[test]
fn alltrails_bridge_refuses_undocumented_write_api() {
    let caps = ManualAllTrailsBridge.capabilities();
    assert!(caps.iter().any(|cap| {
        cap.exchange == AllTrailsExchange::ManualUploadCustomRoute
            && cap.status == BridgeStatus::Manual
            && cap.formats.iter().any(|fmt| fmt == "gpx")
    }));
    assert!(caps.iter().any(|cap| {
        cap.exchange == AllTrailsExchange::DirectWriteApi
            && cap.status == BridgeStatus::Undocumented
    }));
}

#[test]
fn elevation_enrichment_densifies_rates_and_infers_terrain() {
    let draft = SegmentDraft {
        geometry: LineString::new(vec![Coord::new(0.0, 0.0), Coord::new(0.0, 0.01)]).unwrap(),
        terrain: Terrain::Unknown,
        access: Access::Open,
        road_exposure: 0.0,
        confidence: 0.9,
        provenance: Provenance::fixture("climb"),
    };
    let mut graph = GraphBuilder::default().build(&[draft]).unwrap();
    enrich_graph(
        &mut graph,
        &PlaneElevation {
            origin: Coord::new(0.0, 0.0),
            origin_ele_m: 1_000.0,
            east_gain_m_per_degree: 0.0,
            north_gain_m_per_degree: 40_000.0,
            confidence: 0.77,
        },
        EnrichmentConfig {
            sample_spacing_m: 50.0,
            steep_grade_threshold: 0.15,
        },
        DifficultyWeights::default(),
    )
    .unwrap();
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

fn simple_path_draft() -> SegmentDraft {
    SegmentDraft {
        geometry: LineString::new(vec![
            Coord::with_ele(0.0, 0.0, 1_000.0),
            Coord::with_ele(0.01, 0.0, 1_020.0),
            Coord::with_ele(0.02, 0.0, 1_060.0),
        ])
        .unwrap(),
        terrain: Terrain::Trail,
        access: Access::Open,
        road_exposure: 0.0,
        confidence: 1.0,
        provenance: Provenance::fixture("simple-path"),
    }
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
        access: Access::Open,
        road_exposure: 0.0,
        confidence: 1.0,
        provenance: Provenance::fixture(name),
    })
    .collect()
}
