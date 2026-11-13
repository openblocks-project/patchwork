//! Random/Noise — unified random value generator.
//!
//! Two modes:
//! - Trigger: outputs a new random 0-1 value on each trigger rising edge. Holds between triggers.
//! - Smooth: continuously drifting Perlin noise (organic movement).
//!
//! Always outputs 0-1. Use MapRange downstream for other ranges.
//! Also outputs a 2D noise texture on the Image port.

use crate::graph::{ImageData, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum RandomMode {
    Trigger,
    Smooth,
}

impl Default for RandomMode {
    fn default() -> Self {
        RandomMode::Smooth
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiseNode {
    pub mode: RandomMode,
    /// Smooth mode: how fast the value drifts (Perlin time advance per second)
    #[serde(default = "default_speed")]
    pub speed: f32,
    /// Trigger mode: probability of generating a new value (0-1, 1.0 = always)
    #[serde(default = "default_one")]
    pub chance: f32,
    #[serde(default)]
    pub seed: u32,
    /// Current held value (0-1)
    #[serde(skip)]
    pub value: f32,
    /// Internal Perlin phase (advances over time in Smooth mode)
    #[serde(skip)]
    pub phase: f64,
    /// Last trigger input value (for rising edge detection)
    #[serde(skip)]
    pub last_trigger: f32,
    /// Whether value changed this frame (for Changed output)
    #[serde(skip)]
    pub changed: bool,
    /// Cached texture (regenerated when seed changes)
    #[serde(skip)]
    pub cached_image: Option<Arc<ImageData>>,
    #[serde(skip)]
    pub cached_image_seed: u32,
    #[serde(skip, default = "std::time::Instant::now")]
    pub last_instant: std::time::Instant,
}

fn default_speed() -> f32 {
    1.0
}
fn default_one() -> f32 {
    1.0
}

impl Default for NoiseNode {
    fn default() -> Self {
        Self {
            mode: RandomMode::Smooth,
            speed: 1.0,
            chance: 1.0,
            seed: 0,
            value: 0.0,
            phase: 0.0,
            last_trigger: 0.0,
            changed: false,
            cached_image: None,
            cached_image_seed: u32::MAX,
            last_instant: std::time::Instant::now(),
        }
    }
}

impl NodeBehavior for NoiseNode {
    fn title(&self) -> &str {
        "Random/Noise"
    }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Trigger", PortKind::Trigger),
            PortDef::new("Seed", PortKind::Number),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Value", PortKind::Normalized),
            PortDef::new("Changed", PortKind::Trigger),
            PortDef::new("Image", PortKind::Image),
        ]
    }

    fn color_hint(&self) -> [u8; 3] {
        [140, 180, 140]
    }
    fn inline_ports(&self) -> bool {
        true
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Read trigger and seed from inputs
        let trigger_val = inputs.first().map(|v| v.as_float()).unwrap_or(0.0);
        if let Some(PortValue::Float(s)) = inputs.get(1) {
            self.seed = *s as u32;
        }

        let now = std::time::Instant::now();
        let dt = now
            .duration_since(self.last_instant)
            .as_secs_f64()
            .min(0.25);
        self.last_instant = now;

        self.changed = false;

        match self.mode {
            RandomMode::Trigger => {
                // Rising edge detection
                let rising = trigger_val > 0.5 && self.last_trigger <= 0.5;
                self.last_trigger = trigger_val;

                if rising {
                    // Roll chance
                    let roll = simple_random(self.seed, self.phase as u32);
                    if roll <= self.chance {
                        self.value =
                            simple_random(self.seed.wrapping_add(1), self.phase as u32 + 7919);
                        self.changed = true;
                    }
                    self.phase += 1.0;
                }
            }
            RandomMode::Smooth => {
                // Advance Perlin phase by dt * speed
                self.phase += dt * self.speed as f64;
                let new_val = (perlin_1d(self.phase as f32, self.seed) * 0.5 + 0.5).clamp(0.0, 1.0);
                if (new_val - self.value).abs() > 0.0001 {
                    self.changed = true;
                }
                self.value = new_val;
            }
        }

        // Generate/cache texture
        if self.cached_image_seed != self.seed || self.cached_image.is_none() {
            self.cached_image = Some(generate_2d(5.0, self.seed, 0, 128));
            self.cached_image_seed = self.seed;
        }

        vec![
            (0, PortValue::Float(self.value)),
            (1, PortValue::Float(if self.changed { 1.0 } else { 0.0 })),
            (
                2,
                self.cached_image
                    .as_ref()
                    .map(|img| PortValue::Image(img.clone()))
                    .unwrap_or(PortValue::None),
            ),
        ]
    }

    fn type_tag(&self) -> &str {
        "noise"
    }
    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<NoiseNode>(state.clone()) {
            self.mode = l.mode;
            self.speed = l.speed;
            self.chance = l.chance;
            self.seed = l.seed;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let accent = ui.visuals().hyperlink_color;
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // Trigger input port
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui,
                ctx.node_id,
                0,
                true,
                ctx.connections,
                ctx.port_positions,
                ctx.dragging_from,
                ctx.pending_disconnects,
                PortKind::Trigger,
            );
            ui.label(egui::RichText::new("Trigger").small());
        });

        // Seed input port
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui,
                ctx.node_id,
                1,
                true,
                ctx.connections,
                ctx.port_positions,
                ctx.dragging_from,
                ctx.pending_disconnects,
                PortKind::Number,
            );
            let wired = ctx
                .connections
                .iter()
                .any(|c| c.to_node == ctx.node_id && c.to_port == 1);
            if wired {
                ui.label(
                    egui::RichText::new(format!("Seed: {}", self.seed))
                        .small()
                        .color(accent),
                );
            } else {
                ui.label(egui::RichText::new("Seed").small());
                ui.add(egui::DragValue::new(&mut self.seed).speed(1.0));
            }
        });

        ui.separator();

        // Mode selector
        ui.horizontal(|ui| {
            if ui
                .selectable_label(self.mode == RandomMode::Trigger, "Trigger")
                .clicked()
            {
                self.mode = RandomMode::Trigger;
            }
            if ui
                .selectable_label(self.mode == RandomMode::Smooth, "Smooth")
                .clicked()
            {
                self.mode = RandomMode::Smooth;
            }
        });

        // Mode-specific controls
        match self.mode {
            RandomMode::Trigger => {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Chance").small());
                    ui.add(egui::Slider::new(&mut self.chance, 0.0..=1.0).show_value(false));
                    ui.label(egui::RichText::new(format!("{:.0}%", self.chance * 100.0)).small());
                });
            }
            RandomMode::Smooth => {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Speed").small());
                    ui.add(
                        egui::Slider::new(&mut self.speed, 0.01..=10.0)
                            .logarithmic(true)
                            .show_value(false),
                    );
                    ui.label(egui::RichText::new(format!("{:.2}", self.speed)).small());
                });
            }
        }

        // Value display
        let bar_w = ui.available_width().min(140.0);
        let bar_h = 16.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
        let fill_w = rect.width() * self.value.clamp(0.0, 1.0);
        if fill_w > 0.5 {
            let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, bar_h));
            painter.rect_filled(fill_rect, 4.0, accent);
        }
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{:.3}", self.value),
            egui::FontId::proportional(10.0),
            if self.value > 0.5 {
                egui::Color32::WHITE
            } else {
                ui.visuals().text_color()
            },
        );

        ui.separator();

        // Output ports
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("Value: {:.3}", self.value)).small());
            crate::nodes::inline_port_circle(
                ui,
                ctx.node_id,
                0,
                false,
                ctx.connections,
                ctx.port_positions,
                ctx.dragging_from,
                ctx.pending_disconnects,
                PortKind::Normalized,
            );
        });
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Changed: {}",
                    if self.changed { "yes" } else { "—" }
                ))
                .small()
                .color(dim),
            );
            crate::nodes::inline_port_circle(
                ui,
                ctx.node_id,
                1,
                false,
                ctx.connections,
                ctx.port_positions,
                ctx.dragging_from,
                ctx.pending_disconnects,
                PortKind::Trigger,
            );
        });
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Image").small());
            crate::nodes::inline_port_circle(
                ui,
                ctx.node_id,
                2,
                false,
                ctx.connections,
                ctx.port_positions,
                ctx.dragging_from,
                ctx.pending_disconnects,
                PortKind::Image,
            );
        });

        if self.mode == RandomMode::Smooth {
            ui.ctx().request_repaint();
        }
    }
}

