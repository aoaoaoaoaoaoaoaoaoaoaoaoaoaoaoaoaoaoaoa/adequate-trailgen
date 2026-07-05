use crate::geo::LineString;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RouteFileMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_type: Option<String>,
}

impl RouteFileMetadata {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.recorded_at.is_none()
            && self.activity_type.is_none()
    }

    #[must_use]
    pub fn title_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.title.as_deref().unwrap_or(fallback)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouteFile {
    pub line: LineString,
    #[serde(default, skip_serializing_if = "RouteFileMetadata::is_empty")]
    pub metadata: RouteFileMetadata,
}

impl RouteFile {
    #[must_use]
    pub const fn new(line: LineString, metadata: RouteFileMetadata) -> Self {
        Self { line, metadata }
    }
}

#[must_use]
pub fn clean_text(raw: &str) -> Option<String> {
    let s = raw.trim();
    (!s.is_empty()).then(|| s.to_owned())
}
