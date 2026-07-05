use crate::{Result, TrailgenError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrsVerdict {
    AssumedGeographic,
    Wgs84,
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
    } else {
        Err(TrailgenError::InvalidData(format!(
            "{} {name:?} is not supported; reproject input to geographic lon/lat WGS84 (EPSG:4326/CRS84) before ingestion",
            kind.label()
        )))
    }
}

pub fn validate_prj_wkt(wkt: &str) -> Result<CrsVerdict> {
    let normalized = normalize(wkt);
    if normalized.contains("PROJCS")
        || normalized.contains("PROJCRS")
        || normalized.contains("PROJECTION")
    {
        return Err(TrailgenError::InvalidData(
            "shapefile .prj advertises a projected CRS; reproject input to geographic lon/lat WGS84 (EPSG:4326/CRS84) before ingestion"
                .to_owned(),
        ));
    }
    validate_crs_name(VectorCrsKind::ShapefilePrj, wkt)
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
