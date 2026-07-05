use crate::enrich::{ElevationSample, ElevationSampler};
use crate::geo::Coord;
use crate::model::Provenance;
use crate::{Result, TrailgenError};
use serde::{Deserialize, Serialize};
use std::iter::Peekable;
use std::str::FromStr;

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
