use crate::graph::*;
use crate::gpu_image::GpuBlendCallback;
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::nodes::{inline_port_circle, output_port_row};
use eframe::egui;
use eframe::egui_wgpu;
use eframe::egui_wgpu::wgpu;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::sync::Arc;

const BLEND_MODES: &[&str] = &["Normal", "Multiply", "Screen", "Overlay", "Add", "Difference", "Soft Light", "Hard Light"];

// ── Node struct ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlendNode {
    #[serde(default)]
    pub mode: u8,
    #[serde(default = "default_half")]
    pub mix: f32,
}

fn default_half() -> f32 { 0.5 }

impl Default for BlendNode {
    fn default() -> Self { Self { mode: 0, mix: 0.5 } }
}

impl NodeBehavior for BlendNode {
    fn title(&self)        -> &str    { "Blend" }
    fn type_tag(&self)     -> &str    { "blend" }
    fn color_hint(&self)   -> [u8; 3] { [160, 100, 180] }
    fn inline_ports(&self) -> bool    { true }

    fn needs_cpu_image_input(&self, _port: usize) -> bool { false }

    fn blend_params(&self) -> Option<(u8, f32)> { Some((self.mode, self.mix)) }

    fn evaluate_with_ctx(
        &mut self,
        inputs: &[PortValue],
        ctx: &mut crate::node_trait::EvalCtx<'_>,
    ) -> Vec<(usize, PortValue)> {
        // Read Mix from port 2 if wired, else use stored value
        let mix = if inputs.get(2).map(|v| !matches!(v, PortValue::None)).unwrap_or(false) {
            inputs.get(2).map(|v| v.as_float().clamp(0.0, 1.0)).unwrap_or(self.mix)
        } else {
            self.mix
        };

        // Extract image refs from inputs (CPU Image or GPU stub)
        let stub_a: Option<ImageData> = match inputs.first() {
            Some(PortValue::GpuImage(h)) => Some(ImageData { width: h.width, height: h.height, pixels: Vec::new() }),
            _ => None,
        };
        let stub_b: Option<ImageData> = match inputs.get(1) {
            Some(PortValue::GpuImage(h)) => Some(ImageData { width: h.width, height: h.height, pixels: Vec::new() }),
            _ => None,
        };
        let a: Option<&ImageData> = match inputs.first() {
            Some(PortValue::Image(img)) => Some(img.as_ref()),
            Some(PortValue::GpuImage(_)) => stub_a.as_ref(),
            _ => None,
        };
        let b: Option<&ImageData> = match inputs.get(1) {
            Some(PortValue::Image(img)) => Some(img.as_ref()),
            Some(PortValue::GpuImage(_)) => stub_b.as_ref(),
            _ => None,
        };

        if let (Some(a), Some(b)) = (a, b) {
            // Try GPU path
            if let Some(rs) = ctx.render_state {
                let gpu_src_a = ctx.input_sources.first().copied().flatten();
                let gpu_src_b = ctx.input_sources.get(1).copied().flatten();
                if let Some(val) = process_gpu_cached(
                    a, b, self.mode, mix, ctx.node_id, rs,
                    ctx.gpu_tex_cache, gpu_src_a, gpu_src_b,
                    ctx.needs_readback,
                ) {
                    return vec![(0, val)];
                }
            }
            // CPU fallback
            if !a.pixels.is_empty() && !b.pixels.is_empty() {
                return vec![(0, PortValue::Image(process(a, b, self.mode, mix)))];
            }
        }
        vec![(0, PortValue::None)]
    }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("A",   PortKind::Image),
            PortDef::new("B",   PortKind::Image),
            PortDef::new("Mix", PortKind::Normalized),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Image", PortKind::Image)]
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({ "mode": self.mode, "mix": self.mix })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(m) = state.get("mode").and_then(|v| v.as_u64()) { self.mode = m as u8; }
        if let Some(m) = state.get("mix").and_then(|v| v.as_f64())  { self.mix = m as f32; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let target_format = ctx.wgpu_render_state
            .map(|rs| rs.target_format)
            .unwrap_or(eframe::egui_wgpu::wgpu::TextureFormat::Bgra8UnormSrgb);
        render(
            ui,
            &mut self.mode,
            &mut self.mix,
            ctx.node_id,
            ctx.values,
            ctx.connections,
            target_format,
            ctx.port_positions,
            ctx.dragging_from,
            ctx.pending_disconnects,
        );
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("blend", |state| {
        let mut n = BlendNode::default();
        n.load_state(state);
        Box::new(n)
    });
}

// ── Render function ───────────────────────────────────────────────────────────

pub fn render(
    ui: &mut egui::Ui,
    mode: &mut u8,
    mix: &mut f32,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    target_format: eframe::egui_wgpu::wgpu::TextureFormat,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {

    let mix_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 2);

    // When Mix port is wired, use the wired value for display; the eval loop
    // reads it ephemerally from port values, so we only update the local copy here.
    if mix_wired {
        *mix = Graph::static_input_value(connections, values, node_id, 2).as_float();
    }

    let a = Graph::static_input_value(connections, values, node_id, 0);
    let b = Graph::static_input_value(connections, values, node_id, 1);
    let has_a = matches!(&a, PortValue::Image(_) | PortValue::GpuImage(_));
    let has_b = matches!(&b, PortValue::Image(_) | PortValue::GpuImage(_));

    fn dim_label(v: &PortValue) -> Option<(u32, u32)> {
        match v {
            PortValue::Image(img) => Some((img.width, img.height)),
            PortValue::GpuImage(h) => Some((h.width, h.height)),
            _ => None,
        }
    }

    // Port 0: Image A
    ui.horizontal(|ui| {
        inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Image);
        ui.label(egui::RichText::new("A:").small());
        match dim_label(&a) {
            Some((w, h)) => { ui.label(egui::RichText::new(format!("[{}x{}]", w, h)).small().color(egui::Color32::from_rgb(80, 170, 255))); }
            None => { ui.label(egui::RichText::new("—").small().color(egui::Color32::GRAY)); }
        }
    });

