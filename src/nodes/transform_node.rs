//! TransformNode — Scale (X/Y), Rotate, Flip for images.
//!
//! Phase 2.5: GPU-aware. Mirrors the Kaleidoscope template
//! (`src/nodes/kaleidoscope_node.rs`). When a wgpu render state is
//! available we run a fragment shader that applies the inverse 2×3
//! affine (rotation + scale + flip), publish the result to the shared
//! `GpuTextureCache` (so zero-readback Syphon / Spout chains work),
//! and emit `PortValue::GpuImage`. Without wgpu (headless / tests) we
//! fall through to the existing CPU DDA path — unchanged.

use crate::graph::{PortDef, PortKind, PortValue, ImageData};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;
use eframe::egui_wgpu::wgpu;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformNode {
    #[serde(default = "default_one")]
    pub scale_x: f32,
    #[serde(default = "default_one")]
    pub scale_y: f32,
    #[serde(default)]
    pub rotation: f32,
    #[serde(default)]
    pub flip_h: bool,
    #[serde(default)]
    pub flip_v: bool,

    // ── Memoization (transient) ──────────────────────────────────────────
    // UI repaints at ~60 Hz but the camera delivers frames at ~30 Hz, so
    // without this cache Transform ran its flip/scale/rotate work twice
    // per real frame AND produced a fresh output Arc each time — which
    // invalidated the Image node's Arc-ptr cache downstream and caused
    // the jitter the user saw. Keyed on the input Arc's data pointer
    // plus a cheap hash of the transform params.
    #[serde(skip)]
    cache_in_ptr: usize,
    #[serde(skip)]
    cache_param_key: u64,
    #[serde(skip)]
    cache_out: Option<Arc<ImageData>>,
    /// Reusable src-column-index map for the scale-only path. Avoids a
    /// fresh Vec alloc every frame while the user drags the Scale sliders.
    #[serde(skip)]
    col_map: Vec<i32>,
}

fn default_one() -> f32 { 1.0 }

impl Default for TransformNode {
    fn default() -> Self {
        Self {
            scale_x: 1.0, scale_y: 1.0, rotation: 0.0, flip_h: false, flip_v: false,
            cache_in_ptr: 0, cache_param_key: 0, cache_out: None,
            col_map: Vec::new(),
        }
    }
}

/// Allocate an output buffer for an image of `(w * h * 4)` bytes without
/// paying the cost of `vec![0u8; ...]` zeroing. Safety: callers MUST
/// write every byte of the returned Vec before handing it off, otherwise
/// the extension is reading uninitialised memory.
fn alloc_pixel_buf(bytes: usize) -> Vec<u8> {
    let mut v: Vec<u8> = Vec::with_capacity(bytes);
    // SAFETY: the length is ≤ capacity and u8 has no invalid bit
    // patterns. Each caller fills every pixel via its inner loop below
    // — in-bounds source reads write the real pixel, out-of-bounds
    // branches explicitly write 0.
    unsafe { v.set_len(bytes); }
    v
}

impl TransformNode {
    /// Pack the current transform parameters into a single u64 so the
    /// memoization key only needs one compare. Scale/rotation quantized
    /// to 1e-4 — well below user-perceivable change and plenty of
    /// resolution for the sliders.
    fn param_key(&self) -> u64 {
        let sx = (self.scale_x * 10_000.0) as i32 as u64;
        let sy = (self.scale_y * 10_000.0) as i32 as u64;
        let r  = (self.rotation * 10_000.0) as i32 as u64;
        let f  = (self.flip_h as u64) | ((self.flip_v as u64) << 1);
        (sx << 48) ^ (sy << 32) ^ (r << 16) ^ f
    }
}

impl TransformNode {
    /// Whether the current state would produce exactly the input image.
    fn is_identity(&self) -> bool {
        (self.scale_x - 1.0).abs() < 1e-4
            && (self.scale_y - 1.0).abs() < 1e-4
            && self.rotation.abs() < 1e-4
            && !self.flip_h
            && !self.flip_v
    }

    /// Whether only horizontal/vertical flips are active (no scale/rotate).
    /// This case can skip all the trig/division work in the hot loop.
    fn is_pure_flip(&self) -> bool {
        (self.scale_x - 1.0).abs() < 1e-4
            && (self.scale_y - 1.0).abs() < 1e-4
            && self.rotation.abs() < 1e-4
            && (self.flip_h || self.flip_v)
    }

