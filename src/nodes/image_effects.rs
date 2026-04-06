use crate::graph::*;
use eframe::egui;
use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use std::sync::Arc;

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    let (brightness, contrast, saturation, hue, exposure, gamma) = match node_type {
        NodeType::ImageEffects { brightness, contrast, saturation, hue, exposure, gamma } =>
            (brightness, contrast, saturation, hue, exposure, gamma),
        _ => return,
    };

    // Port 0: Image input
    {
        let is_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
        ui.horizontal(|ui| {
            super::inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Image);
            ui.label(egui::RichText::new("Image:").small());
            if is_wired {
                let v = Graph::static_input_value(connections, values, node_id, 0);
                match &v {
                    PortValue::Image(img) => {
                        ui.label(egui::RichText::new(format!("[{}x{}]", img.width, img.height))
                            .small().color(egui::Color32::from_rgb(80, 170, 255)));
                    }
                    _ => { ui.label(egui::RichText::new("—").small()); }
                }
            } else {
                ui.label(egui::RichText::new("—").small().color(egui::Color32::GRAY));
            }
        });
    }

    ui.separator();

    // Parameters: each is ● Label: on first line, slider + value on second
    struct Param<'a> { port: usize, label: &'a str, val: &'a mut f32, min: f32, max: f32, suffix: &'a str, kind: PortKind }
    let mut params = [
        Param { port: 1, label: "Brightness", val: brightness, min: 0.0, max: 3.0, suffix: "", kind: PortKind::Normalized },
        Param { port: 2, label: "Contrast", val: contrast, min: 0.0, max: 3.0, suffix: "", kind: PortKind::Normalized },
        Param { port: 3, label: "Saturation", val: saturation, min: 0.0, max: 3.0, suffix: "", kind: PortKind::Normalized },
        Param { port: 4, label: "Hue", val: hue, min: 0.0, max: 360.0, suffix: "°", kind: PortKind::Number },
        Param { port: 5, label: "Exposure", val: exposure, min: -3.0, max: 3.0, suffix: "", kind: PortKind::Number },
        Param { port: 6, label: "Gamma", val: gamma, min: 0.1, max: 3.0, suffix: "", kind: PortKind::Number },
    ];

    for param in &mut params {
        let is_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == param.port);

        // Override from input if connected
        if is_wired {
            let v = Graph::static_input_value(connections, values, node_id, param.port);
            *param.val = v.as_float();
        }

        // Row 1: ● Label:
        ui.horizontal(|ui| {
            super::inline_port_circle(ui, node_id, param.port, true, connections, port_positions, dragging_from, pending_disconnects, param.kind);

            ui.label(egui::RichText::new(format!("{}:", param.label)).small());

            if is_wired {
                ui.label(egui::RichText::new(format!("{:.2}{}", *param.val, param.suffix))
                    .small().monospace().color(egui::Color32::from_rgb(80, 170, 255)));
            }
        });

        // Row 2: slider + value (only if not wired — wired shows value inline above)
        if !is_wired {
            ui.horizontal(|ui| {
                ui.add_space(16.0); // indent to align with label
                let slider = egui::Slider::new(param.val, param.min..=param.max)
                    .show_value(true);
                let slider = if !param.suffix.is_empty() { slider.suffix(param.suffix) } else { slider };
                ui.add(slider);
            });
        }
    }

    // Output port for processed image
    ui.separator();
    {
        let v = values.get(&(node_id, 0));
        let val_str = match v {
            Some(PortValue::Image(img)) => format!("[{}x{}]", img.width, img.height),
            _ => "—".to_string(),
        };
        super::output_port_row(ui, "Image", &val_str, node_id, 0, port_positions, dragging_from, connections, pending_disconnects, PortKind::Image);
    }

    // Preview info
    let input_val = Graph::static_input_value(connections, values, node_id, 0);
    if let PortValue::Image(img) = &input_val {
        ui.label(egui::RichText::new(format!("{}x{}", img.width, img.height)).small().color(egui::Color32::GRAY));
    } else {
        ui.colored_label(egui::Color32::GRAY, "Connect image input");
    }
}

