use crate::{Result, TrailgenError};
use serde::{Deserialize, Serialize};

const EARTH_R_M: f64 = 6_371_008.8;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Coord {
    pub lon: f64,
    pub lat: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ele: Option<f64>,
}

impl Coord {
    #[must_use]
    pub const fn new(lon: f64, lat: f64) -> Self {
        Self {
            lon,
            lat,
            ele: None,
        }
    }

    #[must_use]
    pub const fn with_ele(lon: f64, lat: f64, ele: f64) -> Self {
        Self {
            lon,
            lat,
            ele: Some(ele),
        }
    }

    #[must_use]
    pub fn haversine_m(self, rhs: Self) -> f64 {
        let φ1 = self.lat.to_radians();
        let φ2 = rhs.lat.to_radians();
        let δφ = (rhs.lat - self.lat).to_radians();
        let δλ = (rhs.lon - self.lon).to_radians();
        let sin_half_lat = (δφ / 2.0).sin();
        let sin_half_lon = (δλ / 2.0).sin();
        let a =
            (φ1.cos() * φ2.cos()).mul_add(sin_half_lon * sin_half_lon, sin_half_lat * sin_half_lat);
        2.0 * EARTH_R_M * a.sqrt().asin()
    }

    #[must_use]
    pub fn lerp(self, rhs: Self, t: f64) -> Self {
        if t == 0.0 {
            return self;
        }
        if t.to_bits() == 1.0_f64.to_bits() {
            return rhs;
        }
        let ele = match (self.ele, rhs.ele) {
            (Some(a), Some(b)) => Some((b - a).mul_add(t, a)),
            _ => None,
        };
        Self {
            lon: (rhs.lon - self.lon).mul_add(t, self.lon),
            lat: (rhs.lat - self.lat).mul_add(t, self.lat),
            ele,
        }
    }

    #[must_use]
    pub fn planar_distance2(self, rhs: Self) -> f64 {
        let dx = self.lon - rhs.lon;
        let dy = self.lat - rhs.lat;
        dx.mul_add(dx, dy * dy)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LineString {
    pub points: Vec<Coord>,
}

impl LineString {
    pub fn new(points: Vec<Coord>) -> Result<Self> {
        if points.len() < 2 {
            return Err(TrailgenError::InvalidGeometry(
                "line string needs at least two coordinates".to_owned(),
            ));
        }
        Ok(Self { points })
    }

    #[must_use]
    pub fn unchecked(points: Vec<Coord>) -> Self {
        debug_assert!(points.len() >= 2);
        Self { points }
    }

    #[must_use]
    pub fn length_m(&self) -> f64 {
        self.points.windows(2).map(|w| w[0].haversine_m(w[1])).sum()
    }

    #[must_use]
    pub fn ascent_descent_m(&self) -> (f64, f64) {
        self.points
            .windows(2)
            .filter_map(|w| Some((w[0].ele?, w[1].ele?)))
            .fold((0.0, 0.0), |(up, down), (a, b)| {
                let d = b - a;
                if d >= 0.0 {
                    (up + d, down)
                } else {
                    (up, down - d)
                }
            })
    }

    #[must_use]
    pub fn reversed(&self) -> Self {
        let mut points = self.points.clone();
        points.reverse();
        Self { points }
    }

    #[must_use]
    pub fn start(&self) -> Coord {
        self.points[0]
    }

    #[must_use]
    pub fn end(&self) -> Coord {
        self.points[self.points.len() - 1]
    }
}
