//! ColorChannelNode — Split & adjust R/G/B channels with per-channel level controls.
//! Output 0: Combined image (levels applied). Outputs 1-3: individual R/G/B as grayscale.

use crate::graph::{ImageData, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui;
use eframe::egui_wgpu::wgpu;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorChannelNode {
    #[serde(default = "default_one")]
    pub r_level: f32,
    #[serde(default = "default_one")]
    pub g_level: f32,
    #[serde(default = "default_one")]
    pub b_level: f32,
}

fn default_one() -> f32 {
    1.0
}

impl Default for ColorChannelNode {
    fn default() -> Self {
        Self {
            r_level: 1.0,
            g_level: 1.0,
            b_level: 1.0,
        }
    }
}

impl ColorChannelNode {
    fn process(
        &self,
        img: &ImageData,
    ) -> (
        Arc<ImageData>,
        Arc<ImageData>,
        Arc<ImageData>,
        Arc<ImageData>,
    ) {
        let len = img.pixels.len();
        let mut combined = img.pixels.clone();
        let mut r_img = vec![0u8; len];
        let mut g_img = vec![0u8; len];
        let mut b_img = vec![0u8; len];

        for i in (0..len).step_by(4) {
            if i + 3 >= len {
                break;
            }

            let r = (img.pixels[i] as f32 * self.r_level).clamp(0.0, 255.0) as u8;
            let g = (img.pixels[i + 1] as f32 * self.g_level).clamp(0.0, 255.0) as u8;
            let b = (img.pixels[i + 2] as f32 * self.b_level).clamp(0.0, 255.0) as u8;
            let a = img.pixels[i + 3];

            // Combined output
            combined[i] = r;
            combined[i + 1] = g;
            combined[i + 2] = b;
            combined[i + 3] = a;

            // Individual channels as grayscale
            r_img[i] = r;
            r_img[i + 1] = r;
            r_img[i + 2] = r;
            r_img[i + 3] = a;
            g_img[i] = g;
            g_img[i + 1] = g;
            g_img[i + 2] = g;
            g_img[i + 3] = a;
            b_img[i] = b;
            b_img[i + 1] = b;
            b_img[i + 2] = b;
            b_img[i + 3] = a;
        }

        (
            Arc::new(ImageData {
                width: img.width,
                height: img.height,
                pixels: combined,
            }),
            Arc::new(ImageData {
                width: img.width,
                height: img.height,
                pixels: r_img,
            }),
            Arc::new(ImageData {
                width: img.width,
                height: img.height,
                pixels: g_img,
            }),
            Arc::new(ImageData {
                width: img.width,
                height: img.height,
                pixels: b_img,
            }),
        )
    }
}

impl NodeBehavior for ColorChannelNode {
    fn title(&self) -> &str {
        "Color Channel"
    }
    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Image", PortKind::Image),
            PortDef::new("R Level", PortKind::Number),
            PortDef::new("G Level", PortKind::Number),
            PortDef::new("B Level", PortKind::Number),
        ]
    }
    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Image", PortKind::Image)]
    }
    fn color_hint(&self) -> [u8; 3] {
        [200, 160, 120]
    }
    fn inline_ports(&self) -> bool {
        true
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        if let Some(PortValue::Float(v)) = inputs.get(1) {
            self.r_level = v.clamp(0.0, 2.0);
        }
        if let Some(PortValue::Float(v)) = inputs.get(2) {
            self.g_level = v.clamp(0.0, 2.0);
        }
        if let Some(PortValue::Float(v)) = inputs.get(3) {
            self.b_level = v.clamp(0.0, 2.0);
        }

        match inputs.first() {
            Some(PortValue::Image(img)) => {
                // If all levels are 1.0, pass through without processing
                let identity = (self.r_level - 1.0).abs() < 0.01
                    && (self.g_level - 1.0).abs() < 0.01
                    && (self.b_level - 1.0).abs() < 0.01;

                if identity {
                    // No processing needed — pass through original
                    vec![(0, PortValue::Image(img.clone()))]
                } else {
                    // Pre-bake u8→u8 LUTs to avoid float math in inner loop
                    let mut r_lut = [0u8; 256];
                    let mut g_lut = [0u8; 256];
                    let mut b_lut = [0u8; 256];
                    for i in 0..256 {
                        r_lut[i] = (i as f32 * self.r_level).clamp(0.0, 255.0) as u8;
                        g_lut[i] = (i as f32 * self.g_level).clamp(0.0, 255.0) as u8;
                        b_lut[i] = (i as f32 * self.b_level).clamp(0.0, 255.0) as u8;
                    }
                    let len = img.pixels.len();
                    let mut pixels = img.pixels.clone();
                    let mut i = 0;
                    while i + 3 < len {
                        pixels[i] = r_lut[pixels[i] as usize];
                        pixels[i + 1] = g_lut[pixels[i + 1] as usize];
                        pixels[i + 2] = b_lut[pixels[i + 2] as usize];
                        i += 4;
                    }
                    vec![(
                        0,
                        PortValue::Image(Arc::new(ImageData {
                            width: img.width,
                            height: img.height,
                            pixels,
                        })),
                    )]
                }
            }
            _ => vec![(0, PortValue::None)],
        }
    }

    fn type_tag(&self) -> &str {
        "color_channel"
    }
    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<ColorChannelNode>(state.clone()) {
            *self = l;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        // Image input
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui,
                ctx.node_id,
                0,
                true,
                ctx.connections,
                ctx.port_positions,
                ctx.dragging_from,
                ctx.pending_disconnects,
                PortKind::Image,
            );
            ui.label(egui::RichText::new("Image").small());
        });

        ui.separator();

        // R level
        let r_wired = ctx
            .connections
            .iter()
            .any(|c| c.to_node == ctx.node_id && c.to_port == 1);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui,
                ctx.node_id,
                1,
                true,
                ctx.connections,
                ctx.port_positions,
                ctx.dragging_from,
                ctx.pending_disconnects,
                PortKind::Number,
            );
            ui.label(
                egui::RichText::new("R")
                    .small()
                    .color(egui::Color32::from_rgb(255, 100, 100)),
            );
            if r_wired {
                ui.label(
                    egui::RichText::new(format!("{:.2}", self.r_level))
                        .small()
                        .color(egui::Color32::from_rgb(255, 100, 100)),
                );
            } else {
                ui.add(
                    egui::Slider::new(&mut self.r_level, 0.0..=2.0)
                        .step_by(0.01)
                        .show_value(true),
                );
            }
        });

        // G level
        let g_wired = ctx
            .connections
            .iter()
            .any(|c| c.to_node == ctx.node_id && c.to_port == 2);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui,
                ctx.node_id,
                2,
                true,
                ctx.connections,
                ctx.port_positions,
                ctx.dragging_from,
                ctx.pending_disconnects,
                PortKind::Number,
            );
            ui.label(
                egui::RichText::new("G")
                    .small()
                    .color(egui::Color32::from_rgb(100, 255, 100)),
            );
            if g_wired {
                ui.label(
                    egui::RichText::new(format!("{:.2}", self.g_level))
                        .small()
                        .color(egui::Color32::from_rgb(100, 255, 100)),
                );
            } else {
                ui.add(
                    egui::Slider::new(&mut self.g_level, 0.0..=2.0)
                        .step_by(0.01)
                        .show_value(true),
                );
            }
        });

        // B level
        let b_wired = ctx
            .connections
            .iter()
            .any(|c| c.to_node == ctx.node_id && c.to_port == 3);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui,
                ctx.node_id,
                3,
                true,
                ctx.connections,
                ctx.port_positions,
                ctx.dragging_from,
                ctx.pending_disconnects,
                PortKind::Number,
            );
            ui.label(
                egui::RichText::new("B")
                    .small()
                    .color(egui::Color32::from_rgb(100, 100, 255)),
            );
            if b_wired {
                ui.label(
                    egui::RichText::new(format!("{:.2}", self.b_level))
                        .small()
                        .color(egui::Color32::from_rgb(100, 100, 255)),
                );
            } else {
                ui.add(
                    egui::Slider::new(&mut self.b_level, 0.0..=2.0)
                        .step_by(0.01)
                        .show_value(true),
                );
            }
        });

        ui.separator();

        // Combined output
        crate::nodes::audio_port_row(
            ui,
            "Image",
            ctx.node_id,
            0,
            false,
            ctx.port_positions,
            ctx.dragging_from,
            ctx.connections,
            ctx.pending_disconnects,
            PortKind::Image,
        );
    }
}

