use crate::map::{ALLTRAILS_GREEN, CANDIDATE_COLORS, terrain_color};
use dwemer_poolrooms::chrome;
use egui::{Color32, Rect, Response, Sense, Shape, Stroke, Ui, pos2, vec2};
use serde::{Deserialize, Serialize};
use trailgen_core::{Route, Terrain, TrailGraph};

pub const TILE_SIZE: egui::Vec2 = egui::Vec2::new(224.0, 146.0);
const PLATE_PAD: f32 = 4.0;
const TILE_RADIUS: u8 = 2;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum CandidateSort {
    #[default]
    Rank,
    Distance,
    Ascent,
    Difficulty,
    Trail,
}

impl CandidateSort {
    pub const ALL: [Self; 5] = [
        Self::Rank,
        Self::Distance,
        Self::Ascent,
        Self::Difficulty,
        Self::Trail,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Rank => "RANK",
            Self::Distance => "DISTANCE",
            Self::Ascent => "ASCENT",
            Self::Difficulty => "DIFFICULTY",
            Self::Trail => "TRAIL %",
        }
    }
}

pub fn order(routes: &[Route], sort: CandidateSort) -> Vec<usize> {
    let mut slots = (0..routes.len()).collect::<Vec<_>>();
    slots.sort_by(|&a, &b| {
        let (a, b) = (&routes[a], &routes[b]);
        match sort {
            CandidateSort::Rank => a
                .pareto_rank
                .cmp(&b.pareto_rank)
                .then_with(|| a.score.total_cmp(&b.score)),
            CandidateSort::Distance => b.metrics.distance_m.total_cmp(&a.metrics.distance_m),
            CandidateSort::Ascent => b.metrics.ascent_m.total_cmp(&a.metrics.ascent_m),
            CandidateSort::Difficulty => a.metrics.difficulty.total_cmp(&b.metrics.difficulty),
            CandidateSort::Trail => {
                terrain_fraction(b, Terrain::Trail).total_cmp(&terrain_fraction(a, Terrain::Trail))
            }
        }
        .then_with(|| a.name.cmp(&b.name))
    });
    slots
}

pub fn tile(
    ui: &mut Ui,
    graph: &TrailGraph,
    route: &Route,
    ordinal: usize,
    active: bool,
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
    paint_miniature(
        ui,
        graph,
        route,
        preview.shrink2(vec2(12.0, 9.0)),
        CANDIDATE_COLORS[ordinal % CANDIDATE_COLORS.len()],
    );

    let title = Rect::from_min_max(
        pos2(well.left() + 7.0, preview.bottom() + 6.0),
        pos2(well.right() - 7.0, well.bottom() - 5.0),
    );
    ui.painter().text(
        title.left_top(),
        egui::Align2::LEFT_TOP,
        route.name.to_ascii_uppercase(),
        egui::FontId::monospace(12.0),
        if active { chrome::HOT } else { chrome::TEXT },
    );
    ui.painter().text(
        pos2(title.left(), title.bottom()),
        egui::Align2::LEFT_BOTTOM,
        format!(
            "{:.1} KM   ↗ {:.0} M   ◇ {:.1}",
            route.metrics.distance_m / 1_000.0,
            route.metrics.ascent_m,
            route.metrics.difficulty
        ),
        egui::FontId::monospace(10.5),
        chrome::MUTED,
    );
    let verdict = if route.verdict.satisfied {
        ("FIT", ALLTRAILS_GREEN)
    } else {
        ("OFF", Color32::from_rgb(208, 116, 72))
    };
    ui.painter().text(
        title.right_top(),
        egui::Align2::RIGHT_TOP,
        verdict.0,
        egui::FontId::monospace(10.5),
        verdict.1,
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

fn paint_miniature(ui: &Ui, graph: &TrailGraph, route: &Route, rect: Rect, color: Color32) {
    let geometry = route.geometry(graph);
    let mean_lat = geometry.points.iter().map(|point| point.lat).sum::<f64>()
        / geometry.points.len().max(1) as f64;
    let cos_lat = mean_lat.to_radians().cos();
    let xy = geometry
        .points
        .iter()
        .map(|point| [point.lon * cos_lat, point.lat])
        .collect::<Vec<_>>();
    let bounds = xy.iter().fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |mut bounds, point| {
            bounds[0] = bounds[0].min(point[0]);
            bounds[1] = bounds[1].min(point[1]);
            bounds[2] = bounds[2].max(point[0]);
            bounds[3] = bounds[3].max(point[1]);
            bounds
        },
    );
    let width = (bounds[2] - bounds[0]).max(1.0e-12);
    let height = (bounds[3] - bounds[1]).max(1.0e-12);
    let scale = (f64::from(rect.width()) / width).min(f64::from(rect.height()) / height);
    let center = [(bounds[0] + bounds[2]) * 0.5, (bounds[1] + bounds[3]) * 0.5];
    let screen = |point: [f64; 2]| {
        rect.center()
            + vec2(
                ((point[0] - center[0]) * scale) as f32,
                (-(point[1] - center[1]) * scale) as f32,
            )
    };
    let points = xy.into_iter().map(screen).collect::<Vec<_>>();
    let _shadow = ui.painter().add(Shape::line(
        points.clone(),
        Stroke::new(4.8_f32, Color32::from_black_alpha(190)),
    ));
    let _path = ui
        .painter()
        .add(Shape::line(points, Stroke::new(2.7_f32, color)));

    let mut at = route.start;
    for edge_id in &route.edges {
        let edge = &graph.edges[edge_id.0];
        let line = edge.oriented_geometry(at);
        let points = line
            .points
            .iter()
            .map(|point| screen([point.lon * cos_lat, point.lat]))
            .collect();
        let _terrain = ui.painter().add(Shape::line(
            points,
            Stroke::new(1.0_f32, terrain_color(edge.attr.terrain)),
        ));
        at = edge
            .traverse(at)
            .expect("validated route edge must be traversable");
    }
}

fn terrain_fraction(route: &Route, terrain: Terrain) -> f64 {
    route
        .metrics
        .terrain_m
        .get(&terrain)
        .copied()
        .unwrap_or_default()
        / route.metrics.distance_m.max(1.0)
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
        assert_eq!(order(&routes, CandidateSort::Distance), vec![2, 1, 0]);
    }
}
