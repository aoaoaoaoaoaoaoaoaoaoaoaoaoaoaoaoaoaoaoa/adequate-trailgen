use crate::chrome;
use crate::{
    library::SavedTrail,
    map::{terrain_color, terrain_label},
};
use egui::{Color32, Rect, Response, Sense, Shape, Stroke, Ui, pos2, vec2};
use trailgen_core::{LineString, Route, Terrain, TrailGraph};

const TERRAIN_RIBBON_HEIGHT: f32 = 5.0;
const TERRAIN_RIBBON_GUTTER: f32 = 4.0;

pub struct ElevationProfile {
    path: Vec<PathSample>,
    samples: Vec<Sample>,
    spans: Vec<Span>,
    distance_m: f64,
    minimum_m: f64,
    maximum_m: f64,
}

#[derive(Clone, Copy)]
struct PathSample {
    distance_m: f64,
    coord: trailgen_core::Coord,
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
}

#[derive(Clone, Copy)]
struct ProfileLanes {
    elevation: Rect,
    terrain: Rect,
}

impl ProfileLanes {
    fn cleave(plot: Rect) -> Self {
        let terrain = Rect::from_min_max(
            pos2(plot.left(), plot.bottom() - TERRAIN_RIBBON_HEIGHT),
            plot.right_bottom(),
        );
        let elevation = Rect::from_min_max(
            plot.min,
            pos2(plot.right(), terrain.top() - TERRAIN_RIBBON_GUTTER),
        );
        Self { elevation, terrain }
    }
}

pub struct ProfileResponse {
    pub response: Response,
    pub hovered_m: Option<f64>,
}

impl ElevationProfile {
    pub fn forge(graph: &TrailGraph, route: &Route) -> Option<Self> {
        let mut at = route.start;
        let legs = route
            .edges
            .iter()
            .map(|edge_id| {
                let edge = &graph.edges[edge_id.0];
                let line = edge.oriented_geometry(at);
                at = edge
                    .traverse(at)
                    .expect("validated route edge must be traversable");
                (line, edge.attr.terrain)
            })
            .collect::<Vec<_>>();
        Self::from_legs(
            legs.iter().map(|(line, terrain)| (line, *terrain)),
            route.metrics.distance_m,
        )
    }

    pub fn forge_saved(trail: &SavedTrail) -> Option<Self> {
        Self::from_legs(
            trail.legs.iter().map(|leg| (&leg.geometry, leg.terrain)),
            trail.metrics.distance_m,
        )
    }

