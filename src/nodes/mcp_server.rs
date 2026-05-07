use crate::mcp::McpLog;
use eframe::egui;
use crate::nodes::ScrollAreaExt;

pub fn render(
    ui: &mut egui::Ui,
    mcp_log: &McpLog,
    _mcp_thread_running: bool,
) {
    let log_entries: Vec<String> = if let Ok(l) = mcp_log.lock() {
        l.clone()
    } else {
        vec![]
    };

    // Detect state from log content
    let has_client = log_entries.iter().any(|e| e.contains("Client initialized"));
    let _has_commands = log_entries.iter().any(|e| e.starts_with('→'));

    // Status
    ui.horizontal(|ui| {
        if has_client {
            ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "● Connected");
        } else if log_entries.iter().any(|e| e.contains("listening")) {
            ui.colored_label(egui::Color32::from_rgb(200, 200, 80), "◐ Listening");
        } else {
            ui.colored_label(egui::Color32::from_rgb(150, 150, 150), "○ Idle");
        }
    });

    if has_client {
        let cmd_count = log_entries.iter().filter(|e| e.starts_with('→')).count();
        ui.label(egui::RichText::new(format!("{} commands processed", cmd_count)).small().color(egui::Color32::GRAY));
    } else {
        ui.label(egui::RichText::new("Configure in Claude Desktop settings").small().color(egui::Color32::GRAY));
    }

    ui.separator();

    // Log
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Activity Log").strong().small());
        if ui.small_button("Clear").clicked() {
            if let Ok(mut l) = mcp_log.lock() {
                l.clear();
            }
        }
    });

    if log_entries.is_empty() {
        ui.colored_label(egui::Color32::from_rgb(80, 80, 80), "Waiting for MCP client...");
    } else {
        egui::ScrollArea::vertical().max_height(180.0).stick_to_bottom(true).show_pannable(ui, |ui| {
            for entry in log_entries.iter().rev().take(50).rev() {
                let color = if entry.starts_with('→') {
                    egui::Color32::from_rgb(100, 180, 255) // command in
                } else if entry.starts_with("← ERR") {
                    egui::Color32::from_rgb(255, 100, 100) // error
                } else if entry.starts_with('←') {
                    egui::Color32::from_rgb(100, 200, 100) // response out
                } else if entry.starts_with('✓') {
                    egui::Color32::from_rgb(80, 200, 80) // system
                } else {
                    egui::Color32::from_rgb(140, 140, 140) // info
                };
                ui.label(egui::RichText::new(entry).small().monospace().color(color));
            }
        });
    }
}