    fn transform_flip_only(&self, img: &ImageData) -> Arc<ImageData> {
        let w = img.width as usize;
        let h = img.height as usize;
        let row_bytes = w * 4;
        // Every output byte is written below, so we can skip the memset.
        let mut pixels = alloc_pixel_buf(row_bytes * h);

        // Cast to u32 so each pixel is one load/store instead of a
        // 4-byte copy_from_slice call. That call had function-call
        // overhead + per-byte bounds checks in debug builds — enough
        // to visibly drop fps when the user enabled Flip H on a live
        // 30 fps camera. u32-granular copies compile to a tight load-
        // store loop even without optimizations.
        let src: &[u32] = bytemuck::cast_slice(&img.pixels);
        let dst: &mut [u32] = bytemuck::cast_slice_mut(&mut pixels);

        for dy in 0..h {
            let src_y = if self.flip_v { h - 1 - dy } else { dy };
            let src_row_start = src_y * w;
            let dst_row_start = dy * w;
            let src_row = &src[src_row_start .. src_row_start + w];
            let dst_row = &mut dst[dst_row_start .. dst_row_start + w];

            if self.flip_h {
                // iter+rev compiles cleanly to a reversed indexed loop
                // with no copy_from_slice bookkeeping.
                for (d, s) in dst_row.iter_mut().zip(src_row.iter().rev()) {
                    *d = *s;
                }
            } else {
                dst_row.copy_from_slice(src_row);
            }
        }
        Arc::new(ImageData { width: img.width, height: img.height, pixels })
    }

    /// Scale-only (optional flip, no rotation). Precomputes the source
    /// column/row maps so the inner loop is just a u32 array lookup +
    /// store — no trig, no per-pixel float math, no copy_from_slice call
    /// overhead. Handles the entire scale-param-only case, which is what
    /// the user was dragging when fps tanked.
    fn transform_scale_only(&mut self, img: &ImageData) -> Arc<ImageData> {
        let in_w = img.width as i32;
        let in_h = img.height as i32;
        let out_w = ((img.width as f32 * self.scale_x).round().max(1.0)) as u32;
        let out_h = ((img.height as f32 * self.scale_y).round().max(1.0)) as u32;

        let cx_out = out_w as f32 * 0.5;
        let cy_out = out_h as f32 * 0.5;
        let cx_in = img.width as f32 * 0.5;
        let cy_in = img.height as f32 * 0.5;
        let inv_sx = 1.0 / self.scale_x;
        let inv_sy = 1.0 / self.scale_y;

        // Reuse the col_map buffer across calls — at out_w=2560 this was
        // a 10 KB alloc every frame otherwise.
        self.col_map.clear();
        self.col_map.reserve(out_w as usize);
        for dx in 0..out_w {
            let mut sx = (dx as f32 - cx_out) * inv_sx + cx_in;
            if self.flip_h { sx = img.width as f32 - 1.0 - sx; }
            self.col_map.push(sx as i32);
        }

        let mut pixels = alloc_pixel_buf((out_w * out_h * 4) as usize);
        let src: &[u32] = bytemuck::cast_slice(&img.pixels);
        let dst: &mut [u32] = bytemuck::cast_slice_mut(&mut pixels);

        for dy in 0..out_h {
            let mut sy = (dy as f32 - cy_out) * inv_sy + cy_in;
            if self.flip_v { sy = img.height as f32 - 1.0 - sy; }
            let iy = sy as i32;
            let dst_row_start = dy as usize * out_w as usize;

            if iy < 0 || iy >= in_h {
                // Source row out of bounds — fill this dst row with 0.
                for i in 0..out_w as usize {
                    dst[dst_row_start + i] = 0;
                }
                continue;
            }
            let src_row_start = iy as usize * img.width as usize;

            for (dx_idx, &ix) in self.col_map.iter().enumerate() {
                let di = dst_row_start + dx_idx;
                dst[di] = if ix >= 0 && ix < in_w {
                    src[src_row_start + ix as usize]
                } else {
                    0
                };
            }
        }

        Arc::new(ImageData { width: out_w, height: out_h, pixels })
    }

