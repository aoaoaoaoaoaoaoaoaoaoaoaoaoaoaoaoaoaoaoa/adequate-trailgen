//! Dependency-free product vocabulary shared across the GUI boundary.
//!
//! This crate names identity only. It contains no product behavior, witness
//! state, fixtures, or tester dependency.

use std::fmt;

pub const APPLICATION_ID: &str = "trailgen";
pub const UI_SCHEMA: u32 = 1;
pub const UI_FINGERPRINT: &str = "trailgen.ui/1";

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
}

impl Target {
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::ProjectName => "projects.new.name",
            Self::ProjectParent => "projects.new.parent",
            Self::ProjectCreate => "projects.new.create",
            Self::SurveyAddArea => "survey.add-area",
            Self::SurveyMap => "survey.map",
            Self::Map => "map.canvas",
            Self::Manual => "search.manual",
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
        }
    }
}

impl AsRef<str> for Target {
    fn as_ref(&self) -> &str {
        self.wire()
    }
}

impl fmt::Display for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.wire())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn target_wire_identity_is_injective() {
        let targets = [
            Target::ProjectName,
            Target::ProjectParent,
            Target::ProjectCreate,
            Target::SurveyAddArea,
            Target::SurveyMap,
            Target::Map,
            Target::Manual,
            Target::Boundary,
            Target::Find,
            Target::Stop,
            Target::DistanceMax,
            Target::FocusBack,
            Target::FocusEdit,
            Target::FocusSave,
            Target::FocusRename,
            Target::RenameField,
            Target::EditorSave,
            Target::CloseLoop,
            Target::Reverse,
            Target::Profile,
        ];
        assert_eq!(
            targets.len(),
            targets
                .into_iter()
                .map(Target::wire)
                .collect::<BTreeSet<_>>()
                .len()
        );
    }
}
