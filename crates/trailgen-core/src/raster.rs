use crate::crs::{UtmCrs, geographic_to_utm, wgs84_to_web_mercator};
use crate::enrich::{ElevationSample, ElevationSampler};
use crate::geo::Coord;
use crate::model::Provenance;
use crate::{Result, TrailgenError};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::iter::Peekable;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;
use tiff::{TiffError, TiffFormatError};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArcAsciiGrid {
    pub ncols: usize,
    pub nrows: usize,
    pub xllcorner: f64,
    pub yllcorner: f64,
    pub cellsize: f64,
    pub nodata_value: f64,
    pub confidence: f64,
    pub provenance: Provenance,
    values: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeoTiffDem {
    pub width: usize,
    pub height: usize,
    pub crs: RasterCrs,
    #[serde(alias = "origin_lon")]
    pub origin_x: f64,
    #[serde(alias = "origin_lat")]
    pub origin_y: f64,
    #[serde(alias = "pixel_width_deg")]
    pub pixel_width: f64,
    #[serde(alias = "pixel_height_deg")]
    pub pixel_height: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<RasterTransform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodata_value: Option<f64>,
    pub confidence: f64,
    pub provenance: Provenance,
    values: Vec<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RasterTransform {
    pub x0: f64,
    pub y0: f64,
    pub dx_col: f64,
    pub dy_col: f64,
    pub dx_row: f64,
    pub dy_row: f64,
}

impl RasterTransform {
    fn north_up(origin_x: f64, origin_y: f64, pixel_width: f64, pixel_height: f64) -> Self {
        Self {
            x0: origin_x,
            y0: origin_y,
            dx_col: pixel_width,
            dy_col: 0.0,
            dx_row: 0.0,
            dy_row: -pixel_height,
        }
    }

    fn from_model_transformation(xs: &[f64]) -> Result<Self> {
        if xs.len() < 16 {
            return Err(TrailgenError::InvalidData(
                "GeoTIFF ModelTransformationTag must contain sixteen numbers".to_owned(),
            ));
        }
        let transform = Self {
            x0: xs[3],
            y0: xs[7],
            dx_col: xs[0],
            dy_col: xs[4],
            dx_row: xs[1],
            dy_row: xs[5],
        };
        transform.validate("GeoTIFF ModelTransformationTag")?;
        Ok(transform)
    }

    fn validate(self, label: &str) -> Result<()> {
        let finite = [
            self.x0,
            self.y0,
            self.dx_col,
            self.dy_col,
            self.dx_row,
            self.dy_row,
        ]
        .into_iter()
        .all(f64::is_finite);
        if !finite || self.det().abs() <= f64::EPSILON {
            return Err(TrailgenError::InvalidData(format!(
                "{label} must be finite and invertible"
            )));
        }
        Ok(())
    }

    fn pixel_xy(self, x: f64, y: f64) -> (f64, f64) {
        let dx = x - self.x0;
        let dy = y - self.y0;
        let det = self.det();
        (
            (dx.mul_add(self.dy_row, -self.dx_row * dy)) / det,
            (self.dx_col.mul_add(dy, -dx * self.dy_col)) / det,
        )
    }

    const fn det(self) -> f64 {
        self.dx_col * self.dy_row - self.dx_row * self.dy_col
    }

    fn pixel_width(self) -> f64 {
        self.dx_col.hypot(self.dy_col)
    }

    fn pixel_height(self) -> f64 {
        self.dx_row.hypot(self.dy_row)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RasterCrs {
    Wgs84Degrees,
    Nad83Degrees,
    WebMercatorMeters,
    #[serde(alias = "wgs84-utm-meters")]
    UtmMeters(UtmCrs),
}

impl RasterCrs {
    fn xy(self, coord: Coord) -> Option<(f64, f64)> {
        match self {
            Self::Wgs84Degrees | Self::Nad83Degrees => Some((coord.lon, coord.lat)),
            Self::WebMercatorMeters => Some(wgs84_to_web_mercator(coord)),
            Self::UtmMeters(crs) => geographic_to_utm(coord, crs),
        }
    }
}

const fn default_raster_crs() -> RasterCrs {
    RasterCrs::Wgs84Degrees
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VrtDem {
    pub width: usize,
    pub height: usize,
    #[serde(default = "default_raster_crs")]
    pub crs: RasterCrs,
    #[serde(alias = "origin_lon")]
    pub origin_x: f64,
    #[serde(alias = "origin_lat")]
    pub origin_y: f64,
    #[serde(alias = "pixel_width_deg")]
    pub pixel_width: f64,
    #[serde(alias = "pixel_height_deg")]
    pub pixel_height: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<RasterTransform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodata_value: Option<f64>,
    pub source_filename: String,
    pub confidence: f64,
    pub provenance: Provenance,
    values: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RasterDem {
    ArcAscii(ArcAsciiGrid),
    GeoTiff(GeoTiffDem),
    Vrt(VrtDem),
}

impl ArcAsciiGrid {
    pub fn parse(raw: &str, provenance: Provenance, confidence: f64) -> Result<Self> {
        let mut lines = raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .peekable();
        let ncols: usize = header(&mut lines, "ncols")?;
        let nrows: usize = header(&mut lines, "nrows")?;
        let xllcorner: f64 = header(&mut lines, "xllcorner")?;
        let yllcorner: f64 = header(&mut lines, "yllcorner")?;
        let cellsize: f64 = header(&mut lines, "cellsize")?;
        let nodata_value: f64 = optional_header(&mut lines, "nodata_value")?.unwrap_or(-9999.0);
        if ncols == 0 || nrows == 0 {
            return Err(TrailgenError::InvalidData(
                "raster dimensions must be nonzero".to_owned(),
            ));
        }
        if !xllcorner.is_finite()
            || !yllcorner.is_finite()
            || !cellsize.is_finite()
            || cellsize <= 0.0
            || !nodata_value.is_finite()
        {
            return Err(TrailgenError::InvalidData(
                "raster georeference headers must be finite and cellsize positive".to_owned(),
            ));
        }
        let values = lines
            .flat_map(str::split_whitespace)
            .map(|x| {
                x.parse::<f64>()
                    .map_err(|e| TrailgenError::InvalidData(format!("invalid raster cell: {e}")))
            })
            .collect::<Result<Vec<_>>>()?;
        let expected = ncols
            .checked_mul(nrows)
            .ok_or_else(|| TrailgenError::InvalidData("raster dimensions overflow".to_owned()))?;
        if values.len() != expected {
            return Err(TrailgenError::InvalidData(format!(
                "raster has {} cells, expected {expected}",
                values.len()
            )));
        }
        Ok(Self {
            ncols,
            nrows,
            xllcorner,
            yllcorner,
            cellsize,
            nodata_value,
            confidence: confidence.clamp(0.0, 1.0),
            provenance,
            values,
        })
    }

    #[must_use]
    pub fn contains(&self, coord: Coord) -> bool {
        let xmax = self
            .cellsize
            .mul_add(usize_to_f64(self.ncols), self.xllcorner);
        let ymax = self
            .cellsize
            .mul_add(usize_to_f64(self.nrows), self.yllcorner);
        (self.xllcorner..=xmax).contains(&coord.lon) && (self.yllcorner..=ymax).contains(&coord.lat)
    }

    fn interpolated_elevation_m(&self, coord: Coord) -> Option<f64> {
        if !self.contains(coord) {
            return None;
        }
        let col = self.x_center_index(coord.lon);
        let row = self.y_center_index(coord.lat);
        let col0 = f64_to_index(col.floor(), self.ncols)?;
        let col1 = f64_to_index(col.ceil(), self.ncols)?;
        let row0 = f64_to_index(row.floor(), self.nrows)?;
        let row1 = f64_to_index(row.ceil(), self.nrows)?;
        let tx = col - usize_to_f64(col0);
        let ty = row - usize_to_f64(row0);
        let z00 = self.cell(row0, col0)?;
        let z01 = self.cell(row0, col1)?;
        let z10 = self.cell(row1, col0)?;
        let z11 = self.cell(row1, col1)?;
        let top = tx.mul_add(z01 - z00, z00);
        let bottom = tx.mul_add(z11 - z10, z10);
        Some(ty.mul_add(bottom - top, top))
    }

    fn x_center_index(&self, lon: f64) -> f64 {
        ((lon - self.xllcorner) / self.cellsize - 0.5)
            .clamp(0.0, usize_to_f64(self.ncols.saturating_sub(1)))
    }

    fn y_center_index(&self, lat: f64) -> f64 {
        let ymax = self
            .cellsize
            .mul_add(usize_to_f64(self.nrows), self.yllcorner);
        ((ymax - lat) / self.cellsize - 0.5).clamp(0.0, usize_to_f64(self.nrows.saturating_sub(1)))
    }

    fn cell(&self, row: usize, col: usize) -> Option<f64> {
        let value = *self
            .values
            .get(row.checked_mul(self.ncols)?.checked_add(col)?)?;
        if (value - self.nodata_value).abs() <= f64::EPSILON {
            return None;
        }
        Some(value)
    }
}

impl GeoTiffDem {
    pub fn from_path(path: &Path, provenance: Provenance, confidence: f64) -> Result<Self> {
        let file = File::open(path)
            .map_err(|error| TrailgenError::InvalidData(format!("open GeoTIFF: {error}")))?;
        let mut decoder = Decoder::new(file)
            .map_err(|error| TrailgenError::InvalidData(format!("decode GeoTIFF: {error}")))?;
        let (width, height) = decoder.dimensions().map_err(|error| {
            TrailgenError::InvalidData(format!("read GeoTIFF dimensions: {error}"))
        })?;
        let width = usize::try_from(width)
            .map_err(|_| TrailgenError::InvalidData("GeoTIFF width overflow".to_owned()))?;
        let height = usize::try_from(height)
            .map_err(|_| TrailgenError::InvalidData("GeoTIFF height overflow".to_owned()))?;
        let expected = width
            .checked_mul(height)
            .ok_or_else(|| TrailgenError::InvalidData("GeoTIFF dimensions overflow".to_owned()))?;
        let georef = GeoTiffGeoref::read(&mut decoder)?;
        let nodata_value = optional_ascii(&mut decoder, Tag::GdalNodata)?
            .and_then(|value| value.trim_matches('\0').trim().parse::<f64>().ok());
        let image = decoder
            .read_image()
            .map_err(|error| TrailgenError::InvalidData(format!("read GeoTIFF pixels: {error}")))?;
        let values = decoding_result_to_f64(image);
        if values.len() != expected {
            return Err(TrailgenError::UnsupportedFormat(format!(
                "GeoTIFF DEM must be single-band; decoded {} sample(s), expected {expected}",
                values.len()
            )));
        }
        Ok(Self {
            width,
            height,
            crs: georef.crs,
            origin_x: georef.origin_x,
            origin_y: georef.origin_y,
            pixel_width: georef.pixel_width,
            pixel_height: georef.pixel_height,
            transform: georef.transform,
            nodata_value,
            confidence: confidence.clamp(0.0, 1.0),
            provenance,
            values,
        })
    }

    #[must_use]
    pub fn contains(&self, coord: Coord) -> bool {
        raster_contains(self.width, self.height, self.crs, self.transform(), coord)
    }

    fn interpolated_elevation_m(&self, coord: Coord) -> Option<f64> {
        interpolated_raster_value(
            self.width,
            self.height,
            &self.values,
            self.nodata_value,
            self.crs,
            self.transform(),
            coord,
        )
    }

    fn transform(&self) -> RasterTransform {
        self.transform.unwrap_or_else(|| {
            RasterTransform::north_up(
                self.origin_x,
                self.origin_y,
                self.pixel_width,
                self.pixel_height,
            )
        })
    }
}

impl VrtDem {
    pub fn from_path(path: &Path, provenance: Provenance, confidence: f64) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|error| TrailgenError::InvalidData(format!("read VRT: {error}")))?;
        let spec = VrtSpec::parse(path, &raw)?;
        let source = GeoTiffDem::from_path(
            &spec.source_path,
            Provenance {
                source: "vrt-source-geotiff".to_owned(),
                layer: Some("vrt-source".to_owned()),
                source_id: spec
                    .source_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned),
                license: None,
            },
            confidence,
        )?;
        let crs = spec.crs.unwrap_or(source.crs);
        if spec.crs.is_some_and(|spec_crs| spec_crs != source.crs) {
            return Err(TrailgenError::UnsupportedFormat(format!(
                "VRT SRS {:?} does not match source GeoTIFF CRS {:?}; reproject or materialize a consistent VRT",
                crs, source.crs
            )));
        }
        if source.width != spec.width || source.height != spec.height {
            return Err(TrailgenError::UnsupportedFormat(format!(
                "VRT source raster dimensions {}x{} do not match VRT {}x{}",
                source.width, source.height, spec.width, spec.height
            )));
        }
        Ok(Self {
            width: spec.width,
            height: spec.height,
            crs,
            origin_x: spec.transform.x0,
            origin_y: spec.transform.y0,
            pixel_width: spec.transform.pixel_width(),
            pixel_height: spec.transform.pixel_height(),
            transform: Some(spec.transform),
            nodata_value: spec.nodata_value.or(source.nodata_value),
            source_filename: spec.source_path.display().to_string(),
            confidence: confidence.clamp(0.0, 1.0),
            provenance,
            values: source.values,
        })
    }

    pub fn referenced_sources(path: &Path) -> Result<Vec<PathBuf>> {
        let raw = std::fs::read_to_string(path)
            .map_err(|error| TrailgenError::InvalidData(format!("read VRT: {error}")))?;
        Ok(vec![VrtSpec::parse(path, &raw)?.source_path])
    }

    #[must_use]
    pub fn contains(&self, coord: Coord) -> bool {
        raster_contains(self.width, self.height, self.crs, self.transform(), coord)
    }

    fn interpolated_elevation_m(&self, coord: Coord) -> Option<f64> {
        interpolated_raster_value(
            self.width,
            self.height,
            &self.values,
            self.nodata_value,
            self.crs,
            self.transform(),
            coord,
        )
    }

    fn transform(&self) -> RasterTransform {
        self.transform.unwrap_or_else(|| {
            RasterTransform::north_up(
                self.origin_x,
                self.origin_y,
                self.pixel_width,
                self.pixel_height,
            )
        })
    }
}

impl ElevationSampler for ArcAsciiGrid {
    fn sample(&self, coord: Coord) -> Option<ElevationSample> {
        let value = self.interpolated_elevation_m(coord)?;
        Some(ElevationSample {
            ele_m: value,
            confidence: self.confidence,
            provenance: self.provenance.clone(),
        })
    }
}

impl ElevationSampler for RasterDem {
    fn sample(&self, coord: Coord) -> Option<ElevationSample> {
        match self {
            Self::ArcAscii(raster) => raster.sample(coord),
            Self::GeoTiff(raster) => raster.sample(coord),
            Self::Vrt(raster) => raster.sample(coord),
        }
    }
}

impl ElevationSampler for GeoTiffDem {
    fn sample(&self, coord: Coord) -> Option<ElevationSample> {
        let value = self.interpolated_elevation_m(coord)?;
        Some(ElevationSample {
            ele_m: value,
            confidence: self.confidence,
            provenance: self.provenance.clone(),
        })
    }
}

impl ElevationSampler for VrtDem {
    fn sample(&self, coord: Coord) -> Option<ElevationSample> {
        let value = self.interpolated_elevation_m(coord)?;
        Some(ElevationSample {
            ele_m: value,
            confidence: self.confidence,
            provenance: self.provenance.clone(),
        })
    }
}

struct VrtSpec {
    width: usize,
    height: usize,
    crs: Option<RasterCrs>,
    transform: RasterTransform,
    nodata_value: Option<f64>,
    source_path: PathBuf,
}

impl VrtSpec {
    fn parse(path: &Path, raw: &str) -> Result<Self> {
        let doc = roxmltree::Document::parse(raw)
            .map_err(|error| TrailgenError::InvalidData(format!("parse VRT XML: {error}")))?;
        let root = doc.root_element();
        if root.tag_name().name() != "VRTDataset" {
            return Err(TrailgenError::UnsupportedFormat(
                "raster VRT must have <VRTDataset> root".to_owned(),
            ));
        }
        let width = required_attr::<usize>(root, "rasterXSize")?;
        let height = required_attr::<usize>(root, "rasterYSize")?;
        if width == 0 || height == 0 {
            return Err(TrailgenError::InvalidData(
                "VRT raster dimensions must be nonzero".to_owned(),
            ));
        }
        let geotransform = child_text(root, "GeoTransform")
            .ok_or_else(|| TrailgenError::InvalidData("VRT missing GeoTransform".to_owned()))?
            .split(',')
            .map(str::trim)
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                TrailgenError::InvalidData(format!("invalid VRT GeoTransform: {error}"))
            })?;
        if geotransform.len() != 6 {
            return Err(TrailgenError::InvalidData(
                "VRT GeoTransform must contain six numbers".to_owned(),
            ));
        }
        let transform = RasterTransform {
            x0: geotransform[0],
            y0: geotransform[3],
            dx_col: geotransform[1],
            dy_col: geotransform[4],
            dx_row: geotransform[2],
            dy_row: geotransform[5],
        };
        transform.validate("VRT GeoTransform")?;
        let crs = child_text(root, "SRS").map(parse_vrt_srs).transpose()?;
        let band = root
            .children()
            .find(|node| node.has_tag_name("VRTRasterBand"))
            .ok_or_else(|| TrailgenError::InvalidData("VRT missing VRTRasterBand".to_owned()))?;
        let simple = band
            .children()
            .find(|node| node.has_tag_name("SimpleSource"))
            .ok_or_else(|| {
                TrailgenError::UnsupportedFormat("VRT DEM requires SimpleSource".to_owned())
            })?;
        ensure_identity_rect(simple, "SrcRect", width, height)?;
        ensure_identity_rect(simple, "DstRect", width, height)?;
        let source_filename = simple
            .children()
            .find(|node| node.has_tag_name("SourceFilename"))
            .ok_or_else(|| {
                TrailgenError::InvalidData("VRT SimpleSource missing SourceFilename".to_owned())
            })?;
        let source_text = source_filename
            .text()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| TrailgenError::InvalidData("VRT SourceFilename is empty".to_owned()))?;
        let source_path = resolve_vrt_source(path, source_filename, source_text);
        let source_band = child_text(simple, "SourceBand").unwrap_or("1").trim();
        if source_band != "1" {
            return Err(TrailgenError::UnsupportedFormat(
                "VRT DEM only supports SourceBand 1".to_owned(),
            ));
        }
        let nodata_value = child_text(band, "NoDataValue")
            .or_else(|| child_text(root, "NoDataValue"))
            .map(str::trim)
            .map(str::parse::<f64>)
            .transpose()
            .map_err(|error| {
                TrailgenError::InvalidData(format!("invalid VRT NoDataValue: {error}"))
            })?;
        Ok(Self {
            width,
            height,
            crs,
            transform,
            nodata_value,
            source_path,
        })
    }
}