    fn transform_image(&mut self, img: &ImageData) -> Arc<ImageData> {
        // Fast path for pure flip: no trig, no division, row-granular copy.
        // Identity is handled by the caller (passes the input Arc through),
        // so we only get here for non-identity transforms.
        if self.is_pure_flip() {
            return self.transform_flip_only(img);
        }

        // Scale-only fast path (optional flip). Any non-zero rotation
        // falls through to the general path below.
        if self.rotation.abs() < 1e-4 {
            return self.transform_scale_only(img);
        }

        // General rotation + scale path.
        //
        // Inner-loop math is DDA (Digital Differential Analyzer) style:
        // we fold the inverse-rotate, inverse-scale, and flip into a
        // single affine 2×3 matrix and then step `sx += a; sy += c` per
        // pixel across a row. That replaces ~8 float ops per pixel (two
        // rotation muls, two scale muls, two subs for ox/oy, two flips)
        // with 2 float ops per pixel — the remaining cost is essentially
        // just the bounds check + u32 store. This is what lets the node
        // stay at camera rate when scale AND rotation are both engaged
        // at the same time.
        let w = img.width as f32;
        let h = img.height as f32;
        let out_w = ((w * self.scale_x).round().max(1.0)) as u32;
        let out_h = ((h * self.scale_y).round().max(1.0)) as u32;
        let mut pixels = alloc_pixel_buf((out_w * out_h * 4) as usize);

        let angle = self.rotation.to_radians();
        let cos_a = angle.cos();
        let sin_a = angle.sin();
        let cx_out = out_w as f32 * 0.5;
        let cy_out = out_h as f32 * 0.5;
        let cx_in = w * 0.5;
        let cy_in = h * 0.5;
        let inv_sx = 1.0 / self.scale_x;
        let inv_sy = 1.0 / self.scale_y;

        // Affine matrix for inverse transform (dst → src), folding in
        // optional flips by flipping the matrix rows directly.
        //   src_x = a*dx + b*dy + tx
        //   src_y = c*dx + d*dy + ty
        let mut a =  cos_a * inv_sx;
        let mut b =  sin_a * inv_sx;
        let mut c = -sin_a * inv_sy;
        let mut d =  cos_a * inv_sy;
        let mut tx = cx_in - a * cx_out - b * cy_out;
        let mut ty = cy_in - c * cx_out - d * cy_out;
        if self.flip_h { a = -a; b = -b; tx = (w - 1.0) - tx; }
        if self.flip_v { c = -c; d = -d; ty = (h - 1.0) - ty; }

        let in_w = img.width as i32;
        let in_h = img.height as i32;
        let src: &[u32] = bytemuck::cast_slice(&img.pixels);
        let dst: &mut [u32] = bytemuck::cast_slice_mut(&mut pixels);

        for dy in 0..out_h {
            let dy_f = dy as f32;
            // Row-start source coords. Inside the row we just increment
            // by (a, c) per dst column.
            let mut sx = b * dy_f + tx;
            let mut sy = d * dy_f + ty;
            let dst_row_start = dy as usize * out_w as usize;

            for dx in 0..out_w {
                let ix = sx as i32;   // truncate is fine for nearest-neighbour
                let iy = sy as i32;
                let di = dst_row_start + dx as usize;

                // Always write — the buffer is uninitialised, and the
                // out-of-bounds branch needs to leave 0 anyway.
                dst[di] = if ix >= 0 && ix < in_w && iy >= 0 && iy < in_h {
                    src[iy as usize * img.width as usize + ix as usize]
                } else {
                    0
                };

                sx += a;
                sy += c;
            }
        }

        Arc::new(ImageData { width: out_w, height: out_h, pixels })
    }
}

// ── GPU pipeline ────────────────────────────────────────────────────────────
//
// Mirrors Kaleidoscope: per-node `TransformGpu` stored inside
// `egui_wgpu::CallbackResources`, inline WGSL (no asset file),
// `ensure_pipeline` idempotent, `process_gpu` consumed by
// `evaluate_with_ctx`, `cleanup_{node,all}` wired from `gpu_image.rs`.

