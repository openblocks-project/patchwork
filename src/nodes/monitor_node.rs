//! MonitorNode — system profiler with inline output ports.
//!
//! Shows FPS, CPU, RAM, GPU, process metrics, node/wire count.
//! Output ports sit inline next to their values — no duplicate display.

use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Default)]
struct SystemMetrics {
    cpu_usage: f32,
    cpu_per_core: Vec<f32>,
    mem_used_gb: f32,
    mem_total_gb: f32,
    mem_percent: f32,
    gpu_name: String,
    process_mem_mb: f32,
    process_cpu: f32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct MonitorNode {
    #[serde(skip, default = "default_metrics")]
    metrics: Arc<Mutex<SystemMetrics>>,
    #[serde(skip)]
    fps_history: VecDeque<f32>,
    #[serde(skip)]
    cpu_history: VecDeque<f32>,
    #[serde(skip)]
    frame_times: VecDeque<f32>,
    #[serde(skip, default = "Instant::now")]
    last_frame: Instant,
    #[serde(skip)]
    node_count: usize,
    #[serde(skip)]
    connection_count: usize,
    #[serde(skip)]
    thread_started: bool,
}

fn default_metrics() -> Arc<Mutex<SystemMetrics>> {
    Arc::new(Mutex::new(SystemMetrics::default()))
}

impl std::fmt::Debug for MonitorNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MonitorNode").finish()
    }
}

impl Default for MonitorNode {
    fn default() -> Self {
        Self {
            metrics: default_metrics(),
            fps_history: VecDeque::with_capacity(60),
            cpu_history: VecDeque::with_capacity(60),
            frame_times: VecDeque::with_capacity(60),
            last_frame: Instant::now(),
            node_count: 0,
            connection_count: 0,
            thread_started: false,
        }
    }
}

impl MonitorNode {
    fn ensure_thread(&mut self) {
        if self.thread_started { return; }
        self.thread_started = true;

        let metrics = self.metrics.clone();
        std::thread::Builder::new()
            .name("monitor-sysinfo".to_string())
            .spawn(move || {
                use sysinfo::{System, Pid};
                let mut sys = System::new_all();
                let pid = Pid::from_u32(std::process::id());

                // GPU name (once)
                {
                    let name = get_gpu_name();
                    if let Ok(mut m) = metrics.lock() { m.gpu_name = name; }
                }

                loop {
                    sys.refresh_all();
                    let cpu = sys.global_cpu_usage();
                    let cores: Vec<f32> = sys.cpus().iter().map(|c| c.cpu_usage()).collect();
                    let mem_used = sys.used_memory() as f64 / 1_073_741_824.0;
                    let mem_total = sys.total_memory() as f64 / 1_073_741_824.0;
                    let mem_pct = if mem_total > 0.0 { (mem_used / mem_total * 100.0) as f32 } else { 0.0 };
                    let (proc_mem, proc_cpu) = sys.process(pid)
                        .map(|p| (p.memory() as f32 / 1_048_576.0, p.cpu_usage()))
                        .unwrap_or((0.0, 0.0));

                    // Normalize process CPU to overall percentage
                    // sysinfo returns per-core percentage (e.g., 200% = 2 full cores)
                    // Divide by core count to get percentage of total system CPU
                    let num_cores = cores.len().max(1) as f32;
                    let proc_cpu_normalized = proc_cpu / num_cores;

                    if let Ok(mut m) = metrics.lock() {
                        m.cpu_usage = cpu;
                        m.cpu_per_core = cores;
                        m.mem_used_gb = mem_used as f32;
                        m.mem_total_gb = mem_total as f32;
                        m.mem_percent = mem_pct;
                        m.process_mem_mb = proc_mem;
                        m.process_cpu = proc_cpu_normalized;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }).ok();
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;

        if dt > 0.0 {
            self.fps_history.push_back(1.0 / dt);
            self.frame_times.push_back(dt * 1000.0);
            if self.fps_history.len() > 60 { self.fps_history.pop_front(); }
            if self.frame_times.len() > 60 { self.frame_times.pop_front(); }
        }

        if let Ok(m) = self.metrics.lock() {
            self.cpu_history.push_back(m.cpu_usage);
            if self.cpu_history.len() > 60 { self.cpu_history.pop_front(); }
        }
    }
}

impl NodeBehavior for MonitorNode {
    fn title(&self) -> &str { "Monitor" }
    fn inputs(&self) -> Vec<PortDef> { vec![] }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("FPS", PortKind::Number),        // 0
            PortDef::new("Frame ms", PortKind::Number),   // 1
            PortDef::new("CPU %", PortKind::Number),       // 2 (system)
            PortDef::new("RAM %", PortKind::Number),       // 3 (system)
            PortDef::new("Proc MB", PortKind::Number),     // 4 (patchwork)
            PortDef::new("Nodes", PortKind::Number),       // 5
            PortDef::new("Proc CPU", PortKind::Number),    // 6 (patchwork CPU%)
            PortDef::new("Wires", PortKind::Number),       // 7
        ]
    }

