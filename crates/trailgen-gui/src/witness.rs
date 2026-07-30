use std::fmt::Display;

use egui::{Rect, Ui};

#[inline]
pub fn anchor(ui: &Ui, name: impl Display, rect: Rect) {
    #[cfg(feature = "egui-test")]
    egui_tester_witness::egui::record(ui, name.to_string(), rect);
    #[cfg(not(feature = "egui-test"))]
    {
        let _ = (ui, rect);
        drop(name);
    }
}

#[inline]
pub fn rect(ctx: &egui::Context, name: impl Display, rect: Rect) {
    #[cfg(feature = "egui-test")]
    egui_tester_witness::egui::record_rect(ctx, name.to_string(), rect);
    #[cfg(not(feature = "egui-test"))]
    {
        let _ = (ctx, rect);
        drop(name);
    }
}

#[cfg(feature = "egui-test")]
pub use active::*;

#[cfg(feature = "egui-test")]
mod active {
    use egui::Rect;
    use serde::Serialize;
    use trailgen_contract::{
        CorpusPhase, EditorOrigin, RouteShape, SearchPhase, TrailColoring, View, Workspace,
    };

    #[derive(Serialize)]
    pub struct State {
        pub contract: &'static str,
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

    impl State {
        pub const fn empty(workspace: Workspace, view: View, text_edit_focused: bool) -> Self {
            Self {
                contract: trailgen_contract::UI_FINGERPRINT,
                workspace,
                view,
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
        pub coloring: TrailColoring,
    }

    impl MapState {
        pub const fn forge(
            rect: Rect,
            center: [f64; 2],
            world_points: f64,
            coloring: TrailColoring,
        ) -> Self {
            Self {
                rect: [rect.min.x, rect.min.y, rect.max.x, rect.max.y],
                center,
                world_points,
                coloring,
            }
        }
    }

    #[derive(Serialize)]
    pub struct EditorState {
        pub origin: EditorOrigin,
        pub shape: RouteShape,
        pub ready: bool,
        pub dragging_support: Option<usize>,
        pub support_points: Vec<[f64; 2]>,
        pub route_signature: Option<u64>,
        pub redo_depth: usize,
    }

    #[derive(Serialize)]
    pub struct SearchState {
        pub phase: SearchPhase,
        pub corpus: CorpusPhase,
        pub trailhead: bool,
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
    }

    #[derive(Serialize)]
    pub struct ProfileState {
        pub visible: bool,
        pub locked: bool,
        pub marker: bool,
    }
}
