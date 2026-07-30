use egui::{Rect, Ui};

#[inline]
pub fn anchor(ui: &Ui, name: impl Into<String>, rect: Rect) {
    #[cfg(feature = "egui-test")]
    egui_tester_witness::egui::record(ui, name, rect);
    #[cfg(not(feature = "egui-test"))]
    {
        let _ = (ui, rect);
        drop(name);
    }
}

#[cfg(feature = "egui-test")]
#[inline]
pub fn rect(ctx: &egui::Context, name: impl Into<String>, rect: Rect) {
    egui_tester_witness::egui::record_rect(ctx, name, rect);
}

#[cfg(feature = "egui-test")]
pub use active::*;

#[cfg(feature = "egui-test")]
mod active {
    use egui::{Context, Rect};
    use egui_tester_witness::{Anchor, FrameObservation, PendingFrame};
    use serde::Serialize;

    #[derive(Serialize)]
    pub struct State {
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

    pub fn install(ctx: &Context) {
        egui_tester_witness::egui::install(ctx);
        ctx.on_begin_pass(
            "clear poolrooms witness anchors",
            std::sync::Arc::new(|ui| {
                drop(dwemer_poolrooms::instrumentation::take(ui.ctx()));
            }),
        );
    }

    pub fn stage(
        ctx: &Context,
        observed: FrameObservation,
        frame: u64,
        pixels_per_point: f32,
        state: State,
    ) -> egui_tester_witness::Result<PendingFrame<State>> {
        let anchors = egui_tester_witness::egui::take(ctx, pixels_per_point)?;
        let poolrooms = dwemer_poolrooms::instrumentation::take(ctx)
            .into_iter()
            .map(|anchor| {
                Anchor::logical(
                    anchor.name,
                    [
                        anchor.rect.min.x,
                        anchor.rect.min.y,
                        anchor.rect.max.x,
                        anchor.rect.max.y,
                    ],
                    pixels_per_point,
                )
            })
            .collect::<egui_tester_witness::Result<Vec<_>>>()?;
        PendingFrame::forge_at(
            observed,
            frame,
            pixels_per_point,
            anchors.into_iter().chain(poolrooms),
            state,
        )
    }
}