    // Port 1: Image B
    ui.horizontal(|ui| {
        inline_port_circle(ui, node_id, 1, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Image);
        ui.label(egui::RichText::new("B:").small());
        match dim_label(&b) {
            Some((w, h)) => { ui.label(egui::RichText::new(format!("[{}x{}]", w, h)).small().color(egui::Color32::from_rgb(80, 170, 255))); }
            None => { ui.label(egui::RichText::new("—").small().color(egui::Color32::GRAY)); }
        }
    });

    // Port 2: Mix — inline_port_circle + slider or wired value
    ui.horizontal(|ui| {
        inline_port_circle(ui, node_id, 2, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Normalized);
        ui.label(egui::RichText::new("Mix:").small());
        if mix_wired {
            ui.label(egui::RichText::new(format!("{:.2}", *mix)).small().monospace().color(egui::Color32::from_rgb(80, 170, 255)));
        }
    });
    if !mix_wired {
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.add(egui::Slider::new(mix, 0.0..=1.0).show_value(true));
        });
    }

    ui.separator();

    // Mode dropdown
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Mode:").small());
        egui::ComboBox::from_id_salt(egui::Id::new(("blend_mode", node_id)))
            .selected_text(*BLEND_MODES.get(*mode as usize).unwrap_or(&"Normal"))
            .show_ui(ui, |ui| {
                for (i, name) in BLEND_MODES.iter().enumerate() {
                    if ui.selectable_label(*mode == i as u8, *name).clicked() {
                        *mode = i as u8;
                    }
                }
            });
    });

    // Status + Preview
    if has_a && has_b {
        if let (PortValue::Image(img_a), PortValue::Image(img_b)) = (&a, &b) {
            let preview_w = ui.available_width().min(250.0);
            let aspect = img_a.height as f32 / img_a.width as f32;
            let preview_h = preview_w * aspect;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(preview_w, preview_h), egui::Sense::hover());

            let target_format = target_format;
            ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                rect,
                GpuBlendCallback {
                    node_id, mode: *mode as u32, mix: *mix,
                    img_a: img_a.clone(), img_b: img_b.clone(), target_format,
                },
            ));
        }
    } else {
        if !has_a { ui.colored_label(egui::Color32::GRAY, "Connect Image A"); }
        if !has_b { ui.colored_label(egui::Color32::GRAY, "Connect Image B"); }
    }

    // Output port
    ui.separator();
    let out_val = match values.get(&(node_id, 0)) {
        Some(PortValue::Image(img)) => format!("[{}x{}]", img.width, img.height),
        Some(PortValue::GpuImage(h)) => format!("[{}x{}]", h.width, h.height),
        _ => "—".into(),
    };
    output_port_row(ui, "Image", &out_val, node_id, 0, port_positions, dragging_from, connections, pending_disconnects, PortKind::Image);
}

