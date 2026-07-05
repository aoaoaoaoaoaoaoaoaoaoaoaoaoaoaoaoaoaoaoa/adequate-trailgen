use crate::builder::SegmentDraft;
use crate::geo::{Coord, LineString};
use crate::model::{Access, CrossingKind, Provenance, Terrain, TrailGraph};
use crate::overlay::{AccessOverlay, ContextOverlay, OverlayGeometry, TerrainOverlay, polygon};
use crate::route::Route;
use crate::{Result, TrailgenError};
use serde_json::{Map, Value, json};

pub fn network_from_str(s: &str) -> Result<Vec<SegmentDraft>> {
    let root: Value = serde_json::from_str(s)?;
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
        for line in lines_from_geometry(geometry)? {
            drafts.push(SegmentDraft {
                geometry: line,
                terrain,
                surface: surface.clone(),
                access,
                road_exposure,
                confidence,
                provenance: provenance.clone(),
            });
        }
    }
    Ok(drafts)
}

pub fn route_line_from_str(s: &str) -> Result<LineString> {
    let root: Value = serde_json::from_str(s)?;
    if root.get("type").and_then(Value::as_str) == Some("FeatureCollection") {
        let features = root
            .get("features")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                TrailgenError::InvalidData("GeoJSON FeatureCollection expected".to_owned())
            })?;
        for feature in features {
            if let Some(geometry) = feature.get("geometry")
                && let Some(line) = lines_from_geometry(geometry)?.into_iter().next()
            {
                return Ok(line);
            }
        }
    }
    if root.get("type").and_then(Value::as_str) == Some("Feature") {
        return lines_from_geometry(root.get("geometry").ok_or_else(|| {
            TrailgenError::InvalidData("GeoJSON feature has no geometry".to_owned())
        })?)?
        .into_iter()
        .next()
        .ok_or_else(|| TrailgenError::InvalidGeometry("no LineString in GeoJSON".to_owned()));
    }
    lines_from_geometry(&root)?
        .into_iter()
        .next()
        .ok_or_else(|| TrailgenError::InvalidGeometry("no LineString in GeoJSON".to_owned()))
}

pub fn access_overlays_from_str(s: &str) -> Result<Vec<AccessOverlay>> {
    let root: Value = serde_json::from_str(s)?;
    let features = root
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            TrailgenError::InvalidData("GeoJSON FeatureCollection expected".to_owned())
        })?;
    features
        .iter()
        .enumerate()
        .map(|(i, feature)| overlay_from_feature(feature, i))
        .collect()
}

pub fn terrain_overlays_from_str(s: &str) -> Result<Vec<TerrainOverlay>> {
    let root: Value = serde_json::from_str(s)?;
    let features = root
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            TrailgenError::InvalidData("GeoJSON FeatureCollection expected".to_owned())
        })?;
    features
        .iter()
        .enumerate()
        .map(|(i, feature)| terrain_overlay_from_feature(feature, i))
        .collect()
}

pub fn context_overlays_from_str(s: &str) -> Result<Vec<ContextOverlay>> {
    let root: Value = serde_json::from_str(s)?;
    let features = root
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            TrailgenError::InvalidData("GeoJSON FeatureCollection expected".to_owned())
        })?;
    Ok(features
        .iter()
        .enumerate()
        .map(|(i, feature)| context_from_feature(feature, i))
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
            "score": route.score,
            "pareto_rank": route.pareto_rank,
            "distance_m": route.metrics.distance_m,
            "ascent_m": route.metrics.ascent_m,
            "descent_m": route.metrics.descent_m,
            "shape": route.metrics.shape,
            "difficulty": route.metrics.difficulty,
            "difficulty_breakdown": route.metrics.difficulty_breakdown,
            "road_fraction": route.metrics.road_fraction,
            "low_confidence_fraction": route.metrics.low_confidence_fraction,
            "repeated_edge_fraction": route.metrics.repeated_edge_fraction,
            "crossings": route.metrics.crossings,
            "satisfied": route.verdict.satisfied,
            "violations": route.verdict.violations,
            "edges": route.edges.iter().map(|id| id.0).collect::<Vec<_>>(),
        },
        "geometry": line_geometry(&route.geometry(graph)),
    })
}

