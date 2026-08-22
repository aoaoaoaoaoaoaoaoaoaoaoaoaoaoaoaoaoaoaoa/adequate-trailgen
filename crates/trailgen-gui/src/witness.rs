use std::fmt::Display;

use egui::{Rect, Ui};

#[inline]
pub fn response(ui: &Ui, name: impl Display, response: &egui::Response) {
    #[cfg(feature = "egui-test")]
    egui_tester_witness::egui::record_response(ui, name.to_string(), response);
    #[cfg(not(feature = "egui-test"))]
    {
        let _ = (ui, response);
        drop(name);
    }
}

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
        BoundaryPhase, CorpusPhase, EditorOrigin, RouteShape, SearchPhase, TrailColoring, View,
        Workspace,
    };

    #[derive(Serialize)]
    pub struct State {
        pub contract: &'static str,
        pub workspace: Workspace,
        pub view: View,
        pub rename_active: bool,
        pub guide_open: bool,
        pub text_edit_focused: bool,
        pub saved_trails: usize,
        pub visible_saved: usize,
        pub last_exported: Option<String>,
        pub candidates: usize,
        pub base_pace_kmh: Option<f64>,
        pub settings: Settings,
        pub map: Option<MapState>,
        pub areas: Option<AreaState>,
        pub civic: Option<CivicState>,
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
                guide_open: false,
                text_edit_focused,
                saved_trails: 0,
                visible_saved: 0,
                last_exported: None,
                candidates: 0,
                base_pace_kmh: None,
                settings: Settings {
                    open: false,
                    fault: false,
                    settled: false,
                },
                map: None,
                areas: None,
                civic: None,
                editor: None,
                search: None,
                survey: None,
                profile: None,
            }
        }
    }

    #[derive(Default, Serialize)]
    pub struct Settings {
        pub open: bool,
        pub fault: bool,
        pub settled: bool,
    }

    #[derive(Serialize)]
    pub struct MapState {
        pub rect: [f32; 4],
        pub center: [f64; 2],
        pub world_points: f64,
        pub coloring: TrailColoring,
        pub basemap_tiles: usize,
        pub probe: Option<[f64; 2]>,
    }

    impl MapState {
        pub const fn forge(
            rect: Rect,
            center: [f64; 2],
            world_points: f64,
            coloring: TrailColoring,
            basemap_tiles: usize,
            probe: Option<[f64; 2]>,
        ) -> Self {
            Self {
                rect: [rect.min.x, rect.min.y, rect.max.x, rect.max.y],
                center,
                world_points,
                coloring,
                basemap_tiles,
                probe,
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
        pub coordinate_callouts: Vec<usize>,
        pub fault_support: Option<usize>,
        pub route_signature: Option<u64>,
        pub redo_depth: usize,
    }

    #[derive(Serialize)]
    pub struct SearchState {
        pub phase: SearchPhase,
        pub corpus: CorpusPhase,
        pub results: trailgen_contract::ResultsPhase,
        pub trailhead: bool,
        pub boundary: BoundaryPhase,
        pub required: usize,
        pub forbidden: usize,
        pub revision_scheduled: bool,
    }

    #[derive(Serialize)]
    pub struct SurveyState {
        pub acquiring: bool,
    }

    #[derive(Serialize)]
    pub struct AreaState {
        pub regions: usize,
        pub drawing: bool,
        pub resizing: Option<AreaResizeState>,
    }

    #[derive(Serialize)]
    pub struct CivicState {
        pub active: usize,
        pub ready: usize,
        pub preparing: usize,
        pub suggestions: usize,
    }

    #[derive(Serialize)]
    pub struct AreaResizeState {
        pub slot: usize,
        pub corner: trailgen_contract::AreaCorner,
    }

    #[derive(Serialize)]
    pub struct ProfileState {
        pub visible: bool,
        pub locked: bool,
        pub marker: bool,
    }
}
