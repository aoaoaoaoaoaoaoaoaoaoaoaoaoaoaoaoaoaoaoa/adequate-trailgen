use crate::geo::{Coord, LineString};
use crate::{Result, TrailgenError};
use serde_json::{Map, Value};

pub fn route_line_from_str(s: &str) -> Result<LineString> {
    let root: Value = serde_json::from_str(s)?;
    let points = route_points(&root).ok_or_else(|| {
        TrailgenError::InvalidData(
            "route JSON must contain coordinates, points, track, or route arrays".to_owned(),
        )
    })?;
    LineString::new(points)
}

fn route_points(value: &Value) -> Option<Vec<Coord>> {
    match value {
        Value::Array(xs) => points_from_array(xs),
        Value::Object(obj) => points_from_object_container(obj),
        _ => None,
    }
}

fn points_from_object_container(obj: &Map<String, Value>) -> Option<Vec<Coord>> {
    for key in [
        "coordinates",
        "points",
        "track",
        "tracks",
        "route",
        "routes",
        "locations",
        "path",
        "data",
        "geometry",
    ] {
        if let Some(points) = obj.get(key).and_then(route_points) {
            return Some(points);
        }
    }
    None
}

fn points_from_array(xs: &[Value]) -> Option<Vec<Coord>> {
    if xs.iter().all(point_like) {
        return xs.iter().map(point_from_value).collect();
    }
    xs.iter().find_map(route_points)
}

fn point_like(value: &Value) -> bool {
    match value {
        Value::Array(xs) => {
            xs.len() >= 2 && value_f64(&xs[0]).is_some() && value_f64(&xs[1]).is_some()
        }
        Value::Object(obj) => {
            keyed_f64(obj, LON_KEYS).is_some() && keyed_f64(obj, LAT_KEYS).is_some()
        }
        _ => false,
    }
}

fn point_from_value(value: &Value) -> Option<Coord> {
    match value {
        Value::Array(xs) => coord(
            value_f64(xs.first()?)?,
            value_f64(xs.get(1)?)?,
            xs.get(2).and_then(value_f64),
        ),
        Value::Object(obj) => coord(
            keyed_f64(obj, LON_KEYS)?,
            keyed_f64(obj, LAT_KEYS)?,
            keyed_f64(obj, ELE_KEYS),
        ),
        _ => None,
    }
}

fn coord(lon: f64, lat: f64, ele: Option<f64>) -> Option<Coord> {
    if !lon.is_finite() || !lat.is_finite() || lon.abs() > 180.0 || lat.abs() > 90.0 {
        return None;
    }
    Some(Coord { lon, lat, ele })
}

const LON_KEYS: &[&str] = &["lon", "lng", "longitude", "x"];
const LAT_KEYS: &[&str] = &["lat", "latitude", "y"];
const ELE_KEYS: &[&str] = &[
    "ele",
    "elevation",
    "elevation_m",
    "alt",
    "altitude",
    "altitude_m",
    "z",
];

fn keyed_f64(obj: &Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| obj.get(*key).and_then(value_f64))
}

fn value_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}
