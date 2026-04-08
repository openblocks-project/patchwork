use crate::graph::*;
use crate::nodes::curve::evaluate_curve;
use eframe::egui;
use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use std::sync::Arc;

const CHANNELS: &[&str] = &["Master", "Red", "Green", "Blue"];
const CHANNEL_COLORS: &[[u8; 3]] = &[[200, 200, 200], [255, 80, 80], [80, 255, 80], [80, 120, 255]];

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
) {
    let (master, red, green, blue, active_channel) = match node_type {
        NodeType::ColorCurves { master, red, green, blue, active_channel } =>
            (master, red, green, blue, active_channel),
        _ => return,
    };

    // Channel selector
    ui.horizontal(|ui| {
        for (i, name) in CHANNELS.iter().enumerate() {
            let is_active = *active_channel == i as u8;
            let col = CHANNEL_COLORS[i];
            let color = if is_active {
                egui::Color32::from_rgb(col[0], col[1], col[2])
            } else {
                egui::Color32::from_rgb(col[0] / 2, col[1] / 2, col[2] / 2)
            };
            if ui.add(egui::Label::new(egui::RichText::new(*name).strong().color(color)).sense(egui::Sense::click())).clicked() {
                *active_channel = i as u8;
            }
        }
    });

    // Presets (applied to active channel)
    let ac = *active_channel;
    ui.horizontal(|ui| {
        if ui.small_button("Reset").clicked() {
            let pts = match ac { 0 => master as &mut Vec<_>, 1 => red, 2 => green, 3 => blue, _ => master };
            *pts = vec![[0.0, 0.0], [1.0, 1.0]];
        }
        if ui.small_button("Contrast").clicked() {
            let pts = match ac { 0 => master as &mut Vec<_>, 1 => red, 2 => green, 3 => blue, _ => master };
            *pts = vec![[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]];
        }
        if ui.small_button("Bright").clicked() {
            let pts = match ac { 0 => master as &mut Vec<_>, 1 => red, 2 => green, 3 => blue, _ => master };
            *pts = vec![[0.0, 0.1], [0.5, 0.65], [1.0, 1.0]];
        }
    });

    // Ensure all curves have at least 2 points
    for pts in [&mut *master, &mut *red, &mut *green, &mut *blue] {
        if pts.len() < 2 { *pts = vec![[0.0, 0.0], [1.0, 1.0]]; }
    }

    // Clone all curves for drawing (before mutable borrow of active channel)
    let all_curves: [(Vec<[f32; 2]>, [u8; 3], bool); 4] = [
        (master.clone(), CHANNEL_COLORS[0], ac == 0),
        (red.clone(), CHANNEL_COLORS[1], ac == 1),
        (green.clone(), CHANNEL_COLORS[2], ac == 2),
        (blue.clone(), CHANNEL_COLORS[3], ac == 3),
    ];

    // Get active curve for editing (mutable borrow AFTER cloning)
    let active_points = match ac {
        0 => master as &mut Vec<[f32; 2]>,
        1 => red,
        2 => green,
        3 => blue,
        _ => master,
    };

    // Curve editor
    let size = 180.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);

    // Background
    painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(20, 20, 30));
    painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 70)), egui::StrokeKind::Outside);

    // Diagonal reference line
    painter.line_segment(
        [egui::pos2(rect.left(), rect.bottom()), egui::pos2(rect.right(), rect.top())],
        egui::Stroke::new(0.5, egui::Color32::from_rgb(40, 40, 50)),
    );

    for (points, col, is_active) in &all_curves {
        let alpha = if *is_active { 255 } else { 60 };
        let width = if *is_active { 2.0 } else { 1.0 };
        let color = egui::Color32::from_rgba_unmultiplied(col[0], col[1], col[2], alpha);

        let steps = 50;
        let mut prev = None;
        for s in 0..=steps {
            let t = s as f32 / steps as f32;
            let y = evaluate_curve(points, t);
            let sx = rect.left() + t * size;
            let sy = rect.bottom() - y.clamp(0.0, 1.0) * size;
            let pt = egui::pos2(sx, sy);
            if let Some(p) = prev {
                painter.line_segment([p, pt], egui::Stroke::new(width, color));
            }
            prev = Some(pt);
        }
    }

    // Control points for active curve (draggable)
    let drag_id = egui::Id::new(("cc_drag", node_id));
    let active_drag: Option<usize> = ui.ctx().data_mut(|d| d.get_temp(drag_id));
    let ac_col = CHANNEL_COLORS[*active_channel as usize];

    for (i, pt) in active_points.iter().enumerate() {
        let sx = rect.left() + pt[0] * size;
        let sy = rect.bottom() - pt[1].clamp(0.0, 1.0) * size;
        let screen_pt = egui::pos2(sx, sy);
        let hit = response.hover_pos().map(|p| p.distance(screen_pt) < 10.0).unwrap_or(false);

        let color = if hit || active_drag == Some(i) {
            egui::Color32::WHITE
        } else {
            egui::Color32::from_rgb(ac_col[0], ac_col[1], ac_col[2])
        };
        painter.circle_filled(screen_pt, 4.0, color);

        if hit && response.drag_started() {
            ui.ctx().data_mut(|d| d.insert_temp(drag_id, i));
        }
    }

    if let Some(idx) = active_drag {
        if response.dragged() {
            if let Some(pos) = response.hover_pos() {
                let nx = ((pos.x - rect.left()) / size).clamp(0.0, 1.0);
                let ny = ((rect.bottom() - pos.y) / size).clamp(0.0, 1.0);
                if idx < active_points.len() {
                    if idx == 0 { active_points[idx] = [0.0, ny]; }
                    else if idx == active_points.len() - 1 { active_points[idx] = [1.0, ny]; }
                    else { active_points[idx] = [nx, ny]; }
                }
            }
        }
        if !response.dragged() {
            ui.ctx().data_mut(|d| d.remove::<usize>(drag_id));
        }
    }

    // Add/remove point
    ui.horizontal(|ui| {
        if ui.small_button("+").clicked() && active_points.len() < 10 {
            let mid = active_points.len() / 2;
            let x = (active_points[mid.saturating_sub(1)][0] + active_points[mid.min(active_points.len()-1)][0]) / 2.0;
            let y = evaluate_curve(active_points, x);
            active_points.insert(mid, [x, y]);
        }
        if ui.small_button("-").clicked() && active_points.len() > 2 {
            active_points.remove(active_points.len() / 2);
        }
    });

    // Input status
    let input_val = Graph::static_input_value(connections, values, node_id, 0);
    match &input_val {
        PortValue::Image(img) => {
            ui.label(egui::RichText::new(format!("Input: {}x{}", img.width, img.height)).small());
        }
        PortValue::GpuImage(h) => {
            ui.label(egui::RichText::new(format!("Input: {}x{} (gpu)", h.width, h.height)).small());
        }
        _ => { ui.colored_label(egui::Color32::GRAY, "Connect image input"); }
    }
}

