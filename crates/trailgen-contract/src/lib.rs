//! Tester-independent vocabulary shared across Trailgen's GUI boundary.
//!
//! This crate owns stable semantic names and wire encodings. It contains no
//! product behavior, fixture authority, or testing-framework dependency.

use std::{borrow::Cow, fmt};

use serde::{Deserialize, Serialize};

pub const UI_FINGERPRINT: &str = "trailgen.ui/11";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Workspace {
    Projects,
    Survey,
    Preparing,
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
#[serde(rename_all = "snake_case")]
pub enum ResultsPhase {
    Dormant,
    Open,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CorpusPhase {
    Idle,
    Updating,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AreaCorner {
    Northwest,
    Northeast,
    Southeast,
    Southwest,
}

impl AreaCorner {
    pub const ALL: [Self; 4] = [
        Self::Northwest,
        Self::Northeast,
        Self::Southeast,
        Self::Southwest,
    ];

    #[must_use]
    pub const fn ordinal(self) -> usize {
        match self {
            Self::Northwest => 0,
            Self::Northeast => 1,
            Self::Southeast => 2,
            Self::Southwest => 3,
        }
    }
}

/// The trail property projected onto tube hue.
///
/// Surface and wayfinding remain a separate, invariant visual channel.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrailColoring {
    #[default]
    Class,
    Formality,
    Terrain,
}

impl TrailColoring {
    pub const ALL: [Self; 3] = [Self::Class, Self::Formality, Self::Terrain];
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
    TrailDataWait,
    SearchWait,
    LegendClass,
    LegendFormality,
    LegendTerrain,
    Finder,
    Manual,
    AddMapArea,
    RefreshTrails,
    CivicSearch,
    Boundary,
    Find,
    Stop,
    DistanceMax,
    MovingTimeMin,
    MovingTimeMax,
    LowerLimbLoad,
    BasePace,
    GlossCard,
    FocusBack,
    FocusEdit,
    FocusSave,
    FocusRename,
    RenameField,
    EditorRename,
    EditorRenameField,
    EditorSave,
    CloseLoop,
    Reverse,
    Profile,
    ShortcutHelp,
    ShortcutHelpCard,
    Support(usize),
    AreaRename(usize),
    AreaRenameField(usize),
    AreaHandle { slot: usize, corner: AreaCorner },
    CivicSuggestion(usize),
    CivicArea(usize),
    CivicRemove(usize),
    CivicRetry(usize),
}

impl Target {
    pub const STATIC: [Self; 38] = [
        Self::ProjectName,
        Self::ProjectParent,
        Self::ProjectCreate,
        Self::SurveyAddArea,
        Self::SurveyMap,
        Self::Map,
        Self::TrailDataWait,
        Self::SearchWait,
        Self::LegendClass,
        Self::LegendFormality,
        Self::LegendTerrain,
        Self::Finder,
        Self::Manual,
        Self::AddMapArea,
        Self::RefreshTrails,
        Self::CivicSearch,
        Self::Boundary,
        Self::Find,
        Self::Stop,
        Self::DistanceMax,
        Self::MovingTimeMin,
        Self::MovingTimeMax,
        Self::LowerLimbLoad,
        Self::BasePace,
        Self::GlossCard,
        Self::FocusBack,
        Self::FocusEdit,
        Self::FocusSave,
        Self::FocusRename,
        Self::RenameField,
        Self::EditorRename,
        Self::EditorRenameField,
        Self::EditorSave,
        Self::CloseLoop,
        Self::Reverse,
        Self::Profile,
        Self::ShortcutHelp,
        Self::ShortcutHelpCard,
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
            Self::TrailDataWait => "status.trail-data.waiting",
            Self::SearchWait => "results.waiting",
            Self::LegendClass => "map.legend/class",
            Self::LegendFormality => "map.legend/formality",
            Self::LegendTerrain => "map.legend/terrain",
            Self::Finder => "creator.finder",
            Self::Manual => "creator.manual",
            Self::AddMapArea => "areas.add",
            Self::RefreshTrails => "areas.refresh",
            Self::CivicSearch => "overlays.search",
            Self::Boundary => "search.boundary",
            Self::Find => "search.find",
            Self::Stop => "search.stop",
            Self::DistanceMax => "search.distance.max",
            Self::MovingTimeMin => "search.moving-time.min",
            Self::MovingTimeMax => "search.moving-time.max",
            Self::LowerLimbLoad => "search.lower-limb-load",
            Self::BasePace => "calibration.base-pace",
            Self::GlossCard => "gloss.card",
            Self::FocusBack => "focus.back",
            Self::FocusEdit => "focus.edit",
            Self::FocusSave => "focus.save",
            Self::FocusRename => "focus.rename",
            Self::RenameField => "focus.rename.field",
            Self::EditorRename => "editor.rename",
            Self::EditorRenameField => "editor.rename.field",
            Self::EditorSave => "editor.save",
            Self::CloseLoop => "editor.close-loop",
            Self::Reverse => "editor.reverse",
            Self::Profile => "profile.canvas",
            Self::ShortcutHelp => "shortcut-help",
            Self::ShortcutHelpCard => "shortcut-help.card",
            Self::Support(slot) => return Cow::Owned(format!("editor.support/{slot}")),
            Self::AreaRename(slot) => return Cow::Owned(format!("areas.rename/{slot}")),
            Self::AreaRenameField(slot) => {
                return Cow::Owned(format!("areas.rename/{slot}/field"));
            }
            Self::AreaHandle { slot, corner } => {
                return Cow::Owned(format!("areas.handle/{slot}/{}", corner.ordinal()));
            }
            Self::CivicSuggestion(slot) => {
                return Cow::Owned(format!("overlays.suggestion/{slot}"));
            }
            Self::CivicArea(slot) => return Cow::Owned(format!("overlays.area/{slot}")),
            Self::CivicRemove(slot) => {
                return Cow::Owned(format!("overlays.area/{slot}/remove"));
            }
            Self::CivicRetry(slot) => {
                return Cow::Owned(format!("overlays.area/{slot}/retry"));
            }
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

    #[test]
    fn area_rename_targets_bind_control_and_field_to_one_slot() {
        assert_eq!(Target::AreaRename(3).wire(), "areas.rename/3");
        assert_eq!(Target::AreaRenameField(3).wire(), "areas.rename/3/field");
    }

    #[test]
    fn area_handle_targets_carry_a_closed_corner_vocabulary() {
        assert_eq!(
            Target::AreaHandle {
                slot: 3,
                corner: AreaCorner::Southwest,
            }
            .wire(),
            "areas.handle/3/3"
        );
        assert_eq!(AreaCorner::ALL.map(AreaCorner::ordinal), [0_usize, 1, 2, 3]);
    }
}
