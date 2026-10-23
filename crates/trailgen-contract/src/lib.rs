//! Tester-independent vocabulary shared across Trailgen's GUI boundary.
//!
//! This crate owns stable semantic names and wire encodings. It contains no
//! product behavior or fixture authority, and borrows only the witness wire
//! grammar, never the tester.

use std::{borrow::Cow, fmt};

use egui_tester_witness::encode_identity;
use serde::{Deserialize, Serialize};

/// Reverse-DNS product identifier; the sole source of Trailgen's platform and crash identity.
pub const PRODUCT_IDENTIFIER: &str = "dev.adequate.trailgen";
/// Product display name.
pub const PRODUCT_NAME: &str = "Trailgen";
pub const UI_FINGERPRINT: &str = "trailgen.ui/24";

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
#[serde(rename_all = "kebab-case")]
pub enum BoundaryPhase {
    Unlimited,
    Drawing,
    Committed,
    Redrawing,
}

impl BoundaryPhase {
    #[must_use]
    pub const fn committed(self) -> bool {
        matches!(self, Self::Committed | Self::Redrawing)
    }

    #[must_use]
    pub const fn drawing(self) -> bool {
        matches!(self, Self::Drawing | Self::Redrawing)
    }
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
    MapProbe,
    TrailheadPin,
    TrailheadPlacement,
    TrailDataWait,
    SearchWait,
    LegendClass,
    LegendFormality,
    LegendTerrain,
    NewTrail,
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
    FocusEdit,
    FocusSave,
    FocusDelete,
    FocusDeleteConfirm,
    FocusDeleteCancel,
    FocusRename,
    RenameField,
    EditorRename,
    EditorRenameField,
    EditorDiscard,
    EditorSave,
    EditorUndo,
    EditorRedo,
    CloseLoop,
    Reverse,
    Profile,
    Panel(&'static str),
    SavedVisibility(usize),
    SavedExport(usize),
    Support(usize),
    SupportCallout(usize),
    SupportFault(usize),
    AreaRename(usize),
    AreaRenameField(usize),
    AreaHandle { slot: usize, corner: AreaCorner },
    CivicSuggestion(usize),
    CivicArea(usize),
    CivicRemove(usize),
    CivicRetry(usize),
}

impl Target {
    pub const STATIC: [Self; 43] = [
        Self::ProjectName,
        Self::ProjectParent,
        Self::ProjectCreate,
        Self::SurveyAddArea,
        Self::SurveyMap,
        Self::Map,
        Self::MapProbe,
        Self::TrailheadPin,
        Self::TrailheadPlacement,
        Self::TrailDataWait,
        Self::SearchWait,
        Self::LegendClass,
        Self::LegendFormality,
        Self::LegendTerrain,
        Self::NewTrail,
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
        Self::FocusEdit,
        Self::FocusSave,
        Self::FocusDelete,
        Self::FocusDeleteConfirm,
        Self::FocusDeleteCancel,
        Self::FocusRename,
        Self::RenameField,
        Self::EditorRename,
        Self::EditorRenameField,
        Self::EditorDiscard,
        Self::EditorSave,
        Self::EditorUndo,
        Self::EditorRedo,
        Self::CloseLoop,
        Self::Reverse,
        Self::Profile,
    ];

