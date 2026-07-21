use crate::{MAX_REGION_DEG2, MAX_SOURCE_BYTES, SurveyRegion, user_agent};
use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};
use std::{env, fmt, str::FromStr, time::Duration};
use trailgen_core::{ContextOverlay, SegmentDraft, io::geojson};

pub const DEFAULT_USGS_TRAILS_ENDPOINT: &str =
    "https://cartowfs.nationalmap.gov/arcgis/rest/services/transportation/MapServer/8/query";
const USGS_PAGE_SIZE: usize = 2_000;

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
    fn acquire(&self, bounds: trailgen_core::source::GeoBounds) -> Result<ProviderPayload>;
    fn normalize(&self, shards: &[RawShard<'_>]) -> Result<NormalizedNetwork>;
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
            precedence: 10,
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
        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .user_agent(user_agent("usgs-trail-source"))
            .build()
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
    properties.insert("trail_class".to_owned(), json!("path"));
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
    use trailgen_core::{Access, TrailStanding};

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
}
