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
}

fn default_one() -> f32 { 1.0 }

impl Default for TransformNode {
    fn default() -> Self {
        Self { scale_x: 1.0, scale_y: 1.0, rotation: 0.0, flip_h: false, flip_v: false }
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
        let mut pixels = vec![0u8; row_bytes * h];

        for dy in 0..h {
            let src_y = if self.flip_v { h - 1 - dy } else { dy };
            let src_row = &img.pixels[src_y * row_bytes .. src_y * row_bytes + row_bytes];
            let dst_row = &mut pixels[dy * row_bytes .. dy * row_bytes + row_bytes];
            if self.flip_h {
                // Per-pixel mirror (can't copy_from_slice a reversed view
                // directly, but this is still massively cheaper than the
                // full rotation-capable path — no trig, no div, no bounds
                // checks).
                for dx in 0..w {
                    let src_x = w - 1 - dx;
                    let si = src_x * 4;
                    let di = dx * 4;
                    dst_row[di .. di + 4].copy_from_slice(&src_row[si .. si + 4]);
                }
            } else {
                // Vertical flip only — whole-row copy.
                dst_row.copy_from_slice(src_row);
            }
        }
        Arc::new(ImageData { width: img.width, height: img.height, pixels })
    }

    fn transform_image(&self, img: &ImageData) -> Arc<ImageData> {
        // Fast path for pure flip: no trig, no division, row-granular copy.
        // Identity is handled by the caller (passes the input Arc through),
        // so we only get here for non-identity transforms.
        if self.is_pure_flip() {
            return self.transform_flip_only(img);
        }

        let w = img.width as f32;
        let h = img.height as f32;

        // Output dimensions after scale
        let out_w = ((w * self.scale_x).round().max(1.0)) as u32;
        let out_h = ((h * self.scale_y).round().max(1.0)) as u32;
        let mut pixels = vec![0u8; (out_w * out_h * 4) as usize];

        let angle = self.rotation.to_radians();
        let cos_a = angle.cos();
        let sin_a = angle.sin();
        let cx_out = out_w as f32 * 0.5;
        let cy_out = out_h as f32 * 0.5;
        let cx_in = w * 0.5;
        let cy_in = h * 0.5;
        let inv_sx = 1.0 / self.scale_x;
        let inv_sy = 1.0 / self.scale_y;

        for dy in 0..out_h {
            for dx in 0..out_w {
                // Center-relative output coords
                let ox = dx as f32 - cx_out;
                let oy = dy as f32 - cy_out;

                // Inverse rotation (rotate output point back to source space)
                let rx = ox * cos_a + oy * sin_a;
                let ry = -ox * sin_a + oy * cos_a;

                // Inverse scale (map back to source pixel)
                let mut sx = rx * inv_sx + cx_in;
                let mut sy = ry * inv_sy + cy_in;

                // Flip
                if self.flip_h { sx = w - 1.0 - sx; }
                if self.flip_v { sy = h - 1.0 - sy; }

                // Nearest-neighbor sample
                let ix = sx.round() as i32;
                let iy = sy.round() as i32;

                if ix >= 0 && ix < img.width as i32 && iy >= 0 && iy < img.height as i32 {
                    let si = ((iy as u32 * img.width + ix as u32) * 4) as usize;
                    let di = ((dy * out_w + dx) * 4) as usize;
                    if si + 3 < img.pixels.len() && di + 3 < pixels.len() {
                        pixels[di..di + 4].copy_from_slice(&img.pixels[si..si + 4]);
                    }
                }
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
        if let Some(PortValue::Float(v)) = inputs.get(1) { self.scale_x = v.clamp(0.1, 5.0); }
        if let Some(PortValue::Float(v)) = inputs.get(2) { self.scale_y = v.clamp(0.1, 5.0); }
        if let Some(PortValue::Float(v)) = inputs.get(3) { self.rotation = *v % 360.0; }

        let result = match inputs.first() {
            // Identity passthrough — just clone the Arc, no pixel work.
            // Without this, a live 30/60 fps camera was being run through
            // a 307k-iteration-per-frame rotate/scale path with all-ones
            // parameters, halving framerate downstream.
            Some(PortValue::Image(img)) if self.is_identity() => {
                PortValue::Image(img.clone())
            }
            Some(PortValue::Image(img)) => PortValue::Image(self.transform_image(img)),
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
                ui.add(egui::Slider::new(&mut self.scale_x, 0.1..=5.0).step_by(0.01).show_value(true));
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
                ui.add(egui::Slider::new(&mut self.scale_y, 0.1..=5.0).step_by(0.01).show_value(true));
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