fn raster_contains(
    width: usize,
    height: usize,
    crs: RasterCrs,
    transform: RasterTransform,
    coord: Coord,
) -> bool {
    let Some((x, y)) = crs.xy(coord) else {
        return false;
    };
    let (col, row) = transform.pixel_xy(x, y);
    (0.0..=usize_to_f64(width)).contains(&col) && (0.0..=usize_to_f64(height)).contains(&row)
}

fn interpolated_raster_value(
    width: usize,
    height: usize,
    values: &[f64],
    nodata_value: Option<f64>,
    crs: RasterCrs,
    transform: RasterTransform,
    coord: Coord,
) -> Option<f64> {
    if !raster_contains(width, height, crs, transform, coord) {
        return None;
    }
    let (x, y) = crs.xy(coord)?;
    let (col_px, row_px) = transform.pixel_xy(x, y);
    let col = (col_px - 0.5).clamp(0.0, usize_to_f64(width.saturating_sub(1)));
    let row = (row_px - 0.5).clamp(0.0, usize_to_f64(height.saturating_sub(1)));
    let col0 = f64_to_index(col.floor(), width)?;
    let col1 = f64_to_index(col.ceil(), width)?;
    let row0 = f64_to_index(row.floor(), height)?;
    let row1 = f64_to_index(row.ceil(), height)?;
    let tx = col - usize_to_f64(col0);
    let ty = row - usize_to_f64(row0);
    let z00 = raster_cell(width, values, nodata_value, row0, col0)?;
    let z01 = raster_cell(width, values, nodata_value, row0, col1)?;
    let z10 = raster_cell(width, values, nodata_value, row1, col0)?;
    let z11 = raster_cell(width, values, nodata_value, row1, col1)?;
    let top = tx.mul_add(z01 - z00, z00);
    let bottom = tx.mul_add(z11 - z10, z10);
    Some(ty.mul_add(bottom - top, top))
}

