use eframe::egui::{self, RichText, Sense};
use std::sync::Arc;
use std::time::Instant;
use crate::graph::{ImageData, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── Image Scanner Node ───────────────────────────────────────────────────────
//
// Sweeps across an image reading pixel values as numeric signals.
// Two modes: Point (single pixel) or Line (average column/row).
// Two directions: Horizontal (sweep X) or Vertical (sweep Y).

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum ScanMode { Point, Line }

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum ScanDir { Horizontal, Vertical }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImageScannerNode {
    mode: ScanMode,
    direction: ScanDir,
    speed: f32,
    phase: f32,
    y_pos: f32,
    running: bool,
    #[serde(skip, default = "Instant::now")]
    last_instant: Instant,
    #[serde(skip)]
    out_r: f32,
    #[serde(skip)]
    out_g: f32,
    #[serde(skip)]
    out_b: f32,
    #[serde(skip)]
    out_bright: f32,
    #[serde(skip)]
    cached_annotated: Option<Arc<ImageData>>,
}

impl Default for ImageScannerNode {
    fn default() -> Self {
        Self {
            mode: ScanMode::Point,
            direction: ScanDir::Horizontal,
            speed: 1.0,
            phase: 0.0,
            y_pos: 0.5,
            running: true,
            last_instant: Instant::now(),
            out_r: 0.0, out_g: 0.0, out_b: 0.0, out_bright: 0.0,
            cached_annotated: None,
        }
    }
}

// ── Pixel helpers ────────────────────────────────────────────────────────────

fn read_pixel(img: &ImageData, x: u32, y: u32) -> (f32, f32, f32) {
    let x = x.min(img.width.saturating_sub(1));
    let y = y.min(img.height.saturating_sub(1));
    let i = ((y * img.width + x) * 4) as usize;
    if i + 2 < img.pixels.len() {
        (img.pixels[i] as f32 / 255.0, img.pixels[i+1] as f32 / 255.0, img.pixels[i+2] as f32 / 255.0)
    } else { (0.0, 0.0, 0.0) }
}

fn average_column(img: &ImageData, x: u32) -> (f32, f32, f32) {
    let x = x.min(img.width.saturating_sub(1));
    let h = img.height.max(1);
    let (mut sr, mut sg, mut sb) = (0.0f32, 0.0f32, 0.0f32);
    for y in 0..h {
        let i = ((y * img.width + x) * 4) as usize;
        if i + 2 < img.pixels.len() {
            sr += img.pixels[i] as f32; sg += img.pixels[i+1] as f32; sb += img.pixels[i+2] as f32;
        }
    }
    let n = h as f32 * 255.0;
    (sr / n, sg / n, sb / n)
}

fn average_row(img: &ImageData, y: u32) -> (f32, f32, f32) {
    let y = y.min(img.height.saturating_sub(1));
    let w = img.width.max(1);
    let (mut sr, mut sg, mut sb) = (0.0f32, 0.0f32, 0.0f32);
    for x in 0..w {
        let i = ((y * img.width + x) * 4) as usize;
        if i + 2 < img.pixels.len() {
            sr += img.pixels[i] as f32; sg += img.pixels[i+1] as f32; sb += img.pixels[i+2] as f32;
        }
    }
    let n = w as f32 * 255.0;
    (sr / n, sg / n, sb / n)
}

/// Draw a full-length line (vertical for H-sweep, horizontal for V-sweep)
fn draw_full_line(pixels: &mut [u8], w: u32, h: u32, pos: u32, vertical: bool, r: u8, g: u8, b: u8) {
    if vertical {
        let x = pos.min(w.saturating_sub(1));
        for y in 0..h {
            let i = ((y * w + x) * 4) as usize;
            if i + 3 < pixels.len() { pixels[i] = r; pixels[i+1] = g; pixels[i+2] = b; pixels[i+3] = 255; }
        }
        // 3px wide
        for dx in [x.wrapping_sub(1), x + 1] {
            if dx < w {
                for y in 0..h {
                    let i = ((y * w + dx) * 4) as usize;
                    if i + 3 < pixels.len() { pixels[i] = r/2; pixels[i+1] = g/2; pixels[i+2] = b/2; pixels[i+3] = 180; }
                }
            }
        }
    } else {
        let y = pos.min(h.saturating_sub(1));
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            if i + 3 < pixels.len() { pixels[i] = r; pixels[i+1] = g; pixels[i+2] = b; pixels[i+3] = 255; }
        }
        for dy in [y.wrapping_sub(1), y + 1] {
            if dy < h {
                for x in 0..w {
                    let i = ((dy * w + x) * 4) as usize;
                    if i + 3 < pixels.len() { pixels[i] = r/2; pixels[i+1] = g/2; pixels[i+2] = b/2; pixels[i+3] = 180; }
                }
            }
        }
    }
}

