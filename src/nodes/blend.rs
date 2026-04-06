use crate::graph::*;
use crate::gpu_image::GpuBlendCallback;
use crate::nodes::{inline_port_circle, output_port_row};
use eframe::egui;
use eframe::egui_wgpu;
use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use std::sync::Arc;

const BLEND_MODES: &[&str] = &["Normal", "Multiply", "Screen", "Overlay", "Add", "Difference", "Soft Light", "Hard Light"];

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    wgpu_render_state: &Option<egui_wgpu::RenderState>,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    let (mode, mix) = match node_type {
        NodeType::Blend { mode, mix } => (mode, mix),
        _ => return,
    };

    let mix_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 2);

    if mix_wired {
        *mix = Graph::static_input_value(connections, values, node_id, 2).as_float();
    }

    let a = Graph::static_input_value(connections, values, node_id, 0);
    let b = Graph::static_input_value(connections, values, node_id, 1);
    let has_a = matches!(&a, PortValue::Image(_));
    let has_b = matches!(&b, PortValue::Image(_));

    // Port 0: Image A
    ui.horizontal(|ui| {
        inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Image);
        ui.label(egui::RichText::new("A:").small());
        match &a {
            PortValue::Image(img) => { ui.label(egui::RichText::new(format!("[{}x{}]", img.width, img.height)).small().color(egui::Color32::from_rgb(80, 170, 255))); }
            _ => { ui.label(egui::RichText::new("—").small().color(egui::Color32::GRAY)); }
        }
    });

    // Port 1: Image B
    ui.horizontal(|ui| {
        inline_port_circle(ui, node_id, 1, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Image);
        ui.label(egui::RichText::new("B:").small());
        match &b {
            PortValue::Image(img) => { ui.label(egui::RichText::new(format!("[{}x{}]", img.width, img.height)).small().color(egui::Color32::from_rgb(80, 170, 255))); }
            _ => { ui.label(egui::RichText::new("—").small().color(egui::Color32::GRAY)); }
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

            let target_format = wgpu_render_state.as_ref()
                .map(|rs| rs.target_format)
                .unwrap_or(eframe::egui_wgpu::wgpu::TextureFormat::Bgra8UnormSrgb);
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
    let out_val = if let Some(PortValue::Image(img)) = values.get(&(node_id, 0)) {
        format!("[{}x{}]", img.width, img.height)
    } else {
        "—".into()
    };
    output_port_row(ui, "Image", &out_val, node_id, 0, port_positions, dragging_from, connections, pending_disconnects, PortKind::Image);
}

/// Blend two images. Called during evaluation.
pub fn process(a: &ImageData, b: &ImageData, mode: u8, mix: f32) -> Arc<ImageData> {
    let w = a.width.min(b.width);
    let h = a.height.min(b.height);
    let mut pixels = vec![0u8; (w * h * 4) as usize];

    for y in 0..h {
        for x in 0..w {
            let ai = ((y * a.width + x) * 4) as usize;
            let bi = ((y * b.width + x) * 4) as usize;
            let oi = ((y * w + x) * 4) as usize;
            if ai + 3 >= a.pixels.len() || bi + 3 >= b.pixels.len() { continue; }

            for c in 0..3 {
                let va = a.pixels[ai + c] as f32 / 255.0;
                let vb = b.pixels[bi + c] as f32 / 255.0;
                let blended = match mode {
                    0 => va * (1.0 - mix) + vb * mix,
                    1 => va * vb,
                    2 => 1.0 - (1.0 - va) * (1.0 - vb),
                    3 => if va < 0.5 { 2.0 * va * vb } else { 1.0 - 2.0 * (1.0 - va) * (1.0 - vb) },
                    4 => (va + vb).min(1.0),
                    5 => (va - vb).abs(),
                    6 => if vb < 0.5 { va - (1.0 - 2.0 * vb) * va * (1.0 - va) } else { va + (2.0 * vb - 1.0) * (va.sqrt() - va) },
                    7 => if vb < 0.5 { 2.0 * va * vb } else { 1.0 - 2.0 * (1.0 - va) * (1.0 - vb) },
                    _ => vb,
                };
                let result = va * (1.0 - mix) + blended * mix;
                pixels[oi + c] = (result.clamp(0.0, 1.0) * 255.0) as u8;
            }
            pixels[oi + 3] = 255;
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

    let has_pipeline = {
        let renderer = render_state.renderer.read();
        renderer.callback_resources.get::<BlendGpuStore>()
            .and_then(|s| s.nodes.get(&node_id))
            .is_some()
    };

    if !has_pipeline {
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
        if error.is_some() { return None; }

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
    }

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

    let has_pipeline = {
        let renderer = render_state.renderer.read();
        renderer.callback_resources.get::<BlendGpuStore>()
            .and_then(|s| s.nodes.get(&node_id)).is_some()
    };
    if !has_pipeline {
        return process_gpu(a, b, mode, mix, node_id, render_state).map(PortValue::Image);
    }

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
