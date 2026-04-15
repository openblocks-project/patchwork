use eframe::egui::{self, RichText, Sense};
use std::sync::Arc;
use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::transport::{TransportState, TransportCommand, ClockThread};

// ── Clock Node ───────────────────────────────────────────────────────────────
//
// Musical tempo clock with multiple synced output channels.
// Each channel has its own subdivision → Trigger + Phase output pair.
// All channels share the same transport, so they're perfectly in sync.
//
// Optional Sync input: wire another Clock's Beat output to lock this
// Clock to an external beat source (cross-Clock sync).
//
// Ports (dynamic):
//   In  0: BPM   (Number)
//   In  1: Sync  (Number — monotonic beats from another Clock)
//   Out [per channel]: Trigger, Phase
//   Out last-2: Beat, BPM

#[derive(Debug, Clone, Copy, PartialEq)]
struct SubdivPreset { label: &'static str, value: f32 }

const SUBDIV_PRESETS: &[SubdivPreset] = &[
    SubdivPreset { label: "÷4", value: 0.25 },
    SubdivPreset { label: "÷2", value: 0.5 },
    SubdivPreset { label: "×1", value: 1.0 },
    SubdivPreset { label: "×2", value: 2.0 },
    SubdivPreset { label: "×3", value: 3.0 },
    SubdivPreset { label: "×4", value: 4.0 },
    SubdivPreset { label: "×8", value: 8.0 },
];

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ClockChannel {
    subdivision: f32,
    #[serde(skip)]
    last_subdiv_beat: u64,
}

impl Default for ClockChannel {
    fn default() -> Self { Self { subdivision: 1.0, last_subdiv_beat: 0 } }
}

pub struct ClockNode {
    pub bpm: f32,
    pub beats_per_bar: u32,
    pub channels: Vec<ClockChannel>,
    // Own transport
    transport: Option<Arc<TransportState>>,
    command_tx: Option<crossbeam_channel::Sender<TransportCommand>>,
    // Cached
    beats: f64,
    beat_count: u64,
    running: bool,
    synced: bool,
}

impl std::fmt::Debug for ClockNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClockNode").field("bpm", &self.bpm).field("channels", &self.channels.len()).finish()
    }
}

impl Clone for ClockNode {
    fn clone(&self) -> Self {
        Self {
            bpm: self.bpm, beats_per_bar: self.beats_per_bar,
            channels: self.channels.clone(),
            transport: None, command_tx: None,
            beats: 0.0, beat_count: 0, running: false, synced: false,
        }
    }
}

impl Default for ClockNode {
    fn default() -> Self {
        Self {
            bpm: 120.0, beats_per_bar: 4,
            channels: vec![ClockChannel::default()],
            transport: None, command_tx: None,
            beats: 0.0, beat_count: 0, running: false, synced: false,
        }
    }
}

impl ClockNode {
    fn ensure_started(&mut self) {
        if self.transport.is_some() { return; }
        let state = Arc::new(TransportState::new(self.bpm as f64));
        let (tx, rx) = crossbeam_channel::unbounded();
        let cs = Arc::clone(&state);
        std::thread::Builder::new()
            .name("patchwork-clock".into())
            .spawn(move || ClockThread { state: cs, commands: rx }.run())
            .ok();
        self.transport = Some(state);
        self.command_tx = Some(tx);
    }

    fn send(&self, cmd: TransportCommand) {
        if let Some(ref tx) = self.command_tx { let _ = tx.send(cmd); }
    }

    // Output port layout:
    // Per channel i: port i*2 = Trigger, port i*2+1 = Phase
    // After all channels: Beat, BPM
    fn beat_port(&self) -> usize { self.channels.len() * 2 }
    fn bpm_port(&self) -> usize { self.channels.len() * 2 + 1 }
}