    fn color_hint(&self) -> [u8; 3] { [255, 160, 60] }
    fn inline_ports(&self) -> bool { true }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        self.ensure_thread();
        self.tick();

        let fps = self.fps_history.back().copied().unwrap_or(0.0);
        let frame_ms = self.frame_times.back().copied().unwrap_or(0.0);
        let (cpu, ram, proc_mb) = self.metrics.lock()
            .map(|m| (m.cpu_usage, m.mem_percent, m.process_mem_mb))
            .unwrap_or((0.0, 0.0, 0.0));

        let proc_cpu = self.metrics.lock().map(|m| m.process_cpu).unwrap_or(0.0);
        vec![
            (0, PortValue::Float(fps)),
            (1, PortValue::Float(frame_ms)),
            (2, PortValue::Float(cpu)),
            (3, PortValue::Float(ram)),
            (4, PortValue::Float(proc_mb)),
            (5, PortValue::Float(self.node_count as f32)),
            (6, PortValue::Float(proc_cpu)),
            (7, PortValue::Float(self.connection_count as f32)),
        ]
    }

    fn type_tag(&self) -> &str { "monitor" }
    fn save_state(&self) -> serde_json::Value { serde_json::json!({}) }

    fn render_with_context(&mut self, ui: &mut eframe::egui::Ui, ctx: &mut RenderContext) {
        use eframe::egui;

        self.ensure_thread();

        let m = self.metrics.lock().ok().map(|m| m.clone()).unwrap_or_default();
        let fps = self.fps_history.back().copied().unwrap_or(0.0);
        let frame_ms = self.frame_times.back().copied().unwrap_or(0.0);
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // ── FPS + Frame ms (one line, ports inline) ──────────────
        let fps_color = if fps >= 55.0 { egui::Color32::from_rgb(80, 200, 80) }
            else if fps >= 30.0 { egui::Color32::from_rgb(200, 200, 80) }
            else { egui::Color32::from_rgb(255, 80, 80) };

        right_port_row(ui, ctx, 0, |ui| {
            ui.label(egui::RichText::new("FPS").small());
            ui.label(egui::RichText::new(format!("{:.0}", fps)).strong().color(fps_color));
        });
        right_port_row(ui, ctx, 1, |ui| {
            ui.label(egui::RichText::new("Frame").small());
            ui.label(egui::RichText::new(format!("{:.1}ms", frame_ms)).small().color(dim));
        });
        draw_sparkline(ui, &self.fps_history, fps_color, 0.0, 120.0);

        // ── System ───────────────────────────────────────────────
        ui.label(egui::RichText::new("System").small().strong().color(dim));

        // CPU
        let cpu_color = if m.cpu_usage < 50.0 { egui::Color32::from_rgb(80, 180, 255) }
            else if m.cpu_usage < 80.0 { egui::Color32::from_rgb(200, 200, 80) }
            else { egui::Color32::from_rgb(255, 80, 80) };

        right_port_row(ui, ctx, 2, |ui| {
            ui.label(egui::RichText::new("CPU").small());
            ui.label(egui::RichText::new(format!("{:.0}%", m.cpu_usage)).strong().color(cpu_color));
            ui.label(egui::RichText::new(format!("({}c)", m.cpu_per_core.len())).small().color(dim));
        });

        // Per-core bars (compact)
        if !m.cpu_per_core.is_empty() {
            let bar_h = 3.0;
            let total_w = ui.available_width();
            let bar_w = (total_w / m.cpu_per_core.len() as f32).max(2.0) - 1.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(total_w, bar_h), egui::Sense::hover());
            let painter = ui.painter();
            for (i, &usage) in m.cpu_per_core.iter().enumerate() {
                let x = rect.left() + i as f32 * (bar_w + 1.0);
                let bg = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(bar_w, bar_h));
                let fill_h = bar_h * (usage / 100.0).min(1.0);
                let fill = egui::Rect::from_min_size(
                    egui::pos2(x, rect.bottom() - fill_h), egui::vec2(bar_w, fill_h));
                painter.rect_filled(bg, 0.0, ui.visuals().extreme_bg_color);
                let c = if usage < 50.0 { egui::Color32::from_rgb(60, 140, 200) }
                    else if usage < 80.0 { egui::Color32::from_rgb(200, 180, 60) }
                    else { egui::Color32::from_rgb(220, 60, 60) };
                painter.rect_filled(fill, 0.0, c);
            }
        }

        // RAM
        right_port_row(ui, ctx, 3, |ui| {
            ui.label(egui::RichText::new("RAM").small());
            ui.label(egui::RichText::new(format!("{:.0}%", m.mem_percent)).strong().color(egui::Color32::from_rgb(200, 120, 255)));
            ui.label(egui::RichText::new(format!("{:.1}/{:.0}G", m.mem_used_gb, m.mem_total_gb)).small().color(dim));
        });

        // GPU
        if !m.gpu_name.is_empty() && m.gpu_name != "Unknown GPU" {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("GPU").small());
                ui.label(egui::RichText::new(&m.gpu_name).small().color(egui::Color32::from_rgb(180, 220, 100)));
            });
        }

        // ── Patchwork ────────────────────────────────────────────
        ui.label(egui::RichText::new("Patchwork").small().strong().color(dim));

        right_port_row(ui, ctx, 6, |ui| {
            ui.label(egui::RichText::new("CPU").small());
            ui.label(egui::RichText::new(format!("{:.1}%", m.process_cpu)).small());
        });
        right_port_row(ui, ctx, 4, |ui| {
            ui.label(egui::RichText::new("RAM").small());
            ui.label(egui::RichText::new(format!("{:.0}MB", m.process_mem_mb)).small().color(egui::Color32::from_rgb(255, 180, 80)));
        });

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{} nodes", self.node_count)).small().color(dim));
            crate::nodes::inline_port_circle(ui, ctx.node_id, 5, false, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(egui::RichText::new(format!("{} wires", self.connection_count)).small().color(dim));
            crate::nodes::inline_port_circle(ui, ctx.node_id, 7, false, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
        });

        ui.ctx().request_repaint();
    }
}

