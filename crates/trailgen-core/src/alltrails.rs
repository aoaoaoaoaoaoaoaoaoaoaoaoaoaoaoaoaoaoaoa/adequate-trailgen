use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AllTrailsCapability {
    pub exchange: AllTrailsExchange,
    pub status: BridgeStatus,
    pub formats: Vec<String>,
    pub workflow: String,
    pub source_url: String,
}

pub trait AllTrailsBridge {
    fn capabilities(&self) -> Vec<AllTrailsCapability>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ManualAllTrailsBridge;

impl AllTrailsBridge for ManualAllTrailsBridge {
    fn capabilities(&self) -> Vec<AllTrailsCapability> {
        vec![
            AllTrailsCapability {
                exchange: AllTrailsExchange::ImportUserExport,
                status: BridgeStatus::Supported,
                formats: vec![
                    "gpx".to_owned(),
                    "geojson".to_owned(),
                    "json".to_owned(),
                    "kml".to_owned(),
                    "kmz".to_owned(),
                    "csv".to_owned(),
                ],
                workflow: "User downloads an AllTrails activity, trail, or custom-route file and imports it locally."
                    .to_owned(),
                source_url:
                    "https://support.alltrails.com/hc/en-us/articles/37230403315476-Downloading-files-from-AllTrails"
                        .to_owned(),
            },
            AllTrailsCapability {
                exchange: AllTrailsExchange::ManualUploadCustomRoute,
                status: BridgeStatus::Manual,
                formats: vec![
                    "gpx".to_owned(),
                    "kml".to_owned(),
                    "kmz".to_owned(),
                    "csv".to_owned(),
                ],
                workflow: "User uploads a generated route through AllTrails Build custom route → Upload a route."
                    .to_owned(),
                source_url:
                    "https://support.alltrails.com/hc/en-us/articles/37228498475028-Uploading-files-to-AllTrails"
                        .to_owned(),
            },
            AllTrailsCapability {
                exchange: AllTrailsExchange::ManualUploadActivity,
                status: BridgeStatus::Manual,
                formats: vec![
                    "gpx".to_owned(),
                    "kml".to_owned(),
                    "kmz".to_owned(),
                    "csv".to_owned(),
                ],
                workflow:
                    "User uploads a generated route to the AllTrails activities list on the website."
                        .to_owned(),
                source_url:
                    "https://support.alltrails.com/hc/en-us/articles/37228498475028-Uploading-files-to-AllTrails"
                        .to_owned(),
            },
            AllTrailsCapability {
                exchange: AllTrailsExchange::DirectWriteApi,
                status: BridgeStatus::Undocumented,
                formats: Vec::new(),
                workflow: "No documented public route-write API was found; private endpoints are intentionally unsupported."
                    .to_owned(),
                source_url:
                    "https://support.alltrails.com/hc/en-us/sections/360006411352-Importing-and-exporting-files"
                        .to_owned(),
            },
        ]
    }
}
