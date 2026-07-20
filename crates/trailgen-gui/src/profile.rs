use crate::map::{terrain_color, terrain_label};
use dwemer_poolrooms::chrome;
use egui::{Color32, Rect, Response, Sense, Shape, Stroke, Ui, pos2, vec2};
use trailgen_core::{Route, Terrain, TrailGraph};

pub struct ElevationProfile {
    samples: Vec<Sample>,
    spans: Vec<Span>,
    distance_m: f64,
    minimum_m: f64,
    maximum_m: f64,
}

#[derive(Clone, Copy)]
struct Sample {
    distance_m: f64,
    elevation_m: f64,
}

#[derive(Clone, Copy)]
struct Span {
    from_m: f64,
    to_m: f64,
    terrain: Terrain,
    grade: f64,
}

impl ElevationProfile {
    pub fn forge(graph: &TrailGraph, route: &Route) -> Option<Self> {
        let mut samples = Vec::new();
        let mut spans = Vec::new();
        let mut raw_distance_m = 0.0;
        let mut at = route.start;
        for edge_id in &route.edges {
            let edge = &graph.edges[edge_id.0];
            let line = edge.oriented_geometry(at);
            let from_m = raw_distance_m;
            for (slot, coord) in line.points.iter().copied().enumerate() {
                if slot > 0 {
                    raw_distance_m += line.points[slot - 1].haversine_m(coord);
                }
                if let Some(elevation_m) = coord.ele.filter(|value| value.is_finite()) {
                    samples.push(Sample {
                        distance_m: raw_distance_m,
                        elevation_m,
                    });
                }
            }
            spans.push(Span {
                from_m,
                to_m: raw_distance_m,
                terrain: edge.attr.terrain,
                grade: edge.attr.grade_abs_mean,
            });
            at = edge
                .traverse(at)
                .expect("validated route edge must be traversable");
        }
        if samples.len() < 2 || raw_distance_m <= f64::EPSILON {
            return None;
        }
        let rescale = route.metrics.distance_m / raw_distance_m;
        for sample in &mut samples {
            sample.distance_m *= rescale;
        }
        for span in &mut spans {
            span.from_m *= rescale;
            span.to_m *= rescale;
        }
        let minimum_m = samples
            .iter()
            .map(|sample| sample.elevation_m)
            .fold(f64::INFINITY, f64::min);
        let maximum_m = samples
            .iter()
            .map(|sample| sample.elevation_m)
            .fold(f64::NEG_INFINITY, f64::max);
        Some(Self {
            samples,
            spans,
            distance_m: route.metrics.distance_m,
            minimum_m,
            maximum_m,
        })
    }

    pub fn show(&self, ui: &mut Ui, height: f32) -> Response {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        let painter = ui.painter_at(rect);
        let _ground = painter.rect_filled(rect, 1.0, chrome::CONTROL);
        let _edge = painter.rect_stroke(
            rect,
            1.0,
            Stroke::new(1.0_f32, chrome::EDGE),
            egui::StrokeKind::Inside,
        );
        let plot = Rect::from_min_max(rect.min + vec2(42.0, 18.0), rect.max - vec2(10.0, 24.0));
        self.paint_grid(&painter, plot);
        self.paint_terrain(&painter, plot);
        self.paint_elevation(&painter, plot);
        self.paint_hover(ui, &painter, plot, &response);
        response
    }

    fn paint_grid(&self, painter: &egui::Painter, plot: Rect) {
        for i in 0..=4 {
            let t = i as f32 / 4.0;
            let y = egui::lerp(plot.bottom()..=plot.top(), t);
            painter.line_segment(
                [pos2(plot.left(), y), pos2(plot.right(), y)],
                Stroke::new(0.6_f32, chrome::EDGE.gamma_multiply(0.45)),
            );
        }
        for i in 0..=4 {
            let t = f64::from(i) / 4.0;
            let x = egui::lerp(plot.left()..=plot.right(), t as f32);
            painter.line_segment(
                [pos2(x, plot.top()), pos2(x, plot.bottom())],
                Stroke::new(0.6_f32, chrome::EDGE.gamma_multiply(0.35)),
            );
            painter.text(
                pos2(x, plot.bottom() + 6.0),
                egui::Align2::CENTER_TOP,
                format!("{:.1}", self.distance_m * t / 1_000.0),
                egui::FontId::monospace(10.0),
                chrome::MUTED,
            );
        }
        painter.text(
            pos2(plot.left() - 6.0, plot.top()),
            egui::Align2::RIGHT_TOP,
            format!("{:.0} m", self.maximum_m),
            egui::FontId::monospace(10.0),
            chrome::MUTED,
        );
        painter.text(
            pos2(plot.left() - 6.0, plot.bottom()),
            egui::Align2::RIGHT_BOTTOM,
            format!("{:.0} m", self.minimum_m),
            egui::FontId::monospace(10.0),
            chrome::MUTED,
        );
        painter.text(
            pos2(plot.right(), plot.bottom() + 6.0),
            egui::Align2::RIGHT_TOP,
            "DISTANCE · KM",
            egui::FontId::monospace(9.0),
            chrome::MUTED,
        );
    }

