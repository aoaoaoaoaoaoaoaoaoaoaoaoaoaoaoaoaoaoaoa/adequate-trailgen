use serde::{Deserialize, Serialize};
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
pub enum AdapterStatus {
    Implemented,
    Planned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourcePriority {
    Required,
    Recommended,
    Optional,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceAdapter {
    pub id: String,
    pub kind: SourceKind,
    pub status: AdapterStatus,
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
    pub search_terms: Vec<String>,
    pub acceptance: String,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<GeoBounds>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceCandidate {
    pub path: String,
    pub kind: SourceKind,
    pub adapter_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<SourceFingerprint>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceFingerprint {
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceManifest {
    pub adapters: Vec<SourceAdapter>,
    #[serde(default)]
    pub recommendations: Vec<SourceRecommendation>,
    pub candidates: Vec<SourceCandidate>,
}

#[must_use]
pub fn adapter_registry() -> Vec<SourceAdapter> {
    vec![
        adapter(
            "geojson-network",
            SourceKind::TrailNetwork,
            AdapterStatus::Implemented,
            ["geojson", "json"],
            ["SegmentDraft", "TrailGraph"],
            "Provider-neutral LineString and MultiLineString network ingestion.",
        ),
        adapter(
            "geojson-route",
            SourceKind::SeedRoute,
            AdapterStatus::Implemented,
            ["geojson", "json"],
            ["LineString", "snapped route metrics"],
            "User-supplied seed route import.",
        ),
        adapter(
            "gpx-route",
            SourceKind::SeedRoute,
            AdapterStatus::Implemented,
            ["gpx", "csv"],
            ["LineString", "snapped route metrics"],
            "GPX and CSV route import/export, including user-supplied app exports.",
        ),
        adapter(
            "kml-route",
            SourceKind::SeedRoute,
            AdapterStatus::Implemented,
            ["kml", "kmz"],
            ["LineString", "snapped route metrics"],
            "KML/KMZ route import/export for manual map-app exchange.",
        ),
        adapter(
            "arc-ascii-elevation",
            SourceKind::Elevation,
            AdapterStatus::Implemented,
            ["asc"],
            ["sampled elevation profile", "edge ascent/descent"],
            "Arc/Info ASCII Grid DEM sampling for local elevation enrichment.",
        ),
        adapter(
            "geospatial-elevation-raster",
            SourceKind::Elevation,
            AdapterStatus::Planned,
            ["tif", "tiff", "vrt"],
            ["sampled elevation profile", "edge ascent/descent"],
            "USGS/3DEP, Copernicus DEM, or local GeoTIFF/VRT sampling seam.",
        ),
        adapter(
            "geojson-terrain-overlay",
            SourceKind::Terrain,
            AdapterStatus::Implemented,
            ["geojson", "json"],
            ["terrain overrides", "confidence/provenance"],
            "GeoJSON land-cover, surface, or user terrain overlays applied after graph construction.",
        ),
        adapter(
            "geojson-access-overlay",
            SourceKind::Access,
            AdapterStatus::Implemented,
            ["geojson", "json"],
            ["access overrides", "confidence/provenance"],
            "GeoJSON access/status overlay applied after graph construction.",
        ),
        adapter(
            "geojson-closure-overlay",
            SourceKind::Closure,
            AdapterStatus::Implemented,
            ["geojson", "json"],
            ["access overrides", "confidence/provenance"],
            "GeoJSON closure/restriction overlay applied after graph construction.",
        ),
        adapter(
            "geojson-road-context",
            SourceKind::Road,
            AdapterStatus::Implemented,
            ["geojson", "json"],
            ["road crossings", "road exposure hints"],
            "GeoJSON road/street context lines used to infer trail crossings.",
        ),
        adapter(
            "geojson-hydrology-context",
            SourceKind::Hydrology,
            AdapterStatus::Implemented,
            ["geojson", "json"],
            ["water crossings"],
            "GeoJSON stream/river context lines used to infer water crossings.",
        ),
        adapter(
            "shapefile-closure-layer",
            SourceKind::Closure,
            AdapterStatus::Planned,
            ["shp"],
            ["access overrides", "confidence/provenance"],
            "Future official park/agency shapefile closure and restriction layers.",
        ),
    ]
}

fn adapter<const C: usize, const P: usize>(
    id: &str,
    kind: SourceKind,
    status: AdapterStatus,
    consumes: [&str; C],
    produces: [&str; P],
    note: &str,
) -> SourceAdapter {
    SourceAdapter {
        id: id.to_owned(),
        kind,
        status,
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
    search_terms: &'static [&'static str],
    acceptance: &'static str,
    rationale: &'static str,
}

impl RecommendationSpec {
    fn materialize(&self, area: Option<GeoBounds>) -> SourceRecommendation {
        SourceRecommendation {
            kind: self.kind,
            priority: self.priority,
            adapter_ids: strings(self.adapter_ids),
            suggested_paths: strings(self.suggested_paths),
            search_terms: strings(self.search_terms),
            acceptance: self.acceptance.to_owned(),
            rationale: self.rationale.to_owned(),
            area,
        }
    }
}

fn strings(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|x| (*x).to_owned()).collect()
}

const RECOMMENDATION_SPECS: &[RecommendationSpec] = &[
    RecommendationSpec {
        kind: SourceKind::TrailNetwork,
        priority: SourcePriority::Required,
        adapter_ids: &["geojson-network"],
        suggested_paths: &["sources/trails.geojson", "sources/network.geojson"],
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
        adapter_ids: &["arc-ascii-elevation", "geospatial-elevation-raster"],
        suggested_paths: &["sources/dem.asc", "sources/dem.tif"],
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
        adapter_ids: &["geojson-terrain-overlay"],
        suggested_paths: &["sources/terrain.geojson", "sources/landcover.geojson"],
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
        suggested_paths: &["sources/closures.geojson", "sources/access.geojson"],
        search_terms: &[
            "official trail closure layer",
            "park access restriction GIS",
            "seasonal closure boundary GeoJSON",
        ],
        acceptance: "Closure, private, restricted, and open statuses can be attached to graph edges with dated provenance.",
        rationale: "A beautiful generated loop is trash if it crosses a closed trail or forbidden parcel.",
    },
    RecommendationSpec {
        kind: SourceKind::Road,
        priority: SourcePriority::Recommended,
        adapter_ids: &["geojson-road-context"],
        suggested_paths: &["sources/roads.geojson", "sources/context-roads.geojson"],
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
        adapter_ids: &["geojson-hydrology-context"],
        suggested_paths: &["sources/hydrology.geojson", "sources/streams.geojson"],
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
        adapter_ids: &["gpx-route", "geojson-route", "kml-route"],
        suggested_paths: &[
            "sources/seeds/completed.gpx",
            "sources/seeds/alltrails-export.gpx",
            "sources/seeds/reference.geojson",
        ],
        search_terms: &[
            "personal completed hike GPX",
            "AllTrails export GPX",
            "reference route KML",
        ],
        acceptance: "Seed routes snap to the current graph and their provenance is preserved.",
        rationale: "Seeds improve confidence/popularity hints and provide validation loops without contaminating the provider-neutral model.",
    },
];

#[must_use]
pub fn classify_path(path: &Path) -> Option<SourceCandidate> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let path_lc = path.display().to_string().to_ascii_lowercase();
    let (kind, adapter_id) = match ext.as_str() {
        "gpx" | "csv" => (SourceKind::SeedRoute, "gpx-route"),
        "kml" | "kmz" => (SourceKind::SeedRoute, "kml-route"),
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
        "asc" => (SourceKind::Elevation, "arc-ascii-elevation"),
        "tif" | "tiff" | "vrt" => (SourceKind::Elevation, "geospatial-elevation-raster"),
        _ => return None,
    };
    Some(SourceCandidate {
        path: path.display().to_string(),
        kind,
        adapter_id: adapter_id.to_owned(),
        fingerprint: None,
    })
}
