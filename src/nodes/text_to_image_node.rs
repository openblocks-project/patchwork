use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use eframe::egui::{self, RichText};
use std::sync::{Arc, LazyLock};
use crate::graph::{Graph, ImageData, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── Text to Image ────────────────────────────────────────────────────────────
//
// Rasterize a text string into an RGBA image. Wire it into Blend.B over a
// video frame to caption the feed; or feed it to a Frame Recorder, WGSL
// Viewer, etc.
//
// Ports:
//   In  0: Text   (overrides the inline editor when wired)
//   In  1: Width  (optional; only used when Auto-size is off)
//   In  2: Height (optional; only used when Auto-size is off)
//   Out 0: Image  (rasterized RGBA)

static SATOSHI_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Satoshi-Regular.ttf");

static FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(SATOSHI_REGULAR)
        .expect("Bundled Satoshi-Regular.ttf failed to parse")
});

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum Align { Left, Center, Right }

#[derive(Debug, Clone)]
pub struct TextToImageNode {
    text: String,
    font_size: f32,
    color: [u8; 4],
    bg_enabled: bool,
    bg_color: [u8; 4],
    align: Align,
    pad_x: u32,
    pad_y: u32,
    auto_size: bool,
    default_width: u32,
    default_height: u32,
    last_rendered_text: String,
    cached_image: Option<Arc<ImageData>>,
    dirty: bool,
}

impl Default for TextToImageNode {
    fn default() -> Self {
        Self {
            text: "Hello".to_string(),
            font_size: 48.0,
            color: [255, 255, 255, 255],
            bg_enabled: false,
            bg_color: [0, 0, 0, 160],
            align: Align::Left,
            pad_x: 16,
            pad_y: 12,
            auto_size: true,
            default_width: 512,
            default_height: 128,
            last_rendered_text: String::new(),
            cached_image: None,
            dirty: true,
        }
    }
}

// ── Pixel helpers (mirrors fill_node patterns) ───────────────────────────────

#[inline(always)]
fn pack_rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    (a as u32) << 24 | (b as u32) << 16 | (g as u32) << 8 | r as u32
}

fn alloc_pixel_buf(w: u32, h: u32) -> Vec<u8> {
    let bytes = (w * h * 4) as usize;
    let mut v: Vec<u8> = Vec::with_capacity(bytes);
    // SAFETY: every pixel is written before this buffer escapes.
    unsafe { v.set_len(bytes); }
    v
}

/// Source-over alpha blend a single pixel of a glyph onto the dst buffer.
/// Uses straight-alpha math; both src and dst are RGBA8 straight-alpha.
#[inline(always)]
fn blend_pixel(dst: &mut [u8], idx: usize, src: [u8; 4], coverage: f32) {
    let sa = (src[3] as f32 / 255.0) * coverage.clamp(0.0, 1.0);
    if sa <= 0.0 { return; }
    let inv = 1.0 - sa;
    let dr = dst[idx]     as f32 / 255.0;
    let dg = dst[idx + 1] as f32 / 255.0;
    let db = dst[idx + 2] as f32 / 255.0;
    let da = dst[idx + 3] as f32 / 255.0;
    let sr = src[0] as f32 / 255.0;
    let sg = src[1] as f32 / 255.0;
    let sb = src[2] as f32 / 255.0;
    let or = sr * sa + dr * inv;
    let og = sg * sa + dg * inv;
    let ob = sb * sa + db * inv;
    let oa = sa + da * inv;
    dst[idx]     = (or * 255.0).round().clamp(0.0, 255.0) as u8;
    dst[idx + 1] = (og * 255.0).round().clamp(0.0, 255.0) as u8;
    dst[idx + 2] = (ob * 255.0).round().clamp(0.0, 255.0) as u8;
    dst[idx + 3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
}

// ── Text generation ──────────────────────────────────────────────────────────

struct LineLayout {
    text: String,
    width: f32,
}

fn measure_lines(font_size: f32, text: &str) -> Vec<LineLayout> {
    let scale = PxScale::from(font_size);
    let scaled = FONT.as_scaled(scale);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let mut w = 0.0f32;
        for c in raw.chars() {
            let gid = scaled.glyph_id(c);
            w += scaled.h_advance(gid);
        }
        out.push(LineLayout { text: raw.to_string(), width: w });
    }
    out
}

