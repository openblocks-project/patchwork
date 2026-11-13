#![allow(dead_code)]
use eframe::egui;
use std::collections::VecDeque;

const HISTORY_LEN: usize = 120;

pub struct MonitorState {
    pub fps_history: VecDeque<f32>,
    pub frame_ms_history: VecDeque<f32>,
    pub last_instant: std::time::Instant,
    pub node_count: usize,
    pub connection_count: usize,
    pub fps: f32,
    pub frame_ms: f32,
}

impl Default for MonitorState {
    fn default() -> Self {
        Self {
            fps_history: VecDeque::with_capacity(HISTORY_LEN),
            frame_ms_history: VecDeque::with_capacity(HISTORY_LEN),
            last_instant: std::time::Instant::now(),
            node_count: 0,
            connection_count: 0,
            fps: 0.0,
            frame_ms: 0.0,
        }
    }
}

impl MonitorState {
    pub fn tick(&mut self, node_count: usize, connection_count: usize) {
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.last_instant).as_secs_f32();
        self.last_instant = now;

        self.frame_ms = dt * 1000.0;
        self.fps = if dt > 0.0 { 1.0 / dt } else { 0.0 };
        self.node_count = node_count;
        self.connection_count = connection_count;

        if self.fps_history.len() >= HISTORY_LEN {
            self.fps_history.pop_front();
        }
        self.fps_history.push_back(self.fps);

        if self.frame_ms_history.len() >= HISTORY_LEN {
            self.frame_ms_history.pop_front();
        }
        self.frame_ms_history.push_back(self.frame_ms);
    }
}

pub fn render(ui: &mut egui::Ui, state: &MonitorState) {
    let fps_color = if state.fps >= 50.0 {
        egui::Color32::from_rgb(100, 255, 100)
    } else if state.fps >= 30.0 {
        egui::Color32::from_rgb(255, 200, 80)
    } else {
        egui::Color32::from_rgb(255, 80, 80)
    };

    // Title + FPS on same line
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Monitor")
                .small()
                .color(egui::Color32::GRAY),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.colored_label(
                fps_color,
                egui::RichText::new(format!("{:.0}", state.fps))
                    .strong()
                    .size(16.0),
            );
            ui.label("FPS:");
        });
    });

    // FPS sparkline
    draw_sparkline(ui, &state.fps_history, fps_color);

    // Frame time
    ui.horizontal(|ui| {
        ui.label("Frame:");
        ui.label(format!("{:.1} ms", state.frame_ms));
    });

    // Frame ms sparkline
    draw_sparkline(
        ui,
        &state.frame_ms_history,
        egui::Color32::from_rgb(100, 180, 255),
    );

    // Stats
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!(
                "Nodes: {}  Conn: {}",
                state.node_count, state.connection_count
            ))
            .small()
            .color(egui::Color32::GRAY),
        );
    });
}

fn draw_sparkline(ui: &mut egui::Ui, data: &VecDeque<f32>, color: egui::Color32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 25.0), egui::Sense::hover());
    if data.len() < 2 {
        return;
    }

    let max_val = data.iter().cloned().fold(f32::MIN, f32::max).max(1.0);
    let min_val = data.iter().cloned().fold(f32::MAX, f32::min).max(0.0);
    let range = (max_val - min_val).max(0.001);

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(25, 25, 30));

    let points: Vec<egui::Pos2> = data
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let x = rect.left() + (i as f32 / (data.len() - 1) as f32) * rect.width();
            let y = rect.bottom() - ((v - min_val) / range) * rect.height();
            egui::pos2(x, y)
        })
        .collect();

    for w in points.windows(2) {
        painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, color));
    }
}
