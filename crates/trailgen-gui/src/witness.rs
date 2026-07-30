use egui::{Rect, Ui};

#[inline]
pub fn anchor(ui: &Ui, name: impl AsRef<str>, rect: Rect) {
    #[cfg(feature = "egui-test")]
    egui_tester_witness::egui::record(ui, name.as_ref().to_owned(), rect);
    #[cfg(not(feature = "egui-test"))]
    {
        let _ = (ui, rect);
        drop(name);
    }
}

#[cfg(feature = "egui-test")]
#[inline]
pub fn rect(ctx: &egui::Context, name: impl AsRef<str>, rect: Rect) {
    egui_tester_witness::egui::record_rect(ctx, name.as_ref().to_owned(), rect);
}

#[cfg(feature = "egui-test")]
pub use active::*;

#[cfg(feature = "egui-test")]
mod active {
    use egui::Rect;
    use serde::Serialize;

    #[derive(Serialize)]
    pub struct State {
        pub contract: &'static str,
        pub workspace: &'static str,
        pub view: &'static str,
        pub focused_trail: Option<String>,
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

    impl State {
        pub const fn empty(
            workspace: &'static str,
            view: &'static str,
            text_edit_focused: bool,
        ) -> Self {
            Self {
                contract: trailgen_contract::UI_FINGERPRINT,
                workspace,
                view,
                focused_trail: None,
                rename_active: false,
                text_edit_focused,
                saved_trails: 0,
                candidates: 0,
                map: None,
                editor: None,
                search: None,
                survey: None,
                profile: None,
            }
        }
    }

    #[derive(Serialize)]
    pub struct MapState {
        pub rect: [f32; 4],
        pub center: [f64; 2],
        pub world_points: f64,
    }

    impl MapState {
        pub const fn forge(rect: Rect, center: [f64; 2], world_points: f64) -> Self {
            Self {
                rect: [rect.min.x, rect.min.y, rect.max.x, rect.max.y],
                center,
                world_points,
            }
        }
    }

    #[derive(Serialize)]
    pub struct EditorState {
        pub origin: &'static str,
        pub shape: &'static str,
        pub ready: bool,
        pub dragging_support: Option<usize>,
        pub support_points: Vec<[f64; 2]>,
        pub route_distance_m: Option<f64>,
        pub route_signature: Option<u64>,
        pub undo_depth: usize,
        pub redo_depth: usize,
        pub fault: Option<String>,
    }

    #[derive(Serialize)]
    pub struct SearchState {
        pub phase: &'static str,
        pub serial: Option<u64>,
        pub stage: Option<&'static str>,
        pub explored: usize,
        pub limit: usize,
        pub discovered: usize,
        pub stopping: bool,
        pub trailhead: Option<[f64; 2]>,
        pub boundary: bool,
        pub required: usize,
        pub forbidden: usize,
        pub revision_scheduled: bool,
    }

    #[derive(Serialize)]
    pub struct SurveyState {
        pub regions: usize,
        pub acquiring: bool,
        pub drawing: bool,
        pub status: String,
        pub fault: Option<String>,
    }

    #[derive(Serialize)]
    pub struct ProfileState {
        pub visible: bool,
        pub locked_distance_m: Option<f64>,
        pub marker: Option<[f64; 2]>,
    }
}