/// Blend two images. Called during evaluation.
///
/// CPU fallback (GPU shader path lives below). Two wins over the
/// old version:
///
/// 1. u32-granular reads and a single u32 store per output pixel
///    replaces 4 per-byte writes plus per-byte bounds checks.
/// 2. Inline-unrolled the three colour channels so the `mode` match
///    runs once per 3 components of the blended output rather than
///    being re-dispatched per channel inside a `for c in 0..3` loop.
///
/// Bounds-mismatched rows (one source wider/taller than the other)
/// still take a safe path via the clamped `w`/`h` dimensions.
pub fn process(a: &ImageData, b: &ImageData, mode: u8, mix: f32) -> Arc<ImageData> {
    let w = a.width.min(b.width);
    let h = a.height.min(b.height);
    let out_bytes = (w * h * 4) as usize;
    let mut pixels: Vec<u8> = Vec::with_capacity(out_bytes);
    // SAFETY: every output pixel is written via the u32 store below.
    unsafe { pixels.set_len(out_bytes); }

    let src_a: &[u32] = bytemuck::cast_slice(&a.pixels);
    let src_b: &[u32] = bytemuck::cast_slice(&b.pixels);
    let dst_u32: &mut [u32] = bytemuck::cast_slice_mut(&mut pixels);

    let a_w = a.width as usize;
    let b_w = b.width as usize;
    let o_w = w as usize;
    let inv_255 = 1.0f32 / 255.0;
    let one_minus_mix = 1.0 - mix;

    // Extract one RGB channel from a little-endian RGBA u32.
    #[inline(always)]
    fn unpack(px: u32) -> (f32, f32, f32) {
        let r = (px & 0xFF) as f32;
        let g = ((px >> 8) & 0xFF) as f32;
        let b = ((px >> 16) & 0xFF) as f32;
        (r, g, b)
    }

    for y in 0..h as usize {
        let ra = y * a_w;
        let rb = y * b_w;
        let ro = y * o_w;
        for x in 0..o_w {
            let (ar, ag, ab) = unpack(src_a[ra + x]);
            let (br, bg, bb) = unpack(src_b[rb + x]);
            let va = (ar * inv_255, ag * inv_255, ab * inv_255);
            let vb = (br * inv_255, bg * inv_255, bb * inv_255);

            #[inline(always)]
            fn blend_channel(va: f32, vb: f32, mode: u8) -> f32 {
                match mode {
                    0 => vb,
                    1 => va * vb,
                    2 => 1.0 - (1.0 - va) * (1.0 - vb),
                    3 => if va < 0.5 { 2.0 * va * vb } else { 1.0 - 2.0 * (1.0 - va) * (1.0 - vb) },
                    4 => (va + vb).min(1.0),
                    5 => (va - vb).abs(),
                    6 => if vb < 0.5 {
                            va - (1.0 - 2.0 * vb) * va * (1.0 - va)
                         } else {
                            va + (2.0 * vb - 1.0) * (va.sqrt() - va)
                         },
                    7 => if vb < 0.5 { 2.0 * va * vb } else { 1.0 - 2.0 * (1.0 - va) * (1.0 - vb) },
                    _ => vb,
                }
            }
            let br_blend = blend_channel(va.0, vb.0, mode);
            let bg_blend = blend_channel(va.1, vb.1, mode);
            let bb_blend = blend_channel(va.2, vb.2, mode);

            let r = (va.0 * one_minus_mix + br_blend * mix).clamp(0.0, 1.0);
            let g = (va.1 * one_minus_mix + bg_blend * mix).clamp(0.0, 1.0);
            let b = (va.2 * one_minus_mix + bb_blend * mix).clamp(0.0, 1.0);

            let rb = (r * 255.0) as u32;
            let gb = (g * 255.0) as u32;
            let bb = (b * 255.0) as u32;
            // RGBA → A<<24 | B<<16 | G<<8 | R (little-endian).
            dst_u32[ro + x] = 0xFF00_0000 | (bb << 16) | (gb << 8) | rb;
        }
    }
    Arc::new(ImageData { width: w, height: h, pixels })
}

