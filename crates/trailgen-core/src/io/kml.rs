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

#[must_use]
pub fn route_to_kml(graph: &WalkGraph, route: &Route) -> String {
    let line = route.geometry(graph);
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<kml xmlns="http://www.opengis.net/kml/2.2">
  <Document>
"#,
    );
    s.push_str("    <Placemark>\n      <name>");
    escape_xml(&route.name, &mut s);
    s.push_str("</name>\n      <description>");
    escape_xml(&export_summary(route), &mut s);
    s.push_str(
        "</description>\n      <LineString>\n        <tessellate>1</tessellate>\n        <coordinates>\n",
    );
    for c in line.points {
        let _ = match c.ele {
            Some(ele) => writeln!(s, "          {:.8},{:.8},{ele:.2}", c.lon, c.lat),
            None => writeln!(s, "          {:.8},{:.8},0", c.lon, c.lat),
        };
    }
    s.push_str(
        "        </coordinates>\n      </LineString>\n    </Placemark>\n  </Document>\n</kml>\n",
    );
    s
}

fn escape_xml(raw: &str, out: &mut String) {
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
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
