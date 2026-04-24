use crate::graph::{PortDef, PortKind, PortValue, Graph};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::sync::mpsc;

// ── Runtime state (non-Clone, lives in egui temp data) ───────────────────────

struct TerminalRuntime {
    sender:     mpsc::Sender<(String, String)>,
    receiver:   mpsc::Receiver<(String, String)>,
    is_running: bool,
}

impl Default for TerminalRuntime {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { sender: tx, receiver: rx, is_running: false }
    }
}

// ── Background command execution ──────────────────────────────────────────────

fn run_command(cmd: String, working_dir: String, tx: mpsc::Sender<(String, String)>) {
    std::thread::spawn(move || {
        #[cfg(target_os = "windows")]
        let (sh, flag) = ("cmd", "/C");
        #[cfg(not(target_os = "windows"))]
        let (sh, flag) = ("sh", "-c");

        let mut builder = std::process::Command::new(sh);
        builder.arg(flag).arg(&cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !working_dir.is_empty() {
            builder.current_dir(&working_dir);
        }

        let output = match builder.output() {
            Ok(o) => {
                let mut out = String::from_utf8_lossy(&o.stdout).into_owned();
                let err = String::from_utf8_lossy(&o.stderr);
                if !err.is_empty() {
                    if !out.is_empty() { out.push('\n'); }
                    out.push_str(&err);
                }
                out.trim_end().to_string()
            }
            Err(e) => format!("Error: {}", e),
        };

        let _ = tx.send((cmd, output));
    });
}

// ── Node struct ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalNode {
    // ── Persisted ──
    #[serde(default)]
    pub history:     Vec<(String, String)>,  // (command, stdout+stderr)
    #[serde(default)]
    pub working_dir: String,
    #[serde(default)]
    pub last_output: String,

    // ── Runtime only ──
    #[serde(skip)] pub input_buf:          String,
    #[serde(skip)] pub last_send_trigger:  f32,
    #[serde(skip)] pub done_pulse:         bool,
    #[serde(skip)] pub scroll_to_bottom:   bool,
    #[serde(skip)] pub queued_injection:   Option<String>,
}

impl Default for TerminalNode {
    fn default() -> Self {
        Self {
            history:            Vec::new(),
            working_dir:        String::new(),
            last_output:        String::new(),
            input_buf:          String::new(),
            last_send_trigger:  0.0,
            done_pulse:         false,
            scroll_to_bottom:   false,
            queued_injection:   None,
        }
    }
}

// ── NodeBehavior ──────────────────────────────────────────────────────────────