const TRANSFORM_SHADER: &str = r#"
struct Params {
    // Inverse affine matrix (destination pixel → source pixel),
    // precomputed on CPU from rotation + scale + flip.
    //   src_x = a*dx + b*dy + tx
    //   src_y = c*dx + d*dy + ty
    a:   f32, b:   f32, c:   f32, d:   f32,
    tx:  f32, ty:  f32,
    in_w:  f32, in_h:  f32,
    out_w: f32, out_h: f32,
    _pad0: f32, _pad1: f32,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var input_tex:     texture_2d<f32>;
@group(0) @binding(2) var input_sampler: sampler;

@vertex fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
    let pos = array(vec2f(-1, -1), vec2f(3, -1), vec2f(-1, 3));
    return vec4f(pos[vi], 0.0, 1.0);
}

@fragment fn fs_main(@builtin(position) coord: vec4f) -> @location(0) vec4f {
    // coord.xy is fragment-center in output pixel space (half-pixel
    // offsets). Apply the same inverse affine the CPU path uses in
    // `transform_image` — fewer ops than rebuilding the matrix per pixel.
    let src_x = params.a * coord.x + params.b * coord.y + params.tx;
    let src_y = params.c * coord.x + params.d * coord.y + params.ty;

    // Match CPU out-of-bounds behaviour: write transparent black
    // (see `transform_image` ix/iy bounds check). Without this,
    // `ClampToEdge` would smear the edge row over the rotated corners.
    if (src_x < 0.0 || src_x >= params.in_w ||
        src_y < 0.0 || src_y >= params.in_h) {
        return vec4f(0.0);
    }

    // Normalize to [0,1] for textureSample. Linear filtering is a
    // visual improvement over the CPU path's nearest-neighbour —
    // smoother rotation + non-integer scale.
    let src_uv = vec2f(src_x / params.in_w, src_y / params.in_h);
    return textureSample(input_tex, input_sampler, src_uv);
}
"#;

struct TransformGpu {
    pipeline:          wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer:    wgpu::Buffer,
    sampler:           wgpu::Sampler,
}

struct TransformStore {
    nodes: HashMap<crate::graph::NodeId, TransformGpu>,
}

fn ensure_pipeline(
    node_id: crate::graph::NodeId,
    render_state: &eframe::egui_wgpu::RenderState,
) -> bool {
    let device = &render_state.device;

    let already = {
        let renderer = render_state.renderer.read();
        renderer
            .callback_resources
            .get::<TransformStore>()
            .and_then(|s| s.nodes.get(&node_id))
            .is_some()
    };
    if already { return true; }

    device.push_error_scope(wgpu::ErrorFilter::Validation);

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("transform_shader"),
        source: wgpu::ShaderSource::Wgsl(TRANSFORM_SHADER.into()),
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("transform_bgl"),
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
        label: Some("transform_pipeline"),
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
            // `Bgra8UnormSrgb` (not `Bgra8Unorm`!) to match the sRGB
            // colourspace of Camera / NDI In uploads, which go through
            // `upload_texture` / `get_or_upload` at
            // `gpu_image.rs` lines 660/830/926/944 as
            // `Rgba8UnormSrgb`. Without this match:
            //   1. Sampler decodes sRGB-encoded upstream bytes → linear.
            //   2. Shader applies the affine transform on linear values.
            //   3. Output writes linear values into a `Bgra8Unorm`
            //      texture, which downstream consumers (Syphon, NDI,
            //      Visual Output, Video In NDI loopback) then render
            //      as if they were sRGB display values.
            // Net effect: gamma-compressed shadows + boosted mid-tones,
            // which the user reads as "added contrast / saturation".
            // Keeping the sRGB suffix makes the linear-math step
            // transparent: encode on write, decode on next sample.
            // BGRA channel order chosen over RGBA so Syphon's MTLTexture
            // hand-off stays unswizzled (matches Syphon's preferred
            // format); NDI readback handles the channel swap in
            // `readback_texture_bgra`. Kaleidoscope uses
            // `Rgba8UnormSrgb` — different channel order for its own
            // Metal-compat reasons.
            targets: &[Some(wgpu::TextureFormat::Bgra8UnormSrgb.into())],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    if pollster::block_on(device.pop_error_scope()).is_some() {
        crate::system_log::error("Transform GPU pipeline creation failed".to_string());
        return false;
    }

    // 12 f32 × 4 bytes = 48 bytes. Multiple of 16, satisfies WGSL
    // uniform struct alignment rule.
    let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("transform_ub"),
        size: 48,
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

    let gpu = TransformGpu { pipeline, bind_group_layout, uniform_buffer, sampler };
    let mut renderer = render_state.renderer.write();
    if let Some(store) = renderer.callback_resources.get_mut::<TransformStore>() {
        store.nodes.insert(node_id, gpu);
    } else {
        let mut nodes = HashMap::new();
        nodes.insert(node_id, gpu);
        renderer.callback_resources.insert(TransformStore { nodes });
    }
    true
}

