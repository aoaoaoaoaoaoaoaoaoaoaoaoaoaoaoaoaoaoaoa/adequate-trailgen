use crate::geo::LineString;
use crate::io::kml;
use crate::io::route_file::RouteFile;
use crate::{Result, TrailgenError};
use std::io::{Cursor, Read};

const MAX_KML_BYTES: u64 = 32 * 1024 * 1024;

pub fn route_line_from_bytes(bytes: &[u8]) -> Result<LineString> {
    route_file_from_bytes(bytes).map(|route| route.line)
}

pub fn route_file_from_bytes(bytes: &[u8]) -> Result<RouteFile> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| TrailgenError::InvalidData(format!("invalid KMZ archive: {e}")))?;
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|e| TrailgenError::InvalidData(format!("invalid KMZ member: {e}")))?;
        if !file.name().to_ascii_lowercase().ends_with(".kml") {
            continue;
        }
        if file.size() > MAX_KML_BYTES {
            return Err(TrailgenError::InvalidData(format!(
                "KMZ KML member is {} bytes; limit is {MAX_KML_BYTES}",
                file.size()
            )));
        }
        let mut xml = String::new();
        file.take(MAX_KML_BYTES + 1)
            .read_to_string(&mut xml)
            .map_err(|e| TrailgenError::InvalidData(format!("invalid KMZ KML text: {e}")))?;
        if u64::try_from(xml.len()).unwrap_or(u64::MAX) > MAX_KML_BYTES {
            return Err(TrailgenError::InvalidData(format!(
                "KMZ KML member exceeds {MAX_KML_BYTES} byte limit"
            )));
        }
        return kml::route_file_from_str(&xml);
    }
    Err(TrailgenError::InvalidData(
        "KMZ archive contains no KML member".to_owned(),
    ))
}
