//! CropNode — polymorphic crop/trim node.
//! Image: crop margins (top/left/bottom/right as 0-1 fractions)
//! Text: substring extraction (start/end as 0-1 fractions)

use crate::graph::{PortDef, PortKind, PortValue, ImageData, Graph};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CropNode {
    #[serde(default)]
    pub top: f32,
    #[serde(default)]
    pub left: f32,
    #[serde(default)]
    pub bottom: f32,
    #[serde(default)]
    pub right: f32,
}

impl Default for CropNode {
    fn default() -> Self {
        Self { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 }
    }
}

impl CropNode {
    fn crop_image(&self, img: &ImageData) -> Arc<ImageData> {
        let w = img.width;
        let h = img.height;
        let x0 = (self.left * w as f32) as u32;
        let y0 = (self.top * h as f32) as u32;
        let x1 = w.saturating_sub((self.right * w as f32) as u32);
        let y1 = h.saturating_sub((self.bottom * h as f32) as u32);
        let out_w = x1.saturating_sub(x0).max(1);
        let out_h = y1.saturating_sub(y0).max(1);

        // u32-granular copy. Each output pixel is a single load/store
        // instead of a 4-byte `copy_from_slice` call — crucial on live
        // camera paths where the 4-byte-slice overhead + bounds checks
        // otherwise dominate in debug builds.
        let out_bytes = (out_w * out_h * 4) as usize;
        let mut pixels: Vec<u8> = Vec::with_capacity(out_bytes);
        // SAFETY: every output pixel is written below — either from the
        // copied row or by the zero-fill on rows that fall outside the
        // source.
        unsafe { pixels.set_len(out_bytes); }

        let src: &[u32] = bytemuck::cast_slice(&img.pixels);
        let dst: &mut [u32] = bytemuck::cast_slice_mut(&mut pixels);
        let w_in = img.width as usize;
        let out_w_us = out_w as usize;

        for y in 0..out_h {
            let dst_row_start = y as usize * out_w_us;
            let src_y = y0 + y;
            if src_y >= h {
                // Below source — zero-fill the row so the buffer stays init.
                for i in 0..out_w_us {
                    dst[dst_row_start + i] = 0;
                }
                continue;
            }
            let src_row_start = src_y as usize * w_in;
            // Clamp how many columns are actually inside the source.
            let copyable = (w.saturating_sub(x0)).min(out_w) as usize;
            let src_col_start = x0 as usize;

            // Fast path: whole-row slice copy via u32.
            dst[dst_row_start .. dst_row_start + copyable]
                .copy_from_slice(&src[src_row_start + src_col_start .. src_row_start + src_col_start + copyable]);

            // Right-edge padding (if out_w > copyable) → zero.
            for i in copyable..out_w_us {
                dst[dst_row_start + i] = 0;
            }
        }
        Arc::new(ImageData { width: out_w, height: out_h, pixels })
    }

    fn crop_text(&self, text: &str) -> String {
        let len = text.len();
        let start = (self.left * len as f32) as usize;
        let end = len.saturating_sub((self.right * len as f32) as usize);
        if start < end { text[start..end].to_string() } else { String::new() }
    }
}

