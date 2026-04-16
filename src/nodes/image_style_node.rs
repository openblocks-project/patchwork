//! ImageStyleNode — Blur, Pixelate, Sharpen for images (GPU-accelerated).

use crate::graph::{PortDef, PortKind, PortValue, ImageData};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;
use eframe::egui_wgpu::wgpu;
use std::sync::Arc;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum StyleMode {
    Blur,
    Pixelate,
    Sharpen,
}

impl Default for StyleMode {
    fn default() -> Self { StyleMode::Blur }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageStyleNode {
    #[serde(default)]
    pub mode: StyleMode,
    #[serde(default = "default_amount")]
    pub amount: f32,
}

fn default_amount() -> f32 { 3.0 }

impl Default for ImageStyleNode {
    fn default() -> Self {
        Self { mode: StyleMode::Blur, amount: 3.0 }
    }
}

// ── GPU Pipeline ────────────────────────────────────────────────────────────

const EFFECT_SHADER: &str = r#"
struct Params {
    mode: f32,
    amount: f32,
    width: f32,
    height: f32,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var input_tex: texture_2d<f32>;
@group(0) @binding(2) var input_sampler: sampler;

@vertex fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
    let pos = array(vec2f(-1,-1), vec2f(3,-1), vec2f(-1,3));
    return vec4f(pos[vi], 0, 1);
}

@fragment fn fs_main(@builtin(position) coord: vec4f) -> @location(0) vec4f {
    let uv = coord.xy / vec2f(params.width, params.height);
    let texel = vec2f(1.0 / params.width, 1.0 / params.height);
    let mode = u32(params.mode);

    if mode == 0u {
        // Box blur — sample (2r+1)^2 neighborhood
        var col = vec4f(0.0);
        let r = i32(params.amount);
        var count = 0.0;
        for (var dy = -r; dy <= r; dy++) {
            for (var dx = -r; dx <= r; dx++) {
                let sample_uv = uv + vec2f(f32(dx), f32(dy)) * texel;
                col += textureSample(input_tex, input_sampler, clamp(sample_uv, vec2f(0.0), vec2f(1.0)));
                count += 1.0;
            }
        }
        return col / count;
    } else if mode == 1u {
        // Pixelate — quantize UV to block grid
        let block = max(params.amount, 2.0);
        let pixel_coord = floor(coord.xy / block) * block + block * 0.5;
        let puv = pixel_coord / vec2f(params.width, params.height);
        return textureSample(input_tex, input_sampler, clamp(puv, vec2f(0.0), vec2f(1.0)));
    } else {
        // Sharpen — unsharp mask (center vs 4-tap average)
        let c = textureSample(input_tex, input_sampler, uv);
        let avg = (
            textureSample(input_tex, input_sampler, uv + vec2f(-texel.x, 0.0)) +
            textureSample(input_tex, input_sampler, uv + vec2f(texel.x, 0.0)) +
            textureSample(input_tex, input_sampler, uv + vec2f(0.0, -texel.y)) +
            textureSample(input_tex, input_sampler, uv + vec2f(0.0, texel.y))
        ) * 0.25;
        return clamp(c + (c - avg) * params.amount, vec4f(0.0), vec4f(1.0));
    }
}
"#;

struct ImageStyleGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
}

struct ImageStyleStore {
    nodes: HashMap<crate::graph::NodeId, ImageStyleGpu>,
}

/// Create the per-node Image Style pipeline if not already cached.
/// Same rationale as `blend::ensure_pipeline` — exists so
/// `process_gpu_cached` can ensure the pipeline without falling back to
/// the CPU-upload `process_gpu` path, which would corrupt stub
/// `GpuImage` inputs (empty `pixels`) into a solid black 1×1 texture.
fn ensure_pipeline(
    node_id: crate::graph::NodeId,
    render_state: &eframe::egui_wgpu::RenderState,
) -> bool {
    let device = &render_state.device;
    let already = {
        let renderer = render_state.renderer.read();
        renderer.callback_resources.get::<ImageStyleStore>()
            .and_then(|s| s.nodes.get(&node_id))
            .is_some()
    };
    if already { return true; }

    device.push_error_scope(wgpu::ErrorFilter::Validation);

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("image_style_shader"),
        source: wgpu::ShaderSource::Wgsl(EFFECT_SHADER.into()),
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("image_style_bgl"),
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
        label: Some("image_style_pipeline"),
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

    let error = pollster::block_on(device.pop_error_scope());
    if error.is_some() {
        crate::system_log::error("Image Style GPU pipeline creation failed".to_string());
        return false;
    }

    let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("image_style_ub"),
        size: 16, // 4 floats × 4 bytes
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    let gpu = ImageStyleGpu { pipeline, bind_group_layout, uniform_buffer, sampler };
    let mut renderer = render_state.renderer.write();
    if let Some(store) = renderer.callback_resources.get_mut::<ImageStyleStore>() {
        store.nodes.insert(node_id, gpu);
    } else {
        let mut nodes = HashMap::new();
        nodes.insert(node_id, gpu);
        renderer.callback_resources.insert(ImageStyleStore { nodes });
    }
    true
}

