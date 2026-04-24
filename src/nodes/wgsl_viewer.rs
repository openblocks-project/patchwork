use crate::graph::*;
use eframe::egui;
use eframe::egui_wgpu;
use eframe::egui_wgpu::wgpu;
use std::collections::{HashMap, HashSet};

// ── Constants ────────────────────────────────────────────────────────────────

const MAX_UNIFORM_FLOATS: usize = 64;
const UNIFORM_BUF_SIZE: u64 = (MAX_UNIFORM_FLOATS * 4) as u64; // 256 bytes

/// Request to auto-create a source node and connect it to a WGSL uniform port.
#[derive(Clone, Debug)]
pub struct WgslSpawnRequest {
    pub source_type: String,  // "time", "slider", "color"
    pub target_node: crate::graph::NodeId,
    pub target_port: usize,
    pub uniform_name: String,
}

/// Determine what source node to create and with what defaults based on uniform name.
fn uniform_spawn_request(name: &str, target_node: crate::graph::NodeId, target_port: usize) -> WgslSpawnRequest {
    let lower = name.to_lowercase();
    let source_type = if lower == "time" || lower == "t" { "time" } else { "slider" };
    WgslSpawnRequest {
        source_type: source_type.to_string(),
        target_node,
        target_port,
        uniform_name: name.to_string(),
    }
}

/// Get smart slider defaults (min, max, step, initial value) based on uniform name.
pub fn slider_defaults_for_uniform(name: &str) -> (f32, f32, f32, f32) {
    let lower = name.to_lowercase();
    if lower.contains("particle") {
        (0.0, 2000.0, 10.0, 400.0)  // particle swarms — 1000s range
    } else if lower.contains("count") || lower.contains("num") || lower == "n" || lower == "i" {
        (0.0, 20.0, 1.0, 5.0)       // integers
    } else if lower.contains("speed") || lower.contains("rate") {
        (0.0, 10.0, 0.1, 1.0)
    } else if lower.contains("fade") || lower.contains("decay") || lower.contains("persist") {
        (0.80, 1.0, 0.001, 0.97)
    } else if lower.contains("dot") {
        (0.0, 0.15, 0.001, 0.04)
    } else if lower.contains("ratio") {
        (0.5, 8.0, 0.01, 3.0)
    } else if lower.contains("segment") {
        (2.0, 32.0, 1.0, 6.0)
    } else if lower.contains("zoom_rate") || lower.contains("zoom_speed") {
        (-1.0, 1.0, 0.01, 0.1)              // signed, for zoom in/out animation
    } else if lower.contains("zoom") {
        (0.1, 4.0, 0.01, 1.0)
    } else if lower.contains("iter") {
        (32.0, 512.0, 1.0, 128.0)           // fractal iteration count
    } else if lower.contains("hue") {
        (0.0, 6.28318, 0.01, 0.5)           // palette rotation in radians
    } else if lower.contains("thick") || lower.contains("width") {
        (0.001, 0.05, 0.0005, 0.008)
    } else if lower.contains("size") || lower.contains("scale") || lower.contains("radius") {
        (0.0, 2.0, 0.01, 0.5)
    } else if lower.contains("freq") {
        (20.0, 2000.0, 1.0, 440.0)
    } else if lower.contains("angle") || lower.contains("rotation") || lower.contains("rot") {
        (0.0, 6.2832, 0.01, 0.0)    // 0–2π
    } else if lower.contains("mix") || lower.contains("blend") || lower.contains("amount") || lower.contains("alpha") {
        (0.0, 1.0, 0.01, 0.5)
    } else {
        (0.0, 1.0, 0.01, 0.5)       // safe default
    }
}

/// Pick a deterministic, visually distinct RGB default for a color
/// uniform named `name`. Stable across reloads of the same shader, but
/// two differently-named colors land at different hues so a preset with
/// `color_a` + `color_b` isn't flat grey.
///
/// Honours a handful of obvious name hints (`bg` → dark, `accent` →
/// saturated, `warm` → orange-ish, `cool` → blue-ish). Otherwise hashes
/// the name into a hue on the golden-ratio wheel, which gives good
/// separation without any two adjacent-name choices colliding.
pub fn default_color_rgb(name: &str) -> [f32; 3] {
    let lower = name.to_lowercase();
    // Name-hint shortcuts: a few intents worth special-casing.
    if lower.contains("bg") || lower.contains("background") {
        return [0.05, 0.06, 0.09];
    }
    if lower.contains("fg") || lower.contains("foreground") {
        return [0.94, 0.94, 0.96];
    }
    if lower.contains("warm") || lower.contains("fire") || lower.contains("sun") {
        return hsl_to_rgb(0.08, 0.80, 0.55); // orange
    }
    if lower.contains("cool") || lower.contains("ice") || lower.contains("ocean") {
        return hsl_to_rgb(0.55, 0.70, 0.50); // cyan-blue
    }
    // FNV-1a hash → [0,1) hue, stepped by golden-ratio conjugate for
    // good perceptual separation between adjacent name choices.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in lower.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let base = (h as f64 / u64::MAX as f64) as f32;
    let hue = (base + 0.618_033_99_f32).fract();
    hsl_to_rgb(hue, 0.65, 0.55)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h6 = h * 6.0;
    let x = c * (1.0 - (h6 % 2.0 - 1.0).abs());
    let (r, g, b) = match h6 as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c * 0.5;
    [r + m, g + m, b + m]
}

// time is NOT a builtin — it's a normal uniform port that users connect a Time node to.
// This lets them control speed, sync with audio, or use Timer phase.
const BUILTINS: &[&str] = &[
    "resolution", "mouse",
    "resolution_x", "resolution_y", "mouse_x", "mouse_y",
    "_p0", "_p1", "_p2",
];

/// Auto-injected image-input bindings for the WGSL Viewer feedback feature.
///
/// Every shader rendered by WGSL Viewer is prefixed with this block so that
/// authors can sample `image_a` / `image_b` (and any feedback ping-pong) using
/// a fixed contract. The bindings are always present in the pipeline layout
/// even when unused — naga and wgpu both allow declared-but-unused globals.
///
/// See plan: ~/.claude/plans/wgsl-feedback-feature.md
const IMAGE_INPUT_BLOCK: &str = r#"
@group(0) @binding(1) var img_sampler: sampler;
@group(0) @binding(2) var image_a: texture_2d<f32>;
@group(0) @binding(3) var image_b: texture_2d<f32>;
"#;

/// Bind group layout entries for the WGSL Viewer's 4-binding contract.
/// `@binding(0)` is the existing flat-f32 uniform buffer.
fn wgsl_viewer_bgl_entries() -> [wgpu::BindGroupLayoutEntry; 4] {
    [
        // @binding(0): uniform buffer (existing)
        wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
        // @binding(1): img_sampler
        wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        },
        // @binding(2): image_a
        wgpu::BindGroupLayoutEntry {
            binding: 2,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        },
        // @binding(3): image_b
        wgpu::BindGroupLayoutEntry {
            binding: 3,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        },
    ]
}

/// Create a 1×1 transparent-black texture used as the default bind for
/// `image_a` / `image_b` when a slot is `Unused` or unwired. wgpu requires
/// SOMETHING bound; this is the cheapest valid option.
fn create_dummy_black_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("wgsl_viewer_dummy_black_1x1"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[0u8, 0u8, 0u8, 255u8],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
    );
    let view = texture.create_view(&Default::default());
    (texture, view)
}

/// Create the default sampler used by WGSL Viewer for `img_sampler`.
/// Linear-clamp; matches the typical "stretch fill" expectation.
fn create_default_image_sampler(device: &wgpu::Device) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("wgsl_viewer_img_sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    })
}

/// Built-in vertex shader for fragment-only shaders (fullscreen triangle)
const BUILTIN_VS: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>(x * 0.5 + 0.5, 1.0 - (y * 0.5 + 0.5));
    return out;
}
"#;

// ── Image Input Sample Detection ─────────────────────────────────────────────

/// Returns `(uses_image_a, uses_image_b)` — whether the user shader text
/// references the auto-injected `image_a` / `image_b` texture bindings.
/// Used by the UI to surface a warning when a slot is `Unused` despite
/// being sampled. Conservative substring match — false positives on
/// `image_ax`-style identifiers are tolerated since the warning is
/// non-blocking.
fn detect_image_samples(code: &str) -> (bool, bool) {
    let uses_a = code.contains("image_a");
    let uses_b = code.contains("image_b");
    (uses_a, uses_b)
}

// ── Uniform Detection ────────────────────────────────────────────────────────

