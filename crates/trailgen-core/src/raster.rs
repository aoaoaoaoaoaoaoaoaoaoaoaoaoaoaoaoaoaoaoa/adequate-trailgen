use crate::enrich::{ElevationSample, ElevationSampler};
use crate::geo::Coord;
use crate::model::Provenance;
use crate::{Result, TrailgenError};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::iter::Peekable;
use std::path::Path;
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
    pub origin_lon: f64,
    pub origin_lat: f64,
    pub pixel_width_deg: f64,
    pub pixel_height_deg: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodata_value: Option<f64>,
    pub confidence: f64,
    pub provenance: Provenance,
    values: Vec<f64>,
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
            .map_err(|error| TrailgenError::InvalidData(format!("decode GeoTIFF: {error}")))?
            .with_limits(tiff::decoder::Limits::unlimited());
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
            origin_lon: georef.origin_lon,
            origin_lat: georef.origin_lat,
            pixel_width_deg: georef.pixel_width_deg,
            pixel_height_deg: georef.pixel_height_deg,
            nodata_value,
            confidence: confidence.clamp(0.0, 1.0),
            provenance,
            values,
        })
    }

    #[must_use]
    pub fn contains(&self, coord: Coord) -> bool {
        let east = self
            .pixel_width_deg
            .mul_add(usize_to_f64(self.width), self.origin_lon);
        let south = self
            .pixel_height_deg
            .mul_add(-usize_to_f64(self.height), self.origin_lat);
        (self.origin_lon..=east).contains(&coord.lon)
            && (south..=self.origin_lat).contains(&coord.lat)
    }

    fn interpolated_elevation_m(&self, coord: Coord) -> Option<f64> {
        if !self.contains(coord) {
            return None;
        }
        let col = ((coord.lon - self.origin_lon) / self.pixel_width_deg - 0.5)
            .clamp(0.0, usize_to_f64(self.width.saturating_sub(1)));
        let row = ((self.origin_lat - coord.lat) / self.pixel_height_deg - 0.5)
            .clamp(0.0, usize_to_f64(self.height.saturating_sub(1)));
        let col0 = f64_to_index(col.floor(), self.width)?;
        let col1 = f64_to_index(col.ceil(), self.width)?;
        let row0 = f64_to_index(row.floor(), self.height)?;
        let row1 = f64_to_index(row.ceil(), self.height)?;
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

    fn cell(&self, row: usize, col: usize) -> Option<f64> {
        let value = *self
            .values
            .get(row.checked_mul(self.width)?.checked_add(col)?)?;
        if self
            .nodata_value
            .is_some_and(|nodata| (value - nodata).abs() <= f64::EPSILON)
        {
            return None;
        }
        value.is_finite().then_some(value)
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

#[derive(Clone, Copy)]
struct GeoTiffGeoref {
    origin_lon: f64,
    origin_lat: f64,
    pixel_width_deg: f64,
    pixel_height_deg: f64,
}

impl GeoTiffGeoref {
    fn read<R: std::io::Read + std::io::Seek>(decoder: &mut Decoder<R>) -> Result<Self> {
        validate_geokeys(decoder)?;
        if optional_f64_vec(decoder, Tag::ModelTransformationTag)?.is_some() {
            return Err(TrailgenError::UnsupportedFormat(
                "GeoTIFF DEM with ModelTransformationTag rotation/shear is unsupported".to_owned(),
            ));
        }
        let scale = required_f64_vec(decoder, Tag::ModelPixelScaleTag)?;
        let tiepoint = required_f64_vec(decoder, Tag::ModelTiepointTag)?;
        if scale.len() < 2 || tiepoint.len() < 6 {
            return Err(TrailgenError::InvalidData(
                "GeoTIFF DEM requires ModelPixelScaleTag[0..2] and ModelTiepointTag[0..6]"
                    .to_owned(),
            ));
        }
        let pixel_width_deg = scale[0].abs();
        let pixel_height_deg = scale[1].abs();
        if !pixel_width_deg.is_finite()
            || !pixel_height_deg.is_finite()
            || pixel_width_deg <= 0.0
            || pixel_height_deg <= 0.0
        {
            return Err(TrailgenError::InvalidData(
                "GeoTIFF DEM pixel scale must be finite and positive".to_owned(),
            ));
        }
        let origin_lon = tiepoint[0].mul_add(-pixel_width_deg, tiepoint[3]);
        let origin_lat = tiepoint[1].mul_add(pixel_height_deg, tiepoint[4]);
        if !origin_lon.is_finite() || !origin_lat.is_finite() {
            return Err(TrailgenError::InvalidData(
                "GeoTIFF DEM tiepoint georeference must be finite".to_owned(),
            ));
        }
        Ok(Self {
            origin_lon,
            origin_lat,
            pixel_width_deg,
            pixel_height_deg,
        })
    }
}

fn validate_geokeys<R: std::io::Read + std::io::Seek>(decoder: &mut Decoder<R>) -> Result<()> {
    let Some(keys) = optional_u16_vec(decoder, Tag::GeoKeyDirectoryTag)? else {
        return Err(TrailgenError::InvalidData(
            "GeoTIFF DEM requires GeoKeyDirectoryTag with geographic degree CRS".to_owned(),
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
    for entry in keys[4..][..count * 4].chunks_exact(4) {
        match entry[0] {
            1024 if entry[1] == 0 => model_type = Some(entry[3]),
            2054 if entry[1] == 0 => angular_units = Some(entry[3]),
            3072 => {
                return Err(TrailgenError::UnsupportedFormat(
                    "projected GeoTIFF DEM requires reprojection before ingestion".to_owned(),
                ));
            }
            _ => {}
        }
    }
    if model_type != Some(2) {
        return Err(TrailgenError::UnsupportedFormat(
            "GeoTIFF DEM must declare GTModelTypeGeoKey=geographic".to_owned(),
        ));
    }
    if angular_units.is_some_and(|unit| unit != 9102) {
        return Err(TrailgenError::UnsupportedFormat(
            "GeoTIFF DEM angular units must be degrees".to_owned(),
        ));
    }
    Ok(())
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
