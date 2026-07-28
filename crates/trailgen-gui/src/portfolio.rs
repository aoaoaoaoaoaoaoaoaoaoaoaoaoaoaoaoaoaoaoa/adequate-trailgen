use crate::{gallery::CandidatePreview, map::RouteOverlay, profile::ElevationProfile};
use std::time::Instant;
use trailgen_core::{LoopConstraints, Route, RouteShape, RoutingLaw, Trail, TrailGraph};

pub struct CandidatePortfolio {
    pub routes: Vec<Route>,
    pub designs: Vec<Option<Trail>>,
    pub profiles: Vec<Option<ElevationProfile>>,
    pub previews: Vec<CandidatePreview>,
    pub overlay: RouteOverlay,
}

impl CandidatePortfolio {
    pub fn forge(
        graph: &TrailGraph,
        routes: Vec<Route>,
        routing: RoutingLaw,
        defaults: &LoopConstraints,
        cancelled: impl Fn() -> bool,
    ) -> Option<Self> {
        let begun = Instant::now();
        let stage = Instant::now();
        let designs = gather(&routes, &cancelled, |route| {
            infer_design(graph, route, routing, defaults)
        })?;
        let design_us = stage.elapsed().as_micros();

        let stage = Instant::now();
        let profiles = gather(&routes, &cancelled, |route| {
            ElevationProfile::forge(graph, route)
        })?;
        let profile_us = stage.elapsed().as_micros();

        let stage = Instant::now();
        let previews = gather(&routes, &cancelled, |route| {
            CandidatePreview::forge(graph, route)
        })?;
        let preview_us = stage.elapsed().as_micros();

        if cancelled() {
            return None;
        }
        let stage = Instant::now();
        let overlay = RouteOverlay::candidates(graph, &routes);
        let overlay_us = stage.elapsed().as_micros();
        if std::env::var_os("TRAILGEN_PROFILE_TRAILS").is_some() {
            eprintln!(
                "candidate-portfolio forge_us={} design_us={design_us} profile_us={profile_us} \
                 preview_us={preview_us} overlay_us={overlay_us} routes={}",
                begun.elapsed().as_micros(),
                routes.len(),
            );
        }
        Some(Self {
            routes,
            designs,
            profiles,
            previews,
            overlay,
        })
    }
}

pub fn manual_constraints(defaults: &LoopConstraints, shape: RouteShape) -> LoopConstraints {
    let mut constraints = defaults.clone();
    constraints.min_distance_m = 0.0;
    constraints.max_distance_m = 1.0e9;
    constraints.min_difficulty = 0.0;
    constraints.max_difficulty = 1.0e9;
    constraints.target_difficulty = None;
    constraints.min_ascent_m = 0.0;
    constraints.max_ascent_m = 1.0e9;
    constraints.min_descent_m = 0.0;
    constraints.max_descent_m = 1.0e9;
    constraints.max_road_fraction = 1.0;
    constraints.max_low_confidence_fraction = 1.0;
    constraints.max_repeated_edge_fraction = if shape == RouteShape::OutAndBack {
        1.0
    } else {
        0.0
    };
    constraints.allowed_shapes = vec![shape];
    constraints
}

fn gather<T>(
    routes: &[Route],
    cancelled: &impl Fn() -> bool,
    mut forge: impl FnMut(&Route) -> T,
) -> Option<Vec<T>> {
    let mut values = Vec::with_capacity(routes.len());
    for route in routes {
        if cancelled() {
            return None;
        }
        values.push(forge(route));
    }
    Some(values)
}

fn infer_design(
    graph: &TrailGraph,
    route: &Route,
    routing: RoutingLaw,
    defaults: &LoopConstraints,
) -> Option<Trail> {
    let trail = Trail::infer(graph, route, routing)?;
    let realized = trail
        .realize(
            route.name.clone(),
            graph,
            &manual_constraints(defaults, route.metrics.shape),
            1.0,
        )
        .ok()?;
    (realized.route.edges == route.edges).then_some(trail)
}
