use crate::builder::SegmentDraft;
use crate::crs::{CoordProjector, CrsVerdict, VectorCrsKind, projector, validate_crs_name};
use crate::geo::{Coord, LineString};
use crate::io::route_file::{RouteFile, RouteFileMetadata};
use crate::model::{Access, CrossingKind, Edge, EdgeTravel, Provenance, Terrain, TrailGraph};
use crate::overlay::{
    AccessOverlay, AccessWindow, ContextOverlay, OverlayGeometry, PlanningDate, TerrainOverlay,
    polygon,
};
use crate::route::{LOW_CONFIDENCE_THRESHOLD, Route, is_restricted_access};
use crate::{Result, TrailgenError};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

pub fn network_from_str(s: &str) -> Result<Vec<SegmentDraft>> {
    let root: Value = serde_json::from_str(s)?;
    let crs = geojson_projector(&root)?;
    let features = root
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            TrailgenError::InvalidData("GeoJSON FeatureCollection expected".to_owned())
        })?;
    let mut drafts = Vec::new();
    for (i, feature) in features.iter().enumerate() {
        let properties = feature
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let surface = prop_str(&properties, "surface").map(str::to_owned);
        let terrain = prop_str(&properties, "terrain")
            .or(surface.as_deref())
            .map_or(Terrain::Unknown, Terrain::from_tag);
        let access = prop_str(&properties, "access").map_or(Access::Unknown, Access::from_tag);
        let travel = travel_from_properties(&properties);
        let confidence = prop_f64(&properties, "confidence").unwrap_or(0.75);
        let road_exposure = prop_f64(&properties, "road_exposure")
            .or_else(|| prop_bool(&properties, "road").map(f64::from))
            .unwrap_or_else(|| f64::from(matches!(terrain, Terrain::Road | Terrain::Pavement)));
        let source = prop_str(&properties, "source")
            .unwrap_or("geojson")
            .to_owned();
        let source_id = prop_str(&properties, "id")
            .or_else(|| prop_str(&properties, "name"))
            .map(str::to_owned)
            .or_else(|| Some(format!("feature-{i}")));
        let provenance = Provenance {
            source,
            layer: prop_str(&properties, "layer").map(str::to_owned),
            source_id,
            license: prop_str(&properties, "license").map(str::to_owned),
        };
        let Some(geometry) = feature.get("geometry") else {
            continue;
        };
        for line in lines_from_geometry(geometry, crs)? {
            drafts.push(SegmentDraft {
                geometry: line,
                terrain,
                surface: surface.clone(),
                access,
                travel,
                road_exposure,
                confidence,
                provenance: provenance.clone(),
            });
        }
    }
    Ok(drafts)
}

pub fn route_line_from_str(s: &str) -> Result<LineString> {
    route_file_from_str(s).map(|route| route.line)
}

pub fn route_file_from_str(s: &str) -> Result<RouteFile> {
    let root: Value = serde_json::from_str(s)?;
    let crs = geojson_projector(&root)?;
    if root.get("type").and_then(Value::as_str) == Some("FeatureCollection") {
        let features = root
            .get("features")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                TrailgenError::InvalidData("GeoJSON FeatureCollection expected".to_owned())
            })?;
        for feature in features {
            if let Some(geometry) = feature.get("geometry")
                && let Some(line) = lines_from_geometry(geometry, crs)?.into_iter().next()
            {
                return Ok(RouteFile::new(line, metadata_from_feature(feature)));
            }
        }
    }
    if root.get("type").and_then(Value::as_str) == Some("Feature") {
        let line = lines_from_geometry(
            root.get("geometry").ok_or_else(|| {
                TrailgenError::InvalidData("GeoJSON feature has no geometry".to_owned())
            })?,
            crs,
        )?
        .into_iter()
        .next()
        .ok_or_else(|| TrailgenError::InvalidGeometry("no LineString in GeoJSON".to_owned()))?;
        return Ok(RouteFile::new(line, metadata_from_feature(&root)));
    }
    let line = lines_from_geometry(&root, crs)?
        .into_iter()
        .next()
        .ok_or_else(|| TrailgenError::InvalidGeometry("no LineString in GeoJSON".to_owned()))?;
    Ok(RouteFile::new(line, RouteFileMetadata::default()))
}