impl NodeBehavior for TerminalNode {
    fn title(&self)      -> &str        { "Terminal" }
    fn type_tag(&self)   -> &str        { "terminal" }
    fn color_hint(&self) -> [u8; 3]     { [30, 160, 80] }
    fn inline_ports(&self) -> bool      { true }
    fn min_width(&self)  -> Option<f32> { Some(300.0) }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Cmd",  PortKind::Text),
            PortDef::new("Send", PortKind::Trigger),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Output", PortKind::Text),
            PortDef::new("Done",   PortKind::Trigger),
        ]
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Port 1: Send — rising-edge detection
        let send_val = inputs.get(1).map(|v| v.as_float()).unwrap_or(0.0);
        if send_val > 0.5 && self.last_send_trigger <= 0.5 {
            if let Some(PortValue::Text(cmd)) = inputs.first() {
                if !cmd.trim().is_empty() {
                    self.queued_injection = Some(cmd.clone());
                }
            }
        }
        self.last_send_trigger = send_val;

        // Done pulse: emit for exactly one frame, then clear
        let done = if self.done_pulse { 1.0f32 } else { 0.0 };
        self.done_pulse = false;

        vec![
            (0, PortValue::Text(self.last_output.clone())),
            (1, PortValue::Float(done)),
        ]
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "history":     self.history,
            "working_dir": self.working_dir,
            "last_output": self.last_output,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(arr) = state.get("history").and_then(|v| v.as_array()) {
            self.history = arr.iter()
                .filter_map(|v| {
                    let cmd = v.get(0).and_then(|s| s.as_str())?.to_string();
                    let out = v.get(1).and_then(|s| s.as_str())?.to_string();
                    Some((cmd, out))
                })
                .collect();
            if self.history.len() > 100 {
                self.history.drain(..self.history.len() - 100);
            }
        }
        if let Some(s) = state.get("working_dir").and_then(|v| v.as_str()) {
            self.working_dir = s.to_string();
        }
        if let Some(s) = state.get("last_output").and_then(|v| v.as_str()) {
            self.last_output = s.to_string();
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let green = egui::Color32::from_rgb(100, 210, 120);
        let blue  = egui::Color32::from_rgb(80, 170, 255);

        // ── Acquire / initialise runtime state ────────────────────────────
        let rt_id = egui::Id::new(("terminal_rt", ctx.node_id));
        let runtime: Arc<Mutex<TerminalRuntime>> = ui.ctx().data_mut(|d| {
            d.get_temp_mut_or_insert_with(rt_id, || {
                Arc::new(Mutex::new(TerminalRuntime::default()))
            }).clone()
        });

        // ── Poll channel for completed commands ───────────────────────────
        {
            let mut rt = runtime.lock().unwrap();
            if let Ok((cmd, output)) = rt.receiver.try_recv() {
                self.last_output = output.clone();
                self.history.push((cmd, output));
                if self.history.len() > 100 {
                    self.history.drain(..self.history.len() - 100);
                }
                rt.is_running = false;
                self.done_pulse = true;
                self.scroll_to_bottom = true;
            }
        }

        let is_running = runtime.lock().unwrap().is_running;

        // ── Handle programmatic injection from evaluate() ─────────────────
        if let Some(injected) = self.queued_injection.take() {
            let mut rt = runtime.lock().unwrap();
            if !rt.is_running {
                rt.is_running = true;
                run_command(injected, self.working_dir.clone(), rt.sender.clone());
            }
        }

        // ── Input ports (inline) ──────────────────────────────────────────
        let cmd_wired  = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 0);
        let send_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 1);

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Text);
            ui.label(egui::RichText::new("Cmd").small().strong());
            if cmd_wired {
                let v = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0);
                if let PortValue::Text(ref t) = v {
                    let preview = crate::nodes::truncate_chars(t, 24);
                    ui.label(egui::RichText::new(preview).small().monospace().color(blue));
                }
            } else {
                ui.label(egui::RichText::new("—").small().color(dim));
            }
        });

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 1, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Trigger);
            ui.label(egui::RichText::new("Send").small().strong());
            if send_wired {
                let v = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 1).as_float();
                let col = if v > 0.5 { egui::Color32::from_rgb(255, 160, 40) } else { dim };
                ui.label(egui::RichText::new(if v > 0.5 { "▶ high" } else { "low" }).small().color(col));
            }
        });

        ui.separator();

        // ── Working directory ─────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Dir:").small().color(dim));
            ui.add(egui::TextEdit::singleline(&mut self.working_dir)
                .hint_text("(current dir)")
                .font(egui::TextStyle::Monospace)
                .desired_width(ui.available_width()));
        });

        ui.add_space(2.0);

        // ── Scrollable output area ────────────────────────────────────────
        let scroll_id = egui::Id::new(("terminal_scroll", ctx.node_id));
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt(scroll_id)
            .max_height(180.0)
            .auto_shrink([false, false]);

        if self.scroll_to_bottom {
            scroll = scroll.stick_to_bottom(true);
        }

        let bg = ui.visuals().extreme_bg_color;
        egui::Frame::new().fill(bg).corner_radius(4.0).show(ui, |ui| {
            scroll.show(ui, |ui| {
                ui.set_min_width(280.0);
                if self.history.is_empty() {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("No output yet.").small().italics().color(dim));
                    ui.add_space(6.0);
                } else {
                    ui.add_space(4.0);
                    for (cmd, output) in &self.history {
                        ui.label(egui::RichText::new(format!("$ {}", cmd))
                            .monospace().size(11.0).color(green));
                        if !output.is_empty() {
                            ui.label(egui::RichText::new(output).monospace().size(11.0));
                        }
                        ui.add_space(2.0);
                    }
                }
                if is_running {
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new("⏳ Running…")
                        .small().color(egui::Color32::from_rgb(255, 180, 60)));
                }
                ui.add_space(4.0);
            });
        });

        self.scroll_to_bottom = false;

        // ── Command input row ─────────────────────────────────────────────
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("$").monospace().color(green));
            let can_run = !is_running && !self.input_buf.trim().is_empty();
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.input_buf)
                    .hint_text("type command…")
                    .font(egui::TextStyle::Monospace)
                    .desired_width(ui.available_width() - 44.0),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (enter || ui.add_enabled(can_run, egui::Button::new("Run")).clicked()) && can_run {
                let cmd = std::mem::take(&mut self.input_buf);
                let mut rt = runtime.lock().unwrap();
                rt.is_running = true;
                run_command(cmd, self.working_dir.clone(), rt.sender.clone());
            }
            // Re-focus the input after Enter so user can keep typing
            if enter { resp.request_focus(); }
        });

        // ── Controls row ──────────────────────────────────────────────────
        ui.horizontal(|ui| {
            if ui.small_button("Clear").clicked() {
                self.history.clear();
                self.last_output.clear();
            }
            ui.label(egui::RichText::new(format!("{} cmd{}", self.history.len(),
                if self.history.len() == 1 { "" } else { "s" })).small().color(dim));
            if is_running {
                ui.label(egui::RichText::new("● running")
                    .small().color(egui::Color32::from_rgb(255, 180, 60)));
            }
        });

        ui.separator();

        // ── Output ports (right-aligned) ──────────────────────────────────
        let out_preview = if self.last_output.is_empty() {
            "—".to_string()
        } else {
            crate::nodes::truncate_chars(&self.last_output, 18)
        };
        crate::nodes::output_port_row(ui, "Output", &out_preview,
            ctx.node_id, 0, ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects, PortKind::Text);

        crate::nodes::output_port_row(ui, "Done",
            if self.done_pulse { "1" } else { "0" },
            ctx.node_id, 1, ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects, PortKind::Trigger);

        // Keep repainting while a command is running
        if is_running { ui.ctx().request_repaint(); }
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("terminal", |state| {
        let mut n = TerminalNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
