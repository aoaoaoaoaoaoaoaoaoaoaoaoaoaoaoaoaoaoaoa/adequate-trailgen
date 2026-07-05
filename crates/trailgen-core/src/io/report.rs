use crate::difficulty::DifficultyFactor;
use crate::model::{Access, Edge, TrailGraph};
use crate::route::Route;
use std::fmt::Write as _;

#[must_use]
pub fn render(graph: &TrailGraph, routes: &[Route]) -> String {
    let mut s = String::from("# Generated Hiking Routes\n\n");
    if routes.is_empty() {
        s.push_str("No candidate routes were generated.\n");
        return s;
    }
    for route in routes {
        render_route(graph, route, &mut s);
    }
    s
}

fn render_route(graph: &TrailGraph, route: &Route, s: &mut String) {
    let _ = write!(s, "## {}\n\n", route.name);
    let _ = write!(
        s,
        "- score: {:.2}\n- pareto rank: {}\n- shape: {:?}\n- distance: {:.2} km\n- ascent/descent: {:.0} m / {:.0} m\n- scalar difficulty: {:.2}\n- road/pavement exposure: {:.1}%\n- low-confidence fraction: {:.1}%\n- restricted-access fraction: {:.1}%\n- repeated-edge fraction: {:.1}%\n- constraint verdict: {}\n",
        route.score,
        route.pareto_rank,
        route.metrics.shape,
        route.metrics.distance_m / 1_000.0,
        route.metrics.ascent_m,
        route.metrics.descent_m,
        route.metrics.difficulty,
        route.metrics.road_fraction * 100.0,
        route.metrics.low_confidence_fraction * 100.0,
        route.metrics.restricted_access_fraction * 100.0,
        route.metrics.repeated_edge_fraction * 100.0,
        if route.verdict.satisfied {
            "satisfied"
        } else {
            "violated"
        },
    );
    render_violations(route, s);
    render_difficulty(route, s);
    render_access_mix(route, s);
    render_access_warnings(graph, route, s);
    render_crossings(route, s);
    render_terrain_mix(route, s);
    render_difficulty_hotspots(graph, route, s);
    render_dubious_edges(graph, route, s);
    render_evidence(graph, route, s);
    s.push('\n');
}

fn render_crossings(route: &Route, s: &mut String) {
    if route.metrics.crossings.is_empty() {
        return;
    }
    s.push_str("\nCrossings:\n");
    for (kind, count) in &route.metrics.crossings {
        let _ = writeln!(s, "- {kind:?}: {count}");
    }
}

fn render_difficulty(route: &Route, s: &mut String) {
    s.push_str("\nDifficulty decomposition:\n");
    let total = route.metrics.difficulty_breakdown.total();
    if total <= f64::EPSILON && route.metrics.difficulty > f64::EPSILON {
        let _ = writeln!(
            s,
            "- legacy scalar-only difficulty: {:.2}",
            route.metrics.difficulty
        );
        return;
    }
    let denominator = total.max(1.0);
    for (factor, value) in sorted_factors(route.metrics.difficulty_breakdown.factors()) {
        if value <= f64::EPSILON {
            continue;
        }
        let _ = writeln!(
            s,
            "- {factor}: {:.2} ({:.1}%)",
            value,
            value / denominator * 100.0
        );
    }
}

fn render_violations(route: &Route, s: &mut String) {
    if route.verdict.violations.is_empty() {
        return;
    }
    s.push_str("\nViolations:\n");
    for violation in &route.verdict.violations {
        let _ = writeln!(s, "- {violation}");
    }
}

fn render_access_mix(route: &Route, s: &mut String) {
    s.push_str("\nAccess mix:\n");
    for (access, fraction) in route.metrics.access_percentages() {
        let _ = writeln!(s, "- {access:?}: {:.1}%", fraction * 100.0);
    }
}

fn render_access_warnings(graph: &TrailGraph, route: &Route, s: &mut String) {
    let warnings = route
        .edges
        .iter()
        .map(|id| &graph.edges[id.0])
        .filter(|edge| !matches!(edge.attr.access, Access::Unknown | Access::Open))
        .collect::<Vec<_>>();
    if warnings.is_empty() {
        return;
    }
    s.push_str("\nAccess warnings:\n");
    for edge in warnings {
        let prov = edge.attr.access_provenance.first().map_or_else(
            || "unknown".to_owned(),
            |p| {
                p.source_id
                    .as_ref()
                    .map_or_else(|| p.source.clone(), |id| format!("{}:{id}", p.source))
            },
        );
        let _ = writeln!(
            s,
            "- edge {}: {:?}, confidence {:.0}%, provenance {prov}",
            edge.id.0,
            edge.attr.access,
            edge.attr.access_confidence * 100.0
        );
    }
}

