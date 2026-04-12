use eframe::egui::{self, RichText, Sense};
use std::sync::Arc;
use crate::graph::{ImageData, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── Fill Node ────────────────────────────────────────────────────────────────
//
// Generates images: solid color, linear/radial/diamond gradient, or tiled image.
// Like Figma's fill panel — gradient stops can be added/removed,
// colors picked per stop, angle controlled for linear.
//
// Ports:
//   In  0: Image  (optional — tile source, or match output size)
//   In  1: Width  (optional override)
//   In  2: Height (optional override)
//   Out 0: Image  (generated fill)

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
enum FillType { Solid, Linear, Radial, Diamond, Tile }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct GradientStop {
    position: f32,    // 0.0–1.0
    color: [u8; 4],   // RGBA
}

#[derive(Debug, Clone)]
pub struct FillNode {
    fill_type: FillType,
    color: [u8; 4],
    stops: Vec<GradientStop>,
    angle: f32,
    tile_scale: f32,
    default_width: u32,
    default_height: u32,
    cached_image: Option<Arc<ImageData>>,
    dirty: bool,
}

impl Default for FillNode {
    fn default() -> Self {
        Self {
            fill_type: FillType::Solid,
            color: [255, 255, 255, 255],
            stops: vec![
                GradientStop { position: 0.0, color: [217, 217, 217, 255] },
                GradientStop { position: 1.0, color: [115, 115, 115, 255] },
            ],
            angle: 0.0,
            tile_scale: 1.0,
            default_width: 256,
            default_height: 256,
            cached_image: None,
            dirty: true,
        }
    }
}

// ── Gradient sampling ────────────────────────────────────────────────────────

fn sample_gradient(stops: &[GradientStop], t: f32) -> (u8, u8, u8, u8) {
    if stops.is_empty() { return (0, 0, 0, 255); }
    if stops.len() == 1 {
        let c = &stops[0].color;
        return (c[0], c[1], c[2], c[3]);
    }
    let t = t.clamp(0.0, 1.0);

    // Find the two stops bracketing t
    let mut left = &stops[0];
    let mut right = &stops[stops.len() - 1];
    for i in 0..stops.len() - 1 {
        if t >= stops[i].position && t <= stops[i + 1].position {
            left = &stops[i];
            right = &stops[i + 1];
            break;
        }
    }

    let span = (right.position - left.position).max(0.0001);
    let f = ((t - left.position) / span).clamp(0.0, 1.0);

    let lerp = |a: u8, b: u8| -> u8 {
        (a as f32 + (b as f32 - a as f32) * f).round().clamp(0.0, 255.0) as u8
    };

    (lerp(left.color[0], right.color[0]),
     lerp(left.color[1], right.color[1]),
     lerp(left.color[2], right.color[2]),
     lerp(left.color[3], right.color[3]))
}

// ── Image generation ─────────────────────────────────────────────────────────

fn generate_solid(w: u32, h: u32, color: [u8; 4]) -> Arc<ImageData> {
    Arc::new(ImageData::solid(w, h, color[0], color[1], color[2], color[3]))
}

fn generate_linear(w: u32, h: u32, stops: &[GradientStop], angle_deg: f32) -> Arc<ImageData> {
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let rad = angle_deg.to_radians();
    let cos_a = rad.cos();
    let sin_a = rad.sin();
    for y in 0..h {
        for x in 0..w {
            let nx = x as f32 / w as f32 - 0.5;
            let ny = y as f32 / h as f32 - 0.5;
            let t = (nx * cos_a + ny * sin_a + 0.5).clamp(0.0, 1.0);
            let (r, g, b, a) = sample_gradient(stops, t);
            let i = ((y * w + x) * 4) as usize;
            pixels[i] = r; pixels[i+1] = g; pixels[i+2] = b; pixels[i+3] = a;
        }
    }
    Arc::new(ImageData::new(w, h, pixels))
}

fn generate_radial(w: u32, h: u32, stops: &[GradientStop]) -> Arc<ImageData> {
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let cx = 0.5f32;
    let cy = 0.5f32;
    let max_r = 0.5f32 * std::f32::consts::SQRT_2; // corner distance
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 / w as f32 - cx;
            let dy = y as f32 / h as f32 - cy;
            let t = ((dx * dx + dy * dy).sqrt() / max_r).clamp(0.0, 1.0);
            let (r, g, b, a) = sample_gradient(stops, t);
            let i = ((y * w + x) * 4) as usize;
            pixels[i] = r; pixels[i+1] = g; pixels[i+2] = b; pixels[i+3] = a;
        }
    }
    Arc::new(ImageData::new(w, h, pixels))
}