/// Scan shader code for `u.xxx` references, return (names, types).
/// Filters out built-ins. Detects _r/_g/_b color groups.
fn detect_uniforms(code: &str) -> (Vec<String>, Vec<String>) {
    let re = regex::Regex::new(r"u\.([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    let builtin_set: HashSet<&str> = BUILTINS.iter().copied().collect();

    // Collect all unique user uniform references in order
    let mut seen = HashSet::new();
    let mut all_refs: Vec<String> = Vec::new();
    for cap in re.captures_iter(code) {
        let name = cap[1].to_string();
        if !builtin_set.contains(name.as_str()) && seen.insert(name.clone()) {
            all_refs.push(name);
        }
    }

    // Detect color groups: if xxx_r, xxx_g, xxx_b all present
    let ref_set: HashSet<&str> = all_refs.iter().map(|s| s.as_str()).collect();
    let mut color_bases: HashSet<String> = HashSet::new();
    for name in &all_refs {
        if let Some(base) = name.strip_suffix("_r") {
            let g = format!("{}_g", base);
            let b = format!("{}_b", base);
            if ref_set.contains(g.as_str()) && ref_set.contains(b.as_str()) {
                color_bases.insert(base.to_string());
            }
        }
    }

    let mut names = Vec::new();
    let mut types = Vec::new();
    let mut skip_set: HashSet<String> = HashSet::new();

    // Add color groups first (from their _r position in order)
    for name in &all_refs {
        if let Some(base) = name.strip_suffix("_r") {
            if color_bases.contains(base) && !skip_set.contains(base) {
                names.push(base.to_string());
                types.push("color".to_string());
                skip_set.insert(base.to_string());
                skip_set.insert(format!("{}_r", base));
                skip_set.insert(format!("{}_g", base));
                skip_set.insert(format!("{}_b", base));
            }
        }
    }

    // Add remaining as floats
    for name in &all_refs {
        if !skip_set.contains(name) {
            names.push(name.clone());
            types.push("float".to_string());
        }
    }

    (names, types)
}

/// Build the WGSL uniform struct declaration from detected uniforms.
fn build_uniform_block(names: &[String], types: &[String]) -> String {
    let mut s = String::from("struct Uniforms {\n");
    s += "    time: f32,\n";
    s += "    resolution: vec2<f32>,\n";
    s += "    mouse: vec2<f32>,\n";
    s += "    _p0: f32,\n";
    // user uniforms (skip "time" — it's already in the header)
    for (i, name) in names.iter().enumerate() {
        if name == "time" { continue; }
        let t = types.get(i).map(|s| s.as_str()).unwrap_or("float");
        match t {
            "color" => {
                s += &format!("    {}_r: f32,\n", name);
                s += &format!("    {}_g: f32,\n", name);
                s += &format!("    {}_b: f32,\n", name);
            }
            _ => {
                s += &format!("    {}: f32,\n", name);
            }
        }
    }
    s += "};\n";
    s += "@group(0) @binding(0) var<uniform> u: Uniforms;\n\n";
    s
}

/// Pack uniform values into f32 array for GPU buffer.
fn pack_uniforms(
    time: f32,
    res_x: f32,
    res_y: f32,
    mouse_x: f32,
    mouse_y: f32,
    user_values: &[f32],
) -> Vec<f32> {
    let mut buf = vec![0.0f32; MAX_UNIFORM_FLOATS];
    buf[0] = time;
    buf[1] = res_x;
    buf[2] = res_y;
    // vec2 alignment: resolution takes indices 1-2, then mouse at 3-4
    // But we're using the struct layout: time(0), resolution(1,2), mouse(3,4), _p0(5)
    // Wait — with vec2 in struct, WGSL alignment puts resolution at offset 8 (2 floats).
    // Let's use a cleaner layout: all f32 individually.
    // Actually, with the struct as declared above:
    //   time: f32          -> offset 0  (4 bytes)
    //   resolution: vec2   -> offset 8  (aligned to 8 bytes = 2 floats)
    //   mouse: vec2        -> offset 16 (aligned to 8 bytes = 4 floats)
    //   _p0: f32           -> offset 24 (6 floats)
    //   user[0]            -> offset 28 (7 floats)
    //
    // So the Rust buffer must match this layout:
    buf[0] = time;
    buf[1] = 0.0; // padding for vec2 alignment
    buf[2] = res_x;
    buf[3] = res_y;
    buf[4] = mouse_x;
    buf[5] = mouse_y;
    buf[6] = 0.0; // _p0
    buf[7] = 0.0; // padding to align next field

    // User uniforms start at index 8
    // But wait — if user uniforms are all f32, they pack tightly after _p0.
    // With _p0 at offset 24, next f32 is at offset 28 = index 7.
    // Actually let me reconsider the struct alignment properly.
    //
    // WGSL struct layout rules:
    // - f32: align 4, size 4
    // - vec2<f32>: align 8, size 8
    // - vec3<f32>: align 16, size 12
    //
    // struct Uniforms {
    //   time: f32,           // offset 0, size 4
    //   // padding 4 bytes to align vec2
    //   resolution: vec2,    // offset 8, size 8
    //   mouse: vec2,         // offset 16, size 8
    //   _p0: f32,            // offset 24, size 4
    //   user0: f32,          // offset 28, size 4
    //   user1: f32,          // offset 32, size 4
    //   ...
    // }
    //
    // Total header in f32 indices: [0]=time, [1]=pad, [2..3]=resolution, [4..5]=mouse, [6]=_p0, [7]=user0...
    // That gives us user uniforms starting at index 7. Let's use that.

    let user_start = 7;
    for (i, v) in user_values.iter().enumerate() {
        if user_start + i < MAX_UNIFORM_FLOATS {
            buf[user_start + i] = *v;
        }
    }
    buf
}

// ── Callback Resources (shared via egui_wgpu) ───────────────────────────────

struct WgslNodeGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    vertex_count: u32,
    shader_hash: u64,
    // Phase 2: every WGSL Viewer pipeline now carries the image-input contract
    // bindings (sampler + image_a + image_b). The on-screen path normally binds
    // dummy black for image_a/image_b. When LastFrame feedback is enabled, the
    // paint callback rebuilds `bind_group` per frame to sample the offscreen
    // ping-pong texture so the inline canvas shows the trail too.
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    #[allow(dead_code)]
    dummy_black_texture: wgpu::Texture,
    dummy_black_view: wgpu::TextureView,
}

/// Per-node image-input hints, populated each frame by `render()` and consumed
/// by `WgslPaintCallback::prepare()` to rebuild on-screen bind groups so the
/// inline canvas can sample the right textures for both LastFrame feedback
/// and External (upstream wired) inputs. Without this, the on-screen pipeline
/// would only ever see dummy black for `image_a`/`image_b`.
#[derive(Default)]
struct WgslFeedbackHints {
    /// [image_a_lastframe, image_b_lastframe] — true ⇒ sample ping-pong.
    nodes: HashMap<NodeId, [bool; 2]>,
    /// Per-node external texture views resolved from `frame_snapshot_get_view`.
    /// Slot 0 = image_a, slot 1 = image_b. `None` ⇒ slot is not in External
    /// mode or upstream isn't published yet (fall back to dummy black).
    external_views: HashMap<NodeId, [Option<wgpu::TextureView>; 2]>,
}

/// Per-node GPU resources, keyed by NodeId
struct WgslGpuStore {
    nodes: HashMap<NodeId, WgslNodeGpu>,
}

/// Per-node uniform data for the current frame
struct WgslUniformStore {
    data: HashMap<NodeId, Vec<f32>>,
}

/// Per-node callback identity (carries node_id into paint)
struct WgslPaintCallback {
    node_id: NodeId,
}

impl egui_wgpu::CallbackTrait for WgslPaintCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // 1) Upload uniforms.
        if let (Some(unis), Some(gpus)) = (
            resources.get::<WgslUniformStore>(),
            resources.get::<WgslGpuStore>(),
        ) {
            if let (Some(data), Some(node_gpu)) = (
                unis.data.get(&self.node_id),
                gpus.nodes.get(&self.node_id),
            ) {
                let bytes: &[u8] = bytemuck::cast_slice(data);
                queue.write_buffer(&node_gpu.uniform_buffer, 0, bytes);
            }
        }

        // 2) Per-frame on-screen bind group rebuild for image inputs.
        //
        // The on-screen pipeline's "default" bind group binds dummy black for
        // image_a / image_b. Two situations need a different binding:
        //   * LastFrame ⇒ sample the offscreen ping-pong (the `read_idx` slot)
        //   * External  ⇒ sample the upstream wire's published texture, taken
        //                 from `WgslFeedbackHints::external_views`
        // We resolve both per slot, and rebuild the bind group only when at
        // least one slot is non-dummy. Without this, "Input A: External"
        // showed dummy black on the inline canvas even though the offscreen
        // path resolved it correctly — leaving e.g. a Camera Grade preset
        // showing only the vignette/tint with no camera image.
        let (feedback_flags, ext_views): ([bool; 2], [Option<wgpu::TextureView>; 2]) = {
            let hints = resources.get::<WgslFeedbackHints>();
            let fb = hints
                .and_then(|h| h.nodes.get(&self.node_id).copied())
                .unwrap_or([false, false]);
            let ext = hints
                .and_then(|h| h.external_views.get(&self.node_id).cloned())
                .unwrap_or([None, None]);
            (fb, ext)
        };

        let needs_rebuild = feedback_flags[0] || feedback_flags[1]
            || ext_views[0].is_some() || ext_views[1].is_some();

        if needs_rebuild {
            // Previous-frame ping-pong view (only needed for LastFrame slots).
            // After `render_offscreen` runs this frame, the just-written
            // texture sits at `prev_views[1 - write_index]`.
            let prev_view: Option<wgpu::TextureView> = resources
                .get::<WgslOffscreenStore>()
                .and_then(|s| s.nodes.get(&self.node_id))
                .and_then(|gpu| {
                    let read_idx = 1usize.wrapping_sub(gpu.write_index) & 1;
                    gpu.prev_views[read_idx].clone()
                });

            if let Some(gpu_store) = resources.get_mut::<WgslGpuStore>() {
                if let Some(node_gpu) = gpu_store.nodes.get_mut(&self.node_id) {
                    let pick = |slot: usize| -> wgpu::TextureView {
                        if feedback_flags[slot] {
                            prev_view.clone().unwrap_or_else(|| node_gpu.dummy_black_view.clone())
                        } else if let Some(v) = &ext_views[slot] {
                            v.clone()
                        } else {
                            node_gpu.dummy_black_view.clone()
                        }
                    };
                    let view_a = pick(0);
                    let view_b = pick(1);
                    let new_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("wgsl_onscreen_bg_per_frame"),
                        layout: &node_gpu.bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: node_gpu.uniform_buffer.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&node_gpu.sampler) },
                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&view_a) },
                            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&view_b) },
                        ],
                    });
                    node_gpu.bind_group = new_bg;
                }
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(store) = resources.get::<WgslGpuStore>() {
            if let Some(gpu) = store.nodes.get(&self.node_id) {
                render_pass.set_pipeline(&gpu.pipeline);
                render_pass.set_bind_group(0, &gpu.bind_group, &[]);
                render_pass.draw(0..gpu.vertex_count, 0..1);
            }
        }
    }
}

