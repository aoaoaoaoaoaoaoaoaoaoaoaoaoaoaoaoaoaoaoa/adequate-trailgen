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
    use egui_tester_witness::{Anchor, PendingFrame, ProductInstant};
    use serde::Serialize;

    #[derive(Serialize)]
    pub struct State {
        pub workspace: &'static str,
        pub view: &'static str,
        pub focused_trail: Option<String>,
        pub rename_active: bool,
        pub text_edit_focused: bool,
        pub map: Option<MapState>,
        pub editor: Option<EditorState>,
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
                map: None,
                editor: None,
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
        pub ready: bool,
        pub dragging_support: Option<usize>,
        pub support_points: Vec<[f64; 2]>,
        pub route_distance_m: Option<f64>,
        pub route_signature: Option<u64>,
    }

    pub fn reset(ctx: &Context) {
        egui_tester_witness::egui::reset(ctx);
    }

    pub fn stage(
        ctx: &Context,
        observed: ProductInstant,
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
