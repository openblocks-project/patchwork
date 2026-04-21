//! TransformNode — Scale (X/Y), Rotate, Flip for images.

use crate::graph::{PortDef, PortKind, PortValue, ImageData};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;
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
