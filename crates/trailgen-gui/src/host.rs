use crate::{projects::Workbench, trail_map::TrailMapGpu, vector_map::VectorMapGpu};
use anyhow::Result;
use trailgen_shell::{NativeApp, WindowSpec};

pub fn run(ctx: egui::Context, app: Workbench) -> Result<()> {
    trailgen_shell::run(ctx, app)
}

impl NativeApp for Workbench {
    const WINDOW: WindowSpec = WindowSpec::new("trailgen · trail workbench", [1_440.0, 920.0]);

    fn draw(&mut self, ui: &mut egui::Ui) {
        self.pulse(ui);
    }

    fn after_present(&mut self) -> bool {
        self.settle()
    }

    fn water(
        &mut self,
        ctx: &egui::Context,
        pixels_per_point: f32,
        tooltip_rects: &[egui::Rect],
    ) -> dwemer_poolrooms::water::Frame {
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
