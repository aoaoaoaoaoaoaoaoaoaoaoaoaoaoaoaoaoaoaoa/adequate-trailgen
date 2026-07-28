use crate::{
    cadence,
    library::SavedTrail,
    map::{
        candidate_color, frailest_standing, paint_trail_tube_at, trail_mark, trail_standing_badge,
        trail_standing_color,
    },
};
use dwemer_poolrooms::chrome;
use egui::{Color32, Pos2, Rect, Response, Sense, Stroke, Ui, pos2, vec2};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use trailgen_core::{LineString, Route, TrailGraph, TrailStanding};

pub const TILE_SIZE: egui::Vec2 = egui::Vec2::new(224.0, 146.0);
const PLATE_PAD: f32 = 4.0;
const TILE_RADIUS: u8 = 2;
const MINIATURE_SIZE: egui::Vec2 = egui::Vec2::new(192.0, 72.0);
const MINIATURE_ERROR_POINTS: f32 = 0.24;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrailSort {
    #[default]
    Best,
    Distance,
    Climb,
}

impl TrailSort {
    pub const ALL: [Self; 3] = [Self::Best, Self::Distance, Self::Climb];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Best => "BEST",
            Self::Distance => "DISTANCE ↓",
            Self::Climb => "CLIMB ↓",
        }
    }
}

pub fn order_candidates(routes: &[Route], sort: TrailSort) -> Vec<usize> {
    let mut slots = (0..routes.len()).collect::<Vec<_>>();
    slots.sort_by(|&left, &right| {
        let (left, right) = (&routes[left], &routes[right]);
        match sort {
            TrailSort::Best => left
                .pareto_rank
                .cmp(&right.pareto_rank)
                .then_with(|| left.score.total_cmp(&right.score)),
            TrailSort::Distance => right.metrics.distance_m.total_cmp(&left.metrics.distance_m),
            TrailSort::Climb => right.metrics.ascent_m.total_cmp(&left.metrics.ascent_m),
        }
        .then_with(|| left.name.cmp(&right.name))
    });
    slots
}

pub fn order_saved(trails: &[&SavedTrail], sort: TrailSort) -> Vec<usize> {
    let mut slots = (0..trails.len()).collect::<Vec<_>>();
    slots.sort_by(|&left, &right| {
        let (left, right) = (trails[left], trails[right]);
        match sort {
            TrailSort::Best => left.name.cmp(&right.name),
            TrailSort::Distance => right.metrics.distance_m.total_cmp(&left.metrics.distance_m),
            TrailSort::Climb => right.metrics.ascent_m.total_cmp(&left.metrics.ascent_m),
        }
        .then_with(|| left.name.cmp(&right.name))
    });
    slots
}

pub fn candidate_tile(
    ui: &mut Ui,
    route: &Route,
    preview: &CandidatePreview,
    ordinal: usize,
    active: bool,
) -> Response {
    tile_shell(
        ui,
        route.name.as_str(),
        &route.metrics,
        preview.standing,
        active,
        |ui, rect| {
            let color = candidate_color(ordinal, active);
            preview.paint(ui, rect, color);
        },
    )
}

pub struct CandidatePreview {
    runs: Vec<PreviewRun>,
    standing: Option<TrailStanding>,
}

struct PreviewRun {
    points: Arc<[Pos2]>,
    mark: crate::map::TrailMark,
    datum: f32,
}

