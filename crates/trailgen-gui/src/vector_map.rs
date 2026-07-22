use crate::basemap::{FillPoint, StrokePoint, TileKey, VectorTile};
use bytemuck::{Pod, Zeroable};
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor, wgpu};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use wgpu::util::DeviceExt as _;

const GPU_CEILING: usize = 384 * 1_048_576;
const MAX_WRAP_RADIUS: u32 = 2;
const MAX_WRAP_INSTANCES: usize = (MAX_WRAP_RADIUS * 2 + 1) as usize;
const MAX_GAPS: usize = 32;
static NEXT_CORPUS: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VectorCorpus(u64);

impl VectorCorpus {
    #[must_use]
    pub fn mint() -> Self {
        Self(NEXT_CORPUS.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Clone)]
pub struct VectorPaint {
    pub layer: VectorLayer,
    pub corpus: VectorCorpus,
    pub geometry: GeometryPass,
    pub gaps: Arc<[VectorGap]>,
    pub tiles: Arc<[Arc<VectorTile>]>,
    pub center_world: [f64; 2],
    pub world_points: f32,
    pub viewport_points: [f32; 2],
    pub view_zoom: f32,
    pub apparition_span: f32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VectorLayer {
    Basemap,
    Relief,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeometryPass {
    Fills,
    Strokes,
    Both,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct VectorGap {
    center: [f32; 2],
    axis: [f32; 2],
    half_extent: [f32; 2],
    _pad: [f32; 2],
}

impl VectorGap {
    #[must_use]
    pub fn screen(center: egui::Pos2, axis: egui::Vec2, half_extent: egui::Vec2) -> Self {
        Self {
            center: center.to_vec2().into(),
            axis: axis.into(),
            half_extent: half_extent.into(),
            _pad: [0.0; 2],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct GpuKey {
    layer: VectorLayer,
    corpus: VectorCorpus,
    tile: TileKey,
}

struct ActiveCorpus {
    corpus: VectorCorpus,
    tiles: Vec<TileKey>,
}

impl CallbackTrait for VectorPaint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(gpu) = resources.get_mut::<VectorMapGpu>() {
            gpu.prepare(device, queue, self);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let Some(gpu) = resources.get::<VectorMapGpu>() else {
            return;
        };
        pass.set_bind_group(0, &gpu.bind, &[]);
        if self.geometry != GeometryPass::Strokes {
            pass.set_pipeline(&gpu.fill_pipeline);
            for tile in self.tiles.iter() {
                if let Some(tile) = gpu.tiles.get(&GpuKey {
                    layer: self.layer,
                    corpus: self.corpus,
                    tile: tile.key,
                }) && let Some(draw) = &tile.fills
                {
                    draw.paint(pass, &tile.buffer, &tile.transform, gpu.instances);
                }
            }
        }
        if self.geometry != GeometryPass::Fills {
            pass.set_pipeline(if self.layer == VectorLayer::Relief {
                &gpu.relief_stroke_pipeline
            } else {
                &gpu.stroke_pipeline
            });
            for tile in self.tiles.iter() {
                if let Some(tile) = gpu.tiles.get(&GpuKey {
                    layer: self.layer,
                    corpus: self.corpus,
                    tile: tile.key,
                }) && let Some(draw) = &tile.strokes
                {
                    draw.paint(pass, &tile.buffer, &tile.transform, gpu.instances);
                }
            }
        }
    }
}

pub struct VectorMapGpu {
    fill_pipeline: wgpu::RenderPipeline,
    stroke_pipeline: wgpu::RenderPipeline,
    relief_stroke_pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    tiles: HashMap<GpuKey, GpuTile>,
    active: HashMap<VectorLayer, ActiveCorpus>,
    active_set: HashSet<GpuKey>,
    order: VecDeque<(GpuKey, u64)>,
    epoch: u64,
    bytes: usize,
    instances: u32,
    profile: bool,
}

impl VectorMapGpu {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vector-map-uniform"),
            size: size_of::<Uniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vector-map"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(size_of::<Uniform>() as u64),
                },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vector-map"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vector-map"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vector-map"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });
        let fill_pipeline = pipeline(
            device,
            format,
            &pipeline_layout,
            &shader,
            PipelineLaw {
                label: "vector-fill",
                vertex_entry: "fill_vertex",
                fragment_entry: fragment_entry(format),
                vertex: fill_layout(),
            },
        );
        let stroke_pipeline = pipeline(
            device,
            format,
            &pipeline_layout,
            &shader,
            PipelineLaw {
                label: "vector-stroke",
                vertex_entry: "stroke_vertex",
                fragment_entry: fragment_entry(format),
                vertex: stroke_layout(),
            },
        );
        let relief_stroke_pipeline = pipeline(
            device,
            format,
            &pipeline_layout,
            &shader,
            PipelineLaw {
                label: "relief-stroke",
                vertex_entry: "stroke_vertex",
                fragment_entry: relief_fragment_entry(format),
                vertex: stroke_layout(),
            },
        );
        Self {
            fill_pipeline,
            stroke_pipeline,
            relief_stroke_pipeline,
            uniform,
            bind,
            tiles: HashMap::new(),
            active: HashMap::new(),
            active_set: HashSet::new(),
            order: VecDeque::new(),
            epoch: 0,
            bytes: 0,
            instances: 1,
            profile: std::env::var_os("TRAILGEN_PROFILE_BASEMAP").is_some(),
        }
    }

    fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, paint: &VectorPaint) {
        let begun = Instant::now();
        let incoming = paint.tiles.iter().map(|tile| tile.key).collect::<Vec<_>>();
        let changed = self
            .active
            .get(&paint.layer)
            .is_none_or(|active| active.corpus != paint.corpus || active.tiles != incoming);
        if changed {
            self.epoch = self.epoch.saturating_add(1);
            self.active.insert(
                paint.layer,
                ActiveCorpus {
                    corpus: paint.corpus,
                    tiles: incoming.clone(),
                },
            );
            self.active_set.clear();
            self.active_set
                .extend(self.active.iter().flat_map(|(layer, active)| {
                    active.tiles.iter().map(|tile| GpuKey {
                        layer: *layer,
                        corpus: active.corpus,
                        tile: *tile,
                    })
                }));
            for key in incoming.iter().map(|tile| GpuKey {
                layer: paint.layer,
                corpus: paint.corpus,
                tile: *tile,
            }) {
                if let Some(resident) = self.tiles.get_mut(&key) {
                    resident.touched = self.epoch;
                    self.order.push_back((key, self.epoch));
                }
            }
        }
        let mut uploaded = 0_usize;
        for tile in paint.tiles.iter() {
            let key = GpuKey {
                layer: paint.layer,
                corpus: paint.corpus,
                tile: tile.key,
            };
            if self.tiles.contains_key(&key) {
                continue;
            }
            let resident = GpuTile::raise(device, tile, self.epoch);
            uploaded = uploaded.saturating_add(resident.bytes);
            self.bytes = self.bytes.saturating_add(resident.bytes);
            self.order.push_back((key, self.epoch));
            let _prior = self.tiles.insert(key, resident);
        }
        let uniform = Uniform::forge(paint);
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&uniform));
        self.instances = uniform.wrap_radius.saturating_mul(2).saturating_add(1);
        self.reap();
        if self.profile {
            eprintln!(
                "vector-gpu prepare_us={} upload_bytes={uploaded} active_tiles={} changed={changed}",
                begun.elapsed().as_micros(),
                incoming.len()
            );
        }
    }

