use crate::builder::{JunctionPolicy, SegmentDraft};
use crate::crs::{CoordProjector, CrsVerdict, VectorCrsKind, projector, validate_crs_name};
use crate::geo::{Coord, LineString};
use crate::io::route_file::{RouteFile, RouteFileMetadata};
use crate::model::{
    Access, CrossingControl, CrossingKind, EdgeTravel, GeometryClaim, Provenance, Terrain,
    TrailMarking, TrailStanding, WalkGraph, WayKind, WayRealm,
};
use crate::overlay::{
    AccessOverlay, AccessWindow, ContextOverlay, DailyTimeWindow, MonthDay, OverlayGeometry,
    PlanningDate, PlanningTime, SeasonalWindow, TerrainOverlay, WeekdaySet, polygon,
};
use crate::{Result, TrailgenError};
use serde_json::{Map, Value, json};

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
        let terrain_tag = prop_str(&properties, "terrain");
        let surface_terrain = surface
            .as_deref()
            .map_or(Terrain::Unknown, Terrain::from_tag);
        let terrain = terrain_tag
            .map(Terrain::from_tag)
            .filter(|terrain| *terrain != Terrain::Unknown)
            .unwrap_or(surface_terrain);
        let terrain_confidence = terrain_confidence_from_properties(
            &properties,
            terrain,
            terrain_tag.is_some(),
            surface_terrain != Terrain::Unknown,
        );
        let way_kind = prop_str(&properties, "way_kind")
            .or_else(|| prop_str(&properties, "highway"))
            .map_or(WayKind::Unknown, WayKind::from_tag);
        let realm =
            prop_str(&properties, "realm").map_or(WayRealm::Recreational, WayRealm::from_tag);
        let geometry_claim = prop_str(&properties, "geometry_claim")
            .map_or(GeometryClaim::Surveyed, GeometryClaim::from_tag);
        let crossing_control = prop_str(&properties, "crossing_control")
            .map_or(CrossingControl::None, CrossingControl::from_tag);
        let standing = prop_str(&properties, "trail_standing")
            .or_else(|| prop_str(&properties, "standing"))
            .map_or(TrailStanding::Unknown, TrailStanding::from_tag);
        let marking = prop_str(&properties, "trail_marking")
            .or_else(|| prop_str(&properties, "marking"))
            .or_else(|| prop_str(&properties, "trailblazed"))
            .or_else(|| prop_str(&properties, "asset"))
            .map_or(TrailMarking::Unknown, TrailMarking::from_tag);
        let access = prop_str(&properties, "access").map_or(Access::Unknown, Access::from_tag);
        let travel = travel_from_properties(&properties);
        let confidence = prop_f64(&properties, "confidence").unwrap_or(0.75);
        let road_exposure = prop_f64(&properties, "road_exposure")
            .or_else(|| prop_bool(&properties, "road").map(f64::from))
            .unwrap_or_else(|| f64::from(terrain == Terrain::Road));
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
                junctions: JunctionPolicy::default(),
                turn_ref: None,
                junction_keys: None,
                turn_restrictions: Vec::new(),
                geometry: line,
                way_kind,
                realm,
                geometry_claim,
                crossing_control,
                standing,
                marking,
                terrain,
                terrain_confidence: Some(terrain_confidence),
                surface: surface.clone(),
                access,
                travel,
                road_exposure,
                confidence,
                provenance: vec![provenance.clone()],
            });
        }
    }
    Ok(drafts)
}

fn terrain_confidence_from_properties(
    properties: &Map<String, Value>,
    terrain: Terrain,
    explicit_terrain: bool,
    explicit_surface: bool,
) -> f64 {
    prop_f64(properties, "terrain_confidence").map_or_else(
        || {
            if terrain == Terrain::Unknown {
                0.0
            } else if explicit_terrain {
                0.90
            } else if explicit_surface {
                0.82
            } else {
                0.45
            }
        },
        |confidence| confidence.clamp(0.0, 1.0),
    )
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
pub fn graph_to_geojson(graph: &WalkGraph) -> Value {
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
                ("hill_slope_deg".to_owned(), json!(edge.attr.hill_slope_deg)),
                ("way_kind".to_owned(), json!(edge.attr.way_kind)),
                ("realm".to_owned(), json!(edge.attr.realm)),
                ("geometry_claim".to_owned(), json!(edge.attr.geometry_claim)),
                ("crossing_control".to_owned(), json!(edge.attr.crossing_control)),
                ("trail_standing".to_owned(), json!(edge.attr.standing)),
                ("trail_marking".to_owned(), json!(edge.attr.marking)),
                ("traversal".to_owned(), json!(edge.attr.traversal)),
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
                "GeoJSON CRS must be a named WGS84/NAD83/CRS84, EPSG:3857, or WGS84/NAD83 UTM object; linked or opaque CRS definitions are unsupported"
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
    let access = access_from_properties(&properties);
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
        travel: travel_override_from_properties(&properties),
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
        seasonal: seasonal_window_from_properties(properties)?,
        weekdays: prop_weekdays(
            properties,
            &[
                "weekdays",
                "weekday",
                "days",
                "day_of_week",
                "active_weekdays",
                "active_days",
            ],
        )?,
        time: time_window_from_properties(properties)?,
    })
}

fn seasonal_window_from_properties(
    properties: &Map<String, Value>,
) -> Result<Option<SeasonalWindow>> {
    let from = prop_month_day(
        properties,
        &[
            "seasonal_from",
            "active_month_from",
            "season_from",
            "month_from",
            "recurs_from",
        ],
    )?;
    let to = prop_month_day(
        properties,
        &[
            "seasonal_to",
            "active_month_to",
            "season_to",
            "month_to",
            "recurs_to",
        ],
    )?;
    match (from, to) {
        (None, None) => Ok(None),
        (Some(from), Some(to)) => Ok(Some(SeasonalWindow::new(from, to))),
        _ => Err(TrailgenError::InvalidData(
            "recurring seasonal access windows require both from and to month-days".to_owned(),
        )),
    }
}

