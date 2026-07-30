use egui_tester::{Condition, field};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Workspace {
    Projects,
    Limbo,
    Survey,
    Trail,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum View {
    Projects,
    Transition,
    Browse,
    FocusCandidate,
    FocusSaved,
    Edit,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum EditorOrigin {
    New,
    Candidate,
    Saved,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum RouteShape {
    Loop,
    OutAndBack,
    FigureEight,
    Open,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum SearchPhase {
    Idle,
    Striking,
}

#[derive(Debug, Deserialize)]
pub struct Observation {
    pub contract: String,
    pub workspace: Workspace,
    pub view: View,
    pub rename_active: bool,
    pub text_edit_focused: bool,
    pub saved_trails: usize,
    pub candidates: usize,
    pub map: Option<MapState>,
    pub editor: Option<EditorState>,
    pub search: Option<SearchState>,
    pub survey: Option<SurveyState>,
    pub profile: Option<ProfileState>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct MapState {
    pub rect: [f32; 4],
    pub center: [f64; 2],
    pub world_points: f64,
}

#[derive(Debug, Deserialize)]
pub struct EditorState {
    pub origin: EditorOrigin,
    pub shape: RouteShape,
    pub ready: bool,
    pub dragging_support: Option<usize>,
    pub support_points: Vec<[f64; 2]>,
    pub route_signature: Option<u64>,
    pub redo_depth: usize,
}

#[derive(Debug, Deserialize)]
pub struct SearchState {
    pub phase: SearchPhase,
    pub trailhead: Option<[f64; 2]>,
    pub boundary: bool,
    pub required: usize,
    pub forbidden: usize,
    pub revision_scheduled: bool,
}

#[derive(Debug, Deserialize)]
pub struct SurveyState {
    pub regions: usize,
    pub acquiring: bool,
    pub drawing: bool,
}

#[derive(Debug, Deserialize)]
pub struct ProfileState {
    pub visible: bool,
    pub locked_distance_m: Option<f64>,
    pub marker: Option<[f64; 2]>,
}

pub mod shows {
    use super::{
        Condition, EditorOrigin, Observation, RouteShape, SearchPhase, View, Workspace, field,
    };

    pub fn condition(
        description: impl Into<String>,
        predicate: impl Fn(&Observation) -> bool + 'static,
    ) -> Condition<Observation> {
        Condition::new(description, predicate)
    }

    pub fn workspace(expected: Workspace) -> Condition<Observation> {
        field("workspace", |state: &Observation| state.workspace).eq(expected)
    }

    pub fn view(expected: View) -> Condition<Observation> {
        field("view", |state: &Observation| state.view).eq(expected)
    }

    pub fn text_focused() -> Condition<Observation> {
        field("text edit focus", |state: &Observation| {
            state.text_edit_focused
        })
        .eq(true)
    }

    pub fn library(expected: usize) -> Condition<Observation> {
        field("saved Library size", |state: &Observation| {
            state.saved_trails
        })
        .eq(expected)
    }

    pub fn candidates(expected: usize) -> Condition<Observation> {
        field("candidate count", |state: &Observation| state.candidates).eq(expected)
    }

    pub fn candidates_at_least(minimum: usize) -> Condition<Observation> {
        field("candidate count", |state: &Observation| state.candidates)
            .satisfies(format!(">= {minimum}"), move |count| *count >= minimum)
    }

    pub fn map() -> Condition<Observation> {
        condition("a visible map", |state| state.map.is_some())
    }

    pub fn map_scale_at_least(minimum: f64) -> Condition<Observation> {
        condition(format!("map scale >= {minimum}"), move |state| {
            state
                .map
                .as_ref()
                .is_some_and(|map| map.world_points >= minimum)
        })
    }

    pub fn survey_drawing() -> Condition<Observation> {
        condition("map-area selection to be armed", |state| {
            state.survey.as_ref().is_some_and(|survey| survey.drawing)
        })
    }

    pub fn survey_acquiring(regions: usize) -> Condition<Observation> {
        condition(
            format!("acquisition of {regions} selected region(s)"),
            move |state| {
                state
                    .survey
                    .as_ref()
                    .is_some_and(|survey| survey.acquiring && survey.regions == regions)
            },
        )
    }

    pub fn search(phase: SearchPhase) -> Condition<Observation> {
        condition(format!("search phase {phase:?}"), move |state| {
            state
                .search
                .as_ref()
                .is_some_and(|search| search.phase == phase)
        })
    }

    pub fn trailhead() -> Condition<Observation> {
        condition("a placed trailhead", |state| {
            state
                .search
                .as_ref()
                .is_some_and(|search| search.trailhead.is_some())
        })
    }

    pub fn boundary() -> Condition<Observation> {
        condition("a committed search boundary", |state| {
            state.search.as_ref().is_some_and(|search| search.boundary)
        })
    }

    pub fn revision() -> Condition<Observation> {
        condition("a scheduled or active search revision", |state| {
            state.search.as_ref().is_some_and(|search| {
                search.revision_scheduled || search.phase == SearchPhase::Striking
            })
        })
    }

    pub fn required(expected: usize) -> Condition<Observation> {
        condition(format!("{expected} required segment(s)"), move |state| {
            state
                .search
                .as_ref()
                .is_some_and(|search| search.required == expected)
        })
    }

    pub fn forbidden(expected: usize) -> Condition<Observation> {
        condition(format!("{expected} forbidden segment(s)"), move |state| {
            state
                .search
                .as_ref()
                .is_some_and(|search| search.forbidden == expected)
        })
    }

    pub fn rename(active: bool) -> Condition<Observation> {
        field("rename transaction", |state: &Observation| {
            state.rename_active
        })
        .eq(active)
    }

    pub fn editor_origin(expected: EditorOrigin) -> Condition<Observation> {
        condition(format!("editor origin {expected:?}"), move |state| {
            state
                .editor
                .as_ref()
                .is_some_and(|editor| editor.origin == expected)
        })
    }

    pub fn editor_ready() -> Condition<Observation> {
        condition("a ready editor", |state| {
            state.editor.as_ref().is_some_and(|editor| editor.ready)
        })
    }

    pub fn dragging_support(expected: Option<usize>) -> Condition<Observation> {
        condition(format!("dragging support {expected:?}"), move |state| {
            state
                .editor
                .as_ref()
                .is_some_and(|editor| editor.dragging_support == expected)
        })
    }

    pub fn supports(expected: usize) -> Condition<Observation> {
        condition(format!("{expected} support point(s)"), move |state| {
            state
                .editor
                .as_ref()
                .is_some_and(|editor| editor.support_points.len() == expected)
        })
    }

    pub fn supports_at_least(minimum: usize) -> Condition<Observation> {
        condition(
            format!("at least {minimum} support point(s)"),
            move |state| {
                state
                    .editor
                    .as_ref()
                    .is_some_and(|editor| editor.support_points.len() >= minimum)
            },
        )
    }

    pub fn support(slot: usize, expected: [f64; 2]) -> Condition<Observation> {
        condition(format!("support {slot} near {expected:?}"), move |state| {
            state.editor.as_ref().is_some_and(|editor| {
                editor
                    .support_points
                    .get(slot)
                    .is_some_and(|point| near(*point, expected))
            })
        })
    }

    pub fn signature(expected: u64) -> Condition<Observation> {
        condition(format!("route signature {expected}"), move |state| {
            state
                .editor
                .as_ref()
                .is_some_and(|editor| editor.route_signature == Some(expected))
        })
    }

    pub fn changed_signature(rejected: u64) -> Condition<Observation> {
        condition(
            format!("route signature other than {rejected}"),
            move |state| {
                state
                    .editor
                    .as_ref()
                    .is_some_and(|editor| editor.route_signature != Some(rejected))
            },
        )
    }

    pub fn redoable() -> Condition<Observation> {
        condition("a redoable editor mutation", |state| {
            state
                .editor
                .as_ref()
                .is_some_and(|editor| editor.redo_depth > 0)
        })
    }

    pub fn shape(expected: RouteShape) -> Condition<Observation> {
        condition(format!("route shape {expected:?}"), move |state| {
            state
                .editor
                .as_ref()
                .is_some_and(|editor| editor.shape == expected)
        })
    }

    pub fn profile_visible() -> Condition<Observation> {
        condition("a visible elevation profile", |state| {
            state
                .profile
                .as_ref()
                .is_some_and(|profile| profile.visible)
        })
    }

    pub fn profile_hovering() -> Condition<Observation> {
        condition("an unlocked profile marker", |state| {
            state.profile.as_ref().is_some_and(|profile| {
                profile.marker.is_some() && profile.locked_distance_m.is_none()
            })
        })
    }

    pub fn profile_locked(locked: bool) -> Condition<Observation> {
        condition(
            if locked {
                "a locked elevation cursor"
            } else {
                "an unlocked elevation cursor"
            },
            move |state| {
                state
                    .profile
                    .as_ref()
                    .is_some_and(|profile| profile.locked_distance_m.is_some() == locked)
            },
        )
    }

    fn near(left: [f64; 2], right: [f64; 2]) -> bool {
        (left[0] - right[0]).abs() <= 5.0e-5 && (left[1] - right[1]).abs() <= 5.0e-5
    }
}