impl ImageStyleNode {
    #[allow(dead_code)]
    pub fn process_gpu(
        &self,
        img: &ImageData,
        node_id: crate::graph::NodeId,
        render_state: &eframe::egui_wgpu::RenderState,
    ) -> Option<Arc<ImageData>> {
        let device = &render_state.device;
        let queue = &render_state.queue;
        let w = img.width;
        let h = img.height;
        if w == 0 || h == 0 { return None; }

        if !ensure_pipeline(node_id, render_state) { return None; }

        // Upload input image as texture
        let input_tex = crate::gpu_image::upload_texture(device, queue, img, "image_style_input");
        let input_view = input_tex.create_view(&Default::default());

        // Create output texture
        let output_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image_style_output"),
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

        // Upload uniforms and render
        let renderer = render_state.renderer.read();
        let store = renderer.callback_resources.get::<ImageStyleStore>()?;
        let gpu = store.nodes.get(&node_id)?;

        let mode_val = match self.mode {
            StyleMode::Blur => 0.0f32,
            StyleMode::Pixelate => 1.0,
            StyleMode::Sharpen => 2.0,
        };
        let params = [mode_val, self.amount, w as f32, h as f32];
        queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::cast_slice(&params));

        // Create bind group with this frame's input texture
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
                label: Some("image_style_pass"),
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

        Some(crate::gpu_image::readback_texture(device, queue, &output_tex, w, h))
    }

    /// GPU processing with output caching for zero-copy display.
    /// Mirrors `process_gpu` but, after readback, hands the freshly rendered
    /// `wgpu::Texture` directly to `GpuTextureCache::cache_node_output` so the
    /// eval loop in `app/mod.rs` does not have to re-`upload_texture` the
    /// pixels we already had in VRAM. Saves one CPU→GPU copy + one texture
    /// allocation per frame whenever an Image Style node is in the chain.
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
        let queue = &render_state.queue;
        let w = img.width;
        let h = img.height;
        if w == 0 || h == 0 { return None; }

        // Ensure pipeline exists. We must NOT fall back to `process_gpu`:
        // it CPU-uploads `img` via `upload_texture`, which silently
        // produces a black 1×1 texture if `img.pixels` is empty (which
        // is the case when the input arrived as a `GpuImage` stub from
        // an upstream GPU node like WGSL Viewer or Blend). The black
        // result then poisons the per-node `("dyn_img_cache", id)`
        // param cache for the rest of the session.
        if !ensure_pipeline(node_id, render_state) { return None; }

        // Bind input from upstream GPU cache when available; only upload from
        // CPU as a fallback. Saves one CPU→GPU upload per frame in fully GPU
        // chains. The cached view's backing texture is kept alive by the
        // cache itself; the upload-fallback's texture is held in
        // `_input_keepalive` until command submission.
        let need_upload = !img.pixels.is_empty();
        let (input_view, _input_keepalive) = match gpu_source
            .and_then(|(nid, p)| tex_cache.get_node_output_cloned(nid, p))
            .filter(|(_, gw, gh)| *gw == w && *gh == h)
        {
            Some((view, _, _)) => (view, None),
            None if need_upload => {
                let tex = crate::gpu_image::upload_texture(device, queue, img, "image_style_input");
                let v = tex.create_view(&Default::default());
                (v, Some(tex))
            }
            None => return None,
        };

        // Output texture — TEXTURE_BINDING so the display callback can sample it
        let output_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image_style_output_cached"),
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
        let store = renderer.callback_resources.get::<ImageStyleStore>()?;
        let gpu = store.nodes.get(&node_id)?;

        let mode_val = match self.mode {
            StyleMode::Blur => 0.0f32,
            StyleMode::Pixelate => 1.0,
            StyleMode::Sharpen => 2.0,
        };
        let params = [mode_val, self.amount, w as f32, h as f32];
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
                label: Some("image_style_pass_cached"),
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

