//! Provisional native host extracted under Trailgen's acceptance contract.
//!
//! This crate owns only the winit, egui, wgpu, Poolrooms-water, and optional
//! post-present witness lifecycle. Product chrome and domain behavior remain
//! in the application. Its name and repository are deliberately provisional
//! until a second application proves the same boundary.

pub mod living_wait;
pub mod responsiveness;

use anyhow::{Context as _, Result, bail};
use dwemer_poolrooms::water::{Engine, Frame as WaterFrame};
use egui_wgpu::{
    RenderState, Renderer, RendererOptions, ScreenDescriptor, WgpuConfiguration, wgpu,
};
#[cfg(feature = "egui-test")]
use serde::Serialize;
use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::{StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Window, WindowAttributes},
};

pub use living_wait::LivingWait;
pub use responsiveness::TraceGuard;

macro_rules! main_phase {
    ($name:literal, $body:expr) => {{
        let _phase = tracing::info_span!(target: "eternalist::main", $name).entered();
        $body
    }};
}

/// Stable top-level window identity and initial geometry.
#[derive(Clone, Copy, Debug)]
pub struct WindowSpec {
    pub title: &'static str,
    pub initial_size: [f64; 2],
}

/// Main-thread wall-time obligations owned by the native product.
#[derive(Clone, Copy, Debug)]
pub struct ResponsivenessSpec {
    pub frame: Duration,
}

impl ResponsivenessSpec {
    #[must_use]
    pub const fn interactive() -> Self {
        Self {
            frame: Duration::from_millis(40),
        }
    }
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
    const RESPONSIVENESS: ResponsivenessSpec = ResponsivenessSpec::interactive();

    /// Current top-level window identity.
    fn window_title(&self) -> String {
        Self::WINDOW.title.to_owned()
    }

    /// Build one ordinary product UI frame.
    fn draw(&mut self, ui: &mut egui::Ui);

    /// Commit work deliberately deferred until a successful surface present.
    fn after_present(&mut self) -> bool;

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
    type Observation: Serialize + Send + 'static;

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
    let witness: Option<egui_tester_witness::Publisher<A::Observation>> =
        egui_tester_witness::Publisher::from_env().context("arm egui-tester witness")?;
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
        window_title: A::WINDOW.title.to_owned(),
        fault: None,
        trace_deadline: responsiveness::deadline()?,
        #[cfg(feature = "egui-test")]
        witness,
    };
    event_loop.run_app(&mut shell).context("run event loop")?;
    #[cfg(feature = "egui-test")]
    if let Some(witness) = &shell.witness {
        witness.flush().context("flush egui-tester witness")?;
    }
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

struct Shell<A: NativeApp> {
    ctx: egui::Context,
    app: A,
    alarm: Alarm,
    rig: Option<Rig>,
    force_redraw: bool,
    window_title: String,
    fault: Option<anyhow::Error>,
    trace_deadline: Option<Instant>,
    #[cfg(feature = "egui-test")]
    witness: Option<egui_tester_witness::Publisher<A::Observation>>,
}