pub fn access_overlays_from_str(s: &str) -> Result<Vec<AccessOverlay>> {
    let root: Value = serde_json::from_str(s)?;
    let crs = geojson_projector(&root)?;
    let features = root
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            TrailgenError::InvalidData("GeoJSON FeatureCollection expected".to_owned())
        })?;
    features
        .iter()
        .enumerate()
        .map(|(i, feature)| overlay_from_feature(feature, i, crs))
        .collect()
}

pub fn terrain_overlays_from_str(s: &str) -> Result<Vec<TerrainOverlay>> {
    let root: Value = serde_json::from_str(s)?;
    let crs = geojson_projector(&root)?;
    let features = root
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            TrailgenError::InvalidData("GeoJSON FeatureCollection expected".to_owned())
        })?;
    features
        .iter()
        .enumerate()
        .map(|(i, feature)| terrain_overlay_from_feature(feature, i, crs))
        .collect()
}

pub fn context_overlays_from_str(s: &str) -> Result<Vec<ContextOverlay>> {
    let root: Value = serde_json::from_str(s)?;
    let crs = geojson_projector(&root)?;
    let features = root
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            TrailgenError::InvalidData("GeoJSON FeatureCollection expected".to_owned())
        })?;
    Ok(features
        .iter()
        .enumerate()
        .map(|(i, feature)| context_from_feature(feature, i, crs))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

#[must_use]
pub fn routes_to_geojson(graph: &TrailGraph, routes: &[Route]) -> Value {
    json!({
        "type": "FeatureCollection",
        "features": routes.iter().map(|route| route_feature(graph, route)).collect::<Vec<_>>()
    })
}

#[must_use]
pub fn graph_to_geojson(graph: &TrailGraph) -> Value {
    json!({
        "type": "FeatureCollection",
        "features": graph.edges.iter().map(|edge| {
            let mut properties = Map::from_iter([
                ("edge_id".to_owned(), json!(edge.id.0)),
                ("from".to_owned(), json!(edge.a.0)),
                ("to".to_owned(), json!(edge.b.0)),
                ("length_m".to_owned(), json!(edge.attr.length_m)),
                ("ascent_m".to_owned(), json!(edge.attr.ascent_m)),
                ("descent_m".to_owned(), json!(edge.attr.descent_m)),
                ("grade_abs_mean".to_owned(), json!(edge.attr.grade_abs_mean)),
                ("grade_abs_max".to_owned(), json!(edge.attr.grade_abs_max)),
                ("sustained_steep_m".to_owned(), json!(edge.attr.sustained_steep_m)),
                ("grade_distribution".to_owned(), json!(edge.attr.grade_distribution)),
                ("difficulty".to_owned(), json!(edge.attr.difficulty)),
                ("difficulty_breakdown".to_owned(), json!(edge.attr.difficulty_breakdown)),
                ("terrain".to_owned(), json!(edge.attr.terrain)),
                ("travel".to_owned(), json!(edge.attr.travel)),
                ("terrain_confidence".to_owned(), json!(edge.attr.terrain_confidence)),
                ("terrain_evidence".to_owned(), json!(edge.attr.terrain_evidence)),
                ("access".to_owned(), json!(edge.attr.access)),
                ("access_confidence".to_owned(), json!(edge.attr.access_confidence)),
                ("access_provenance".to_owned(), json!(edge.attr.access_provenance)),
                ("crossings".to_owned(), json!(edge.attr.crossings)),
                ("confidence".to_owned(), json!(edge.attr.confidence)),
                ("seed_count".to_owned(), json!(edge.attr.seed_count)),
                ("popularity".to_owned(), json!(edge.attr.popularity)),
                ("seed_provenance".to_owned(), json!(edge.attr.seed_provenance)),
                ("road_exposure".to_owned(), json!(edge.attr.road_exposure)),
                ("elevation_provenance".to_owned(), json!(edge.attr.elevation_provenance)),
                ("provenance".to_owned(), json!(edge.attr.provenance)),
            ]);
            if let Some(surface) = &edge.attr.surface {
                properties.insert("surface".to_owned(), json!(surface));
            }
            json!({
                "type": "Feature",
                "properties": properties,
                "geometry": line_geometry(&edge.geometry),
            })
        }).collect::<Vec<_>>()
    })
}

fn route_feature(graph: &TrailGraph, route: &Route) -> Value {
    json!({
        "type": "Feature",
        "properties": {
            "name": route.name,
            "score": route.computed_score(),
            "pareto_rank": route.pareto_rank,
            "constraint_penalty": route.verdict.penalty,
            "distance_m": route.metrics.distance_m,
            "ascent_m": route.metrics.ascent_m,
            "descent_m": route.metrics.descent_m,
            "shape": route.metrics.shape,
            "difficulty": route.metrics.difficulty,
            "difficulty_breakdown": route.metrics.difficulty_breakdown,
            "road_fraction": route.metrics.road_fraction,
            "low_confidence_fraction": route.metrics.low_confidence_fraction,
            "restricted_access_fraction": route.metrics.restricted_access_fraction,
            "repeated_edge_fraction": route.metrics.repeated_edge_fraction,
            "terrain_m": route.metrics.terrain_m,
            "terrain_fraction": route.metrics.terrain_percentages(),
            "access_m": route.metrics.access_m,
            "access_fraction": route.metrics.access_percentages(),
            "crossings": route.metrics.crossings,
            "satisfied": route.verdict.satisfied,
            "violations": route.verdict.violations,
            "constraint_audit": route.verdict.audit,
            "edge_count": route.edges.len(),
            "edges": route.edges.iter().map(|id| id.0).collect::<Vec<_>>(),
            "difficulty_hotspots": route_difficulty_hotspots(graph, route),
            "access_warning_edges": route_access_warning_edges(graph, route),
            "low_confidence_edges": route_low_confidence_edges(graph, route),
            "dubious_edges": route_dubious_edges(graph, route),
            "source_provenance": route_source_provenance(graph, route),
        },
        "geometry": line_geometry(&route.geometry(graph)),
    })
}

fn route_difficulty_hotspots(graph: &TrailGraph, route: &Route) -> Vec<Value> {
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
    hotspots.sort_by(|a, b| b.2.total_cmp(&a.2));
    let denominator = route.metrics.difficulty.max(1.0);
    hotspots
        .into_iter()
        .take(5)
        .map(|(edge, factor, value)| {
            json!({
                "edge_id": edge.id.0,
                "factor": factor,
                "value": value,
                "route_fraction": value / denominator,
                "terrain": edge.attr.terrain,
                "access": edge.attr.access,
                "length_m": edge.attr.length_m,
                "confidence": edge.attr.confidence,
            })
        })
        .collect()
}

fn route_low_confidence_edges(graph: &TrailGraph, route: &Route) -> Vec<Value> {
    let mut edges = route
        .edges
        .iter()
        .map(|id| &graph.edges[id.0])
        .filter(|edge| edge.attr.confidence < LOW_CONFIDENCE_THRESHOLD)
        .collect::<Vec<_>>();
    edges.sort_by(|a, b| {
        a.attr
            .confidence
            .total_cmp(&b.attr.confidence)
            .then_with(|| b.attr.length_m.total_cmp(&a.attr.length_m))
    });
    edges.into_iter().map(route_edge_diagnostic).collect()
}

fn route_access_warning_edges(graph: &TrailGraph, route: &Route) -> Vec<Value> {
    let mut edges = route
        .edges
        .iter()
        .map(|id| &graph.edges[id.0])
        .filter(|edge| is_restricted_access(edge.attr.access))
        .collect::<Vec<_>>();
    edges.sort_by(|a, b| {
        b.attr
            .access
            .cmp(&a.attr.access)
            .then_with(|| b.attr.length_m.total_cmp(&a.attr.length_m))
    });
    edges.into_iter().map(route_edge_diagnostic).collect()
}

fn route_dubious_edges(graph: &TrailGraph, route: &Route) -> Vec<Value> {
    let mut dubious = route
        .edges
        .iter()
        .map(|id| &graph.edges[id.0])
        .collect::<Vec<_>>();
    dubious.sort_by(|a, b| {
        a.attr
            .confidence
            .total_cmp(&b.attr.confidence)
            .then_with(|| b.attr.length_m.total_cmp(&a.attr.length_m))
    });
    dubious
        .into_iter()
        .take(5)
        .map(route_edge_diagnostic)
        .collect()
}

fn route_edge_diagnostic(edge: &Edge) -> Value {
    json!({
        "edge_id": edge.id.0,
        "length_m": edge.attr.length_m,
        "terrain": edge.attr.terrain,
        "surface": edge.attr.surface.as_deref(),
        "access": edge.attr.access,
        "confidence": edge.attr.confidence,
        "terrain_confidence": edge.attr.terrain_confidence,
        "access_confidence": edge.attr.access_confidence,
        "access_provenance": edge.attr.access_provenance,
        "grade_abs_max": edge.attr.grade_abs_max,
        "grade_distribution": edge.attr.grade_distribution,
        "crossings": &edge.attr.crossings,
        "seed_count": edge.attr.seed_count,
        "provenance": primary_provenance_label(edge),
    })
}

fn route_source_provenance(graph: &TrailGraph, route: &Route) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    let mut provenance = Vec::new();
    for p in route
        .edges
        .iter()
        .flat_map(|id| graph.edges[id.0].attr.provenance.iter())
    {
        let key = (
            p.source.clone(),
            p.layer.clone(),
            p.source_id.clone(),
            p.license.clone(),
        );
        if seen.insert(key) {
            provenance.push(json!(p));
        }
    }
    provenance
}

