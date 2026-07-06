use crate::builder::SegmentDraft;
use crate::crs::{CoordProjector, CrsVerdict, projector, validate_prj_wkt};
use crate::geo::{Coord, LineString};
use crate::model::{Access, CrossingKind, EdgeTravel, Provenance, Terrain};
use crate::overlay::{
    AccessOverlay, AccessWindow, ContextOverlay, DailyTimeWindow, MonthDay, OverlayGeometry,
    PlanningDate, PlanningTime, SeasonalWindow, TerrainOverlay, WeekdaySet,
};
use crate::{Result, TrailgenError};
use ::shapefile::dbase::{FieldValue, Record};
use ::shapefile::{Point, PointM, PointZ, PolygonRing, Shape};
use std::fs;
use std::path::Path;

pub fn network_from_path(path: &Path) -> Result<Vec<SegmentDraft>> {
    let mut drafts = Vec::new();
    let crs = shapefile_projector(path)?;
    for (i, row) in read(path)?.into_iter().enumerate() {
        let (shape, record) = row;
        let props = ShpProps::new(&record);
        let surface = props.str("surface").map(str::to_owned);
        let terrain_tag = props.str("terrain");
        let surface_terrain = surface
            .as_deref()
            .map_or(Terrain::Unknown, Terrain::from_tag);
        let terrain = terrain_tag
            .map(Terrain::from_tag)
            .filter(|terrain| *terrain != Terrain::Unknown)
            .unwrap_or(surface_terrain);
        let terrain_confidence = terrain_confidence_from_props(
            &props,
            terrain,
            terrain_tag.is_some(),
            surface_terrain != Terrain::Unknown,
        );
        let access = props
            .str("access")
            .or_else(|| props.str("status"))
            .map_or(Access::Unknown, Access::from_tag);
        let travel = travel_from_props(&props);
        let confidence = props.f64("confidence").unwrap_or(0.78).clamp(0.0, 1.0);
        let road_exposure = props
            .f64("road_exposure")
            .or_else(|| props.bool("road").map(f64::from))
            .unwrap_or_else(|| f64::from(matches!(terrain, Terrain::Road | Terrain::Pavement)));
        let provenance = Provenance {
            source: props
                .str("source")
                .unwrap_or("shapefile-network")
                .to_owned(),
            layer: props.str("layer").map(str::to_owned),
            source_id: props
                .str("id")
                .or_else(|| props.str("name"))
                .map(str::to_owned)
                .or_else(|| Some(format!("feature-{i}"))),
            license: props.str("license").map(str::to_owned),
        };
        for line in lines(&shape, crs)? {
            drafts.push(SegmentDraft {
                geometry: line,
                terrain,
                terrain_confidence: Some(terrain_confidence),
                surface: surface.clone(),
                access,
                travel,
                road_exposure,
                confidence,
                provenance: provenance.clone(),
            });
        }
    }
    Ok(drafts)
}

fn terrain_confidence_from_props(
    props: &ShpProps<'_>,
    terrain: Terrain,
    explicit_terrain: bool,
    explicit_surface: bool,
) -> f64 {
    props.f64("terrain_confidence").map_or_else(
        || {
            if terrain == Terrain::Unknown {
                0.0
            } else if explicit_terrain {
                0.90
            } else if explicit_surface {
                0.82
            } else {
                0.45
            }
        },
        |confidence| confidence.clamp(0.0, 1.0),
    )
}

