use crate::graph::{NodeId, NetPortSchema, NetPortKind, PortValue, Connection};
use crate::network::{NetworkAction, NetPort, PortSchema, PortSchemaKind};
use std::collections::HashMap;
use eframe::egui;

pub fn render(
    ui: &mut egui::Ui,
    schema: &mut Vec<NetPortSchema>,
    identity_mode: &mut u8,
    persistent_secret: &mut Option<String>,
    label: &mut String,
    last_link: &mut Option<String>,
    last_peer_count: &mut usize,
    initialized: &mut bool,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    network_actions: &mut Vec<NetworkAction>,
) {
    let accent = egui::Color32::from_rgb(80, 200, 160);
    let dim = egui::Color32::from_rgb(140, 140, 140);

    // Label
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Label").small().color(dim));
        ui.add(egui::TextEdit::singleline(label).desired_width(120.0).font(egui::TextStyle::Small));
    });

    // Schema editor
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Ports").small().strong().color(accent));
        if ui.small_button("+").clicked() {
            schema.push(NetPortSchema {
                name: format!("port{}", schema.len()),
                kind: NetPortKind::Float,
            });
            *initialized = false;
        }
    });

    let mut remove_idx = None;
    let schema_len = schema.len();
    for i in 0..schema_len {
        ui.horizontal(|ui| {
            let port = &mut schema[i];
            ui.add(egui::TextEdit::singleline(&mut port.name).desired_width(60.0).font(egui::TextStyle::Small));
            egui::ComboBox::from_id_salt(("net_kind", node_id, i))
                .width(50.0)
                .selected_text(match port.kind {
                    NetPortKind::Float => "Float",
                    NetPortKind::Text => "Text",
                    NetPortKind::Image => "Image",
                })
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut port.kind, NetPortKind::Float, "Float").changed() { *initialized = false; }
                    if ui.selectable_value(&mut port.kind, NetPortKind::Text, "Text").changed() { *initialized = false; }
                    if ui.selectable_value(&mut port.kind, NetPortKind::Image, "Image").changed() { *initialized = false; }
                });
            if schema_len > 1 && ui.small_button("-").clicked() {
                remove_idx = Some(i);
            }
        });
    }
    if let Some(idx) = remove_idx {
        schema.remove(idx);
        *initialized = false;
    }

    // Identity mode
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Identity").small().color(dim));
        if ui.selectable_label(*identity_mode == 0, "Ephemeral").clicked() && *identity_mode != 0 {
            *identity_mode = 0;
            *persistent_secret = None;
            *initialized = false;
        }
        if ui.selectable_label(*identity_mode == 1, "Permanent").clicked() && *identity_mode != 1 {
            *identity_mode = 1;
            *initialized = false;
        }
    });

    // Initialize / reinitialize when needed
    if !*initialized {
        let port_schema: Vec<PortSchema> = schema.iter().map(|s| PortSchema {
            name: s.name.clone(),
            kind: match s.kind {
                NetPortKind::Float => PortSchemaKind::Float,
                NetPortKind::Text => PortSchemaKind::Text,
                NetPortKind::Image => PortSchemaKind::Image,
            },
        }).collect();

        network_actions.push(NetworkAction::CreateSend {
            node_id,
            schema: port_schema,
            secret_key_hex: persistent_secret.clone(),
            label: label.clone(),
        });
        *initialized = true;
    }

    // Link display
    ui.separator();
    if let Some(link) = last_link {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Link").small().color(accent));
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(link.clone());
            }
        });
        let short = if link.chars().count() > 40 {
            let head: String = link.chars().take(40).collect();
            format!("{}...", head)
        } else {
            link.clone()
        };
        ui.label(egui::RichText::new(short).small().monospace().color(dim));
    } else {
        ui.label(egui::RichText::new("Initializing...").small().color(dim));
    }

    // Status
    ui.label(egui::RichText::new(format!("{} peers", last_peer_count)).small().color(
        if *last_peer_count > 0 { egui::Color32::from_rgb(80, 255, 80) } else { dim }
    ));

    // Send values every frame — read from connected source nodes' outputs
    if last_link.is_some() {
        let mut net_values = Vec::with_capacity(schema.len());
        for (i, s) in schema.iter().enumerate() {
            // Follow connection: find which output port feeds this input port
            let input_val = connections.iter()
                .find(|c| c.to_node == node_id && c.to_port == i)
                .and_then(|c| values.get(&(c.from_node, c.from_port)));
            match s.kind {
                NetPortKind::Float => {
                    let f = input_val.map(|v| v.as_float()).unwrap_or(0.0);
                    net_values.push(NetPort::Float(f));
                }
                NetPortKind::Text => {
                    let t = input_val.map(|v| format!("{}", v)).unwrap_or_default();
                    net_values.push(NetPort::Text(t));
                }
                NetPortKind::Image => {
                    net_values.push(NetPort::None);
                }
            }
        }
        network_actions.push(NetworkAction::SendValues {
            node_id,
            values: net_values,
        });
    }
}
