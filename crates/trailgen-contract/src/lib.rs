//! Tester-independent vocabulary shared across Trailgen's GUI boundary.
//!
//! This crate owns stable semantic names and wire encodings. It contains no
//! product behavior, fixture authority, or testing-framework dependency.

use std::{borrow::Cow, fmt};

use serde::{Deserialize, Serialize};

pub const UI_FINGERPRINT: &str = "trailgen.ui/3";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Workspace {
    Projects,
    Survey,
    Trail,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum View {
    Projects,
    Browse,
    FocusCandidate,
    FocusSaved,
    Edit,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EditorOrigin {
    New,
    Candidate,
    Saved,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RouteShape {
    Loop,
    OutAndBack,
    FigureEight,
    Open,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SearchPhase {
    Idle,
    Running,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CorpusPhase {
    Idle,
    Updating,
}

/// Stable recipient of a native user gesture.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Target {
    ProjectName,
    ProjectParent,
    ProjectCreate,
    SurveyAddArea,
    SurveyMap,
    Map,
    Manual,
    AddMapArea,
    RefreshTrails,
    Boundary,
    Find,
    Stop,
    DistanceMax,
    FocusBack,
    FocusEdit,
    FocusSave,
    FocusRename,
    RenameField,
    EditorSave,
    CloseLoop,
    Reverse,
    Profile,
    Support(usize),
}

impl Target {
    pub const STATIC: [Self; 22] = [
        Self::ProjectName,
        Self::ProjectParent,
        Self::ProjectCreate,
        Self::SurveyAddArea,
        Self::SurveyMap,
        Self::Map,
        Self::Manual,
        Self::AddMapArea,
        Self::RefreshTrails,
        Self::Boundary,
        Self::Find,
        Self::Stop,
        Self::DistanceMax,
        Self::FocusBack,
        Self::FocusEdit,
        Self::FocusSave,
        Self::FocusRename,
        Self::RenameField,
        Self::EditorSave,
        Self::CloseLoop,
        Self::Reverse,
        Self::Profile,
    ];

    #[must_use]
    pub fn wire(self) -> Cow<'static, str> {
        let static_name = match self {
            Self::ProjectName => "projects.new.name",
            Self::ProjectParent => "projects.new.parent",
            Self::ProjectCreate => "projects.new.create",
            Self::SurveyAddArea => "survey.add-area",
            Self::SurveyMap => "survey.map",
            Self::Map => "map.canvas",
            Self::Manual => "search.manual",
            Self::AddMapArea => "areas.add",
            Self::RefreshTrails => "areas.refresh",
            Self::Boundary => "search.boundary",
            Self::Find => "search.find",
            Self::Stop => "search.stop",
            Self::DistanceMax => "search.distance.max",
            Self::FocusBack => "focus.back",
            Self::FocusEdit => "focus.edit",
            Self::FocusSave => "focus.save",
            Self::FocusRename => "focus.rename",
            Self::RenameField => "focus.rename.field",
            Self::EditorSave => "editor.save",
            Self::CloseLoop => "editor.close-loop",
            Self::Reverse => "editor.reverse",
            Self::Profile => "profile.canvas",
            Self::Support(slot) => return Cow::Owned(format!("editor.support/{slot}")),
        };
        Cow::Borrowed(static_name)
    }
}

impl fmt::Display for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.wire())
    }
}

/// Stable namespace whose concrete members are product data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetClass {
    LibraryTrail,
    Candidate,
}

impl TargetClass {
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::LibraryTrail => "library.trail/",
            Self::Candidate => "results.candidate/",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn static_target_wire_identity_is_injective() {
        assert_eq!(
            Target::STATIC.len(),
            Target::STATIC
                .into_iter()
                .map(Target::wire)
                .collect::<BTreeSet<_>>()
                .len()
        );
    }

    #[test]
    fn support_targets_are_indexed_without_raw_string_construction() {
        assert_eq!(Target::Support(17).wire(), "editor.support/17");
    }
}
