use crate::graph::{NodeId, PortValue};
use crate::osc::OscAction;
use eframe::egui;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    host: &mut String,
    port: &mut u16,
    address: &mut String,
    arg_count: &mut usize,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    osc_actions: &mut Vec<OscAction>,
) {
    // Host
    ui.horizontal(|ui| {
        ui.label("Host:");
        ui.add(egui::TextEdit::singleline(host).desired_width(100.0));
    });

    // Port
    ui.horizontal(|ui| {
        ui.label("Port:");
        let mut port_str = port.to_string();
        if ui
            .add(egui::TextEdit::singleline(&mut port_str).desired_width(60.0))
            .changed()
        {
            if let Ok(p) = port_str.parse::<u16>() {
                *port = p;
            }
        }
    });

    // OSC address
    ui.horizontal(|ui| {
        ui.label("Addr:");
        ui.add(egui::TextEdit::singleline(address).desired_width(100.0));
    });

    // Arg count with +/-
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Args").small().strong());
        if ui.small_button("+").clicked() {
            *arg_count += 1;
        }
        if *arg_count > 0 && ui.small_button("-").clicked() {
            *arg_count -= 1;
        }
        ui.label(format!("({})", arg_count));
    });

    // Show current arg values from inputs
    for i in 0..*arg_count {
        let val = values
            .get(&(node_id, i))
            .map(|v| v.as_float())
            .unwrap_or(0.0);
        ui.label(
            egui::RichText::new(format!("  [{}]: {:.3}", i, val))
                .small()
                .color(egui::Color32::GRAY),
        );
    }

    // Send on every frame if address is set
    if !host.is_empty() && *port > 0 && !address.is_empty() && *arg_count > 0 {
        let args: Vec<f32> = (0..*arg_count)
            .map(|i| {
                values
                    .get(&(node_id, i))
                    .map(|v| v.as_float())
                    .unwrap_or(0.0)
            })
            .collect();
        osc_actions.push(OscAction::Send {
            node_id,
            host: host.clone(),
            port: *port,
            address: address.clone(),
            args,
        });
        ui.label(
            egui::RichText::new("Sending...")
                .small()
                .color(egui::Color32::from_rgb(100, 200, 100)),
        );
    }
}