fn primary_provenance_label(edge: &Edge) -> String {
    edge.attr.provenance.first().map_or_else(
        || "unknown".to_owned(),
        |p| {
            p.source_id
                .as_ref()
                .map_or_else(|| p.source.clone(), |id| format!("{}:{id}", p.source))
        },
    )
}

fn metadata_from_feature(feature: &Value) -> RouteFileMetadata {
    let properties = feature
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    RouteFileMetadata {
        title: prop_str_any(
            &properties,
            &["name", "title", "route_name", "activity_name"],
        )
        .map(str::to_owned),
        description: prop_str_any(&properties, &["description", "desc", "comment", "notes"])
            .map(str::to_owned),
        recorded_at: prop_str_any(
            &properties,
            &[
                "recorded_at",
                "time",
                "timestamp",
                "start_time",
                "created_at",
                "date",
            ],
        )
        .map(str::to_owned),
        activity_type: prop_str_any(&properties, &["activity_type", "activity", "sport", "type"])
            .map(str::to_owned),
    }
}

fn geojson_projector(value: &Value) -> Result<CoordProjector> {
    let mut crs = None;
    collect_geojson_projector(value, &mut crs)?;
    Ok(crs.map_or_else(|| projector(CrsVerdict::AssumedGeographic), projector))
}

