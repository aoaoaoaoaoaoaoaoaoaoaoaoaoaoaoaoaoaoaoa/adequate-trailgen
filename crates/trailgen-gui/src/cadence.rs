use egui::{Color32, Pos2, Shape, Stroke};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pattern {
    Dash { dash: f32, gap: f32 },
    DashDot { dash: f32, gap: f32, dot: f32 },
    Dots { spacing: f32, radius: f32 },
}

impl Pattern {
    pub fn period(self) -> f32 {
        match self {
            Self::Dash { dash, gap } => dash + gap,
            Self::DashDot { dash, gap, dot } => dash + gap.mul_add(2.0, dot),
            Self::Dots { spacing, .. } => spacing,
        }
    }

    pub fn tessellate<I>(
        self,
        points: I,
        stroke: Stroke,
        phase: f32,
        limit: f32,
        shapes: &mut Vec<Shape>,
    ) where
        I: IntoIterator<Item = Pos2>,
    {
        match self {
            Self::Dash { dash, gap } => {
                stroke_pattern(points, stroke, &[dash, gap], phase, limit, shapes);
            }
            Self::DashDot { dash, gap, dot } => {
                stroke_pattern(points, stroke, &[dash, gap, dot, gap], phase, limit, shapes);
            }
            Self::Dots { spacing, radius } => {
                dot_pattern(points, stroke.color, spacing, radius, phase, limit, shapes);
            }
        }
    }

    pub fn splice(self, start_phase: f32, end_phase: f32, length: f32) -> f32 {
        if matches!(self, Self::Dots { .. }) || length <= f32::EPSILON {
            return length * 0.5;
        }
        let period = self.period();
        let midpoint = length * 0.5;
        let radius = (period * 0.5).min(midpoint);
        let start = midpoint - radius;
        let span = radius * 2.0;
        (0..=64)
            .map(|slot| start + span * slot as f32 / 64.0)
            .min_by(|left, right| {
                splice_cost(self, start_phase, end_phase, length, *left, midpoint).total_cmp(
                    &splice_cost(self, start_phase, end_phase, length, *right, midpoint),
                )
            })
            .expect("fixed splice search is nonempty")
    }

    fn ink_at(self, phase: f32) -> bool {
        let phase = phase.rem_euclid(self.period());
        match self {
            Self::Dash { dash, .. } => phase < dash,
            Self::DashDot { dash, gap, dot, .. } => {
                phase < dash || (dash + gap..dash + gap + dot).contains(&phase)
            }
            Self::Dots { .. } => false,
        }
    }
}

fn splice_cost(
    pattern: Pattern,
    start_phase: f32,
    end_phase: f32,
    length: f32,
    splice: f32,
    midpoint: f32,
) -> f32 {
    let left_ink = pattern.ink_at(start_phase + splice);
    let right_ink = pattern.ink_at(end_phase + length - splice);
    let discontinuity: f32 = match (left_ink, right_ink) {
        (true, true) => 0.0,
        (true, false) | (false, true) => 1.0,
        (false, false) => 2.0,
    };
    discontinuity.mul_add(pattern.period(), (splice - midpoint).abs())
}

pub fn polyline_length(points: &[Pos2]) -> f32 {
    points
        .windows(2)
        .map(|window| window[0].distance(window[1]))
        .sum()
}

#[expect(
    clippy::while_float,
    reason = "the arc-length cursor advances by a strictly positive cadence span"
)]
fn stroke_pattern<I>(
    points: I,
    stroke: Stroke,
    spans: &[f32],
    phase: f32,
    limit: f32,
    shapes: &mut Vec<Shape>,
) where
    I: IntoIterator<Item = Pos2>,
{
    debug_assert!(
        !spans.is_empty()
            && spans.len().is_multiple_of(2)
            && spans.iter().all(|span| span.is_finite() && *span > 0.0)
    );
    let period = spans.iter().sum::<f32>();
    let mut phase = phase.rem_euclid(period);
    let mut step = 0;
    for (candidate, span) in spans.iter().copied().enumerate() {
        if phase < span {
            step = candidate;
            break;
        }
        phase -= span;
    }
    let mut remaining = spans[step] - phase;
    let mut ink = step.is_multiple_of(2);
    let mut stroke_points = Vec::new();
    let mut points = points.into_iter();
    let Some(mut start) = points.next() else {
        return;
    };
    if ink {
        stroke_points.push(start);
    }
    let mut budget = limit;

    for end in points {
        if budget <= f32::EPSILON {
            break;
        }
        let vector = end - start;
        let segment_length = vector.length();
        if segment_length <= f32::EPSILON {
            start = end;
            continue;
        }
        let traversable = segment_length.min(budget);
        let mut cursor = 0.0;
        while remaining <= traversable - cursor {
            cursor += remaining;
            let cut = start + vector * (cursor / segment_length);
            if ink {
                stroke_points.push(cut);
                emit_stroke(&mut stroke_points, stroke, shapes);
            }
            step = (step + 1) % spans.len();
            ink = step.is_multiple_of(2);
            remaining = spans[step];
            if ink {
                stroke_points.push(cut);
            }
        }
        let residue = traversable - cursor;
        if residue > f32::EPSILON {
            let cut = start + vector * (traversable / segment_length);
            if ink {
                stroke_points.push(cut);
            }
            remaining -= residue;
        }
        budget -= traversable;
        start = end;
    }
    if ink {
        emit_stroke(&mut stroke_points, stroke, shapes);
    }
}