impl<A: NativeApp> Shell<A> {
    fn paint(&mut self) -> Result<()> {
        let Some(rig) = self.rig.as_mut() else {
            return Ok(());
        };
        let begun = Instant::now();
        let frame = self.ctx.cumulative_frame_nr();
        let frame_span = tracing::info_span!(
            target: "eternalist::main",
            "frame",
            frame,
            primitives = tracing::field::Empty,
            pixels_per_point = tracing::field::Empty,
            presented = tracing::field::Empty,
        );
        let _frame = frame_span.enter();
        #[cfg(feature = "egui-test")]
        let pulse = self
            .witness
            .as_ref()
            .map(|_| egui_tester_witness::FramePulse::begin());
        let raw_input = main_phase!("frame.input", rig.input.take_egui_input(&rig.window));
        let output = main_phase!(
            "frame.ui",
            self.ctx.run_ui(raw_input, |ui| self.app.draw(ui))
        );
        frame_span.record("pixels_per_point", output.pixels_per_point);
        let title = self.app.window_title();
        if title != self.window_title {
            rig.window.set_title(&title);
            self.window_title = title;
        }
        main_phase!(
            "frame.platform_output",
            rig.input
                .handle_platform_output(&rig.window, output.platform_output)
        );
        let primitives = main_phase!(
            "frame.tessellate",
            self.ctx.tessellate(output.shapes, output.pixels_per_point)
        );
        frame_span.record("primitives", primitives.len());
        let tooltip_rects = main_phase!("frame.tooltip_geometry", tooltip_rects(&self.ctx));
        let water = main_phase!(
            "frame.water",
            self.app
                .water(&self.ctx, output.pixels_per_point, &tooltip_rects)
        );
        if water.wants_repaint() {
            rig.window.request_redraw();
        }
        #[cfg(feature = "egui-test")]
        let pending = pulse
            .map(|pulse| {
                stage_witness(
                    &self.ctx,
                    pulse,
                    self.ctx.cumulative_frame_nr(),
                    output.pixels_per_point,
                    self.app.observe(self.ctx.text_edit_focused()),
                )
            })
            .transpose()
            .context("stage egui-tester witness")?;
        let presented = main_phase!(
            "frame.render",
            rig.render(
                &primitives,
                &output.textures_delta,
                output.pixels_per_point,
                &water,
            )?
        );
        frame_span.record("presented", presented);
        #[cfg(not(feature = "egui-test"))]
        let _ = presented;
        #[cfg(feature = "egui-test")]
        if presented && let (Some(publisher), Some(pending)) = (&mut self.witness, pending) {
            let surface_presented = egui_tester_witness::ProductInstant::now();
            let _surface_sequence = publisher
                .surface_present_at(pending, surface_presented)
                .context("publish egui-tester witness")?;
        }
        if presented {
            self.force_redraw |= main_phase!("frame.after_present", self.app.after_present());
        }
        if let Some(viewport) = output.viewport_output.get(&egui::ViewportId::ROOT) {
            if viewport.repaint_delay.is_zero() {
                rig.window.request_redraw();
            } else if let Some(when) = Instant::now().checked_add(viewport.repaint_delay) {
                advance_alarm(&self.alarm, when);
            }
        }
        warn_frame_overrun(begun.elapsed(), A::RESPONSIVENESS.frame);
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

fn warn_frame_overrun(elapsed: Duration, budget: Duration) {
    if elapsed <= budget {
        return;
    }
    let micros = |duration: Duration| u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
    tracing::warn!(
        target: "eternalist::latency",
        operation = "frame",
        elapsed_us = micros(elapsed),
        budget_us = micros(budget),
        "main-thread budget exceeded"
    );
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
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let event_name = match &event {
            WindowEvent::RedrawRequested => "redraw",
            WindowEvent::CursorMoved { .. } => "cursor_moved",
            WindowEvent::MouseWheel { .. } => "mouse_wheel",
            WindowEvent::MouseInput { .. } => "mouse_input",
            WindowEvent::KeyboardInput { .. } => "keyboard_input",
            WindowEvent::Resized(_) => "resized",
            WindowEvent::CloseRequested => "close_requested",
            _ => "other",
        };
        let _event = tracing::info_span!(
            target: "eternalist::main",
            "window.event",
            kind = event_name
        )
        .entered();
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
        if self
            .trace_deadline
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            event_loop.exit();
            return;
        }
        if std::mem::take(&mut self.force_redraw) {
            if let Some(rig) = &self.rig {
                rig.window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        self.tend_alarm();
        let deadline = *lock_alarm(&self.alarm);
        let deadline = deadline.into_iter().chain(self.trace_deadline).min();
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
        let mut encoder = main_phase!(
            "render.encoder",
            self.gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("native-app-shell"),
                })
        );
        let user_commands = main_phase!("render.prepare", {
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
        });
        let frame = main_phase!(
            "render.acquire_surface",
            match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(frame)
                | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
                wgpu::CurrentSurfaceTexture::Timeout => {
                    self.free_textures(delta);
                    self.window.request_redraw();
                    return Ok(false);
                }
                wgpu::CurrentSurfaceTexture::Occluded => {
                    self.free_textures(delta);
                    return Ok(false);
                }
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    self.free_textures(delta);
                    self.surface.configure(&self.gpu.device, &self.config);
                    self.window.request_redraw();
                    return Ok(false);
                }
                wgpu::CurrentSurfaceTexture::Validation => {
                    self.free_textures(delta);
                    bail!("surface texture validation failure");
                }
            }
        );
        let surface_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        if water.dry() {
            self.water.becalm(&self.gpu.queue);
        }
        let frosted = water.live() && self.water.scene_view().is_some();
        main_phase!("render.egui_pass", {
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
        });
        if frosted {
            main_phase!(
                "render.water_compose",
                self.water.compose(
                    &self.gpu.device,
                    &self.gpu.queue,
                    &mut encoder,
                    &surface_view,
                    water,
                )
            );
        }
        let _submission = main_phase!(
            "render.submit",
            self.gpu
                .queue
                .submit(user_commands.into_iter().chain([encoder.finish()]))
        );
        if main_phase!(
            "render.water_after_submit",
            self.water
                .after_submit(&self.gpu.device, &self.gpu.queue, water)
        ) {
            self.window.request_redraw();
        }
        main_phase!("render.free_textures", self.free_textures(delta));
        self.window.pre_present_notify();
        main_phase!("render.present", frame.present());
        Ok(true)
    }

    fn free_textures(&self, delta: &egui::TexturesDelta) {
        let mut renderer = self.gpu.renderer.write();
        for id in &delta.free {
            renderer.free_texture(id);
        }
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
    pulse: egui_tester_witness::FramePulse,
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
    let observed = pulse.observe();
    egui_tester_witness::PendingFrame::forge_at(
        observed,
        frame,
        pixels_per_point,
        anchors.into_iter().chain(poolrooms),
        state,
    )
}