impl NodeBehavior for ClockNode {
    fn title(&self)      -> &str   { "Clock" }
    fn type_tag(&self)   -> &str   { "clock" }
    fn color_hint(&self) -> [u8;3] { [80, 220, 180] }
    fn min_width(&self)  -> Option<f32> { Some(220.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("BPM",  PortKind::Number),
            PortDef::new("Beat Sync", PortKind::Number),  // monotonic beats from another Clock
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        let mut ports = Vec::new();
        for (i, ch) in self.channels.iter().enumerate() {
            let label = if self.channels.len() == 1 {
                format!("Trigger")
            } else {
                format!("Trig ×{}", ch.subdivision)
            };
            ports.push(PortDef::dynamic(label, PortKind::Trigger));
            let plabel = if self.channels.len() == 1 {
                format!("Phase")
            } else {
                format!("Phase ×{}", ch.subdivision)
            };
            ports.push(PortDef::dynamic(plabel, PortKind::Normalized));
        }
        ports.push(PortDef::new("Beat", PortKind::Number));
        ports.push(PortDef::new("BPM",  PortKind::Number));
        ports
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        self.ensure_started();

        // BPM override
        if let Some(PortValue::Float(b)) = inputs.first() {
            if *b > 0.0 && (*b - self.bpm).abs() > 0.01 {
                self.bpm = *b;
                self.send(TransportCommand::SetBpm(self.bpm as f64));
            }
        }

        // Sync input — monotonic beats from another Clock
        let sync_beats = match inputs.get(1) {
            Some(PortValue::Float(b)) => Some(*b as f64),
            _ => None,
        };

        if let Some(ext_beats) = sync_beats {
            // Synced: use external beat counter
            self.beats = ext_beats;
            self.beat_count = ext_beats.floor() as u64;
            self.running = true;
            self.synced = true;
        } else if let Some(ref t) = self.transport {
            // Free: own transport
            self.beats = t.transport_beats.load();
            self.beat_count = t.beat_count.load(std::sync::atomic::Ordering::Relaxed);
            self.running = t.running.load(std::sync::atomic::Ordering::Relaxed);
            self.synced = false;
        }

        // Build outputs per channel
        let mut out = Vec::new();
        for (i, ch) in self.channels.iter_mut().enumerate() {
            let phase = ((self.beats * ch.subdivision as f64) % 1.0) as f32;
            let subdiv_beat = (self.beats * ch.subdivision as f64).floor() as u64;
            let trigger = if subdiv_beat > ch.last_subdiv_beat && (self.running || self.synced) {
                ch.last_subdiv_beat = subdiv_beat;
                1.0
            } else {
                if subdiv_beat < ch.last_subdiv_beat { ch.last_subdiv_beat = subdiv_beat; }
                0.0
            };
            out.push((i * 2,     PortValue::Float(trigger)));
            out.push((i * 2 + 1, PortValue::Float(phase)));
        }
        out.push((self.beat_port(), PortValue::Float(self.beat_count as f32)));
        out.push((self.bpm_port(),  PortValue::Float(self.bpm)));
        out
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim   = egui::Color32::from_rgb(110, 110, 120);
        let green = egui::Color32::from_rgb(80, 220, 120);
        let blue  = egui::Color32::from_rgb(100, 180, 240);

        self.ensure_started();

        // ── Input ports ──────────────────────────────────────────────────
        let bpm_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
        let sync_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(RichText::new("BPM").small().color(dim));
            if bpm_wired {
                ui.label(RichText::new(format!("{:.1}", self.bpm)).small().monospace().color(blue));
            } else {
                let old = self.bpm;
                ui.add(egui::DragValue::new(&mut self.bpm).speed(0.5).range(1.0..=999.0).max_decimals(1));
                if (self.bpm - old).abs() > 0.01 {
                    self.send(TransportCommand::SetBpm(self.bpm as f64));
                }
            }
        });

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 1, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(RichText::new("Beat Sync").small().color(
                if sync_wired { green } else { dim }));
            if sync_wired {
                ui.label(RichText::new("✓ locked").small().color(green));
            }
        });

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Transport controls ───────────────────────────────────────────
        if !sync_wired {
            ui.horizontal(|ui| {
                if ui.button(if self.running { "⏸" } else { "▶" }).clicked() {
                    if self.running { self.send(TransportCommand::Pause); }
                    else { self.send(TransportCommand::Play); }
                }
                if ui.button("⏹").clicked() {
                    self.send(TransportCommand::Stop);
                    for ch in &mut self.channels { ch.last_subdiv_beat = 0; }
                }
                ui.add_space(4.0);
                ui.label(RichText::new(format!("{:.0} BPM", self.bpm)).strong());
            });
        }

        // Bar / Beat
        let bpb = self.beats_per_bar.max(1);
        let bar = (self.beat_count / bpb as u64) + 1;
        let beat_in_bar = (self.beat_count % bpb as u64) + 1;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}:{}", bar, beat_in_bar))
                .monospace().strong().color(if self.running { green } else { dim }));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut bpb_val = self.beats_per_bar as f32;
                ui.add(egui::DragValue::new(&mut bpb_val).speed(0.1).range(1.0..=16.0).max_decimals(0).suffix("/bar"));
                self.beats_per_bar = bpb_val as u32;
            });
        });

        // Phase ring (first channel)
        let main_phase = if let Some(ch) = self.channels.first() {
            ((self.beats * ch.subdivision as f64) % 1.0) as f32
        } else { 0.0 };

        let ring_size = 40.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ring_size, ring_size), Sense::hover());
        let center = rect.center();
        let radius = ring_size * 0.4;
        let painter = ui.painter();
        painter.circle_stroke(center, radius, egui::Stroke::new(1.5, egui::Color32::from_rgb(40, 45, 55)));
        let segments = 24;
        let filled = (main_phase * segments as f32) as usize;
        for i in 0..segments {
            let a1 = std::f32::consts::TAU * (i as f32 / segments as f32) - std::f32::consts::FRAC_PI_2;
            let a2 = std::f32::consts::TAU * ((i + 1) as f32 / segments as f32) - std::f32::consts::FRAC_PI_2;
            let ri = radius - 4.0;
            let p1 = center + egui::vec2(a1.cos() * ri, a1.sin() * ri);
            let p2 = center + egui::vec2(a1.cos() * radius, a1.sin() * radius);
            let p3 = center + egui::vec2(a2.cos() * radius, a2.sin() * radius);
            let p4 = center + egui::vec2(a2.cos() * ri, a2.sin() * ri);
            let col = if i < filled {
                if self.running { egui::Color32::from_rgb(80, 220, 180) } else { egui::Color32::from_rgb(80, 80, 60) }
            } else { egui::Color32::from_rgb(30, 32, 38) };
            painter.add(egui::Shape::convex_polygon(vec![p1, p2, p3, p4], col, egui::Stroke::NONE));
        }

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Channels (add/remove) ────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Channels").small().strong().color(dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(RichText::new("+").color(green)).clicked() {
                    self.channels.push(ClockChannel::default());
                }
            });
        });

        let mut remove_idx: Option<usize> = None;
        for i in 0..self.channels.len() {
            let phase = ((self.beats * self.channels[i].subdivision as f64) % 1.0) as f32;
            let trig_port = i * 2;
            let phase_port = i * 2 + 1;

            ui.horizontal(|ui| {
                // Subdivision DragValue + dropdown
                ui.add(egui::DragValue::new(&mut self.channels[i].subdivision)
                    .speed(0.05).range(0.0625..=32.0).max_decimals(2).prefix("×"));
                egui::ComboBox::from_id_salt(egui::Id::new(("ch_subdiv", node_id, i)))
                    .width(42.0)
                    .selected_text("")
                    .show_ui(ui, |ui| {
                        for preset in SUBDIV_PRESETS {
                            if ui.selectable_label(
                                (self.channels[i].subdivision - preset.value).abs() < 0.001,
                                preset.label).clicked() {
                                self.channels[i].subdivision = preset.value;
                            }
                        }
                    });

                // Mini phase bar
                let bar_w = 30.0;
                let (br, _) = ui.allocate_exact_size(egui::vec2(bar_w, 6.0), Sense::hover());
                let p = ui.painter();
                p.rect_filled(br, 2.0, egui::Color32::from_rgb(30, 32, 38));
                let fw = br.width() * phase.clamp(0.0, 1.0);
                if fw > 0.5 {
                    p.rect_filled(egui::Rect::from_min_size(br.min, egui::vec2(fw, br.height())), 2.0,
                        egui::Color32::from_rgb(80, 220, 180));
                }

                // Output ports on right
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.channels.len() > 1 {
                        if ui.small_button(RichText::new("−").color(egui::Color32::from_rgb(220, 80, 80))).clicked() {
                            remove_idx = Some(i);
                        }
                    }
                    crate::nodes::inline_port_circle(ui, node_id, phase_port, false,
                        ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Normalized);
                    crate::nodes::inline_port_circle(ui, node_id, trig_port, false,
                        ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Trigger);
                });
            });
        }

        if let Some(idx) = remove_idx {
            self.channels.remove(idx);
            // Disconnect shifted ports
            let start = idx * 2;
            for p in start..=(self.channels.len() * 2 + 2) {
                ctx.pending_disconnects.push((node_id, p));
            }
        }

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Fixed output ports (Beat, BPM) ───────────────────────────────
        crate::nodes::output_port_row(ui, "Beat", &format!("{}", self.beat_count),
            node_id, self.beat_port(), ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Number);
        crate::nodes::output_port_row(ui, "BPM", &format!("{:.0}", self.bpm),
            node_id, self.bpm_port(), ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Number);

        if self.running || self.synced { ui.ctx().request_repaint(); }
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "bpm": self.bpm,
            "beats_per_bar": self.beats_per_bar,
            "channels": self.channels.iter().map(|ch| serde_json::json!({
                "subdivision": ch.subdivision,
            })).collect::<Vec<_>>(),
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(v) = state.get("bpm").and_then(|v| v.as_f64()) { self.bpm = v as f32; }
        if let Some(v) = state.get("beats_per_bar").and_then(|v| v.as_u64()) { self.beats_per_bar = v as u32; }
        if let Some(chs) = state.get("channels").and_then(|v| v.as_array()) {
            self.channels = chs.iter().map(|ch| {
                let subdiv = ch.get("subdivision").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                ClockChannel { subdivision: subdiv, last_subdiv_beat: 0 }
            }).collect();
            if self.channels.is_empty() { self.channels.push(ClockChannel::default()); }
        }
    }
}

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("clock", |state| {
        let mut n = ClockNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
