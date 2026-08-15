use crate::{
    ProjectIntent, habitat::Habitat, projects::Workbench, trail_map::TrailMapGpu,
    vector_map::VectorMapGpu,
};
use anyhow::Result;
use eternalist_apps::{CrashProduct, CrashReportSpec, NativeApp, WindowSpec};
use std::time::Instant;

pub fn run(ctx: egui::Context, intent: ProjectIntent, offline: bool) -> Result<()> {
    eternalist_apps::run_with(ctx, |ctx| {
        let bootstrap =
            tracing::info_span!(target: "eternalist::startup", "application.bootstrap").entered();
        let habitat = Habitat::discover()?;
        let app = Workbench::launch(ctx, habitat, intent, offline);
        drop(bootstrap);
        Ok(app)
    })
}

impl NativeApp for Workbench {
    const WINDOW: WindowSpec = WindowSpec::new("trailgen · trail workbench", [1_440.0, 920.0]);

    fn crash_reports() -> Option<CrashReportSpec> {
        Habitat::discover().ok().map(|habitat| {
            CrashReportSpec::new(
                CrashProduct::Trailgen,
                env!("CARGO_PKG_VERSION"),
                habitat.state_dir(),
            )
        })
    }

    fn window_title(&self) -> String {
        Self::window_title(self)
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        self.pulse(ui);
    }

    fn service_deadline(&self, now: Instant) -> Option<Instant> {
        self.next_service_deadline(now)
    }

    fn service_deadline_reached(&mut self, now: Instant) -> bool {
        self.service_reached(now)
    }

    fn after_present(&mut self) -> bool {
        self.settle()
    }

    fn water(
        &mut self,
        ctx: &egui::Context,
        pixels_per_point: f32,
        tooltip_rects: &[egui::Rect],
    ) -> brass_poolrooms::water::Frame {
        self.water_frame(ctx, pixels_per_point, tooltip_rects)
    }

    fn register_gpu(
        renderer: &mut egui_wgpu::Renderer,
        device: &egui_wgpu::wgpu::Device,
        format: egui_wgpu::wgpu::TextureFormat,
    ) {
        let _prior = renderer
            .callback_resources
            .insert(VectorMapGpu::new(device, format));
        let _prior = renderer
            .callback_resources
            .insert(TrailMapGpu::new(device, format));
    }

    #[cfg(feature = "egui-test")]
    type Observation = crate::witness::State;

    #[cfg(feature = "egui-test")]
    fn observe(&self, text_edit_focused: bool) -> Self::Observation {
        self.witness_state(text_edit_focused)
    }
}
