//! KaleidoscopeNode — radial mirror/symmetry effect (GPU-accelerated).
//!
//! Classic kaleidoscope: divide the output into `segments` equal wedges,
//! each a rotated + alternately-mirrored copy of the "seed" wedge around
//! the configured center. Entirely pixel-parallel, so the WGSL fragment
//! shader does the whole thing in one pass.
//!
//! Follows the ImageStyleNode template: per-node pipeline cache, opt-in
//! `needs_cpu_image_input(port) == false`, GPU path via
//! `evaluate_with_ctx` + `process_gpu_cached`, and a benign CPU fallback
//! (passthrough) for headless / no-wgpu contexts.

use crate::graph::{PortDef, PortKind, PortValue, ImageData};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;
use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;

// ── Defaults + struct ───────────────────────────────────────────────────────

fn default_segments() -> f32 { 6.0 }
fn default_zoom()     -> f32 { 1.0 }
fn default_half()     -> f32 { 0.5 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KaleidoscopeNode {
    #[serde(default = "default_segments")]
    pub segments: f32,
    #[serde(default)]
    pub rotation_deg: f32,
    #[serde(default = "default_zoom")]
    pub zoom: f32,
    #[serde(default = "default_half")]
    pub center_x: f32,
    #[serde(default = "default_half")]
    pub center_y: f32,
}

impl Default for KaleidoscopeNode {
    fn default() -> Self {
        Self {
            segments: 6.0,
            rotation_deg: 0.0,
            zoom: 1.0,
            center_x: 0.5,
            center_y: 0.5,
        }
    }
}

// ── GPU pipeline ────────────────────────────────────────────────────────────

const KALEIDOSCOPE_SHADER: &str = r#"
struct Params {
    segments:     f32,
    rotation_rad: f32,
    zoom:         f32,
    _pad0:        f32,
    center_x:     f32,
    center_y:     f32,
    width:        f32,
    height:       f32,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var input_tex:    texture_2d<f32>;
@group(0) @binding(2) var input_sampler: sampler;

const TWO_PI: f32 = 6.28318530718;

@vertex fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
    let pos = array(vec2f(-1, -1), vec2f(3, -1), vec2f(-1, 3));
    return vec4f(pos[vi], 0.0, 1.0);
}

@fragment fn fs_main(@builtin(position) coord: vec4f) -> @location(0) vec4f {
    let size   = vec2f(params.width, params.height);
    let center = vec2f(params.center_x, params.center_y);
    let zoom   = max(params.zoom, 0.001);
    let segs   = max(params.segments, 2.0);
    let wedge  = TWO_PI / segs;

    // Normalize to [0,1], center-relative.
    let uv       = coord.xy / size;
    let centered = (uv - center) / zoom;

    let r     = length(centered);
    var theta = atan2(centered.y, centered.x);

    // Fold theta into one wedge; alternate wedges mirror so the seams
    // line up (stained-glass look). k&1 catches both positive and
    // negative wrap-around since i32 is two's complement.
    let k = floor(theta / wedge);
    theta = theta - k * wedge;
    if ((i32(k) & 1) != 0) {
        theta = wedge - theta;
    }
    theta = theta + params.rotation_rad;

    // Convert polar → cartesian in source space and sample.
    let sampled = vec2f(cos(theta), sin(theta)) * r;
    let src_uv  = sampled + center;
    return textureSample(input_tex, input_sampler, clamp(src_uv, vec2f(0.0), vec2f(1.0)));
}
"#;

struct KaleidoscopeGpu {
    pipeline:          wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer:    wgpu::Buffer,
    sampler:           wgpu::Sampler,
}

struct KaleidoscopeStore {
    nodes: HashMap<crate::graph::NodeId, KaleidoscopeGpu>,
}

fn ensure_pipeline(
    node_id: crate::graph::NodeId,
    render_state: &eframe::egui_wgpu::RenderState,
) -> bool {
    let device = &render_state.device;
    let already = {
        let renderer = render_state.renderer.read();
        renderer.callback_resources.get::<KaleidoscopeStore>()
            .and_then(|s| s.nodes.get(&node_id))
            .is_some()
    };
    if already { return true; }

    device.push_error_scope(wgpu::ErrorFilter::Validation);

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kaleidoscope_shader"),
        source: wgpu::ShaderSource::Wgsl(KALEIDOSCOPE_SHADER.into()),
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("kaleidoscope_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("kaleidoscope_pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::TextureFormat::Rgba8UnormSrgb.into())],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    if pollster::block_on(device.pop_error_scope()).is_some() {
        crate::system_log::error("Kaleidoscope GPU pipeline creation failed".to_string());
        return false;
    }

    let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kaleidoscope_ub"),
        size: 32, // 8 floats × 4 bytes (std140-aligned)
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    });

    let gpu = KaleidoscopeGpu { pipeline, bind_group_layout, uniform_buffer, sampler };
    let mut renderer = render_state.renderer.write();
    if let Some(store) = renderer.callback_resources.get_mut::<KaleidoscopeStore>() {
        store.nodes.insert(node_id, gpu);
    } else {
        let mut nodes = HashMap::new();
        nodes.insert(node_id, gpu);
        renderer.callback_resources.insert(KaleidoscopeStore { nodes });
    }
    true
}