fn generate_text_image(
    text: &str,
    font_size: f32,
    color: [u8; 4],
    bg: Option<[u8; 4]>,
    align: Align,
    pad_x: u32,
    pad_y: u32,
    forced_w: Option<u32>,
    forced_h: Option<u32>,
) -> Arc<ImageData> {
    let scale = PxScale::from(font_size.max(1.0));
    let scaled = FONT.as_scaled(scale);
    let line_h = scaled.height();
    let ascent = scaled.ascent();

    let lines = measure_lines(font_size, text);
    let text_w = lines.iter().fold(0.0f32, |a, l| a.max(l.width)).ceil() as u32;
    let text_h = (line_h * lines.len().max(1) as f32).ceil() as u32;

    let out_w = forced_w.unwrap_or(text_w + 2 * pad_x).max(1);
    let out_h = forced_h.unwrap_or(text_h + 2 * pad_y).max(1);

    let mut pixels = alloc_pixel_buf(out_w, out_h);
    {
        let dst_u32: &mut [u32] = bytemuck::cast_slice_mut(&mut pixels);
        let fill = match bg {
            Some(c) => pack_rgba(c[0], c[1], c[2], c[3]),
            None    => 0,
        };
        for px in dst_u32.iter_mut() { *px = fill; }
    }

    // Center the text block vertically inside the (possibly oversized) output.
    let block_top = if forced_h.is_some() {
        ((out_h as f32 - text_h as f32) * 0.5).max(0.0)
    } else {
        pad_y as f32
    };

    for (i, line) in lines.iter().enumerate() {
        let baseline_y = block_top + ascent + i as f32 * line_h;

        let line_x = match align {
            Align::Left   => pad_x as f32,
            Align::Center => ((out_w as f32 - line.width) * 0.5).max(0.0),
            Align::Right  => (out_w as f32 - pad_x as f32 - line.width).max(0.0),
        };

        let mut pen_x = line_x;
        for c in line.text.chars() {
            let gid = scaled.glyph_id(c);
            let advance = scaled.h_advance(gid);
            let glyph = gid.with_scale_and_position(scale, point(pen_x, baseline_y));
            if let Some(outlined) = FONT.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                let ox = bounds.min.x.floor() as i32;
                let oy = bounds.min.y.floor() as i32;
                outlined.draw(|gx, gy, coverage| {
                    let px = ox + gx as i32;
                    let py = oy + gy as i32;
                    if px < 0 || py < 0 || px >= out_w as i32 || py >= out_h as i32 { return; }
                    let idx = ((py as u32 * out_w + px as u32) * 4) as usize;
                    blend_pixel(&mut pixels, idx, color, coverage);
                });
            }
            pen_x += advance;
        }
    }

    Arc::new(ImageData::new(out_w, out_h, pixels))
}

// ── NodeBehavior ─────────────────────────────────────────────────────────────