impl CandidatePreview {
    pub fn forge(graph: &TrailGraph, route: &Route) -> Self {
        let geometry = route.geometry(graph);
        let projection = MiniatureProjection::fit(&geometry);
        let mut drafts = Vec::<PreviewDraft>::new();
        let mut at = route.start;
        let mut datum = 0.0;
        for edge_id in &route.edges {
            let edge = &graph.edges[edge_id.0];
            let points = projection.project(&edge.oriented_geometry(at));
            let advance = cadence::polyline_length(&points);
            let mark = trail_mark(
                edge.attr.trail_class,
                edge.attr.standing,
                edge.attr.marking,
                edge.attr.terrain,
                edge.attr.surface.as_deref(),
            );
            if let Some(run) = drafts.last_mut()
                && run.mark == mark
                && run
                    .points
                    .last()
                    .zip(points.first())
                    .is_some_and(|(left, right)| left.distance(*right) <= f32::EPSILON)
            {
                run.points.extend(points.iter().skip(1).copied());
            } else {
                drafts.push(PreviewDraft {
                    points,
                    mark,
                    datum,
                });
            }
            datum += advance;
            at = edge
                .traverse(at)
                .expect("candidate edge must remain traversable");
        }
        let runs = drafts
            .into_iter()
            .map(|run| PreviewRun {
                points: simplify_miniature(&run.points).into(),
                mark: run.mark,
                datum: run.datum,
            })
            .collect();
        Self {
            runs,
            standing: frailest_standing(
                route
                    .edges
                    .iter()
                    .map(|edge| graph.edges[edge.0].attr.standing),
            ),
        }
    }

    fn paint(&self, ui: &Ui, rect: Rect, color: Color32) {
        for run in &self.runs {
            let points = run
                .points
                .iter()
                .map(|point| rect.min + point.to_vec2())
                .collect::<Vec<_>>();
            let _advance =
                paint_trail_tube_at(ui.painter(), &points, 5.4, color, run.mark, run.datum);
        }
    }
}

struct PreviewDraft {
    points: Vec<Pos2>,
    mark: crate::map::TrailMark,
    datum: f32,
}

struct MiniatureProjection {
    cos_lat: f64,
    center: [f64; 2],
    scale: f64,
}

impl MiniatureProjection {
    fn fit(route: &LineString) -> Self {
        let mean_lat = route.points.iter().map(|point| point.lat).sum::<f64>()
            / route.points.len().max(1) as f64;
        let cos_lat = mean_lat.to_radians().cos();
        let bounds = route.points.iter().fold(
            [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ],
            |mut bounds, point| {
                let x = point.lon * cos_lat;
                bounds[0] = bounds[0].min(x);
                bounds[1] = bounds[1].min(point.lat);
                bounds[2] = bounds[2].max(x);
                bounds[3] = bounds[3].max(point.lat);
                bounds
            },
        );
        let width = (bounds[2] - bounds[0]).max(1.0e-12);
        let height = (bounds[3] - bounds[1]).max(1.0e-12);
        Self {
            cos_lat,
            center: [(bounds[0] + bounds[2]) * 0.5, (bounds[1] + bounds[3]) * 0.5],
            scale: (f64::from(MINIATURE_SIZE.x) / width).min(f64::from(MINIATURE_SIZE.y) / height),
        }
    }

    fn project(&self, line: &LineString) -> Vec<Pos2> {
        line.points
            .iter()
            .map(|point| {
                MINIATURE_SIZE.to_pos2() * 0.5
                    + vec2(
                        (point.lon.mul_add(self.cos_lat, -self.center[0]) * self.scale) as f32,
                        (-(point.lat - self.center[1]) * self.scale) as f32,
                    )
            })
            .collect()
    }
}

fn simplify_miniature(points: &[Pos2]) -> Vec<Pos2> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut frontier = vec![(0, points.len() - 1)];
    while let Some((start, end)) = frontier.pop() {
        if end <= start + 1 {
            continue;
        }
        let (slot, error) = (start + 1..end)
            .map(|slot| {
                (
                    slot,
                    point_segment_distance(points[slot], points[start], points[end]),
                )
            })
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .expect("a nontrivial interval has an interior point");
        if error > MINIATURE_ERROR_POINTS {
            keep[slot] = true;
            frontier.extend([(start, slot), (slot, end)]);
        }
    }
    points
        .iter()
        .copied()
        .zip(keep)
        .filter_map(|(point, keep)| keep.then_some(point))
        .collect()
}

fn point_segment_distance(point: Pos2, start: Pos2, end: Pos2) -> f32 {
    let edge = end - start;
    let length_squared = edge.length_sq();
    if length_squared <= f32::EPSILON {
        return point.distance(start);
    }
    let progress = ((point - start).dot(edge) / length_squared).clamp(0.0, 1.0);
    point.distance(start + edge * progress)
}