// ── GPU-accelerated blend ───────────────────────────────────────────────────

const BLEND_SHADER: &str = r#"
struct Params {
    mode: f32,
    mix: f32,
    width: f32,
    height: f32,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var tex_a: texture_2d<f32>;
@group(0) @binding(2) var tex_b: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;

@vertex fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
    let pos = array(vec2f(-1,-1), vec2f(3,-1), vec2f(-1,3));
    return vec4f(pos[vi], 0, 1);
}

@fragment fn fs_main(@builtin(position) coord: vec4f) -> @location(0) vec4f {
    let uv = coord.xy / vec2f(params.width, params.height);
    let a = textureSample(tex_a, tex_sampler, uv);
    let b = textureSample(tex_b, tex_sampler, uv);
    let mode = u32(params.mode);
    let mix = params.mix;

    var blended: vec3f;
    if mode == 0u { // Normal
        blended = a.rgb * (1.0 - mix) + b.rgb * mix;
    } else if mode == 1u { // Multiply
        blended = a.rgb * b.rgb;
    } else if mode == 2u { // Screen
        blended = 1.0 - (1.0 - a.rgb) * (1.0 - b.rgb);
    } else if mode == 3u { // Overlay
        blended = select(
            1.0 - 2.0 * (1.0 - a.rgb) * (1.0 - b.rgb),
            2.0 * a.rgb * b.rgb,
            a.rgb < vec3f(0.5)
        );
    } else if mode == 4u { // Add
        blended = min(a.rgb + b.rgb, vec3f(1.0));
    } else if mode == 5u { // Difference
        blended = abs(a.rgb - b.rgb);
    } else if mode == 6u { // Soft Light
        blended = select(
            a.rgb + (2.0 * b.rgb - 1.0) * (sqrt(a.rgb) - a.rgb),
            a.rgb - (1.0 - 2.0 * b.rgb) * a.rgb * (1.0 - a.rgb),
            b.rgb < vec3f(0.5)
        );
    } else { // Hard Light
        blended = select(
            1.0 - 2.0 * (1.0 - a.rgb) * (1.0 - b.rgb),
            2.0 * a.rgb * b.rgb,
            b.rgb < vec3f(0.5)
        );
    }

    let result = a.rgb * (1.0 - mix) + blended * mix;
    return vec4f(clamp(result, vec3f(0.0), vec3f(1.0)), 1.0);
}
"#;

struct BlendGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
}

struct BlendGpuStore {
    nodes: HashMap<NodeId, BlendGpu>,
}

