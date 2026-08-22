//! Deterministic cartographic color cycles in `OKLab`.
//!
//! Exact continuous farthest-point insertion would require maintaining the
//! three-dimensional Voronoi complex of the lawful sRGB intersection. Instead,
//! a fixed low-discrepancy reservoir caches every probe's nearest ΔE². Each
//! admitted color then costs O(M) within an epoch; replenishing an exhausted
//! reservoir costs O(kM). Memory is O(M+k), and ties resolve by probe order.

use egui::Color32;
use std::collections::HashSet;

const SEED_COLORS: usize = 8;
const PROBE_BATCH: usize = 2_048;

/// A closed scalar interval in OKLCH space.
#[derive(Clone, Copy)]
pub struct Span {
    low: f64,
    high: f64,
}

impl Span {
    #[must_use]
    pub const fn new(low: f64, high: f64) -> Self {
        Self { low, high }
    }

    fn at(self, unit: f64) -> f64 {
        (self.high - self.low).mul_add(unit, self.low)
    }

    const fn admits(self, value: f64) -> bool {
        value >= self.low && value <= self.high
    }
}

/// The map-safe hue domain. Green is absent because it aliases park ground.
pub const CARTOGRAPHIC_HUES: [Span; 2] = [Span::new(18.0, 82.0), Span::new(220.0, 355.0)];

/// The trail-comparison hue domain. Green aliases park ground; blue aliases
/// hydrology and other geographic context that must remain subordinate.
pub const TRAIL_HIGHLIGHT_HUES: [Span; 2] = [Span::new(18.0, 82.0), Span::new(300.0, 355.0)];

/// One perceptual color-cycle law.
#[derive(Clone, Copy)]
pub struct CycleLaw {
    lightness: Span,
    chroma: Span,
    hues: &'static [Span],
    phase: f64,
    seed_lightness: f64,
    seed_chroma: f64,
}

impl CycleLaw {
    #[must_use]
    pub const fn new(
        lightness: Span,
        chroma: Span,
        hues: &'static [Span],
        phase: f64,
        seed_lightness: f64,
        seed_chroma: f64,
    ) -> Self {
        Self {
            lightness,
            chroma,
            hues,
            phase,
            seed_lightness,
            seed_chroma,
        }
    }

    fn validate(self) {
        assert!(self.lightness.low >= 0.0 && self.lightness.high <= 1.0);
        assert!(self.lightness.low <= self.lightness.high);
        assert!(self.chroma.low >= 0.0 && self.chroma.low <= self.chroma.high);
        assert!(!self.hues.is_empty());
        assert!(
            self.hues
                .iter()
                .all(|hue| { hue.low >= 0.0 && hue.low <= hue.high && hue.high <= 360.0 })
        );
        assert!(self.lightness.admits(self.seed_lightness));
        assert!(self.chroma.admits(self.seed_chroma));
    }

    fn seed(self, ordinal: usize) -> Swatch {
        let phase = radical_inverse(ordinal, 2);
        self.swatch(self.seed_lightness, self.seed_chroma, phase)
    }

    fn probe(self, ordinal: usize) -> Swatch {
        let index = ordinal + 1;
        self.swatch(
            self.lightness.at(radical_inverse(index, 3)),
            self.chroma.at(radical_inverse(index, 5)),
            radical_inverse(index, 2),
        )
    }

    fn swatch(self, lightness: f64, chroma: f64, phase: f64) -> Swatch {
        oklch_srgb(
            lightness,
            chroma,
            hue_at(self.hues, (phase + self.phase).rem_euclid(1.0)),
        )
    }
}

/// A deterministic perceptual cycle with a sparse prefix and maximin tail.
pub struct ColorCycle {
    law: CycleLaw,
    colors: Vec<Color32>,
    labs: Vec<[f64; 3]>,
    occupied: HashSet<[u8; 3]>,
    probes: Vec<Swatch>,
    nearest: Vec<f64>,
    queued: HashSet<[u8; 3]>,
    probe_cursor: usize,
}

impl ColorCycle {
    #[must_use]
    pub fn new(law: CycleLaw) -> Self {
        law.validate();
        Self {
            law,
            colors: Vec::new(),
            labs: Vec::new(),
            occupied: HashSet::new(),
            probes: Vec::new(),
            nearest: Vec::new(),
            queued: HashSet::new(),
            probe_cursor: SEED_COLORS,
        }
    }

    pub fn color(&mut self, ordinal: usize) -> Color32 {
        while self.colors.len() <= ordinal {
            let swatch = if self.colors.len() < SEED_COLORS {
                self.law.seed(self.colors.len())
            } else {
                self.farthest()
            };
            self.admit(swatch);
        }
        self.colors[ordinal]
    }

