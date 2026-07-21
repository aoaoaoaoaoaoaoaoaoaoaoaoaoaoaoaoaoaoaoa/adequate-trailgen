use crate::{
    library::SavedTrail,
    map::{
        CANDIDATE_COLORS, frailest_standing, paint_trail_tube, trail_mark, trail_standing_badge,
        trail_standing_color,
    },
};
use dwemer_poolrooms::chrome;
use egui::{Color32, Rect, Response, Sense, Stroke, Ui, pos2, vec2};
use serde::{Deserialize, Serialize};
use trailgen_core::{LineString, Route, TrailGraph, TrailStanding};

pub const TILE_SIZE: egui::Vec2 = egui::Vec2::new(224.0, 146.0);
const PLATE_PAD: f32 = 4.0;
const TILE_RADIUS: u8 = 2;

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
    graph: &TrailGraph,
    route: &Route,
    ordinal: usize,
    active: bool,
) -> Response {
    let geometry = route.geometry(graph);
    tile_shell(
        ui,
        route.name.as_str(),
        &route.metrics,
        frailest_standing(
            route
                .edges
                .iter()
                .map(|edge| graph.edges[edge.0].attr.standing),
        ),
        active,
        |ui, rect| {
            let color = CANDIDATE_COLORS[ordinal % CANDIDATE_COLORS.len()];
            let mut at = route.start;
            for edge_id in &route.edges {
                let edge = &graph.edges[edge_id.0];
                paint_miniature_leg(
                    ui,
                    rect,
                    &geometry,
                    &edge.oriented_geometry(at),
                    color,
                    trail_mark(
                        edge.attr.trail_class,
                        edge.attr.standing,
                        edge.attr.terrain,
                        edge.attr.surface.as_deref(),
                    ),
                );
                at = edge
                    .traverse(at)
                    .expect("candidate edge must remain traversable");
            }
        },
    )
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
            for leg in &trail.legs {
                paint_miniature_leg(
                    ui,
                    rect,
                    &geometry,
                    &leg.geometry,
                    crate::map::ALLTRAILS_GREEN,
                    trail_mark(
                        leg.trail_class,
                        leg.standing,
                        leg.terrain,
                        leg.surface.as_deref(),
                    ),
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
    ui.painter().text(
        pos2(title.left(), title.bottom()),
        egui::Align2::LEFT_BOTTOM,
        format!(
            "{:.1} KM   ↗ {:.0} M",
            metrics.distance_m / 1_000.0,
            metrics.ascent_m,
        ),
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
) {
    let points = miniature_points(rect, route, leg);
    paint_trail_tube(ui.painter(), &points, 5.4, color, mark);
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
}