fn lines_from_geometry(geometry: &Value) -> Result<Vec<LineString>> {
    match geometry.get("type").and_then(Value::as_str) {
        Some("LineString") => Ok(vec![LineString::new(coords(
            geometry.get("coordinates").ok_or_else(|| {
                TrailgenError::InvalidGeometry("LineString lacks coordinates".to_owned())
            })?,
        )?)?]),
        Some("MultiLineString") => geometry
            .get("coordinates")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                TrailgenError::InvalidGeometry("MultiLineString lacks coordinates".to_owned())
            })?
            .iter()
            .map(|xs| LineString::new(coords(xs)?))
            .collect(),
        Some(other) => Err(TrailgenError::UnsupportedFormat(format!(
            "GeoJSON geometry {other}"
        ))),
        None => Err(TrailgenError::InvalidGeometry(
            "GeoJSON geometry lacks type".to_owned(),
        )),
    }
}

fn overlay_from_feature(feature: &Value, i: usize) -> Result<AccessOverlay> {
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
        confidence: prop_f64(&properties, "confidence")
            .unwrap_or(0.9)
            .clamp(0.0, 1.0),
        tolerance_m: prop_f64(&properties, "tolerance_m")
            .unwrap_or(20.0)
            .max(0.0),
        provenance,
        geometry: overlay_geometry(feature.get("geometry").ok_or_else(|| {
            TrailgenError::InvalidData("GeoJSON overlay feature has no geometry".to_owned())
        })?)?,
    })
}

fn terrain_overlay_from_feature(feature: &Value, i: usize) -> Result<TerrainOverlay> {
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
        geometry: overlay_geometry(feature.get("geometry").ok_or_else(|| {
            TrailgenError::InvalidData("GeoJSON terrain overlay feature has no geometry".to_owned())
        })?)?,
    })
}

fn overlay_geometry(geometry: &Value) -> Result<OverlayGeometry> {
    match geometry.get("type").and_then(Value::as_str) {
        Some("Polygon") => {
            let rings = geometry
                .get("coordinates")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    TrailgenError::InvalidGeometry("Polygon lacks coordinates".to_owned())
                })?;
            polygon(coords(rings.first().ok_or_else(|| {
                TrailgenError::InvalidGeometry("Polygon lacks exterior ring".to_owned())
            })?)?)
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
                    coords(rings.first().ok_or_else(|| {
                        TrailgenError::InvalidGeometry(
                            "MultiPolygon member lacks exterior ring".to_owned(),
                        )
                    })?)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(OverlayGeometry::MultiPolygon(rings))
        }
        Some("LineString") => lines_from_geometry(geometry)?
            .into_iter()
            .next()
            .map(OverlayGeometry::Line)
            .ok_or_else(|| TrailgenError::InvalidGeometry("empty LineString".to_owned())),
        Some(other) => Err(TrailgenError::UnsupportedFormat(format!(
            "GeoJSON overlay geometry {other}"
        ))),
        None => Err(TrailgenError::InvalidGeometry(
            "GeoJSON overlay geometry lacks type".to_owned(),
        )),
    }
}

fn context_from_feature(feature: &Value, i: usize) -> Result<Option<ContextOverlay>> {
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
        return Ok(None);
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
    let lines = lines_from_geometry(geometry)?;
    let line = LineString::new(
        lines
            .into_iter()
            .flat_map(|line| line.points)
            .collect::<Vec<_>>(),
    )?;
    Ok(Some(ContextOverlay {
        name,
        kind,
        confidence: prop_f64(&properties, "confidence")
            .unwrap_or(0.8)
            .clamp(0.0, 1.0),
        provenance,
        geometry: line,
    }))
}

fn coords(value: &Value) -> Result<Vec<Coord>> {
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
            Ok(Coord { lon, lat, ele })
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
    props.get(key).and_then(Value::as_str)
}

fn prop_f64(props: &Map<String, Value>, key: &str) -> Option<f64> {
    props.get(key).and_then(Value::as_f64)
}

fn prop_bool(props: &Map<String, Value>, key: &str) -> Option<bool> {
    props.get(key).and_then(Value::as_bool)
}