/// Create the per-node blend pipeline if it isn't already cached in
/// `callback_resources`. Returns `true` on success (pipeline now
/// present) or `false` if shader/pipeline validation failed. Extracted
/// so `process_gpu_cached` can ensure the pipeline without falling back
/// to the CPU-upload-heavy `process_gpu` path, which would corrupt
/// stub `GpuImage` inputs (empty pixels) into solid black textures.
fn ensure_pipeline(
    node_id: NodeId,
    render_state: &egui_wgpu::RenderState,
) -> bool {
    let device = &render_state.device;
    let already = {
        let renderer = render_state.renderer.read();
        renderer.callback_resources.get::<BlendGpuStore>()
            .and_then(|s| s.nodes.get(&node_id))
            .is_some()
    };
    if already { return true; }

    device.push_error_scope(wgpu::ErrorFilter::Validation);

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("blend_gpu_shader"),
        source: wgpu::ShaderSource::Wgsl(BLEND_SHADER.into()),
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("blend_gpu_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0, visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1, visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2, visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3, visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None, bind_group_layouts: &[&bind_group_layout], push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("blend_gpu_pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_main"), buffers: &[], compilation_options: wgpu::PipelineCompilationOptions::default() },
        fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_main"), targets: &[Some(wgpu::TextureFormat::Rgba8UnormSrgb.into())], compilation_options: wgpu::PipelineCompilationOptions::default() }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None, multisample: wgpu::MultisampleState::default(), multiview: None, cache: None,
    });

    let error = pollster::block_on(device.pop_error_scope());
    if error.is_some() { return false; }

    let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("blend_gpu_ub"), size: 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });

    let gpu = BlendGpu { pipeline, bind_group_layout, uniform_buffer, sampler };
    let mut renderer = render_state.renderer.write();
    if let Some(store) = renderer.callback_resources.get_mut::<BlendGpuStore>() {
        store.nodes.insert(node_id, gpu);
    } else {
        let mut nodes = HashMap::new();
        nodes.insert(node_id, gpu);
        renderer.callback_resources.insert(BlendGpuStore { nodes });
    }
    true
}

#[allow(dead_code)]
pub fn process_gpu(
    a: &ImageData, b: &ImageData,
    mode: u8, mix: f32,
    node_id: NodeId,
    render_state: &egui_wgpu::RenderState,
) -> Option<Arc<ImageData>> {
    let device = &render_state.device;
    let queue = &render_state.queue;
    let w = a.width.min(b.width);
    let h = a.height.min(b.height);
    if w == 0 || h == 0 { return None; }

    if !ensure_pipeline(node_id, render_state) { return None; }

    let tex_a = crate::gpu_image::upload_texture(device, queue, a, "blend_a");
    let tex_b = crate::gpu_image::upload_texture(device, queue, b, "blend_b");
    let view_a = tex_a.create_view(&Default::default());
    let view_b = tex_b.create_view(&Default::default());

    let output_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("blend_output"), size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output_view = output_tex.create_view(&Default::default());

    let renderer = render_state.renderer.read();
    let store = renderer.callback_resources.get::<BlendGpuStore>()?;
    let gpu = store.nodes.get(&node_id)?;

    let params = [mode as f32, mix, w as f32, h as f32];
    queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::cast_slice(&params));

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None, layout: &gpu.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: gpu.uniform_buffer.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view_a) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&view_b) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&gpu.sampler) },
        ],
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blend_gpu_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &output_view, resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None, ..Default::default()
        });
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    queue.submit(Some(encoder.finish()));
    drop(renderer);

    Some(crate::gpu_image::readback_texture(device, queue, &output_tex, w, h))
}