pub fn access_overlays_from_path(path: &Path) -> Result<Vec<AccessOverlay>> {
    let mut overlays = Vec::new();
    let crs = shapefile_projector(path)?;
    for (i, row) in read(path)?.into_iter().enumerate() {
        let (shape, record) = row;
        let props = ShpProps::new(&record);
        let access = access_from_props(&props);
        let name = props
            .str("name")
            .or_else(|| props.str("id"))
            .map_or_else(|| format!("overlay-{i}"), str::to_owned);
        let provenance = Provenance {
            source: props
                .str("source")
                .unwrap_or("shapefile-access-overlay")
                .to_owned(),
            layer: props.str("layer").map(str::to_owned),
            source_id: Some(name.clone()),
            license: props.str("license").map(str::to_owned),
        };
        for geometry in overlay_geometries(&shape, crs)? {
            overlays.push(AccessOverlay {
                name: name.clone(),
                access,
                travel: travel_override_from_props(&props),
                active: access_window_from_props(&props)?,
                confidence: props.f64("confidence").unwrap_or(0.86).clamp(0.0, 1.0),
                tolerance_m: props.f64("tolerance_m").unwrap_or(20.0).max(0.0),
                provenance: provenance.clone(),
                geometry,
            });
        }
    }
    Ok(overlays)
}

fn access_window_from_props(props: &ShpProps<'_>) -> Result<AccessWindow> {
    Ok(AccessWindow {
        from: props.date(&["active_from", "start_date", "starts_on", "from"])?,
        to: props.date(&["active_to", "end_date", "ends_on", "to"])?,
        seasonal: props.seasonal_window()?,
        weekdays: props.weekdays()?,
        time: props.time_window()?,
    })
}

fn access_from_props(props: &ShpProps<'_>) -> Access {
    props
        .str("access")
        .or_else(|| props.str("status"))
        .map(Access::from_tag)
        .or_else(|| {
            props
                .permit_required(&[
                    "permit_required",
                    "requires_permit",
                    "permit_req",
                    "permit",
                    "permits",
                    "reservation_required",
                    "requires_reservation",
                    "reserv_req",
                    "reservation",
                    "reservatio",
                    "timed_entry_required",
                    "timed_req",
                    "timedentry",
                    "quota_required",
                    "quota_req",
                ])
                .map(|required| {
                    if required {
                        Access::Restricted
                    } else {
                        Access::Open
                    }
                })
        })
        .unwrap_or(Access::Closed)
}

impl ShpProps<'_> {
    fn permit_required(&self, keys: &[&str]) -> Option<bool> {
        keys.iter().find_map(|key| self.bool(key))
    }
}

pub fn terrain_overlays_from_path(path: &Path) -> Result<Vec<TerrainOverlay>> {
    let mut overlays = Vec::new();
    let crs = shapefile_projector(path)?;
    for (i, row) in read(path)?.into_iter().enumerate() {
        let (shape, record) = row;
        let props = ShpProps::new(&record);
        let surface = props.str("surface").map(str::to_owned);
        let terrain = props.terrain();
        if terrain == Terrain::Unknown {
            return Err(TrailgenError::InvalidData(format!(
                "shapefile terrain feature {i} has no recognized terrain/surface/landcover tag"
            )));
        }
        let name = props
            .str("name")
            .or_else(|| props.str("id"))
            .map_or_else(|| format!("terrain-{i}"), str::to_owned);
        let provenance = provenance(&props, "shapefile-terrain-overlay", &name);
        for geometry in overlay_geometries(&shape, crs)? {
            overlays.push(TerrainOverlay {
                name: name.clone(),
                terrain,
                surface: surface.clone(),
                confidence: props.f64("confidence").unwrap_or(0.75).clamp(0.0, 1.0),
                tolerance_m: props.f64("tolerance_m").unwrap_or(20.0).max(0.0),
                provenance: provenance.clone(),
                geometry,
            });
        }
    }
    Ok(overlays)
}

pub fn context_overlays_from_path(path: &Path) -> Result<Vec<ContextOverlay>> {
    let mut overlays = Vec::new();
    let crs = shapefile_projector(path)?;
    for (i, row) in read(path)?.into_iter().enumerate() {
        let (shape, record) = row;
        let props = ShpProps::new(&record);
        let Some(kind) = props
            .str("kind")
            .or_else(|| props.str("context"))
            .or_else(|| props.str("type"))
            .and_then(CrossingKind::from_tag)
            .or_else(|| context_kind_from_path(path))
        else {
            continue;
        };
        let name = props
            .str("name")
            .or_else(|| props.str("id"))
            .map_or_else(|| format!("context-{i}"), str::to_owned);
        let provenance = provenance(&props, default_context_source(kind), &name);
        for geometry in lines(&shape, crs)? {
            overlays.push(ContextOverlay {
                name: name.clone(),
                kind,
                confidence: props.f64("confidence").unwrap_or(0.80).clamp(0.0, 1.0),
                provenance: provenance.clone(),
                geometry,
            });
        }
    }
    Ok(overlays)
}