fn collect_geojson_projector(value: &Value, seen: &mut Option<CrsVerdict>) -> Result<()> {
    if let Some(crs) = value.get("crs") {
        let verdict = validate_crs_name(VectorCrsKind::GeoJson, geojson_crs_name(crs)?)?;
        if seen.is_some_and(|previous| previous != verdict) {
            return Err(TrailgenError::InvalidData(
                "GeoJSON contains conflicting CRS declarations".to_owned(),
            ));
        }
        *seen = Some(verdict);
    }
    match value.get("type").and_then(Value::as_str) {
        Some("FeatureCollection") => {
            if let Some(features) = value.get("features").and_then(Value::as_array) {
                for feature in features {
                    collect_geojson_projector(feature, seen)?;
                }
            }
        }
        Some("Feature") => {
            if let Some(geometry) = value.get("geometry") {
                collect_geojson_projector(geometry, seen)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn geojson_crs_name(crs: &Value) -> Result<&str> {
    crs.get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| properties.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            TrailgenError::InvalidData(
                "GeoJSON CRS must be a named WGS84/CRS84 object; linked or opaque CRS definitions are unsupported"
                    .to_owned(),
            )
        })
}

fn lines_from_geometry(geometry: &Value, crs: CoordProjector) -> Result<Vec<LineString>> {
    match geometry.get("type").and_then(Value::as_str) {
        Some("LineString") => Ok(vec![LineString::new(coords(
            geometry.get("coordinates").ok_or_else(|| {
                TrailgenError::InvalidGeometry("LineString lacks coordinates".to_owned())
            })?,
            crs,
        )?)?]),
        Some("MultiLineString") => geometry
            .get("coordinates")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                TrailgenError::InvalidGeometry("MultiLineString lacks coordinates".to_owned())
            })?
            .iter()
            .map(|xs| LineString::new(coords(xs, crs)?))
            .collect(),
        Some(other) => Err(TrailgenError::UnsupportedFormat(format!(
            "GeoJSON geometry {other}"
        ))),
        None => Err(TrailgenError::InvalidGeometry(
            "GeoJSON geometry lacks type".to_owned(),
        )),
    }
}

