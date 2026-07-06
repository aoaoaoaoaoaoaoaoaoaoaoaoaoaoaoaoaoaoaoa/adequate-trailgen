use crate::geo::Coord;
use crate::{Result, TrailgenError};

const WEB_MERCATOR_R_M: f64 = 6_378_137.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrsVerdict {
    AssumedGeographic,
    Wgs84,
    WebMercator,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordProjector {
    Identity,
    WebMercator,
}

impl CoordProjector {
    #[must_use]
    pub fn project(self, x: f64, y: f64, ele: Option<f64>) -> Coord {
        match self {
            Self::Identity => Coord {
                lon: x,
                lat: y,
                ele,
            },
            Self::WebMercator => web_mercator_to_wgs84(x, y, ele),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorCrsKind {
    GeoJson,
    ShapefilePrj,
}

impl VectorCrsKind {
    const fn label(self) -> &'static str {
        match self {
            Self::GeoJson => "GeoJSON CRS",
            Self::ShapefilePrj => "shapefile .prj CRS",
        }
    }
}

pub fn validate_crs_name(kind: VectorCrsKind, name: &str) -> Result<CrsVerdict> {
    let normalized = normalize(name);
    if is_wgs84(&normalized) {
        Ok(CrsVerdict::Wgs84)
    } else if is_web_mercator(&normalized) {
        Ok(CrsVerdict::WebMercator)
    } else {
        Err(TrailgenError::InvalidData(format!(
            "{} {name:?} is not supported; reproject input to geographic lon/lat WGS84 (EPSG:4326/CRS84), or EPSG:3857 Web Mercator, before ingestion",
            kind.label()
        )))
    }
}

pub fn validate_prj_wkt(wkt: &str) -> Result<CrsVerdict> {
    let normalized = normalize(wkt);
    if is_web_mercator(&normalized) {
        return Ok(CrsVerdict::WebMercator);
    }
    if is_projected(&normalized) {
        return Err(TrailgenError::InvalidData(
            "shapefile .prj advertises an unsupported projected CRS; reproject input to geographic lon/lat WGS84 (EPSG:4326/CRS84), or EPSG:3857 Web Mercator, before ingestion"
                .to_owned(),
        ));
    }
    validate_crs_name(VectorCrsKind::ShapefilePrj, wkt)
}

#[must_use]
pub const fn projector(verdict: CrsVerdict) -> CoordProjector {
    match verdict {
        CrsVerdict::AssumedGeographic | CrsVerdict::Wgs84 => CoordProjector::Identity,
        CrsVerdict::WebMercator => CoordProjector::WebMercator,
    }
}

fn normalize(raw: &str) -> String {
    raw.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_uppercase)
        .collect()
}

fn is_wgs84(normalized: &str) -> bool {
    normalized.contains("EPSG4326")
        || normalized.contains("OGC13CRS84")
        || normalized.contains("OGC14CRS84")
        || normalized.contains("CRS84")
        || normalized.contains("WGS84")
        || normalized.contains("WGS1984")
}

fn is_web_mercator(normalized: &str) -> bool {
    normalized.contains("EPSG3857")
        || normalized.contains("EPSG900913")
        || normalized.contains("WEBMERCATOR")
        || normalized.contains("PSEUDOMERCATOR")
        || normalized.contains("WGS84PSEUDOMERCATOR")
}

fn is_projected(normalized: &str) -> bool {
    normalized.contains("PROJCS")
        || normalized.contains("PROJCRS")
        || normalized.contains("PROJECTION")
}

fn web_mercator_to_wgs84(x_m: f64, y_m: f64, ele: Option<f64>) -> Coord {
    let lon = (x_m / WEB_MERCATOR_R_M).to_degrees();
    let lat = 2.0f64
        .mul_add(
            (y_m / WEB_MERCATOR_R_M).exp().atan(),
            -std::f64::consts::FRAC_PI_2,
        )
        .to_degrees();
    Coord { lon, lat, ele }
}