/// Public callback for rendering a WGSL shader as canvas background.
/// Reuses the same GPU pipeline/uniforms as the node's preview.
pub struct WgslBgCallback {
    pub node_id: NodeId,
}

impl egui_wgpu::CallbackTrait for WgslBgCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // Uniforms already uploaded by the node's own WgslPaintCallback::prepare.
        // But if the BG renders before the node, we upload here too.
        let uni_store = resources.get::<WgslUniformStore>();
        let gpu_store = resources.get::<WgslGpuStore>();
        if let (Some(unis), Some(gpus)) = (uni_store, gpu_store) {
            if let (Some(data), Some(node_gpu)) = (unis.data.get(&self.node_id), gpus.nodes.get(&self.node_id)) {
                let bytes: &[u8] = bytemuck::cast_slice(data);
                queue.write_buffer(&node_gpu.uniform_buffer, 0, bytes);
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(store) = resources.get::<WgslGpuStore>() {
            if let Some(gpu) = store.nodes.get(&self.node_id) {
                render_pass.set_pipeline(&gpu.pipeline);
                render_pass.set_bind_group(0, &gpu.bind_group, &[]);
                render_pass.draw(0..gpu.vertex_count, 0..1);
            }
        }
    }
}

// ── Main Render Function ────────────────────────────────────────────────────

