use crate::constraints::LoopConstraints;
use crate::model::{Edge, EdgeId, Terrain, TrailGraph, VertexId};
use crate::route::{LOW_CONFIDENCE_THRESHOLD, is_restricted_access};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinearSense {
    Le,
    Eq,
    Ge,
}

impl LinearSense {
    const fn lp(self) -> &'static str {
        match self {
            Self::Le => "<=",
            Self::Eq => "=",
            Self::Ge => ">=",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearTerm {
    pub coeff: f64,
    pub var: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearRow {
    pub name: String,
    pub terms: Vec<LinearTerm>,
    pub sense: LinearSense,
    pub rhs: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VariableBound {
    pub var: String,
    pub lower: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upper: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopMilpFormulation {
    pub name: String,
    pub start: VertexId,
    pub objective: Vec<LinearTerm>,
    pub rows: Vec<LinearRow>,
    pub bounds: Vec<VariableBound>,
    pub binaries: Vec<String>,
}

#[derive(Clone, Copy)]
struct ArcVar {
    edge: EdgeId,
    from: VertexId,
    to: VertexId,
}

impl LoopMilpFormulation {
    #[must_use]
    pub fn formulate(graph: &TrailGraph, start: VertexId, constraints: &LoopConstraints) -> Self {
        let arcs = graph
            .edges
            .iter()
            .flat_map(allowed_arcs)
            .collect::<Vec<_>>();
        let mut rows = Vec::new();
        let mut binaries = BTreeSet::new();
        let mut bounds = Vec::new();
        let objective = arcs
            .iter()
            .map(|arc| {
                let edge = &graph.edges[arc.edge.0];
                LinearTerm {
                    coeff: arc_objective(edge),
                    var: arc.z(),
                }
            })
            .collect::<Vec<_>>();

        for v in &graph.vertices {
            let y = y(v.id);
            binaries.insert(y.clone());
            rows.push(LinearRow {
                name: format!("out_degree_v{}", v.id.0),
                terms: degree_terms(&arcs, v.id, Direction::Out, &y),
                sense: LinearSense::Eq,
                rhs: 0.0,
            });
            rows.push(LinearRow {
                name: format!("in_degree_v{}", v.id.0),
                terms: degree_terms(&arcs, v.id, Direction::In, &y),
                sense: LinearSense::Eq,
                rhs: 0.0,
            });
        }
        rows.push(LinearRow {
            name: "force_start".to_owned(),
            terms: vec![term(1.0, y(start))],
            sense: LinearSense::Eq,
            rhs: 1.0,
        });

        let m = f64::from(
            u32::try_from(graph.vertices.len().saturating_sub(1))
                .expect("MILP formulation supports at most u32::MAX visited vertices"),
        );
        for arc in &arcs {
            binaries.insert(arc.z());
            bounds.push(VariableBound {
                var: arc.f(),
                lower: 0.0,
                upper: None,
            });
            rows.push(LinearRow {
                name: format!("flow_gate_e{}_v{}_v{}", arc.edge.0, arc.from.0, arc.to.0),
                terms: vec![term(1.0, arc.f()), term(-m, arc.z())],
                sense: LinearSense::Le,
                rhs: 0.0,
            });
        }
        rows.push(start_flow_row(&arcs, graph, start));
        rows.extend(
            graph
                .vertices
                .iter()
                .filter(|v| v.id != start)
                .map(|v| vertex_flow_row(&arcs, v.id)),
        );

        rows.extend(bound_rows(graph, &arcs, constraints));
        rows.extend(terrain_rows(graph, &arcs, constraints));

        Self {
            name: "trailgen_loop_milp".to_owned(),
            start,
            objective,
            rows,
            bounds,
            binaries: binaries.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn to_lp(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "\\* {} start_v{} *\\", self.name, self.start.0);
        s.push_str("Minimize\n");
        write_row_expr(&mut s, " obj", &self.objective, LinearSense::Eq, 0.0, false);
        s.push_str("Subject To\n");
        for row in &self.rows {
            write_row_expr(
                &mut s,
                &format!(" {}", row.name),
                &row.terms,
                row.sense,
                row.rhs,
                true,
            );
        }
        if !self.bounds.is_empty() {
            s.push_str("Bounds\n");
            for bound in &self.bounds {
                match bound.upper {
                    Some(upper) => {
                        let _ = writeln!(
                            s,
                            " {} <= {} <= {}",
                            fmt_num(bound.lower),
                            bound.var,
                            fmt_num(upper)
                        );
                    }
                    None => {
                        let _ = writeln!(s, " {} <= {}", fmt_num(bound.lower), bound.var);
                    }
                }
            }
        }
        s.push_str("Binary\n");
        for var in &self.binaries {
            let _ = writeln!(s, " {var}");
        }
        s.push_str("End\n");
        s
    }
}

#[derive(Clone, Copy)]
enum Direction {
    In,
    Out,
}

impl ArcVar {
    fn z(self) -> String {
        format!("z_e{}_v{}_v{}", self.edge.0, self.from.0, self.to.0)
    }

    fn f(self) -> String {
        format!("f_e{}_v{}_v{}", self.edge.0, self.from.0, self.to.0)
    }
}

fn allowed_arcs(edge: &Edge) -> Vec<ArcVar> {
    [edge.a, edge.b]
        .into_iter()
        .filter_map(|from| {
            Some(ArcVar {
                edge: edge.id,
                from,
                to: edge.traverse(from)?,
            })
        })
        .collect()
}

fn arc_objective(edge: &Edge) -> f64 {
    let low_conf_m = if edge.attr.confidence < LOW_CONFIDENCE_THRESHOLD {
        edge.attr.length_m
    } else {
        0.0
    };
    let road_m = edge.attr.length_m * road_pavement_exposure(edge);
    let restricted_m = if is_restricted_access(edge.attr.access) {
        edge.attr.length_m
    } else {
        0.0
    };
    edge.attr.difficulty.mul_add(
        0.05,
        low_conf_m.mul_add(0.002, road_m.mul_add(0.001, restricted_m * 0.01)),
    )
}

fn degree_terms(arcs: &[ArcVar], v: VertexId, direction: Direction, y: &str) -> Vec<LinearTerm> {
    let mut terms = arcs
        .iter()
        .filter(|arc| match direction {
            Direction::In => arc.to == v,
            Direction::Out => arc.from == v,
        })
        .map(|arc| term(1.0, arc.z()))
        .collect::<Vec<_>>();
    terms.push(term(-1.0, y));
    terms
}

fn start_flow_row(arcs: &[ArcVar], graph: &TrailGraph, start: VertexId) -> LinearRow {
    let mut terms = Vec::new();
    for arc in arcs {
        if arc.from == start {
            terms.push(term(1.0, arc.f()));
        }
        if arc.to == start {
            terms.push(term(-1.0, arc.f()));
        }
    }
    for v in &graph.vertices {
        if v.id != start {
            terms.push(term(-1.0, y(v.id)));
        }
    }
    LinearRow {
        name: "flow_start_supplies_visited_vertices".to_owned(),
        terms,
        sense: LinearSense::Eq,
        rhs: 0.0,
    }
}

fn vertex_flow_row(arcs: &[ArcVar], v: VertexId) -> LinearRow {
    let mut terms = Vec::new();
    for arc in arcs {
        if arc.to == v {
            terms.push(term(1.0, arc.f()));
        }
        if arc.from == v {
            terms.push(term(-1.0, arc.f()));
        }
    }
    terms.push(term(-1.0, y(v)));
    LinearRow {
        name: format!("flow_v{}_receives_if_visited", v.0),
        terms,
        sense: LinearSense::Eq,
        rhs: 0.0,
    }
}

fn bound_rows(
    graph: &TrailGraph,
    arcs: &[ArcVar],
    constraints: &LoopConstraints,
) -> Vec<LinearRow> {
    let specs = [
        (
            "distance_min",
            EdgeScalar::Length,
            LinearSense::Ge,
            constraints.min_distance_m,
        ),
        (
            "distance_max",
            EdgeScalar::Length,
            LinearSense::Le,
            constraints.max_distance_m,
        ),
        (
            "difficulty_min",
            EdgeScalar::Difficulty,
            LinearSense::Ge,
            constraints.min_difficulty,
        ),
        (
            "difficulty_max",
            EdgeScalar::Difficulty,
            LinearSense::Le,
            constraints.max_difficulty,
        ),
        (
            "ascent_min",
            EdgeScalar::Ascent,
            LinearSense::Ge,
            constraints.min_ascent_m,
        ),
        (
            "ascent_max",
            EdgeScalar::Ascent,
            LinearSense::Le,
            constraints.max_ascent_m,
        ),
        (
            "descent_min",
            EdgeScalar::Descent,
            LinearSense::Ge,
            constraints.min_descent_m,
        ),
        (
            "descent_max",
            EdgeScalar::Descent,
            LinearSense::Le,
            constraints.max_descent_m,
        ),
    ];
    let mut rows = specs
        .into_iter()
        .map(|(name, scalar, sense, rhs)| LinearRow {
            name: name.to_owned(),
            terms: arcs
                .iter()
                .map(|arc| term(arc_scalar(graph, *arc, scalar), arc.z()))
                .collect(),
            sense,
            rhs,
        })
        .collect::<Vec<_>>();
    rows.extend([
        fraction_row(
            graph,
            arcs,
            "road_fraction_max",
            constraints.max_road_fraction,
            road_pavement_m,
        ),
        fraction_row(
            graph,
            arcs,
            "low_confidence_fraction_max",
            constraints.max_low_confidence_fraction,
            low_confidence_m,
        ),
        fraction_row(
            graph,
            arcs,
            "restricted_access_fraction_max",
            constraints.max_restricted_access_fraction,
            restricted_access_m,
        ),
    ]);
    rows
}

fn terrain_rows(
    graph: &TrailGraph,
    arcs: &[ArcVar],
    constraints: &LoopConstraints,
) -> Vec<LinearRow> {
    let mut rows = Vec::new();
    for terrain in &constraints.forbidden_terrain {
        rows.push(LinearRow {
            name: format!("forbid_terrain_{terrain:?}").to_ascii_lowercase(),
            terms: terrain_terms(graph, arcs, *terrain, 0.0),
            sense: LinearSense::Eq,
            rhs: 0.0,
        });
    }
    for (terrain, minimum) in &constraints.min_terrain_fraction {
        rows.push(LinearRow {
            name: format!("terrain_{terrain:?}_min").to_ascii_lowercase(),
            terms: terrain_terms(graph, arcs, *terrain, *minimum),
            sense: LinearSense::Ge,
            rhs: 0.0,
        });
    }
    for (terrain, maximum) in &constraints.max_terrain_fraction {
        rows.push(LinearRow {
            name: format!("terrain_{terrain:?}_max").to_ascii_lowercase(),
            terms: terrain_terms(graph, arcs, *terrain, *maximum),
            sense: LinearSense::Le,
            rhs: 0.0,
        });
    }
    rows
}

#[derive(Clone, Copy)]
enum EdgeScalar {
    Length,
    Difficulty,
    Ascent,
    Descent,
}

fn arc_scalar(graph: &TrailGraph, arc: ArcVar, scalar: EdgeScalar) -> f64 {
    let edge = &graph.edges[arc.edge.0];
    match scalar {
        EdgeScalar::Length => edge.attr.length_m,
        EdgeScalar::Difficulty => edge.attr.difficulty,
        EdgeScalar::Ascent if arc.from == edge.a => edge.attr.ascent_m,
        EdgeScalar::Ascent => edge.attr.descent_m,
        EdgeScalar::Descent if arc.from == edge.a => edge.attr.descent_m,
        EdgeScalar::Descent => edge.attr.ascent_m,
    }
}

fn fraction_row(
    graph: &TrailGraph,
    arcs: &[ArcVar],
    name: &str,
    maximum: f64,
    numerator: fn(&Edge) -> f64,
) -> LinearRow {
    LinearRow {
        name: name.to_owned(),
        terms: arcs
            .iter()
            .map(|arc| {
                let edge = &graph.edges[arc.edge.0];
                term(
                    maximum.mul_add(-edge.attr.length_m, numerator(edge)),
                    arc.z(),
                )
            })
            .collect(),
        sense: LinearSense::Le,
        rhs: 0.0,
    }
}

fn terrain_terms(
    graph: &TrailGraph,
    arcs: &[ArcVar],
    terrain: Terrain,
    fraction: f64,
) -> Vec<LinearTerm> {
    arcs.iter()
        .map(|arc| {
            let edge = &graph.edges[arc.edge.0];
            let terrain_m = if edge.attr.terrain == terrain {
                edge.attr.length_m
            } else {
                0.0
            };
            term(fraction.mul_add(-edge.attr.length_m, terrain_m), arc.z())
        })
        .collect()
}

fn road_pavement_m(edge: &Edge) -> f64 {
    edge.attr.length_m * road_pavement_exposure(edge)
}

fn low_confidence_m(edge: &Edge) -> f64 {
    if edge.attr.confidence < LOW_CONFIDENCE_THRESHOLD {
        edge.attr.length_m
    } else {
        0.0
    }
}

const fn restricted_access_m(edge: &Edge) -> f64 {
    if is_restricted_access(edge.attr.access) {
        edge.attr.length_m
    } else {
        0.0
    }
}

const fn road_pavement_exposure(edge: &Edge) -> f64 {
    edge.attr.road_exposure.clamp(0.0, 1.0).max(
        if matches!(edge.attr.terrain, Terrain::Pavement | Terrain::Road) {
            1.0
        } else {
            0.0
        },
    )
}

fn term(coeff: f64, var: impl Into<String>) -> LinearTerm {
    LinearTerm {
        coeff,
        var: var.into(),
    }
}

fn y(v: VertexId) -> String {
    format!("y_v{}", v.0)
}

fn write_row_expr(
    out: &mut String,
    name: &str,
    terms: &[LinearTerm],
    sense: LinearSense,
    rhs: f64,
    include_sense: bool,
) {
    let _ = write!(out, "{name}:");
    for term in terms {
        if term.coeff >= 0.0 {
            let _ = write!(out, " + {} {}", fmt_num(term.coeff), term.var);
        } else {
            let _ = write!(out, " - {} {}", fmt_num(-term.coeff), term.var);
        }
    }
    if include_sense {
        let _ = write!(out, " {} {}", sense.lp(), fmt_num(rhs));
    }
    out.push('\n');
}

fn fmt_num(x: f64) -> String {
    let mut s = format!("{x:.9}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" { "0".to_owned() } else { s }
}
