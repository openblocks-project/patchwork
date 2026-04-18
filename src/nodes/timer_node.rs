use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;

// ── Timer Node ───────────────────────────────────────────────────────────────
//
// Simple seconds-based repeating trigger. Set an interval, get pulses.
// For musical timing (BPM, beats, bars), use the Clock node instead.
//
// Ports:
//   In  0: Interval (Number, seconds)
//   Out 0: Trigger  (Trigger, pulse at start of each cycle)
//   Out 1: Phase    (Normalized, 0–1 ramp per cycle)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerNode {
    #[serde(default = "default_one")]
    pub interval: f32,
    #[serde(default)]
    pub elapsed: f32,
    #[serde(default = "default_true")]
    pub running: bool,
    #[serde(default = "default_pulse_width")]
    pub pulse_width: f32,
    /// Fine phase offset applied when synced via the Phase input port.
    /// 0.0 = in-phase with source, 0.5 = anti-phase, etc. Persisted.
    /// When the Phase port is unwired, this field is dormant.
    #[serde(default)]
    pub phase_offset: f32,
    #[serde(skip, default = "std::time::Instant::now")]
    ref_time: std::time::Instant,
    #[serde(skip)]
    paused_elapsed: f64,
    #[serde(skip)]
    time_initialized: bool,
    #[serde(skip)]
    last_start_val: f32,
    /// Previous shifted-phase value for wrap-around edge detection in
    /// sync mode. When the shifted phase drops from ~1 back toward ~0
    /// we fire a trigger.
    #[serde(skip)]
    last_sync_phase: f32,
}

fn default_one() -> f32 { 1.0 }
fn default_true() -> bool { true }
fn default_pulse_width() -> f32 { 0.1 }

impl Default for TimerNode {
    fn default() -> Self {
        Self {
            interval: 1.0,
            elapsed: 0.0,
            running: true,
            pulse_width: 0.1,
            phase_offset: 0.0,
            ref_time: std::time::Instant::now(),
            paused_elapsed: 0.0,
            time_initialized: false,
            last_start_val: 0.0,
            last_sync_phase: 0.0,
        }
    }
}

