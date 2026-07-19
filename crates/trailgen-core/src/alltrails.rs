use serde::{Deserialize, Serialize};

const UPLOAD_URL: &str =
    "https://support.alltrails.com/hc/en-us/articles/37228498475028-Uploading-files-to-AllTrails";
const DOWNLOAD_URL: &str = "https://support.alltrails.com/hc/en-us/articles/37230403315476-Downloading-files-from-AllTrails";
const EXCHANGE_URL: &str =
    "https://support.alltrails.com/hc/en-us/sections/360006411352-Importing-and-exporting-files";
pub const ALLTRAILS_POLICY_VERIFIED_ON: &str = "2026-07-06";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AllTrailsExchange {
    ImportUserExport,
    ManualUploadCustomRoute,
    ManualUploadActivity,
    DirectWriteApi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BridgeStatus {
    Supported,
    Manual,
    Undocumented,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RouteExchangeFormat {
    Csv,
    Geojson,
    Gpx,
    Json,
    Kml,
    Kmz,
}

impl RouteExchangeFormat {
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Geojson => "geojson",
            Self::Gpx => "gpx",
            Self::Json => "json",
            Self::Kml => "kml",
            Self::Kmz => "kmz",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrailgenExchangeAction {
    ImportSeed,
    ExportGeneratedRoute,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AllTrailsPlan {
    pub exchange: AllTrailsExchange,
    pub status: BridgeStatus,
    pub format: RouteExchangeFormat,
    pub trailgen_action: TrailgenExchangeAction,
    pub trailgen_template: String,
    pub verified_on: &'static str,
    pub workflow: &'static str,
    pub source_url: &'static str,
}

/// Describe the documented, deliberately manual `AllTrails` exchange surface.
#[must_use]
pub fn alltrails_plan(exchange: AllTrailsExchange, format: RouteExchangeFormat) -> AllTrailsPlan {
    use AllTrailsExchange::{
        DirectWriteApi, ImportUserExport, ManualUploadActivity, ManualUploadCustomRoute,
    };

    let (status, action, workflow, source_url) = match exchange {
        ImportUserExport => (
            BridgeStatus::Supported,
            TrailgenExchangeAction::ImportSeed,
            "Download an AllTrails activity, trail, or custom route and import it locally.",
            DOWNLOAD_URL,
        ),
        ManualUploadCustomRoute if upload_format(format) => (
            BridgeStatus::Manual,
            TrailgenExchangeAction::ExportGeneratedRoute,
            "Upload the generated file through Build custom route → Upload a route.",
            UPLOAD_URL,
        ),
        ManualUploadActivity if upload_format(format) => (
            BridgeStatus::Manual,
            TrailgenExchangeAction::ExportGeneratedRoute,
            "Upload the generated file to the activities list on the website.",
            UPLOAD_URL,
        ),
        DirectWriteApi | ManualUploadCustomRoute | ManualUploadActivity => (
            BridgeStatus::Undocumented,
            TrailgenExchangeAction::Unsupported,
            "No documented exchange path exists for this operation and format.",
            EXCHANGE_URL,
        ),
    };
    let ext = format.extension();
    let trailgen_template = match action {
        TrailgenExchangeAction::ImportSeed => {
            format!("trailgen import-seed <project> --route alltrails-export.{ext} --name <name>")
        }
        TrailgenExchangeAction::ExportGeneratedRoute => format!(
            "trailgen export <project> --route candidate-1 --format {ext} --output candidate-1.{ext}"
        ),
        TrailgenExchangeAction::Unsupported => String::new(),
    };
    AllTrailsPlan {
        exchange,
        status,
        format,
        trailgen_action: action,
        trailgen_template,
        verified_on: ALLTRAILS_POLICY_VERIFIED_ON,
        workflow,
        source_url,
    }
}

/// Enumerate every documented exchange plus the deliberately unsupported API row.
#[must_use]
pub fn alltrails_plans() -> Vec<AllTrailsPlan> {
    use AllTrailsExchange::{
        DirectWriteApi, ImportUserExport, ManualUploadActivity, ManualUploadCustomRoute,
    };
    use RouteExchangeFormat::{Csv, Geojson, Gpx, Json, Kml, Kmz};
    [
        (ImportUserExport, Gpx),
        (ImportUserExport, Geojson),
        (ImportUserExport, Json),
        (ImportUserExport, Kml),
        (ImportUserExport, Kmz),
        (ImportUserExport, Csv),
        (ManualUploadCustomRoute, Gpx),
        (ManualUploadCustomRoute, Kml),
        (ManualUploadCustomRoute, Kmz),
        (ManualUploadCustomRoute, Csv),
        (ManualUploadActivity, Gpx),
        (ManualUploadActivity, Kml),
        (ManualUploadActivity, Kmz),
        (ManualUploadActivity, Csv),
        (DirectWriteApi, Gpx),
    ]
    .into_iter()
    .map(|(exchange, format)| alltrails_plan(exchange, format))
    .collect()
}

const fn upload_format(format: RouteExchangeFormat) -> bool {
    matches!(
        format,
        RouteExchangeFormat::Csv
            | RouteExchangeFormat::Gpx
            | RouteExchangeFormat::Kml
            | RouteExchangeFormat::Kmz
    )
}