    fn farthest(&mut self) -> Swatch {
        if self.probes.is_empty() {
            self.refill();
        }
        let mut best = 0;
        for slot in 1..self.probes.len() {
            if self.nearest[slot] > self.nearest[best] {
                best = slot;
            }
        }
        let swatch = self.probes.swap_remove(best);
        let _distance = self.nearest.swap_remove(best);
        let _present = self.queued.remove(&swatch.rgb);
        swatch
    }

    fn refill(&mut self) {
        let mut attempts = 0;
        while self.probes.len() < PROBE_BATCH && attempts < PROBE_BATCH * 4 {
            let swatch = self.law.probe(self.probe_cursor);
            self.probe_cursor = self.probe_cursor.saturating_add(1);
            attempts += 1;
            if self.occupied.contains(&swatch.rgb) || !self.queued.insert(swatch.rgb) {
                continue;
            }
            let nearest = self
                .labs
                .iter()
                .map(|lab| distance2(*lab, swatch.lab))
                .min_by(f64::total_cmp)
                .unwrap_or(f64::INFINITY);
            self.probes.push(swatch);
            self.nearest.push(nearest);
        }
        assert!(
            !self.probes.is_empty(),
            "perceptual color volume exhausted its sRGB images"
        );
    }

    fn admit(&mut self, swatch: Swatch) {
        assert!(
            self.occupied.insert(swatch.rgb),
            "perceptual cycle emitted a duplicate sRGB color"
        );
        for (probe, nearest) in self.probes.iter().zip(&mut self.nearest) {
            *nearest = nearest.min(distance2(swatch.lab, probe.lab));
        }
        self.colors.push(swatch.color);
        self.labs.push(swatch.lab);
    }
}

#[derive(Clone, Copy)]
struct Swatch {
    color: Color32,
    rgb: [u8; 3],
    lab: [f64; 3],
}

fn hue_at(arcs: &[Span], phase: f64) -> f64 {
    let circumference = arcs.iter().map(|arc| arc.high - arc.low).sum::<f64>();
    let mut distance = phase * circumference;
    for arc in arcs {
        let span = arc.high - arc.low;
        if distance <= span {
            return arc.low + distance;
        }
        distance -= span;
    }
    arcs.last().expect("validated hue domain is nonempty").high
}

fn radical_inverse(mut value: usize, base: usize) -> f64 {
    let inverse = 1.0 / base as f64;
    let mut scale = inverse;
    let mut result = 0.0;
    while value > 0 {
        result = ((value % base) as f64).mul_add(scale, result);
        value /= base;
        scale *= inverse;
    }
    result
}

fn distance2(left: [f64; 3], right: [f64; 3]) -> f64 {
    left.into_iter()
        .zip(right)
        .map(|(left, right)| (left - right).powi(2))
        .sum()
}

fn oklch_srgb(lightness: f64, chroma: f64, hue_degrees: f64) -> Swatch {
    let hue = hue_degrees.to_radians();
    let (sin, cos) = hue.sin_cos();
    let linear = |chroma: f64| {
        let ok_a = chroma * cos;
        let ok_b = chroma * sin;
        let lms_l = 0.215_803_757_3_f64
            .mul_add(ok_b, 0.396_337_777_4_f64.mul_add(ok_a, lightness))
            .powi(3);
        let lms_m = 0.063_854_172_8_f64
            .mul_add(-ok_b, 0.105_561_345_8_f64.mul_add(-ok_a, lightness))
            .powi(3);
        let lms_s = 1.291_485_548_f64
            .mul_add(-ok_b, 0.089_484_177_5_f64.mul_add(-ok_a, lightness))
            .powi(3);
        [
            0.230_969_929_2_f64.mul_add(
                lms_s,
                3.307_711_591_3_f64.mul_add(-lms_m, 4.076_741_662_1 * lms_l),
            ),
            0.341_319_396_5_f64.mul_add(
                -lms_s,
                2.609_757_401_1_f64.mul_add(lms_m, -1.268_438_004_6 * lms_l),
            ),
            1.707_614_701_f64.mul_add(
                lms_s,
                0.703_418_614_7_f64.mul_add(-lms_m, -0.004_196_086_3 * lms_l),
            ),
        ]
    };
    let mut low = 0.0;
    let mut high = chroma;
    for _ in 0..14 {
        let probe = (low + high) * 0.5;
        if linear(probe)
            .into_iter()
            .all(|channel| (0.0..=1.0).contains(&channel))
        {
            low = probe;
        } else {
            high = probe;
        }
    }
    let [red, green, blue] = linear(low);
    let gamma = |linear: f64| {
        let srgb = if linear <= 0.003_130_8 {
            12.92 * linear
        } else {
            1.055_f64.mul_add(linear.powf(1.0 / 2.4), -0.055)
        };
        (srgb.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    let rgb = [gamma(red), gamma(green), gamma(blue)];
    Swatch {
        color: Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
        rgb,
        lab: [lightness, low * cos, low * sin],
    }
}