    fn reap(&mut self) {
        if self.bytes <= GPU_CEILING {
            return;
        }
        let candidates = self.order.len();
        for _ in 0..candidates {
            let Some((key, epoch)) = self.order.pop_front() else {
                break;
            };
            let Some(resident) = self.tiles.get(&key) else {
                continue;
            };
            if resident.touched != epoch {
                continue;
            }
            if self.active_set.contains(&key) {
                self.order.push_back((key, epoch));
                continue;
            }
            let Some(victim) = self.tiles.remove(&key) else {
                continue;
            };
            self.bytes = self.bytes.saturating_sub(victim.bytes);
            if self.bytes <= GPU_CEILING {
                break;
            }
        }
    }
}

struct PipelineLaw {
    label: &'static str,
    vertex_entry: &'static str,
    fragment_entry: &'static str,
    vertex: wgpu::VertexBufferLayout<'static>,
}

fn pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    law: PipelineLaw,
) -> wgpu::RenderPipeline {
    let buffers = [law.vertex, tile_layout()];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(law.label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(law.vertex_entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &buffers,
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(law.fragment_entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn fragment_entry(format: wgpu::TextureFormat) -> &'static str {
    if format.is_srgb() {
        "fragment_linear"
    } else {
        "fragment_gamma"
    }
}

fn relief_fragment_entry(format: wgpu::TextureFormat) -> &'static str {
    if format.is_srgb() {
        "fragment_linear_relief"
    } else {
        "fragment_gamma_relief"
    }
}

const fn fill_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Unorm8x4,
        7 => Float32
    ];
    wgpu::VertexBufferLayout {
        array_stride: size_of::<FillPoint>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

const fn stroke_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Unorm8x4,
        3 => Float32,
        7 => Float32
    ];
    wgpu::VertexBufferLayout {
        array_stride: size_of::<StrokePoint>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

const fn tile_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
        4 => Float32x2,
        5 => Float32x2,
        6 => Float32
    ];
    wgpu::VertexBufferLayout {
        array_stride: size_of::<TileInstance>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRIBUTES,
    }
}