impl NodeBehavior for CropNode {
    fn title(&self) -> &str { "Crop" }
    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Input", PortKind::Image),
            PortDef::new("Top", PortKind::Normalized),
            PortDef::new("Left", PortKind::Normalized),
            PortDef::new("Bottom", PortKind::Normalized),
            PortDef::new("Right", PortKind::Normalized),
        ]
    }
    fn outputs(&self) -> Vec<PortDef> { vec![PortDef::new("Cropped", PortKind::Image)] }
    fn color_hint(&self) -> [u8; 3] { [160, 140, 200] }
    fn inline_ports(&self) -> bool { true }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Override margins from ports
        if let Some(PortValue::Float(v)) = inputs.get(1) { self.top = v.clamp(0.0, 0.95); }
        if let Some(PortValue::Float(v)) = inputs.get(2) { self.left = v.clamp(0.0, 0.95); }
        if let Some(PortValue::Float(v)) = inputs.get(3) { self.bottom = v.clamp(0.0, 0.95); }
        if let Some(PortValue::Float(v)) = inputs.get(4) { self.right = v.clamp(0.0, 0.95); }

        let result = match inputs.first() {
            Some(PortValue::Image(img)) => PortValue::Image(self.crop_image(img)),
            Some(PortValue::Text(text)) => PortValue::Text(self.crop_text(text)),
            // GpuImage should never reach here in practice — the image-eval
            // dispatch in app/mod.rs reads it back to a CPU `Image` first.
            // If it does, return None rather than the previous silent
            // identity passthrough (`other.clone()`) which would have made
            // the crop sliders look broken to the user.
            Some(PortValue::GpuImage(_)) => PortValue::None,
            Some(other) => other.clone(),
            None => PortValue::None,
        };
        vec![(0, result)]
    }

    fn type_tag(&self) -> &str { "crop" }
    fn save_state(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or_default() }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<CropNode>(state.clone()) { *self = l; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let accent = ui.visuals().hyperlink_color;
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // Input port
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Image);
            ui.label(egui::RichText::new("Input").small());
        });

        // Preview crop region. Accept dimensions from either CPU `Image`
        // or `GpuImage` so the preview rect renders for GPU-resident chains.
        let input_val = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0);
        let img_dims: Option<(u32, u32)> = match &input_val {
            PortValue::Image(img) => Some((img.width, img.height)),
            PortValue::GpuImage(h) => Some((h.width, h.height)),
            _ => None,
        };
        if let Some((iw, ih)) = img_dims {
            let max_w = ui.available_width().min(200.0);
            let aspect = ih as f32 / iw as f32;
            let preview_h = max_w * aspect;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(max_w, preview_h), egui::Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 100));
            let crop_rect = egui::Rect::from_min_max(
                egui::pos2(rect.left() + rect.width() * self.left, rect.top() + rect.height() * self.top),
                egui::pos2(rect.right() - rect.width() * self.right, rect.bottom() - rect.height() * self.bottom),
            );
            painter.rect_filled(crop_rect, 0.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40));
            painter.rect_stroke(crop_rect, 0.0, egui::Stroke::new(1.5, accent), egui::StrokeKind::Outside);

            let out_w = ((1.0 - self.left - self.right).max(0.01) * iw as f32) as u32;
            let out_h = ((1.0 - self.top - self.bottom).max(0.01) * ih as f32) as u32;
            ui.label(egui::RichText::new(format!("{}×{} → {}×{}", iw, ih, out_w, out_h)).small().color(dim));
        } else if let PortValue::Text(text) = &input_val {
            let start = (self.left * text.len() as f32) as usize;
            let end = text.len().saturating_sub((self.right * text.len() as f32) as usize);
            ui.label(egui::RichText::new(format!("Text: {} chars → {}", text.len(), end.saturating_sub(start))).small().color(dim));
        }

        ui.separator();

        // Margin sliders
        let mut slider_row = |ui: &mut egui::Ui, label: &str, val: &mut f32, port: usize| {
            let wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == port);
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(ui, ctx.node_id, port, true, ctx.connections,
                    ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Normalized);
                if wired {
                    ui.label(egui::RichText::new(format!("{}: {:.0}%", label, *val * 100.0)).small().color(accent));
                } else {
                    ui.label(egui::RichText::new(format!("{}:", label)).small());
                    ui.add(egui::Slider::new(val, 0.0..=0.95).show_value(false));
                    ui.label(egui::RichText::new(format!("{:.0}%", *val * 100.0)).small());
                }
            });
        };
        slider_row(ui, "Top", &mut self.top, 1);
        slider_row(ui, "Left", &mut self.left, 2);
        slider_row(ui, "Bottom", &mut self.bottom, 3);
        slider_row(ui, "Right", &mut self.right, 4);

        // Clamp
        if self.top + self.bottom > 0.95 { self.bottom = 0.95 - self.top; }
        if self.left + self.right > 0.95 { self.right = 0.95 - self.left; }

        ui.separator();
        crate::nodes::output_port_row(ui, "Cropped", "", ctx.node_id, 0, ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Image);
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("crop", |state| {
        if let Ok(n) = serde_json::from_value::<CropNode>(state.clone()) { Box::new(n) }
        else { Box::new(CropNode::default()) }
    });
}