pub fn render(
    ui: &mut egui::Ui,
    wgsl_code: &mut String,
    uniform_names: &mut Vec<String>,
    uniform_types: &mut Vec<String>,
    uniform_values: &mut Vec<f32>,
    canvas_w: &mut f32,
    canvas_h: &mut f32,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    wgpu_render_state: &Option<egui_wgpu::RenderState>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    // ── Phase 3: image-input feedback state ──
    image_a_mode: &mut crate::graph::ImageInputMode,
    image_b_mode: &mut crate::graph::ImageInputMode,
    feedback_reset_pending: &mut bool,
    // ── Phase 4: debounced auto-reset + reset-flash bookkeeping ──
    last_compile_ok: &mut bool,
    pending_shader_hash: &mut u64,
    pending_shader_since_ms: &mut u64,
    last_auto_reset_ms: &mut u64,
    image_a_hint_shown: &mut bool,
    image_b_hint_shown: &mut bool,
) {
    // ── Port 0: WGSL code input (inline) ────────────────────────────
    {
        let is_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
        ui.horizontal(|ui| {
            super::inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Text);
            ui.label(egui::RichText::new("WGSL").small());
            if is_wired {
                ui.label(egui::RichText::new("⟵ connected").small().color(egui::Color32::from_rgb(80, 170, 255)));
            } else {
                ui.colored_label(egui::Color32::GRAY, "—");
            }
        });
    }

    // Read shader code from input port 0. We *latch* the last non-empty
    // source we ever saw — disconnecting the text editor (or swapping it
    // for another) must NOT wipe the running shader. The viewer keeps
    // rendering whatever was last loaded until a new non-empty source
    // arrives or the user clears it manually.
    let input_code = Graph::static_input_value(connections, values, node_id, 0);
    if let PortValue::Text(s) = &input_code {
        if !s.trim().is_empty() {
            *wgsl_code = s.clone();
        }
    }
    let code = wgsl_code.clone();

    if code.is_empty() {
        ui.colored_label(egui::Color32::GRAY, "Connect WGSL code to render");
        // Offer a one-click bootstrap: spawn a Visual Presets picker to the
        // left, auto-wire it, with "gradient" selected so the viewer lights up
        // immediately. Mirrors the picker's own "+ WGSL Viewer" flow but in
        // the opposite direction.
        if ui.button("+ Load from Presets").clicked() {
            let req = crate::nodes::wgsl_presets::WgslViewerLoadPresetRequest {
                viewer_node: node_id,
            };
            ui.ctx().data_mut(|d| {
                d.insert_temp(egui::Id::new("wgsl_viewer_load_preset_request"), req);
            });
        }
        return;
    }

    // Auto-detect uniforms from shader code
    let (detected_names, detected_types) = detect_uniforms(&code);

    // Diff old vs new uniform layout to detect ports whose semantic changed.
    // Port layout: port 0 = WGSL code, ports 1+ = uniform components
    // (float uniforms = 1 port, color uniforms = 3 ports for r/g/b).
    // If a port's logical name changed (removed or shifted), disconnect it
    // so stale wires don't silently drive the wrong uniform.
    let build_port_labels = |names: &[String], types: &[String]| -> Vec<String> {
        let mut labels = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let t = types.get(i).map(|s| s.as_str()).unwrap_or("float");
            if t == "color" {
                labels.push(format!("{}.r", name));
                labels.push(format!("{}.g", name));
                labels.push(format!("{}.b", name));
            } else {
                labels.push(name.clone());
            }
        }
        labels
    };
    let old_labels = build_port_labels(uniform_names, uniform_types);
    let new_labels = build_port_labels(&detected_names, &detected_types);
    if old_labels != new_labels {
        let max_len = old_labels.len().max(new_labels.len());
        for i in 0..max_len {
            let old_l = old_labels.get(i);
            let new_l = new_labels.get(i);
            if old_l != new_l {
                // Uniform port indices start at 1 (port 0 = WGSL code).
                pending_disconnects.push((node_id, i + 1));
            }
        }
    }

    *uniform_names = detected_names;
    *uniform_types = detected_types;

    // Ensure uniform_values matches detected uniforms size. NEW entries
    // (introduced by a shader edit / preset switch) get smart defaults from
    // `slider_defaults_for_uniform` so the shader looks right immediately
    // — without this, every new uniform starts at 0.0 and presets like
    // Smear render as a frozen single dot until the user touches every slider.
    let total_floats: usize = uniform_names.iter().enumerate().map(|(i, _)| {
        let t = uniform_types.get(i).map(|s| s.as_str()).unwrap_or("float");
        if t == "color" { 3 } else { 1 }
    }).sum();
    if uniform_values.len() < total_floats {
        // Walk uniforms in declared order and append defaults for the
        // newly-introduced trailing entries.
        let mut idx = 0usize;
        for (i, name) in uniform_names.iter().enumerate() {
            let t = uniform_types.get(i).map(|s| s.as_str()).unwrap_or("float");
            let comps = if t == "color" { 3 } else { 1 };
            let rgb = if t == "color" { Some(default_color_rgb(name)) } else { None };
            for c in 0..comps {
                if idx >= uniform_values.len() {
                    let default = if let Some([r, g, b]) = rgb {
                        // Per-name deterministic hue so every color uniform
                        // starts visually distinct instead of mid-gray.
                        match c { 0 => r, 1 => g, 2 => b, _ => 0.5 }
                    } else {
                        slider_defaults_for_uniform(name).3
                    };
                    uniform_values.push(default);
                }
                idx += 1;
            }
        }
    }
    uniform_values.resize(total_floats, 0.0);

    // Read user uniform values: prefer connected port, else use inline uniform_values
    let mut user_values: Vec<f32> = Vec::new();
    let mut port_idx = 1usize;
    let mut value_idx = 0usize;
    for (i, _name) in uniform_names.iter().enumerate() {
        let t = uniform_types.get(i).map(|s| s.as_str()).unwrap_or("float");
        if t == "color" {
            for c in 0..3 {
                let port_connected = connections.iter().any(|cn| cn.to_node == node_id && cn.to_port == port_idx);
                if port_connected {
                    let v = Graph::static_input_value(connections, values, node_id, port_idx);
                    let fv = v.as_float() / 255.0;
                    user_values.push(fv);
                    if value_idx + c < uniform_values.len() { uniform_values[value_idx + c] = fv; }
                } else {
                    let fv = if value_idx + c < uniform_values.len() { uniform_values[value_idx + c] } else { 0.0 };
                    user_values.push(fv);
                }
                port_idx += 1;
            }
            value_idx += 3;
        } else {
            let port_connected = connections.iter().any(|cn| cn.to_node == node_id && cn.to_port == port_idx);
            if port_connected {
                let v = Graph::static_input_value(connections, values, node_id, port_idx);
                let fv = v.as_float();
                user_values.push(fv);
                if value_idx < uniform_values.len() { uniform_values[value_idx] = fv; }
            } else {
                let fv = if value_idx < uniform_values.len() { uniform_values[value_idx] } else { 0.0 };
                user_values.push(fv);
            }
            port_idx += 1;
            value_idx += 1;
        }
    }

    // Detect vertex shader
    let has_vs = code.contains("@vertex");
    let has_uniforms_decl = code.contains("var<uniform>");

    // Build final shader
    let uniform_block = if has_uniforms_decl {
        String::new()
    } else {
        build_uniform_block(uniform_names, uniform_types)
    };

    // Auto-inject the image-input bindings unless the user already declared
    // them. We look for the exact `image_a` token to avoid double-defining.
    let has_image_inputs = code.contains("image_a") && code.contains("@binding(2)");
    let image_block = if has_image_inputs { "" } else { IMAGE_INPUT_BLOCK };

    let final_code = if has_vs {
        format!("{}{}{}", uniform_block, image_block, code)
    } else {
        format!("{}{}{}\n{}", uniform_block, image_block, BUILTIN_VS, code)
    };

    let vertex_count = if has_vs && (code.contains(", 6>") || code.contains(",6>")) {
        6u32
    } else {
        3
    };

    // Canvas size controls
    ui.horizontal(|ui| {
        ui.label("Size:");
        ui.add(egui::DragValue::new(canvas_w).speed(1.0).range(100.0..=1920.0).prefix("W:"));
        ui.add(egui::DragValue::new(canvas_h).speed(1.0).range(100.0..=1080.0).prefix("H:"));
    });

    // Pop-out button
    let popout_id = egui::Id::new(("wgsl_popout", node_id));
    let mut popout_open = ui.ctx().data_mut(|d| d.get_temp::<bool>(popout_id).unwrap_or(false));
    if ui.button(if popout_open { "Close Window" } else { "Pop Out" }).clicked() {
        popout_open = !popout_open;
        ui.ctx().data_mut(|d| d.insert_temp(popout_id, popout_open));
    }

    // ── Uniform ports (inline — each with connector + DragValue) ────
    if !uniform_names.is_empty() {
        ui.separator();
        ui.label(egui::RichText::new("Uniforms").small().strong());
        let mut vi = 0usize;
        let mut pi = 1usize;
        let names_clone = uniform_names.clone();
        let types_clone = uniform_types.clone();
        let ch_labels = ["R", "G", "B"];

        for (i, name) in names_clone.iter().enumerate() {
            let t = types_clone.get(i).map(|s| s.as_str()).unwrap_or("float");
            if t == "color" {
                // Color group: label + optional "+" button
                let any_wired = (0..3).any(|c| connections.iter().any(|cn| cn.to_node == node_id && cn.to_port == pi + c));
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("u.{}", name)).small());
                    if !any_wired {
                        if ui.small_button("+").on_hover_text("Create Color node").clicked() {
                            let spawn = WgslSpawnRequest {
                                source_type: "color".to_string(),
                                target_node: node_id,
                                target_port: pi,
                                uniform_name: name.clone(),
                            };
                            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("wgsl_spawn_request"), spawn));
                        }
                    }
                });
                ui.horizontal(|ui| {
                    for c in 0..3 {
                        let port = pi;
                        let is_wired = connections.iter().any(|cn| cn.to_node == node_id && cn.to_port == port);
                        super::inline_port_circle(ui, node_id, port, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Color);
                        ui.label(egui::RichText::new(ch_labels[c]).small());
                        if is_wired {
                            let v = if vi + c < uniform_values.len() { uniform_values[vi + c] } else { 0.0 };
                            ui.label(egui::RichText::new(format!("{:.2}", v)).small().monospace());
                        } else if vi + c < uniform_values.len() {
                            ui.add(egui::DragValue::new(&mut uniform_values[vi + c]).speed(0.01).range(0.0..=1.0));
                        }
                        pi += 1;
                    }
                });
                vi += 3;
            } else {
                // Float: ● label [DragValue] [+]
                let port = pi;
                let is_wired = connections.iter().any(|cn| cn.to_node == node_id && cn.to_port == port);
                ui.horizontal(|ui| {
                    super::inline_port_circle(ui, node_id, port, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Number);
                    ui.label(egui::RichText::new(format!("u.{}", name)).small());
                    if is_wired {
                        let v = if vi < uniform_values.len() { uniform_values[vi] } else { 0.0 };
                        ui.label(egui::RichText::new(format!("{:.3}", v)).strong().monospace());
                        ui.label(egui::RichText::new("⟵").small().color(egui::Color32::from_rgb(80, 170, 255)));
                    } else {
                        if vi < uniform_values.len() {
                            ui.add(egui::DragValue::new(&mut uniform_values[vi]).speed(0.01));
                        }
                        // "+" button: auto-create the right source node
                        if ui.small_button("+").on_hover_text("Create source node").clicked() {
                            let spawn = uniform_spawn_request(name, node_id, port);
                            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("wgsl_spawn_request"), spawn));
                        }
                    }
                });
                pi += 1;
                vi += 1;
            }
        }
    }

    // ── Image input slots UI (Phase 4) ────────────────────────────────────
    // Compute Input A / Input B port indices (must match graph.rs port order).
    let uniform_port_count_ui: usize = uniform_types.iter()
        .map(|t| if t == "color" { 3 } else { 1 })
        .sum();
    let ui_input_a_port = 1 + uniform_port_count_ui;
    let ui_input_b_port = 2 + uniform_port_count_ui;

    let (uses_image_a, uses_image_b) = detect_image_samples(&code);

    let now_ms = (ui.ctx().input(|i| i.time) * 1000.0) as u64;
    let reset_flash_active = *last_auto_reset_ms != 0
        && now_ms.saturating_sub(*last_auto_reset_ms) < 300;

    ui.separator();
    ui.label(egui::RichText::new("Image inputs").small().strong());

    // Render one input row. Returns true if the dropdown was just changed
    // to LastFrame (so the caller can fire the first-use hint).
    fn input_row(
        ui: &mut egui::Ui,
        node_id: NodeId,
        port: usize,
        label: &str,
        mode: &mut crate::graph::ImageInputMode,
        connections: &[Connection],
        port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
        dragging_from: &mut Option<(NodeId, usize, bool)>,
        pending_disconnects: &mut Vec<(NodeId, usize)>,
    ) -> bool {
        let mut just_set_lastframe = false;
        ui.horizontal(|ui| {
            super::inline_port_circle(ui, node_id, port, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Image);
            // Loop glyph when LastFrame mode is active (refinement D, light version).
            let glyph = if *mode == crate::graph::ImageInputMode::LastFrame { "↻" } else { " " };
            ui.label(egui::RichText::new(glyph).small().color(egui::Color32::from_rgb(120, 200, 255)));
            ui.label(egui::RichText::new(label).small());

            let prev = *mode;
            egui::ComboBox::from_id_salt(("wgsl_image_mode", node_id, port))
                .selected_text(prev.label())
                .width(90.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(mode, crate::graph::ImageInputMode::Unused, "Unused");
                    ui.selectable_value(mode, crate::graph::ImageInputMode::External, "External");
                    ui.selectable_value(mode, crate::graph::ImageInputMode::LastFrame, "Last frame")
                        .on_hover_text(
                            "Sample this viewer's previous frame as feedback.\n\
                             WGSL Viewer is single-pass — for multi-buffer\n\
                             simulations (two-buffer fluid sims), chain two viewers."
                        );
                })
                .response
                .on_hover_text("How to bind this image input slot.");
            if prev != *mode && *mode == crate::graph::ImageInputMode::LastFrame {
                just_set_lastframe = true;
            }
        });
        just_set_lastframe
    }

    let just_set_a = input_row(ui, node_id, ui_input_a_port, "Input A",
        image_a_mode, connections, port_positions, dragging_from, pending_disconnects);
    let just_set_b = input_row(ui, node_id, ui_input_b_port, "Input B",
        image_b_mode, connections, port_positions, dragging_from, pending_disconnects);

    // ── Refinement J: warn when shader samples a slot that's Unused ──
    if uses_image_a && *image_a_mode == crate::graph::ImageInputMode::Unused {
        ui.colored_label(
            egui::Color32::from_rgb(220, 180, 60),
            "⚠ Shader samples image_a but Input A is Unused. Set to Last frame or wire an image source.",
        );
    }
    if uses_image_b && *image_b_mode == crate::graph::ImageInputMode::Unused {
        ui.colored_label(
            egui::Color32::from_rgb(220, 180, 60),
            "⚠ Shader samples image_b but Input B is Unused. Set to Last frame or wire an image source.",
        );
    }

    // ── Refinement M: first-use inline hint when newly switching to LastFrame ──
    if just_set_a { *image_a_hint_shown = false; }
    if just_set_b { *image_b_hint_shown = false; }
    let hint_for = |ui: &mut egui::Ui, slot: &str, shown: &mut bool| {
        if !*shown {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!(
                    "// In your shader: textureSample({}, img_sampler, in.uv)", slot
                )).small().monospace().color(egui::Color32::from_rgb(150, 200, 150)));
                if ui.small_button("copy").clicked() {
                    ui.ctx().copy_text(format!(
                        "textureSample({}, img_sampler, in.uv)", slot
                    ));
                }
                if ui.small_button("✕").on_hover_text("dismiss").clicked() {
                    *shown = true;
                }
            });
        }
    };
    if *image_a_mode == crate::graph::ImageInputMode::LastFrame {
        hint_for(ui, "image_a", image_a_hint_shown);
    }
    if *image_b_mode == crate::graph::ImageInputMode::LastFrame {
        hint_for(ui, "image_b", image_b_hint_shown);
    }

    // ── Reset feedback button (with auto-reset flash, refinement K) ──
    let any_feedback_active =
        *image_a_mode == crate::graph::ImageInputMode::LastFrame ||
        *image_b_mode == crate::graph::ImageInputMode::LastFrame;
    if any_feedback_active {
        let reset_btn = if reset_flash_active {
            egui::Button::new("Reset feedback ⟳")
                .fill(egui::Color32::from_rgb(80, 130, 200))
        } else {
            egui::Button::new("Reset feedback")
        };
        if ui.add(reset_btn).on_hover_text("Clear feedback ping-pong textures to black.").clicked() {
            *feedback_reset_pending = true;
        }
        if reset_flash_active {
            ui.ctx().request_repaint();
        }
    }

    // ── Refinement K: debounced auto-reset on shader edit ──
    // Compute a hash of the user shader text and compare against the
    // pending hash. When the hash changes we reset the "stable since"
    // timer; when it has been stable for >500ms AND the shader compiles
    // successfully (`last_compile_ok` from validate_wgsl_naga below)
    // we fire a reset.
    let current_hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        code.hash(&mut h);
        h.finish()
    };
    if current_hash != *pending_shader_hash {
        *pending_shader_hash = current_hash;
        *pending_shader_since_ms = now_ms;
    } else if *last_compile_ok
        && any_feedback_active
        && *pending_shader_since_ms != 0
        && now_ms.saturating_sub(*pending_shader_since_ms) >= 500
        && !*feedback_reset_pending
    {
        // Only fire once per stable shader: zero out the timer so we
        // don't keep resetting every frame after the debounce window.
        *feedback_reset_pending = true;
        *last_auto_reset_ms = now_ms;
        *pending_shader_since_ms = 0;
    }

    // Validate with naga
    let validation_result = validate_wgsl_naga(&final_code);
    *last_compile_ok = validation_result.is_ok();
    match validation_result {
        Err(e) => {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100), format!("Error: {}", e));
            let (rect, _) = ui.allocate_exact_size(egui::vec2(*canvas_w, *canvas_h), egui::Sense::hover());
            ui.painter().rect_filled(rect, 4.0, egui::Color32::from_rgb(40, 15, 15));
            // Log to system console (deduplicated — only when error text changes)
            let err_id = egui::Id::new(("wgsl_last_err", node_id));
            let prev: Option<String> = ui.ctx().data_mut(|d| d.get_temp(err_id));
            if prev.as_deref() != Some(&e) {
                ui.ctx().data_mut(|d| d.insert_temp(err_id, e.clone()));
            }
            // Raise a sticky node error so the canvas badge + Console reflect it.
            // `report_for` dedupes internally (only new messages hit the log).
            crate::node_errors::report_for(node_id, format!("WGSL compile: {}", e));
        }
        Ok(()) => {
            // Shader compiled cleanly — drop any stale error badge from a
            // previous frame (e.g. the user just fixed a typo).
            crate::node_errors::clear(node_id);
            if let Some(render_state) = wgpu_render_state {
                // Use connected Time node value if "time" uniform port is wired,
                // otherwise fall back to egui's internal clock.
                let time = {
                    let time_uniform_idx = uniform_names.iter().position(|n| n == "time");
                    if let Some(idx) = time_uniform_idx {
                        // Check if this uniform's port is connected (port 0 = WGSL code, so time port = 1 + idx)
                        let time_port = 1 + idx;
                        let connected = connections.iter().any(|c| c.to_node == node_id && c.to_port == time_port);
                        if connected {
                            user_values.get(idx).copied().unwrap_or(0.0)
                        } else {
                            ui.ctx().input(|i| i.time) as f32
                        }
                    } else {
                        ui.ctx().input(|i| i.time) as f32
                    }
                };
                // Build GPU values by walking uniforms in declared order, expanding
                // colors to 3 components and skipping "time" (which lives in the
                // header at buf[0], not in the user-uniform tail). The previous
                // zip-based filter silently dropped entries when colors and floats
                // were mixed, because user_values has 3 slots per color while
                // uniform_names has 1 slot per color — the indices misaligned and
                // every uniform after the first color came out as 0.
                let gpu_values: Vec<f32> = {
                    let mut out = Vec::with_capacity(user_values.len());
                    let mut vi = 0usize;
                    for (i, name) in uniform_names.iter().enumerate() {
                        let t = uniform_types.get(i).map(|s| s.as_str()).unwrap_or("float");
                        let comps = if t == "color" { 3 } else { 1 };
                        if name != "time" {
                            for c in 0..comps {
                                if vi + c < user_values.len() {
                                    out.push(user_values[vi + c]);
                                }
                            }
                        }
                        vi += comps;
                    }
                    out
                };
                let packed = pack_uniforms(
                    time, *canvas_w, *canvas_h,
                    0.0, 0.0,
                    &gpu_values,
                );

                render_with_wgpu(
                    ui, &final_code, render_state, vertex_count,
                    node_id, packed, *canvas_w, *canvas_h,
                );

                // Pop-out window rendering
                if popout_open {
                    let packed2 = pack_uniforms(time, *canvas_w, *canvas_h, 0.0, 0.0, &gpu_values);
                    render_popout_window(ui.ctx(), &final_code, render_state, vertex_count, node_id, packed2, canvas_w, canvas_h);
                }
            } else {
                ui.colored_label(egui::Color32::YELLOW, "No WGPU render state");
            }
        }
    }

    // Image output port (right-aligned)
    super::audio_port_row(ui, "Image", node_id, 0, false, port_positions, dragging_from, connections, pending_disconnects, PortKind::Image);

    // ── Compute the Input A / Input B port indices ──
    // Layout: [0=WGSL, 1..uniform_port_count=uniforms, then Input A, then Input B]
    let uniform_port_count: usize = uniform_types.iter()
        .map(|t| if t == "color" { 3 } else { 1 })
        .sum();
    let input_a_port = 1 + uniform_port_count;
    let input_b_port = 2 + uniform_port_count;

    // ── Auto-promote Unused → External when a wire arrives (refinement I) ──
    let input_a_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == input_a_port);
    let input_b_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == input_b_port);
    if input_a_wired && *image_a_mode == crate::graph::ImageInputMode::Unused {
        *image_a_mode = crate::graph::ImageInputMode::External;
    }
    if input_b_wired && *image_b_mode == crate::graph::ImageInputMode::Unused {
        *image_b_mode = crate::graph::ImageInputMode::External;
    }

    // GPU readback: render to offscreen texture and produce PortValue::Image
    // Run when the Image output is connected OR when LastFrame feedback is
    // active for either slot (so the inline canvas can sample the ping-pong).
    let output_connected = connections.iter().any(|c| c.from_node == node_id && c.from_port == 0);
    let feedback_active = *image_a_mode == crate::graph::ImageInputMode::LastFrame
        || *image_b_mode == crate::graph::ImageInputMode::LastFrame;

    // Publish per-frame image-input hints for the on-screen paint callback.
    // Includes both LastFrame flags and resolved External texture views, so
    // the callback can rebuild the on-screen bind group to sample whatever
    // the user wired (camera, image, last frame, etc).
    if let Some(render_state) = wgpu_render_state {
        // Resolve External wires to texture views OUTSIDE the renderer lock.
        let resolve_ext = |port_idx: usize, mode: crate::graph::ImageInputMode|
            -> Option<wgpu::TextureView>
        {
            if mode != crate::graph::ImageInputMode::External { return None; }
            let src = connections.iter()
                .find(|c| c.to_node == node_id && c.to_port == port_idx)
                .map(|c| (c.from_node, c.from_port))?;
            crate::gpu_image::frame_snapshot_get_view(src.0, src.1).map(|(v, _, _)| v)
        };
        let ext_a_view = resolve_ext(input_a_port, *image_a_mode);
        let ext_b_view = resolve_ext(input_b_port, *image_b_mode);

        let mut renderer = render_state.renderer.write();
        let store = match renderer.callback_resources.get_mut::<WgslFeedbackHints>() {
            Some(s) => s,
            None => {
                renderer.callback_resources.insert(WgslFeedbackHints::default());
                renderer.callback_resources.get_mut::<WgslFeedbackHints>().unwrap()
            }
        };
        store.nodes.insert(
            node_id,
            [
                *image_a_mode == crate::graph::ImageInputMode::LastFrame,
                *image_b_mode == crate::graph::ImageInputMode::LastFrame,
            ],
        );
        store.external_views.insert(node_id, [ext_a_view, ext_b_view]);
    }
    if (output_connected || feedback_active) && !final_code.is_empty() {
        if let Some(render_state) = wgpu_render_state {
            let readback_w = (*canvas_w as u32).max(1).min(800);
            let readback_h = (*canvas_h as u32).max(1).min(600);
            // Reuse same time logic as main render — connected port or internal clock
            let rb_time = {
                let time_idx = uniform_names.iter().position(|n| n == "time");
                if let Some(idx) = time_idx {
                    let time_port = 1 + idx;
                    let connected = connections.iter().any(|c| c.to_node == node_id && c.to_port == time_port);
                    if connected { user_values.get(idx).copied().unwrap_or(0.0) }
                    else { ui.ctx().input(|i| i.time) as f32 }
                } else { ui.ctx().input(|i| i.time) as f32 }
            };
            let rb_gpu_values: Vec<f32> = {
                let mut out = Vec::with_capacity(user_values.len());
                let mut vi = 0usize;
                for (i, name) in uniform_names.iter().enumerate() {
                    let t = uniform_types.get(i).map(|s| s.as_str()).unwrap_or("float");
                    let comps = if t == "color" { 3 } else { 1 };
                    if name != "time" {
                        for c in 0..comps {
                            if vi + c < user_values.len() {
                                out.push(user_values[vi + c]);
                            }
                        }
                    }
                    vi += comps;
                }
                out
            };
            let packed = pack_uniforms(rb_time, readback_w as f32, readback_h as f32, 0.0, 0.0, &rb_gpu_values);

            // ── Resolve image_a / image_b sources ──
            // External slots look up the upstream wire and snapshot the producer's
            // output via the per-frame thread-local snapshot. Last-frame slots use
            // the ping-pong texture (resolved inside render_offscreen). Unused
            // slots and unwired-External slots fall back to dummy black.
            let resolve_external = |port_idx: usize| -> Option<(NodeId, usize)> {
                connections.iter()
                    .find(|c| c.to_node == node_id && c.to_port == port_idx)
                    .map(|c| (c.from_node, c.from_port))
            };
            let image_a_source = match *image_a_mode {
                crate::graph::ImageInputMode::External => resolve_external(input_a_port),
                _ => None,
            };
            let image_b_source = match *image_b_mode {
                crate::graph::ImageInputMode::External => resolve_external(input_b_port),
                _ => None,
            };

            if let Some(img) = render_offscreen(
                render_state, &final_code, node_id, packed, readback_w, readback_h,
                *image_a_mode, *image_b_mode,
                image_a_source, image_b_source,
                feedback_reset_pending,
            ) {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(egui::Id::new(("wgsl_image_output", node_id)), img);
                });
            }
        }
    }

    // Show code preview (collapsible)
    ui.collapsing("Shader Code", |ui| {
        let preview = if final_code.chars().count() > 600 {
            let head: String = final_code.chars().take(600).collect();
            format!("{}...", head)
        } else {
            final_code.clone()
        };
        let mut p = preview;
        ui.code_editor(&mut p);
    });
}