struct GpuTile {
    fills: Option<Draw>,
    strokes: Option<Draw>,
    buffer: wgpu::Buffer,
    transform: Range<u64>,
    bytes: usize,
    touched: u64,
}

impl GpuTile {
    fn raise(device: &wgpu::Device, tile: &VectorTile, touched: u64) -> Self {
        let mut blade = Vec::with_capacity(
            tile.resident_bytes()
                .saturating_add(size_of::<TileInstance>() * MAX_WRAP_INSTANCES),
        );
        let fills = Draw::pack(&mut blade, &tile.fills.vertices, &tile.fills.indices);
        let strokes = Draw::pack(&mut blade, &tile.strokes.vertices, &tile.strokes.indices);
        let transform = append(
            &mut blade,
            &[TileInstance::forge(tile.key); MAX_WRAP_INSTANCES],
        );
        let bytes = blade.len();
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vector-tile"),
            contents: &blade,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::INDEX,
        });
        Self {
            fills,
            strokes,
            buffer,
            transform,
            bytes,
            touched,
        }
    }
}

struct Draw {
    vertices: Range<u64>,
    indices: Range<u64>,
    index_count: u32,
}

impl Draw {
    fn pack<V: Pod>(blade: &mut Vec<u8>, vertices: &[V], indices: &[u32]) -> Option<Self> {
        if vertices.is_empty() || indices.is_empty() {
            return None;
        }
        let index_count = u32::try_from(indices.len()).ok()?;
        let vertices = append(blade, vertices);
        let indices = append(blade, indices);
        Some(Self {
            vertices,
            indices,
            index_count,
        })
    }

    fn paint(
        &self,
        pass: &mut wgpu::RenderPass<'static>,
        buffer: &wgpu::Buffer,
        transform: &Range<u64>,
        instances: u32,
    ) {
        pass.set_vertex_buffer(0, buffer.slice(self.vertices.clone()));
        pass.set_vertex_buffer(1, buffer.slice(transform.clone()));
        pass.set_index_buffer(
            buffer.slice(self.indices.clone()),
            wgpu::IndexFormat::Uint32,
        );
        pass.draw_indexed(0..self.index_count, 0, 0..instances);
    }
}

