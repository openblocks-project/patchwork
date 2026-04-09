use crate::graph::{NodeId, NetPortSchema, NetPortKind};
use crate::network::{NetworkAction, PortSchemaKind, decode_link};
use eframe::egui;

pub fn render(
    ui: &mut egui::Ui,
    link: &mut String,
    imported_schema: &mut Vec<NetPortSchema>,
    _last_values: &mut Vec<f32>,
    _last_values_text: &mut Vec<String>,
    last_sender: &mut String,
    status: &mut String,
    connected: &mut bool,
    node_id: NodeId,
    network_actions: &mut Vec<NetworkAction>,
) {
    let accent = egui::Color32::from_rgb(160, 80, 200);
    let dim = egui::Color32::from_rgb(140, 140, 140);

    // Link input
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Link").small().color(dim));
        if ui.small_button("Paste").clicked() {
            if let Some(clip) = ui.input(|i| i.events.iter().find_map(|e| {
                if let egui::Event::Paste(s) = e { Some(s.clone()) } else { None }
            })) {
                *link = clip;
            } else {
                // Try clipboard via output
                ui.output_mut(|_o| {});
            }
        }
    });

    let mut link_changed = false;
    if ui.add(egui::TextEdit::singleline(link).desired_width(180.0).font(egui::TextStyle::Small).hint_text("patchwork://...")).changed() {
        link_changed = true;
    }

    // On link change, decode and connect
    if link_changed && !link.is_empty() {
        match decode_link(link) {
            Ok(parsed) => {
                *imported_schema = parsed.schema.iter().map(|s| NetPortSchema {
                    name: s.name.clone(),
                    kind: match s.kind {
                        PortSchemaKind::Float => NetPortKind::Float,
                        PortSchemaKind::Text => NetPortKind::Text,
                        PortSchemaKind::Image => NetPortKind::Image,
                    },
                }).collect();
                *status = format!("Connecting to '{}'...", parsed.label);
                *connected = false;

                network_actions.push(NetworkAction::CreateReceive {
                    node_id,
                    link: link.clone(),
                });
            }
            Err(e) => {
                *status = format!("Error: {}", e);
                *connected = false;
                imported_schema.clear();
            }
        }
    }

    // Show imported schema
    if !imported_schema.is_empty() {
        ui.separator();
        ui.label(egui::RichText::new("Outputs").small().strong().color(accent));
        for s in imported_schema.iter() {
            let kind_str = match s.kind {
                NetPortKind::Float => "Float",
                NetPortKind::Text => "Text",
                NetPortKind::Image => "Image",
            };
            ui.label(egui::RichText::new(format!("  {} ({})", s.name, kind_str)).small().color(dim));
        }
    }

    // Sender info
    if !last_sender.is_empty() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("From").small().color(dim));
            ui.label(egui::RichText::new(last_sender.as_str()).small().monospace().color(accent));
        });
    }

    // Status
    ui.separator();
    let status_color = if *connected {
        egui::Color32::from_rgb(80, 255, 80)
    } else if status.starts_with("Error") {
        egui::Color32::from_rgb(255, 80, 80)
    } else {
        dim
    };
    ui.horizontal(|ui| {
        if *connected {
            ui.label(egui::RichText::new("●").color(egui::Color32::from_rgb(80, 255, 80)).small());
        }
        ui.label(egui::RichText::new(status.as_str()).small().color(status_color));
    });
}