fn raster_cell(
    width: usize,
    values: &[f64],
    nodata_value: Option<f64>,
    row: usize,
    col: usize,
) -> Option<f64> {
    let value = *values.get(row.checked_mul(width)?.checked_add(col)?)?;
    if nodata_value.is_some_and(|nodata| (value - nodata).abs() <= f64::EPSILON) {
        return None;
    }
    value.is_finite().then_some(value)
}

fn parse_vrt_srs(srs: &str) -> Result<RasterCrs> {
    let normalized: String = srs
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_uppercase)
        .collect();
    if normalized.contains("EPSG3857")
        || normalized.contains("EPSG900913")
        || normalized.contains("WEBMERCATOR")
        || normalized.contains("PSEUDOMERCATOR")
    {
        Ok(RasterCrs::WebMercatorMeters)
    } else if let Some(crs) = UtmCrs::from_normalized_srs(&normalized) {
        Ok(RasterCrs::UtmMeters(crs))
    } else if normalized.contains("PROJCS")
        || normalized.contains("PROJCRS")
        || normalized.contains("PROJECTION")
    {
        Err(TrailgenError::UnsupportedFormat(
            "projected VRT SRS must be EPSG:3857 Web Mercator or WGS84/NAD83 UTM (EPSG:326xx/327xx/269xx), or reprojected before ingestion"
                .to_owned(),
        ))
    } else if normalized.contains("EPSG4326")
        || normalized.contains("OGC13CRS84")
        || normalized.contains("OGC14CRS84")
        || normalized.contains("CRS84")
        || normalized.contains("WGS84")
        || normalized.contains("WGS1984")
    {
        Ok(RasterCrs::Wgs84Degrees)
    } else if normalized.contains("EPSG4269")
        || normalized.contains("NAD83")
        || normalized.contains("NORTHAMERICANDATUM1983")
    {
        Ok(RasterCrs::Nad83Degrees)
    } else {
        Err(TrailgenError::UnsupportedFormat(
            "VRT SRS must declare WGS84/NAD83/CRS84 geographic, EPSG:3857 Web Mercator, or WGS84/NAD83 UTM (EPSG:326xx/327xx/269xx)"
                .to_owned(),
        ))
    }
}