fn generate_diamond(w: u32, h: u32, stops: &[GradientStop]) -> Arc<ImageData> {
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let dx = (x as f32 / w as f32 - 0.5).abs();
            let dy = (y as f32 / h as f32 - 0.5).abs();
            let t = ((dx + dy) / 0.5).clamp(0.0, 1.0); // Manhattan distance, normalized
            let (r, g, b, a) = sample_gradient(stops, t);
            let i = ((y * w + x) * 4) as usize;
            pixels[i] = r; pixels[i+1] = g; pixels[i+2] = b; pixels[i+3] = a;
        }
    }
    Arc::new(ImageData::new(w, h, pixels))
}

fn generate_tile(w: u32, h: u32, src: &ImageData, scale: f32) -> Arc<ImageData> {
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let scale = scale.max(0.01);
    for y in 0..h {
        for x in 0..w {
            let sx = ((x as f32 * scale) as u32) % src.width;
            let sy = ((y as f32 * scale) as u32) % src.height;
            let si = ((sy * src.width + sx) * 4) as usize;
            let di = ((y * w + x) * 4) as usize;
            if si + 3 < src.pixels.len() {
                pixels[di]   = src.pixels[si];
                pixels[di+1] = src.pixels[si+1];
                pixels[di+2] = src.pixels[si+2];
                pixels[di+3] = src.pixels[si+3];
            }
        }
    }
    Arc::new(ImageData::new(w, h, pixels))
}

// ── NodeBehavior ──────────────────────────────────────────────────────────────

