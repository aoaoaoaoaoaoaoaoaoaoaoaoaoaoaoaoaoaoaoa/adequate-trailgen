use crate::geo::{Coord, LineString};
use crate::{Result, TrailgenError};

pub fn route_line_from_str(s: &str) -> Result<LineString> {
    let mut rows = s
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let first = rows
        .next()
        .ok_or_else(|| TrailgenError::InvalidData("CSV route has no rows".to_owned()))?;
    let header = CsvHeader::parse(first);
    let mut points = Vec::new();
    if let Some(header) = header {
        for row in rows {
            points.push(header.coord(row)?);
        }
    } else {
        points.push(coord_from_bare_row(first)?);
        for row in rows {
            points.push(coord_from_bare_row(row)?);
        }
    }
    LineString::new(points)
}

#[derive(Clone, Copy)]
struct CsvHeader {
    lon: usize,
    lat: usize,
    ele: Option<usize>,
}

impl CsvHeader {
    fn parse(row: &str) -> Option<Self> {
        let fields = split(row)
            .into_iter()
            .map(|x| x.trim().to_ascii_lowercase())
            .collect::<Vec<_>>();
        let lon = fields
            .iter()
            .position(|x| matches!(x.as_str(), "lon" | "lng" | "long" | "longitude"))?;
        let lat = fields
            .iter()
            .position(|x| matches!(x.as_str(), "lat" | "latitude"))?;
        let ele = fields
            .iter()
            .position(|x| matches!(x.as_str(), "ele" | "elevation" | "elevation_m" | "alt"));
        Some(Self { lon, lat, ele })
    }

    fn coord(self, row: &str) -> Result<Coord> {
        let fields = split(row);
        let lon = field_f64(&fields, self.lon, "longitude")?;
        let lat = field_f64(&fields, self.lat, "latitude")?;
        let ele = self.ele.and_then(|i| fields.get(i)?.trim().parse().ok());
        Ok(Coord { lon, lat, ele })
    }
}

fn coord_from_bare_row(row: &str) -> Result<Coord> {
    let fields = split(row);
    let lon = field_f64(&fields, 0, "longitude")?;
    let lat = field_f64(&fields, 1, "latitude")?;
    let ele = fields.get(2).and_then(|x| x.trim().parse().ok());
    Ok(Coord { lon, lat, ele })
}

fn split(row: &str) -> Vec<&str> {
    row.split([',', '\t', ';']).collect()
}

fn field_f64(fields: &[&str], index: usize, name: &str) -> Result<f64> {
    fields
        .get(index)
        .ok_or_else(|| TrailgenError::InvalidData(format!("CSV row missing {name}")))?
        .trim()
        .parse()
        .map_err(|e| TrailgenError::InvalidData(format!("invalid CSV {name}: {e}")))
}