fn overlay_from_feature(feature: &Value, i: usize, crs: CoordProjector) -> Result<AccessOverlay> {
    let properties = feature
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let access = prop_str(&properties, "access")
        .or_else(|| prop_str(&properties, "status"))
        .map_or(Access::Closed, Access::from_tag);
    let name = prop_str(&properties, "name")
        .or_else(|| prop_str(&properties, "id"))
        .map_or_else(|| format!("overlay-{i}"), str::to_owned);
    let source = prop_str(&properties, "source")
        .unwrap_or("geojson-access-overlay")
        .to_owned();
    let provenance = Provenance {
        source,
        layer: prop_str(&properties, "layer").map(str::to_owned),
        source_id: Some(name.clone()),
        license: prop_str(&properties, "license").map(str::to_owned),
    };
    Ok(AccessOverlay {
        name,
        access,
        active: access_window_from_properties(&properties)?,
        confidence: prop_f64(&properties, "confidence")
            .unwrap_or(0.9)
            .clamp(0.0, 1.0),
        tolerance_m: prop_f64(&properties, "tolerance_m")
            .unwrap_or(20.0)
            .max(0.0),
        provenance,
        geometry: overlay_geometry(
            feature.get("geometry").ok_or_else(|| {
                TrailgenError::InvalidData("GeoJSON overlay feature has no geometry".to_owned())
            })?,
            crs,
        )?,
    })
}

fn access_window_from_properties(properties: &Map<String, Value>) -> Result<AccessWindow> {
    Ok(AccessWindow {
        from: prop_date(
            properties,
            &["active_from", "start_date", "starts_on", "from"],
        )?,
        to: prop_date(properties, &["active_to", "end_date", "ends_on", "to"])?,
    })
}