/// Run the offscreen GPU render pass for a WGSL node that is currently collapsed.
///
/// When a WGSL Viewer window is minimized, egui's `win.show()` closure does not
/// execute, so `render()` is never called and the offscreen texture is never
/// updated.  That breaks multi-pass chains: downstream viewers see stale / blank
/// image data.
///
/// This function re-derives everything that the offscreen block inside `render()`
/// needs and calls `render_offscreen()` directly, without touching any UI.  It is
/// called from `app/mod.rs` immediately after the collapsed-port-position fallback
/// whenever the node is a `WgslViewer`.
#[allow(clippy::too_many_arguments)]
pub fn render_offscreen_pass(
    ctx: &egui::Context,
    node_id: NodeId,
    wgsl_code: &str,
    uniform_names: &[String],
    uniform_types: &[String],
    uniform_values: &[f32],
    canvas_w: f32,
    canvas_h: f32,
    image_a_mode: crate::graph::ImageInputMode,
    image_b_mode: crate::graph::ImageInputMode,
    feedback_reset_pending: &mut bool,
    connections: &[Connection],
    values: &HashMap<(NodeId, usize), PortValue>,
    wgpu_render_state: &Option<egui_wgpu::RenderState>,
) {
    if wgsl_code.is_empty() { return; }

    // ── Rebuild final_code (same logic as in render()) ──
    let has_vs = wgsl_code.contains("@vertex");
    let has_uniforms_decl = wgsl_code.contains("var<uniform>");
    let uniform_block = if has_uniforms_decl {
        String::new()
    } else {
        build_uniform_block(uniform_names, uniform_types)
    };
    let has_image_inputs = wgsl_code.contains("image_a") && wgsl_code.contains("@binding(2)");
    let image_block = if has_image_inputs { "" } else { IMAGE_INPUT_BLOCK };
    let final_code = if has_vs {
        format!("{}{}{}", uniform_block, image_block, wgsl_code)
    } else {
        format!("{}{}{}\n{}", uniform_block, image_block, BUILTIN_VS, wgsl_code)
    };
    if final_code.is_empty() { return; }

    // ── Port indices ──
    let uniform_port_count: usize = uniform_types.iter()
        .map(|t| if t == "color" { 3 } else { 1 })
        .sum();
    let input_a_port = 1 + uniform_port_count;
    let input_b_port = 2 + uniform_port_count;

    // ── Check whether the offscreen pass is actually needed ──
    let output_connected = connections.iter().any(|c| c.from_node == node_id && c.from_port == 0);
    let feedback_active = image_a_mode == crate::graph::ImageInputMode::LastFrame
        || image_b_mode == crate::graph::ImageInputMode::LastFrame;
    if !output_connected && !feedback_active { return; }

    // ── Update WgslFeedbackHints store (external texture views) ──
    if let Some(render_state) = wgpu_render_state {
        let resolve_ext = |port_idx: usize, mode: crate::graph::ImageInputMode|
            -> Option<wgpu::TextureView>
        {
            if mode != crate::graph::ImageInputMode::External { return None; }
            let src = connections.iter()
                .find(|c| c.to_node == node_id && c.to_port == port_idx)
                .map(|c| (c.from_node, c.from_port))?;
            crate::gpu_image::frame_snapshot_get_view(src.0, src.1).map(|(v, _, _)| v)
        };
        let ext_a_view = resolve_ext(input_a_port, image_a_mode);
        let ext_b_view = resolve_ext(input_b_port, image_b_mode);

        let mut renderer = render_state.renderer.write();
        let store = match renderer.callback_resources.get_mut::<WgslFeedbackHints>() {
            Some(s) => s,
            None => {
                renderer.callback_resources.insert(WgslFeedbackHints::default());
                renderer.callback_resources.get_mut::<WgslFeedbackHints>().unwrap()
            }
        };
        store.nodes.insert(node_id, [
            image_a_mode == crate::graph::ImageInputMode::LastFrame,
            image_b_mode == crate::graph::ImageInputMode::LastFrame,
        ]);
        store.external_views.insert(node_id, [ext_a_view, ext_b_view]);
    } else {
        return;
    }

    let render_state = wgpu_render_state.as_ref().unwrap();
    let readback_w = (canvas_w as u32).max(1).min(800);
    let readback_h = (canvas_h as u32).max(1).min(600);

    // ── Resolve user_values from connections / stored values ──
    let mut user_values: Vec<f32> = Vec::new();
    {
        let mut port_idx = 1usize;
        let mut value_idx = 0usize;
        for (i, _name) in uniform_names.iter().enumerate() {
            let t = uniform_types.get(i).map(|s| s.as_str()).unwrap_or("float");
            if t == "color" {
                for c in 0..3 {
                    let port_connected = connections.iter().any(|cn| cn.to_node == node_id && cn.to_port == port_idx);
                    let fv = if port_connected {
                        let v = Graph::static_input_value(connections, values, node_id, port_idx);
                        v.as_float() / 255.0
                    } else {
                        if value_idx + c < uniform_values.len() { uniform_values[value_idx + c] } else { 0.0 }
                    };
                    user_values.push(fv);
                    port_idx += 1;
                }
                value_idx += 3;
            } else {
                let port_connected = connections.iter().any(|cn| cn.to_node == node_id && cn.to_port == port_idx);
                let fv = if port_connected {
                    let v = Graph::static_input_value(connections, values, node_id, port_idx);
                    v.as_float()
                } else {
                    if value_idx < uniform_values.len() { uniform_values[value_idx] } else { 0.0 }
                };
                user_values.push(fv);
                port_idx += 1;
                value_idx += 1;
            }
        }
    }

    // ── Time uniform ──
    let rb_time = {
        let time_idx = uniform_names.iter().position(|n| n == "time");
        if let Some(idx) = time_idx {
            let time_port = 1 + idx;
            let connected = connections.iter().any(|c| c.to_node == node_id && c.to_port == time_port);
            if connected { user_values.get(idx).copied().unwrap_or(0.0) }
            else { ctx.input(|i| i.time) as f32 }
        } else { ctx.input(|i| i.time) as f32 }
    };

    // ── GPU values (skip time, expand colors) ──
    let rb_gpu_values: Vec<f32> = {
        let mut out = Vec::with_capacity(user_values.len());
        let mut vi = 0usize;
        for (i, name) in uniform_names.iter().enumerate() {
            let t = uniform_types.get(i).map(|s| s.as_str()).unwrap_or("float");
            let comps = if t == "color" { 3 } else { 1 };
            if name != "time" {
                for c in 0..comps {
                    if vi + c < user_values.len() { out.push(user_values[vi + c]); }
                }
            }
            vi += comps;
        }
        out
    };
    let packed = pack_uniforms(rb_time, readback_w as f32, readback_h as f32, 0.0, 0.0, &rb_gpu_values);

    // ── Resolve image sources ──
    let resolve_external = |port_idx: usize| -> Option<(NodeId, usize)> {
        connections.iter()
            .find(|c| c.to_node == node_id && c.to_port == port_idx)
            .map(|c| (c.from_node, c.from_port))
    };
    let image_a_source = match image_a_mode {
        crate::graph::ImageInputMode::External => resolve_external(input_a_port),
        _ => None,
    };
    let image_b_source = match image_b_mode {
        crate::graph::ImageInputMode::External => resolve_external(input_b_port),
        _ => None,
    };

    if let Some(img) = render_offscreen(
        render_state, &final_code, node_id, packed, readback_w, readback_h,
        image_a_mode, image_b_mode,
        image_a_source, image_b_source,
        feedback_reset_pending,
    ) {
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new(("wgsl_image_output", node_id)), img);
        });
    }

    // Keep animation alive while collapsed
    ctx.request_repaint();
}

