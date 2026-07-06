use serde::{Deserialize, Serialize};

const ALLTRAILS_UPLOAD_URL: &str =
    "https://support.alltrails.com/hc/en-us/articles/37228498475028-Uploading-files-to-AllTrails";
const ALLTRAILS_DOWNLOAD_URL: &str = "https://support.alltrails.com/hc/en-us/articles/37230403315476-Downloading-files-from-AllTrails";
const ALLTRAILS_EXCHANGE_URL: &str =
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

impl AllTrailsExchange {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ImportUserExport => "import-user-export",
            Self::ManualUploadCustomRoute => "manual-upload-custom-route",
            Self::ManualUploadActivity => "manual-upload-activity",
            Self::DirectWriteApi => "direct-write-api",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrailgenExchangeAction {
    ImportSeed,
    ExportGeneratedRoute,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AllTrailsCapability {
    pub exchange: AllTrailsExchange,
    pub status: BridgeStatus,
    pub formats: Vec<RouteExchangeFormat>,
    pub verified_on: String,
    pub workflow: String,
    pub source_url: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AllTrailsRequest {
    pub exchange: AllTrailsExchange,
    pub format: RouteExchangeFormat,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AllTrailsPlan {
    pub exchange: AllTrailsExchange,
    pub status: BridgeStatus,
    pub format: RouteExchangeFormat,
    pub trailgen_action: TrailgenExchangeAction,
    pub trailgen_template: String,
    pub verified_on: String,
    pub workflow: String,
    pub source_url: String,
}

pub trait AllTrailsBridge {
    fn capabilities(&self) -> Vec<AllTrailsCapability>;

    fn plan(&self, request: AllTrailsRequest) -> AllTrailsPlan {
        let caps = self.capabilities();
        let cap = caps
            .iter()
            .find(|cap| cap.exchange == request.exchange && cap.formats.contains(&request.format));
        cap.map_or_else(
            || AllTrailsPlan {
                exchange: request.exchange,
                status: BridgeStatus::Undocumented,
                format: request.format,
                trailgen_action: TrailgenExchangeAction::Unsupported,
                trailgen_template: String::new(),
                verified_on: ALLTRAILS_POLICY_VERIFIED_ON.to_owned(),
                workflow: unsupported_workflow(request),
                source_url: ALLTRAILS_EXCHANGE_URL.to_owned(),
            },
            |cap| AllTrailsPlan {
                exchange: cap.exchange,
                status: cap.status,
                format: request.format,
                trailgen_action: trailgen_action(request.exchange),
                trailgen_template: trailgen_template(request.exchange, request.format),
                verified_on: cap.verified_on.clone(),
                workflow: cap.workflow.clone(),
                source_url: cap.source_url.clone(),
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ManualAllTrailsBridge;

impl AllTrailsBridge for ManualAllTrailsBridge {
    fn capabilities(&self) -> Vec<AllTrailsCapability> {
        vec![
            capability(
                AllTrailsExchange::ImportUserExport,
                BridgeStatus::Supported,
                vec![
                    RouteExchangeFormat::Gpx,
                    RouteExchangeFormat::Geojson,
                    RouteExchangeFormat::Json,
                    RouteExchangeFormat::Kml,
                    RouteExchangeFormat::Kmz,
                    RouteExchangeFormat::Csv,
                ],
                "User downloads an AllTrails activity, trail, or custom-route file and imports it locally.",
                ALLTRAILS_DOWNLOAD_URL,
            ),
            capability(
                AllTrailsExchange::ManualUploadCustomRoute,
                BridgeStatus::Manual,
                vec![
                    RouteExchangeFormat::Gpx,
                    RouteExchangeFormat::Kml,
                    RouteExchangeFormat::Kmz,
                    RouteExchangeFormat::Csv,
                ],
                "User uploads a generated route through AllTrails Build custom route → Upload a route.",
                ALLTRAILS_UPLOAD_URL,
            ),
            capability(
                AllTrailsExchange::ManualUploadActivity,
                BridgeStatus::Manual,
                vec![
                    RouteExchangeFormat::Gpx,
                    RouteExchangeFormat::Kml,
                    RouteExchangeFormat::Kmz,
                    RouteExchangeFormat::Csv,
                ],
                "User uploads a generated route to the AllTrails activities list on the website.",
                ALLTRAILS_UPLOAD_URL,
            ),
            capability(
                AllTrailsExchange::DirectWriteApi,
                BridgeStatus::Undocumented,
                Vec::new(),
                "No documented public route-write API was found; private endpoints are intentionally unsupported.",
                ALLTRAILS_EXCHANGE_URL,
            ),
        ]
    }
}

fn capability(
    exchange: AllTrailsExchange,
    status: BridgeStatus,
    formats: Vec<RouteExchangeFormat>,
    workflow: &str,
    source_url: &str,
) -> AllTrailsCapability {
    AllTrailsCapability {
        exchange,
        status,
        formats,
        verified_on: ALLTRAILS_POLICY_VERIFIED_ON.to_owned(),
        workflow: workflow.to_owned(),
        source_url: source_url.to_owned(),
    }
}

const fn trailgen_action(exchange: AllTrailsExchange) -> TrailgenExchangeAction {
    match exchange {
        AllTrailsExchange::ImportUserExport => TrailgenExchangeAction::ImportSeed,
        AllTrailsExchange::ManualUploadCustomRoute | AllTrailsExchange::ManualUploadActivity => {
            TrailgenExchangeAction::ExportGeneratedRoute
        }
        AllTrailsExchange::DirectWriteApi => TrailgenExchangeAction::Unsupported,
    }
}

fn trailgen_template(exchange: AllTrailsExchange, format: RouteExchangeFormat) -> String {
    let ext = format.extension();
    match exchange {
        AllTrailsExchange::ImportUserExport => {
            format!("trailgen import-seed <project> --route alltrails-export.{ext} --name <name>")
        }
        AllTrailsExchange::ManualUploadCustomRoute | AllTrailsExchange::ManualUploadActivity => {
            format!(
                "trailgen export <project> --route candidate-1 --format {ext} --output candidate-1.{ext}"
            )
        }
        AllTrailsExchange::DirectWriteApi => String::new(),
    }
}

fn unsupported_workflow(request: AllTrailsRequest) -> String {
    format!(
        "{} does not document a {} exchange path; use a supported manual route format or keep this behind a future bridge implementation.",
        request.exchange.label(),
        request.format.extension()
    )
}
