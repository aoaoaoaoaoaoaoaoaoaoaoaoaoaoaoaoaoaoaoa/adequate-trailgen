use crate::geo::{Coord, LineString};
use crate::io::route_file::{RouteFile, RouteFileMetadata, clean_text};
use crate::{Result, TrailgenError};

pub fn route_line_from_str(s: &str) -> Result<LineString> {
    route_file_from_str(s).map(|route| route.line)
}

pub fn route_file_from_str(s: &str) -> Result<RouteFile> {
    let doc = roxmltree::Document::parse(s).map_err(|e| TrailgenError::Xml(e.to_string()))?;
    let coords = doc
        .descendants()
        .find(|n| n.has_tag_name("coordinates"))
        .and_then(|n| n.text())
        .ok_or_else(|| TrailgenError::InvalidData("KML has no coordinates element".to_owned()))?;
    let points = coords
        .split_whitespace()
        .map(|tuple| {
            let mut xs = tuple.split(',');
            let lon = xs
                .next()
                .ok_or_else(|| TrailgenError::InvalidData("KML coordinate missing lon".to_owned()))?
                .parse::<f64>()
                .map_err(|e| TrailgenError::InvalidData(format!("invalid KML lon: {e}")))?;
            let lat = xs
                .next()
                .ok_or_else(|| TrailgenError::InvalidData("KML coordinate missing lat".to_owned()))?
                .parse::<f64>()
                .map_err(|e| TrailgenError::InvalidData(format!("invalid KML lat: {e}")))?;
            let ele = xs.next().and_then(|x| x.parse::<f64>().ok());
            Ok(Coord { lon, lat, ele })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(RouteFile::new(
        LineString::new(points)?,
        metadata_from_doc(&doc),
    ))
}

fn metadata_from_doc(doc: &roxmltree::Document<'_>) -> RouteFileMetadata {
    let placemark = doc.descendants().find(|n| n.has_tag_name("Placemark"));
    RouteFileMetadata {
        title: placemark.and_then(|n| child_text(n, "name")).or_else(|| {
            doc.descendants()
                .find(|n| n.has_tag_name("Document"))
                .and_then(|n| child_text(n, "name"))
        }),
        description: placemark.and_then(|n| child_text(n, "description")),
        recorded_at: doc
            .descendants()
            .find(|n| n.has_tag_name("when"))
            .and_then(|n| n.text())
            .and_then(clean_text),
        activity_type: None,
    }
}

fn child_text(node: roxmltree::Node<'_, '_>, tag: &str) -> Option<String> {
    node.children()
        .find(|child| child.has_tag_name(tag))
        .and_then(|child| child.text())
        .and_then(clean_text)
}
