use crate::constraints::LoopConstraints;
use crate::geo::LineString;
use crate::io::route_file::RouteFileMetadata;
use crate::model::{EdgeId, Provenance, RouteSnapStats, TrailGraph, VertexId};
use crate::route::{Route, RouteMetrics};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeedRoute {
    pub name: String,
    pub source_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_source_path: Option<String>,
    pub source_format: String,
    #[serde(default, skip_serializing_if = "RouteFileMetadata::is_empty")]
    pub metadata: RouteFileMetadata,
    pub point_count: usize,
    pub snapped_edges: Vec<EdgeId>,
    #[serde(default)]
    pub snap: RouteSnapStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<VertexId>,
    pub closed_loop: bool,
    pub metrics: RouteMetrics,
    pub provenance: Provenance,
}

impl SeedRoute {
    #[must_use]
    pub fn snap(
        graph: &TrailGraph,
        name: impl Into<String>,
        source_path: impl Into<String>,
        source_format: impl Into<String>,
        line: &LineString,
    ) -> Self {
        Self::snap_with_limit(graph, name, source_path, source_format, line, f64::INFINITY)
    }

    #[must_use]
    pub fn snap_with_limit(
        graph: &TrailGraph,
        name: impl Into<String>,
        source_path: impl Into<String>,
        source_format: impl Into<String>,
        line: &LineString,
        max_snap_m: f64,
    ) -> Self {
        let snap = graph.trace_coverage(line, max_snap_m);
        let snapped_edges = snap.edges;
        let candidate_start = graph.snapped_line_start(line, &snapped_edges);
        let finish =
            candidate_start.and_then(|v| graph.walk_edges(v, &snapped_edges).map(|f| (v, f)));
        let start = finish.map(|(start, _)| start);
        let closed_loop = finish.is_some_and(|(start, finish)| finish == start);
        let metrics = start.map_or_else(RouteMetrics::default, |v| {
            RouteMetrics::measure(graph, v, &snapped_edges)
        });
        let source_path = source_path.into();
        let source_format = source_format.into();
        let name = name.into();
        let provenance = Provenance {
            source: "seed-route".to_owned(),
            layer: Some(source_format.clone()),
            source_id: Some(name.clone()),
            license: None,
        };
        Self {
            name,
            source_path,
            original_source_path: None,
            source_format,
            metadata: RouteFileMetadata::default(),
            point_count: line.points.len(),
            snapped_edges,
            snap: snap.stats,
            start,
            closed_loop,
            metrics,
            provenance,
        }
    }

    #[must_use]
    pub fn as_route(&self, graph: &TrailGraph, constraints: &LoopConstraints) -> Option<Route> {
        let start = self.start?;
        if !self.closed_loop {
            return None;
        }
        Some(Route::from_edges(
            format!("seed-{}", artifact_key(&self.name)),
            graph,
            start,
            self.snapped_edges.clone(),
            constraints,
        ))
    }
}

#[must_use]
pub fn artifact_key(raw: &str) -> String {
    let mut stem = String::with_capacity(raw.len().min(64));
    for c in raw.chars() {
        if stem.len() == 64 {
            break;
        }
        if c.is_ascii_alphanumeric() {
            stem.push(c.to_ascii_lowercase());
        } else if !stem.is_empty() && !stem.ends_with('-') {
            stem.push('-');
        }
    }
    while stem.ends_with('-') {
        stem.pop();
    }
    if stem.is_empty() {
        stem.push_str("route");
    }
    let scar = raw
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    format!("{stem}-{scar:016x}")
}