fn read(path: &Path) -> Result<Vec<(Shape, Record)>> {
    ::shapefile::read(path).map_err(|error| {
        TrailgenError::InvalidData(format!("read shapefile {}: {error}", path.display()))
    })
}

fn shapefile_projector(path: &Path) -> Result<CoordProjector> {
    let prj = path.with_extension("prj");
    if !prj.exists() {
        return Ok(projector(CrsVerdict::AssumedGeographic));
    }
    let wkt = fs::read_to_string(&prj).map_err(|error| {
        TrailgenError::InvalidData(format!("read shapefile CRS {}: {error}", prj.display()))
    })?;
    Ok(projector(validate_prj_wkt(&wkt)?))
}

fn provenance(props: &ShpProps<'_>, default_source: &str, source_id: &str) -> Provenance {
    Provenance {
        source: props.str("source").unwrap_or(default_source).to_owned(),
        layer: props.str("layer").map(str::to_owned),
        source_id: Some(source_id.to_owned()),
        license: props.str("license").map(str::to_owned),
    }
}

fn context_kind_from_path(path: &Path) -> Option<CrossingKind> {
    let name = path.display().to_string().to_ascii_lowercase();
    if name.contains("road") || name.contains("street") {
        Some(CrossingKind::Road)
    } else if name.contains("hydrology")
        || name.contains("water")
        || name.contains("stream")
        || name.contains("creek")
        || name.contains("river")
    {
        Some(CrossingKind::Water)
    } else {
        None
    }
}

const fn default_context_source(kind: CrossingKind) -> &'static str {
    match kind {
        CrossingKind::Road => "shapefile-road-context",
        CrossingKind::Water => "shapefile-hydrology-context",
    }
}

fn travel_override_from_props(props: &ShpProps<'_>) -> Option<EdgeTravel> {
    props
        .str("travel")
        .or_else(|| props.str("travel_direction"))
        .or_else(|| props.str("direction"))
        .or_else(|| props.str("oneway"))
        .or_else(|| props.str("one_way"))
        .map(EdgeTravel::from_tag)
        .or_else(|| {
            props
                .bool("oneway")
                .or_else(|| props.bool("one_way"))
                .map(|oneway| {
                    if oneway {
                        EdgeTravel::Forward
                    } else {
                        EdgeTravel::Both
                    }
                })
        })
}

fn travel_from_props(props: &ShpProps<'_>) -> EdgeTravel {
    travel_override_from_props(props).unwrap_or_default()
}

fn lines(shape: &Shape, crs: CoordProjector) -> Result<Vec<LineString>> {
    match shape {
        Shape::Polyline(polyline) => polyline
            .parts()
            .iter()
            .map(|part| line_xy(part, crs))
            .collect(),
        Shape::PolylineM(polyline) => polyline
            .parts()
            .iter()
            .map(|part| line_xym(part, crs))
            .collect(),
        Shape::PolylineZ(polyline) => polyline
            .parts()
            .iter()
            .map(|part| line_xyz(part, crs))
            .collect(),
        other => Err(TrailgenError::UnsupportedFormat(format!(
            "shapefile network geometry {:?}",
            other.shapetype()
        ))),
    }
}