    fn from_legs<'a>(
        legs: impl IntoIterator<Item = (&'a LineString, Terrain)>,
        measured_distance_m: f64,
    ) -> Option<Self> {
        let mut path = Vec::new();
        let mut samples = Vec::new();
        let mut spans: Vec<Span> = Vec::new();
        let mut raw_distance_m = 0.0;
        for (line, terrain) in legs {
            let from_m = raw_distance_m;
            for (slot, coord) in line.points.iter().copied().enumerate() {
                if slot > 0 {
                    raw_distance_m += line.points[slot - 1].haversine_m(coord);
                }
                path.push(PathSample {
                    distance_m: raw_distance_m,
                    coord,
                });
                if let Some(elevation_m) = coord.ele.filter(|value| value.is_finite()) {
                    samples.push(Sample {
                        distance_m: raw_distance_m,
                        elevation_m,
                    });
                }
            }
            annex_span(&mut spans, from_m, raw_distance_m, terrain);
        }
        if samples.len() < 2 || raw_distance_m <= f64::EPSILON {
            return None;
        }
        let rescale = measured_distance_m / raw_distance_m;
        for sample in &mut samples {
            sample.distance_m *= rescale;
        }
        for sample in &mut path {
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
            path,
            samples,
            spans,
            distance_m: measured_distance_m,
            minimum_m,
            maximum_m,
        })
    }

    pub fn show(&self, ui: &mut Ui, height: f32, locked_m: Option<f64>) -> ProfileResponse {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
        let painter = ui.painter_at(rect);
        let _ground = painter.rect_filled(rect, 1.0, chrome::CONTROL);
        let _edge = painter.rect_stroke(
            rect,
            1.0,
            Stroke::new(1.0_f32, chrome::EDGE),
            egui::StrokeKind::Inside,
        );
        let plot = Rect::from_min_max(rect.min + vec2(42.0, 18.0), rect.max - vec2(10.0, 24.0));
        let lanes = ProfileLanes::cleave(plot);
        self.paint_grid(&painter, lanes);
        self.paint_elevation(&painter, lanes.elevation);
        self.paint_terrain(&painter, lanes.terrain);
        let hovered_m = self.hovered_distance(plot, &response);
        if let Some(distance_m) = locked_m.or(hovered_m) {
            self.paint_probe(ui, &painter, lanes, distance_m);
        }
        ProfileResponse {
            response,
            hovered_m,
        }
    }

    fn paint_grid(&self, painter: &egui::Painter, lanes: ProfileLanes) {
        let plot = lanes.elevation;
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
            if let Some(label) = distance_notch(self.distance_m, i) {
                painter.text(
                    pos2(x, lanes.terrain.bottom() + 6.0),
                    egui::Align2::CENTER_TOP,
                    label,
                    egui::FontId::monospace(10.0),
                    chrome::MUTED,
                );
            }
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
            pos2(plot.left(), lanes.terrain.bottom() + 6.0),
            egui::Align2::LEFT_TOP,
            "DISTANCE · KM",
            egui::FontId::monospace(9.0),
            chrome::MUTED,
        );
    }

    fn paint_terrain(&self, painter: &egui::Painter, ribbon: Rect) {
        let mut mesh = egui::Mesh::default();
        mesh.reserve_vertices(self.spans.len() * 4);
        mesh.reserve_triangles(self.spans.len() * 2);
        for span in &self.spans {
            let left = self.x(ribbon, span.from_m);
            let right = self.x(ribbon, span.to_m);
            let color = terrain_color(span.terrain);
            let first = mesh.vertices.len() as u32;
            mesh.colored_vertex(pos2(left, ribbon.top()), color);
            mesh.colored_vertex(pos2(left, ribbon.bottom()), color);
            mesh.colored_vertex(pos2(right, ribbon.top()), color);
            mesh.colored_vertex(pos2(right, ribbon.bottom()), color);
            mesh.add_triangle(first, first + 1, first + 2);
            mesh.add_triangle(first + 2, first + 1, first + 3);
        }
        let _ribbon = painter.add(Shape::mesh(mesh));
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
            fill.colored_vertex(*point, Color32::from_rgb(43, 22, 18));
            fill.colored_vertex(pos2(point.x, plot.bottom()), Color32::from_rgb(43, 22, 18));
        }
        for slot in 0..points.len().saturating_sub(1) {
            let top = (slot * 2) as u32;
            fill.add_triangle(top, top + 1, top + 2);
            fill.add_triangle(top + 2, top + 1, top + 3);
        }
        let _fill = painter.add(Shape::mesh(fill));
        let _line = painter.add(Shape::line(
            points,
            Stroke::new(1.8_f32, crate::map::SELECTED_TRAIL_COLOR),
        ));
    }

    fn hovered_distance(&self, plot: Rect, response: &Response) -> Option<f64> {
        let pointer = response
            .hover_pos()
            .filter(|pointer| plot.contains(*pointer))?;
        let distance_m = f64::from((pointer.x - plot.left()) / plot.width()) * self.distance_m;
        self.samples
            .iter()
            .min_by(|a, b| {
                (a.distance_m - distance_m)
                    .abs()
                    .total_cmp(&(b.distance_m - distance_m).abs())
            })
            .map(|sample| sample.distance_m)
    }

    fn paint_probe(&self, ui: &Ui, canvas: &egui::Painter, lanes: ProfileLanes, distance_m: f64) {
        let Some(sample) = self.samples.iter().min_by(|a, b| {
            (a.distance_m - distance_m)
                .abs()
                .total_cmp(&(b.distance_m - distance_m).abs())
        }) else {
            return;
        };
        let plot = lanes.elevation;
        let x = self.x(plot, sample.distance_m);
        let y = self.y(plot, sample.elevation_m);
        canvas.line_segment(
            [pos2(x, plot.top()), pos2(x, lanes.terrain.bottom())],
            Stroke::new(1.0_f32, chrome::HOT),
        );
        let _dot = canvas.circle_filled(pos2(x, y), 3.2, chrome::HOT);
        let terrain = self
            .spans
            .iter()
            .find(|span| sample.distance_m >= span.from_m && sample.distance_m <= span.to_m)
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

    pub fn coord_at(&self, distance_m: f64) -> Option<trailgen_core::Coord> {
        let distance_m = distance_m.clamp(0.0, self.distance_m);
        let east = self
            .path
            .partition_point(|sample| sample.distance_m <= distance_m);
        let Some(west) = east.checked_sub(1).and_then(|west| self.path.get(west)) else {
            return self.path.first().map(|sample| sample.coord);
        };
        let Some(east) = self.path.get(east) else {
            return Some(west.coord);
        };
        let span_m = east.distance_m - west.distance_m;
        let t = if span_m <= f64::EPSILON {
            0.0
        } else {
            (distance_m - west.distance_m) / span_m
        };
        Some(west.coord.lerp(east.coord, t.clamp(0.0, 1.0)))
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

fn distance_notch(distance_m: f64, slot: u8) -> Option<String> {
    (slot != 0).then(|| format!("{:.1}", distance_m * f64::from(slot) / 4_000.0))
}

fn annex_span(spans: &mut Vec<Span>, from_m: f64, to_m: f64, terrain: Terrain) {
    if to_m <= from_m {
        return;
    }
    if let Some(tail) = spans.last_mut().filter(|tail| tail.terrain == terrain) {
        tail.to_m = to_m;
    } else {
        spans.push(Span {
            from_m,
            to_m,
            terrain,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trailgen_core::{GraphBuilder, LoopConstraints, SearchParams, SolverKind, io::geojson};

    #[test]
    fn profile_lanes_are_disjoint() {
        let plot = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 120.0));
        let lanes = ProfileLanes::cleave(plot);
        assert!(
            (lanes.terrain.height() - TERRAIN_RIBBON_HEIGHT).abs() < f32::EPSILON,
            "terrain ribbon must retain its declared height"
        );
        assert!(
            (lanes.terrain.top() - lanes.elevation.bottom() - TERRAIN_RIBBON_GUTTER).abs()
                < f32::EPSILON,
            "the gutter must separate elevation from terrain"
        );
        assert!(!lanes.elevation.intersects(lanes.terrain));
    }

    #[test]
    fn distance_caption_owns_the_origin_notch() {
        assert_eq!(distance_notch(24_000.0, 0), None);
        assert_eq!(distance_notch(24_000.0, 1).as_deref(), Some("6.0"));
        assert_eq!(distance_notch(24_000.0, 4).as_deref(), Some("24.0"));
    }

    #[test]
    fn adjacent_equal_terrain_is_one_ribbon_run() {
        let mut spans = Vec::new();
        annex_span(&mut spans, 0.0, 10.0, Terrain::Trail);
        annex_span(&mut spans, 10.0, 20.0, Terrain::Trail);
        annex_span(&mut spans, 20.0, 30.0, Terrain::Road);
        annex_span(&mut spans, 30.0, 30.0, Terrain::Forest);
        assert_eq!(spans.len(), 2);
        assert!(spans[0].from_m.abs() < f64::EPSILON);
        assert!((spans[0].to_m - 20.0).abs() < f64::EPSILON);
        assert!((spans[1].from_m - 20.0).abs() < f64::EPSILON);
        assert!((spans[1].to_m - 30.0).abs() < f64::EPSILON);
    }

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
        let geometry = routes[0].geometry(&graph);
        assert_eq!(profile.coord_at(0.0), Some(geometry.start()));
        assert_eq!(
            profile.coord_at(routes[0].metrics.distance_m),
            Some(geometry.end())
        );
        Ok(())
    }
}