impl NodeBehavior for FillNode {
    fn title(&self)      -> &str   { "Fill" }
    fn type_tag(&self)   -> &str   { "fill" }
    fn color_hint(&self) -> [u8;3] { [200, 140, 240] }
    fn min_width(&self)  -> Option<f32> { Some(220.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Image",  PortKind::Image),
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

        // ── Input ports ──────────────────────────────────────────────────
        // Image input (for Tile source / size matching)
        let img_connected = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects, PortKind::Image);
            let lbl = if img_connected { "Image ✓" } else { "Image" };
            ui.label(RichText::new(lbl).small().color(if img_connected { blue } else { dim }));
        });

        // Width / Height inputs
        let w_connected = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);
        let h_connected = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 2);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 1, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(RichText::new("W").small().color(dim));
            if !w_connected {
                let mut w = self.default_width as f32;
                if ui.add(egui::DragValue::new(&mut w).speed(1.0).range(1.0..=4096.0).max_decimals(0)).changed() {
                    self.default_width = w as u32;
                    self.dirty = true;
                }
            } else {
                let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 1);
                if let PortValue::Float(f) = v { ui.label(RichText::new(format!("{:.0}", f)).small().monospace()); }
            }

            ui.add_space(8.0);
            crate::nodes::inline_port_circle(ui, node_id, 2, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(RichText::new("H").small().color(dim));
            if !h_connected {
                let mut h = self.default_height as f32;
                if ui.add(egui::DragValue::new(&mut h).speed(1.0).range(1.0..=4096.0).max_decimals(0)).changed() {
                    self.default_height = h as u32;
                    self.dirty = true;
                }
            } else {
                let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 2);
                if let PortValue::Float(f) = v { ui.label(RichText::new(format!("{:.0}", f)).small().monospace()); }
            }
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Fill type selector ───────────────────────────────────────────
        ui.horizontal(|ui| {
            let types = [
                (FillType::Solid,   "Solid"),
                (FillType::Linear,  "Linear"),
                (FillType::Radial,  "Radial"),
                (FillType::Diamond, "Diamond"),
                (FillType::Tile,    "Tile"),
            ];
            for (ft, label) in &types {
                let selected = self.fill_type == *ft;
                let btn = ui.selectable_label(selected, RichText::new(*label).small());
                if btn.clicked() && !selected {
                    self.fill_type = ft.clone();
                    self.dirty = true;
                }
            }
        });

        ui.add_space(4.0);

        // ── Fill-type-specific UI ────────────────────────────────────────
        match self.fill_type {
            FillType::Solid => {
                ui.horizontal(|ui| {
                    let mut c = egui::Color32::from_rgba_unmultiplied(
                        self.color[0], self.color[1], self.color[2], self.color[3]);
                    if ui.color_edit_button_srgba(&mut c).changed() {
                        let a = c.to_array();
                        self.color = [a[0], a[1], a[2], a[3]];
                        self.dirty = true;
                    }
                    ui.label(RichText::new(format!("#{:02X}{:02X}{:02X}",
                        self.color[0], self.color[1], self.color[2])).small().monospace());

                    // Opacity
                    let mut opacity = self.color[3] as f32 / 255.0 * 100.0;
                    ui.label(RichText::new("α").small().color(dim));
                    if ui.add(egui::DragValue::new(&mut opacity)
                        .speed(0.5).range(0.0..=100.0).suffix("%").max_decimals(0)).changed() {
                        self.color[3] = (opacity / 100.0 * 255.0).round() as u8;
                        self.dirty = true;
                    }
                });
            }

            FillType::Linear | FillType::Radial | FillType::Diamond => {
                // ── Gradient preview bar ──────────────────────────────────
                let bar_w = ui.available_width().max(100.0);
                let bar_h = 24.0;
                let (bar_rect, _bar_resp) = ui.allocate_exact_size(
                    egui::vec2(bar_w, bar_h), Sense::hover());
                let painter = ui.painter_at(bar_rect);

                // Checkerboard background (for alpha)
                painter.rect_filled(bar_rect, 3.0, egui::Color32::from_rgb(40, 40, 45));

                // Draw gradient strips
                let steps = bar_w as i32;
                for px in 0..steps {
                    let t = px as f32 / (steps - 1).max(1) as f32;
                    let (r, g, b, a) = sample_gradient(&self.stops, t);
                    let x = bar_rect.left() + px as f32;
                    painter.line_segment(
                        [egui::pos2(x, bar_rect.top()), egui::pos2(x, bar_rect.bottom())],
                        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(r, g, b, a)));
                }

                painter.rect_stroke(bar_rect, 3.0,
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 65, 75)),
                    egui::StrokeKind::Outside);

                // Stop markers (small triangles below the bar)
                for stop in &self.stops {
                    let sx = bar_rect.left() + stop.position * bar_rect.width();
                    let sc = egui::Color32::from_rgba_unmultiplied(
                        stop.color[0], stop.color[1], stop.color[2], stop.color[3]);
                    painter.circle_filled(egui::pos2(sx, bar_rect.bottom()), 4.0, sc);
                    painter.circle_stroke(egui::pos2(sx, bar_rect.bottom()), 4.0,
                        egui::Stroke::new(1.0, egui::Color32::WHITE));
                }

                ui.add_space(6.0);

                // ── Angle (Linear only) ──────────────────────────────────
                if self.fill_type == FillType::Linear {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Angle").small().color(dim));
                        if ui.add(egui::DragValue::new(&mut self.angle)
                            .speed(1.0).range(0.0..=360.0).suffix("°").max_decimals(0)).changed() {
                            self.dirty = true;
                        }
                    });
                    ui.add_space(2.0);
                }

                // ── Stops list ────────────────────────────────────────────
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Stops").small().strong().color(dim));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(RichText::new("+").color(egui::Color32::from_rgb(80, 210, 120))).clicked() {
                            // Insert new stop at midpoint
                            let mid = if self.stops.len() >= 2 {
                                let last = self.stops.len() - 1;
                                (self.stops[last - 1].position + self.stops[last].position) / 2.0
                            } else { 0.5 };
                            let (r, g, b, a) = sample_gradient(&self.stops, mid);
                            self.stops.push(GradientStop { position: mid, color: [r, g, b, a] });
                            self.stops.sort_by(|a, b| a.position.partial_cmp(&b.position).unwrap());
                            self.dirty = true;
                        }
                    });
                });

                let mut remove_idx: Option<usize> = None;
                let num_stops = self.stops.len();

                egui::ScrollArea::vertical()
                    .id_salt(egui::Id::new(("fill_stops", node_id)))
                    .max_height(120.0)
                    .show(ui, |ui| {
                        for (i, stop) in self.stops.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                // Position
                                let mut pct = stop.position * 100.0;
                                if ui.add(egui::DragValue::new(&mut pct)
                                    .speed(0.5).range(0.0..=100.0).suffix("%")
                                    .max_decimals(0)).changed() {
                                    stop.position = (pct / 100.0).clamp(0.0, 1.0);
                                }

                                // Color swatch
                                let mut c = egui::Color32::from_rgba_unmultiplied(
                                    stop.color[0], stop.color[1], stop.color[2], stop.color[3]);
                                if ui.color_edit_button_srgba(&mut c).changed() {
                                    let arr = c.to_array();
                                    stop.color = [arr[0], arr[1], arr[2], arr[3]];
                                }

                                // Opacity
                                let mut op = stop.color[3] as f32 / 255.0 * 100.0;
                                if ui.add(egui::DragValue::new(&mut op)
                                    .speed(0.5).range(0.0..=100.0).suffix("%")
                                    .max_decimals(0)).changed() {
                                    stop.color[3] = (op / 100.0 * 255.0).round() as u8;
                                }

                                // Remove button (disabled if ≤2 stops)
                                ui.add_enabled_ui(num_stops > 2, |ui| {
                                    if ui.small_button(RichText::new("−")
                                        .color(egui::Color32::from_rgb(220, 80, 80))).clicked() {
                                        remove_idx = Some(i);
                                    }
                                });
                            });
                        }
                    });

                if let Some(idx) = remove_idx {
                    self.stops.remove(idx);
                    self.dirty = true;
                }

                // Sort stops and mark dirty if any changed
                let sorted: Vec<f32> = self.stops.iter().map(|s| s.position).collect();
                let mut sorted2 = sorted.clone();
                sorted2.sort_by(|a, b| a.partial_cmp(b).unwrap());
                if sorted != sorted2 {
                    self.stops.sort_by(|a, b| a.position.partial_cmp(&b.position).unwrap());
                }
                // Always mark dirty after gradient edits (colors might have changed)
                // The dirty flag is set per-field above; we also catch color changes here
                if ui.ctx().input(|i| i.pointer.any_released()) {
                    self.dirty = true; // catch drag releases on color pickers
                }
            }

            FillType::Tile => {
                if !img_connected {
                    ui.label(RichText::new("Connect an image input").small().color(dim).italics());
                } else {
                    ui.label(RichText::new("Tile source ✓").small()
                        .color(egui::Color32::from_rgb(80, 200, 120)));
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Scale").small().color(dim));
                    if ui.add(egui::DragValue::new(&mut self.tile_scale)
                        .speed(0.05).range(0.1..=10.0).max_decimals(2)).changed() {
                        self.dirty = true;
                    }
                });
            }
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Generate image ───────────────────────────────────────────────
        // Determine output size
        let input_img = if img_connected {
            let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 0);
            if let PortValue::Image(img) = v { Some(img) } else { None }
        } else { None };

        let w_override = if w_connected {
            let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 1);
            if let PortValue::Float(f) = v { Some((f as u32).max(1).min(4096)) } else { None }
        } else { None };
        let h_override = if h_connected {
            let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 2);
            if let PortValue::Float(f) = v { Some((f as u32).max(1).min(4096)) } else { None }
        } else { None };

        let out_w = w_override
            .or_else(|| input_img.as_ref().map(|img| img.width))
            .unwrap_or(self.default_width);
        let out_h = h_override
            .or_else(|| input_img.as_ref().map(|img| img.height))
            .unwrap_or(self.default_height);

        // Regenerate if dirty or size changed
        let size_changed = self.cached_image.as_ref()
            .map(|img| img.width != out_w || img.height != out_h)
            .unwrap_or(true);

        if self.dirty || size_changed {
            self.cached_image = Some(match self.fill_type {
                FillType::Solid   => generate_solid(out_w, out_h, self.color),
                FillType::Linear  => generate_linear(out_w, out_h, &self.stops, self.angle),
                FillType::Radial  => generate_radial(out_w, out_h, &self.stops),
                FillType::Diamond => generate_diamond(out_w, out_h, &self.stops),
                FillType::Tile    => {
                    if let Some(ref src) = input_img {
                        generate_tile(out_w, out_h, src, self.tile_scale)
                    } else {
                        generate_solid(out_w, out_h, [128, 128, 128, 255])
                    }
                }
            });
            self.dirty = false;
        }

        // Size info
        ui.label(RichText::new(format!("{}×{}", out_w, out_h)).small().color(dim));

        // ── Output port ──────────────────────────────────────────────────
        crate::nodes::output_port_row(ui, "Image",
            if self.cached_image.is_some() { "✓" } else { "none" }, node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Image);
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "fill_type": match self.fill_type {
                FillType::Solid => "solid",
                FillType::Linear => "linear",
                FillType::Radial => "radial",
                FillType::Diamond => "diamond",
                FillType::Tile => "tile",
            },
            "color": self.color,
            "stops": self.stops.iter().map(|s| serde_json::json!({
                "position": s.position,
                "color": s.color,
            })).collect::<Vec<_>>(),
            "angle": self.angle,
            "tile_scale": self.tile_scale,
            "default_width": self.default_width,
            "default_height": self.default_height,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(ft) = state.get("fill_type").and_then(|v| v.as_str()) {
            self.fill_type = match ft {
                "linear"  => FillType::Linear,
                "radial"  => FillType::Radial,
                "diamond" => FillType::Diamond,
                "tile"    => FillType::Tile,
                _         => FillType::Solid,
            };
        }
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
        if let Some(stops) = state.get("stops").and_then(|v| v.as_array()) {
            self.stops = stops.iter().filter_map(|s| {
                let pos = s.get("position")?.as_f64()? as f32;
                let c = s.get("color")?.as_array()?;
                if c.len() >= 4 {
                    Some(GradientStop {
                        position: pos,
                        color: [
                            c[0].as_u64()? as u8,
                            c[1].as_u64()? as u8,
                            c[2].as_u64()? as u8,
                            c[3].as_u64()? as u8,
                        ],
                    })
                } else { None }
            }).collect();
        }
        if let Some(v) = state.get("angle").and_then(|v| v.as_f64()) { self.angle = v as f32; }
        if let Some(v) = state.get("tile_scale").and_then(|v| v.as_f64()) { self.tile_scale = v as f32; }
        if let Some(v) = state.get("default_width").and_then(|v| v.as_u64()) { self.default_width = v as u32; }
        if let Some(v) = state.get("default_height").and_then(|v| v.as_u64()) { self.default_height = v as u32; }
        self.dirty = true;
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("fill", |state| {
        let mut n = FillNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