impl NodeBehavior for ImageStyleNode {
    fn title(&self) -> &str { "Image Style" }
    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Image", PortKind::Image),
            PortDef::new("Amount", PortKind::Number),
        ]
    }
    fn outputs(&self) -> Vec<PortDef> { vec![PortDef::new("Image", PortKind::Image)] }
    fn color_hint(&self) -> [u8; 3] { [200, 120, 180] }
    fn inline_ports(&self) -> bool { true }

    /// GPU-native node: receives GpuImage inputs directly (no readback).
    fn needs_cpu_image_input(&self, _port: usize) -> bool { false }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // CPU-only fallback: apply port inputs and pass through the image.
        // Used when evaluate_with_ctx is not available (e.g., legacy callers).
        if let Some(PortValue::Float(v)) = inputs.get(1) {
            self.amount = match self.mode {
                StyleMode::Blur => v.clamp(1.0, 20.0),
                StyleMode::Pixelate => v.clamp(2.0, 64.0),
                StyleMode::Sharpen => v.clamp(0.1, 5.0),
            };
        }
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
        // 1. Apply port inputs (Amount from upstream slider/MapRange)
        if let Some(PortValue::Float(v)) = inputs.get(1) {
            self.amount = match self.mode {
                StyleMode::Blur => v.clamp(1.0, 20.0),
                StyleMode::Pixelate => v.clamp(2.0, 64.0),
                StyleMode::Sharpen => v.clamp(0.1, 5.0),
            };
        }

        // 2. Extract image dimensions from either CPU Image or GPU stub
        let stub_owned: Option<ImageData> = match inputs.first() {
            Some(PortValue::GpuImage(h)) => Some(ImageData {
                width: h.width, height: h.height, pixels: Vec::new(),
            }),
            _ => None,
        };
        let img_ref: Option<&ImageData> = match inputs.first() {
            Some(PortValue::Image(img)) => Some(img.as_ref()),
            Some(PortValue::GpuImage(_)) => stub_owned.as_ref(),
            _ => None,
        };

        let img = match img_ref {
            Some(img) => img,
            None => return vec![(0, PortValue::None)],
        };

        // 3. Try GPU path via process_gpu_cached
        if let Some(rs) = ctx.render_state {
            let gpu_src = ctx.input_sources.first().copied().flatten();
            if let Some(val) = self.process_gpu_cached(
                img, ctx.node_id, rs, ctx.gpu_tex_cache,
                gpu_src, ctx.needs_readback,
            ) {
                return vec![(0, val)];
            }
        }

        // 4. CPU fallback
        self.evaluate(inputs)
    }

    fn type_tag(&self) -> &str { "image_style" }
    fn save_state(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or_default() }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<ImageStyleNode>(state.clone()) { *self = l; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        // Image input
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Image);
            ui.label(egui::RichText::new("Image").small());
        });

        // Mode selector
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Mode").small());
            egui::ComboBox::from_id_salt(egui::Id::new(("style_mode", ctx.node_id)))
                .selected_text(match self.mode {
                    StyleMode::Blur => "Blur",
                    StyleMode::Pixelate => "Pixelate",
                    StyleMode::Sharpen => "Sharpen",
                })
                .width(80.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.mode, StyleMode::Blur, "Blur");
                    ui.selectable_value(&mut self.mode, StyleMode::Pixelate, "Pixelate");
                    ui.selectable_value(&mut self.mode, StyleMode::Sharpen, "Sharpen");
                });
        });

        // Amount slider with port
        let amt_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 1);
        let (range, step, label) = match self.mode {
            StyleMode::Blur => (1.0..=20.0, 1.0, "Radius"),
            StyleMode::Pixelate => (2.0..=64.0, 1.0, "Block"),
            StyleMode::Sharpen => (0.1..=5.0, 0.1, "Strength"),
        };
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 1, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(egui::RichText::new(label).small());
            if amt_wired {
                ui.label(egui::RichText::new(format!("{:.1}", self.amount)).small()
                    .color(egui::Color32::from_rgb(80, 170, 255)));
            } else {
                ui.add(egui::Slider::new(&mut self.amount, range).step_by(step as f64).show_value(true));
            }
        });

        // GPU indicator
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        ui.label(egui::RichText::new("⚡ GPU").small().color(dim));

        ui.separator();
        crate::nodes::audio_port_row(ui, "Image", ctx.node_id, 0, false, ctx.port_positions,
            ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Image);
    }
}

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("image_style", |state| {
        if let Ok(n) = serde_json::from_value::<ImageStyleNode>(state.clone()) { Box::new(n) }
        else { Box::new(ImageStyleNode::default()) }
    });
}

/// Drop all GPU resources for a deleted Image Style node so VRAM is
/// released immediately. Called from
/// `crate::gpu_image::GpuTextureCache::invalidate_node`.
pub(crate) fn cleanup_node(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
    node_id: crate::graph::NodeId,
) {
    if let Some(store) = callback_resources.get_mut::<ImageStyleStore>() {
        store.nodes.remove(&node_id);
    }
}

/// Wipe every Image Style GPU pipeline. See
/// `GpuTextureCache::clear_all`.
pub(crate) fn cleanup_all(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
) {
    if let Some(store) = callback_resources.get_mut::<ImageStyleStore>() {
        store.nodes.clear();
    }
}
