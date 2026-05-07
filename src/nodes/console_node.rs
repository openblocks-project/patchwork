use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;
use crate::nodes::ScrollAreaExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleNode {
    #[serde(default)]
    pub messages: Vec<String>,
    #[serde(skip)]
    pub last_logged: String,
    #[serde(skip, default = "std::time::Instant::now")]
    pub start_time: std::time::Instant,
    /// Index into global system_log — tracks how far we've read
    #[serde(skip)]
    pub log_read_index: usize,
    /// Show system log messages (in addition to wired input)
    #[serde(default = "default_true")]
    pub show_system_log: bool,
}

fn default_true() -> bool { true }

impl Default for ConsoleNode {
    fn default() -> Self {
        Self {
            messages: Vec::new(),
            last_logged: String::new(),
            start_time: std::time::Instant::now(),
            log_read_index: 0,
            show_system_log: true,
        }
    }
}

impl NodeBehavior for ConsoleNode {
    fn title(&self) -> &str { "Console" }
    fn inputs(&self) -> Vec<PortDef> { vec![PortDef::new("Log", PortKind::Generic)] }
    fn outputs(&self) -> Vec<PortDef> { vec![] }
    fn color_hint(&self) -> [u8; 3] { [100, 150, 100] }
    fn inline_ports(&self) -> bool { true }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Log wired input values
        if let Some(val) = inputs.first() {
            let text = format!("{}", val);
            if !text.is_empty() && text != "—" && text != self.last_logged {
                let secs = self.start_time.elapsed().as_secs();
                let mins = secs / 60;
                let s = secs % 60;
                self.messages.push(format!("[{:02}:{:02}] {}", mins, s, text));
                self.last_logged = text;
            }
        }

        // Pull system log messages
        if self.show_system_log {
            let (new_idx, entries) = crate::system_log::read_since(self.log_read_index);
            for entry in entries {
                let prefix = match entry.level {
                    crate::system_log::LogLevel::Info => "ℹ",
                    crate::system_log::LogLevel::Warn => "⚠",
                    crate::system_log::LogLevel::Error => "✖",
                };
                self.messages.push(format!("{} {}", prefix, entry.message));
            }
            self.log_read_index = new_idx;
        }

        // Trim
        if self.messages.len() > 500 {
            self.messages.drain(..self.messages.len() - 500);
        }
        vec![]
    }

    fn type_tag(&self) -> &str { "console" }
    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({ "messages": self.messages, "show_system_log": self.show_system_log })
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(msgs) = state.get("messages").and_then(|v| v.as_array()) {
            self.messages = msgs.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
        }
        if let Some(b) = state.get("show_system_log").and_then(|v| v.as_bool()) {
            self.show_system_log = b;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // Input port
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Generic);
            let connected = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 0);
            if connected {
                ui.label(egui::RichText::new("Logging").small().color(egui::Color32::from_rgb(80, 200, 80)));
            } else {
                ui.label(egui::RichText::new("Connect to log").small().color(dim));
            }
        });

        ui.horizontal(|ui| {
            if ui.small_button("Clear").clicked() {
                self.messages.clear();
                self.last_logged.clear();
            }
            ui.label(egui::RichText::new(format!("{} msgs", self.messages.len())).small().color(dim));
            // Toggle system log
            let sys_label = if self.show_system_log { "Sys ✓" } else { "Sys" };
            let sys_color = if self.show_system_log { egui::Color32::from_rgb(80, 170, 255) } else { egui::Color32::GRAY };
            if ui.add(egui::Button::new(egui::RichText::new(sys_label).small().color(sys_color)).min_size(egui::vec2(32.0, 16.0))).clicked() {
                self.show_system_log = !self.show_system_log;
            }
        });

        ui.separator();

        egui::ScrollArea::vertical().max_height(200.0).stick_to_bottom(true).show_pannable(ui, |ui| {
            if self.messages.is_empty() {
                ui.label(egui::RichText::new("No messages yet").small().italics().color(dim));
            }
            for msg in &self.messages {
                let color = if msg.starts_with("✖") || msg.contains("error") || msg.contains("Error") || msg.contains("failed") {
                    egui::Color32::from_rgb(255, 100, 100)
                } else if msg.starts_with("⚠") || msg.contains("warn") || msg.contains("Warning") {
                    egui::Color32::from_rgb(255, 200, 100)
                } else if msg.starts_with("ℹ") {
                    egui::Color32::from_rgb(140, 180, 220)
                } else {
                    ui.visuals().text_color()
                };
                ui.label(egui::RichText::new(msg).color(color).monospace().size(11.0));
            }
        });

        // Keep updating if system log is active
        if self.show_system_log {
            ui.ctx().request_repaint();
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("console", |state| {
        let mut n = ConsoleNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