/// Render the shader to an offscreen RGBA8 texture and read back to CPU.
/// Returns None if the shader fails to compile or render.
///
/// Phase 3: this path now owns ping-pong textures per node and resolves
/// `Input A` / `Input B` slots from the dropdown mode + upstream wires.
fn render_offscreen(
    render_state: &egui_wgpu::RenderState,
    shader_code: &str,
    node_id: NodeId,
    packed_uniforms: Vec<f32>,
    width: u32,
    height: u32,
    image_a_mode: crate::graph::ImageInputMode,
    image_b_mode: crate::graph::ImageInputMode,
    image_a_source: Option<(NodeId, usize)>,
    image_b_source: Option<(NodeId, usize)>,
    feedback_reset_pending: &mut bool,
) -> Option<std::sync::Arc<ImageData>> {
    let device = &render_state.device;
    let queue = &render_state.queue;
    let readback_format = wgpu::TextureFormat::Rgba8UnormSrgb;

    // Cache key for the offscreen pipeline (separate from screen pipeline)
    let shader_hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        shader_code.hash(&mut h);
        "offscreen".hash(&mut h);
        h.finish()
    };

    // Check cache for existing offscreen pipeline
    let has_pipeline = {
        let renderer = render_state.renderer.read();
        renderer.callback_resources.get::<WgslOffscreenStore>()
            .and_then(|s| s.nodes.get(&node_id))
            .map(|g| g.shader_hash == shader_hash)
            .unwrap_or(false)
    };

    if !has_pipeline {
        // Create offscreen pipeline with Rgba8UnormSrgb target
        device.push_error_scope(wgpu::ErrorFilter::Validation);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgsl_offscreen_shader"),
            source: wgpu::ShaderSource::Wgsl(shader_code.into()),
        });

        let bgl_entries = wgsl_viewer_bgl_entries();
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgsl_offscreen_bgl"),
            entries: &bgl_entries,
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgsl_offscreen_pipeline"),
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
                targets: &[Some(readback_format.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let error = pollster::block_on(device.pop_error_scope());
        if error.is_some() { return None; }

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgsl_offscreen_ub"),
            size: UNIFORM_BUF_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Phase 2: build the image-input infrastructure for this node. The
        // initial bind group binds dummy black for both image_a and image_b;
        // Phase 3 will rebuild the bind group per frame as the dropdown
        // sources resolve to ping-pong textures or external images.
        let sampler = create_default_image_sampler(device);
        let (dummy_black_texture, dummy_black_view) = create_dummy_black_texture(device, queue);

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgsl_offscreen_bg_initial"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&dummy_black_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&dummy_black_view) },
            ],
        });

        let node_gpu = WgslOffscreenGpu {
            pipeline,
            bind_group,
            uniform_buffer,
            shader_hash,
            bind_group_layout,
            sampler,
            dummy_black_texture,
            dummy_black_view,
            prev_textures: [None, None],
            prev_views: [None, None],
            prev_size: (0, 0),
            write_index: 0,
        };

        let mut renderer = render_state.renderer.write();
        if let Some(store) = renderer.callback_resources.get_mut::<WgslOffscreenStore>() {
            store.nodes.insert(node_id, node_gpu);
        } else {
            let mut nodes = HashMap::new();
            nodes.insert(node_id, node_gpu);
            renderer.callback_resources.insert(WgslOffscreenStore { nodes });
        }
    }

    // ── Phase 3: ping-pong texture management + per-frame bind group ──
    //
    // The offscreen render now targets one of two persistent ping-pong
    // textures owned by the per-node WgslOffscreenGpu. The OTHER texture
    // (the previous frame's output) is what `LastFrame` mode samples for
    // self-feedback. After rendering we swap `write_index` so next frame
    // writes to the now-stale slot and reads from the just-written one.

    // Resolve external image inputs from the per-frame snapshot. This is
    // done OUTSIDE the renderer write-lock to keep the critical section short.
    let ext_a_view: Option<wgpu::TextureView> = image_a_source
        .and_then(|(nid, p)| crate::gpu_image::frame_snapshot_get_view(nid, p))
        .map(|(v, _, _)| v);
    let ext_b_view: Option<wgpu::TextureView> = image_b_source
        .and_then(|(nid, p)| crate::gpu_image::frame_snapshot_get_view(nid, p))
        .map(|(v, _, _)| v);

    // ── Acquire per-frame ping-pong target ──
    // We need a write lock on the renderer to (re)allocate the ping-pong
    // textures and rebuild the per-frame bind group, then keep it long
    // enough to encode the render pass.
    let (write_texture, write_view, read_view, swap_after) = {
        let mut renderer = render_state.renderer.write();
        let store = renderer.callback_resources.get_mut::<WgslOffscreenStore>()?;
        let gpu = store.nodes.get_mut(&node_id)?;

        // (Re)allocate ping-pong textures if size changed, missing, or a
        // feedback reset was requested. Allocating drops the previous
        // pair, which is the cheapest way to "clear to black" — first
        // sample of any newly created texture is undefined though, so we
        // explicitly clear-load on first use via LoadOp::Clear below.
        let need_alloc = gpu.prev_textures[0].is_none()
            || gpu.prev_textures[1].is_none()
            || gpu.prev_size != (width, height)
            || *feedback_reset_pending;

        if need_alloc {
            for i in 0..2 {
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("wgsl_offscreen_pingpong"),
                    size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: readback_format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                let v = tex.create_view(&Default::default());
                gpu.prev_textures[i] = Some(tex);
                gpu.prev_views[i] = Some(v);
            }
            gpu.prev_size = (width, height);
            gpu.write_index = 0;
            *feedback_reset_pending = false;
        }

        let write_index = gpu.write_index;
        let read_index = 1 - write_index;

        // Clone the views/textures we'll need outside the lock for the
        // render pass + publish + readback.
        let write_tex = gpu.prev_textures[write_index].as_ref()?.clone();
        let write_view_local = gpu.prev_views[write_index].as_ref()?.clone();
        let prev_view_local = gpu.prev_views[read_index].as_ref()?.clone();

        // ── Build the per-frame bind group ──
        // image_a / image_b are resolved per the dropdown mode:
        //   * Unused        → dummy black
        //   * External      → upstream wire snapshot, falling back to dummy
        //                     black if the source isn't published yet
        //   * LastFrame     → the previous-frame ping-pong texture (read_index)
        let view_for = |mode: crate::graph::ImageInputMode,
                        ext: &Option<wgpu::TextureView>|
         -> wgpu::TextureView {
            match mode {
                crate::graph::ImageInputMode::Unused => gpu.dummy_black_view.clone(),
                crate::graph::ImageInputMode::External => {
                    ext.clone().unwrap_or_else(|| gpu.dummy_black_view.clone())
                }
                crate::graph::ImageInputMode::LastFrame => prev_view_local.clone(),
            }
        };
        let bind_a_view = view_for(image_a_mode, &ext_a_view);
        let bind_b_view = view_for(image_b_mode, &ext_b_view);

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgsl_offscreen_bg_per_frame"),
            layout: &gpu.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: gpu.uniform_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&gpu.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&bind_a_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&bind_b_view) },
            ],
        });

        // Upload uniforms.
        let mut padded = packed_uniforms;
        padded.resize(MAX_UNIFORM_FLOATS, 0.0);
        queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::cast_slice(&padded));

        // Encode the render pass.
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wgsl_offscreen_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &write_view_local,
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

        // Stash the freshly built bind group on the gpu struct so it stays
        // alive for the duration of any in-flight GPU work referencing the
        // bound views. (egui_wgpu drops the old `gpu.bind_group` here.)
        gpu.bind_group = bind_group;

        (write_tex, write_view_local, prev_view_local, write_index)
    }; // renderer write lock released here

    // Swap write_index for next frame: we just rendered into `write_index`,
    // so next frame should write into the other slot and read this one.
    {
        let mut renderer = render_state.renderer.write();
        if let Some(store) = renderer.callback_resources.get_mut::<WgslOffscreenStore>() {
            if let Some(gpu) = store.nodes.get_mut(&node_id) {
                gpu.write_index = 1 - swap_after;
            }
        }
    }

    let _ = read_view; // explicitly held only for documentation; drops here

    // Publish the freshly rendered ping-pong texture into the GPU cache so
    // downstream consumers can sample it directly. wgpu::Texture is
    // Arc-counted internally so this clone is cheap.
    crate::gpu_image::queue_publish_node_output(node_id, 0, write_texture.clone(), width, height);
    let _ = write_view;

    // Conditional readback: only stall the CPU on `device.poll(Wait)` if
    // some downstream node actually needs CPU pixel bytes. Default-true
    // (readback) is the safe path; if the hint isn't set yet (graph just
    // changed), we readback once and recover next frame.
    let needs_readback = crate::gpu_image::cpu_consumer_hint(node_id).unwrap_or(true);
    if needs_readback {
        Some(crate::gpu_image::readback_texture(device, queue, &write_texture, width, height))
    } else {
        // Return a zero-pixel stub so the existing eval-loop injection path
        // keeps working without the readback stall.
        Some(std::sync::Arc::new(ImageData {
            width,
            height,
            pixels: Vec::new(),
        }))
    }
}