/// Render a row with labels on the left and an output port circle right-aligned.
fn right_port_row(
    ui: &mut eframe::egui::Ui,
    ctx: &mut crate::node_trait::RenderContext,
    port: usize,
    labels: impl FnOnce(&mut eframe::egui::Ui),
) {
    use eframe::egui;
    ui.horizontal(|ui| {
        labels(ui);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, port, false, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
        });
    });
}

fn draw_sparkline(ui: &mut eframe::egui::Ui, data: &VecDeque<f32>, color: eframe::egui::Color32, min: f32, max: f32) {
    use eframe::egui;

    let h = 18.0;
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);

    if data.len() < 2 { return; }

    let range = (max - min).max(0.001);
    let points: Vec<egui::Pos2> = data.iter().enumerate().map(|(i, &v)| {
        let x = rect.left() + (i as f32 / (data.len() - 1) as f32) * w;
        let y = rect.bottom() - ((v - min) / range).clamp(0.0, 1.0) * h;
        egui::pos2(x, y)
    }).collect();

    let fill_color = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 25);
    let mut fill_pts = points.clone();
    fill_pts.push(egui::pos2(rect.right(), rect.bottom()));
    fill_pts.push(egui::pos2(rect.left(), rect.bottom()));
    painter.add(egui::Shape::convex_polygon(fill_pts, fill_color, egui::Stroke::NONE));

    for window in points.windows(2) {
        painter.line_segment([window[0], window[1]], egui::Stroke::new(1.0, color));
    }
}

fn get_gpu_name() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("system_profiler")
            .args(["SPDisplaysDataType", "-detailLevel", "mini"])
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("Chipset Model:") || trimmed.starts_with("Chip:") {
                    return trimmed.split(':').nth(1).map(|s| s.trim().to_string()).unwrap_or_default();
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = std::process::Command::new("wmic")
            .args(["path", "win32_VideoController", "get", "name"])
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            // Skip header line ("Name"), take first non-empty data line
            for line in text.lines().skip(1) {
                let trimmed = line.trim();
                if !trimmed.is_empty() { return trimmed.to_string(); }
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = std::process::Command::new("lspci")
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                if line.contains("VGA") || line.contains("3D") || line.contains("Display") {
                    // Format: "XX:XX.X VGA compatible controller: NVIDIA ..."
                    if let Some(pos) = line.find(": ") {
                        return line[pos + 2..].trim().to_string();
                    }
                }
            }
        }
    }
    "Unknown GPU".to_string()
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("monitor", |_state| Box::new(MonitorNode::default()));
}
