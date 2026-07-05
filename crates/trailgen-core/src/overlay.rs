use crate::difficulty::DifficultyWeights;
use crate::geo::{Coord, LineString};
use crate::model::{
    Access, CrossingEvidence, CrossingKind, Edge, Provenance, Terrain, TerrainEvidence, TrailGraph,
};
use crate::{Result, TrailgenError};
use serde::de::Visitor;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PlanningDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl PlanningDate {
    #[must_use]
    pub const fn new(year: u16, month: u8, day: u8) -> Option<Self> {
        if year >= 1 && month >= 1 && month <= 12 && day >= 1 && day <= days_in_month(year, month) {
            Some(Self { year, month, day })
        } else {
            None
        }
    }
}

impl Display for PlanningDate {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl FromStr for PlanningDate {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        let mut parts = raw.trim().split('-');
        let year = parts
            .next()
            .ok_or_else(|| "date must be YYYY-MM-DD".to_owned())?
            .parse::<u16>()
            .map_err(|error| error.to_string())?;
        let month = parts
            .next()
            .ok_or_else(|| "date must be YYYY-MM-DD".to_owned())?
            .parse::<u8>()
            .map_err(|error| error.to_string())?;
        let day = parts
            .next()
            .ok_or_else(|| "date must be YYYY-MM-DD".to_owned())?
            .parse::<u8>()
            .map_err(|error| error.to_string())?;
        if parts.next().is_some() {
            return Err("date must be YYYY-MM-DD".to_owned());
        }
        Self::new(year, month, day).ok_or_else(|| "invalid civil date".to_owned())
    }
}

impl Serialize for PlanningDate {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PlanningDate {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct DateVisitor;

        impl Visitor<'_> for DateVisitor {
            type Value = PlanningDate;

            fn expecting(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.write_str("a YYYY-MM-DD civil date")
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                v.parse::<PlanningDate>().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(DateVisitor)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AccessWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<PlanningDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<PlanningDate>,
}

impl AccessWindow {
    #[must_use]
    pub const fn is_always(&self) -> bool {
        self.from.is_none() && self.to.is_none()
    }

