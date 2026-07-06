use crate::geo::Coord;
use crate::{Result, TrailgenError};
use serde::{Deserialize, Serialize};

const WEB_MERCATOR_R_M: f64 = 6_378_137.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrsVerdict {
    AssumedGeographic,
    Wgs84,
    WebMercator,
    Wgs84Utm(UtmCrs),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordProjector {
    Identity,
    WebMercator,
    Wgs84Utm(UtmCrs),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UtmHemisphere {
    North,
    South,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UtmCrs {
    pub zone: u8,
    pub hemisphere: UtmHemisphere,
}

impl UtmCrs {
    #[must_use]
    pub const fn from_parts(zone: u8, hemisphere: UtmHemisphere) -> Option<Self> {
        if zone >= 1 && zone <= 60 {
            Some(Self { zone, hemisphere })
        } else {
            None
        }
    }

    #[must_use]
    pub fn from_epsg(epsg: u16) -> Option<Self> {
        match epsg {
            32601..=32660 => {
                Self::from_parts(u8::try_from(epsg - 32600).ok()?, UtmHemisphere::North)
            }
            32701..=32760 => {
                Self::from_parts(u8::try_from(epsg - 32700).ok()?, UtmHemisphere::South)
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn from_normalized_srs(normalized: &str) -> Option<Self> {
        (1_u16..=60).find_map(|zone| {
            let zone_u8 = u8::try_from(zone).ok()?;
            [
                (
                    format!("EPSG326{zone:02}"),
                    Self::from_parts(zone_u8, UtmHemisphere::North)?,
                ),
                (
                    format!("EPSG327{zone:02}"),
                    Self::from_parts(zone_u8, UtmHemisphere::South)?,
                ),
                (
                    format!("WGS84UTMZONE{zone}N"),
                    Self::from_parts(zone_u8, UtmHemisphere::North)?,
                ),
                (
                    format!("WGS84UTMZONE{zone}S"),
                    Self::from_parts(zone_u8, UtmHemisphere::South)?,
                ),
            ]
            .into_iter()
            .find_map(|(needle, crs)| normalized.contains(&needle).then_some(crs))
        })
    }

    const fn false_northing_m(self) -> f64 {
        match self.hemisphere {
            UtmHemisphere::North => 0.0,
            UtmHemisphere::South => 10_000_000.0,
        }
    }

    fn λ0(self) -> f64 {
        ((f64::from(self.zone) - 1.0).mul_add(6.0, -177.0)).to_radians()
    }
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
            Self::Wgs84Utm(crs) => utm_to_wgs84(x, y, ele, crs),
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
    let hemisphere = if north {
        UtmHemisphere::North
    } else {
        UtmHemisphere::South
    };
    wgs84_to_utm_crs(coord, UtmCrs::from_parts(zone, hemisphere)?)
}

#[must_use]
#[allow(clippy::many_single_char_names, clippy::suboptimal_flops)]
pub fn wgs84_to_utm_crs(coord: Coord, crs: UtmCrs) -> Option<(f64, f64)> {
    if !coord.lon.is_finite() || !coord.lat.is_finite() {
        return None;
    }
    let φ = coord.lat.to_radians();
    let λ = coord.lon.to_radians();
    let λ0 = crs.λ0();
    let a = 6_378_137.0;
    let f: f64 = 1.0 / 298.257_223_563;
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
    northing += crs.false_northing_m();
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
    match UtmCrs::from_normalized_srs(&normalized) {
        Some(crs) => Ok(CrsVerdict::Wgs84Utm(crs)),
        None if is_wgs84(&normalized) => Ok(CrsVerdict::Wgs84),
        None if is_web_mercator(&normalized) => Ok(CrsVerdict::WebMercator),
        None => Err(TrailgenError::InvalidData(format!(
            "{} {name:?} is not supported; reproject input to geographic lon/lat WGS84 (EPSG:4326/CRS84), EPSG:3857 Web Mercator, or WGS84 UTM (EPSG:326xx/327xx), before ingestion",
            kind.label()
        ))),
    }
}

pub fn validate_prj_wkt(wkt: &str) -> Result<CrsVerdict> {
    let normalized = normalize(wkt);
    if is_web_mercator(&normalized) {
        return Ok(CrsVerdict::WebMercator);
    }
    if let Some(crs) = UtmCrs::from_normalized_srs(&normalized) {
        return Ok(CrsVerdict::Wgs84Utm(crs));
    }
    if is_projected(&normalized) {
        return Err(TrailgenError::InvalidData(
            "shapefile .prj advertises an unsupported projected CRS; reproject input to geographic lon/lat WGS84 (EPSG:4326/CRS84), EPSG:3857 Web Mercator, or WGS84 UTM (EPSG:326xx/327xx), before ingestion"
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
        CrsVerdict::Wgs84Utm(crs) => CoordProjector::Wgs84Utm(crs),
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

#[allow(clippy::many_single_char_names, clippy::suboptimal_flops)]
fn utm_to_wgs84(easting_m: f64, northing_m: f64, ele: Option<f64>, crs: UtmCrs) -> Coord {
    let x = easting_m - 500_000.0;
    let y = northing_m - crs.false_northing_m();
    let a = 6_378_137.0;
    let f: f64 = 1.0 / 298.257_223_563;
    let k0 = 0.9996;
    let e2 = f * (2.0 - f);
    let e4 = e2 * e2;
    let e6 = e4 * e2;
    let ep2 = e2 / (1.0 - e2);
    let e1 = (1.0 - (1.0 - e2).sqrt()) / (1.0 + (1.0 - e2).sqrt());
    let e1_2 = e1 * e1;
    let e1_3 = e1_2 * e1;
    let e1_4 = e1_2 * e1_2;
    let μ = y / (k0 * a * (1.0 - e2 / 4.0 - 3.0 * e4 / 64.0 - 5.0 * e6 / 256.0));
    let φ1 = μ
        + (3.0 * e1 / 2.0 - 27.0 * e1_3 / 32.0) * (2.0 * μ).sin()
        + (21.0 * e1_2 / 16.0 - 55.0 * e1_4 / 32.0) * (4.0 * μ).sin()
        + (151.0 * e1_3 / 96.0) * (6.0 * μ).sin()
        + (1097.0 * e1_4 / 512.0) * (8.0 * μ).sin();
    let sinφ1 = φ1.sin();
    let cosφ1 = φ1.cos();
    let tanφ1 = φ1.tan();
    let n1 = a / (1.0 - e2 * sinφ1 * sinφ1).sqrt();
    let r1 = a * (1.0 - e2) / (1.0 - e2 * sinφ1 * sinφ1).powf(1.5);
    let t1 = tanφ1 * tanφ1;
    let c1 = ep2 * cosφ1 * cosφ1;
    let d = x / (n1 * k0);
    let lat = φ1
        - (n1 * tanφ1 / r1)
            * (d.powi(2) / 2.0
                - (5.0 + 3.0 * t1 + 10.0 * c1 - 4.0 * c1 * c1 - 9.0 * ep2) * d.powi(4) / 24.0
                + (61.0 + 90.0 * t1 + 298.0 * c1 + 45.0 * t1 * t1 - 252.0 * ep2 - 3.0 * c1 * c1)
                    * d.powi(6)
                    / 720.0);
    let lon = crs.λ0()
        + (d - (1.0 + 2.0 * t1 + c1) * d.powi(3) / 6.0
            + (5.0 - 2.0 * c1 + 28.0 * t1 - 3.0 * c1 * c1 + 8.0 * ep2 + 24.0 * t1 * t1)
                * d.powi(5)
                / 120.0)
            / cosφ1;
    Coord {
        lon: lon.to_degrees(),
        lat: lat.to_degrees(),
        ele,
    }
}