fn append<T: Pod>(blade: &mut Vec<u8>, values: &[T]) -> Range<u64> {
    let start = blade.len() as u64;
    blade.extend_from_slice(bytemuck::cast_slice(values));
    start..blade.len() as u64
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniform {
    center_high: [f32; 2],
    center_low: [f32; 2],
    viewport: [f32; 2],
    world_points: f32,
    wrap_radius: u32,
    view_zoom: f32,
    apparition_span: f32,
    gap_count: u32,
    _pad: u32,
    gaps: [VectorGap; MAX_GAPS],
}

impl Uniform {
    fn forge(paint: &VectorPaint) -> Self {
        let [x_high, x_low] = split(paint.center_world[0]);
        let [y_high, y_low] = split(paint.center_world[1]);
        let wrap_radius = wrap_radius(
            paint.viewport_points[0] / paint.world_points,
            paint.center_world[0] as f32,
        );
        Self {
            center_high: [x_high, y_high],
            center_low: [x_low, y_low],
            viewport: paint.viewport_points,
            world_points: paint.world_points,
            wrap_radius,
            view_zoom: paint.view_zoom,
            apparition_span: paint.apparition_span,
            gap_count: paint.gaps.len().min(MAX_GAPS) as u32,
            _pad: 0,
            gaps: std::array::from_fn(|slot| {
                paint
                    .gaps
                    .get(slot)
                    .copied()
                    .unwrap_or_else(VectorGap::zeroed)
            }),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TileInstance {
    origin_high: [f32; 2],
    origin_low: [f32; 2],
    span: f32,
    _pad: [f32; 3],
}

impl TileInstance {
    fn forge(key: TileKey) -> Self {
        let divisions = f64::from(1_u32 << key.zoom);
        let [x_high, x_low] = split(f64::from(key.x) / divisions);
        let [y_high, y_low] = split(f64::from(key.y) / divisions);
        Self {
            origin_high: [x_high, y_high],
            origin_low: [x_low, y_low],
            span: (1.0 / divisions) as f32,
            _pad: [0.0; 3],
        }
    }
}

fn split(value: f64) -> [f32; 2] {
    let high = value as f32;
    [high, (value - f64::from(high)) as f32]
}

fn wrap_radius(world_width: f32, center_x: f32) -> u32 {
    let half = world_width * 0.5;
    let west = (half - center_x).max(0.0);
    let east = (center_x + half - 1.0).max(0.0);
    (west.max(east).ceil() as u32).min(MAX_WRAP_RADIUS)
}

const WGSL: &str = r"
struct Gap {
    center: vec2f,
    axis: vec2f,
    half_extent: vec2f,
    pad: vec2f,
};

struct Uniform {
    center_high: vec2f,
    center_low: vec2f,
    viewport: vec2f,
    world_points: f32,
    wrap_radius: u32,
    view_zoom: f32,
    apparition_span: f32,
    gap_count: u32,
    pad: u32,
    gaps: array<Gap, 32>,
};

@group(0) @binding(0) var<uniform> u: Uniform;

struct VertexOut {
    @builtin(position) position: vec4f,
    @location(0) color: vec4f,
    @location(1) edge_distance: f32,
    @location(2) solid_radius: f32,
    @location(3) tile_local: vec2f,
    @location(4) map_point: vec2f,
};

fn apparition(onset_zoom: f32) -> f32 {
    let phase = clamp(
        (u.view_zoom - onset_zoom) / max(u.apparition_span, 0.001),
        0.0,
        1.0,
    );
    return phase * phase * (3.0 - 2.0 * phase);
}

fn clip_at(
    local: vec2f,
    origin_high: vec2f,
    origin_low: vec2f,
    tile_span: f32,
    instance: u32,
) -> vec2f {
    let origin_delta = (origin_high - u.center_high)
        + (origin_low - u.center_low);
    var delta = origin_delta + local * tile_span;
    // A tile is an indivisible chart. Per-vertex wrapping tears coarse
    // triangles across the antimeridian into screen-spanning shards.
    delta.x -= round(origin_delta.x + tile_span * 0.5);
    delta.x += f32(instance) - f32(u.wrap_radius);
    let points = delta * u.world_points;
    return vec2f(points.x * 2.0 / u.viewport.x, -points.y * 2.0 / u.viewport.y);
}

@vertex
fn fill_vertex(
    @location(0) local: vec2f,
    @location(1) color: vec4f,
    @location(7) onset_zoom: f32,
    @location(4) origin_high: vec2f,
    @location(5) origin_low: vec2f,
    @location(6) tile_span: f32,
    @builtin(instance_index) instance: u32,
) -> VertexOut {
    var out: VertexOut;
    let clip = clip_at(local, origin_high, origin_low, tile_span, instance);
    out.position = vec4f(clip, 0.0, 1.0);
    let maturity = apparition(onset_zoom);
    out.color = vec4f(color.rgb, color.a * maturity);
    out.edge_distance = 0.0;
    out.solid_radius = -1.0;
    out.tile_local = local;
    out.map_point = (clip * vec2f(0.5, -0.5) + vec2f(0.5)) * u.viewport;
    return out;
}

@vertex
fn stroke_vertex(
    @location(0) local: vec2f,
    @location(1) extrusion: vec2f,
    @location(2) color: vec4f,
    @location(3) radius: f32,
    @location(7) onset_side: f32,
    @location(4) origin_high: vec2f,
    @location(5) origin_low: vec2f,
    @location(6) tile_span: f32,
    @builtin(instance_index) instance: u32,
) -> VertexOut {
    var out: VertexOut;
    let onset_zoom = abs(onset_side) - 1.0;
    let side = sign(onset_side);
    let maturity = apparition(onset_zoom);
    let visible_radius = radius * mix(0.12, 1.0, maturity);
    let expanded_radius = visible_radius + 0.8;
    let offset = extrusion * expanded_radius * 2.0 / u.viewport;
    let clip = clip_at(local, origin_high, origin_low, tile_span, instance)
        + vec2f(offset.x, -offset.y);
    out.position = vec4f(clip, 0.0, 1.0);
    out.color = vec4f(color.rgb, color.a * mix(0.16, 1.0, maturity));
    out.edge_distance = side * expanded_radius;
    out.solid_radius = visible_radius;
    out.tile_local = local
        + extrusion * expanded_radius / (u.world_points * tile_span);
    out.map_point = (clip * vec2f(0.5, -0.5) + vec2f(0.5)) * u.viewport;
    return out;
}

fn inside_gap(point: vec2f) -> bool {
    for (var slot = 0u; slot < min(u.gap_count, 32u); slot += 1u) {
        let gap = u.gaps[slot];
        let delta = point - gap.center;
        let normal = vec2f(-gap.axis.y, gap.axis.x);
        if abs(dot(delta, gap.axis)) <= gap.half_extent.x
            && abs(dot(delta, normal)) <= gap.half_extent.y {
            return true;
        }
    }
    return false;
}

fn painted(in: VertexOut, break_contours: bool) -> vec4f {
    // MVTs overlap their neighbors; half-open ownership prevents translucent
    // skirts from double-blending into visible tile seams.
    if any(in.tile_local < vec2f(0.0)) || any(in.tile_local >= vec2f(1.0)) {
        discard;
    }
    if break_contours && inside_gap(in.map_point) {
        discard;
    }
    var coverage = 1.0;
    if in.solid_radius >= 0.0 {
        let feather = max(fwidth(in.edge_distance), 0.65);
        coverage = clamp(
            (in.solid_radius + feather * 0.5 - abs(in.edge_distance)) / feather,
            0.0,
            1.0,
        );
    }
    return vec4f(in.color.rgb, in.color.a * coverage);
}

@fragment
fn fragment_gamma(in: VertexOut) -> @location(0) vec4f {
    return painted(in, false);
}

@fragment
fn fragment_gamma_relief(in: VertexOut) -> @location(0) vec4f {
    return painted(in, true);
}

fn linear_channel(encoded: f32) -> f32 {
    if encoded <= 0.04045 { return encoded / 12.92; }
    return pow((encoded + 0.055) / 1.055, 2.4);
}

@fragment
fn fragment_linear(in: VertexOut) -> @location(0) vec4f {
    let color = painted(in, false);
    return vec4f(
        linear_channel(color.r),
        linear_channel(color.g),
        linear_channel(color.b),
        color.a,
    );
}

@fragment
fn fragment_linear_relief(in: VertexOut) -> @location(0) vec4f {
    let color = painted(in, true);
    return vec4f(
        linear_channel(color.r),
        linear_channel(color.g),
        linear_channel(color.b),
        color.a,
    );
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relief_and_basemap_tiles_cannot_alias_gpu_residency() {
        let key = TileKey {
            zoom: 12,
            x: 1_204,
            y: 1_532,
        };
        let mut resident = HashSet::new();
        let corpus = VectorCorpus::mint();
        assert!(resident.insert(GpuKey {
            layer: VectorLayer::Basemap,
            corpus,
            tile: key,
        }));
        assert!(resident.insert(GpuKey {
            layer: VectorLayer::Relief,
            corpus,
            tile: key,
        }));
        assert_eq!(resident.len(), 2);
    }

    #[test]
    fn project_corpora_cannot_alias_gpu_residency() {
        let tile = TileKey {
            zoom: 12,
            x: 1_204,
            y: 1_532,
        };
        let mut resident = HashSet::new();
        assert!(resident.insert(GpuKey {
            layer: VectorLayer::Relief,
            corpus: VectorCorpus::mint(),
            tile,
        }));
        assert!(resident.insert(GpuKey {
            layer: VectorLayer::Relief,
            corpus: VectorCorpus::mint(),
            tile,
        }));
        assert_eq!(resident.len(), 2);
    }

    #[test]
    fn framebuffer_transfer_function_matches_egui() {
        assert_eq!(
            fragment_entry(wgpu::TextureFormat::Bgra8Unorm),
            "fragment_gamma"
        );
        assert_eq!(
            fragment_entry(wgpu::TextureFormat::Bgra8UnormSrgb),
            "fragment_linear"
        );
        assert_eq!(
            relief_fragment_entry(wgpu::TextureFormat::Bgra8Unorm),
            "fragment_gamma_relief"
        );
        assert_eq!(
            relief_fragment_entry(wgpu::TextureFormat::Bgra8UnormSrgb),
            "fragment_linear_relief"
        );
        assert_eq!(size_of::<VectorGap>(), 8 * size_of::<f32>());
    }

    #[test]
    fn stroke_vertex_keeps_onset_and_side_inside_seven_words() {
        assert_eq!(size_of::<StrokePoint>(), 7 * size_of::<f32>());
    }

    #[test]
    fn split_coordinates_hold_subpixel_precision_at_z24() {
        let center = 0.229_166_666_666_666_67_f64;
        let zoom = 24.0_f64;
        let world_points = 256.0 * zoom.exp2();
        let key = TileKey {
            zoom: 12,
            x: (center * 4096.0).floor() as u32,
            y: 0,
        };
        let tile = TileInstance::forge(key);
        let local = 1234.0_f32 / 4096.0;
        let [center_high, center_low] = split(center);
        let actual = f64::from(
            (tile.origin_high[0] - center_high)
                + (tile.origin_low[0] - center_low)
                + local * tile.span,
        ) * world_points;
        let point = (f64::from(key.x) + f64::from(local)) / 4096.0;
        let expected = (point - center) * world_points;
        assert!((actual - expected).abs() < 0.1, "{actual} != {expected}");
    }

    #[test]
    fn repetition_covers_every_world_crossing() {
        assert_eq!(wrap_radius(0.99, 0.5), 0);
        assert_eq!(wrap_radius(0.99, 0.229), 1);
        assert_eq!(wrap_radius(0.1, 0.99), 1);
        assert_eq!(wrap_radius(1.01, 0.5), 1);
        assert_eq!(wrap_radius(2.2, 0.5), 1);
        assert_eq!(wrap_radius(3.2, 0.5), 2);
        assert_eq!(
            wrap_radius(f32::INFINITY, 0.5) * 2 + 1,
            MAX_WRAP_INSTANCES as u32
        );
    }
}
