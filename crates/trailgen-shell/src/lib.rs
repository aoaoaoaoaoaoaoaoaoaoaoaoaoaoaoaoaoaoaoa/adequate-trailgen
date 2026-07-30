//! Provisional native host extracted under Trailgen's acceptance contract.
//!
//! This crate owns only the winit, egui, wgpu, Poolrooms-water, and optional
//! post-present witness lifecycle. Product chrome and domain behavior remain
//! in the application. Its name and repository are deliberately provisional
//! until a second application proves the same boundary.

use anyhow::{Context as _, Result, bail};
use dwemer_poolrooms::water::{Engine, Frame as WaterFrame};
use egui_wgpu::{
    RenderState, Renderer, RendererOptions, ScreenDescriptor, WgpuConfiguration, wgpu,
};
use egui_winit::winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::{StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Window, WindowAttributes},
};
#[cfg(feature = "egui-test")]
use serde::Serialize;
use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

/// Stable top-level window identity and initial geometry.
#[derive(Clone, Copy, Debug)]
pub struct WindowSpec {
    pub title: &'static str,
    pub initial_size: [f64; 2],
}

impl WindowSpec {
    #[must_use]
    pub const fn new(title: &'static str, initial_size: [f64; 2]) -> Self {
        Self {
            title,
            initial_size,
        }
    }
}

/// The narrow product seam admitted by the native host.
pub trait NativeApp {
    const WINDOW: WindowSpec;

    /// Build one ordinary product UI frame.
    fn draw(&mut self, ui: &mut egui::Ui);

    /// Commit work deliberately deferred until the frame boundary.
    fn commit_frame(&mut self) -> bool;

    /// Describe Poolrooms water composition for the frame.
    fn water(
        &mut self,
        ctx: &egui::Context,
        pixels_per_point: f32,
        tooltip_rects: &[egui::Rect],
    ) -> WaterFrame;

    /// Install application-owned wgpu callback resources.
    fn register_gpu(renderer: &mut Renderer, device: &wgpu::Device, format: wgpu::TextureFormat);

    #[cfg(feature = "egui-test")]
    type Observation: Serialize;

    /// Project the smallest useful one-way acceptance observation.
    #[cfg(feature = "egui-test")]
    fn observe(&self, text_edit_focused: bool) -> Self::Observation;
}

#[derive(Clone, Copy, Debug)]
struct Spark;

type Alarm = Arc<Mutex<Option<Instant>>>;