// ── GPU path ────────────────────────────────────────────────────────────────

impl KaleidoscopeNode {
    /// Run the kaleidoscope shader, publish the result into the shared
    /// GpuTextureCache so downstream GPU consumers can sample it
    /// zero-copy, and either read back to CPU or emit a `GpuImage` stub
    /// depending on `needs_readback`.
    pub fn process_gpu_cached(
        &self,
        img: &ImageData,
        node_id: crate::graph::NodeId,
        render_state: &eframe::egui_wgpu::RenderState,
        tex_cache: &mut crate::gpu_image::GpuTextureCache,
        gpu_source: Option<(crate::graph::NodeId, usize)>,
        needs_readback: bool,
    ) -> Option<crate::graph::PortValue> {
        let device = &render_state.device;
        let queue  = &render_state.queue;
        let w = img.width;
        let h = img.height;
        if w == 0 || h == 0 { return None; }

        if !ensure_pipeline(node_id, render_state) { return None; }

        // Prefer the upstream GPU-resident texture if available; only
        // CPU-upload when we have real pixels and no GPU source.
        let need_upload = !img.pixels.is_empty();
        let (input_view, _input_keepalive) = match gpu_source
            .and_then(|(nid, p)| tex_cache.get_node_output_cloned(nid, p))
            .filter(|(_, gw, gh)| *gw == w && *gh == h)
        {
            Some((view, _, _)) => (view, None),
            None if need_upload => {
                let tex = crate::gpu_image::upload_texture(device, queue, img, "kaleidoscope_input");
                let v = tex.create_view(&Default::default());
                (v, Some(tex))
            }
            None => return None,
        };

        let output_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kaleidoscope_output"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                 | wgpu::TextureUsages::COPY_SRC
                 | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let output_view = output_tex.create_view(&Default::default());

        let renderer = render_state.renderer.read();
        let store = renderer.callback_resources.get::<KaleidoscopeStore>()?;
        let gpu = store.nodes.get(&node_id)?;

        // Pack uniforms (std140: vec4 alignment per 16-byte slot).
        let rot_rad = self.rotation_deg.to_radians();
        let params: [f32; 8] = [
            self.segments.max(2.0),
            rot_rad,
            self.zoom.max(0.01),
            0.0, // pad
            self.center_x.clamp(0.0, 1.0),
            self.center_y.clamp(0.0, 1.0),
            w as f32,
            h as f32,
        ];
        queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::cast_slice(&params));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &gpu.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: gpu.uniform_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&input_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&gpu.sampler) },
            ],
        });

        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kaleidoscope_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            pass.set_pipeline(&gpu.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
        drop(renderer);

        if needs_readback {
            let result = crate::gpu_image::readback_texture(device, queue, &output_tex, w, h);
            tex_cache.publish(node_id, 0, output_tex, w, h, render_state);
            Some(crate::graph::PortValue::Image(result))
        } else {
            tex_cache.publish(node_id, 0, output_tex, w, h, render_state);
            Some(crate::graph::PortValue::GpuImage(crate::graph::GpuImageHandle {
                node_id,
                port: 0,
                width: w,
                height: h,
                frame_stamp: tex_cache.frame_stamp(node_id, 0),
            }))
        }
    }
}

// ── NodeBehavior ────────────────────────────────────────────────────────────