struct WgslOffscreenGpu {
    pipeline: wgpu::RenderPipeline,
    /// The currently bound bind group. Phase 3 rebuilds this per frame as the
    /// image_a / image_b sources change. Phase 2 still uses a single static
    /// dummy-black bind group.
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    shader_hash: u64,

    // ── Phase 2 additions: image input infrastructure ──
    /// Persistent BGL — per-frame bind groups in Phase 3 will be built against this.
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// 1×1 black texture used as the default bind for `Unused` slots and as
    /// the fallback when an `External` slot has nothing wired.
    #[allow(dead_code)]
    dummy_black_texture: wgpu::Texture,
    dummy_black_view: wgpu::TextureView,

    // ── Phase 2 scaffolding for Phase 3 ping-pong feedback ──
    /// Two textures used as the alternating render target for self-feedback.
    /// Allocated lazily on the first frame that needs them at the active
    /// canvas size. `None` until needed; reset on size change or shader edit.
    prev_textures: [Option<wgpu::Texture>; 2],
    prev_views: [Option<wgpu::TextureView>; 2],
    /// Allocated size of the ping-pong pair (used to detect resizes).
    prev_size: (u32, u32),
    /// Index of the texture we will render INTO this frame (the other index
    /// is the "previous frame" sampled by `LastFrame` mode).
    write_index: usize,
}

