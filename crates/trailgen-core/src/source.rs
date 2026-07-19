use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    TrailNetwork,
    SeedRoute,
    Elevation,
    Terrain,
    Access,
    Closure,
    Road,
    Hydrology,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeoBounds {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

impl GeoBounds {
    #[must_use]
    pub const fn new(west: f64, south: f64, east: f64, north: f64) -> Self {
        Self {
            west,
            south,
            east,
            north,
        }
    }

    #[must_use]
    pub fn is_valid(self) -> bool {
        self.west >= -180.0
            && self.east <= 180.0
            && self.south >= -90.0
            && self.north <= 90.0
            && self.west < self.east
            && self.south < self.north
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourcePriority {
    Required,
    Recommended,
    Optional,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceCoverageStatus {
    Satisfied,
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceAdapter {
    pub id: String,
    pub kind: SourceKind,
    pub consumes: Vec<String>,
    pub produces: Vec<String>,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceRecommendation {
    pub kind: SourceKind,
    pub priority: SourcePriority,
    pub adapter_ids: Vec<String>,
    pub suggested_paths: Vec<String>,
    #[serde(default)]
    pub acquisition_hints: Vec<AcquisitionHint>,
    pub search_terms: Vec<String>,
    pub acceptance: String,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<GeoBounds>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AcquisitionHint {
    pub label: String,
    pub url: String,
    pub formats: Vec<String>,
    pub note: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceCandidate {
    pub path: String,
    pub kind: SourceKind,
    pub adapter_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<SourceFingerprint>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceFingerprint {
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceCoverage {
    pub kind: SourceKind,
    pub priority: SourcePriority,
    pub status: SourceCoverageStatus,
    pub candidate_paths: Vec<String>,
    pub implemented_adapter_ids: Vec<String>,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceCoverageTally {
    pub total: usize,
    pub satisfied: usize,
    pub missing: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceCoverageSummary {
    pub total: SourceCoverageTally,
    pub required: SourceCoverageTally,
    pub recommended: SourceCoverageTally,
    pub optional: SourceCoverageTally,
    pub missing_required: Vec<SourceKind>,
    pub missing_recommended: Vec<SourceKind>,
}

impl SourceCoverageTally {
    const fn record(&mut self, status: SourceCoverageStatus) {
        self.total += 1;
        match status {
            SourceCoverageStatus::Satisfied => self.satisfied += 1,
            SourceCoverageStatus::Missing => self.missing += 1,
        }
    }
}

impl SourceCoverageSummary {
    #[must_use]
    pub const fn required_complete(&self) -> bool {
        self.required.missing == 0
    }

    #[must_use]
    pub const fn recommended_complete(&self) -> bool {
        self.recommended.missing == 0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceManifest {
    pub adapters: Vec<SourceAdapter>,
    #[serde(default)]
    pub recommendations: Vec<SourceRecommendation>,
    #[serde(default)]
    pub coverage: Vec<SourceCoverage>,
    pub candidates: Vec<SourceCandidate>,
}

#[must_use]
pub fn summarize_source_coverage(coverage: &[SourceCoverage]) -> SourceCoverageSummary {
    let mut summary = SourceCoverageSummary::default();
    for entry in coverage {
        summary.total.record(entry.status);
        match entry.priority {
            SourcePriority::Required => {
                summary.required.record(entry.status);
                record_gap(entry, &mut summary.missing_required);
            }
            SourcePriority::Recommended => {
                summary.recommended.record(entry.status);
                record_gap(entry, &mut summary.missing_recommended);
            }
            SourcePriority::Optional => summary.optional.record(entry.status),
        }
    }
    summary.missing_required.sort();
    summary.missing_recommended.sort();
    summary
}

fn record_gap(entry: &SourceCoverage, missing: &mut Vec<SourceKind>) {
    if entry.status == SourceCoverageStatus::Missing {
        missing.push(entry.kind);
    }
}

#[must_use]
pub fn source_coverage(
    adapters: &[SourceAdapter],
    recommendations: &[SourceRecommendation],
    candidates: &[SourceCandidate],
) -> Vec<SourceCoverage> {
    let adapter_ids = adapters
        .iter()
        .map(|adapter| adapter.id.as_str())
        .collect::<BTreeSet<_>>();
    recommendations
        .iter()
        .map(|recommendation| coverage_for_recommendation(recommendation, candidates, &adapter_ids))
        .collect()
}

fn coverage_for_recommendation(
    recommendation: &SourceRecommendation,
    candidates: &[SourceCandidate],
    adapter_ids: &BTreeSet<&str>,
) -> SourceCoverage {
    let matching = candidates
        .iter()
        .filter(|candidate| {
            candidate.kind == recommendation.kind
                && recommendation
                    .adapter_ids
                    .iter()
                    .any(|id| id == &candidate.adapter_id)
        })
        .collect::<Vec<_>>();
    let candidate_paths = matching
        .iter()
        .map(|candidate| candidate.path.clone())
        .collect::<Vec<_>>();
    let implemented_adapter_ids = matching
        .iter()
        .filter(|candidate| adapter_ids.contains(candidate.adapter_id.as_str()))
        .map(|candidate| candidate.adapter_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let status = if implemented_adapter_ids.is_empty() {
        SourceCoverageStatus::Missing
    } else {
        SourceCoverageStatus::Satisfied
    };
    SourceCoverage {
        kind: recommendation.kind,
        priority: recommendation.priority,
        status,
        candidate_paths,
        implemented_adapter_ids,
        message: coverage_message(recommendation, status),
    }
}

fn coverage_message(recommendation: &SourceRecommendation, status: SourceCoverageStatus) -> String {
    match status {
        SourceCoverageStatus::Satisfied => {
            format!(
                "{:?} source requirement has implemented candidate(s).",
                recommendation.kind
            )
        }
        SourceCoverageStatus::Missing => format!(
            "{:?} source is {:?}; acquire one of {}.",
            recommendation.kind,
            recommendation.priority,
            recommendation.suggested_paths.join(", ")
        ),
    }
}

#[must_use]
pub fn adapter_registry() -> Vec<SourceAdapter> {
    let mut adapters = network_adapters();
    adapters.extend(route_adapters());
    adapters.extend(elevation_adapters());
    adapters.extend(overlay_context_adapters());
    adapters
}

fn network_adapters() -> Vec<SourceAdapter> {
    vec![
        adapter(
            "geojson-network",
            SourceKind::TrailNetwork,
            ["geojson", "json"],
            ["SegmentDraft", "TrailGraph"],
            "Provider-neutral LineString and MultiLineString network ingestion.",
        ),
        adapter(
            "shapefile-network",
            SourceKind::TrailNetwork,
            ["shp", "dbf", "shx"],
            ["SegmentDraft", "TrailGraph"],
            "Official/agency polyline shapefile trail-network ingestion with DBF attribute normalization.",
        ),
        adapter(
            "osm-xml-network",
            SourceKind::TrailNetwork,
            ["osm"],
            ["SegmentDraft", "TrailGraph"],
            "OSM XML walkable-way trail-network ingestion with access, surface, direction, and provenance normalization.",
        ),
        adapter(
            "osm-pbf-network",
            SourceKind::TrailNetwork,
            ["osm.pbf"],
            ["SegmentDraft", "TrailGraph"],
            "OSM PBF extract ingestion for walkable ways with access, surface, direction, and ODbL provenance normalization.",
        ),
    ]
}

fn route_adapters() -> Vec<SourceAdapter> {
    vec![
        adapter(
            "geojson-route",
            SourceKind::SeedRoute,
            ["geojson"],
            ["LineString", "snapped route metrics"],
            "GeoJSON seed route import.",
        ),
        adapter(
            "json-route",
            SourceKind::SeedRoute,
            ["json"],
            ["LineString", "snapped route metrics"],
            "Provider-neutral route JSON import for coordinate arrays and point-object app exports.",
        ),
        adapter(
            "gpx-route",
            SourceKind::SeedRoute,
            ["gpx"],
            ["LineString", "snapped route metrics"],
            "GPX route import/export, including user-supplied app exports.",
        ),
        adapter(
            "csv-route",
            SourceKind::SeedRoute,
            ["csv"],
            ["LineString", "snapped route metrics"],
            "CSV lon/lat/elevation route import/export for manual app exchange.",
        ),
        adapter(
            "kml-route",
            SourceKind::SeedRoute,
            ["kml", "kmz"],
            ["LineString", "snapped route metrics"],
            "KML/KMZ route import/export for manual map-app exchange.",
        ),
    ]
}

fn elevation_adapters() -> Vec<SourceAdapter> {
    vec![
        adapter(
            "arc-ascii-elevation",
            SourceKind::Elevation,
            ["asc"],
            ["sampled elevation profile", "edge ascent/descent"],
            "Arc/Info ASCII Grid DEM sampling for local elevation enrichment.",
        ),
        adapter(
            "geotiff-elevation",
            SourceKind::Elevation,
            ["tif", "tiff"],
            ["sampled elevation profile", "edge ascent/descent"],
            "Affine WGS84/NAD83, EPSG:3857, or WGS84/NAD83 UTM single-band GeoTIFF DEM sampling.",
        ),
        adapter(
            "vrt-elevation",
            SourceKind::Elevation,
            ["vrt"],
            ["sampled elevation profile", "edge ascent/descent"],
            "GDAL VRT SimpleSource DEM wrapper with affine WGS84/NAD83, EPSG:3857, or WGS84/NAD83 UTM GeoTransform sampling.",
        ),
    ]
}

fn overlay_context_adapters() -> Vec<SourceAdapter> {
    vec![
        adapter(
            "geojson-terrain-overlay",
            SourceKind::Terrain,
            ["geojson", "json"],
            ["terrain overrides", "confidence/provenance"],
            "GeoJSON land-cover, surface, or user terrain overlays applied after graph construction.",
        ),
        adapter(
            "shapefile-terrain-overlay",
            SourceKind::Terrain,
            ["shp", "dbf", "shx"],
            ["terrain overrides", "confidence/provenance"],
            "Polygon or line shapefile land-cover, surface, or terrain overlays applied after graph construction.",
        ),
        adapter(
            "geojson-access-overlay",
            SourceKind::Access,
            ["geojson", "json"],
            ["access overrides", "confidence/provenance"],
            "GeoJSON access/status overlay applied after graph construction.",
        ),
        adapter(
            "shapefile-access-overlay",
            SourceKind::Access,
            ["shp", "dbf", "shx"],
            ["access overrides", "confidence/provenance"],
            "Polygon or line shapefile access/status overlay applied after graph construction.",
        ),
        adapter(
            "geojson-closure-overlay",
            SourceKind::Closure,
            ["geojson", "json"],
            ["access overrides", "confidence/provenance"],
            "GeoJSON closure/restriction overlay applied after graph construction.",
        ),
        adapter(
            "geojson-road-context",
            SourceKind::Road,
            ["geojson", "json"],
            ["road crossings", "road exposure hints"],
            "GeoJSON road/street context lines used to infer trail crossings.",
        ),
        adapter(
            "shapefile-road-context",
            SourceKind::Road,
            ["shp", "dbf", "shx"],
            ["road crossings", "road exposure hints"],
            "Shapefile road/street centerlines used to infer trail crossings.",
        ),
        adapter(
            "osm-road-context",
            SourceKind::Road,
            ["osm", "osm.pbf"],
            ["road crossings", "road exposure hints"],
            "OSM XML/PBF highway centerlines used to infer trail crossings and road exposure.",
        ),
        adapter(
            "geojson-hydrology-context",
            SourceKind::Hydrology,
            ["geojson", "json"],
            ["water crossings"],
            "GeoJSON stream/river context lines used to infer water crossings.",
        ),
        adapter(
            "shapefile-hydrology-context",
            SourceKind::Hydrology,
            ["shp", "dbf", "shx"],
            ["water crossings"],
            "Shapefile stream/river centerlines used to infer water crossings.",
        ),
        adapter(
            "osm-hydrology-context",
            SourceKind::Hydrology,
            ["osm", "osm.pbf"],
            ["water crossings"],
            "OSM XML/PBF waterway linework used to infer stream, river, canal, drain, and ditch crossings.",
        ),
        adapter(
            "shapefile-closure-layer",
            SourceKind::Closure,
            ["shp", "dbf", "shx"],
            ["access overrides", "confidence/provenance"],
            "Official park/agency shapefile closure and restriction overlays.",
        ),
    ]
}

fn adapter<const C: usize, const P: usize>(
    id: &str,
    kind: SourceKind,
    consumes: [&str; C],
    produces: [&str; P],
    note: &str,
) -> SourceAdapter {
    SourceAdapter {
        id: id.to_owned(),
        kind,
        consumes: consumes.into_iter().map(str::to_owned).collect(),
        produces: produces.into_iter().map(str::to_owned).collect(),
        note: note.to_owned(),
    }
}

#[must_use]
pub fn discovery_recommendations(area: Option<GeoBounds>) -> Vec<SourceRecommendation> {
    RECOMMENDATION_SPECS
        .iter()
        .map(|spec| spec.materialize(area))
        .collect()
}

struct RecommendationSpec {
    kind: SourceKind,
    priority: SourcePriority,
    adapter_ids: &'static [&'static str],
    suggested_paths: &'static [&'static str],
    acquisition_hints: &'static [AcquisitionHintSpec],
    search_terms: &'static [&'static str],
    acceptance: &'static str,
    rationale: &'static str,
}

struct AcquisitionHintSpec {
    label: &'static str,
    url: &'static str,
    formats: &'static [&'static str],
    note: &'static str,
}

impl RecommendationSpec {
    fn materialize(&self, area: Option<GeoBounds>) -> SourceRecommendation {
        SourceRecommendation {
            kind: self.kind,
            priority: self.priority,
            adapter_ids: strings(self.adapter_ids),
            suggested_paths: strings(self.suggested_paths),
            acquisition_hints: self
                .acquisition_hints
                .iter()
                .map(AcquisitionHintSpec::materialize)
                .collect(),
            search_terms: strings(self.search_terms),
            acceptance: self.acceptance.to_owned(),
            rationale: self.rationale.to_owned(),
            area,
        }
    }
}

impl AcquisitionHintSpec {
    fn materialize(&self) -> AcquisitionHint {
        AcquisitionHint {
            label: self.label.to_owned(),
            url: self.url.to_owned(),
            formats: strings(self.formats),
            note: self.note.to_owned(),
        }
    }
}

fn strings(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|x| (*x).to_owned()).collect()
}

const TRAIL_NETWORK_HINTS: &[AcquisitionHintSpec] = &[
    AcquisitionHintSpec {
        label: "NPS official GIS open data",
        url: "https://www.nps.gov/subjects/gisandmapping/tools-and-data.htm",
        formats: &["GeoJSON", "Shapefile", "Feature Service"],
        note: "Use first for National Park Service units; cache exported trail linework under sources/ with provenance intact.",
    },
    AcquisitionHintSpec {
        label: "USFS geospatial data discovery",
        url: "https://data-usfs.hub.arcgis.com/",
        formats: &["Shapefile", "File Geodatabase", "Feature Service"],
        note: "Use for National Forest roads/trails and agency-managed transportation layers before falling back to volunteered data.",
    },
    AcquisitionHintSpec {
        label: "Geofabrik OpenStreetMap extracts",
        url: "https://download.geofabrik.de/",
        formats: &["OSM PBF", "OSM XML after conversion", "Shapefile"],
        note: "Use as a broad fallback extract, then filter hiking paths/tracks directly from OSM PBF or cache a normalized OSM XML/GeoJSON/shapefile artifact.",
    },
];

const ELEVATION_HINTS: &[AcquisitionHintSpec] = &[
    AcquisitionHintSpec {
        label: "USGS The National Map Downloader",
        url: "https://www.usgs.gov/tools/download-data-maps-national-map",
        formats: &["GeoTIFF", "IMG"],
        note: "Use 3DEP DEM products for United States AOIs; prefer GeoTIFF tiles that cover the graph envelope.",
    },
    AcquisitionHintSpec {
        label: "USGS TNMAccess API",
        url: "https://apps.nationalmap.gov/tnmaccess/",
        formats: &["GeoTIFF", "JSON metadata"],
        note: "Use for scripted 3DEP product discovery by bounding box before caching selected raster downloads.",
    },
];

const TERRAIN_HINTS: &[AcquisitionHintSpec] = &[
    AcquisitionHintSpec {
        label: "MRLC NLCD data",
        url: "https://www.mrlc.gov/data",
        formats: &["GeoTIFF", "Raster service"],
        note: "Use for broad land-cover evidence, then convert relevant classes into terrain overlays with explicit confidence.",
    },
    AcquisitionHintSpec {
        label: "Agency surface or land-cover GIS",
        url: "https://data-usfs.hub.arcgis.com/",
        formats: &["Shapefile", "GeoJSON", "Feature Service"],
        note: "Prefer local agency surface, land-cover, or trail-condition attributes when available.",
    },
];

const CLOSURE_HINTS: &[AcquisitionHintSpec] = &[
    AcquisitionHintSpec {
        label: "Agency closure and alert GIS",
        url: "https://public-nps.opendata.arcgis.com/",
        formats: &["GeoJSON", "Shapefile", "Feature Service"],
        note: "Use current official closure/restriction features; preserve dates, weekdays, hours, direction rules, and alert provenance in cached overlays.",
    },
    AcquisitionHintSpec {
        label: "Local park or forest alerts",
        url: "https://www.nps.gov/subjects/gisandmapping/tools-and-data.htm",
        formats: &["GeoJSON", "Shapefile", "Web page"],
        note: "When no machine layer exists, hand-normalize official closure geometry into a small GeoJSON overlay.",
    },
];

const ACCESS_HINTS: &[AcquisitionHintSpec] = &[
    AcquisitionHintSpec {
        label: "USGS PAD-US data download",
        url: "https://www.usgs.gov/programs/gap-analysis-project/science/pad-us-data-download",
        formats: &["File Geodatabase", "Shapefile"],
        note: "Use protected-area ownership/manager data as access context; normalize to open/restricted/private where justified.",
    },
    AcquisitionHintSpec {
        label: "PAD-US protected areas overview",
        url: "https://www.usgs.gov/programs/gap-analysis-project/science/protected-areas",
        formats: &["Metadata", "Download links"],
        note: "Use to understand PAD-US scope before treating ownership as a route legality signal.",
    },
];

const ROAD_HINTS: &[AcquisitionHintSpec] = &[
    AcquisitionHintSpec {
        label: "USFS roads data",
        url: "https://data.fs.usda.gov/geodata/edw/datasets.php?dsetCategory=transportation",
        formats: &["Shapefile", "File Geodatabase", "Map service"],
        note: "Use for National Forest road exposure and crossings; cache centerlines as road context.",
    },
    AcquisitionHintSpec {
        label: "The National Map transportation",
        url: "https://apps.nationalmap.gov/tnmaccess/",
        formats: &["Shapefile", "GeoPackage", "JSON metadata"],
        note: "Use TNM transportation products when local road centerlines are absent.",
    },
    AcquisitionHintSpec {
        label: "Geofabrik OpenStreetMap roads",
        url: "https://download.geofabrik.de/",
        formats: &["OSM PBF", "Shapefile"],
        note: "Use as a fallback road/street extract, then filter and normalize to context linework.",
    },
];

const HYDROLOGY_HINTS: &[AcquisitionHintSpec] = &[
    AcquisitionHintSpec {
        label: "USGS National Hydrography products",
        url: "https://www.usgs.gov/national-hydrography/access-national-hydrography-products",
        formats: &["Shapefile", "File Geodatabase"],
        note: "Use NHD/3DHP stream and waterbody linework to infer water crossings.",
    },
    AcquisitionHintSpec {
        label: "The National Map hydrography",
        url: "https://apps.nationalmap.gov/tnmaccess/",
        formats: &["Shapefile", "File Geodatabase", "JSON metadata"],
        note: "Use TNMAccess to locate hydrography products by AOI before caching selected linework.",
    },
];

const SEED_ROUTE_HINTS: &[AcquisitionHintSpec] = &[
    AcquisitionHintSpec {
        label: "AllTrails import/export support",
        url: "https://support.alltrails.com/hc/en-us/sections/360006411352-Importing-and-exporting-files",
        formats: &["GPX", "GeoJSON", "KML", "KMZ", "CSV"],
        note: "Use user-supplied exports as seed routes only; never couple core graph semantics to private AllTrails APIs.",
    },
    AcquisitionHintSpec {
        label: "Personal GPS archives",
        url: "file://local-user-supplied-routes",
        formats: &["GPX", "GeoJSON", "KML", "KMZ", "CSV"],
        note: "Cache completed hikes under sources/seeds or import them directly so provenance and fingerprints are preserved.",
    },
];

const RECOMMENDATION_SPECS: &[RecommendationSpec] = &[
    RecommendationSpec {
        kind: SourceKind::TrailNetwork,
        priority: SourcePriority::Required,
        adapter_ids: &[
            "geojson-network",
            "shapefile-network",
            "osm-xml-network",
            "osm-pbf-network",
        ],
        suggested_paths: &[
            "sources/trails.geojson",
            "sources/network.geojson",
            "sources/trails.shp",
            "sources/osm-trails.osm",
            "sources/osm-trails.osm.pbf",
        ],
        acquisition_hints: TRAIL_NETWORK_HINTS,
        search_terms: &[
            "official trail GIS line layer",
            "OSM hiking path extract",
            "park trail network GeoJSON",
        ],
        acceptance: "LineString or MultiLineString trail geometries covering the AOI, with names, access/surface tags when available, and enough topology to build junctions.",
        rationale: "The normalized graph cannot exist without a routable trail network.",
    },
    RecommendationSpec {
        kind: SourceKind::Elevation,
        priority: SourcePriority::Required,
        adapter_ids: &["arc-ascii-elevation", "geotiff-elevation", "vrt-elevation"],
        suggested_paths: &["sources/dem.asc", "sources/dem.tif"],
        acquisition_hints: ELEVATION_HINTS,
        search_terms: &[
            "USGS 3DEP DEM",
            "Copernicus DEM",
            "local elevation raster for hiking area",
        ],
        acceptance: "DEM coverage intersects every trail edge; vertical units and CRS are documented before enrichment.",
        rationale: "Long-day route quality depends on ascent, descent, grade, and sustained steepness.",
    },
    RecommendationSpec {
        kind: SourceKind::Terrain,
        priority: SourcePriority::Recommended,
        adapter_ids: &["geojson-terrain-overlay", "shapefile-terrain-overlay"],
        suggested_paths: &[
            "sources/terrain.geojson",
            "sources/landcover.geojson",
            "sources/terrain.shp",
        ],
        acquisition_hints: TERRAIN_HINTS,
        search_terms: &[
            "land cover polygons",
            "trail surface GIS layer",
            "alpine talus scramble terrain map",
        ],
        acceptance: "Terrain or surface features can be normalized into known buckets and carry confidence/provenance.",
        rationale: "Terrain multipliers are inspectable only when roughness evidence is explicit instead of magical.",
    },
    RecommendationSpec {
        kind: SourceKind::Closure,
        priority: SourcePriority::Recommended,
        adapter_ids: &["geojson-closure-overlay", "shapefile-closure-layer"],
        suggested_paths: &[
            "sources/closures.geojson",
            "sources/access.geojson",
            "sources/closures.shp",
        ],
        acquisition_hints: CLOSURE_HINTS,
        search_terms: &[
            "official trail closure layer",
            "park access restriction GIS",
            "seasonal closure boundary GeoJSON",
        ],
        acceptance: "Closure, private, restricted, open, and directional statuses can be attached to graph edges with temporal provenance.",
        rationale: "A beautiful generated loop is trash if it crosses a closed trail or forbidden parcel.",
    },
    RecommendationSpec {
        kind: SourceKind::Access,
        priority: SourcePriority::Recommended,
        adapter_ids: &["geojson-access-overlay", "shapefile-access-overlay"],
        suggested_paths: &[
            "sources/access.geojson",
            "sources/ownership.geojson",
            "sources/access.shp",
        ],
        acquisition_hints: ACCESS_HINTS,
        search_terms: &[
            "public access boundary GeoJSON",
            "land ownership parcel open space GIS",
            "park access status trail layer",
        ],
        acceptance: "Open, restricted, private, or unknown access statuses can be attached to graph edges with provenance.",
        rationale: "Access and ownership boundaries are distinct from temporary closures and should be visible in route legality diagnostics.",
    },
    RecommendationSpec {
        kind: SourceKind::Road,
        priority: SourcePriority::Recommended,
        adapter_ids: &[
            "geojson-road-context",
            "shapefile-road-context",
            "osm-road-context",
        ],
        suggested_paths: &[
            "sources/roads.geojson",
            "sources/context-roads.geojson",
            "sources/roads.shp",
            "sources/roads.osm.pbf",
        ],
        acquisition_hints: ROAD_HINTS,
        search_terms: &[
            "road centerline GeoJSON",
            "street context lines",
            "OSM road extract",
        ],
        acceptance: "Road context lines cover the AOI and can identify crossings or road-exposed trail segments.",
        rationale: "Road exposure and road crossings are hard constraints for many hikes.",
    },
    RecommendationSpec {
        kind: SourceKind::Hydrology,
        priority: SourcePriority::Recommended,
        adapter_ids: &[
            "geojson-hydrology-context",
            "shapefile-hydrology-context",
            "osm-hydrology-context",
        ],
        suggested_paths: &[
            "sources/hydrology.geojson",
            "sources/streams.geojson",
            "sources/hydrology.shp",
            "sources/hydrology.osm.pbf",
        ],
        acquisition_hints: HYDROLOGY_HINTS,
        search_terms: &[
            "NHD stream lines",
            "hydrology GeoJSON",
            "river creek crossing layer",
        ],
        acceptance: "Hydrology linework intersects likely crossings and carries source confidence where known.",
        rationale: "Water crossings are route diagnostics, risk signals, and useful report context.",
    },
    RecommendationSpec {
        kind: SourceKind::SeedRoute,
        priority: SourcePriority::Optional,
        adapter_ids: &[
            "gpx-route",
            "geojson-route",
            "json-route",
            "csv-route",
            "kml-route",
        ],
        suggested_paths: &[
            "sources/seeds/completed.gpx",
            "sources/seeds/completed.csv",
            "sources/seeds/alltrails-export.gpx",
            "sources/seeds/reference.geojson",
            "sources/seeds/app-export.json",
        ],
        acquisition_hints: SEED_ROUTE_HINTS,
        search_terms: &[
            "personal completed hike GPX",
            "AllTrails export GPX",
            "app route JSON export",
            "reference route KML",
        ],
        acceptance: "Seed routes snap to the current graph and their provenance is preserved.",
        rationale: "Seeds improve confidence/popularity hints and provide validation loops without contaminating the provider-neutral model.",
    },
];

#[must_use]
pub fn classify_path(path: &Path) -> Option<SourceCandidate> {
    let path_lc = path.display().to_string().to_ascii_lowercase();
    let ext = if path_lc.ends_with(".osm.pbf") {
        "osm.pbf".to_owned()
    } else {
        path.extension()?.to_str()?.to_ascii_lowercase()
    };
    let (kind, adapter_id) = match ext.as_str() {
        "gpx" => (SourceKind::SeedRoute, "gpx-route"),
        "csv" => (SourceKind::SeedRoute, "csv-route"),
        "kml" | "kmz" => (SourceKind::SeedRoute, "kml-route"),
        "json"
            if path_lc.contains("route")
                || path_lc.contains("track")
                || path_lc.contains("seed")
                || path_lc.contains("activity") =>
        {
            (SourceKind::SeedRoute, "json-route")
        }
        "geojson" | "json" if path_lc.contains("closure") => {
            (SourceKind::Closure, "geojson-closure-overlay")
        }
        "geojson" | "json" if path_lc.contains("access") => {
            (SourceKind::Access, "geojson-access-overlay")
        }
        "geojson" | "json"
            if path_lc.contains("terrain")
                || path_lc.contains("surface")
                || path_lc.contains("landcover")
                || path_lc.contains("land-cover")
                || path_lc.contains("land_cover") =>
        {
            (SourceKind::Terrain, "geojson-terrain-overlay")
        }
        "geojson" | "json" if path_lc.contains("road") => {
            (SourceKind::Road, "geojson-road-context")
        }
        "geojson" | "json"
            if path_lc.contains("hydrology")
                || path_lc.contains("water")
                || path_lc.contains("stream") =>
        {
            (SourceKind::Hydrology, "geojson-hydrology-context")
        }
        "geojson" | "json" => (SourceKind::TrailNetwork, "geojson-network"),
        "osm" | "osm.pbf"
            if path_lc.contains("road")
                || path_lc.contains("street")
                || path_lc.contains("highway") =>
        {
            (SourceKind::Road, "osm-road-context")
        }
        "osm" | "osm.pbf"
            if path_lc.contains("hydrology")
                || path_lc.contains("water")
                || path_lc.contains("stream")
                || path_lc.contains("river")
                || path_lc.contains("creek") =>
        {
            (SourceKind::Hydrology, "osm-hydrology-context")
        }
        "osm" => (SourceKind::TrailNetwork, "osm-xml-network"),
        "osm.pbf" => (SourceKind::TrailNetwork, "osm-pbf-network"),
        "asc" => (SourceKind::Elevation, "arc-ascii-elevation"),
        "tif" | "tiff" => (SourceKind::Elevation, "geotiff-elevation"),
        "vrt" => (SourceKind::Elevation, "vrt-elevation"),
        "shp" if path_lc.contains("closure") => (SourceKind::Closure, "shapefile-closure-layer"),
        "shp" if path_lc.contains("access") || path_lc.contains("ownership") => {
            (SourceKind::Access, "shapefile-access-overlay")
        }
        "shp"
            if path_lc.contains("terrain")
                || path_lc.contains("surface")
                || path_lc.contains("landcover")
                || path_lc.contains("land-cover")
                || path_lc.contains("land_cover") =>
        {
            (SourceKind::Terrain, "shapefile-terrain-overlay")
        }
        "shp" if path_lc.contains("road") || path_lc.contains("street") => {
            (SourceKind::Road, "shapefile-road-context")
        }
        "shp"
            if path_lc.contains("hydrology")
                || path_lc.contains("water")
                || path_lc.contains("stream") =>
        {
            (SourceKind::Hydrology, "shapefile-hydrology-context")
        }
        "shp" => (SourceKind::TrailNetwork, "shapefile-network"),
        _ => return None,
    };
    Some(SourceCandidate {
        path: path.display().to_string(),
        kind,
        adapter_id: adapter_id.to_owned(),
        origin: None,
        fingerprint: None,
    })
}