/// Apply color curves to an image. Called during evaluation.
/// Uses pre-baked u8→u8 LUTs (channel curve + master in one table) for maximum throughput.
pub fn process(img: &ImageData, master: &[[f32; 2]], red: &[[f32; 2]], green: &[[f32; 2]], blue: &[[f32; 2]]) -> Arc<ImageData> {
    // Build combined u8→u8 LUTs: input byte → channel curve → master curve → output byte
    // This makes the inner loop just 3 array lookups with zero float math.
    let mut r_lut = [0u8; 256];
    let mut g_lut = [0u8; 256];
    let mut b_lut = [0u8; 256];
    for i in 0..256 {
        let t = i as f32 / 255.0;
        let rv = evaluate_curve(red, t).clamp(0.0, 1.0);
        let gv = evaluate_curve(green, t).clamp(0.0, 1.0);
        let bv = evaluate_curve(blue, t).clamp(0.0, 1.0);
        r_lut[i] = (evaluate_curve(master, rv).clamp(0.0, 1.0) * 255.0) as u8;
        g_lut[i] = (evaluate_curve(master, gv).clamp(0.0, 1.0) * 255.0) as u8;
        b_lut[i] = (evaluate_curve(master, bv).clamp(0.0, 1.0) * 255.0) as u8;
    }

    let mut pixels = img.pixels.clone();
    let len = pixels.len();
    let mut i = 0;
    while i + 3 < len {
        pixels[i]     = r_lut[pixels[i] as usize];
        pixels[i + 1] = g_lut[pixels[i + 1] as usize];
        pixels[i + 2] = b_lut[pixels[i + 2] as usize];
        i += 4;
    }
    Arc::new(ImageData::new(img.width, img.height, pixels))
}

// ── GPU-accelerated color curves ────────────────────────────────────────────

const CURVES_SHADER: &str = r#"
struct Params {
    width: f32,
    height: f32,
    _pad0: f32,
    _pad1: f32,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var input_tex: texture_2d<f32>;
@group(0) @binding(2) var lut_tex: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;

@vertex fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
    let pos = array(vec2f(-1,-1), vec2f(3,-1), vec2f(-1,3));
    return vec4f(pos[vi], 0, 1);
}