impl TransformNode {
    /// Build the inverse 2×3 affine matrix `(a, b, c, d, tx, ty)` from
    /// the current params, matching the CPU path in `transform_image`
    /// lines 241-248. Returns `(matrix, out_w, out_h)`.
    fn build_inverse_affine(&self, in_w: u32, in_h: u32) -> ([f32; 6], u32, u32) {
        let w = in_w as f32;
        let h = in_h as f32;
        let out_w = ((w * self.scale_x).round().max(1.0)) as u32;
        let out_h = ((h * self.scale_y).round().max(1.0)) as u32;

        let angle = self.rotation.to_radians();
        let cos_a = angle.cos();
        let sin_a = angle.sin();
        let cx_out = out_w as f32 * 0.5;
        let cy_out = out_h as f32 * 0.5;
        let cx_in = w * 0.5;
        let cy_in = h * 0.5;
        let inv_sx = 1.0 / self.scale_x;
        let inv_sy = 1.0 / self.scale_y;

        let mut a =  cos_a * inv_sx;
        let mut b =  sin_a * inv_sx;
        let mut c = -sin_a * inv_sy;
        let mut d =  cos_a * inv_sy;
        let mut tx = cx_in - a * cx_out - b * cy_out;
        let mut ty = cy_in - c * cx_out - d * cy_out;
        if self.flip_h { a = -a; b = -b; tx = (w - 1.0) - tx; }
        if self.flip_v { c = -c; d = -d; ty = (h - 1.0) - ty; }
        ([a, b, c, d, tx, ty], out_w, out_h)
    }

    /// GPU implementation of Transform: run the inverse-affine shader,
    /// publish to the shared cache, return `GpuImage` (or readback to
    /// `Image` if a CPU consumer is downstream).
    #[allow(clippy::too_many_arguments)]
    pub fn process_gpu(
        &self,
        in_w: u32,
        in_h: u32,
        cpu_img: Option<&ImageData>,
        node_id: crate::graph::NodeId,
        render_state: &eframe::egui_wgpu::RenderState,
        tex_cache: &mut crate::gpu_image::GpuTextureCache,
        gpu_source: Option<(crate::graph::NodeId, usize)>,
        needs_readback: bool,
    ) -> Option<crate::graph::PortValue> {
        let device = &render_state.device;
        let queue  = &render_state.queue;
        if in_w == 0 || in_h == 0 { return None; }

        if !ensure_pipeline(node_id, render_state) { return None; }

        // Prefer the upstream GPU-resident texture; only CPU-upload
        // when we have real pixels and no GPU source is available.
        let (input_view, _keepalive) = match gpu_source
            .and_then(|(nid, p)| tex_cache.get_node_output_cloned(nid, p))
            .filter(|(_, gw, gh)| *gw == in_w && *gh == in_h)
        {
            Some((view, _, _)) => (view, None),
            None => match cpu_img.filter(|i| !i.pixels.is_empty()) {
                Some(img) => {
                    let tex = crate::gpu_image::upload_texture(device, queue, img, "transform_input");
                    let v = tex.create_view(&Default::default());
                    (v, Some(tex))
                }
                None => return None,
            },
        };

        let (m, out_w, out_h) = self.build_inverse_affine(in_w, in_h);

        let output_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("transform_output"),
            size: wgpu::Extent3d { width: out_w, height: out_h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // `Bgra8UnormSrgb` — must match the render-pipeline target
            // format declared at line :435. See the long-form comment
            // there for the sRGB-matching rationale; TL;DR: the upstream
            // texture is sRGB, so the output has to be sRGB too or the
            // gamma conversions on sample and store don't cancel.
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                 | wgpu::TextureUsages::COPY_SRC
                 | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let output_view = output_tex.create_view(&Default::default());

        let renderer = render_state.renderer.read();
        let store = renderer.callback_resources.get::<TransformStore>()?;
        let gpu = store.nodes.get(&node_id)?;

        // Pack uniforms (12 f32s, 48 bytes — std140 16-byte struct align).
        let params: [f32; 12] = [
            m[0], m[1], m[2], m[3], // a b c d
            m[4], m[5],             // tx ty
            in_w as f32, in_h as f32,
            out_w as f32, out_h as f32,
            0.0, 0.0,               // pad
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
                label: Some("transform_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Transparent black clear — shader also writes
                        // black on OOB, so this only matters if fragments
                        // outside the triangle get skipped (never happens
                        // with a fullscreen triangle, but defensive).
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
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
            let result = crate::gpu_image::readback_texture(device, queue, &output_tex, out_w, out_h);
            tex_cache.publish(node_id, 0, output_tex, out_w, out_h, render_state);
            Some(crate::graph::PortValue::Image(result))
        } else {
            tex_cache.publish(node_id, 0, output_tex, out_w, out_h, render_state);
            Some(crate::graph::PortValue::GpuImage(crate::graph::GpuImageHandle {
                node_id,
                port: 0,
                width: out_w,
                height: out_h,
                frame_stamp: tex_cache.frame_stamp(node_id, 0),
            }))
        }
    }
}

impl NodeBehavior for TransformNode {
    fn title(&self) -> &str { "Transform" }
    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Image", PortKind::Image),
            PortDef::new("Scale X", PortKind::Number),
            PortDef::new("Scale Y", PortKind::Number),
            PortDef::new("Rotation", PortKind::Number),
        ]
    }
    fn outputs(&self) -> Vec<PortDef> { vec![PortDef::new("Image", PortKind::Image)] }
    fn color_hint(&self) -> [u8; 3] { [140, 180, 220] }
    fn inline_ports(&self) -> bool { true }

