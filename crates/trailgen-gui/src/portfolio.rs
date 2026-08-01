use crate::{gallery::CandidatePreview, map::RouteOverlay, profile::ElevationProfile};
use std::{collections::BTreeMap, sync::Arc, time::Instant};
use trailgen_core::{
    EdgeId, LoopConstraints, Route, RouteShape, RoutingLaw, Trail, VertexId, WalkGraph,
};

pub struct CandidatePortfolio {
    pub routes: Arc<[Route]>,
    pub designs: Arc<[Arc<Trail>]>,
    pub profiles: Arc<[Option<Arc<ElevationProfile>>]>,
    pub previews: Arc<[Arc<CandidatePreview>]>,
    pub identities: Arc<[usize]>,
    pub overlay: RouteOverlay,
}

#[derive(Clone, Default)]
pub struct CandidateWarmth {
    routes: Arc<[Route]>,
    designs: Arc<[Arc<Trail>]>,
    profiles: Arc<[Option<Arc<ElevationProfile>>]>,
    previews: Arc<[Arc<CandidatePreview>]>,
    identities: Arc<[usize]>,
}

impl CandidatePortfolio {
    #[must_use]
    pub fn slot(&self, identity: usize) -> Option<usize> {
        self.identities.iter().position(|known| *known == identity)
    }

    #[must_use]
    pub fn warmth(&self) -> CandidateWarmth {
        CandidateWarmth {
            routes: Arc::clone(&self.routes),
            designs: Arc::clone(&self.designs),
            profiles: Arc::clone(&self.profiles),
            previews: Arc::clone(&self.previews),
            identities: Arc::clone(&self.identities),
        }
    }

    pub fn forge(
        graph: &WalkGraph,
        routes: Vec<Route>,
        routing: RoutingLaw,
        defaults: &LoopConstraints,
        warmth: &CandidateWarmth,
        cancelled: impl Fn() -> bool,
    ) -> Option<Self> {
        let begun = Instant::now();
        let prior = warmth.index();
        let stage = Instant::now();
        let mut designs = Vec::with_capacity(routes.len());
        let routes = routes
            .into_iter()
            .filter(|route| {
                if cancelled() {
                    return false;
                }
                let design = prior
                    .get(&RouteKey::forge(route))
                    .map(|slot| Arc::clone(&warmth.designs[*slot]))
                    .or_else(|| infer_design(graph, route, routing, defaults).map(Arc::new));
                design.is_some_and(|design| {
                    designs.push(design);
                    true
                })
            })
            .collect::<Vec<_>>();
        if cancelled() {
            return None;
        }
        let design_us = stage.elapsed().as_micros();
        let next_identity = warmth
            .identities
            .iter()
            .copied()
            .max()
            .map_or(0, |identity| identity.saturating_add(1));
        let identities = routes
            .iter()
            .scan(next_identity, |next, route| {
                let identity = prior.get(&RouteKey::forge(route)).map_or_else(
                    || {
                        let identity = *next;
                        *next = next.saturating_add(1);
                        identity
                    },
                    |slot| warmth.identities[*slot],
                );
                Some(identity)
            })
            .collect::<Vec<_>>();

        let stage = Instant::now();
        let profiles = gather(&routes, &cancelled, |route| {
            prior
                .get(&RouteKey::forge(route))
                .and_then(|slot| warmth.profiles[*slot].clone())
                .or_else(|| ElevationProfile::forge(graph, route).map(Arc::new))
        })?;
        let profile_us = stage.elapsed().as_micros();

        let stage = Instant::now();
        let previews = gather(&routes, &cancelled, |route| {
            prior.get(&RouteKey::forge(route)).map_or_else(
                || Arc::new(CandidatePreview::forge(graph, route)),
                |slot| Arc::clone(&warmth.previews[*slot]),
            )
        })?;
        let preview_us = stage.elapsed().as_micros();

        if cancelled() {
            return None;
        }
        let stage = Instant::now();
        let overlay = RouteOverlay::candidates(graph, &routes, &identities);
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
            routes: routes.into(),
            designs: designs.into(),
            profiles: profiles.into(),
            previews: previews.into(),
            identities: identities.into(),
            overlay,
        })
    }
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct RouteKey {
    start: VertexId,
    edges: Vec<EdgeId>,
}

impl RouteKey {
    fn forge(route: &Route) -> Self {
        Self {
            start: route.start,
            edges: route.edges.clone(),
        }
    }
}

impl CandidateWarmth {
    fn index(&self) -> BTreeMap<RouteKey, usize> {
        self.routes
            .iter()
            .enumerate()
            .map(|(slot, route)| (RouteKey::forge(route), slot))
            .collect()
    }

    pub fn routes(&self) -> &[Route] {
        &self.routes
    }
}

