use crate::difficulty::DifficultyWeights;
use crate::geo::{Coord, LineString};
use crate::model::{Edge, GradeDistribution, Provenance, Terrain, TerrainEvidence, TrailGraph};
use crate::{Result, TrailgenError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnrichmentConfig {
    pub sample_spacing_m: f64,
    pub steep_grade_threshold: f64,
}

impl Default for EnrichmentConfig {
    fn default() -> Self {
        Self {
            sample_spacing_m: 25.0,
            steep_grade_threshold: 0.15,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ElevationSample {
    pub ele_m: f64,
    pub confidence: f64,
    pub provenance: Provenance,
}

pub trait ElevationSampler {
    fn sample(&self, coord: Coord) -> Option<ElevationSample>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ElevationMosaic<S> {
    samplers: Vec<S>,
}

impl<S> ElevationMosaic<S> {
    pub fn new(samplers: Vec<S>) -> Result<Self> {
        if samplers.is_empty() {
            return Err(TrailgenError::InvalidData(
                "elevation mosaic requires at least one sampler".to_owned(),
            ));
        }
        Ok(Self { samplers })
    }
}

impl<S: ElevationSampler> ElevationSampler for ElevationMosaic<S> {
    fn sample(&self, coord: Coord) -> Option<ElevationSample> {
        self.samplers
            .iter()
            .find_map(|sampler| sampler.sample(coord))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EmbeddedElevation;

impl ElevationSampler for EmbeddedElevation {
    fn sample(&self, coord: Coord) -> Option<ElevationSample> {
        Some(ElevationSample {
            ele_m: coord.ele?,
            confidence: 0.85,
            provenance: Provenance {
                source: "embedded-geometry-elevation".to_owned(),
                layer: None,
                source_id: None,
                license: None,
            },
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlaneElevation {
    pub origin: Coord,
    pub origin_ele_m: f64,
    pub east_gain_m_per_degree: f64,
    pub north_gain_m_per_degree: f64,
    pub confidence: f64,
}

impl ElevationSampler for PlaneElevation {
    fn sample(&self, coord: Coord) -> Option<ElevationSample> {
        Some(ElevationSample {
            ele_m: self.north_gain_m_per_degree.mul_add(
                coord.lat - self.origin.lat,
                self.east_gain_m_per_degree
                    .mul_add(coord.lon - self.origin.lon, self.origin_ele_m),
            ),
            confidence: self.confidence,
            provenance: Provenance {
                source: "synthetic-plane-elevation".to_owned(),
                layer: Some("fixture".to_owned()),
                source_id: None,
                license: Some("CC0-fixture".to_owned()),
            },
        })
    }
}

pub fn enrich_graph<S: ElevationSampler>(
    graph: &mut TrailGraph,
    sampler: &S,
    config: EnrichmentConfig,
    weights: DifficultyWeights,
) -> Result<()> {
    if config.sample_spacing_m <= 0.0 {
        return Err(TrailgenError::InvalidData(
            "enrichment sample spacing must be positive".to_owned(),
        ));
    }
    for edge in &mut graph.edges {
        enrich_edge(edge, sampler, config, weights)?;
    }
    Ok(())
}

fn enrich_edge<S: ElevationSampler>(
    edge: &mut Edge,
    sampler: &S,
    config: EnrichmentConfig,
    weights: DifficultyWeights,
) -> Result<()> {
    let (sampled_line, elevation_provenance, elevation_confidence) =
        densify_and_sample(&edge.geometry, sampler, config.sample_spacing_m)?;
    let profile = grade_profile(&sampled_line, config.steep_grade_threshold);
    edge.geometry = sampled_line;
    edge.attr.length_m = edge.geometry.length_m();
    edge.attr.ascent_m = profile.ascent_m;
    edge.attr.descent_m = profile.descent_m;
    edge.attr.grade_abs_mean = profile.grade_abs_mean;
    edge.attr.grade_abs_max = profile.grade_abs_max;
    edge.attr.sustained_steep_m = profile.sustained_steep_m;
    edge.attr.grade_distribution = profile.grade_distribution;
    edge.attr.elevation_provenance = elevation_provenance;
    if elevation_confidence > 0.0 {
        edge.attr.confidence = edge.attr.confidence.min(elevation_confidence);
    }
    infer_terrain(edge, &profile);
    weights.apply_edge(edge);
    Ok(())
}

fn densify_and_sample<S: ElevationSampler>(
    line: &LineString,
    sampler: &S,
    spacing_m: f64,
) -> Result<(LineString, Vec<Provenance>, f64)> {
    let mut points = Vec::new();
    let mut provenance = Vec::new();
    let mut confidence = 1.0;
    let mut sample_count = 0u32;
    let mut sample_attempts = 0u32;
    for segment in line.points.windows(2) {
        let a = segment[0];
        let b = segment[1];
        if points.is_empty() {
            points.push(sample_coord(
                a,
                sampler,
                &mut provenance,
                &mut confidence,
                &mut sample_count,
                &mut sample_attempts,
            ));
        }
        let length_m = a.haversine_m(b);
        for distance_m in std::iter::successors(Some(spacing_m), move |d| Some(d + spacing_m))
            .take_while(|d| *d < length_m)
        {
            points.push(sample_coord(
                a.lerp(b, distance_m / length_m),
                sampler,
                &mut provenance,
                &mut confidence,
                &mut sample_count,
                &mut sample_attempts,
            ));
        }
        points.push(sample_coord(
            b,
            sampler,
            &mut provenance,
            &mut confidence,
            &mut sample_count,
            &mut sample_attempts,
        ));
    }
    if sample_count == 0 {
        return Ok((line.clone(), Vec::new(), 0.0));
    }
    if sample_count > 0 && sample_count < sample_attempts {
        confidence = confidence.min(f64::from(sample_count) / f64::from(sample_attempts));
    }
    Ok((LineString::new(points)?, provenance, confidence))
}

fn sample_coord<S: ElevationSampler>(
    coord: Coord,
    sampler: &S,
    provenance: &mut Vec<Provenance>,
    confidence: &mut f64,
    sample_count: &mut u32,
    sample_attempts: &mut u32,
) -> Coord {
    *sample_attempts += 1;
    sampler
        .sample(coord)
        .map_or(Coord { ele: None, ..coord }, |sample| {
            *sample_count += 1;
            *confidence = confidence.min(sample.confidence);
            if !provenance.contains(&sample.provenance) {
                provenance.push(sample.provenance);
            }
            Coord {
                ele: Some(sample.ele_m),
                ..coord
            }
        })
}

#[derive(Clone, Copy)]
struct GradeProfile {
    ascent_m: f64,
    descent_m: f64,
    grade_abs_mean: f64,
    grade_abs_max: f64,
    sustained_steep_m: f64,
    grade_distribution: GradeDistribution,
}

fn grade_profile(line: &LineString, steep_grade_threshold: f64) -> GradeProfile {
    let mut ascent_m = 0.0;
    let mut descent_m = 0.0;
    let mut graded_m = 0.0;
    let mut weighted_abs_grade = 0.0;
    let mut grade_abs_max = 0.0;
    let mut sustained_steep_m = 0.0;
    let mut grade_distribution = GradeDistribution::default();

    for segment in line.points.windows(2) {
        let a = segment[0];
        let b = segment[1];
        let distance = a.haversine_m(b);
        let Some(ele_a) = a.ele else {
            continue;
        };
        let Some(ele_b) = b.ele else {
            continue;
        };
        let rise = ele_b - ele_a;
        if rise >= 0.0 {
            ascent_m += rise;
        } else {
            descent_m -= rise;
        }
        if distance > 0.0 {
            let abs_grade = (rise / distance).abs();
            graded_m += distance;
            weighted_abs_grade = abs_grade.mul_add(distance, weighted_abs_grade);
            if abs_grade > grade_abs_max {
                grade_abs_max = abs_grade;
            }
            if abs_grade >= steep_grade_threshold {
                sustained_steep_m += distance;
            }
            grade_distribution = grade_distribution.add_segment(distance, abs_grade);
        }
    }

    GradeProfile {
        ascent_m,
        descent_m,
        grade_abs_mean: weighted_abs_grade / graded_m.max(1.0),
        grade_abs_max,
        sustained_steep_m,
        grade_distribution,
    }
}

struct TerrainInference {
    terrain: Terrain,
    confidence: f64,
    rationale: String,
    provenance: Option<Provenance>,
}

fn infer_terrain(edge: &mut Edge, profile: &GradeProfile) {
    let evicted_enrichment_evidence = evict_enrichment_terrain_evidence(edge);
    if evicted_enrichment_evidence
        && edge.attr.terrain_confidence < 0.85
        && !edge
            .attr
            .terrain_evidence
            .iter()
            .any(|e| e.terrain == edge.attr.terrain)
    {
        edge.attr.terrain = Terrain::Unknown;
        edge.attr.terrain_confidence = 0.0;
    }

    let terrain = edge.attr.terrain;
    let explicit_confidence = edge.attr.terrain_confidence;
    if terrain != Terrain::Unknown && explicit_confidence >= 0.85 {
        if !edge
            .attr
            .terrain_evidence
            .iter()
            .any(|e| e.terrain == terrain)
        {
            upsert_terrain_evidence(
                edge,
                TerrainEvidence {
                    terrain,
                    confidence: explicit_confidence,
                    rationale: "explicit source terrain tag".to_owned(),
                    provenance: edge.attr.provenance.first().cloned(),
                },
            );
        }
        edge.attr.terrain_confidence = edge.attr.terrain_confidence.max(explicit_confidence);
        return;
    }

    let inference = terrain_inference(edge, profile);
    if inference.confidence < edge.attr.terrain_confidence {
        return;
    }
    edge.attr.terrain = inference.terrain;
    edge.attr.terrain_confidence = inference.confidence;
    edge.attr.confidence = edge
        .attr
        .confidence
        .min(inference.confidence.mul_add(0.35, 0.65));
    upsert_terrain_evidence(
        edge,
        TerrainEvidence {
            terrain: inference.terrain,
            confidence: inference.confidence,
            rationale: inference.rationale,
            provenance: inference.provenance,
        },
    );
}

fn terrain_inference(edge: &Edge, profile: &GradeProfile) -> TerrainInference {
    let grade_basis = grade_basis(edge, profile);
    let edge_provenance = edge.attr.provenance.first().cloned();
    if edge.attr.road_exposure >= 0.75 {
        TerrainInference {
            terrain: Terrain::Road,
            confidence: 0.70,
            rationale: format!(
                "inferred from road exposure {:.0}% with {grade_basis}",
                edge.attr.road_exposure * 100.0
            ),
            provenance: edge
                .attr
                .crossings
                .iter()
                .find(|x| x.kind == crate::model::CrossingKind::Road)
                .map(|x| x.provenance.clone())
                .or(edge_provenance),
        }
    } else if profile.grade_abs_max >= 0.32 {
        TerrainInference {
            terrain: Terrain::Scramble,
            confidence: 0.52,
            rationale: format!("inferred from savage sampled grade: {grade_basis}"),
            provenance: edge
                .attr
                .elevation_provenance
                .first()
                .cloned()
                .or(edge_provenance),
        }
    } else if profile.grade_abs_max >= 0.22 {
        TerrainInference {
            terrain: Terrain::Talus,
            confidence: 0.45,
            rationale: format!("inferred from steep sampled grade: {grade_basis}"),
            provenance: edge
                .attr
                .elevation_provenance
                .first()
                .cloned()
                .or(edge_provenance),
        }
    } else {
        TerrainInference {
            terrain: Terrain::Trail,
            confidence: 0.35,
            rationale: format!("inferred default hiking surface: {grade_basis}"),
            provenance: edge_provenance,
        }
    }
}

fn grade_basis(edge: &Edge, profile: &GradeProfile) -> String {
    if edge.attr.elevation_provenance.is_empty() {
        format!(
            "no sampled elevation source; road exposure {:.0}%",
            edge.attr.road_exposure * 100.0
        )
    } else {
        let bins = profile.grade_distribution;
        let total = bins.total_m().max(1.0);
        format!(
            "max grade {:.1}%, mean grade {:.1}%, steep {:.0}%, savage {:.0}%",
            profile.grade_abs_max * 100.0,
            profile.grade_abs_mean * 100.0,
            bins.steep_m / total * 100.0,
            bins.savage_m / total * 100.0
        )
    }
}

fn evict_enrichment_terrain_evidence(edge: &mut Edge) -> bool {
    let old_len = edge.attr.terrain_evidence.len();
    edge.attr.terrain_evidence.retain(|e| {
        !(e.rationale.starts_with("inferred ")
            || matches!(
                e.rationale.as_str(),
                "high road-exposure fraction"
                    | "very steep sampled grade"
                    | "steep sampled grade"
                    | "default low-confidence hiking surface"
            ))
    });
    edge.attr.terrain_evidence.len() != old_len
}

fn upsert_terrain_evidence(edge: &mut Edge, evidence: TerrainEvidence) {
    if let Some(existing) = edge.attr.terrain_evidence.iter_mut().find(|e| {
        e.terrain == evidence.terrain
            && e.rationale == evidence.rationale
            && e.provenance == evidence.provenance
    }) {
        existing.confidence = existing.confidence.max(evidence.confidence);
    } else {
        edge.attr.terrain_evidence.push(evidence);
    }
}