fn emit_stroke(points: &mut Vec<Pos2>, stroke: Stroke, shapes: &mut Vec<Shape>) {
    if points.len() >= 2 {
        shapes.push(Shape::line(std::mem::take(points), stroke));
    } else {
        points.clear();
    }
}

#[expect(
    clippy::while_float,
    reason = "the arc-length cursor advances by a strictly positive dot spacing"
)]
fn dot_pattern<I>(
    points: I,
    color: Color32,
    spacing: f32,
    radius: f32,
    phase: f32,
    limit: f32,
    shapes: &mut Vec<Shape>,
) where
    I: IntoIterator<Item = Pos2>,
{
    debug_assert!(spacing.is_finite() && spacing > 0.0);
    let phase = phase.rem_euclid(spacing);
    let mut next = if phase <= f32::EPSILON {
        0.0
    } else {
        spacing - phase
    };
    let mut traversed = 0.0;
    let mut points = points.into_iter();
    let Some(mut start) = points.next() else {
        return;
    };

    for end in points {
        if traversed >= limit {
            break;
        }
        let vector = end - start;
        let segment_length = vector.length();
        if segment_length <= f32::EPSILON {
            start = end;
            continue;
        }
        let traversable = segment_length.min(limit - traversed);
        while next <= traversed + traversable {
            let progress = (next - traversed) / segment_length;
            shapes.push(Shape::circle_filled(
                start + vector * progress,
                radius,
                color,
            ));
            next += spacing;
        }
        traversed += traversable;
        start = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn primitive_boundaries_do_not_reset_dash_phase() {
        let pattern = Pattern::Dash {
            dash: 6.0,
            gap: 4.0,
        };
        let stroke = Stroke::new(1.0_f32, Color32::BLACK);
        let first = [pos2(0.0, 0.0), pos2(13.0, 0.0)];
        let second = [pos2(13.0, 0.0), pos2(26.0, 0.0)];
        let whole = [pos2(0.0, 0.0), pos2(13.0, 0.0), pos2(26.0, 0.0)];
        let mut split_shapes = Vec::new();
        pattern.tessellate(first, stroke, 0.0, f32::INFINITY, &mut split_shapes);
        pattern.tessellate(second, stroke, 13.0, f32::INFINITY, &mut split_shapes);
        let mut whole_shapes = Vec::new();
        pattern.tessellate(whole, stroke, 0.0, f32::INFINITY, &mut whole_shapes);

        assert_eq!(ink_bounds(&split_shapes), ink_bounds(&whole_shapes));
    }

    #[test]
    fn a_subpixel_limit_emits_no_partial_stroke() {
        let mut shapes = Vec::new();
        Pattern::Dash {
            dash: 6.0,
            gap: 4.0,
        }
        .tessellate(
            [pos2(0.0, 0.0), pos2(20.0, 0.0)],
            Stroke::new(1.0_f32, Color32::BLACK),
            8.0,
            1.0,
            &mut shapes,
        );
        assert!(shapes.is_empty());
    }

    #[test]
    fn chord_splices_prefer_ink_over_a_doubled_gap() {
        let pattern = Pattern::Dash {
            dash: 6.0,
            gap: 4.0,
        };
        let length = 23.0;
        let splice = pattern.splice(8.0, 7.0, length);

        assert!(pattern.ink_at(8.0 + splice));
        assert!(pattern.ink_at(7.0 + length - splice));
    }

    fn ink_bounds(shapes: &[Shape]) -> Vec<(i32, i32)> {
        shapes
            .iter()
            .map(Shape::visual_bounding_rect)
            .map(|bounds| {
                (
                    (bounds.left() * 1_000.0).round() as i32,
                    (bounds.right() * 1_000.0).round() as i32,
                )
            })
            .fold(Vec::new(), |mut union, next| {
                if let Some(previous) = union.last_mut()
                    && next.0 <= previous.1
                {
                    previous.1 = previous.1.max(next.1);
                } else {
                    union.push(next);
                }
                union
            })
    }
}
