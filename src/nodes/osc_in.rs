use crate::graph::NodeId;
use crate::osc::OscAction;
use eframe::egui;
use crate::nodes::ScrollAreaExt;

pub fn render(
    ui: &mut egui::Ui,
    port: &mut u16,
    address_filter: &mut String,
    arg_count: &mut usize,
    last_args: &mut Vec<f32>,
    last_args_text: &mut Vec<String>,
    log: &mut Vec<String>,
    listening: &mut bool,
    discovered: &mut Vec<(String, usize, String)>,
    node_id: NodeId,
    is_listening: bool,
    osc_actions: &mut Vec<OscAction>,
) {
    let accent = egui::Color32::from_rgb(60, 160, 220);
    let dim = egui::Color32::from_rgb(140, 140, 140);

    // ── Header: Port + Listen ──
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Port").small().color(dim));
        let mut port_str = port.to_string();
        if ui.add(egui::TextEdit::singleline(&mut port_str).desired_width(50.0).font(egui::TextStyle::Small)).changed() {
            if let Ok(p) = port_str.parse::<u16>() { *port = p; }
        }

        if is_listening {
            let dot = egui::RichText::new("●").color(egui::Color32::from_rgb(80, 255, 80)).small();
            ui.label(dot);
            if ui.small_button("Stop").clicked() {
                *listening = false;
                osc_actions.push(OscAction::StopListening { node_id });
            }
        } else {
            if ui.add_enabled(*port > 0, egui::Button::new("Listen").small()).clicked() {
                *listening = true;
                osc_actions.push(OscAction::StartListening { node_id, port: *port });
            }
        }
    });

    // ── Live Log (always visible when listening) ──
    if !log.is_empty() {
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Log").small().strong().color(dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Clear").clicked() {
                    log.clear();
                }
            });
        });
        egui::ScrollArea::vertical()
            .id_salt(("osc_log", node_id))
            .max_height(80.0)
            .stick_to_bottom(true)
            .show_pannable(ui, |ui| {
                for msg in log.iter().rev().take(50).collect::<Vec<_>>().into_iter().rev() {
                    ui.label(egui::RichText::new(msg).small().monospace()
                        .color(egui::Color32::from_rgb(170, 170, 170)));
                }
            });
    }

    // ── Discovered Addresses ──
    if !discovered.is_empty() {
        ui.separator();
        ui.label(egui::RichText::new("Discovered").small().strong().color(accent));

        let mut use_addr: Option<(String, usize)> = None;
        let mut spawn_addr: Option<(String, usize)> = None;

        for (addr, argc, preview) in discovered.iter() {
            let is_selected = !address_filter.is_empty() && addr.contains(address_filter.as_str());

            ui.horizontal(|ui| {
                // Colored address label
                let addr_color = if is_selected {
                    egui::Color32::from_rgb(80, 255, 120)
                } else {
                    egui::Color32::from_rgb(180, 200, 220)
                };
                ui.label(egui::RichText::new(addr).small().monospace().color(addr_color));

                ui.label(egui::RichText::new(format!("({})", argc)).small().color(dim));

                // "Use" button — auto-configures this node for this address
                if !is_selected {
                    if ui.small_button("Use").clicked() {
                        use_addr = Some((addr.clone(), *argc));
                    }
                }
                // "+" button — spawn a new OscIn pre-configured for this address
                if ui.small_button("+").clicked() {
                    spawn_addr = Some((addr.clone(), *argc));
                }
            });

            // Preview of last values
            if !preview.is_empty() {
                ui.indent(addr, |ui| {
                    ui.label(egui::RichText::new(preview).small().monospace().color(dim));
                });
            }
        }

        // Apply "Use" selection
        if let Some((addr, argc)) = use_addr {
            *address_filter = addr;
            *arg_count = argc;
            // Resize last_args to match
            last_args.resize(argc, 0.0);
        }

        // Store spawn request in egui temp data (picked up by app layer)
        if let Some((addr, argc)) = spawn_addr {
            ui.ctx().data_mut(|d| {
                d.insert_temp(
                    egui::Id::new(("osc_spawn", node_id)),
                    (addr, argc, *port),
                );
            });
        }
    }

    // ── Address Filter + Outputs ──
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Address").small().color(dim));
        ui.add(egui::TextEdit::singleline(address_filter).desired_width(120.0).font(egui::TextStyle::Small).hint_text("/path"));
    });

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Args").small().color(dim));
        if ui.small_button("+").clicked() {
            *arg_count += 1;
            last_args.push(0.0);
        }
        if *arg_count > 0 && ui.small_button("−").clicked() {
            *arg_count = arg_count.saturating_sub(1);
            last_args.truncate(*arg_count);
        }
        ui.label(egui::RichText::new(format!("{}", arg_count)).small().strong());
    });

    // ── Current Output Values ──
    if *arg_count > 0 {
        for i in 0..*arg_count {
            let fval = last_args.get(i).copied().unwrap_or(0.0);
            let tval = last_args_text.get(i).map(|s| s.as_str()).unwrap_or("");
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("[{}]", i)).small().monospace().color(dim));
                ui.label(egui::RichText::new(format!("{:.3}", fval)).small().monospace()
                    .color(egui::Color32::from_rgb(120, 200, 120)));
                if !tval.is_empty() && tval != &format!("{:.4}", fval) {
                    ui.label(egui::RichText::new(tval).small().monospace()
                        .color(egui::Color32::from_rgb(200, 180, 120)));
                }
            });
        }
        // Raw text output indicator
        ui.label(egui::RichText::new(format!("[Raw] {}", last_args_text.join(", "))).small().monospace().color(dim));
    }
}
