use crate::constraints::LoopConstraints;
use crate::model::{Edge, EdgeId, Terrain, TrailGraph, VertexId};
use crate::route::{LOW_CONFIDENCE_THRESHOLD, is_restricted_access};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct MilpSelectedArc {
    pub edge: EdgeId,
    pub from: VertexId,
    pub to: VertexId,
}

#[derive(Debug, thiserror::Error)]
pub enum MilpIncumbentError {
    #[error("MILP incumbent selects no directed arc variables")]
    Empty,
    #[error("MILP solution variable {0:?} is not a trailgen directed-arc variable")]
    InvalidArcVariable(String),
    #[error("MILP incumbent selects duplicate outgoing arcs from vertex {0}")]
    DuplicateOutgoingArc(usize),
    #[error("MILP incumbent selects duplicate incoming arcs to vertex {0}")]
    DuplicateIncomingArc(usize),
    #[error("MILP incumbent has no selected outgoing arc from start vertex {0}")]
    MissingStartArc(usize),
    #[error("MILP incumbent selected edge {edge} is missing from the graph")]
    MissingEdge { edge: usize },
    #[error(
        "MILP incumbent selected arc z_e{edge}_v{from}_v{to}, but graph edge {edge} does not connect those directed vertices"
    )]
    ImpossibleArc { edge: usize, from: usize, to: usize },
    #[error(
        "MILP incumbent path hits vertex {0} with no outgoing selected arc before returning to start"
    )]
    OpenWalk(usize),
    #[error("MILP incumbent path revisits vertex {0} before closing at the requested start")]
    Subtour(usize),
    #[error("MILP incumbent leaves {0} selected arc(s) outside the reconstructed start loop")]
    DisconnectedArcs(usize),
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

pub fn selected_arcs_from_solution(raw: &str) -> Result<Vec<MilpSelectedArc>, MilpIncumbentError> {
    let mut arcs = raw
        .lines()
        .flat_map(solution_line_assignment)
        .filter(|(var, value)| value.abs() > 0.5 && var.starts_with("z_e"))
        .map(|(var, _)| parse_arc_var(var))
        .collect::<Result<Vec<_>, _>>()?;
    arcs.sort_unstable();
    arcs.dedup();
    Ok(arcs)
}

pub fn route_edges_from_solution(
    graph: &TrailGraph,
    start: VertexId,
    raw: &str,
) -> Result<Vec<EdgeId>, MilpIncumbentError> {
    route_edges_from_selected_arcs(graph, start, selected_arcs_from_solution(raw)?)
}

pub fn route_edges_from_selected_arcs(
    graph: &TrailGraph,
    start: VertexId,
    arcs: Vec<MilpSelectedArc>,
) -> Result<Vec<EdgeId>, MilpIncumbentError> {
    if arcs.is_empty() {
        return Err(MilpIncumbentError::Empty);
    }
    let mut out = BTreeMap::<VertexId, MilpSelectedArc>::new();
    let mut ins = BTreeMap::<VertexId, MilpSelectedArc>::new();
    for arc in arcs {
        validate_selected_arc(graph, arc)?;
        if out.insert(arc.from, arc).is_some() {
            return Err(MilpIncumbentError::DuplicateOutgoingArc(arc.from.0));
        }
        if ins.insert(arc.to, arc).is_some() {
            return Err(MilpIncumbentError::DuplicateIncomingArc(arc.to.0));
        }
    }
    if !out.contains_key(&start) {
        return Err(MilpIncumbentError::MissingStartArc(start.0));
    }
    let mut at = start;
    let mut seen_vertices = BTreeSet::from([start]);
    let mut edges = Vec::new();
    loop {
        let arc = out.remove(&at).ok_or(MilpIncumbentError::OpenWalk(at.0))?;
        edges.push(arc.edge);
        at = arc.to;
        if at == start {
            break;
        }
        if !seen_vertices.insert(at) {
            return Err(MilpIncumbentError::Subtour(at.0));
        }
    }
    if !out.is_empty() {
        return Err(MilpIncumbentError::DisconnectedArcs(out.len()));
    }
    Ok(edges)
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

fn validate_selected_arc(
    graph: &TrailGraph,
    arc: MilpSelectedArc,
) -> Result<(), MilpIncumbentError> {
    let edge = graph
        .edges
        .get(arc.edge.0)
        .ok_or(MilpIncumbentError::MissingEdge { edge: arc.edge.0 })?;
    if edge.id != arc.edge || edge.traverse(arc.from) != Some(arc.to) {
        return Err(MilpIncumbentError::ImpossibleArc {
            edge: arc.edge.0,
            from: arc.from.0,
            to: arc.to.0,
        });
    }
    Ok(())
}

fn solution_line_assignment(line: &str) -> impl Iterator<Item = (&str, f64)> {
    let line = line.split('#').next().unwrap_or_default();
    let tokens = line
        .split(|c: char| c.is_whitespace() || matches!(c, '=' | ',' | ';'))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let assignments = tokens.windows(2).filter_map(|pair| {
        parse_value(pair[1])
            .map(|value| (pair[0], value))
            .or_else(|| parse_value(pair[0]).map(|value| (pair[1], value)))
    });
    assignments.collect::<Vec<_>>().into_iter()
}

fn parse_value(token: &str) -> Option<f64> {
    token.parse::<f64>().ok()
}

fn parse_arc_var(var: &str) -> Result<MilpSelectedArc, MilpIncumbentError> {
    let rest = var
        .strip_prefix("z_e")
        .ok_or_else(|| MilpIncumbentError::InvalidArcVariable(var.to_owned()))?;
    let mut xs = rest.split("_v");
    let edge = parse_id(xs.next(), var)?;
    let from = parse_id(xs.next(), var)?;
    let to = parse_id(xs.next(), var)?;
    if xs.next().is_some() {
        return Err(MilpIncumbentError::InvalidArcVariable(var.to_owned()));
    }
    Ok(MilpSelectedArc {
        edge: EdgeId(edge),
        from: VertexId(from),
        to: VertexId(to),
    })
}

fn parse_id(token: Option<&str>, var: &str) -> Result<usize, MilpIncumbentError> {
    token
        .and_then(|x| x.parse::<usize>().ok())
        .ok_or_else(|| MilpIncumbentError::InvalidArcVariable(var.to_owned()))
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