pub fn process_gpu_cached(
    a: &ImageData, b: &ImageData, mode: u8, mix: f32,
    node_id: NodeId, render_state: &egui_wgpu::RenderState,
    tex_cache: &mut crate::gpu_image::GpuTextureCache,
    gpu_source_a: Option<(NodeId, usize)>,
    gpu_source_b: Option<(NodeId, usize)>,
    needs_readback: bool,
) -> Option<PortValue> {
    let device = &render_state.device;
    let queue = &render_state.queue;
    let w = a.width.min(b.width);
    let h = a.height.min(b.height);
    if w == 0 || h == 0 { return None; }

    // Ensure pipeline exists. We must NOT fall back to `process_gpu` here:
    // it CPU-uploads `a`/`b` via `upload_texture`, which silently produces a
    // black 1×1 texture for any input that arrived as a `GpuImage` stub
    // (empty `pixels`). That is the post-reload "background goes black"
    // bug — on the first frame after load, the upstream WGSL Viewer hands
    // Blend a stub, the no-pipeline branch black-uploads it, and the
    // result poisons the param-cache for the rest of the session.
    if !ensure_pipeline(node_id, render_state) { return None; }

    // Bind both inputs from the upstream GPU cache when available; only
    // upload from CPU as a fallback. Each input is checked independently.
    // Dimension guards use the per-input source size, not the post-min `w/h`.
    let need_upload_a = !a.pixels.is_empty();
    let need_upload_b = !b.pixels.is_empty();
    let (view_a, _keepalive_a) = match gpu_source_a
        .and_then(|(nid, p)| tex_cache.get_node_output_cloned(nid, p))
        .filter(|(_, gw, gh)| *gw == a.width && *gh == a.height)
    {
        Some((view, _, _)) => (view, None),
        None if need_upload_a => {
            let tex = crate::gpu_image::upload_texture(device, queue, a, "blend_a");
            let v = tex.create_view(&Default::default());
            (v, Some(tex))
        }
        None => return None,
    };
    let (view_b, _keepalive_b) = match gpu_source_b
        .and_then(|(nid, p)| tex_cache.get_node_output_cloned(nid, p))
        .filter(|(_, gw, gh)| *gw == b.width && *gh == b.height)
    {
        Some((view, _, _)) => (view, None),
        None if need_upload_b => {
            let tex = crate::gpu_image::upload_texture(device, queue, b, "blend_b");
            let v = tex.create_view(&Default::default());
            (v, Some(tex))
        }
        None => return None,
    };

    let output_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("blend_output"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let output_view = output_tex.create_view(&Default::default());

    let renderer = render_state.renderer.read();
    let store = renderer.callback_resources.get::<BlendGpuStore>()?;
    let gpu = store.nodes.get(&node_id)?;

    let params = [mode as f32, mix, w as f32, h as f32];
    queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::cast_slice(&params));

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None, layout: &gpu.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: gpu.uniform_buffer.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view_a) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&view_b) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&gpu.sampler) },
        ],
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blend_gpu_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &output_view, resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None, ..Default::default()
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
        Some(PortValue::Image(result))
    } else {
        tex_cache.publish(node_id, 0, output_tex, w, h, render_state);
        Some(PortValue::GpuImage(GpuImageHandle {
            node_id,
            port: 0,
            width: w,
            height: h,
            frame_stamp: tex_cache.frame_stamp(node_id, 0),
        }))
    }
}

/// Drop all GPU resources for a deleted Blend node so VRAM is
/// released immediately. Called from
/// `crate::gpu_image::GpuTextureCache::invalidate_node`.
pub(crate) fn cleanup_node(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
    node_id: crate::graph::NodeId,
) {
    if let Some(store) = callback_resources.get_mut::<BlendGpuStore>() {
        store.nodes.remove(&node_id);
    }
}

/// Wipe every Blend GPU pipeline. Called by
/// `GpuTextureCache::clear_all` when a new project is loaded so VRAM
/// from the previous project doesn't leak forever (and node-id
/// collisions can't reuse stale pipelines).
pub(crate) fn cleanup_all(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
) {
    if let Some(store) = callback_resources.get_mut::<BlendGpuStore>() {
        store.nodes.clear();
    }
}