/// Run one native application until its sole top-level window closes.
pub fn run<A: NativeApp>(ctx: egui::Context, app: A) -> Result<()> {
    let event_loop = EventLoop::<Spark>::with_user_event()
        .build()
        .context("build event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let alarm = Alarm::default();
    arm_repaints(&ctx, Arc::clone(&alarm), event_loop.create_proxy());
    #[cfg(feature = "egui-test")]
    let witness = egui_tester_witness::Publisher::from_env().context("arm egui-tester witness")?;
    #[cfg(feature = "egui-test")]
    if witness.is_some() {
        install_witness(&ctx);
    }
    let mut shell = Shell {
        ctx,
        app,
        alarm,
        rig: None,
        force_redraw: false,
        fault: None,
        #[cfg(feature = "egui-test")]
        witness,
    };
    event_loop.run_app(&mut shell).context("run event loop")?;
    shell.fault.map_or(Ok(()), Err)
}

fn arm_repaints(ctx: &egui::Context, alarm: Alarm, proxy: EventLoopProxy<Spark>) {
    ctx.set_request_repaint_callback(move |info| {
        advance_alarm(&alarm, Instant::now() + info.delay);
        let _woken = proxy.send_event(Spark);
    });
}

fn advance_alarm(alarm: &Alarm, when: Instant) {
    let mut alarm = lock_alarm(alarm);
    if alarm.is_none_or(|set| when < set) {
        *alarm = Some(when);
    }
}

fn lock_alarm(alarm: &Alarm) -> MutexGuard<'_, Option<Instant>> {
    match alarm.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

struct Shell<A> {
    ctx: egui::Context,
    app: A,
    alarm: Alarm,
    rig: Option<Rig>,
    force_redraw: bool,
    fault: Option<anyhow::Error>,
    #[cfg(feature = "egui-test")]
    witness: Option<egui_tester_witness::Publisher>,
}

impl<A: NativeApp> Shell<A> {
    fn paint(&mut self) -> Result<()> {
        let Some(rig) = self.rig.as_mut() else {
            return Ok(());
        };
        #[cfg(feature = "egui-test")]
        let pulse = self
            .witness
            .as_ref()
            .map(|_| egui_tester_witness::FramePulse::begin());
        let raw_input = rig.input.take_egui_input(&rig.window);
        let output = self.ctx.run_ui(raw_input, |ui| self.app.draw(ui));
        rig.input
            .handle_platform_output(&rig.window, output.platform_output);
        #[cfg(feature = "egui-test")]
        let observed = pulse.map(egui_tester_witness::FramePulse::observe);
        let primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        let tooltip_rects = tooltip_rects(&self.ctx);
        let water = self
            .app
            .water(&self.ctx, output.pixels_per_point, &tooltip_rects);
        if water.wants_repaint() {
            rig.window.request_redraw();
        }
        let presented = rig.render(
            &primitives,
            &output.textures_delta,
            output.pixels_per_point,
            &water,
        )?;
        #[cfg(not(feature = "egui-test"))]
        let _ = presented;
        #[cfg(feature = "egui-test")]
        if presented && let (Some(publisher), Some(observed)) = (&mut self.witness, observed) {
            let presentation = egui_tester_witness::ProductInstant::now();
            let pending = stage_witness(
                &self.ctx,
                observed,
                self.ctx.cumulative_frame_nr(),
                output.pixels_per_point,
                self.app.observe(self.ctx.text_edit_focused()),
            )
            .context("stage egui-tester witness")?;
            let _presentation = publisher
                .present_at(pending, presentation)
                .context("publish egui-tester witness")?;
        }
        if self.app.commit_frame() {
            self.force_redraw = true;
        }
        if let Some(viewport) = output.viewport_output.get(&egui::ViewportId::ROOT) {
            if viewport.repaint_delay.is_zero() {
                rig.window.request_redraw();
            } else if let Some(when) = Instant::now().checked_add(viewport.repaint_delay) {
                advance_alarm(&self.alarm, when);
            }
        }
        Ok(())
    }

    fn tend_alarm(&self) {
        let Some(rig) = &self.rig else {
            return;
        };
        let fire = {
            let mut alarm = lock_alarm(&self.alarm);
            let fire = alarm.is_some_and(|when| when <= Instant::now());
            if fire {
                *alarm = None;
            }
            fire
        };
        if fire {
            rig.window.request_redraw();
        }
    }

    fn abort(&mut self, event_loop: &ActiveEventLoop, error: anyhow::Error) {
        if self.fault.is_none() {
            self.fault = Some(error);
        }
        event_loop.exit();
    }
}

fn tooltip_rects(ctx: &egui::Context) -> Vec<egui::Rect> {
    ctx.memory(|memory| {
        memory
            .layer_ids()
            .filter(|layer| layer.order == egui::Order::Tooltip && memory.areas().is_visible(layer))
            .filter_map(|layer| memory.area_rect(layer.id))
            .filter(egui::Rect::is_positive)
            .collect()
    })
}

impl<A: NativeApp> ApplicationHandler<Spark> for Shell<A> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.rig.is_some() {
            return;
        }
        match Rig::raise::<A>(event_loop, &self.ctx) {
            Ok(rig) => self.rig = Some(rig),
            Err(error) => self.abort(event_loop, error.context("raise native window")),
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            self.tend_alarm();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: Spark) {
        self.tend_alarm();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: egui_winit::winit::window::WindowId,
        event: WindowEvent,
    ) {
        match &event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
                return;
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.paint() {
                    self.abort(event_loop, error);
                }
                return;
            }
            WindowEvent::Resized(size) => {
                if let Some(rig) = &mut self.rig {
                    rig.resize(*size);
                }
            }
            _ => {}
        }
        let Some(rig) = &mut self.rig else {
            return;
        };
        let response = rig.input.on_window_event(&rig.window, &event);
        if response.repaint {
            rig.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if std::mem::take(&mut self.force_redraw) {
            if let Some(rig) = &self.rig {
                rig.window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        self.tend_alarm();
        let deadline = *lock_alarm(&self.alarm);
        event_loop.set_control_flow(deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }
}

struct Rig {
    window: Arc<Window>,
    input: egui_winit::State,
    surface: wgpu::Surface<'static>,
    gpu: RenderState,
    config: wgpu::SurfaceConfiguration,
    water: Engine,
}

impl Rig {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "winit reports DPI as f64 while egui's scale contract is f32"
    )]
    fn raise<A: NativeApp>(event_loop: &ActiveEventLoop, ctx: &egui::Context) -> Result<Self> {
        let [width, height] = A::WINDOW.initial_size;
        let window = Arc::new(
            event_loop
                .create_window(
                    WindowAttributes::default()
                        .with_title(A::WINDOW.title)
                        .with_inner_size(LogicalSize::new(width, height)),
                )
                .context("create window")?,
        );
        let input = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        let configuration = WgpuConfiguration::default();
        let instance = pollster::block_on(configuration.wgpu_setup.new_instance());
        let surface = instance
            .create_surface(Arc::clone(&window))
            .context("create surface")?;
        let gpu = pollster::block_on(RenderState::create(
            &configuration,
            &instance,
            Some(&surface),
            RendererOptions::default(),
        ))
        .context("create wgpu render state")?;
        A::register_gpu(&mut gpu.renderer.write(), &gpu.device, gpu.target_format);
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
            .context("surface is unsupported by the adapter")?;
        config.format = gpu.target_format;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.view_formats = vec![gpu.target_format];
        surface.configure(&gpu.device, &config);
        let mut water = Engine::new(&gpu.device, gpu.target_format);
        water.resize(&gpu.device, config.width, config.height);
        Ok(Self {
            window,
            input,
            surface,
            gpu,
            config,
            water,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.gpu.device, &self.config);
        self.water.resize(&self.gpu.device, size.width, size.height);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the canonical Poolrooms render graph is one ordered GPU transaction"
    )]
    fn render(
        &mut self,
        primitives: &[egui::ClippedPrimitive],
        delta: &egui::TexturesDelta,
        pixels_per_point: f32,
        water: &WaterFrame,
    ) -> Result<bool> {
        let screen = ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point,
        };
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("native-app-shell"),
            });
        let user_commands = {
            let mut renderer = self.gpu.renderer.write();
            for (id, image_delta) in &delta.set {
                renderer.update_texture(&self.gpu.device, &self.gpu.queue, *id, image_delta);
            }
            renderer.update_buffers(
                &self.gpu.device,
                &self.gpu.queue,
                &mut encoder,
                primitives,
                &screen,
            )
        };
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout => {
                self.window.request_redraw();
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Occluded => return Ok(false),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.gpu.device, &self.config);
                self.window.request_redraw();
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                bail!("surface texture validation failure");
            }
        };
        let surface_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        if water.dry() {
            self.water.becalm(&self.gpu.queue);
        }
        let frosted = water.live() && self.water.scene_view().is_some();
        {
            let target = if frosted {
                self.water.scene_view().unwrap_or(&surface_view)
            } else {
                &surface_view
            };
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("native-app-egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.gpu
                .renderer
                .read()
                .render(&mut pass, primitives, &screen);
        }
        if frosted {
            self.water.compose(
                &self.gpu.device,
                &self.gpu.queue,
                &mut encoder,
                &surface_view,
                water,
            );
        }
        let _submission = self
            .gpu
            .queue
            .submit(user_commands.into_iter().chain([encoder.finish()]));
        if self
            .water
            .after_submit(&self.gpu.device, &self.gpu.queue, water)
        {
            self.window.request_redraw();
        }
        self.window.pre_present_notify();
        frame.present();
        {
            let mut renderer = self.gpu.renderer.write();
            for id in &delta.free {
                renderer.free_texture(id);
            }
        }
        Ok(true)
    }
}

#[cfg(feature = "egui-test")]
fn install_witness(ctx: &egui::Context) {
    egui_tester_witness::egui::install(ctx);
    ctx.on_begin_pass(
        "clear poolrooms witness anchors",
        Arc::new(|ui| {
            drop(dwemer_poolrooms::instrumentation::take(ui.ctx()));
        }),
    );
}

#[cfg(feature = "egui-test")]
fn stage_witness<T: Serialize>(
    ctx: &egui::Context,
    observed: egui_tester_witness::FrameObservation,
    frame: u64,
    pixels_per_point: f32,
    state: T,
) -> egui_tester_witness::Result<egui_tester_witness::PendingFrame<T>> {
    use egui_tester_witness::Anchor;

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
    egui_tester_witness::PendingFrame::forge_at(
        observed,
        frame,
        pixels_per_point,
        anchors.into_iter().chain(poolrooms),
        state,
    )
}