impl NodeBehavior for TimerNode {
    fn title(&self) -> &str { "Timer" }
    fn type_tag(&self) -> &str { "timer" }
    fn color_hint(&self) -> [u8; 3] { [100, 200, 180] }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Interval", PortKind::Number),
            PortDef::new("Start",    PortKind::Trigger),
            PortDef::new("Phase",    PortKind::Normalized),  // sync: 0..1 from Clock or another Timer
            PortDef::new("Offset",   PortKind::Normalized),  // overrides inline phase_offset slider when wired
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Trigger", PortKind::Trigger),
            PortDef::new("Phase",   PortKind::Normalized),
        ]
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Override interval from input port
        if let Some(PortValue::Float(v)) = inputs.first() {
            if *v > 0.0 { self.interval = *v; }
        }

        // Start trigger (port 1) — rising edge resets and starts
        if let Some(PortValue::Float(v)) = inputs.get(1) {
            if *v > 0.5 && self.last_start_val <= 0.5 {
                self.elapsed = 0.0;
                self.paused_elapsed = 0.0;
                self.ref_time = std::time::Instant::now();
                self.running = true;
            }
            self.last_start_val = *v;
        }

        // ── Sync mode ──────────────────────────────────────────
        // When Phase input (port 2) is wired, Timer locks to the
        // external phase instead of free-running. The Offset input
        // (port 3) or inline slider shifts the trigger point.
        // This early-returns so the wall-clock path below is skipped.
        if let Some(PortValue::Float(ext_phase)) = inputs.get(2) {
            let offset = match inputs.get(3) {
                Some(PortValue::Float(v)) => *v,
                _ => self.phase_offset,
            };
            let shifted = (ext_phase + offset).fract();
            let shifted = if shifted < 0.0 { shifted + 1.0 } else { shifted };
            // Trigger on phase wrap-around: shifted drops from ~1 to ~0.
            // Guard > 0.5 prevents false fires from float jitter.
            let trigger = if shifted < self.last_sync_phase
                             && (self.last_sync_phase - shifted) > 0.5
                             && self.running
            { 1.0 } else { 0.0 };
            self.last_sync_phase = shifted;
            return vec![
                (0, PortValue::Float(trigger)),
                (1, PortValue::Float(shifted)),
            ];
        }

        // ── Free-running wall-clock mode (unchanged) ─────────
        // Wall-clock timing
        if !self.time_initialized {
            if self.running {
                self.ref_time = std::time::Instant::now();
                self.paused_elapsed = self.elapsed as f64;
            }
            self.time_initialized = true;
        }

        if self.running {
            self.elapsed = (self.ref_time.elapsed().as_secs_f64() + self.paused_elapsed) as f32;
        }

        let safe_interval = self.interval.max(0.01);
        let phase = (self.elapsed % safe_interval) / safe_interval;
        let is_pulse = phase < (self.pulse_width / safe_interval).min(0.5);
        let trigger = if is_pulse && self.running { 1.0 } else { 0.0 };

        vec![
            (0, PortValue::Float(trigger)),
            (1, PortValue::Float(phase)),
        ]
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "interval": self.interval,
            "elapsed": self.elapsed,
            "running": self.running,
            "pulse_width": self.pulse_width,
            "phase_offset": self.phase_offset,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(v) = state.get("interval").and_then(|v| v.as_f64()) { self.interval = v as f32; }
        if let Some(v) = state.get("elapsed").and_then(|v| v.as_f64()) { self.elapsed = v as f32; }
        if let Some(v) = state.get("running").and_then(|v| v.as_bool()) { self.running = v; }
        if let Some(v) = state.get("pulse_width").and_then(|v| v.as_f64()) { self.pulse_width = v as f32; }
        if let Some(v) = state.get("phase_offset").and_then(|v| v.as_f64()) { self.phase_offset = v as f32; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = egui::Color32::from_rgb(110, 110, 120);
        let blue = egui::Color32::from_rgb(80, 170, 255);
        let interval_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 0);
        let start_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 1);

        // ── Input ports ──────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(egui::RichText::new("Interval").small());
            if interval_wired {
                ui.label(egui::RichText::new(format!("{:.3}s", self.interval)).small().color(blue));
            }
        });
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 1, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Trigger);
            ui.label(egui::RichText::new("Start").small().color(
                if start_wired { egui::Color32::from_rgb(255, 150, 60) } else { dim }));
        });

        // Phase sync input (port 2) — connect a Clock's Phase output here
        let phase_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 2);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 2, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Normalized);
            ui.label(egui::RichText::new("Phase").small().color(
                if phase_wired { blue } else { dim }));
            if phase_wired {
                ui.label(egui::RichText::new("synced").small().color(egui::Color32::from_rgb(80, 220, 120)));
            }
        });

        // Offset input (port 3) — overrides inline slider when wired
        let offset_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 3);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 3, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Normalized);
            ui.label(egui::RichText::new("Offset").small().color(
                if offset_wired { blue } else { dim }));
            if !offset_wired && phase_wired {
                // Inline offset slider — visible only when synced.
                // Progressive disclosure: no clutter for free-running Timers.
                ui.add(egui::DragValue::new(&mut self.phase_offset)
                    .speed(0.01).range(0.0..=0.99).max_decimals(2)
                    .custom_formatter(|v, _| {
                        if v.abs() < 0.005 { "0".into() }
                        else if (v - 0.25).abs() < 0.01 { "¼".into() }
                        else if (v - 0.333).abs() < 0.02 { "⅓".into() }
                        else if (v - 0.5).abs() < 0.01 { "½".into() }
                        else if (v - 0.667).abs() < 0.02 { "⅔".into() }
                        else if (v - 0.75).abs() < 0.01 { "¾".into() }
                        else { format!("{:.2}", v) }
                    })
                );
            }
        });

        ui.separator();

        // ── Controls ─────────────────────────────────────────────
        let was_running = self.running;
        ui.horizontal(|ui| {
            if ui.button(if self.running { "⏸" } else { "▶" }).clicked() {
                self.running = !self.running;
                if self.running && !was_running {
                    self.paused_elapsed = self.elapsed as f64;
                    self.ref_time = std::time::Instant::now();
                } else if !self.running && was_running {
                    self.paused_elapsed = self.elapsed as f64;
                }
            }
            if ui.button("Reset").clicked() {
                self.elapsed = 0.0;
                self.paused_elapsed = 0.0;
                self.ref_time = std::time::Instant::now();
            }
        });

        // Interval slider (only when not wired)
        if !interval_wired {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Interval").small());
                ui.add(egui::Slider::new(&mut self.interval, 0.001..=30.0)
                    .suffix("s")
                    .logarithmic(true)
                    .custom_formatter(|v, _| format!("{:.3}", v))
                    .clamping(egui::SliderClamping::Never));
            });
        }

        // Pulse width
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Pulse").small());
            ui.add(egui::Slider::new(&mut self.pulse_width, 0.001..=1.0)
                .suffix("s")
                .logarithmic(true)
                .custom_formatter(|v, _| format!("{:.3}", v))
                .clamping(egui::SliderClamping::Never));
        });

        // ── Spinner visual ───────────────────────────────────────
        let safe_interval = self.interval.max(0.01);
        // In sync mode the spinner shows the shifted external phase;
        // in free-running mode it shows the wall-clock-derived phase.
        let (phase, is_pulse) = if phase_wired {
            (self.last_sync_phase, false) // sync mode: no pulse width concept
        } else {
            let p = (self.elapsed % safe_interval) / safe_interval;
            (p, p < (self.pulse_width / safe_interval).min(0.5))
        };

        let spinner_size = 50.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(spinner_size, spinner_size), egui::Sense::hover());
        let center = rect.center();
        let radius = spinner_size * 0.4;
        let painter = ui.painter();

        painter.circle_stroke(center, radius, egui::Stroke::new(2.0, egui::Color32::from_rgb(60, 60, 70)));

        let segments = 32;
        let filled = (phase * segments as f32) as usize;
        for i in 0..segments {
            let a1 = std::f32::consts::TAU * (i as f32 / segments as f32) - std::f32::consts::FRAC_PI_2;
            let a2 = std::f32::consts::TAU * ((i + 1) as f32 / segments as f32) - std::f32::consts::FRAC_PI_2;
            let r_inner = radius - 5.0;
            let p1 = center + egui::vec2(a1.cos() * r_inner, a1.sin() * r_inner);
            let p2 = center + egui::vec2(a1.cos() * radius, a1.sin() * radius);
            let p3 = center + egui::vec2(a2.cos() * radius, a2.sin() * radius);
            let p4 = center + egui::vec2(a2.cos() * r_inner, a2.sin() * r_inner);
            let col = if i < filled {
                if self.running { egui::Color32::from_rgb(80, 200, 120) } else { egui::Color32::from_rgb(120, 120, 60) }
            } else {
                egui::Color32::from_rgb(40, 40, 50)
            };
            painter.add(egui::Shape::convex_polygon(vec![p1, p2, p3, p4], col, egui::Stroke::NONE));
        }

        let dot_col = if is_pulse && self.running {
            egui::Color32::from_rgb(255, 220, 60)
        } else {
            egui::Color32::from_rgb(80, 80, 90)
        };
        painter.circle_filled(center, 5.0, dot_col);

        let angle = std::f32::consts::TAU * phase - std::f32::consts::FRAC_PI_2;
        let tip = center + egui::vec2(angle.cos() * (radius - 2.0), angle.sin() * (radius - 2.0));
        painter.line_segment([center, tip], egui::Stroke::new(2.0, egui::Color32::WHITE));

        // Status
        ui.horizontal(|ui| {
            if self.running {
                ui.colored_label(egui::Color32::from_rgb(80, 200, 120), "⏱ Running");
            } else {
                ui.colored_label(dim, "⏸ Paused");
            }
            ui.label(egui::RichText::new(format!("{:.2}s", self.elapsed)).small().monospace().color(dim));
        });

        ui.separator();

        // ── Output ports ─────────────────────────────────────────
        crate::nodes::output_port_row(ui, "Trigger",
            &format!("{}", if is_pulse && self.running { 1 } else { 0 }),
            ctx.node_id, 0, ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Trigger);
        crate::nodes::output_port_row(ui, "Phase", &format!("{:.2}", phase),
            ctx.node_id, 1, ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Normalized);

        if self.running { ui.ctx().request_repaint(); }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("timer", |state| {
        let mut n = TimerNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