pub fn saved_tile(ui: &mut Ui, trail: &SavedTrail, active: bool) -> Response {
    let geometry = trail.geometry();
    tile_shell(
        ui,
        trail.name.as_str(),
        &trail.metrics,
        frailest_standing(trail.legs.iter().map(|leg| leg.standing)),
        active,
        |ui, rect| {
            let mut datum = 0.0;
            for leg in &trail.legs {
                datum += paint_miniature_leg(
                    ui,
                    rect,
                    &geometry,
                    &leg.geometry,
                    crate::map::SELECTED_TRAIL_COLOR,
                    trail_mark(
                        leg.trail_class,
                        leg.standing,
                        leg.marking,
                        leg.terrain,
                        leg.surface.as_deref(),
                    ),
                    datum,
                );
            }
        },
    )
}

fn tile_shell(
    ui: &mut Ui,
    name: &str,
    metrics: &trailgen_core::RouteMetrics,
    standing: Option<TrailStanding>,
    active: bool,
    paint: impl FnOnce(&Ui, Rect),
) -> Response {
    let (rect, response) = ui.allocate_exact_size(TILE_SIZE, Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let plate = Plate::flat(rect);
    plate.paint(ui, response.hovered(), active);
    let well = plate.well;
    let preview = Rect::from_min_max(well.min, pos2(well.right(), well.top() + 90.0));
    let _preview = ui
        .painter()
        .rect_filled(preview, 0.0, Color32::from_rgb(8, 9, 8));
    paint_grid(ui, preview);
    paint(ui, preview.shrink2(vec2(12.0, 9.0)));
    if let Some(standing) = standing.filter(|standing| *standing != TrailStanding::Established) {
        let label = trail_standing_badge(standing);
        let galley = ui.painter().layout_no_wrap(
            label.to_owned(),
            egui::FontId::monospace(9.0),
            chrome::TEXT,
        );
        let badge = Rect::from_min_size(
            preview.right_top() - vec2(galley.size().x + 11.0, -5.0),
            galley.size() + vec2(7.0, 4.0),
        );
        let _fill = ui
            .painter()
            .rect_filled(badge, 1.0, trail_standing_color(standing));
        ui.painter()
            .galley(badge.min + vec2(3.5, 2.0), galley, chrome::TEXT);
    }

    let title = Rect::from_min_max(
        pos2(well.left() + 7.0, preview.bottom() + 6.0),
        pos2(well.right() - 7.0, well.bottom() - 5.0),
    );
    ui.painter().text(
        title.left_top(),
        egui::Align2::LEFT_TOP,
        name.to_ascii_uppercase(),
        egui::FontId::monospace(12.0),
        if active { chrome::HOT } else { chrome::TEXT },
    );
    let measurements = if metrics.elevation_fraction >= 0.8 {
        format!(
            "{:.1} KM   ASCENT {:.0} M",
            metrics.distance_m / 1_000.0,
            metrics.ascent_m,
        )
    } else {
        format!("{:.1} KM   NO ELEVATION", metrics.distance_m / 1_000.0)
    };
    ui.painter().text(
        pos2(title.left(), title.bottom()),
        egui::Align2::LEFT_BOTTOM,
        measurements,
        egui::FontId::monospace(10.5),
        chrome::MUTED,
    );
    response
}

#[derive(Clone, Copy)]
struct Plate {
    rect: Rect,
    well: Rect,
}

impl Plate {
    fn flat(rect: Rect) -> Self {
        Self {
            rect,
            well: rect.shrink(PLATE_PAD),
        }
    }

    fn paint(self, ui: &Ui, hovered: bool, active: bool) {
        let radius = egui::CornerRadius::same(TILE_RADIUS);
        let _fill = ui.painter().rect_filled(self.rect, radius, chrome::SURFACE);
        let edge = if active {
            chrome::HOT
        } else if hovered {
            chrome::EDGE_STRONG
        } else {
            chrome::EDGE.gamma_multiply(0.55)
        };
        let _stroke = ui.painter().rect_stroke(
            self.rect,
            radius,
            Stroke::new(if active { 1.4_f32 } else { 1.0_f32 }, edge),
            egui::StrokeKind::Inside,
        );
    }
}

fn paint_grid(ui: &Ui, rect: Rect) {
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let x = egui::lerp(rect.left()..=rect.right(), t);
        let y = egui::lerp(rect.top()..=rect.bottom(), t);
        ui.painter().line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(0.5_f32, chrome::EDGE.gamma_multiply(0.24)),
        );
        ui.painter().line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(0.5_f32, chrome::EDGE.gamma_multiply(0.24)),
        );
    }
}