impl NodeBehavior for KaleidoscopeNode {
    fn title(&self)       -> &str   { "Kaleidoscope" }
    fn type_tag(&self)    -> &str   { "kaleidoscope" }
    fn color_hint(&self)  -> [u8;3] { [220, 140, 220] }
    fn min_width(&self)   -> Option<f32> { Some(240.0) }
    fn inline_ports(&self) -> bool  { true }
    fn needs_cpu_image_input(&self, _port: usize) -> bool { false }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Image",    PortKind::Image),
            PortDef::new("Segments", PortKind::Number),
            PortDef::new("Rotation", PortKind::Number),
            PortDef::new("Zoom",     PortKind::Number),
            PortDef::new("Center X", PortKind::Normalized),
            PortDef::new("Center Y", PortKind::Normalized),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> { vec![PortDef::new("Image", PortKind::Image)] }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // CPU fallback — we don't CPU-render the kaleidoscope, just
        // passthrough so the node is never a dead end when there's no
        // wgpu context (headless tests etc.).
        if let Some(PortValue::Float(v)) = inputs.get(1) { self.segments     = v.clamp(2.0, 32.0); }
        if let Some(PortValue::Float(v)) = inputs.get(2) { self.rotation_deg = v % 360.0; }
        if let Some(PortValue::Float(v)) = inputs.get(3) { self.zoom         = v.clamp(0.1, 4.0); }
        if let Some(PortValue::Float(v)) = inputs.get(4) { self.center_x     = v.clamp(0.0, 1.0); }
        if let Some(PortValue::Float(v)) = inputs.get(5) { self.center_y     = v.clamp(0.0, 1.0); }
        let result = match inputs.first() {
            Some(PortValue::Image(img)) => PortValue::Image(img.clone()),
            _ => PortValue::None,
        };
        vec![(0, result)]
    }

    fn evaluate_with_ctx(
        &mut self,
        inputs: &[PortValue],
        ctx: &mut crate::node_trait::EvalCtx<'_>,
    ) -> Vec<(usize, PortValue)> {
        // Apply param port overrides.
        if let Some(PortValue::Float(v)) = inputs.get(1) { self.segments     = v.clamp(2.0, 32.0); }
        if let Some(PortValue::Float(v)) = inputs.get(2) { self.rotation_deg = v % 360.0; }
        if let Some(PortValue::Float(v)) = inputs.get(3) { self.zoom         = v.clamp(0.1, 4.0); }
        if let Some(PortValue::Float(v)) = inputs.get(4) { self.center_x     = v.clamp(0.0, 1.0); }
        if let Some(PortValue::Float(v)) = inputs.get(5) { self.center_y     = v.clamp(0.0, 1.0); }

        // Dimensions can come from either a CPU `Image` or a GPU stub.
        let stub_owned: Option<ImageData> = match inputs.first() {
            Some(PortValue::GpuImage(h)) => Some(ImageData {
                width: h.width, height: h.height, pixels: Vec::new(),
            }),
            _ => None,
        };
        let img_ref: Option<&ImageData> = match inputs.first() {
            Some(PortValue::Image(img))    => Some(img.as_ref()),
            Some(PortValue::GpuImage(_))   => stub_owned.as_ref(),
            _ => None,
        };
        let img = match img_ref {
            Some(img) => img,
            None => return vec![(0, PortValue::None)],
        };

        // GPU path.
        if let Some(rs) = ctx.render_state {
            let gpu_src = ctx.input_sources.first().copied().flatten();
            if let Some(val) = self.process_gpu_cached(
                img, ctx.node_id, rs, ctx.gpu_tex_cache,
                gpu_src, ctx.needs_readback,
            ) {
                return vec![(0, val)];
            }
        }

        // Fallback (no wgpu): passthrough.
        self.evaluate(inputs)
    }

    fn save_state(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or_default() }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<KaleidoscopeNode>(state.clone()) { *self = l; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let blue = egui::Color32::from_rgb(80, 170, 255);

        // Image input port
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Image);
            ui.label(egui::RichText::new("Image").small());
        });

        // Helper closure for the parameter rows (port + optional slider).
        let mut param_row = |ui: &mut egui::Ui,
                             label: &str, port: usize,
                             val: &mut f32,
                             kind: PortKind,
                             range: std::ops::RangeInclusive<f32>,
                             step: f32,
                             suffix: &str| {
            let wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == port);
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(ui, ctx.node_id, port, true, ctx.connections,
                    ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, kind);
                ui.label(egui::RichText::new(label).small());
                if wired {
                    ui.label(egui::RichText::new(format!("{:.2}{}", *val, suffix)).small().color(blue));
                } else {
                    ui.add(egui::Slider::new(val, range).step_by(step as f64).show_value(true).suffix(suffix));
                }
            });
        };

        param_row(ui, "Segments", 1, &mut self.segments,     PortKind::Number,     2.0..=32.0,  1.0, "");
        param_row(ui, "Rotation", 2, &mut self.rotation_deg, PortKind::Number,     0.0..=360.0, 1.0, "°");
        param_row(ui, "Zoom",     3, &mut self.zoom,         PortKind::Number,     0.1..=4.0,   0.01, "");
        param_row(ui, "Center X", 4, &mut self.center_x,     PortKind::Normalized, 0.0..=1.0,   0.01, "");
        param_row(ui, "Center Y", 5, &mut self.center_y,     PortKind::Normalized, 0.0..=1.0,   0.01, "");

        ui.label(egui::RichText::new("⚡ GPU").small().color(dim));

        ui.separator();
        crate::nodes::audio_port_row(ui, "Image", ctx.node_id, 0, false, ctx.port_positions,
            ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Image);
    }
}

// ── Registration + cleanup ──────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("kaleidoscope", |state| {
        if let Ok(n) = serde_json::from_value::<KaleidoscopeNode>(state.clone()) { Box::new(n) }
        else { Box::new(KaleidoscopeNode::default()) }
    });
}

pub(crate) fn cleanup_node(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
    node_id: crate::graph::NodeId,
) {
    if let Some(store) = callback_resources.get_mut::<KaleidoscopeStore>() {
        store.nodes.remove(&node_id);
    }
}

pub(crate) fn cleanup_all(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
) {
    if let Some(store) = callback_resources.get_mut::<KaleidoscopeStore>() {
        store.nodes.clear();
    }
}