    /// Image input (port 0) is happy with a `GpuImage` handle —
    /// `evaluate_with_ctx` resolves it through the GPU texture cache.
    /// Keeps the zero-readback Syphon / Spout chain intact. Param
    /// ports (1..=3) stay CPU (Number values).
    fn needs_cpu_image_input(&self, port: usize) -> bool { port != 0 }

    fn evaluate_with_ctx(
        &mut self,
        inputs: &[PortValue],
        ctx: &mut crate::node_trait::EvalCtx<'_>,
    ) -> Vec<(usize, PortValue)> {
        // Apply param-port overrides — same clamps as `evaluate`.
        if let Some(PortValue::Float(v)) = inputs.get(1) { self.scale_x = v.clamp(0.1, 3.0); }
        if let Some(PortValue::Float(v)) = inputs.get(2) { self.scale_y = v.clamp(0.1, 3.0); }
        if let Some(PortValue::Float(v)) = inputs.get(3) { self.rotation = *v % 360.0; }

        // Resolve input dimensions from either a CPU `Image` or a GPU
        // handle. The cpu_img option lets `process_gpu` fall back to
        // a CPU→GPU upload if no GPU source is available.
        let (in_w, in_h, cpu_img) = match inputs.first() {
            Some(PortValue::Image(img)) => (img.width, img.height, Some(img.as_ref())),
            Some(PortValue::GpuImage(h)) => (h.width, h.height, None),
            _ => return vec![(0, PortValue::None)],
        };

        // Identity passthrough — skip the whole GPU pipeline. If input
        // was a GpuImage, re-emit the handle verbatim (no cache churn).
        // If input was CPU, clone the Arc (zero-copy).
        if self.is_identity() {
            let result = match inputs.first() {
                Some(PortValue::Image(img)) => PortValue::Image(img.clone()),
                Some(PortValue::GpuImage(h)) => PortValue::GpuImage(*h),
                _ => PortValue::None,
            };
            return vec![(0, result)];
        }

        // GPU path.
        if let Some(rs) = ctx.render_state {
            let gpu_src = ctx.input_sources.first().copied().flatten();
            if let Some(val) = self.process_gpu(
                in_w, in_h, cpu_img,
                ctx.node_id, rs, ctx.gpu_tex_cache,
                gpu_src, ctx.needs_readback,
            ) {
                return vec![(0, val)];
            }
        }

        // CPU fallback (headless / no render_state) — unchanged path
        // still benefits from the Arc-ptr memoization.
        self.evaluate(inputs)
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        if let Some(PortValue::Float(v)) = inputs.get(1) { self.scale_x = v.clamp(0.1, 3.0); }
        if let Some(PortValue::Float(v)) = inputs.get(2) { self.scale_y = v.clamp(0.1, 3.0); }
        if let Some(PortValue::Float(v)) = inputs.get(3) { self.rotation = *v % 360.0; }

        let result = match inputs.first() {
            // Identity passthrough — just clone the Arc, no pixel work.
            Some(PortValue::Image(img)) if self.is_identity() => {
                PortValue::Image(img.clone())
            }
            Some(PortValue::Image(img)) => {
                // Memoize across repaints: UI runs at ~60 Hz but Camera
                // emits at ~30 Hz, so we see the same input Arc twice.
                // Returning the same cached output Arc also keeps the
                // Image node's Arc-ptr cache stable.
                let in_ptr = Arc::as_ptr(img) as usize;
                let key = self.param_key();
                let hit = self.cache_in_ptr == in_ptr
                    && self.cache_param_key == key
                    && self.cache_out.is_some();
                if !hit {
                    self.cache_out = Some(self.transform_image(img));
                    self.cache_in_ptr = in_ptr;
                    self.cache_param_key = key;
                }
                PortValue::Image(self.cache_out.clone().unwrap())
            }
            _ => PortValue::None,
        };
        vec![(0, result)]
    }