/// Process image with effects. Works on full resolution.
pub fn process(img: &ImageData, brightness: f32, contrast: f32, saturation: f32, hue: f32, exposure: f32, gamma: f32) -> Arc<ImageData> {
    let mut pixels = img.pixels.clone();
    let len = pixels.len();
    let hue_rad = hue * std::f32::consts::PI / 180.0;

    let mut i = 0;
    while i + 3 < len {
        let mut r = pixels[i] as f32 / 255.0;
        let mut g = pixels[i+1] as f32 / 255.0;
        let mut b = pixels[i+2] as f32 / 255.0;

        // Exposure (applied first, in linear space)
        if exposure.abs() > 0.001 {
            let mult = 2.0f32.powf(exposure);
            r *= mult; g *= mult; b *= mult;
        }

        // Brightness
        r *= brightness; g *= brightness; b *= brightness;

        // Contrast (around 0.5 midpoint)
        r = (r - 0.5) * contrast + 0.5;
        g = (g - 0.5) * contrast + 0.5;
        b = (b - 0.5) * contrast + 0.5;

        // Saturation
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        r = lum + (r - lum) * saturation;
        g = lum + (g - lum) * saturation;
        b = lum + (b - lum) * saturation;

        // Hue rotation
        if hue_rad.abs() > 0.001 {
            let cos_h = hue_rad.cos();
            let sin_h = hue_rad.sin();
            let nr = r * (0.213 + 0.787 * cos_h - 0.213 * sin_h)
                   + g * (0.715 - 0.715 * cos_h - 0.715 * sin_h)
                   + b * (0.072 - 0.072 * cos_h + 0.928 * sin_h);
            let ng = r * (0.213 - 0.213 * cos_h + 0.143 * sin_h)
                   + g * (0.715 + 0.285 * cos_h + 0.140 * sin_h)
                   + b * (0.072 - 0.072 * cos_h - 0.283 * sin_h);
            let nb = r * (0.213 - 0.213 * cos_h - 0.787 * sin_h)
                   + g * (0.715 - 0.715 * cos_h + 0.715 * sin_h)
                   + b * (0.072 + 0.928 * cos_h + 0.072 * sin_h);
            r = nr; g = ng; b = nb;
        }

        // Gamma
        if (gamma - 1.0).abs() > 0.001 {
            let inv_g = 1.0 / gamma;
            r = r.max(0.0).powf(inv_g);
            g = g.max(0.0).powf(inv_g);
            b = b.max(0.0).powf(inv_g);
        }

        pixels[i]   = (r.clamp(0.0, 1.0) * 255.0) as u8;
        pixels[i+1] = (g.clamp(0.0, 1.0) * 255.0) as u8;
        pixels[i+2] = (b.clamp(0.0, 1.0) * 255.0) as u8;
        i += 4;
    }

    Arc::new(ImageData { width: img.width, height: img.height, pixels })
}

// ── GPU-accelerated image effects ───────────────────────────────────────────

const EFFECTS_SHADER: &str = r#"
struct Params {
    brightness: f32,
    contrast: f32,
    saturation: f32,
    hue: f32,
    exposure: f32,
    gamma: f32,
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
    var c = textureSample(input_tex, input_sampler, uv);
    var r = c.r; var g = c.g; var b = c.b;

    // Exposure
    let exp_mult = pow(2.0, params.exposure);
    r *= exp_mult; g *= exp_mult; b *= exp_mult;

    // Brightness
    r *= params.brightness; g *= params.brightness; b *= params.brightness;

    // Contrast (around 0.5)
    r = (r - 0.5) * params.contrast + 0.5;
    g = (g - 0.5) * params.contrast + 0.5;
    b = (b - 0.5) * params.contrast + 0.5;

    // Saturation
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    r = lum + (r - lum) * params.saturation;
    g = lum + (g - lum) * params.saturation;
    b = lum + (b - lum) * params.saturation;

    // Hue rotation
    let hue_rad = params.hue * 3.14159265 / 180.0;
    let cos_h = cos(hue_rad);
    let sin_h = sin(hue_rad);
    let nr = r * (0.213 + 0.787 * cos_h - 0.213 * sin_h)
           + g * (0.715 - 0.715 * cos_h - 0.715 * sin_h)
           + b * (0.072 - 0.072 * cos_h + 0.928 * sin_h);
    let ng = r * (0.213 - 0.213 * cos_h + 0.143 * sin_h)
           + g * (0.715 + 0.285 * cos_h + 0.140 * sin_h)
           + b * (0.072 - 0.072 * cos_h - 0.283 * sin_h);
    let nb = r * (0.213 - 0.213 * cos_h - 0.787 * sin_h)
           + g * (0.715 - 0.715 * cos_h + 0.715 * sin_h)
           + b * (0.072 + 0.928 * cos_h + 0.072 * sin_h);

    // Gamma
    let inv_g = 1.0 / params.gamma;
    let fr = pow(clamp(nr, 0.0, 1.0), inv_g);
    let fg = pow(clamp(ng, 0.0, 1.0), inv_g);
    let fb = pow(clamp(nb, 0.0, 1.0), inv_g);

    return vec4f(clamp(fr, 0.0, 1.0), clamp(fg, 0.0, 1.0), clamp(fb, 0.0, 1.0), c.a);
}
"#;

struct ImageEffectsGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
}

struct ImageEffectsStore {
    nodes: HashMap<NodeId, ImageEffectsGpu>,
}