pub fn manual_constraints(defaults: &LoopConstraints, shape: RouteShape) -> LoopConstraints {
    let mut constraints = defaults.clone();
    constraints.min_distance_m = 0.0;
    constraints.max_distance_m = 1.0e9;
    constraints.min_lower_limb_load_km = 0.0;
    constraints.max_lower_limb_load_km = 1.0e9;
    constraints.target_lower_limb_load_km = None;
    constraints.min_moving_time_s = 0.0;
    constraints.max_moving_time_s = 1.0e12;
    constraints.min_ascent_m = 0.0;
    constraints.max_ascent_m = 1.0e9;
    constraints.min_descent_m = 0.0;
    constraints.max_descent_m = 1.0e9;
    constraints.max_road_fraction = 1.0;
    constraints.max_low_confidence_fraction = 1.0;
    constraints.max_repeated_edge_fraction = 1.0;
    constraints.allowed_shapes = match shape {
        // `Trail::shape` is the authored design, whereas `RouteMetrics::shape`
        // records the walk that design realizes. Every closed morphology is a
        // lawful manual loop; generated searches retain their strict defaults.
        RouteShape::Loop => vec![
            RouteShape::Loop,
            RouteShape::FigureEight,
            RouteShape::OutAndBack,
        ],
        _ => vec![shape],
    };
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
    graph: &WalkGraph,
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
    let lhs = &realized.route.metrics;
    let rhs = &route.metrics;
    (lhs.shape == rhs.shape
        && same_path(
            &route.geometry(graph),
            &realized.route.geometry(realized.graph()),
        )
        && (lhs.distance_m - rhs.distance_m).abs() < 0.05
        && (lhs.ascent_m - rhs.ascent_m).abs() < 0.05
        && (lhs.descent_m - rhs.descent_m).abs() < 0.05)
        .then_some(trail)
}

fn same_path(left: &trailgen_core::LineString, right: &trailgen_core::LineString) -> bool {
    const EPSILON2: f64 = 1.0e-18;
    let endpoints_match = left.start().planar_distance2(right.start()) <= EPSILON2
        && left.end().planar_distance2(right.end()) <= EPSILON2;
    let contained = |point: trailgen_core::Coord, line: &trailgen_core::LineString| {
        line.points.windows(2).any(|segment| {
            let dx = segment[1].lon - segment[0].lon;
            let dy = segment[1].lat - segment[0].lat;
            let span2 = dx.mul_add(dx, dy * dy);
            let t = if span2 <= f64::EPSILON {
                0.0
            } else {
                (point.lat - segment[0].lat).mul_add(dy, (point.lon - segment[0].lon) * dx) / span2
            }
            .clamp(0.0, 1.0);
            let projected = trailgen_core::Coord::new(
                dx.mul_add(t, segment[0].lon),
                dy.mul_add(t, segment[0].lat),
            );
            point.planar_distance2(projected) <= EPSILON2
        })
    };
    endpoints_match
        && left
            .points
            .iter()
            .copied()
            .all(|point| contained(point, right))
        && right
            .points
            .iter()
            .copied()
            .all(|point| contained(point, left))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use trailgen_core::{ExactLoopSolver, GraphBuilder, RouteMetrics, VertexId, io::geojson};

    fn fixture() -> Result<WalkGraph> {
        Ok(
            GraphBuilder::default().build(&geojson::network_from_str(include_str!(
                "../../trailgen-core/tests/fixtures/mini_network.geojson"
            ))?)?,
        )
    }

    #[test]
    fn warmed_portfolio_preserves_identity_and_derived_artifacts() -> Result<()> {
        let graph = fixture()?;
        let constraints = LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: f64::MAX,
            max_lower_limb_load_km: f64::MAX,
            max_repeated_edge_fraction: 0.0,
            allowed_shapes: vec![RouteShape::Loop],
            ..LoopConstraints::default()
        };
        let routes = ExactLoopSolver::default().enumerate(&graph, VertexId(0), &constraints, 4);
        let first = CandidatePortfolio::forge(
            &graph,
            routes,
            RoutingLaw::default(),
            &constraints,
            &CandidateWarmth::default(),
            || false,
        )
        .expect("forge portfolio");
        assert_eq!(first.designs.len(), first.routes.len());
        let mut reordered = first.routes.iter().cloned().collect::<Vec<_>>();
        reordered.reverse();
        let second = CandidatePortfolio::forge(
            &graph,
            reordered,
            RoutingLaw::default(),
            &constraints,
            &first.warmth(),
            || false,
        )
        .expect("forge warmed portfolio");
        for (slot, route) in second.routes.iter().enumerate() {
            let prior = first
                .routes
                .iter()
                .position(|candidate| {
                    candidate.start == route.start && candidate.edges == route.edges
                })
                .expect("route survived revision");
            assert_eq!(second.identities[slot], first.identities[prior]);
            assert_eq!(second.slot(first.identities[prior]), Some(slot));
            assert!(Arc::ptr_eq(&second.previews[slot], &first.previews[prior]));
        }
        Ok(())
    }

    #[test]
    fn manual_loops_admit_closed_morphologies_without_weakening_search_defaults() {
        let defaults = LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: f64::MAX,
            max_repeated_edge_fraction: 0.0,
            allowed_shapes: vec![RouteShape::Loop],
            ..LoopConstraints::default()
        };
        let manual = manual_constraints(&defaults, RouteShape::Loop);
        let repeated = RouteMetrics {
            shape: RouteShape::OutAndBack,
            distance_m: 1_000.0,
            repeated_edge_fraction: 0.5,
            ..RouteMetrics::default()
        };

        assert!(defaults.max_repeated_edge_fraction.abs() < f64::EPSILON);
        assert_eq!(defaults.allowed_shapes, [RouteShape::Loop]);
        assert!(!defaults.judge(&repeated).satisfied);
        assert!((manual.max_repeated_edge_fraction - 1.0).abs() < f64::EPSILON);
        assert_eq!(
            manual.allowed_shapes,
            [
                RouteShape::Loop,
                RouteShape::FigureEight,
                RouteShape::OutAndBack,
            ]
        );
        assert!(manual.judge(&repeated).satisfied);
    }
}