fn time_window_from_properties(properties: &Map<String, Value>) -> Result<Option<DailyTimeWindow>> {
    let from = prop_time(
        properties,
        &[
            "time_from",
            "active_time_from",
            "start_time",
            "starts_at",
            "hour_from",
            "hours_from",
        ],
    )?;
    let to = prop_time(
        properties,
        &[
            "time_to",
            "active_time_to",
            "end_time",
            "ends_at",
            "hour_to",
            "hours_to",
        ],
    )?;
    match (from, to) {
        (None, None) => Ok(None),
        (Some(from), Some(to)) => Ok(Some(DailyTimeWindow::new(from, to))),
        _ => Err(TrailgenError::InvalidData(
            "hourly access windows require both from and to times".to_owned(),
        )),
    }
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
    let terrain = terrain_from_properties(&properties);
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
    props.get(key).and_then(|value| {
        value
            .as_bool()
            .or_else(|| {
                value
                    .as_str()
                    .and_then(|raw| match raw.trim().to_ascii_lowercase().as_str() {
                        "1" | "true" | "t" | "yes" | "y" | "required" | "permit"
                        | "reservation" => Some(true),
                        "0" | "false" | "f" | "no" | "n" | "none" | "not-required" => Some(false),
                        _ => None,
                    })
            })
            .or_else(|| value.as_f64().map(|raw| raw != 0.0))
    })
}

fn access_from_properties(properties: &Map<String, Value>) -> Access {
    prop_str(properties, "access")
        .or_else(|| prop_str(properties, "status"))
        .map(Access::from_tag)
        .or_else(|| {
            permit_required(
                properties,
                &[
                    "permit_required",
                    "requires_permit",
                    "permit",
                    "permits",
                    "reservation_required",
                    "requires_reservation",
                    "reservation",
                    "reservations",
                    "timed_entry_required",
                    "timed_entry",
                    "quota_required",
                ],
            )
            .map(|required| {
                if required {
                    Access::Restricted
                } else {
                    Access::Open
                }
            })
        })
        .unwrap_or(Access::Closed)
}

fn permit_required(properties: &Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| prop_bool(properties, key))
}

fn terrain_from_properties(props: &Map<String, Value>) -> Terrain {
    let direct = ["terrain", "surface"].into_iter().find_map(|key| {
        prop_str(props, key)
            .map(Terrain::from_tag)
            .filter(|terrain| *terrain != Terrain::Unknown)
    });
    direct.unwrap_or_else(|| {
        [
            "landcover",
            "land_cover",
            "landcover_class",
            "land_cover_class",
            "nlcd",
            "nlcd_code",
            "gridcode",
            "class",
            "class_name",
            "cover",
            "cover_type",
        ]
        .into_iter()
        .find_map(|key| props.get(key).and_then(terrain_from_landcover_value))
        .unwrap_or(Terrain::Unknown)
    })
}

fn terrain_from_landcover_value(value: &Value) -> Option<Terrain> {
    let terrain = value.as_str().map_or_else(
        || Terrain::from_landcover_tag(&value.to_string()),
        Terrain::from_landcover_tag,
    );
    (terrain != Terrain::Unknown).then_some(terrain)
}

fn prop_date(props: &Map<String, Value>, keys: &[&str]) -> Result<Option<PlanningDate>> {
    keys.iter()
        .find_map(|key| prop_str(props, key))
        .map(str::parse::<PlanningDate>)
        .transpose()
        .map_err(TrailgenError::InvalidData)
}

fn prop_month_day(props: &Map<String, Value>, keys: &[&str]) -> Result<Option<MonthDay>> {
    keys.iter()
        .find_map(|key| prop_str(props, key))
        .map(str::parse::<MonthDay>)
        .transpose()
        .map_err(TrailgenError::InvalidData)
}

fn prop_time(props: &Map<String, Value>, keys: &[&str]) -> Result<Option<PlanningTime>> {
    keys.iter()
        .find_map(|key| prop_str(props, key))
        .map(str::parse::<PlanningTime>)
        .transpose()
        .map_err(TrailgenError::InvalidData)
}

fn prop_weekdays(props: &Map<String, Value>, keys: &[&str]) -> Result<WeekdaySet> {
    keys.iter()
        .find_map(|key| props.get(*key))
        .map_or_else(|| Ok(WeekdaySet::empty()), weekdays_from_value)
}

fn weekdays_from_value(value: &Value) -> Result<WeekdaySet> {
    match value {
        Value::String(raw) => raw
            .parse::<WeekdaySet>()
            .map_err(TrailgenError::InvalidData),
        Value::Array(values) => {
            let mut set = WeekdaySet::empty();
            for value in values {
                let raw = value.as_str().ok_or_else(|| {
                    TrailgenError::InvalidData(
                        "weekday arrays must contain only strings".to_owned(),
                    )
                })?;
                set = set.union(
                    raw.parse::<WeekdaySet>()
                        .map_err(TrailgenError::InvalidData)?,
                );
            }
            Ok(set)
        }
        Value::Null => Ok(WeekdaySet::empty()),
        _ => Err(TrailgenError::InvalidData(
            "weekday fields must be strings or string arrays".to_owned(),
        )),
    }
}

fn travel_override_from_properties(props: &Map<String, Value>) -> Option<EdgeTravel> {
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
}

fn travel_from_properties(props: &Map<String, Value>) -> EdgeTravel {
    travel_override_from_properties(props).unwrap_or_default()
}