    #[must_use]
    pub fn wire(self) -> Cow<'static, str> {
        let static_name = match self {
            Self::ProjectName => "trailgen.projects.new.name",
            Self::ProjectParent => "trailgen.projects.new.parent",
            Self::ProjectCreate => "trailgen.projects.new.create",
            Self::SurveyAddArea => "trailgen.survey.add-area",
            Self::SurveyMap => "trailgen.survey.map",
            Self::Map => "trailgen.map.canvas",
            Self::MapProbe => "trailgen.map.coordinate-probe",
            Self::TrailheadPin => "trailgen.map.trailhead-pin",
            Self::TrailheadPlacement => "trailgen.search.trailhead",
            Self::TrailDataWait => "trailgen.status.trail-data.waiting",
            Self::SearchWait => "trailgen.results.waiting",
            Self::LegendClass => "trailgen.map.legend.class",
            Self::LegendFormality => "trailgen.map.legend.formality",
            Self::LegendTerrain => "trailgen.map.legend.terrain",
            Self::NewTrail => "trailgen.details.new-trail",
            Self::AddMapArea => "trailgen.areas.add",
            Self::RefreshTrails => "trailgen.areas.refresh",
            Self::CivicSearch => "trailgen.overlays.search",
            Self::Boundary => "trailgen.search.boundary",
            Self::Find => "trailgen.search.find",
            Self::Stop => "trailgen.search.stop",
            Self::DistanceMax => "trailgen.search.distance.max",
            Self::MovingTimeMin => "trailgen.search.moving-time.min",
            Self::MovingTimeMax => "trailgen.search.moving-time.max",
            Self::LowerLimbLoad => "trailgen.search.lower-limb-load",
            Self::BasePace => "trailgen.calibration.base-pace",
            Self::GlossCard => "trailgen.gloss.card",
            Self::FocusEdit => "trailgen.focus.edit",
            Self::FocusSave => "trailgen.focus.save",
            Self::FocusDelete => "trailgen.focus.delete",
            Self::FocusDeleteConfirm => "trailgen.focus.delete.confirm",
            Self::FocusDeleteCancel => "trailgen.focus.delete.cancel",
            Self::FocusRename => "trailgen.focus.rename",
            Self::RenameField => "trailgen.focus.rename.field",
            Self::EditorRename => "trailgen.editor.rename",
            Self::EditorRenameField => "trailgen.editor.rename.field",
            Self::EditorDiscard => "trailgen.editor.discard",
            Self::EditorSave => "trailgen.editor.save",
            Self::EditorUndo => "trailgen.editor.undo",
            Self::EditorRedo => "trailgen.editor.redo",
            Self::CloseLoop => "trailgen.editor.close-loop",
            Self::Reverse => "trailgen.editor.reverse",
            Self::Profile => "trailgen.profile.canvas",
            Self::Panel(name) => {
                return Cow::Owned(format!("trailgen.inspector.panel/{name}"));
            }
            Self::SavedVisibility(slot) => {
                return Cow::Owned(format!("trailgen.library.entry.visibility/{slot}"));
            }
            Self::SavedExport(slot) => {
                return Cow::Owned(format!("trailgen.library.entry.export/{slot}"));
            }
            Self::Support(slot) => {
                return Cow::Owned(format!("trailgen.editor.support/{slot}"));
            }
            Self::SupportCallout(slot) => {
                return Cow::Owned(format!("trailgen.editor.support.coordinates/{slot}"));
            }
            Self::SupportFault(slot) => {
                return Cow::Owned(format!("trailgen.editor.support.fault/{slot}"));
            }
            Self::AreaRename(slot) => {
                return Cow::Owned(format!("trailgen.areas.rename/{slot}"));
            }
            Self::AreaRenameField(slot) => {
                return Cow::Owned(format!("trailgen.areas.rename.field/{slot}"));
            }
            Self::AreaHandle { slot, corner } => {
                return Cow::Owned(format!("trailgen.areas.handle/{slot}/{}", corner.ordinal()));
            }
            Self::CivicSuggestion(slot) => {
                return Cow::Owned(format!("trailgen.overlays.suggestion/{slot}"));
            }
            Self::CivicArea(slot) => {
                return Cow::Owned(format!("trailgen.overlays.area/{slot}"));
            }
            Self::CivicRemove(slot) => {
                return Cow::Owned(format!("trailgen.overlays.area.remove/{slot}"));
            }
            Self::CivicRetry(slot) => {
                return Cow::Owned(format!("trailgen.overlays.area.retry/{slot}"));
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
    /// The semantic part every member Target of this class shares.
    #[must_use]
    pub const fn semantic(self) -> &'static str {
        match self {
            Self::LibraryTrail => "trailgen.library.trail",
            Self::Candidate => "trailgen.results.candidate",
        }
    }

    /// The member Target naming one raw runtime `identity` in this class; an
    /// empty identity has no spelling and so names none.
    #[must_use]
    pub fn member(self, identity: &str) -> Option<impl fmt::Display + '_> {
        let identity = encode_identity(identity)?;
        let semantic = self.semantic();
        Some(fmt::from_fn(move |formatter| {
            write!(formatter, "{semantic}/{identity}")
        }))
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
}