impl NodeBehavior for TextToImageNode {
    fn title(&self)      -> &str   { "Text to Image" }
    fn type_tag(&self)   -> &str   { "text_to_image" }
    fn color_hint(&self) -> [u8;3] { [140, 200, 240] }
    fn min_width(&self)  -> Option<f32> { Some(240.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Text",   PortKind::Text),
            PortDef::new("Width",  PortKind::Number),
            PortDef::new("Height", PortKind::Number),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Image", PortKind::Image)]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        if let Some(ref img) = self.cached_image {
            vec![(0, PortValue::Image(img.clone()))]
        } else {
            vec![]
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim  = egui::Color32::from_rgb(110, 110, 120);
        let blue = egui::Color32::from_rgb(100, 180, 240);

        // ── Text input row (port + inline editor fallback) ───────────────
        let text_connected = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
        let upstream_text: Option<String> = if text_connected {
            match Graph::static_input_value(ctx.connections, ctx.values, node_id, 0) {
                PortValue::Text(t) => Some(t),
                _ => None,
            }
        } else { None };

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects, PortKind::Text);
            let lbl = if text_connected { "Text ✓" } else { "Text" };
            ui.label(RichText::new(lbl).small().color(if text_connected { blue } else { dim }));
        });

        if !text_connected {
            let resp = ui.add(
                egui::TextEdit::multiline(&mut self.text)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .hint_text("Type text…")
            );
            if resp.changed() { self.dirty = true; }
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Auto / Fixed size toggle ─────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Size").small().color(dim));
            let auto = self.auto_size;
            if ui.selectable_label(auto, RichText::new("Auto").small()).clicked() && !auto {
                self.auto_size = true;
                self.dirty = true;
            }
            if ui.selectable_label(!auto, RichText::new("Fixed").small()).clicked() && auto {
                self.auto_size = false;
                self.dirty = true;
            }
        });

        // ── Width / Height (only meaningful when !auto_size) ─────────────
        let w_connected = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);
        let h_connected = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 2);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 1, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(RichText::new("W").small().color(dim));
            if !w_connected {
                let mut w = self.default_width as f32;
                let resp = ui.add_enabled(!self.auto_size,
                    egui::DragValue::new(&mut w).speed(1.0).range(1.0..=4096.0).max_decimals(0));
                if resp.changed() {
                    self.default_width = w as u32;
                    self.dirty = true;
                }
            } else {
                let v = Graph::static_input_value(ctx.connections, ctx.values, node_id, 1);
                if let PortValue::Float(f) = v {
                    ui.label(RichText::new(format!("{:.0}", f)).small().monospace());
                }
            }

            ui.add_space(8.0);
            crate::nodes::inline_port_circle(ui, node_id, 2, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(RichText::new("H").small().color(dim));
            if !h_connected {
                let mut h = self.default_height as f32;
                let resp = ui.add_enabled(!self.auto_size,
                    egui::DragValue::new(&mut h).speed(1.0).range(1.0..=4096.0).max_decimals(0));
                if resp.changed() {
                    self.default_height = h as u32;
                    self.dirty = true;
                }
            } else {
                let v = Graph::static_input_value(ctx.connections, ctx.values, node_id, 2);
                if let PortValue::Float(f) = v {
                    ui.label(RichText::new(format!("{:.0}", f)).small().monospace());
                }
            }
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Style: font size + glyph color ───────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Size").small().color(dim));
            if ui.add(egui::DragValue::new(&mut self.font_size)
                .speed(0.5).range(4.0..=512.0).suffix("px").max_decimals(1)).changed() {
                self.dirty = true;
            }
            ui.add_space(8.0);
            ui.label(RichText::new("Color").small().color(dim));
            let mut c = egui::Color32::from_rgba_unmultiplied(
                self.color[0], self.color[1], self.color[2], self.color[3]);
            if ui.color_edit_button_srgba(&mut c).changed() {
                let a = c.to_array();
                self.color = [a[0], a[1], a[2], a[3]];
                self.dirty = true;
            }
        });

        // ── Alignment ────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Align").small().color(dim));
            for (a, lbl) in [(Align::Left, "Left"), (Align::Center, "Center"), (Align::Right, "Right")] {
                let sel = self.align == a;
                if ui.selectable_label(sel, RichText::new(lbl).small()).clicked() && !sel {
                    self.align = a;
                    self.dirty = true;
                }
            }
        });

        // ── Padding ──────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Pad X").small().color(dim));
            let mut px = self.pad_x as f32;
            if ui.add(egui::DragValue::new(&mut px).speed(0.5).range(0.0..=512.0).max_decimals(0)).changed() {
                self.pad_x = px as u32;
                self.dirty = true;
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Pad Y").small().color(dim));
            let mut py = self.pad_y as f32;
            if ui.add(egui::DragValue::new(&mut py).speed(0.5).range(0.0..=512.0).max_decimals(0)).changed() {
                self.pad_y = py as u32;
                self.dirty = true;
            }
        });

        // ── Background ───────────────────────────────────────────────────
        ui.horizontal(|ui| {
            if ui.checkbox(&mut self.bg_enabled, RichText::new("Background").small().color(dim)).changed() {
                self.dirty = true;
            }
            if self.bg_enabled {
                let mut c = egui::Color32::from_rgba_unmultiplied(
                    self.bg_color[0], self.bg_color[1], self.bg_color[2], self.bg_color[3]);
                if ui.color_edit_button_srgba(&mut c).changed() {
                    let a = c.to_array();
                    self.bg_color = [a[0], a[1], a[2], a[3]];
                    self.dirty = true;
                }
            }
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Resolve effective text + dirty-on-upstream-change ────────────
        let effective_text = upstream_text.clone().unwrap_or_else(|| self.text.clone());
        if effective_text != self.last_rendered_text {
            self.dirty = true;
        }

        // ── Compute output size ──────────────────────────────────────────
        let w_override = if !self.auto_size && w_connected {
            match Graph::static_input_value(ctx.connections, ctx.values, node_id, 1) {
                PortValue::Float(f) => Some((f as u32).max(1).min(4096)),
                _ => None,
            }
        } else { None };
        let h_override = if !self.auto_size && h_connected {
            match Graph::static_input_value(ctx.connections, ctx.values, node_id, 2) {
                PortValue::Float(f) => Some((f as u32).max(1).min(4096)),
                _ => None,
            }
        } else { None };

        let (forced_w, forced_h) = if self.auto_size {
            (None, None)
        } else {
            (Some(w_override.unwrap_or(self.default_width)),
             Some(h_override.unwrap_or(self.default_height)))
        };

        let prev_size = self.cached_image.as_ref().map(|i| (i.width, i.height));
        let target_w_h = if self.auto_size { None } else { Some((forced_w.unwrap(), forced_h.unwrap())) };
        let size_changed = match (prev_size, target_w_h) {
            (Some((w, h)), Some((tw, th))) => w != tw || h != th,
            (None, _) => true,
            _ => false,
        };

        if self.dirty || size_changed {
            let bg = if self.bg_enabled { Some(self.bg_color) } else { None };
            self.cached_image = Some(generate_text_image(
                &effective_text, self.font_size, self.color, bg,
                self.align, self.pad_x, self.pad_y, forced_w, forced_h,
            ));
            self.last_rendered_text = effective_text;
            self.dirty = false;
        }

        // ── Size info ────────────────────────────────────────────────────
        if let Some(ref img) = self.cached_image {
            ui.label(RichText::new(format!("{}×{}", img.width, img.height)).small().color(dim));
        }

        // ── Output port row ──────────────────────────────────────────────
        crate::nodes::output_port_row(ui, "Image",
            if self.cached_image.is_some() { "✓" } else { "none" }, node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Image);
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "text": self.text,
            "font_size": self.font_size,
            "color": self.color,
            "bg_enabled": self.bg_enabled,
            "bg_color": self.bg_color,
            "align": match self.align {
                Align::Left => "left",
                Align::Center => "center",
                Align::Right => "right",
            },
            "pad_x": self.pad_x,
            "pad_y": self.pad_y,
            "auto_size": self.auto_size,
            "default_width": self.default_width,
            "default_height": self.default_height,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(v) = state.get("text").and_then(|v| v.as_str())     { self.text = v.to_string(); }
        if let Some(v) = state.get("font_size").and_then(|v| v.as_f64()) { self.font_size = v as f32; }
        if let Some(c) = state.get("color").and_then(|v| v.as_array()) {
            if c.len() >= 4 {
                self.color = [
                    c[0].as_u64().unwrap_or(255) as u8,
                    c[1].as_u64().unwrap_or(255) as u8,
                    c[2].as_u64().unwrap_or(255) as u8,
                    c[3].as_u64().unwrap_or(255) as u8,
                ];
            }
        }
        if let Some(v) = state.get("bg_enabled").and_then(|v| v.as_bool()) { self.bg_enabled = v; }
        if let Some(c) = state.get("bg_color").and_then(|v| v.as_array()) {
            if c.len() >= 4 {
                self.bg_color = [
                    c[0].as_u64().unwrap_or(0) as u8,
                    c[1].as_u64().unwrap_or(0) as u8,
                    c[2].as_u64().unwrap_or(0) as u8,
                    c[3].as_u64().unwrap_or(160) as u8,
                ];
            }
        }
        if let Some(v) = state.get("align").and_then(|v| v.as_str()) {
            self.align = match v {
                "center" => Align::Center,
                "right"  => Align::Right,
                _        => Align::Left,
            };
        }
        if let Some(v) = state.get("pad_x").and_then(|v| v.as_u64()) { self.pad_x = v as u32; }
        if let Some(v) = state.get("pad_y").and_then(|v| v.as_u64()) { self.pad_y = v as u32; }
        if let Some(v) = state.get("auto_size").and_then(|v| v.as_bool()) { self.auto_size = v; }
        if let Some(v) = state.get("default_width").and_then(|v| v.as_u64())  { self.default_width = v as u32; }
        if let Some(v) = state.get("default_height").and_then(|v| v.as_u64()) { self.default_height = v as u32; }
        self.dirty = true;
    }
}

// ── Registration ─────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("text_to_image", |state| {
        let mut n = TextToImageNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
