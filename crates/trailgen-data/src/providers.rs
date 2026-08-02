use crate::{MAX_REGION_DEG2, MAX_SOURCE_BYTES, SurveyRegion, provider_client};
use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};
use std::{env, fmt, str::FromStr, time::Duration};
use trailgen_core::{ContextOverlay, SegmentDraft, io::geojson, source::GeoBounds};

pub const DEFAULT_USGS_TRAILS_ENDPOINT: &str =
    "https://cartowfs.nationalmap.gov/arcgis/rest/services/transportation/MapServer/8/query";
pub const DEFAULT_NY_STATE_PARKS_ENDPOINT: &str = "https://services.arcgis.com/1xFZPtKn1wKC6POA/arcgis/rest/services/NY_State_Parks_Trails/FeatureServer/0/query";
pub const DEFAULT_TEXAS_STATE_PARKS_ENDPOINT: &str =
    "https://tpwd.texas.gov/arcgis/rest/services/Parks/TexasStateParksTrails/MapServer/0/query";
const USGS_PAGE_SIZE: usize = 2_000;
const AUTHORITY_PAGE_SIZE: usize = 2_000;
const NEW_YORK_BOUNDS: GeoBounds = GeoBounds::new(-79.77, 40.47, -71.75, 45.02);
const TEXAS_BOUNDS: GeoBounds = GeoBounds::new(-106.66, 25.83, -93.50, 36.51);
const NY_LICENSE: &str = "NYS OPRHP informational and non-commercial use; attribution required";
const TEXAS_LICENSE: &str = "TPWD public trail data; informational use; attribution TPWD|SP|NR|PGR";

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn new(raw: impl Into<String>) -> Result<Self> {
        let raw = raw.into();
        ensure!(
            !raw.is_empty()
                && raw.len() <= 64
                && raw
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
            "provider id must contain only lowercase ASCII letters, digits, or hyphens"
        );
        Ok(Self(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ProviderId {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        Self::new(raw)
    }
}

impl Serialize for ProviderId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProviderId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderDescriptor {
    pub id: ProviderId,
    pub label: &'static str,
    pub adapter_revision: u16,
    pub precedence: u16,
    pub extension: &'static str,
    pub request_extension: &'static str,
}

#[derive(Clone, Debug)]
pub struct ProviderPayload {
    pub bytes: Vec<u8>,
    pub request: String,
    pub origin: String,
}

#[derive(Clone, Copy)]
pub struct RawShard<'a> {
    pub region: &'a SurveyRegion,
    pub bytes: &'a [u8],
}

#[derive(Default)]
pub struct NormalizedNetwork {
    pub drafts: Vec<SegmentDraft>,
    pub context: Vec<ContextOverlay>,
}

pub trait NetworkProvider {
    fn descriptor(&self) -> ProviderDescriptor;
    fn covers(&self, _bounds: GeoBounds) -> bool {
        true
    }
    fn acquire(&self, bounds: trailgen_core::source::GeoBounds) -> Result<ProviderPayload>;
    fn normalize(&self, shards: &[RawShard<'_>]) -> Result<NormalizedNetwork>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Authority {
    NewYork,
    Texas,
}

impl Authority {
    fn descriptor(self) -> ProviderDescriptor {
        let (id, label) = match self {
            Self::NewYork => ("ny-state-parks", "New York State Parks"),
            Self::Texas => ("texas-state-parks", "Texas State Parks"),
        };
        ProviderDescriptor {
            id: ProviderId::new(id).expect("static provider id is valid"),
            label,
            adapter_revision: 1,
            precedence: 0,
            extension: "geojson",
            request_extension: "request",
        }
    }

    const fn bounds(self) -> GeoBounds {
        match self {
            Self::NewYork => NEW_YORK_BOUNDS,
            Self::Texas => TEXAS_BOUNDS,
        }
    }

    const fn where_clause(self) -> &'static str {
        match self {
            Self::NewYork => "Public_='Y' AND Foot='Y' AND (Status IS NULL OR Status<>'Proposed')",
            Self::Texas => "Official='Yes' AND TrailUse LIKE '%Hiking%'",
        }
    }

    const fn out_fields(self) -> &'static str {
        match self {
            Self::NewYork => {
                "OBJECTID,Unit,Facility,Asset,Sub_Asset,Name,Alt_Name,Abbreviation,Blaze,Blaze_2,Blaze_3,Map_Blaze,Map_Blaze_2,Public_,Status,Surface,Foot,Miles,MID,ParksApp,GlobalID"
            }
            Self::Texas => "OBJECTID,ParkName,Official,Name1,TrailUse,LengthMI,GlobalID",
        }
    }
}

/// A bounded, authority-owned `ArcGIS` trail service admitted into the automatic
/// corpus with a provider-native normalization law.
#[derive(Clone, Debug)]
pub struct AuthorityTrailProvider {
    authority: Authority,
    endpoint: String,
    timeout: Duration,
}

impl AuthorityTrailProvider {
    #[must_use]
    pub fn new_york() -> Self {
        Self::new_york_at(
            env::var("TRAILGEN_NY_STATE_PARKS_ENDPOINT")
                .unwrap_or_else(|_| DEFAULT_NY_STATE_PARKS_ENDPOINT.to_owned()),
            Duration::from_secs(90),
        )
    }

    #[must_use]
    pub fn texas() -> Self {
        Self::texas_at(
            env::var("TRAILGEN_TEXAS_STATE_PARKS_ENDPOINT")
                .unwrap_or_else(|_| DEFAULT_TEXAS_STATE_PARKS_ENDPOINT.to_owned()),
            Duration::from_secs(90),
        )
    }

    #[must_use]
    pub fn new_york_at(endpoint: impl Into<String>, timeout: Duration) -> Self {
        Self {
            authority: Authority::NewYork,
            endpoint: endpoint.into(),
            timeout,
        }
    }

    #[must_use]
    pub fn texas_at(endpoint: impl Into<String>, timeout: Duration) -> Self {
        Self {
            authority: Authority::Texas,
            endpoint: endpoint.into(),
            timeout,
        }
    }

    fn fetch_page(
        &self,
        client: &reqwest::blocking::Client,
        bounds: GeoBounds,
        offset: usize,
    ) -> Result<Value> {
        let descriptor = self.descriptor();
        let page = client
            .get(&self.endpoint)
            .query(&authority_query(self.authority, bounds, offset))
            .send()
            .with_context(|| format!("query {} through {}", descriptor.label, self.endpoint))?
            .error_for_status()
            .with_context(|| format!("{} endpoint returned an HTTP error", descriptor.label))?
            .json::<Value>()
            .with_context(|| format!("decode {} GeoJSON", descriptor.label))?;
        if let Some(error) = page.get("error") {
            bail!("{} endpoint rejected the query: {error}", descriptor.label);
        }
        Ok(page)
    }
}

impl NetworkProvider for AuthorityTrailProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        self.authority.descriptor()
    }

    fn covers(&self, bounds: GeoBounds) -> bool {
        intersects(self.authority.bounds(), bounds)
    }

    fn acquire(&self, bounds: GeoBounds) -> Result<ProviderPayload> {
        ensure!(bounds.is_valid(), "invalid authority trail-data bounds");
        let area = (bounds.east - bounds.west) * (bounds.north - bounds.south);
        ensure!(
            area <= MAX_REGION_DEG2,
            "authority trail-data bounds span {area:.2} square degrees; limit is {MAX_REGION_DEG2:.2}"
        );
        if !self.covers(bounds) {
            return Ok(ProviderPayload {
                bytes: empty_feature_collection()?,
                request: format!(
                    "outside {} coverage; bbox={},{},{},{}",
                    self.descriptor().label,
                    bounds.west,
                    bounds.south,
                    bounds.east,
                    bounds.north
                ),
                origin: self.endpoint.clone(),
            });
        }
        let client = provider_client(self.descriptor().id.as_str(), self.timeout)
            .with_context(|| format!("build {} client", self.descriptor().label))?;
        let mut features = Vec::new();
        let mut encoded_feature_bytes = 0_u64;
        for offset in (0..).step_by(AUTHORITY_PAGE_SIZE) {
            let page = self.fetch_page(&client, bounds, offset)?;
            let mut page_features = page
                .get("features")
                .and_then(Value::as_array)
                .cloned()
                .with_context(|| {
                    format!(
                        "{} response is not a GeoJSON FeatureCollection",
                        self.descriptor().label
                    )
                })?;
            let page_len = page_features.len();
            encoded_feature_bytes = encoded_feature_bytes
                .checked_add(serde_json::to_vec(&page_features)?.len() as u64)
                .context("authority trail response size overflow")?;
            ensure!(
                encoded_feature_bytes <= MAX_SOURCE_BYTES,
                "{} response exceeds {} MiB",
                self.descriptor().label,
                MAX_SOURCE_BYTES / 1_048_576
            );
            features.append(&mut page_features);
            if page_len < AUTHORITY_PAGE_SIZE {
                break;
            }
        }
        let bytes = serde_json::to_vec(&json!({
            "type": "FeatureCollection",
            "features": features,
        }))?;
        ensure!(
            bytes.len() as u64 <= MAX_SOURCE_BYTES,
            "{} response exceeds {} MiB",
            self.descriptor().label,
            MAX_SOURCE_BYTES / 1_048_576
        );
        Ok(ProviderPayload {
            bytes,
            request: format!(
                "bbox={},{},{},{}; where={}; out_fields={}; page_size={AUTHORITY_PAGE_SIZE}",
                bounds.west,
                bounds.south,
                bounds.east,
                bounds.north,
                self.authority.where_clause(),
                self.authority.out_fields()
            ),
            origin: self.endpoint.clone(),
        })
    }

    fn normalize(&self, shards: &[RawShard<'_>]) -> Result<NormalizedNetwork> {
        let mut drafts = Vec::new();
        for shard in shards {
            let mut root = serde_json::from_slice::<Value>(shard.bytes).with_context(|| {
                format!("parse sequestered {} GeoJSON", self.descriptor().label)
            })?;
            let features = root
                .get_mut("features")
                .and_then(Value::as_array_mut)
                .with_context(|| {
                    format!(
                        "{} receipt is not a GeoJSON FeatureCollection",
                        self.descriptor().label
                    )
                })?;
            let mut admitted = Vec::with_capacity(features.len());
            for mut feature in std::mem::take(features) {
                let keep = match self.authority {
                    Authority::NewYork => normalize_new_york(&mut feature)?,
                    Authority::Texas => normalize_texas(&mut feature)?,
                };
                if keep {
                    admitted.push(feature);
                }
            }
            *features = admitted;
            drafts.extend(
                geojson::network_from_str(&serde_json::to_string(&root)?)
                    .with_context(|| format!("normalize {} geometry", self.descriptor().label))?,
            );
        }
        Ok(NormalizedNetwork {
            drafts,
            context: Vec::new(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct UsgsNationalTrails {
    endpoint: String,
    timeout: Duration,
}

impl Default for UsgsNationalTrails {
    fn default() -> Self {
        Self {
            endpoint: env::var("TRAILGEN_USGS_TRAILS_ENDPOINT")
                .unwrap_or_else(|_| DEFAULT_USGS_TRAILS_ENDPOINT.to_owned()),
            timeout: Duration::from_secs(90),
        }
    }
}

impl UsgsNationalTrails {
    #[must_use]
    pub fn new(endpoint: impl Into<String>, timeout: Duration) -> Self {
        Self {
            endpoint: endpoint.into(),
            timeout,
        }
    }

    fn fetch_page(
        &self,
        client: &reqwest::blocking::Client,
        bounds: trailgen_core::source::GeoBounds,
        offset: usize,
    ) -> Result<Value> {
        client
            .get(&self.endpoint)
            .query(&usgs_query(bounds, offset))
            .send()
            .with_context(|| {
                format!(
                    "query USGS National Digital Trails through {}",
                    self.endpoint
                )
            })?
            .error_for_status()
            .with_context(|| {
                format!(
                    "USGS National Digital Trails endpoint {} returned an error",
                    self.endpoint
                )
            })?
            .json()
            .context("decode USGS National Digital Trails GeoJSON")
    }
}

impl NetworkProvider for UsgsNationalTrails {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: ProviderId::new("usgs-national-trails").expect("static provider id is valid"),
            label: "USGS National Digital Trails",
            adapter_revision: 1,
            precedence: 20,
            extension: "geojson",
            request_extension: "request",
        }
    }

    fn acquire(&self, bounds: trailgen_core::source::GeoBounds) -> Result<ProviderPayload> {
        let area = (bounds.east - bounds.west) * (bounds.north - bounds.south);
        ensure!(bounds.is_valid(), "invalid USGS trail-data bounds");
        ensure!(
            area <= MAX_REGION_DEG2,
            "USGS trail-data bounds span {area:.2} square degrees; limit is {MAX_REGION_DEG2:.2}"
        );
        let client = provider_client("usgs-trail-source", self.timeout)
            .context("build USGS National Digital Trails client")?;
        let mut features = Vec::new();
        let mut encoded_feature_bytes = 0_u64;
        for offset in (0..).step_by(USGS_PAGE_SIZE) {
            let page = self.fetch_page(&client, bounds, offset)?;
            let mut page_features = page
                .get("features")
                .and_then(Value::as_array)
                .cloned()
                .context("USGS response is not a GeoJSON FeatureCollection")?;
            let page_len = page_features.len();
            encoded_feature_bytes = encoded_feature_bytes
                .checked_add(serde_json::to_vec(&page_features)?.len() as u64)
                .context("USGS trail response size overflow")?;
            ensure!(
                encoded_feature_bytes <= MAX_SOURCE_BYTES,
                "USGS trail response exceeds {} MiB",
                MAX_SOURCE_BYTES / 1_048_576
            );
            features.append(&mut page_features);
            if page_len < USGS_PAGE_SIZE {
                break;
            }
        }
        let bytes = serde_json::to_vec(&json!({
            "type": "FeatureCollection",
            "features": features,
        }))?;
        ensure!(
            bytes.len() as u64 <= MAX_SOURCE_BYTES,
            "USGS trail response exceeds {} MiB",
            MAX_SOURCE_BYTES / 1_048_576
        );
        Ok(ProviderPayload {
            bytes,
            request: format!(
                "bbox={},{},{},{}; where=trailtype='Terra Trail' and hikerpedestrian='Y'; page_size={USGS_PAGE_SIZE}",
                bounds.west, bounds.south, bounds.east, bounds.north
            ),
            origin: self.endpoint.clone(),
        })
    }

    fn normalize(&self, shards: &[RawShard<'_>]) -> Result<NormalizedNetwork> {
        let mut drafts = Vec::new();
        for shard in shards {
            let mut root = serde_json::from_slice::<Value>(shard.bytes)
                .context("parse sequestered USGS trail GeoJSON")?;
            let features = root
                .get_mut("features")
                .and_then(Value::as_array_mut)
                .context("USGS receipt is not a GeoJSON FeatureCollection")?;
            for feature in features {
                normalize_usgs_properties(feature)?;
            }
            drafts.extend(
                geojson::network_from_str(&serde_json::to_string(&root)?)
                    .context("normalize USGS trail geometry")?,
            );
        }
        Ok(NormalizedNetwork {
            drafts,
            context: Vec::new(),
        })
    }
}

fn usgs_query(
    bounds: trailgen_core::source::GeoBounds,
    offset: usize,
) -> Vec<(&'static str, String)> {
    vec![
        ("f", "geojson".to_owned()),
        (
            "where",
            "trailtype='Terra Trail' AND hikerpedestrian='Y'".to_owned(),
        ),
        (
            "geometry",
            format!(
                "{},{},{},{}",
                bounds.west, bounds.south, bounds.east, bounds.north
            ),
        ),
        ("geometryType", "esriGeometryEnvelope".to_owned()),
        ("inSR", "4326".to_owned()),
        ("spatialRel", "esriSpatialRelIntersects".to_owned()),
        (
            "outFields",
            "objectid,permanentidentifier,name,namealternate,trailnumber,sourcefeatureid,sourcedatasetid,sourceoriginator,publisheddate,sourceeditdate,trailsurface,routetype,trailtype,hikerpedestrian".to_owned(),
        ),
        ("outSR", "4326".to_owned()),
        ("returnGeometry", "true".to_owned()),
        ("orderByFields", "objectid".to_owned()),
        ("resultOffset", offset.to_string()),
        ("resultRecordCount", USGS_PAGE_SIZE.to_string()),
    ]
}

fn normalize_usgs_properties(feature: &mut Value) -> Result<()> {
    let properties = feature
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .context("USGS feature has no properties")?;
    let id = string_property(properties, "permanentidentifier")
        .or_else(|| string_property(properties, "sourcefeatureid"))
        .or_else(|| properties.get("objectid").map(Value::to_string));
    let originator = string_property(properties, "sourceoriginator");
    let dataset = string_property(properties, "sourcedatasetid");
    let layer = match (originator, dataset) {
        (Some(originator), Some(dataset)) => Some(format!("{originator} · {dataset}")),
        (originator, dataset) => originator.or(dataset),
    };
    let surface = string_property(properties, "trailsurface");
    properties.insert("source".to_owned(), json!("usgs-national-trails"));
    properties.insert("license".to_owned(), json!("USGS public domain"));
    properties.insert("way_kind".to_owned(), json!("path"));
    properties.insert("trail_standing".to_owned(), json!("established"));
    properties.insert("terrain".to_owned(), json!("trail"));
    properties.insert("access".to_owned(), json!("unknown"));
    properties.insert("confidence".to_owned(), json!(0.86));
    if let Some(id) = id {
        properties.insert("id".to_owned(), Value::String(id));
    }
    if let Some(layer) = layer {
        properties.insert("layer".to_owned(), Value::String(layer));
    }
    if let Some(surface) = surface {
        properties.insert("surface".to_owned(), Value::String(surface));
    }
    Ok(())
}

fn authority_query(
    authority: Authority,
    bounds: GeoBounds,
    offset: usize,
) -> Vec<(&'static str, String)> {
    vec![
        ("f", "geojson".to_owned()),
        ("where", authority.where_clause().to_owned()),
        (
            "geometry",
            format!(
                "{},{},{},{}",
                bounds.west, bounds.south, bounds.east, bounds.north
            ),
        ),
        ("geometryType", "esriGeometryEnvelope".to_owned()),
        ("inSR", "4326".to_owned()),
        ("spatialRel", "esriSpatialRelIntersects".to_owned()),
        ("outFields", authority.out_fields().to_owned()),
        ("outSR", "4326".to_owned()),
        ("returnGeometry", "true".to_owned()),
        ("orderByFields", "OBJECTID".to_owned()),
        ("resultOffset", offset.to_string()),
        ("resultRecordCount", AUTHORITY_PAGE_SIZE.to_string()),
    ]
}

fn normalize_new_york(feature: &mut Value) -> Result<bool> {
    let properties = feature
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .context("New York State Parks feature has no properties")?;
    if !property_is(properties, "Public_", "Y")
        || !property_is(properties, "Foot", "Y")
        || property_is(properties, "Status", "Proposed")
    {
        return Ok(false);
    }
    let Some(asset) = properties.get("Asset").and_then(Value::as_i64) else {
        return Ok(false);
    };
    let (class, terrain, marking, road_exposure) = match asset {
        0 => ("path", "trail", "unmarked", 0.0),
        1 => ("path", "trail", "marked", 0.0),
        2 => ("track", "road", "unknown", 0.85),
        3 => ("road", "road", "unknown", 1.0),
        4 => ("footway", "pavement", "unknown", 0.0),
        _ => return Ok(false),
    };
    let access = match string_property(properties, "Status")
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("open") => "open",
        Some("closed") => "closed",
        _ => "unknown",
    };
    let id = string_property(properties, "MID")
        .or_else(|| string_property(properties, "GlobalID"))
        .or_else(|| properties.get("OBJECTID").map(Value::to_string));
    let layer = joined_properties(properties, &["Facility", "Unit"]);
    let surface = string_property(properties, "Surface");

    properties.insert("source".to_owned(), json!("ny-state-parks"));
    properties.insert("license".to_owned(), json!(NY_LICENSE));
    properties.insert("way_kind".to_owned(), json!(class));
    properties.insert("trail_standing".to_owned(), json!("established"));
    properties.insert("trail_marking".to_owned(), json!(marking));
    properties.insert("terrain".to_owned(), json!(terrain));
    properties.insert("access".to_owned(), json!(access));
    properties.insert("road_exposure".to_owned(), json!(road_exposure));
    properties.insert("confidence".to_owned(), json!(0.97));
    if let Some(id) = id {
        properties.insert("id".to_owned(), Value::String(id));
    }
    if let Some(layer) = layer {
        properties.insert("layer".to_owned(), Value::String(layer));
    }
    if let Some(surface) = surface {
        properties.insert("surface".to_owned(), Value::String(surface));
    }
    Ok(true)
}

fn normalize_texas(feature: &mut Value) -> Result<bool> {
    let properties = feature
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .context("Texas State Parks feature has no properties")?;
    let hiking = string_property(properties, "TrailUse")
        .is_some_and(|uses| uses.to_ascii_lowercase().contains("hiking"));
    if !property_is(properties, "Official", "Yes") || !hiking {
        return Ok(false);
    }
    let id = string_property(properties, "GlobalID")
        .or_else(|| properties.get("OBJECTID").map(Value::to_string));
    let layer = string_property(properties, "ParkName");

    properties.insert("source".to_owned(), json!("texas-state-parks"));
    properties.insert("license".to_owned(), json!(TEXAS_LICENSE));
    properties.insert("way_kind".to_owned(), json!("path"));
    properties.insert("trail_standing".to_owned(), json!("established"));
    properties.insert("trail_marking".to_owned(), json!("unknown"));
    properties.insert("terrain".to_owned(), json!("trail"));
    properties.insert("access".to_owned(), json!("unknown"));
    properties.insert("road_exposure".to_owned(), json!(0.0));
    properties.insert("confidence".to_owned(), json!(0.95));
    if let Some(id) = id {
        properties.insert("id".to_owned(), Value::String(id));
    }
    if let Some(layer) = layer {
        properties.insert("layer".to_owned(), Value::String(layer));
    }
    Ok(true)
}

fn empty_feature_collection() -> Result<Vec<u8>> {
    serde_json::to_vec(&json!({
        "type": "FeatureCollection",
        "features": [],
    }))
    .context("encode empty authority receipt")
}

fn intersects(left: GeoBounds, right: GeoBounds) -> bool {
    left.west < right.east
        && right.west < left.east
        && left.south < right.north
        && right.south < left.north
}

fn property_is(properties: &Map<String, Value>, key: &str, expected: &str) -> bool {
    string_property(properties, key).is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn joined_properties(properties: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    let fields = keys
        .iter()
        .filter_map(|key| string_property(properties, key))
        .collect::<Vec<_>>();
    (!fields.is_empty()).then(|| fields.join(" · "))
}

fn string_property(properties: &Map<String, Value>, key: &str) -> Option<String> {
    properties
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trailgen_core::{Access, Terrain, TrailMarking, TrailStanding, WayKind};

    #[test]
    fn provider_ids_are_safe_path_atoms() {
        assert_eq!(
            ProviderId::new("usgs-national-trails").unwrap().as_str(),
            "usgs-national-trails"
        );
        assert!(ProviderId::new("../escape").is_err());
        assert!(ProviderId::new("USGS").is_err());
    }

    #[test]
    fn usgs_normalization_preserves_identity_and_marks_standing() -> Result<()> {
        let region = SurveyRegion::new(trailgen_core::source::GeoBounds::new(
            -74.2, 41.1, -74.0, 41.3,
        ))?;
        let provider = UsgsNationalTrails::default();
        let normalized = provider.normalize(&[RawShard {
            region: &region,
            bytes: include_bytes!("../tests/fixtures/tiny-usgs-trails.geojson"),
        }])?;
        assert_eq!(normalized.drafts.len(), 1);
        let draft = &normalized.drafts[0];
        assert_eq!(draft.standing, TrailStanding::Established);
        assert_eq!(draft.access, Access::Unknown);
        assert_eq!(draft.provenance[0].source, "usgs-national-trails");
        assert_eq!(
            draft.provenance[0].source_id.as_deref(),
            Some("usgs-fixture-1")
        );
        Ok(())
    }

    #[test]
    fn new_york_admits_only_public_current_foot_geometry() -> Result<()> {
        let region = SurveyRegion::new(GeoBounds::new(-74.2, 41.1, -74.0, 41.3))?;
        let provider = AuthorityTrailProvider::new_york();
        let normalized = provider.normalize(&[RawShard {
            region: &region,
            bytes: include_bytes!("../tests/fixtures/tiny-ny-state-parks.geojson"),
        }])?;

        assert_eq!(normalized.drafts.len(), 2);
        let unmarked = &normalized.drafts[0];
        assert_eq!(unmarked.way_kind, WayKind::Path);
        assert_eq!(unmarked.standing, TrailStanding::Established);
        assert_eq!(unmarked.marking, TrailMarking::Unmarked);
        assert_eq!(unmarked.terrain, Terrain::Trail);
        assert_eq!(unmarked.access, Access::Open);
        assert_eq!(
            unmarked.provenance[0].source_id.as_deref(),
            Some("08020RT10414")
        );
        assert_eq!(
            unmarked.provenance[0].layer.as_deref(),
            Some("Harriman State Park · Palisades")
        );
        assert!(
            unmarked.provenance[0]
                .license
                .as_deref()
                .is_some_and(|license| license.contains("non-commercial"))
        );

        let closed = &normalized.drafts[1];
        assert_eq!(closed.marking, TrailMarking::Marked);
        assert_eq!(closed.access, Access::Closed);
        Ok(())
    }

    #[test]
    fn texas_admits_only_official_hiking_geometry() -> Result<()> {
        let region = SurveyRegion::new(GeoBounds::new(-98.6, 29.4, -98.3, 29.7))?;
        let provider = AuthorityTrailProvider::texas();
        let normalized = provider.normalize(&[RawShard {
            region: &region,
            bytes: include_bytes!("../tests/fixtures/tiny-texas-state-parks.geojson"),
        }])?;

        assert_eq!(normalized.drafts.len(), 1);
        let trail = &normalized.drafts[0];
        assert_eq!(trail.way_kind, WayKind::Path);
        assert_eq!(trail.standing, TrailStanding::Established);
        assert_eq!(trail.marking, TrailMarking::Unknown);
        assert_eq!(trail.access, Access::Unknown);
        assert_eq!(trail.provenance[0].source_id.as_deref(), Some("tx-hiking"));
        assert_eq!(
            trail.provenance[0].layer.as_deref(),
            Some("Government Canyon")
        );
        Ok(())
    }

    #[test]
    fn state_authorities_do_not_contact_foreign_rectangles() -> Result<()> {
        let texas =
            AuthorityTrailProvider::texas_at("http://127.0.0.1:1/never", Duration::from_millis(1));
        let new_york = GeoBounds::new(-74.2, 41.1, -74.0, 41.3);

        assert!(!texas.covers(new_york));
        let payload = texas.acquire(new_york)?;
        let root = serde_json::from_slice::<Value>(&payload.bytes)?;
        assert_eq!(
            root.get("features").and_then(Value::as_array).map(Vec::len),
            Some(0)
        );
        Ok(())
    }
}