fn terrain_overlay_from_feature(
    feature: &Value,
    i: usize,
    crs: CoordProjector,
) -> Result<TerrainOverlay> {
    let properties = feature
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let terrain = prop_str(&properties, "terrain")
        .or_else(|| prop_str(&properties, "surface"))
        .or_else(|| prop_str(&properties, "landcover"))
        .or_else(|| prop_str(&properties, "land_cover"))
        .map_or(Terrain::Unknown, Terrain::from_tag);
    if terrain == Terrain::Unknown {
        return Err(TrailgenError::InvalidData(
            "terrain overlay feature has no recognized terrain/surface/landcover tag".to_owned(),
        ));
    }
    let name = prop_str(&properties, "name")
        .or_else(|| prop_str(&properties, "id"))
        .map_or_else(|| format!("terrain-{i}"), str::to_owned);
    let source = prop_str(&properties, "source")
        .unwrap_or("geojson-terrain-overlay")
        .to_owned();
    let provenance = Provenance {
        source,
        layer: prop_str(&properties, "layer").map(str::to_owned),
        source_id: Some(name.clone()),
        license: prop_str(&properties, "license").map(str::to_owned),
    };
    Ok(TerrainOverlay {
        name,
        terrain,
        surface: prop_str(&properties, "surface").map(str::to_owned),
        confidence: prop_f64(&properties, "confidence")
            .unwrap_or(0.75)
            .clamp(0.0, 1.0),
        tolerance_m: prop_f64(&properties, "tolerance_m")
            .unwrap_or(20.0)
            .max(0.0),
        provenance,
        geometry: overlay_geometry(
            feature.get("geometry").ok_or_else(|| {
                TrailgenError::InvalidData(
                    "GeoJSON terrain overlay feature has no geometry".to_owned(),
                )
            })?,
            crs,
        )?,
    })
}

fn overlay_geometry(geometry: &Value, crs: CoordProjector) -> Result<OverlayGeometry> {
    match geometry.get("type").and_then(Value::as_str) {
        Some("Polygon") => {
            let rings = geometry
                .get("coordinates")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    TrailgenError::InvalidGeometry("Polygon lacks coordinates".to_owned())
                })?;
            polygon(coords(
                rings.first().ok_or_else(|| {
                    TrailgenError::InvalidGeometry("Polygon lacks exterior ring".to_owned())
                })?,
                crs,
            )?)
        }
        Some("MultiPolygon") => {
            let rings = geometry
                .get("coordinates")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    TrailgenError::InvalidGeometry("MultiPolygon lacks coordinates".to_owned())
                })?
                .iter()
                .map(|poly| {
                    let rings = poly.as_array().ok_or_else(|| {
                        TrailgenError::InvalidGeometry("MultiPolygon member expected".to_owned())
                    })?;
                    coords(
                        rings.first().ok_or_else(|| {
                            TrailgenError::InvalidGeometry(
                                "MultiPolygon member lacks exterior ring".to_owned(),
                            )
                        })?,
                        crs,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(OverlayGeometry::MultiPolygon(rings))
        }
        Some("LineString") => lines_from_geometry(geometry, crs)?
            .into_iter()
            .next()
            .map(OverlayGeometry::Line)
            .ok_or_else(|| TrailgenError::InvalidGeometry("empty LineString".to_owned())),
        Some("MultiLineString") => {
            let lines = lines_from_geometry(geometry, crs)?;
            if lines.is_empty() {
                Err(TrailgenError::InvalidGeometry(
                    "empty MultiLineString".to_owned(),
                ))
            } else {
                Ok(OverlayGeometry::MultiLine(lines))
            }
        }
        Some(other) => Err(TrailgenError::UnsupportedFormat(format!(
            "GeoJSON overlay geometry {other}"
        ))),
        None => Err(TrailgenError::InvalidGeometry(
            "GeoJSON overlay geometry lacks type".to_owned(),
        )),
    }
}

