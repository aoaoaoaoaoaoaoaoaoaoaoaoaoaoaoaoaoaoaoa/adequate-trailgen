use crate::geo::LineString;
use crate::io::kml;
use crate::io::route_file::RouteFile;
use crate::model::TrailGraph;
use crate::route::Route;
use crate::{Result, TrailgenError};
use std::io::{Cursor, Read, Write};
use zip::write::SimpleFileOptions;

pub fn route_line_from_bytes(bytes: &[u8]) -> Result<LineString> {
    route_file_from_bytes(bytes).map(|route| route.line)
}

pub fn route_file_from_bytes(bytes: &[u8]) -> Result<RouteFile> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| TrailgenError::InvalidData(format!("invalid KMZ archive: {e}")))?;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|e| TrailgenError::InvalidData(format!("invalid KMZ member: {e}")))?;
        if !file.name().to_ascii_lowercase().ends_with(".kml") {
            continue;
        }
        let mut xml = String::new();
        file.read_to_string(&mut xml)
            .map_err(|e| TrailgenError::InvalidData(format!("invalid KMZ KML text: {e}")))?;
        return kml::route_file_from_str(&xml);
    }
    Err(TrailgenError::InvalidData(
        "KMZ archive contains no KML member".to_owned(),
    ))
}

pub fn route_to_kmz(graph: &TrailGraph, route: &Route) -> Result<Vec<u8>> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(cursor);
    writer
        .start_file("doc.kml", SimpleFileOptions::default())
        .map_err(|e| TrailgenError::InvalidData(format!("cannot start KMZ member: {e}")))?;
    writer
        .write_all(kml::route_to_kml(graph, route).as_bytes())
        .map_err(|e| TrailgenError::InvalidData(format!("cannot write KMZ member: {e}")))?;
    let cursor = writer
        .finish()
        .map_err(|e| TrailgenError::InvalidData(format!("cannot finish KMZ archive: {e}")))?;
    Ok(cursor.into_inner())
}