@fragment fn fs_main(@builtin(position) coord: vec4f) -> @location(0) vec4f {
    let uv = coord.xy / vec2f(params.width, params.height);
    let c = textureSample(input_tex, tex_sampler, uv);

    // LUT texture: 256 wide, 4 rows (0=master, 1=red, 2=green, 3=blue)
    // Sample channel LUT first, then master LUT
    let r_curved = textureSample(lut_tex, tex_sampler, vec2f(c.r, 0.375)).r;  // row 1 (red)
    let g_curved = textureSample(lut_tex, tex_sampler, vec2f(c.g, 0.625)).r;  // row 2 (green)
    let b_curved = textureSample(lut_tex, tex_sampler, vec2f(c.b, 0.875)).r;  // row 3 (blue)

    // Then apply master curve
    let r_final = textureSample(lut_tex, tex_sampler, vec2f(r_curved, 0.125)).r;  // row 0 (master)
    let g_final = textureSample(lut_tex, tex_sampler, vec2f(g_curved, 0.125)).r;
    let b_final = textureSample(lut_tex, tex_sampler, vec2f(b_curved, 0.125)).r;

    return vec4f(clamp(r_final, 0.0, 1.0), clamp(g_final, 0.0, 1.0), clamp(b_final, 0.0, 1.0), c.a);
}
"#;

struct CurvesGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
}

struct CurvesGpuStore {
    nodes: HashMap<NodeId, CurvesGpu>,
}

/// Build a 256×4 RGBA LUT texture from the four curve arrays.
/// Row 0 = master, row 1 = red, row 2 = green, row 3 = blue.
fn build_lut_texture(
    device: &wgpu::Device, queue: &wgpu::Queue,
    master: &[[f32; 2]], red: &[[f32; 2]], green: &[[f32; 2]], blue: &[[f32; 2]],
) -> wgpu::Texture {
    let w = 256u32;
    let h = 4u32;
    let mut pixels = vec![0u8; (w * h * 4) as usize];

    let curves: [&[[f32; 2]]; 4] = [master, red, green, blue];
    for (row, curve) in curves.iter().enumerate() {
        for x in 0..w {
            let t = x as f32 / 255.0;
            let v = evaluate_curve(curve, t);
            let byte = (v.clamp(0.0, 1.0) * 255.0) as u8;
            let idx = ((row as u32 * w + x) * 4) as usize;
            pixels[idx] = byte;
            pixels[idx + 1] = byte;
            pixels[idx + 2] = byte;
            pixels[idx + 3] = 255;
        }
    }

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("curves_lut"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        &pixels,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );

    texture
}

/// Create the per-node Color Curves pipeline if not already cached.
/// Same rationale as `blend::ensure_pipeline` — exists so
/// `process_gpu_cached` can ensure the pipeline without falling back to
/// the CPU-upload `process_gpu` path, which would corrupt stub
/// `GpuImage` inputs (empty `pixels`) into a solid black 1×1 texture.
fn ensure_pipeline(
    node_id: NodeId,
    render_state: &eframe::egui_wgpu::RenderState,
) -> bool {
    let device = &render_state.device;
    let already = {
        let renderer = render_state.renderer.read();
        renderer.callback_resources.get::<CurvesGpuStore>()
            .and_then(|s| s.nodes.get(&node_id))
            .is_some()
    };
    if already { return true; }

    device.push_error_scope(wgpu::ErrorFilter::Validation);

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("curves_shader"),
        source: wgpu::ShaderSource::Wgsl(CURVES_SHADER.into()),
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("curves_bgl"),
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
        label: Some("curves_pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_main"), buffers: &[], compilation_options: wgpu::PipelineCompilationOptions::default() },
        fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_main"), targets: &[Some(wgpu::TextureFormat::Rgba8UnormSrgb.into())], compilation_options: wgpu::PipelineCompilationOptions::default() }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None, multisample: wgpu::MultisampleState::default(), multiview: None, cache: None,
    });

    let error = pollster::block_on(device.pop_error_scope());
    if error.is_some() { return false; }

    let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("curves_ub"), size: 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default()
    });

    let gpu = CurvesGpu { pipeline, bind_group_layout, uniform_buffer, sampler };
    let mut renderer = render_state.renderer.write();
    if let Some(store) = renderer.callback_resources.get_mut::<CurvesGpuStore>() {
        store.nodes.insert(node_id, gpu);
    } else {
        let mut nodes = HashMap::new();
        nodes.insert(node_id, gpu);
        renderer.callback_resources.insert(CurvesGpuStore { nodes });
    }
    true
}

