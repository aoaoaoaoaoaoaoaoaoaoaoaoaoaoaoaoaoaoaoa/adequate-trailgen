use crate::geo::{Coord, LineString};
use crate::io::route_file::{RouteFile, RouteFileMetadata, clean_text, export_summary};
use crate::model::WalkGraph;
use crate::route::Route;
use crate::{Result, TrailgenError};
use std::fmt::Write as _;

pub fn route_line_from_str(s: &str) -> Result<LineString> {
    route_file_from_str(s).map(|route| route.line)
}

pub fn route_file_from_str(s: &str) -> Result<RouteFile> {
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
    Ok(RouteFile::new(
        LineString::new(points)?,
        metadata_from_comments(s),
    ))
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

#[must_use]
pub fn route_to_csv(graph: &WalkGraph, route: &Route) -> String {
    let mut out = format!(
        "# name: {}\n# description: {}\n# activity: hiking\nlongitude,latitude,elevation_m\n",
        csv_comment(&route.name),
        csv_comment(&export_summary(route))
    );
    for Coord { lon, lat, ele } in route.geometry(graph).points {
        writeln!(out, "{lon:.7},{lat:.7},{}", csv_ele(ele)).expect("write to string");
    }
    out
}

fn csv_comment(raw: &str) -> String {
    raw.replace(['\n', '\r'], " ")
}

fn csv_ele(ele: Option<f64>) -> String {
    ele.map_or_else(String::new, |x| format!("{x:.3}"))
}

fn metadata_from_comments(s: &str) -> RouteFileMetadata {
    let mut metadata = RouteFileMetadata::default();
    for line in s.lines().map(str::trim) {
        let Some(comment) = line.strip_prefix('#') else {
            if !line.is_empty() {
                break;
            }
            continue;
        };
        let Some((key, value)) = comment.split_once(':') else {
            continue;
        };
        let value = clean_text(value);
        match key.trim().to_ascii_lowercase().as_str() {
            "name" | "title" => metadata.title = metadata.title.or(value),
            "description" | "desc" => metadata.description = metadata.description.or(value),
            "recorded_at" | "time" | "timestamp" | "date" => {
                metadata.recorded_at = metadata.recorded_at.or(value);
            }
            "activity" | "activity_type" | "sport" | "type" => {
                metadata.activity_type = metadata.activity_type.or(value);
            }
            _ => {}
        }
    }
    metadata
}
