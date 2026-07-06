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

#[must_use]
pub(crate) fn wgs84_to_web_mercator(coord: Coord) -> (f64, f64) {
    let lat = coord.lat.clamp(-85.051_128_78, 85.051_128_78).to_radians();
    (
        WEB_MERCATOR_R_M * coord.lon.to_radians(),
        WEB_MERCATOR_R_M * (std::f64::consts::FRAC_PI_4 + lat / 2.0).tan().ln(),
    )
}

#[must_use]
#[allow(clippy::many_single_char_names, clippy::suboptimal_flops)]
pub fn wgs84_to_utm(coord: Coord, zone: u8, north: bool) -> Option<(f64, f64)> {
    if !(1..=60).contains(&zone) || !coord.lon.is_finite() || !coord.lat.is_finite() {
        return None;
    }
    let φ = coord.lat.to_radians();
    let λ = coord.lon.to_radians();
    let λ0 = ((f64::from(zone) - 1.0).mul_add(6.0, -177.0)).to_radians();
    let a = 6_378_137.0;
    let f = 1.0 / 298.257_223_563;
    let k0 = 0.9996;
    let e2 = f * (2.0 - f);
    let e4 = e2 * e2;
    let e6 = e4 * e2;
    let ep2 = e2 / (1.0 - e2);
    let sinφ = φ.sin();
    let cosφ = φ.cos();
    let tanφ = φ.tan();
    let n = a / (1.0 - e2 * sinφ * sinφ).sqrt();
    let t = tanφ * tanφ;
    let c = ep2 * cosφ * cosφ;
    let aa = cosφ * (λ - λ0);
    let m = a
        * ((1.0 - e2 / 4.0 - 3.0 * e4 / 64.0 - 5.0 * e6 / 256.0) * φ
            - (3.0 * e2 / 8.0 + 3.0 * e4 / 32.0 + 45.0 * e6 / 1024.0) * (2.0 * φ).sin()
            + (15.0 * e4 / 256.0 + 45.0 * e6 / 1024.0) * (4.0 * φ).sin()
            - (35.0 * e6 / 3072.0) * (6.0 * φ).sin());
    let easting = 500_000.0
        + k0 * n
            * (aa
                + (1.0 - t + c) * aa.powi(3) / 6.0
                + (5.0 - 18.0 * t + t * t + 72.0 * c - 58.0 * ep2) * aa.powi(5) / 120.0);
    let mut northing = k0
        * (m + n
            * tanφ
            * (aa.powi(2) / 2.0
                + (5.0 - t + 9.0 * c + 4.0 * c * c) * aa.powi(4) / 24.0
                + (61.0 - 58.0 * t + t * t + 600.0 * c - 330.0 * ep2) * aa.powi(6) / 720.0));
    if !north {
        northing += 10_000_000.0;
    }
    (easting.is_finite() && northing.is_finite()).then_some((easting, northing))
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