fn context_from_feature(
    feature: &Value,
    i: usize,
    crs: CoordProjector,
) -> Result<Vec<ContextOverlay>> {
    let properties = feature
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let Some(kind) = prop_str(&properties, "kind")
        .or_else(|| prop_str(&properties, "context"))
        .or_else(|| prop_str(&properties, "type"))
        .and_then(CrossingKind::from_tag)
    else {
        return Ok(Vec::new());
    };
    let name = prop_str(&properties, "name")
        .or_else(|| prop_str(&properties, "id"))
        .map_or_else(|| format!("context-{i}"), str::to_owned);
    let source = prop_str(&properties, "source")
        .unwrap_or(match kind {
            CrossingKind::Road => "geojson-road-context",
            CrossingKind::Water => "geojson-hydrology-context",
        })
        .to_owned();
    let provenance = Provenance {
        source,
        layer: prop_str(&properties, "layer").map(str::to_owned),
        source_id: Some(name.clone()),
        license: prop_str(&properties, "license").map(str::to_owned),
    };
    let geometry = feature.get("geometry").ok_or_else(|| {
        TrailgenError::InvalidData("GeoJSON context feature has no geometry".to_owned())
    })?;
    let lines = lines_from_geometry(geometry, crs)?;
    let multi = lines.len() > 1;
    let confidence = prop_f64(&properties, "confidence")
        .unwrap_or(0.8)
        .clamp(0.0, 1.0);
    Ok(lines
        .into_iter()
        .enumerate()
        .map(|(j, geometry)| ContextOverlay {
            name: if multi {
                format!("{name}#{j}")
            } else {
                name.clone()
            },
            kind,
            confidence,
            provenance: provenance.clone(),
            geometry,
        })
        .collect())
}

fn coords(value: &Value, crs: CoordProjector) -> Result<Vec<Coord>> {
    value
        .as_array()
        .ok_or_else(|| TrailgenError::InvalidGeometry("coordinate array expected".to_owned()))?
        .iter()
        .map(|v| {
            let xs = v.as_array().ok_or_else(|| {
                TrailgenError::InvalidGeometry("position array expected".to_owned())
            })?;
            let lon = xs
                .first()
                .and_then(Value::as_f64)
                .ok_or_else(|| TrailgenError::InvalidGeometry("longitude missing".to_owned()))?;
            let lat = xs
                .get(1)
                .and_then(Value::as_f64)
                .ok_or_else(|| TrailgenError::InvalidGeometry("latitude missing".to_owned()))?;
            let ele = xs.get(2).and_then(Value::as_f64);
            Ok(crs.project(lon, lat, ele))
        })
        .collect()
}

fn line_geometry(line: &LineString) -> Value {
    json!({
        "type": "LineString",
        "coordinates": line.points.iter().map(|c| {
            c.ele
                .map_or_else(|| json!([c.lon, c.lat]), |ele| json!([c.lon, c.lat, ele]))
        }).collect::<Vec<_>>()
    })
}

fn prop_str<'a>(props: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    props
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn prop_str_any<'a>(props: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| prop_str(props, key))
}

fn prop_f64(props: &Map<String, Value>, key: &str) -> Option<f64> {
    props.get(key).and_then(Value::as_f64)
}

fn prop_bool(props: &Map<String, Value>, key: &str) -> Option<bool> {
    props.get(key).and_then(Value::as_bool)
}

fn prop_date(props: &Map<String, Value>, keys: &[&str]) -> Result<Option<PlanningDate>> {
    keys.iter()
        .find_map(|key| prop_str(props, key))
        .map(str::parse::<PlanningDate>)
        .transpose()
        .map_err(TrailgenError::InvalidData)
}

fn travel_from_properties(props: &Map<String, Value>) -> EdgeTravel {
    prop_str(props, "travel")
        .or_else(|| prop_str(props, "travel_direction"))
        .or_else(|| prop_str(props, "direction"))
        .or_else(|| prop_str(props, "oneway"))
        .or_else(|| prop_str(props, "one_way"))
        .map(EdgeTravel::from_tag)
        .or_else(|| {
            prop_bool(props, "oneway")
                .or_else(|| prop_bool(props, "one_way"))
                .map(|oneway| {
                    if oneway {
                        EdgeTravel::Forward
                    } else {
                        EdgeTravel::Both
                    }
                })
        })
        .unwrap_or_default()
}