    #[must_use]
    pub fn contains(self, date: Option<PlanningDate>) -> bool {
        let Some(date) = date else {
            return true;
        };
        self.from.is_none_or(|from| from <= date) && self.to.is_none_or(|to| date <= to)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccessOverlay {
    pub name: String,
    pub access: Access,
    #[serde(default, skip_serializing_if = "AccessWindow::is_always")]
    pub active: AccessWindow,
    pub confidence: f64,
    pub tolerance_m: f64,
    pub provenance: Provenance,
    pub geometry: OverlayGeometry,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerrainOverlay {
    pub name: String,
    pub terrain: Terrain,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    pub confidence: f64,
    pub tolerance_m: f64,
    pub provenance: Provenance,
    pub geometry: OverlayGeometry,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextOverlay {
    pub name: String,
    pub kind: CrossingKind,
    pub confidence: f64,
    pub provenance: Provenance,
    pub geometry: LineString,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum OverlayGeometry {
    Polygon(Vec<Coord>),
    MultiPolygon(Vec<Vec<Coord>>),
    Line(LineString),
    MultiLine(Vec<LineString>),
}

impl OverlayGeometry {
    #[must_use]
    pub fn affects(&self, edge: &Edge, tolerance_m: f64) -> bool {
        let midpoint = edge_midpoint(edge);
        match self {
            Self::Polygon(ring) => point_in_ring(midpoint, ring),
            Self::MultiPolygon(rings) => rings.iter().any(|ring| point_in_ring(midpoint, ring)),
            Self::Line(line) => point_line_distance_m(midpoint, line) <= tolerance_m,
            Self::MultiLine(lines) => lines
                .iter()
                .any(|line| point_line_distance_m(midpoint, line) <= tolerance_m),
        }
    }
}

impl AccessOverlay {
    #[must_use]
    pub fn affects(&self, edge: &Edge) -> bool {
        self.geometry.affects(edge, self.tolerance_m)
    }

    #[must_use]
    pub fn active_on(&self, date: Option<PlanningDate>) -> bool {
        self.active.contains(date)
    }
}

impl TerrainOverlay {
    #[must_use]
    pub fn affects(&self, edge: &Edge) -> bool {
        self.geometry.affects(edge, self.tolerance_m)
    }
}

pub fn apply_access_overlays(
    graph: &mut TrailGraph,
    overlays: &[AccessOverlay],
    planning_date: Option<PlanningDate>,
    weights: DifficultyWeights,
) -> usize {
    let mut touched = 0usize;
    for edge in &mut graph.edges {
        for overlay in overlays {
            if !overlay.active_on(planning_date) || !overlay.affects(edge) {
                continue;
            }
            touched += 1;
            edge.attr.access = overlay.access;
            edge.attr.access_confidence = edge.attr.access_confidence.max(overlay.confidence);
            edge.attr.confidence = edge.attr.confidence.min(overlay.confidence);
            if !edge.attr.access_provenance.contains(&overlay.provenance) {
                edge.attr.access_provenance.push(overlay.provenance.clone());
            }
        }
        weights.apply_edge(edge);
    }
    touched
}

pub fn apply_terrain_overlays(
    graph: &mut TrailGraph,
    overlays: &[TerrainOverlay],
    weights: DifficultyWeights,
) -> usize {
    let mut touched = 0usize;
    for edge in &mut graph.edges {
        let mut changed = false;
        for overlay in overlays {
            if !overlay.affects(edge) {
                continue;
            }
            touched += 1;
            changed = true;
            edge.attr.terrain = overlay.terrain;
            if let Some(surface) = &overlay.surface {
                edge.attr.surface = Some(surface.clone());
            }
            edge.attr.terrain_confidence = edge.attr.terrain_confidence.max(overlay.confidence);
            edge.attr.confidence = edge.attr.confidence.min(overlay.confidence);
            push_terrain_evidence(edge, overlay);
        }
        if changed {
            weights.apply_edge(edge);
        }
    }
    touched
}

const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

const fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && !year.is_multiple_of(100) || year.is_multiple_of(400)
}

pub fn apply_context_overlays(
    graph: &mut TrailGraph,
    overlays: &[ContextOverlay],
    weights: DifficultyWeights,
) -> usize {
    let mut crossings = 0usize;
    for edge in &mut graph.edges {
        let mut touched = false;
        for overlay in overlays {
            let count = crossing_count(&edge.geometry, &overlay.geometry);
            if count == 0 {
                continue;
            }
            touched = true;
            crossings += usize::try_from(count).unwrap_or(usize::MAX);
            push_crossing(edge, overlay, count);
            if overlay.kind == CrossingKind::Road {
                edge.attr.road_exposure =
                    edge.attr.road_exposure.max(road_crossing_exposure(count));
            }
            edge.attr.confidence = edge.attr.confidence.min(overlay.confidence);
        }
        if touched {
            weights.apply_edge(edge);
        }
    }
    crossings
}

fn push_terrain_evidence(edge: &mut Edge, overlay: &TerrainOverlay) {
    let rationale = "terrain overlay";
    if let Some(existing) = edge.attr.terrain_evidence.iter_mut().find(|x| {
        x.terrain == overlay.terrain
            && x.provenance.as_ref() == Some(&overlay.provenance)
            && x.rationale == rationale
    }) {
        existing.confidence = existing.confidence.max(overlay.confidence);
        return;
    }
    edge.attr.terrain_evidence.push(TerrainEvidence {
        terrain: overlay.terrain,
        confidence: overlay.confidence,
        rationale: rationale.to_owned(),
        provenance: Some(overlay.provenance.clone()),
    });
}

#[must_use]
pub fn edge_midpoint(edge: &Edge) -> Coord {
    let points = &edge.geometry.points;
    let mid = points.len() / 2;
    if points.len().is_multiple_of(2) {
        points[mid - 1].lerp(points[mid], 0.5)
    } else {
        points[mid]
    }
}

fn point_in_ring(point: Coord, ring: &[Coord]) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let pi = ring[i];
        let pj = ring[j];
        let crosses = (pi.lat > point.lat) != (pj.lat > point.lat);
        if crosses {
            let lon = (pj.lon - pi.lon).mul_add((point.lat - pi.lat) / (pj.lat - pi.lat), pi.lon);
            if point.lon < lon {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

fn point_line_distance_m(point: Coord, line: &LineString) -> f64 {
    line.points
        .windows(2)
        .map(|segment| point_segment_distance_m(point, segment[0], segment[1]))
        .min_by(f64::total_cmp)
        .unwrap_or(f64::INFINITY)
}

fn crossing_count(a: &LineString, b: &LineString) -> u32 {
    a.points
        .windows(2)
        .map(|lhs| {
            b.points
                .windows(2)
                .filter(|rhs| segments_cross(lhs[0], lhs[1], rhs[0], rhs[1]))
                .count()
        })
        .sum::<usize>()
        .try_into()
        .unwrap_or(u32::MAX)
}

fn segments_cross(a0: Coord, a1: Coord, b0: Coord, b1: Coord) -> bool {
    let d1 = orient(a0, a1, b0);
    let d2 = orient(a0, a1, b1);
    let d3 = orient(b0, b1, a0);
    let d4 = orient(b0, b1, a1);
    if d1.abs() <= 1.0e-12 && on_segment(a0, a1, b0)
        || d2.abs() <= 1.0e-12 && on_segment(a0, a1, b1)
        || d3.abs() <= 1.0e-12 && on_segment(b0, b1, a0)
        || d4.abs() <= 1.0e-12 && on_segment(b0, b1, a1)
    {
        return true;
    }
    (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0)
}

fn orient(a: Coord, b: Coord, c: Coord) -> f64 {
    (b.lon - a.lon).mul_add(c.lat - a.lat, -(b.lat - a.lat) * (c.lon - a.lon))
}

fn on_segment(a: Coord, b: Coord, p: Coord) -> bool {
    (a.lon.min(b.lon) - 1.0e-12..=a.lon.max(b.lon) + 1.0e-12).contains(&p.lon)
        && (a.lat.min(b.lat) - 1.0e-12..=a.lat.max(b.lat) + 1.0e-12).contains(&p.lat)
}

fn push_crossing(edge: &mut Edge, overlay: &ContextOverlay, count: u32) {
    if let Some(existing) = edge
        .attr
        .crossings
        .iter_mut()
        .find(|x| x.kind == overlay.kind && x.provenance == overlay.provenance)
    {
        existing.count = existing.count.max(count);
        return;
    }
    edge.attr.crossings.push(CrossingEvidence {
        kind: overlay.kind,
        count,
        provenance: overlay.provenance.clone(),
    });
}

fn road_crossing_exposure(count: u32) -> f64 {
    (f64::from(count) * 0.03).clamp(0.0, 0.20)
}

fn point_segment_distance_m(point: Coord, start: Coord, end: Coord) -> f64 {
    let lat_scale = 111_320.0;
    let lon_scale = lat_scale * point.lat.to_radians().cos().abs().max(0.01);
    let point_x = point.lon * lon_scale;
    let point_y = point.lat * lat_scale;
    let start_x = start.lon * lon_scale;
    let start_y = start.lat * lat_scale;
    let end_x = end.lon * lon_scale;
    let end_y = end.lat * lat_scale;
    let delta_x = end_x - start_x;
    let delta_y = end_y - start_y;
    let denom = delta_x.mul_add(delta_x, delta_y * delta_y);
    if denom <= f64::EPSILON {
        return (point_x - start_x).hypot(point_y - start_y);
    }
    let projection = ((point_y - start_y).mul_add(delta_y, (point_x - start_x) * delta_x) / denom)
        .clamp(0.0, 1.0);
    let closest_x = delta_x.mul_add(projection, start_x);
    let closest_y = delta_y.mul_add(projection, start_y);
    (point_x - closest_x).hypot(point_y - closest_y)
}

pub fn polygon(ring: Vec<Coord>) -> Result<OverlayGeometry> {
    if ring.len() < 4 {
        return Err(TrailgenError::InvalidGeometry(
            "overlay polygon ring needs at least four coordinates".to_owned(),
        ));
    }
    Ok(OverlayGeometry::Polygon(ring))
}