fn render_terrain_mix(route: &Route, s: &mut String) {
    s.push_str("\nTerrain mix:\n");
    for (terrain, fraction) in route.metrics.terrain_percentages() {
        let _ = writeln!(s, "- {terrain:?}: {:.1}%", fraction * 100.0);
    }
}

fn render_difficulty_hotspots(graph: &TrailGraph, route: &Route, s: &mut String) {
    let mut hotspots = route
        .edges
        .iter()
        .flat_map(|id| {
            let edge = &graph.edges[id.0];
            edge.attr
                .difficulty_breakdown
                .factors()
                .into_iter()
                .filter(|(_, value)| *value > f64::EPSILON)
                .map(move |(factor, value)| (edge, factor, value))
        })
        .collect::<Vec<_>>();
    if hotspots.is_empty() {
        return;
    }
    hotspots.sort_by(|a, b| b.2.total_cmp(&a.2));
    s.push_str("\nLargest difficulty contributors:\n");
    let denominator = route.metrics.difficulty.max(1.0);
    for (edge, factor, value) in hotspots.into_iter().take(5) {
        let _ = writeln!(
            s,
            "- edge {} {factor}: {:.2} ({:.1}% of route), {:?}, {:.0} m",
            edge.id.0,
            value,
            value / denominator * 100.0,
            edge.attr.terrain,
            edge.attr.length_m
        );
    }
}

fn render_dubious_edges(graph: &TrailGraph, route: &Route, s: &mut String) {
    s.push_str("\nMost dubious segments:\n");
    let mut dubious = route
        .edges
        .iter()
        .map(|id| &graph.edges[id.0])
        .collect::<Vec<_>>();
    dubious.sort_by(|a, b| a.attr.confidence.total_cmp(&b.attr.confidence));
    for edge in dubious.into_iter().take(5) {
        render_dubious_edge(edge, s);
    }
}

fn render_dubious_edge(edge: &Edge, s: &mut String) {
    let prov = edge.attr.provenance.first().map_or_else(
        || "unknown".to_owned(),
        |p| {
            p.source_id
                .as_ref()
                .map_or_else(|| p.source.clone(), |id| format!("{}:{id}", p.source))
        },
    );
    let _ = writeln!(
        s,
        "- edge {}: {:.0} m, {:?}, grade max {:.1}%, grade bins {}, crossings {}, confidence {:.2}, seed count {}, provenance {prov}",
        edge.id.0,
        edge.attr.length_m,
        edge.attr.terrain,
        edge.attr.grade_abs_max * 100.0,
        grade_bins(edge),
        edge.attr.crossings.iter().map(|x| x.count).sum::<u32>(),
        edge.attr.confidence,
        edge.attr.seed_count
    );
}

fn grade_bins(edge: &Edge) -> String {
    let d = edge.attr.grade_distribution;
    let total = d.total_m().max(1.0);
    format!(
        "flat {:.0}% / rolling {:.0}% / steep {:.0}% / savage {:.0}%",
        d.flat_m / total * 100.0,
        d.rolling_m / total * 100.0,
        d.steep_m / total * 100.0,
        d.savage_m / total * 100.0
    )
}

fn render_evidence(graph: &TrailGraph, route: &Route, s: &mut String) {
    s.push_str("\nTerrain/elevation evidence:\n");
    for edge_id in route.edges.iter().take(8) {
        let edge = &graph.edges[edge_id.0];
        let terrain = edge.attr.terrain_evidence.first().map_or_else(
            || "no terrain evidence".to_owned(),
            |e| {
                format!(
                    "{:?} {:.0}%: {}",
                    e.terrain,
                    e.confidence * 100.0,
                    e.rationale
                )
            },
        );
        let elevation = edge.attr.elevation_provenance.first().map_or_else(
            || "no elevation provenance".to_owned(),
            |p| p.source.clone(),
        );
        let _ = writeln!(
            s,
            "- edge {}: {terrain}; elevation source {elevation}",
            edge.id.0
        );
    }
}

fn sorted_factors<const N: usize>(
    factors: [(DifficultyFactor, f64); N],
) -> Vec<(DifficultyFactor, f64)> {
    let mut factors = Vec::from(factors);
    factors.sort_by(|a, b| b.1.total_cmp(&a.1));
    factors
}