fn required_attr<T>(node: roxmltree::Node<'_, '_>, key: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
    T: FromStr,
{
    node.attribute(key)
        .ok_or_else(|| TrailgenError::InvalidData(format!("VRT missing {key} attribute")))?
        .parse()
        .map_err(|error| TrailgenError::InvalidData(format!("invalid VRT {key}: {error}")))
}

fn child_text<'a>(node: roxmltree::Node<'a, 'a>, tag: &str) -> Option<&'a str> {
    node.children()
        .find(|child| child.has_tag_name(tag))
        .and_then(|child| child.text())
}

fn ensure_identity_rect(
    simple: roxmltree::Node<'_, '_>,
    tag: &str,
    width: usize,
    height: usize,
) -> Result<()> {
    let Some(rect) = simple.children().find(|node| node.has_tag_name(tag)) else {
        return Ok(());
    };
    let x_off = required_attr::<usize>(rect, "xOff")?;
    let y_off = required_attr::<usize>(rect, "yOff")?;
    let x_size = required_attr::<usize>(rect, "xSize")?;
    let y_size = required_attr::<usize>(rect, "ySize")?;
    if x_off == 0 && y_off == 0 && x_size == width && y_size == height {
        Ok(())
    } else {
        Err(TrailgenError::UnsupportedFormat(format!(
            "VRT DEM only supports full-raster identity {tag}"
        )))
    }
}

fn resolve_vrt_source(
    vrt_path: &Path,
    source_filename: roxmltree::Node<'_, '_>,
    source_text: &str,
) -> PathBuf {
    let path = PathBuf::from(source_text);
    if path.is_absolute() {
        return path;
    }
    let relative = source_filename
        .attribute("relativeToVRT")
        .is_some_and(|value| matches!(value, "1" | "true" | "TRUE" | "True"));
    if relative || vrt_path.parent().is_some() {
        vrt_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    } else {
        path
    }
}

#[derive(Clone, Copy)]
struct GeoTiffGeoref {
    crs: RasterCrs,
    origin_x: f64,
    origin_y: f64,
    pixel_width: f64,
    pixel_height: f64,
    transform: Option<RasterTransform>,
}

impl GeoTiffGeoref {
    fn read<R: std::io::Read + std::io::Seek>(decoder: &mut Decoder<R>) -> Result<Self> {
        let crs = validate_geokeys(decoder)?;
        if let Some(xs) = optional_f64_vec(decoder, Tag::ModelTransformationTag)? {
            let transform = RasterTransform::from_model_transformation(&xs)?;
            return Ok(Self {
                crs,
                origin_x: transform.x0,
                origin_y: transform.y0,
                pixel_width: transform.pixel_width(),
                pixel_height: transform.pixel_height(),
                transform: Some(transform),
            });
        }
        let scale = required_f64_vec(decoder, Tag::ModelPixelScaleTag)?;
        let tiepoint = required_f64_vec(decoder, Tag::ModelTiepointTag)?;
        if scale.len() < 2 || tiepoint.len() < 6 {
            return Err(TrailgenError::InvalidData(
                "GeoTIFF DEM requires ModelPixelScaleTag[0..2] and ModelTiepointTag[0..6]"
                    .to_owned(),
            ));
        }
        let pixel_width = scale[0].abs();
        let pixel_height = scale[1].abs();
        if !pixel_width.is_finite()
            || !pixel_height.is_finite()
            || pixel_width <= 0.0
            || pixel_height <= 0.0
        {
            return Err(TrailgenError::InvalidData(
                "GeoTIFF DEM pixel scale must be finite and positive".to_owned(),
            ));
        }
        let origin_x = tiepoint[0].mul_add(-pixel_width, tiepoint[3]);
        let origin_y = tiepoint[1].mul_add(pixel_height, tiepoint[4]);
        if !origin_x.is_finite() || !origin_y.is_finite() {
            return Err(TrailgenError::InvalidData(
                "GeoTIFF DEM tiepoint georeference must be finite".to_owned(),
            ));
        }
        Ok(Self {
            crs,
            origin_x,
            origin_y,
            pixel_width,
            pixel_height,
            transform: None,
        })
    }
}

fn validate_geokeys<R: std::io::Read + std::io::Seek>(
    decoder: &mut Decoder<R>,
) -> Result<RasterCrs> {
    let Some(keys) = optional_u16_vec(decoder, Tag::GeoKeyDirectoryTag)? else {
        return Err(TrailgenError::InvalidData(
            "GeoTIFF DEM requires GeoKeyDirectoryTag declaring WGS84/NAD83 geographic, EPSG:3857 Web Mercator, or WGS84/NAD83 UTM (EPSG:326xx/327xx/269xx)"
                .to_owned(),
        ));
    };
    if keys.len() < 4 {
        return Err(TrailgenError::InvalidData(
            "GeoTIFF GeoKeyDirectoryTag header is truncated".to_owned(),
        ));
    }
    let count = usize::from(keys[3]);
    if keys.len() < 4 + count.saturating_mul(4) {
        return Err(TrailgenError::InvalidData(
            "GeoTIFF GeoKeyDirectoryTag entries are truncated".to_owned(),
        ));
    }
    let mut model_type = None;
    let mut angular_units = None;
    let mut geographic_crs = None;
    let mut projected_crs = None;
    let mut linear_units = None;
    for entry in keys[4..][..count * 4].chunks_exact(4) {
        match entry[0] {
            1024 if entry[1] == 0 => model_type = Some(entry[3]),
            2048 if entry[1] == 0 => geographic_crs = Some(entry[3]),
            2054 if entry[1] == 0 => angular_units = Some(entry[3]),
            3072 if entry[1] == 0 => projected_crs = Some(entry[3]),
            3076 if entry[1] == 0 => linear_units = Some(entry[3]),
            _ => {}
        }
    }
    if model_type == Some(1) {
        if let Some(crs) = projected_crs.and_then(projected_raster_crs) {
            if linear_units.is_some_and(|unit| unit != 9001) {
                return Err(TrailgenError::UnsupportedFormat(
                    "projected GeoTIFF DEM linear units must be metres".to_owned(),
                ));
            }
            return Ok(crs);
        }
        return Err(TrailgenError::UnsupportedFormat(
            "projected GeoTIFF DEM must be EPSG:3857 Web Mercator or WGS84/NAD83 UTM (EPSG:326xx/327xx/269xx), or reprojected before ingestion"
                .to_owned(),
        ));
    }
    if model_type == Some(2) {
        if angular_units.is_some_and(|unit| unit != 9102) {
            return Err(TrailgenError::UnsupportedFormat(
                "GeoTIFF DEM angular units must be degrees".to_owned(),
            ));
        }
        return geographic_crs.map_or(Ok(RasterCrs::Wgs84Degrees), geographic_raster_crs);
    }
    Err(TrailgenError::UnsupportedFormat(
        "GeoTIFF DEM must declare GTModelTypeGeoKey=geographic, EPSG:3857 projected, or WGS84/NAD83 UTM projected".to_owned(),
    ))
}

fn geographic_raster_crs(epsg: u16) -> Result<RasterCrs> {
    match epsg {
        4326 => Ok(RasterCrs::Wgs84Degrees),
        4269 => Ok(RasterCrs::Nad83Degrees),
        _ => Err(TrailgenError::UnsupportedFormat(
            "GeoTIFF DEM geographic CRS must be WGS84 EPSG:4326 or NAD83 EPSG:4269".to_owned(),
        )),
    }
}

fn projected_raster_crs(epsg: u16) -> Option<RasterCrs> {
    match epsg {
        3857 => Some(RasterCrs::WebMercatorMeters),
        32601..=32760 | 26901..=26923 => Some(RasterCrs::UtmMeters(UtmCrs::from_epsg(epsg)?)),
        _ => None,
    }
}

fn decoding_result_to_f64(image: DecodingResult) -> Vec<f64> {
    match image {
        DecodingResult::U8(xs) => xs.into_iter().map(f64::from).collect(),
        DecodingResult::U16(xs) => xs.into_iter().map(f64::from).collect(),
        DecodingResult::U32(xs) => xs.into_iter().map(f64::from).collect(),
        DecodingResult::U64(xs) => xs.into_iter().map(|x| parse_numeric(&x)).collect(),
        DecodingResult::F16(xs) => xs.into_iter().map(|x| f64::from(x.to_f32())).collect(),
        DecodingResult::F32(xs) => xs.into_iter().map(f64::from).collect(),
        DecodingResult::F64(xs) => xs,
        DecodingResult::I8(xs) => xs.into_iter().map(f64::from).collect(),
        DecodingResult::I16(xs) => xs.into_iter().map(f64::from).collect(),
        DecodingResult::I32(xs) => xs.into_iter().map(f64::from).collect(),
        DecodingResult::I64(xs) => xs.into_iter().map(|x| parse_numeric(&x)).collect(),
    }
}

fn required_f64_vec<R: std::io::Read + std::io::Seek>(
    decoder: &mut Decoder<R>,
    tag: Tag,
) -> Result<Vec<f64>> {
    optional_f64_vec(decoder, tag)?
        .ok_or_else(|| TrailgenError::InvalidData(format!("GeoTIFF DEM missing {tag:?}")))
}

fn optional_f64_vec<R: std::io::Read + std::io::Seek>(
    decoder: &mut Decoder<R>,
    tag: Tag,
) -> Result<Option<Vec<f64>>> {
    match decoder.get_tag(tag) {
        Ok(value) => value
            .into_f64_vec()
            .map(Some)
            .map_err(|error| TrailgenError::InvalidData(format!("read GeoTIFF {tag:?}: {error}"))),
        Err(error) if missing_tag(&error, tag) => Ok(None),
        Err(error) => Err(TrailgenError::InvalidData(format!(
            "read GeoTIFF {tag:?}: {error}"
        ))),
    }
}

fn optional_u16_vec<R: std::io::Read + std::io::Seek>(
    decoder: &mut Decoder<R>,
    tag: Tag,
) -> Result<Option<Vec<u16>>> {
    match decoder.get_tag(tag) {
        Ok(value) => value
            .into_u16_vec()
            .map(Some)
            .map_err(|error| TrailgenError::InvalidData(format!("read GeoTIFF {tag:?}: {error}"))),
        Err(error) if missing_tag(&error, tag) => Ok(None),
        Err(error) => Err(TrailgenError::InvalidData(format!(
            "read GeoTIFF {tag:?}: {error}"
        ))),
    }
}

fn optional_ascii<R: std::io::Read + std::io::Seek>(
    decoder: &mut Decoder<R>,
    tag: Tag,
) -> Result<Option<String>> {
    match decoder.get_tag_ascii_string(tag) {
        Ok(value) => Ok(Some(value)),
        Err(error) if missing_tag(&error, tag) => Ok(None),
        Err(error) => Err(TrailgenError::InvalidData(format!(
            "read GeoTIFF {tag:?}: {error}"
        ))),
    }
}

fn missing_tag(error: &TiffError, tag: Tag) -> bool {
    matches!(
        error,
        TiffError::FormatError(TiffFormatError::RequiredTagNotFound(missing)) if *missing == tag
    )
}

fn header<'a, I, T>(lines: &mut I, key: &str) -> Result<T>
where
    I: Iterator<Item = &'a str>,
    T::Err: std::fmt::Display,
    T: FromStr,
{
    let line = lines
        .next()
        .ok_or_else(|| TrailgenError::InvalidData(format!("raster missing {key} header")))?;
    let (actual, value) = split_header(line)?;
    if !actual.eq_ignore_ascii_case(key) {
        return Err(TrailgenError::InvalidData(format!(
            "raster expected {key} header, found {actual}"
        )));
    }
    value
        .parse()
        .map_err(|e| TrailgenError::InvalidData(format!("invalid raster {key}: {e}")))
}

fn optional_header<'a, I, T>(lines: &mut Peekable<I>, key: &str) -> Result<Option<T>>
where
    I: Iterator<Item = &'a str>,
    T::Err: std::fmt::Display,
    T: FromStr,
{
    let Some(line) = lines.peek() else {
        return Ok(None);
    };
    let (actual, _) = split_header(line)?;
    if !actual.eq_ignore_ascii_case(key) {
        return Ok(None);
    }
    let Some(line) = lines.next() else {
        return Ok(None);
    };
    let (_, value) = split_header(line)?;
    value
        .parse()
        .map(Some)
        .map_err(|e| TrailgenError::InvalidData(format!("invalid raster {key}: {e}")))
}

fn split_header(line: &str) -> Result<(&str, &str)> {
    let mut parts = line.split_whitespace();
    let key = parts
        .next()
        .ok_or_else(|| TrailgenError::InvalidData("empty raster header".to_owned()))?;
    let value = parts
        .next()
        .ok_or_else(|| TrailgenError::InvalidData(format!("raster header {key} has no value")))?;
    Ok((key, value))
}

fn f64_to_index(value: f64, upper: usize) -> Option<usize> {
    if value.is_nan() || value < 0.0 {
        return None;
    }
    let floored = value.floor().min(usize_to_f64(upper.saturating_sub(1)));
    floored.to_string().parse::<usize>().ok()
}

fn usize_to_f64(value: usize) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::INFINITY)
}

fn parse_numeric<T: ToString + ?Sized>(value: &T) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::NAN)
}