    fn paint_terrain(&self, painter: &egui::Painter, plot: Rect) {
        for span in &self.spans {
            let left = self.x(plot, span.from_m);
            let right = self.x(plot, span.to_m);
            let color = terrain_color(span.terrain);
            let band =
                Rect::from_min_max(pos2(left, plot.bottom() - 5.0), pos2(right, plot.bottom()));
            let _band = painter.rect_filled(band, 0.0, color);
            let grade_y = plot.bottom() - 7.0 - (span.grade.clamp(0.0, 0.5) * 22.0) as f32;
            painter.line_segment(
                [pos2(left, grade_y), pos2(right, grade_y)],
                Stroke::new(1.3_f32, grade_color(span.grade)),
            );
        }
    }

    fn paint_elevation(&self, painter: &egui::Painter, plot: Rect) {
        let points = self
            .samples
            .iter()
            .map(|sample| {
                pos2(
                    self.x(plot, sample.distance_m),
                    self.y(plot, sample.elevation_m),
                )
            })
            .collect::<Vec<_>>();
        let mut fill = egui::Mesh::default();
        fill.reserve_vertices(points.len() * 2);
        fill.reserve_triangles(points.len().saturating_sub(1) * 2);
        for point in &points {
            fill.colored_vertex(*point, Color32::from_rgb(18, 31, 16));
            fill.colored_vertex(
                pos2(point.x, plot.bottom() - 5.0),
                Color32::from_rgb(18, 31, 16),
            );
        }
        for slot in 0..points.len().saturating_sub(1) {
            let top = (slot * 2) as u32;
            fill.add_triangle(top, top + 1, top + 2);
            fill.add_triangle(top + 2, top + 1, top + 3);
        }
        let _fill = painter.add(Shape::mesh(fill));
        let _line = painter.add(Shape::line(
            points,
            Stroke::new(1.8_f32, crate::map::ALLTRAILS_GREEN),
        ));
    }

    fn paint_hover(&self, ui: &Ui, canvas: &egui::Painter, plot: Rect, response: &Response) {
        let Some(pointer) = response
            .hover_pos()
            .filter(|pointer| plot.contains(*pointer))
        else {
            return;
        };
        let distance_m = f64::from((pointer.x - plot.left()) / plot.width()) * self.distance_m;
        let Some(sample) = self.samples.iter().min_by(|a, b| {
            (a.distance_m - distance_m)
                .abs()
                .total_cmp(&(b.distance_m - distance_m).abs())
        }) else {
            return;
        };
        let x = self.x(plot, sample.distance_m);
        let y = self.y(plot, sample.elevation_m);
        canvas.line_segment(
            [pos2(x, plot.top()), pos2(x, plot.bottom())],
            Stroke::new(1.0_f32, chrome::HOT),
        );
        let _dot = canvas.circle_filled(pos2(x, y), 3.2, chrome::HOT);
        let terrain = self
            .spans
            .iter()
            .find(|span| distance_m >= span.from_m && distance_m <= span.to_m)
            .map_or(Terrain::Unknown, |span| span.terrain);
        let text = format!(
            "{:.2} KM  ·  {:.0} M  ·  {}",
            sample.distance_m / 1_000.0,
            sample.elevation_m,
            terrain_label(terrain)
        );
        let galley = canvas.layout_no_wrap(text, egui::FontId::monospace(11.0), chrome::TEXT);
        let label = Rect::from_min_size(
            pos2(
                (x + 8.0).min(plot.right() - galley.size().x - 10.0),
                plot.top() + 7.0,
            ),
            galley.size() + vec2(10.0, 6.0),
        );
        let _plate = canvas.rect_filled(label, 1.0, chrome::RAISED);
        let _rim = canvas.rect_stroke(
            label,
            1.0,
            Stroke::new(1.0_f32, chrome::EDGE_STRONG),
            egui::StrokeKind::Inside,
        );
        canvas.galley(label.min + vec2(5.0, 3.0), galley, chrome::TEXT);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(50));
    }

    fn x(&self, plot: Rect, distance_m: f64) -> f32 {
        egui::lerp(
            plot.left()..=plot.right(),
            (distance_m / self.distance_m.max(1.0)) as f32,
        )
    }

    fn y(&self, plot: Rect, elevation_m: f64) -> f32 {
        let span = (self.maximum_m - self.minimum_m).max(1.0);
        egui::lerp(
            plot.bottom()..=plot.top(),
            ((elevation_m - self.minimum_m) / span) as f32,
        )
    }
}

const fn grade_color(grade: f64) -> Color32 {
    if grade < 0.05 {
        Color32::from_rgb(104, 171, 64)
    } else if grade < 0.15 {
        Color32::from_rgb(211, 178, 78)
    } else if grade < 0.30 {
        Color32::from_rgb(218, 124, 65)
    } else {
        Color32::from_rgb(205, 73, 58)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trailgen_core::{GraphBuilder, LoopConstraints, SearchParams, SolverKind, io::geojson};

    #[test]
    fn route_profile_ends_at_measured_distance() -> anyhow::Result<()> {
        let drafts = geojson::network_from_str(include_str!(
            "../../trailgen-core/tests/fixtures/mini_network.geojson"
        ))?;
        let graph = GraphBuilder::default().build(&drafts)?;
        let constraints = LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 20_000.0,
            ..LoopConstraints::default()
        };
        let routes = SolverKind::Exact.solve(
            SearchParams::default(),
            &graph,
            trailgen_core::VertexId(0),
            &constraints,
            1,
        );
        assert!(!routes.is_empty(), "fixture must contain a loop");
        let profile = ElevationProfile::forge(&graph, &routes[0]).expect("fixture has elevation");
        assert!(
            (profile.samples.last().expect("sample").distance_m - routes[0].metrics.distance_m)
                .abs()
                < 1.0e-6
        );
        Ok(())
    }
}