fn overlay_geometries(shape: &Shape, crs: CoordProjector) -> Result<Vec<OverlayGeometry>> {
    match shape {
        Shape::Polygon(polygon) => Ok(outer_rings(polygon.rings(), |points| ring_xy(points, crs))),
        Shape::PolygonM(polygon) => {
            Ok(outer_rings(polygon.rings(), |points| ring_xym(points, crs)))
        }
        Shape::PolygonZ(polygon) => {
            Ok(outer_rings(polygon.rings(), |points| ring_xyz(points, crs)))
        }
        Shape::Polyline(polyline) => polyline
            .parts()
            .iter()
            .map(|part| Ok(OverlayGeometry::Line(line_xy(part, crs)?)))
            .collect(),
        Shape::PolylineM(polyline) => polyline
            .parts()
            .iter()
            .map(|part| Ok(OverlayGeometry::Line(line_xym(part, crs)?)))
            .collect(),
        Shape::PolylineZ(polyline) => polyline
            .parts()
            .iter()
            .map(|part| Ok(OverlayGeometry::Line(line_xyz(part, crs)?)))
            .collect(),
        other => Err(TrailgenError::UnsupportedFormat(format!(
            "shapefile overlay geometry {:?}",
            other.shapetype()
        ))),
    }
}

fn outer_rings<P, F>(rings: &[PolygonRing<P>], f: F) -> Vec<OverlayGeometry>
where
    F: Fn(&[P]) -> Vec<Coord>,
{
    rings
        .iter()
        .filter_map(|ring| match ring {
            PolygonRing::Outer(points) => Some(OverlayGeometry::Polygon(f(points))),
            PolygonRing::Inner(_) => None,
        })
        .collect()
}

fn line_xy(points: &[Point], crs: CoordProjector) -> Result<LineString> {
    LineString::new(points.iter().map(|p| crs.project(p.x, p.y, None)).collect())
}

fn line_xym(points: &[PointM], crs: CoordProjector) -> Result<LineString> {
    LineString::new(points.iter().map(|p| crs.project(p.x, p.y, None)).collect())
}

fn line_xyz(points: &[PointZ], crs: CoordProjector) -> Result<LineString> {
    LineString::new(
        points
            .iter()
            .map(|p| crs.project(p.x, p.y, Some(p.z)))
            .collect(),
    )
}

fn ring_xy(points: &[Point], crs: CoordProjector) -> Vec<Coord> {
    points.iter().map(|p| crs.project(p.x, p.y, None)).collect()
}

fn ring_xym(points: &[PointM], crs: CoordProjector) -> Vec<Coord> {
    points.iter().map(|p| crs.project(p.x, p.y, None)).collect()
}

fn ring_xyz(points: &[PointZ], crs: CoordProjector) -> Vec<Coord> {
    points
        .iter()
        .map(|p| crs.project(p.x, p.y, Some(p.z)))
        .collect()
}

struct ShpProps<'a> {
    record: &'a Record,
}

impl<'a> ShpProps<'a> {
    const fn new(record: &'a Record) -> Self {
        Self { record }
    }

