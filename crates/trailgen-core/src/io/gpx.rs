use crate::geo::{Coord, LineString};
use crate::model::TrailGraph;
use crate::route::Route;
use crate::{Result, TrailgenError};
use std::fmt::Write as _;

pub fn route_line_from_str(s: &str) -> Result<LineString> {
    let doc = roxmltree::Document::parse(s).map_err(|e| TrailgenError::Xml(e.to_string()))?;
    let points = doc
        .descendants()
        .filter(|n| n.has_tag_name("trkpt") || n.has_tag_name("rtept"))
        .map(|n| {
            let lon = n
                .attribute("lon")
                .ok_or_else(|| TrailgenError::InvalidData("GPX point missing lon".to_owned()))?
                .parse::<f64>()
                .map_err(|e| TrailgenError::InvalidData(format!("invalid GPX lon: {e}")))?;
            let lat = n
                .attribute("lat")
                .ok_or_else(|| TrailgenError::InvalidData("GPX point missing lat".to_owned()))?
                .parse::<f64>()
                .map_err(|e| TrailgenError::InvalidData(format!("invalid GPX lat: {e}")))?;
            let ele = n
                .children()
                .find(|c| c.has_tag_name("ele"))
                .and_then(|e| e.text())
                .and_then(|e| e.parse::<f64>().ok());
            Ok(Coord { lon, lat, ele })
        })
        .collect::<Result<Vec<_>>>()?;
    LineString::new(points)
}

#[must_use]
pub fn route_to_gpx(graph: &TrailGraph, route: &Route) -> String {
    let line = route.geometry(graph);
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="adequate-trailgen" xmlns="http://www.topografix.com/GPX/1/1">
  <trk>
"#,
    );
    s.push_str("    <name>");
    escape_xml(&route.name, &mut s);
    s.push_str("</name>\n    <trkseg>\n");
    for c in line.points {
        let _ = write!(s, r#"      <trkpt lat="{:.8}" lon="{:.8}">"#, c.lat, c.lon);
        if let Some(ele) = c.ele {
            let _ = write!(s, "<ele>{ele:.2}</ele>");
        }
        s.push_str("</trkpt>\n");
    }
    s.push_str("    </trkseg>\n  </trk>\n</gpx>\n");
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