fn paint_miniature_leg(
    ui: &Ui,
    rect: Rect,
    route: &LineString,
    leg: &LineString,
    color: Color32,
    mark: crate::map::TrailMark,
    datum: f32,
) -> f32 {
    let points = miniature_points(rect, route, leg);
    paint_trail_tube_at(ui.painter(), &points, 5.4, color, mark, datum)
}

fn miniature_points(rect: Rect, route: &LineString, line: &LineString) -> Vec<egui::Pos2> {
    let mean_lat =
        route.points.iter().map(|point| point.lat).sum::<f64>() / route.points.len().max(1) as f64;
    let cos_lat = mean_lat.to_radians().cos();
    let bounds = route.points.iter().fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |mut bounds, point| {
            let x = point.lon * cos_lat;
            bounds[0] = bounds[0].min(x);
            bounds[1] = bounds[1].min(point.lat);
            bounds[2] = bounds[2].max(x);
            bounds[3] = bounds[3].max(point.lat);
            bounds
        },
    );
    if !bounds.into_iter().all(f64::is_finite) {
        return Vec::new();
    }
    let width = (bounds[2] - bounds[0]).max(1.0e-12);
    let height = (bounds[3] - bounds[1]).max(1.0e-12);
    let scale = (f64::from(rect.width()) / width).min(f64::from(rect.height()) / height);
    let center = [(bounds[0] + bounds[2]) * 0.5, (bounds[1] + bounds[3]) * 0.5];
    line.points
        .iter()
        .map(|point| {
            rect.center()
                + vec2(
                    (point.lon.mul_add(cos_lat, -center[0]) * scale) as f32,
                    (-(point.lat - center[1]) * scale) as f32,
                )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use trailgen_core::{ConstraintVerdict, RouteMetrics, VertexId};

    fn route(name: &str, distance_m: f64, ascent_m: f64) -> Route {
        Route {
            name: name.to_owned(),
            start: VertexId(0),
            edges: Vec::new(),
            pareto_rank: 1,
            metrics: RouteMetrics {
                distance_m,
                ascent_m,
                ..RouteMetrics::default()
            },
            verdict: ConstraintVerdict {
                satisfied: true,
                violations: Vec::new(),
                audit: Vec::new(),
                penalty: 0.0,
            },
            score: 0.0,
        }
    }

    #[test]
    fn distance_sort_is_longest_first_and_stable_by_name() {
        let routes = vec![
            route("b", 1_000.0, 2.0),
            route("a", 1_000.0, 3.0),
            route("c", 2_000.0, 1.0),
        ];
        assert_eq!(
            order_candidates(&routes, TrailSort::Distance),
            vec![2, 1, 0]
        );
    }

    #[test]
    fn miniature_simplification_is_subpixel_bounded_and_endpoint_exact() {
        let points = (0..200)
            .map(|slot| {
                let x = slot as f32;
                pos2(x, (x * 0.11).sin() * 3.0)
            })
            .collect::<Vec<_>>();
        let simplified = simplify_miniature(&points);

        assert_eq!(simplified.first(), points.first());
        assert_eq!(simplified.last(), points.last());
        assert!(simplified.len() < points.len());
        for point in points {
            let error = simplified
                .windows(2)
                .map(|segment| point_segment_distance(point, segment[0], segment[1]))
                .fold(f32::INFINITY, f32::min);
            assert!(error <= MINIATURE_ERROR_POINTS + 1.0e-4);
        }
    }
}