struct WgslOffscreenStore {
    nodes: HashMap<NodeId, WgslOffscreenGpu>,
}

fn render_with_wgpu(
    ui: &mut egui::Ui,
    shader_code: &str,
    render_state: &egui_wgpu::RenderState,
    vertex_count: u32,
    node_id: NodeId,
    mut packed_uniforms: Vec<f32>,
    canvas_w: f32,
    canvas_h: f32,
) {
    let device = &render_state.device;
    let target_format = render_state.target_format;

    // Compute shader hash for caching
    let shader_hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        shader_code.hash(&mut h);
        h.finish()
    };

    // Check if we need to recreate pipeline
    let need_recreate = {
        let renderer = render_state.renderer.read();
        if let Some(store) = renderer.callback_resources.get::<WgslGpuStore>() {
            match store.nodes.get(&node_id) {
                Some(gpu) => gpu.shader_hash != shader_hash,
                None => true,
            }
        } else {
            true
        }
    };

    if need_recreate {
        // Create pipeline + bind group + buffer
        device.push_error_scope(wgpu::ErrorFilter::Validation);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgsl_user_shader"),
            source: wgpu::ShaderSource::Wgsl(shader_code.into()),
        });

        let bgl_entries = wgsl_viewer_bgl_entries();
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgsl_uniform_layout"),
            entries: &bgl_entries,
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgsl_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgsl_user_pipeline"),
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
                targets: &[Some(target_format.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let error = pollster::block_on(device.pop_error_scope());
        if let Some(err) = error {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100), format!("❌ GPU: {}", err));
            let (rect, _) = ui.allocate_exact_size(egui::vec2(canvas_w, canvas_h), egui::Sense::hover());
            ui.painter().rect_filled(rect, 4.0, egui::Color32::from_rgb(40, 15, 15));
            return;
        }

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgsl_uniform_buffer"),
            size: UNIFORM_BUF_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Phase 2: every WGSL Viewer pipeline now has a sampler + 2 image
        // bindings. The on-screen path always binds dummy black for image_a/b
        // — feedback only flows through the offscreen path in v1.
        let sampler = create_default_image_sampler(device);
        let (dummy_black_texture, dummy_black_view) = create_dummy_black_texture(device, &render_state.queue);

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgsl_uniform_bind_group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&dummy_black_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&dummy_black_view) },
            ],
        });

        let node_gpu = WgslNodeGpu {
            pipeline,
            bind_group,
            uniform_buffer,
            vertex_count,
            shader_hash,
            bind_group_layout,
            sampler,
            dummy_black_texture,
            dummy_black_view,
        };

        let mut renderer = render_state.renderer.write();
        if let Some(store) = renderer.callback_resources.get_mut::<WgslGpuStore>() {
            store.nodes.insert(node_id, node_gpu);
        } else {
            let mut nodes = HashMap::new();
            nodes.insert(node_id, node_gpu);
            renderer.callback_resources.insert(WgslGpuStore { nodes });
        }
    }

    // Allocate canvas rect
    let (rect, response) = ui.allocate_exact_size(egui::vec2(canvas_w, canvas_h), egui::Sense::hover());

    // Update mouse relative to canvas
    if let Some(pos) = response.hover_pos() {
        let rel_x = (pos.x - rect.min.x) / canvas_w;
        let rel_y = (pos.y - rect.min.y) / canvas_h;
        packed_uniforms[4] = rel_x.clamp(0.0, 1.0); // mouse_x at index 4
        packed_uniforms[5] = rel_y.clamp(0.0, 1.0); // mouse_y at index 5
    }

    // Ensure uniform_floats length is MAX
    packed_uniforms.resize(MAX_UNIFORM_FLOATS, 0.0);

    // Store uniform data for prepare()
    {
        let mut renderer = render_state.renderer.write();
        if let Some(store) = renderer.callback_resources.get_mut::<WgslUniformStore>() {
            store.data.insert(node_id, packed_uniforms);
        } else {
            let mut data = HashMap::new();
            data.insert(node_id, packed_uniforms);
            renderer.callback_resources.insert(WgslUniformStore { data });
        }
    }

    // Add paint callback
    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
        rect,
        WgslPaintCallback { node_id },
    ));

    ui.ctx().request_repaint(); // keep animating
}

fn render_popout_window(
    ctx: &egui::Context,
    _shader_code: &str,
    render_state: &egui_wgpu::RenderState,
    _vertex_count: u32,
    node_id: NodeId,
    packed_uniforms: Vec<f32>,
    _canvas_w: &mut f32,
    _canvas_h: &mut f32,
) {
    let viewport_id = egui::ViewportId::from_hash_of(("wgsl_popout", node_id));
    let render_state = render_state.clone();
    let uniforms = packed_uniforms;
    let popout_id = egui::Id::new(("wgsl_popout", node_id));

    ctx.show_viewport_immediate(
        viewport_id,
        egui::ViewportBuilder::default()
            .with_title(format!("Shader #{}", node_id))
            .with_inner_size([800.0, 600.0]),
        move |ctx, _class| {
            // Detect native window close (red ❌ button) and set popout flag to false
            if ctx.input(|i| i.viewport().close_requested()) {
                ctx.data_mut(|d| d.insert_temp(popout_id, false));
                return;
            }

            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
                .show(ctx, |ui| {
                    let avail = ui.available_size();
                    let w = avail.x.max(100.0);
                    let h = avail.y.max(100.0);

                    let (rect, response) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());

                    let mut packed = uniforms.clone();
                    // Update mouse
                    if let Some(pos) = response.hover_pos() {
                        let rel_x = (pos.x - rect.min.x) / w;
                        let rel_y = (pos.y - rect.min.y) / h;
                        packed[4] = rel_x.clamp(0.0, 1.0);
                        packed[5] = rel_y.clamp(0.0, 1.0);
                    }
                    // Update resolution to match window size
                    packed[2] = w;
                    packed[3] = h;
                    packed.resize(MAX_UNIFORM_FLOATS, 0.0);

                    // Store uniform data for the main node_id
                    // (The popout shares the same pipeline as the inline render)
                    {
                        let mut renderer = render_state.renderer.write();
                        if let Some(store) = renderer.callback_resources.get_mut::<WgslUniformStore>() {
                            store.data.insert(node_id, packed);
                        } else {
                            let mut data = HashMap::new();
                            data.insert(node_id, packed);
                            renderer.callback_resources.insert(WgslUniformStore { data });
                        }
                    }

                    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                        rect,
                        WgslPaintCallback { node_id },
                    ));

                    ctx.request_repaint();
                });
        },
    );
}

fn validate_wgsl_naga(code: &str) -> Result<(), String> {
    let module = naga::front::wgsl::parse_str(code)
        .map_err(|e| format!("Parse: {}", e))?;
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator.validate(&module).map_err(|e| format!("Validation: {}", e))?;
    Ok(())
}

/// Drop all GPU resources for a deleted WGSL Viewer node so VRAM is
/// released immediately rather than leaking inside egui_wgpu's
/// callback_resources. Called from
/// `crate::gpu_image::GpuTextureCache::invalidate_node`.
pub(crate) fn cleanup_node(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
    node_id: crate::graph::NodeId,
) {
    if let Some(store) = callback_resources.get_mut::<WgslGpuStore>() {
        store.nodes.remove(&node_id);
    }
    if let Some(store) = callback_resources.get_mut::<WgslOffscreenStore>() {
        store.nodes.remove(&node_id);
    }
    if let Some(store) = callback_resources.get_mut::<WgslUniformStore>() {
        store.data.remove(&node_id);
    }
}

/// Wipe every WGSL Viewer GPU pipeline + offscreen target +
/// uniform buffer. See `GpuTextureCache::clear_all`.
pub(crate) fn cleanup_all(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
) {
    if let Some(store) = callback_resources.get_mut::<WgslGpuStore>() {
        store.nodes.clear();
    }
    if let Some(store) = callback_resources.get_mut::<WgslOffscreenStore>() {
        store.nodes.clear();
    }
    if let Some(store) = callback_resources.get_mut::<WgslUniformStore>() {
        store.data.clear();
    }
}
