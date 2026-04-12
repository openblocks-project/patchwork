use eframe::egui::{self, RichText};
use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── SmootherNode ──────────────────────────────────────────────────────────────
//
// Takes a noisy float value and outputs a smoothed version.
//
// Two modes:
//   SMA  — Simple Moving Average over last N samples.
//           Clean, predictable lag. Good for position tracking.
//   EMA  — Exponential Moving Average.
//           No fixed lag — recent values weighted more.
//           One parameter: factor (0=no smooth, 1=freeze).

const MAX_WINDOW: usize = 64;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SmoothMode { Sma, Ema }

#[derive(Debug, Clone)]
pub struct SmootherNode {
    pub mode:     SmoothMode,
    /// SMA window size (1–64)
    pub window:   usize,
    /// EMA factor (0.0 = no smoothing, 0.99 = very heavy)
    pub ema_factor: f32,
    // ── Runtime ──
    ring:    Vec<f32>,  // ring buffer for SMA
    head:    usize,
    filled:  usize,
    ema_val: f32,
    out_val: f32,
}

impl Default for SmootherNode {
    fn default() -> Self {
        Self {
            mode:       SmoothMode::Sma,
            window:     8,
            ema_factor: 0.85,
            ring:       vec![0.0; MAX_WINDOW],
            head:       0,
            filled:     0,
            ema_val:    0.0,
            out_val:    0.0,
        }
    }
}

impl NodeBehavior for SmootherNode {
    fn title(&self)      -> &str    { "Smoother" }
    fn type_tag(&self)   -> &str    { "smoother" }
    fn color_hint(&self) -> [u8;3]  { [80, 190, 160] }
    fn min_width(&self)  -> Option<f32> { Some(190.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Value", PortKind::Number)]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Smooth", PortKind::Number)]
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let input = inputs.get(0).map(|v| v.as_float()).unwrap_or(0.0);

        self.out_val = match self.mode {
            SmoothMode::Sma => {
                let n = self.window.clamp(1, MAX_WINDOW);
                // Push into ring buffer
                self.ring[self.head] = input;
                self.head = (self.head + 1) % n;
                if self.filled < n { self.filled += 1; }
                // Average filled slots
                let sum: f32 = self.ring[..self.filled].iter().sum();
                sum / self.filled as f32
            }
            SmoothMode::Ema => {
                let f = self.ema_factor.clamp(0.0, 0.999);
                self.ema_val = f * self.ema_val + (1.0 - f) * input;
                self.ema_val
            }
        };

        vec![(0, PortValue::Float(self.out_val))]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim     = egui::Color32::from_rgb(110, 110, 120);
        let teal    = egui::Color32::from_rgb(80, 210, 170);

        // ── Value input port ──────────────────────────────────────────────
        let connected = ctx.connections.iter()
            .any(|c| c.to_node == node_id && c.to_port == 0);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 0, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Number,
            );
            let lbl = if connected { "Value ✓" } else { "Value" };
            let col = if connected { teal } else { dim };
            ui.label(RichText::new(lbl).small().color(col));
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Mode selector ─────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Mode").small().color(dim));
            ui.add_space(4.0);
            if ui.selectable_label(self.mode == SmoothMode::Sma,
                RichText::new("Avg N").small()).clicked() {
                self.mode = SmoothMode::Sma;
                // Reset buffer on mode switch
                self.filled = 0;
                self.head = 0;
            }
            if ui.selectable_label(self.mode == SmoothMode::Ema,
                RichText::new("EMA").small()).clicked() {
                self.mode = SmoothMode::Ema;
            }
        });

        ui.add_space(4.0);

        match self.mode {
            SmoothMode::Sma => {
                // Window size slider
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Window").small().color(dim));
                    let mut n = self.window as i32;
                    let changed = ui.add(
                        egui::Slider::new(&mut n, 1..=MAX_WINDOW as i32)
                            .show_value(false)
                    ).changed();
                    ui.label(RichText::new(format!("N={}", n)).small().monospace());
                    if changed {
                        let new_n = n as usize;
                        if new_n != self.window {
                            self.window = new_n;
                            // Reset buffer when window changes
                            self.filled = 0;
                            self.head = 0;
                        }
                    }
                });
                ui.label(RichText::new(format!("avg of last {} samples", self.window))
                    .small().color(dim).italics());
            }
            SmoothMode::Ema => {
                // EMA factor slider
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Strength").small().color(dim));
                    ui.add(
                        egui::Slider::new(&mut self.ema_factor, 0.0..=0.99)
                            .show_value(false)
                    );
                    // Show as % smoothing
                    ui.label(RichText::new(format!("{:.0}%", self.ema_factor * 100.0))
                        .small().monospace());
                });
                ui.label(RichText::new("0% = none   99% = max").small().color(dim).italics());
            }
        }

        ui.add_space(6.0);

        // ── Live output display ───────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Out:").small().color(dim));
            ui.label(RichText::new(format!("{:.2}", self.out_val))
                .small().monospace().color(teal));
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Output port ───────────────────────────────────────────────────
        let out_str = format!("{:.3}", self.out_val);
        crate::nodes::output_port_row(
            ui, "Smooth", &out_str, node_id, 0,
            ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects,
            PortKind::Number,
        );
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "mode":       match self.mode { SmoothMode::Sma => "sma", SmoothMode::Ema => "ema" },
            "window":     self.window,
            "ema_factor": self.ema_factor,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(m) = state.get("mode").and_then(|v| v.as_str()) {
            self.mode = if m == "ema" { SmoothMode::Ema } else { SmoothMode::Sma };
        }
        if let Some(v) = state.get("window").and_then(|v| v.as_u64()) {
            self.window = (v as usize).clamp(1, MAX_WINDOW);
        }
        if let Some(v) = state.get("ema_factor").and_then(|v| v.as_f64()) {
            self.ema_factor = v as f32;
        }
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("smoother", |state| {
        let mut n = SmootherNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