    fn type_tag(&self) -> &str { "transform" }
    fn save_state(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or_default() }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<TransformNode>(state.clone()) { *self = l; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        // Image input
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Image);
            ui.label(egui::RichText::new("Image").small());
        });

        // Scale X
        let sx_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 1);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 1, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(egui::RichText::new("Scale X").small());
            if sx_wired {
                ui.label(egui::RichText::new(format!("{:.2}", self.scale_x)).small().color(egui::Color32::from_rgb(80, 170, 255)));
            } else {
                ui.add(egui::Slider::new(&mut self.scale_x, 0.1..=3.0).step_by(0.01).show_value(true));
            }
        });

        // Scale Y
        let sy_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 2);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 2, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(egui::RichText::new("Scale Y").small());
            if sy_wired {
                ui.label(egui::RichText::new(format!("{:.2}", self.scale_y)).small().color(egui::Color32::from_rgb(80, 170, 255)));
            } else {
                ui.add(egui::Slider::new(&mut self.scale_y, 0.1..=3.0).step_by(0.01).show_value(true));
            }
        });

        // Rotation
        let rot_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 3);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 3, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(egui::RichText::new("Rotate").small());
            if rot_wired {
                ui.label(egui::RichText::new(format!("{:.0}°", self.rotation)).small().color(egui::Color32::from_rgb(80, 170, 255)));
            } else {
                ui.add(egui::Slider::new(&mut self.rotation, 0.0..=360.0).step_by(1.0).show_value(true).suffix("°"));
            }
        });

        // Flip toggles
        ui.horizontal(|ui| {
            ui.toggle_value(&mut self.flip_h, "Flip H");
            ui.toggle_value(&mut self.flip_v, "Flip V");
        });

        // GPU badge — matches Kaleidoscope / ImageEffects convention
        // so users know this node is hardware-accelerated.
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        ui.label(egui::RichText::new("⚡ GPU").small().color(dim));

        ui.separator();
        crate::nodes::audio_port_row(ui, "Image", ctx.node_id, 0, false, ctx.port_positions,
            ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Image);
    }
}

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("transform", |state| {
        if let Ok(n) = serde_json::from_value::<TransformNode>(state.clone()) { Box::new(n) }
        else { Box::new(TransformNode::default()) }
    });
}

/// Drop the `TransformGpu` entry for a removed node so its uniform
/// buffer + pipeline refcounts release immediately instead of waiting
/// for the next `clear_all`. Called from `gpu_image.rs` alongside
/// Kaleidoscope / ImageEffects / ColorCurves / etc.
pub(crate) fn cleanup_node(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
    node_id: crate::graph::NodeId,
) {
    if let Some(store) = callback_resources.get_mut::<TransformStore>() {
        store.nodes.remove(&node_id);
    }
}

/// Wipe every per-node GPU entry. Called on project-load/reset so a
/// fresh graph can't read back the previous project's textures via
/// stale NodeId collisions.
pub(crate) fn cleanup_all(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
) {
    if let Some(store) = callback_resources.get_mut::<TransformStore>() {
        store.nodes.clear();
    }
}