// ── Noise/random functions ───────────────────────────────────────────────────

fn hash(x: i32, seed: u32) -> u32 {
    let mut h = x as u32 ^ seed;
    h = h.wrapping_mul(0x85ebca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2ae35);
    h ^= h >> 16;
    h
}

/// Simple 0-1 random from seed + counter
fn simple_random(seed: u32, counter: u32) -> f32 {
    let h = hash(counter as i32, seed);
    h as f32 / u32::MAX as f32
}

fn grad_1d(hash: u32, x: f32) -> f32 {
    if hash & 1 == 0 { x } else { -x }
}
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + t * (b - a)
}

pub fn perlin_1d(x: f32, seed: u32) -> f32 {
    let xi = x.floor() as i32;
    let xf = x - x.floor();
    let u = fade(xf);
    lerp(
        grad_1d(hash(xi, seed), xf),
        grad_1d(hash(xi + 1, seed), xf - 1.0),
        u,
    )
}

fn grad_2d(hash: u32, x: f32, y: f32) -> f32 {
    match hash & 3 {
        0 => x + y,
        1 => -x + y,
        2 => x - y,
        _ => -x - y,
    }
}

pub fn perlin_2d(x: f32, y: f32, seed: u32) -> f32 {
    let xi = x.floor() as i32;
    let yi = y.floor() as i32;
    let xf = x - x.floor();
    let yf = y - y.floor();
    let u = fade(xf);
    let v = fade(yf);
    let aa = hash(xi + hash(yi, seed) as i32, 0);
    let ab = hash(xi + hash(yi + 1, seed) as i32, 0);
    let ba = hash(xi + 1 + hash(yi, seed) as i32, 0);
    let bb = hash(xi + 1 + hash(yi + 1, seed) as i32, 0);
    let x1 = lerp(grad_2d(aa, xf, yf), grad_2d(ba, xf - 1.0, yf), u);
    let x2 = lerp(
        grad_2d(ab, xf, yf - 1.0),
        grad_2d(bb, xf - 1.0, yf - 1.0),
        u,
    );
    lerp(x1, x2, v)
}

fn generate_2d(scale: f32, seed: u32, noise_type: u8, size: u32) -> Arc<ImageData> {
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let nx = x as f32 / size as f32 * scale;
            let ny = y as f32 / size as f32 * scale;
            let v = if noise_type == 0 {
                perlin_2d(nx, ny, seed)
            } else {
                simple_random(seed, x + y * size) * 2.0 - 1.0
            };
            let byte = ((v * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
            let idx = ((y * size + x) * 4) as usize;
            pixels[idx] = byte;
            pixels[idx + 1] = byte;
            pixels[idx + 2] = byte;
            pixels[idx + 3] = 255;
        }
    }
    Arc::new(ImageData::new(size, size, pixels))
}

#[allow(dead_code)]
pub fn generate_2d_pub(scale: f32, seed: u32, noise_type: u8, size: u32) -> Arc<ImageData> {
    generate_2d(scale, seed, noise_type, size)
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("noise", |state| {
        if let Ok(n) = serde_json::from_value::<NoiseNode>(state.clone()) {
            Box::new(n)
        } else {
            Box::new(NoiseNode::default())
        }
    });
}
