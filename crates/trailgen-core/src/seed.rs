use crate::constraints::LoopConstraints;
use crate::geo::LineString;
use crate::model::{EdgeId, Provenance, TrailGraph, VertexId};
use crate::route::{Route, RouteMetrics};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeedRoute {
    pub name: String,
    pub source_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_source_path: Option<String>,
    pub source_format: String,
    pub point_count: usize,
    pub snapped_edges: Vec<EdgeId>,
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
        let snapped_edges = graph.snap_line_edges(line);
        let start = graph.snapped_line_start(line, &snapped_edges);
        let closed_loop = start
            .and_then(|v| graph.walk_edges(v, &snapped_edges))
            .is_some_and(|finish| Some(finish) == start);
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
            point_count: line.points.len(),
            snapped_edges,
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
            format!("seed-{}", slug(&self.name)),
            graph,
            start,
            self.snapped_edges.clone(),
            constraints,
        ))
    }
}

#[must_use]
pub fn slug(raw: &str) -> String {
    let slug = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    slug.split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}