#[allow(dead_code)]
pub fn process_gpu(
    img: &ImageData,
    master: &[[f32; 2]], red: &[[f32; 2]], green: &[[f32; 2]], blue: &[[f32; 2]],
    node_id: NodeId,
    render_state: &eframe::egui_wgpu::RenderState,
) -> Option<Arc<ImageData>> {
    let device = &render_state.device;
    let queue = &render_state.queue;
    let w = img.width;
    let h = img.height;
    if w == 0 || h == 0 { return None; }

    if !ensure_pipeline(node_id, render_state) { return None; }

    // Upload input image + LUT
    let input_tex = crate::gpu_image::upload_texture(device, queue, img, "curves_input");
    let input_view = input_tex.create_view(&Default::default());
    let lut_tex = build_lut_texture(device, queue, master, red, green, blue);
    let lut_view = lut_tex.create_view(&Default::default());

    // Output texture
    let output_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("curves_output"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output_view = output_tex.create_view(&Default::default());

    let renderer = render_state.renderer.read();
    let store = renderer.callback_resources.get::<CurvesGpuStore>()?;
    let gpu = store.nodes.get(&node_id)?;

    let params = [w as f32, h as f32, 0.0f32, 0.0f32];
    queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::cast_slice(&params));

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None, layout: &gpu.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: gpu.uniform_buffer.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&input_view) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&lut_view) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&gpu.sampler) },
        ],
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("curves_pass"),
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

/// GPU processing with texture caching — no readback.
/// Returns a placeholder Arc<ImageData> (dimensions only, empty pixels).
/// The actual GPU texture is stored in the cache for downstream GPU consumers.
pub fn process_gpu_cached(
    img: &ImageData,
    master: &[[f32; 2]], red: &[[f32; 2]], green: &[[f32; 2]], blue: &[[f32; 2]],
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

    // Ensure pipeline exists. We must NOT fall back to `process_gpu`:
    // it CPU-uploads `img` via `upload_texture`, which silently produces
    // a black 1×1 texture if `img.pixels` is empty (which is the case
    // when the input arrived as a `GpuImage` stub from an upstream GPU
    // node). The black result then poisons the per-node `("cc_cache",
    // id)` param cache for the rest of the session.
    if !ensure_pipeline(node_id, render_state) { return None; }

    // Bind input from upstream GPU cache when available; only upload from CPU as fallback.
    let need_upload = !img.pixels.is_empty();
    let (input_view, _input_keepalive) = match gpu_source
        .and_then(|(nid, p)| tex_cache.get_node_output_cloned(nid, p))
        .filter(|(_, gw, gh)| *gw == w && *gh == h)
    {
        Some((view, _, _)) => (view, None),
        None if need_upload => {
            let tex = crate::gpu_image::upload_texture(device, queue, img, "curves_input");
            let v = tex.create_view(&Default::default());
            (v, Some(tex))
        }
        None => return None,
    };
    let lut_tex = build_lut_texture(device, queue, master, red, green, blue);
    let lut_view = lut_tex.create_view(&Default::default());

    // Output texture — TEXTURE_BINDING so it can be sampled by next node's display callback
    let output_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("curves_output_cached"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output_view = output_tex.create_view(&Default::default());

    let renderer = render_state.renderer.read();
    let store = renderer.callback_resources.get::<CurvesGpuStore>()?;
    let gpu = store.nodes.get(&node_id)?;

    let params = [w as f32, h as f32, 0.0f32, 0.0f32];
    queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::cast_slice(&params));

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None, layout: &gpu.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: gpu.uniform_buffer.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&input_view) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&lut_view) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&gpu.sampler) },
        ],
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("curves_pass_cached"),
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

/// Drop all GPU resources for a deleted Color Curves node so VRAM is
/// released immediately. Called from
/// `crate::gpu_image::GpuTextureCache::invalidate_node`.
pub(crate) fn cleanup_node(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
    node_id: crate::graph::NodeId,
) {
    if let Some(store) = callback_resources.get_mut::<CurvesGpuStore>() {
        store.nodes.remove(&node_id);
    }
}

/// Wipe every Color Curves GPU pipeline. See
/// `GpuTextureCache::clear_all`.
pub(crate) fn cleanup_all(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
) {
    if let Some(store) = callback_resources.get_mut::<CurvesGpuStore>() {
        store.nodes.clear();
    }
}