// ── NodeBehavior ──────────────────────────────────────────────────────────────

impl NodeBehavior for ImageScannerNode {
    fn title(&self)      -> &str   { "Image Scanner" }
    fn type_tag(&self)   -> &str   { "image_scanner" }
    fn color_hint(&self) -> [u8;3] { [100, 200, 220] }
    fn min_width(&self)  -> Option<f32> { Some(220.0) }
    fn inline_ports(&self) -> bool { true }
    fn needs_cpu_image_input(&self, _port: usize) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Image", PortKind::Image),
            PortDef::new("X",     PortKind::Normalized),
            PortDef::new("Y",     PortKind::Normalized),
            PortDef::new("Speed", PortKind::Number),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Annotated",  PortKind::Image),
            PortDef::new("R",          PortKind::Normalized),
            PortDef::new("G",          PortKind::Normalized),
            PortDef::new("B",          PortKind::Normalized),
            PortDef::new("Brightness", PortKind::Normalized),
            PortDef::new("Phase",      PortKind::Normalized),
        ]
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Phase is advanced in render_with_context (runs every frame).
        // evaluate() just reads pixel values at current phase.

        let x_wired = matches!(inputs.get(1), Some(PortValue::Float(_)));
        let y_wired = matches!(inputs.get(2), Some(PortValue::Float(_)));

        if let Some(PortValue::Float(s)) = inputs.get(3) { self.speed = *s; }
        if x_wired { if let Some(PortValue::Float(v)) = inputs.get(1) { self.phase = v.clamp(0.0, 1.0); } }
        if y_wired { if let Some(PortValue::Float(v)) = inputs.get(2) { self.y_pos = v.clamp(0.0, 1.0); } }

        let img = match inputs.first() {
            Some(PortValue::Image(img)) => img,
            _ => {
                return vec![
                    (1, PortValue::Float(0.0)), (2, PortValue::Float(0.0)),
                    (3, PortValue::Float(0.0)), (4, PortValue::Float(0.0)),
                    (5, PortValue::Float(self.phase)),
                ];
            }
        };

        let w = img.width;
        let h = img.height;
        if w == 0 || h == 0 {
            return vec![(5, PortValue::Float(self.phase))];
        }

        // Read pixel values
        // Point mode: raster scan — phase covers entire image (row by row)
        //   pixel_index = phase * (w * h)
        //   col = pixel_index % w,  row = pixel_index / w
        // Line mode: single axis sweep (unchanged)
        let (px, py, r, g, b) = match self.mode {
            ScanMode::Point => {
                let total = (w * h) as f32;
                let idx = (self.phase * total).floor().min(total - 1.0) as u32;
                let px = idx % w;
                let py = idx / w;
                // Update y_pos to reflect current row (for display)
                self.y_pos = py as f32 / h.max(1) as f32;
                let (r, g, b) = read_pixel(img, px, py);
                (px, py, r, g, b)
            }
            ScanMode::Line => {
                let (r, g, b) = match self.direction {
                    ScanDir::Horizontal => average_column(img, (self.phase * w as f32).floor() as u32),
                    ScanDir::Vertical   => average_row(img, (self.phase * h as f32).floor() as u32),
                };
                let px = (self.phase * w as f32).floor().min((w - 1) as f32) as u32;
                let py = (self.phase * h as f32).floor().min((h - 1) as f32) as u32;
                (px, py, r, g, b)
            }
        };

        self.out_r = r;
        self.out_g = g;
        self.out_b = b;
        self.out_bright = (r + g + b) / 3.0;

        // Generate annotated image
        let mut ann = img.pixels.clone();

        match self.mode {
            ScanMode::Point => {
                // Two full-length crossing lines at current raster position
                draw_full_line(&mut ann, w, h, px, true, 0, 255, 255);   // vertical
                draw_full_line(&mut ann, w, h, py, false, 0, 255, 255);  // horizontal
            }
            ScanMode::Line => {
                let is_vertical = self.direction == ScanDir::Horizontal;
                let pos = if is_vertical { px } else { py };
                draw_full_line(&mut ann, w, h, pos, is_vertical, 0, 255, 255);
            }
        }

        let annotated = Arc::new(ImageData::new(w, h, ann));
        self.cached_annotated = Some(annotated.clone());

        vec![
            (0, PortValue::Image(annotated)),
            (1, PortValue::Float(self.out_r)),
            (2, PortValue::Float(self.out_g)),
            (3, PortValue::Float(self.out_b)),
            (4, PortValue::Float(self.out_bright)),
            (5, PortValue::Float(self.phase)),
        ]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim  = egui::Color32::from_rgb(110, 110, 120);
        let blue = egui::Color32::from_rgb(100, 180, 240);

        // ── Advance phase per-pixel (runs every frame) ──────────────────
        // Point mode: phase covers w×h pixels (raster scan, line by line)
        // Line mode: phase covers w or h pixels (single axis)
        // Speed=1.0 → 1 pixel per frame.
        let x_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);
        if self.running && !x_wired {
            let input_val = crate::graph::Graph::static_input_value(
                ctx.connections, ctx.values, node_id, 0);
            let total_pixels = match &input_val {
                PortValue::Image(img) => match self.mode {
                    ScanMode::Point => (img.width.max(1) * img.height.max(1)) as f32,
                    ScanMode::Line => match self.direction {
                        ScanDir::Horizontal => img.width.max(1) as f32,
                        ScanDir::Vertical   => img.height.max(1) as f32,
                    },
                },
                _ => 256.0,
            };
            let step = self.speed / total_pixels;
            self.phase += step;
            if self.phase >= 1.0 { self.phase = self.phase.fract(); }
            if self.phase < 0.0 { self.phase = 0.0; }
        }

        let wired: Vec<bool> = (0..4).map(|p|
            ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == p)
        ).collect();

        // ── Input ports ──────────────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Image);
            ui.label(RichText::new(if wired[0] { "Image ✓" } else { "Image" }).small()
                .color(if wired[0] { blue } else { dim }));
        });

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 1, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Normalized);
            ui.label(RichText::new("X").small().color(dim));
            if wired[1] {
                ui.label(RichText::new(format!("{:.2}", self.phase)).small().monospace().color(blue));
            } else {
                ui.add(egui::DragValue::new(&mut self.phase).speed(0.005).range(0.0..=1.0).max_decimals(3));
            }
            ui.add_space(8.0);
            crate::nodes::inline_port_circle(ui, node_id, 2, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Normalized);
            ui.label(RichText::new("Y").small().color(dim));
            if wired[2] {
                ui.label(RichText::new(format!("{:.2}", self.y_pos)).small().monospace().color(blue));
            } else {
                ui.add(egui::DragValue::new(&mut self.y_pos).speed(0.005).range(0.0..=1.0).max_decimals(3));
            }
        });

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 3, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(RichText::new("Speed").small().color(dim));
            if !wired[3] {
                ui.add(egui::DragValue::new(&mut self.speed).speed(0.1).range(0.1..=50.0).max_decimals(1));
            } else {
                ui.label(RichText::new(format!("{:.2}", self.speed)).small().monospace().color(blue));
            }
        });

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Controls ─────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            if ui.small_button(if self.running { "⏸" } else { "▶" }).clicked() { self.running = !self.running; }
            if ui.small_button("↺").clicked() { self.phase = 0.0; }
            ui.add_space(4.0);
            if ui.selectable_label(self.mode == ScanMode::Point, RichText::new("Point").small()).clicked() { self.mode = ScanMode::Point; }
            if ui.selectable_label(self.mode == ScanMode::Line, RichText::new("Line").small()).clicked() { self.mode = ScanMode::Line; }
            ui.add_space(4.0);
            if ui.selectable_label(self.direction == ScanDir::Horizontal, RichText::new("H").small()).clicked() { self.direction = ScanDir::Horizontal; }
            if ui.selectable_label(self.direction == ScanDir::Vertical, RichText::new("V").small()).clicked() { self.direction = ScanDir::Vertical; }
        });

        ui.add_space(2.0);

        // ── Annotated image preview inside the node ──────────────────────
        if let Some(ref ann_img) = self.cached_annotated {
            let preview_w = ui.available_width().max(100.0).min(220.0);
            let aspect = ann_img.height as f32 / ann_img.width.max(1) as f32;
            let preview_h = (preview_w * aspect).min(160.0);
            let tex_id = egui::Id::new(("scanner_preview_tex", node_id));

            let texture = ui.ctx().data_mut(|d| {
                d.get_temp::<egui::TextureHandle>(tex_id)
            });

            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [ann_img.width as usize, ann_img.height as usize],
                &ann_img.pixels,
            );

            let tex = if let Some(mut existing) = texture {
                existing.set(color_image, egui::TextureOptions::LINEAR);
                existing
            } else {
                let new_tex = ui.ctx().load_texture(
                    format!("scanner_preview_{}", node_id),
                    color_image,
                    egui::TextureOptions::LINEAR,
                );
                new_tex
            };

            ui.ctx().data_mut(|d| d.insert_temp(tex_id, tex.clone()));

            let (rect, _) = ui.allocate_exact_size(egui::vec2(preview_w, preview_h), Sense::hover());
            ui.painter().image(
                tex.id(), rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }

        ui.add_space(2.0);

        // ── Live values ──────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("R {:.0}%", self.out_r * 100.0)).small()
                .color(egui::Color32::from_rgb(255, 80, 80)));
            ui.label(RichText::new(format!("G {:.0}%", self.out_g * 100.0)).small()
                .color(egui::Color32::from_rgb(80, 220, 80)));
            ui.label(RichText::new(format!("B {:.0}%", self.out_b * 100.0)).small()
                .color(egui::Color32::from_rgb(80, 120, 255)));
            ui.label(RichText::new(format!("☀ {:.0}%", self.out_bright * 100.0)).small());
        });

        // Phase bar
        let bar_w = ui.available_width().max(60.0);
        let (bar_rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, 6.0), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(bar_rect, 2.0, egui::Color32::from_rgb(30, 32, 38));
        let fill_w = bar_rect.width() * self.phase.clamp(0.0, 1.0);
        if fill_w > 0.5 {
            let fill = egui::Rect::from_min_size(bar_rect.min, egui::vec2(fill_w, bar_rect.height()));
            painter.rect_filled(fill, 2.0, egui::Color32::from_rgb(0, 200, 220));
        }

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Output ports ─────────────────────────────────────────────────
        crate::nodes::output_port_row(ui, "Annotated", if self.cached_annotated.is_some() { "✓" } else { "—" }, node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Image);
        crate::nodes::output_port_row(ui, "R", &format!("{:.2}", self.out_r), node_id, 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Normalized);
        crate::nodes::output_port_row(ui, "G", &format!("{:.2}", self.out_g), node_id, 2,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Normalized);
        crate::nodes::output_port_row(ui, "B", &format!("{:.2}", self.out_b), node_id, 3,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Normalized);
        crate::nodes::output_port_row(ui, "Brightness", &format!("{:.2}", self.out_bright), node_id, 4,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Normalized);
        crate::nodes::output_port_row(ui, "Phase", &format!("{:.2}", self.phase), node_id, 5,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Normalized);

        if self.running {
            ui.ctx().request_repaint();
        }
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(n) = serde_json::from_value::<ImageScannerNode>(state.clone()) { *self = n; }
        self.last_instant = Instant::now();
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("image_scanner", |state| {
        if let Ok(mut n) = serde_json::from_value::<ImageScannerNode>(state.clone()) {
            n.last_instant = Instant::now();
            Box::new(n)
        } else {
            Box::new(ImageScannerNode::default())
        }
    });
}