    fn field(&self, key: &str) -> Option<&'a FieldValue> {
        self.record
            .as_ref()
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value)
    }

    fn str(&self, key: &str) -> Option<&'a str> {
        match self.field(key)? {
            FieldValue::Character(Some(value)) | FieldValue::Memo(value) => Some(value.trim()),
            _ => None,
        }
        .filter(|value| !value.is_empty())
    }

    fn f64(&self, key: &str) -> Option<f64> {
        match self.field(key)? {
            FieldValue::Numeric(Some(value))
            | FieldValue::Double(value)
            | FieldValue::Currency(value) => Some(*value),
            FieldValue::Float(Some(value)) => Some(f64::from(*value)),
            FieldValue::Integer(value) => Some(f64::from(*value)),
            FieldValue::Character(Some(value)) => value.trim().parse().ok(),
            _ => None,
        }
    }

    fn terrain(&self) -> Terrain {
        ["terrain", "surface"]
            .into_iter()
            .find_map(|key| {
                self.str(key)
                    .map(Terrain::from_tag)
                    .filter(|terrain| *terrain != Terrain::Unknown)
            })
            .unwrap_or_else(|| {
                [
                    "landcover",
                    "land_cover",
                    "landcover_class",
                    "land_cover_class",
                    "nlcd",
                    "nlcd_code",
                    "gridcode",
                    "class",
                    "class_name",
                    "cover",
                    "cover_type",
                ]
                .into_iter()
                .find_map(|key| self.landcover_terrain(key))
                .unwrap_or(Terrain::Unknown)
            })
    }

    fn landcover_terrain(&self, key: &str) -> Option<Terrain> {
        let value = self.field(key)?;
        let terrain = match value {
            FieldValue::Character(Some(value)) | FieldValue::Memo(value) => {
                Terrain::from_landcover_tag(value)
            }
            FieldValue::Numeric(Some(value))
            | FieldValue::Double(value)
            | FieldValue::Currency(value) => Terrain::from_landcover_tag(&value.to_string()),
            FieldValue::Float(Some(value)) => Terrain::from_landcover_tag(&value.to_string()),
            FieldValue::Integer(value) => Terrain::from_landcover_tag(&value.to_string()),
            _ => Terrain::Unknown,
        };
        (terrain != Terrain::Unknown).then_some(terrain)
    }

    fn date(&self, keys: &[&str]) -> Result<Option<PlanningDate>> {
        keys.iter()
            .find_map(|key| self.str(key))
            .map(str::parse::<PlanningDate>)
            .transpose()
            .map_err(TrailgenError::InvalidData)
    }

    fn month_day(&self, keys: &[&str]) -> Result<Option<MonthDay>> {
        keys.iter()
            .find_map(|key| self.str(key))
            .map(str::parse::<MonthDay>)
            .transpose()
            .map_err(TrailgenError::InvalidData)
    }

    fn time(&self, keys: &[&str]) -> Result<Option<PlanningTime>> {
        keys.iter()
            .find_map(|key| self.str(key))
            .map(str::parse::<PlanningTime>)
            .transpose()
            .map_err(TrailgenError::InvalidData)
    }

    fn seasonal_window(&self) -> Result<Option<SeasonalWindow>> {
        let from = self.month_day(&[
            "seasonal_from",
            "active_month_from",
            "season_from",
            "month_from",
            "recurs_from",
        ])?;
        let to = self.month_day(&[
            "seasonal_to",
            "active_month_to",
            "season_to",
            "month_to",
            "recurs_to",
        ])?;
        match (from, to) {
            (None, None) => Ok(None),
            (Some(from), Some(to)) => Ok(Some(SeasonalWindow::new(from, to))),
            _ => Err(TrailgenError::InvalidData(
                "recurring seasonal access windows require both from and to month-days".to_owned(),
            )),
        }
    }

    fn time_window(&self) -> Result<Option<DailyTimeWindow>> {
        let from = self.time(&[
            "time_from",
            "active_time_from",
            "start_time",
            "starts_at",
            "hour_from",
            "hours_from",
        ])?;
        let to = self.time(&[
            "time_to",
            "active_time_to",
            "end_time",
            "ends_at",
            "hour_to",
            "hours_to",
        ])?;
        match (from, to) {
            (None, None) => Ok(None),
            (Some(from), Some(to)) => Ok(Some(DailyTimeWindow::new(from, to))),
            _ => Err(TrailgenError::InvalidData(
                "hourly access windows require both from and to times".to_owned(),
            )),
        }
    }

    fn weekdays(&self) -> Result<WeekdaySet> {
        [
            "weekdays",
            "weekday",
            "days",
            "day_of_week",
            "active_weekdays",
            "active_days",
        ]
        .into_iter()
        .find_map(|key| self.str(key))
        .map(str::parse::<WeekdaySet>)
        .transpose()
        .map(Option::unwrap_or_default)
        .map_err(TrailgenError::InvalidData)
    }

    fn bool(&self, key: &str) -> Option<bool> {
        match self.field(key)? {
            FieldValue::Logical(Some(value)) => Some(*value),
            FieldValue::Character(Some(value)) => {
                match value.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "t" | "yes" | "y" => Some(true),
                    "0" | "false" | "f" | "no" | "n" => Some(false),
                    _ => None,
                }
            }
            FieldValue::Numeric(Some(value)) => Some(*value != 0.0),
            FieldValue::Integer(value) => Some(*value != 0),
            _ => None,
        }
    }
}