// ── GPU-accelerated color channel split ─────────────────────────────────────

const CHANNEL_SHADER: &str = r#"
struct Params {
    r_level: f32,
    g_level: f32,
    b_level: f32,
    channel: f32,   // 0=combined, 1=R gray, 2=G gray, 3=B gray
    width: f32,
    height: f32,
    _p0: f32,
    _p1: f32,
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
    let c = textureSample(input_tex, input_sampler, uv);
    let r = clamp(c.r * params.r_level, 0.0, 1.0);
    let g = clamp(c.g * params.g_level, 0.0, 1.0);
    let b = clamp(c.b * params.b_level, 0.0, 1.0);
    let ch = u32(params.channel);
    if ch == 1u { return vec4f(r, r, r, c.a); }      // R as grayscale
    if ch == 2u { return vec4f(g, g, g, c.a); }      // G as grayscale
    if ch == 3u { return vec4f(b, b, b, c.a); }      // B as grayscale
    return vec4f(r, g, b, c.a);                        // Combined
}
"#;

struct ChannelGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
}

struct ChannelGpuStore {
    nodes: HashMap<crate::graph::NodeId, ChannelGpu>,
}

impl ColorChannelNode {
    pub fn process_gpu(
        &self,
        img: &ImageData,
        node_id: crate::graph::NodeId,
        render_state: &eframe::egui_wgpu::RenderState,
    ) -> Option<(
        Arc<ImageData>,
        Arc<ImageData>,
        Arc<ImageData>,
        Arc<ImageData>,
    )> {
        let device = &render_state.device;
        let queue = &render_state.queue;
        let w = img.width;
        let h = img.height;
        if w == 0 || h == 0 {
            return None;
        }

        // Ensure pipeline cached
        let has_pipeline = {
            let renderer = render_state.renderer.read();
            renderer
                .callback_resources
                .get::<ChannelGpuStore>()
                .and_then(|s| s.nodes.get(&node_id))
                .is_some()
        };

        if !has_pipeline {
            device.push_error_scope(wgpu::ErrorFilter::Validation);

            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("channel_shader"),
                source: wgpu::ShaderSource::Wgsl(CHANNEL_SHADER.into()),
            });

            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("channel_bgl"),
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
                label: Some("channel_pipeline"),
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
                return None;
            }

            let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("channel_ub"),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });

            let gpu = ChannelGpu {
                pipeline,
                bind_group_layout,
                uniform_buffer,
                sampler,
            };
            let mut renderer = render_state.renderer.write();
            if let Some(store) = renderer.callback_resources.get_mut::<ChannelGpuStore>() {
                store.nodes.insert(node_id, gpu);
            } else {
                let mut nodes = HashMap::new();
                nodes.insert(node_id, gpu);
                renderer
                    .callback_resources
                    .insert(ChannelGpuStore { nodes });
            }
        }

        // Upload input once, render 4 passes with different channel mode
        let input_tex = crate::gpu_image::upload_texture(device, queue, img, "channel_input");
        let input_view = input_tex.create_view(&Default::default());

        let mut results: Vec<Arc<ImageData>> = Vec::with_capacity(4);

        for channel in 0..4u32 {
            let output_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("channel_output"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let output_view = output_tex.create_view(&Default::default());

            let renderer = render_state.renderer.read();
            let store = renderer.callback_resources.get::<ChannelGpuStore>()?;
            let gpu = store.nodes.get(&node_id)?;

            let params = [
                self.r_level,
                self.g_level,
                self.b_level,
                channel as f32,
                w as f32,
                h as f32,
                0.0f32,
                0.0f32,
            ];
            queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::cast_slice(&params));

            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &gpu.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: gpu.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&input_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                    },
                ],
            });

            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("channel_pass"),
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

            results.push(crate::gpu_image::readback_texture(
                device,
                queue,
                &output_tex,
                w,
                h,
            ));
        }

        Some((
            results.remove(0),
            results.remove(0),
            results.remove(0),
            results.remove(0),
        ))
    }
}

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("color_channel", |state| {
        if let Ok(n) = serde_json::from_value::<ColorChannelNode>(state.clone()) {
            Box::new(n)
        } else {
            Box::new(ColorChannelNode::default())
        }
    });
}