pub fn process_gpu(
    img: &ImageData,
    brightness: f32, contrast: f32, saturation: f32,
    hue: f32, exposure: f32, gamma: f32,
    node_id: NodeId,
    render_state: &eframe::egui_wgpu::RenderState,
) -> Option<Arc<ImageData>> {
    let device = &render_state.device;
    let queue = &render_state.queue;
    let w = img.width;
    let h = img.height;
    if w == 0 || h == 0 { return None; }

    // Ensure pipeline is cached
    let has_pipeline = {
        let renderer = render_state.renderer.read();
        renderer.callback_resources.get::<ImageEffectsStore>()
            .and_then(|s| s.nodes.get(&node_id))
            .is_some()
    };

    if !has_pipeline {
        device.push_error_scope(wgpu::ErrorFilter::Validation);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("image_effects_shader"),
            source: wgpu::ShaderSource::Wgsl(EFFECTS_SHADER.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image_effects_bgl"),
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
            label: Some("image_effects_pipeline"),
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
            crate::system_log::error("Image Effects GPU pipeline creation failed".to_string());
            return None;
        }

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image_effects_ub"),
            size: 32, // 8 floats × 4 bytes
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let gpu = ImageEffectsGpu { pipeline, bind_group_layout, uniform_buffer, sampler };
        let mut renderer = render_state.renderer.write();
        if let Some(store) = renderer.callback_resources.get_mut::<ImageEffectsStore>() {
            store.nodes.insert(node_id, gpu);
        } else {
            let mut nodes = HashMap::new();
            nodes.insert(node_id, gpu);
            renderer.callback_resources.insert(ImageEffectsStore { nodes });
        }
    }

    // Upload input image
    let input_tex = crate::gpu_image::upload_texture(device, queue, img, "img_fx_input");
    let input_view = input_tex.create_view(&Default::default());

    // Output texture
    let output_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("img_fx_output"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output_view = output_tex.create_view(&Default::default());

    // Render
    let renderer = render_state.renderer.read();
    let store = renderer.callback_resources.get::<ImageEffectsStore>()?;
    let gpu = store.nodes.get(&node_id)?;

    let params = [brightness, contrast, saturation, hue, exposure, gamma, w as f32, h as f32];
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
            label: Some("img_fx_pass"),
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
/// Same as process_gpu but caches output texture for downstream GPU nodes and Display.
pub fn process_gpu_cached(
    img: &ImageData,
    brightness: f32, contrast: f32, saturation: f32,
    hue: f32, exposure: f32, gamma: f32,
    node_id: NodeId,
    render_state: &eframe::egui_wgpu::RenderState,
    tex_cache: &mut crate::gpu_image::GpuTextureCache,
    gpu_source: Option<(NodeId, usize)>,
    needs_readback: bool,
) -> Option<PortValue> {
    let device = &render_state.device;
    let queue = &render_state.queue;
    let w = img.width;
    let h = img.height;
    if w == 0 || h == 0 { return None; }

    // Reuse pipeline from process_gpu (ensure it exists)
    let has_pipeline = {
        let renderer = render_state.renderer.read();
        renderer.callback_resources.get::<ImageEffectsStore>()
            .and_then(|s| s.nodes.get(&node_id)).is_some()
    };
    if !has_pipeline {
        return process_gpu(img, brightness, contrast, saturation, hue, exposure, gamma, node_id, render_state)
            .map(PortValue::Image);
    }

    // Bind input from upstream GPU cache when available; only upload from
    // CPU as a fallback (saves one CPU→GPU upload per frame in GPU chains).
    // Empty pixels mean the caller is sourcing from a `PortValue::GpuImage`
    // and has no CPU bytes — in that case we MUST cache-hit (else bail).
    let need_upload = !img.pixels.is_empty();
    let (input_view, _input_keepalive) = match gpu_source
        .and_then(|(nid, p)| tex_cache.get_node_output_cloned(nid, p))
        .filter(|(_, gw, gh)| *gw == w && *gh == h)
    {
        Some((view, _, _)) => (view, None),
        None if need_upload => {
            let tex = crate::gpu_image::upload_texture(device, queue, img, "img_fx_input");
            let v = tex.create_view(&Default::default());
            (v, Some(tex))
        }
        None => return None,
    };

    // Output with TEXTURE_BINDING so it can be cached for display
    let output_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("img_fx_output"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let output_view = output_tex.create_view(&Default::default());

    let renderer = render_state.renderer.read();
    let store = renderer.callback_resources.get::<ImageEffectsStore>()?;
    let gpu = store.nodes.get(&node_id)?;

    let params = [brightness, contrast, saturation, hue, exposure, gamma, w as f32, h as f32];
    queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::cast_slice(&params));

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None, layout: &gpu.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: gpu.uniform_buffer.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&input_view) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&gpu.sampler) },
        ],
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("img_fx_pass"),
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

    // Publish to the GPU cache (and to the display callback's prerendered
    // map). After this, downstream GPU consumers can sample our output
    // texture directly via `tex_cache.get_node_output_cloned(node_id, 0)`.
    if needs_readback {
        // CPU consumer downstream — readback BEFORE moving the texture
        // into the cache.
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

/// Drop all GPU resources for a deleted Image Effects node so VRAM is
/// released immediately. Called from
/// `crate::gpu_image::GpuTextureCache::invalidate_node`.
pub(crate) fn cleanup_node(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
    node_id: crate::graph::NodeId,
) {
    if let Some(store) = callback_resources.get_mut::<ImageEffectsStore>() {
        store.nodes.remove(&node_id);
    }
}
