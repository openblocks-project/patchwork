use eframe::egui;
use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── Node struct ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PrinterNode {
    // ── Persisted state ──
    pub selected_printer: String,
    pub document_path: String,
    pub copies: u32,
    pub fit_to_page: bool,

    // ── Runtime (not saved) ──
    cached_printers: Vec<String>,
    printers_loaded: bool,
    last_trigger: f32,
    pub status: String,
}

impl Default for PrinterNode {
    fn default() -> Self {
        Self {
            selected_printer: String::new(),
            document_path: String::new(),
            copies: 1,
            fit_to_page: true,
            cached_printers: Vec::new(),
            printers_loaded: false,
            last_trigger: 0.0,
            status: "Ready".into(),
        }
    }
}

// ── Printer enumeration ───────────────────────────────────────────────────────

fn enumerate_printers() -> Vec<String> {
    #[cfg(target_os = "windows")]
    {
        let out = std::process::Command::new("wmic")
            .args(["printer", "get", "name"])
            .output();
        if let Ok(out) = out {
            let text = String::from_utf8_lossy(&out.stdout);
            return text.lines()
                .skip(1) // header "Name"
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
        }
        Vec::new()
    }
    #[cfg(not(target_os = "windows"))]
    {
        let out = std::process::Command::new("lpstat")
            .arg("-a")
            .output();
        if let Ok(out) = out {
            let text = String::from_utf8_lossy(&out.stdout);
            return text.lines()
                .filter_map(|l| l.split_whitespace().next())
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        Vec::new()
    }
}

// ── Print dispatch ────────────────────────────────────────────────────────────

fn send_to_printer(
    printer: &str,
    file: &str,
    copies: u32,
    fit_to_page: bool,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let _ = fit_to_page;
        std::process::Command::new("print")
            .args([&format!("/D:{}", printer), file])
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut args: Vec<String> = vec![
            "-d".into(), printer.to_string(),
            "-n".into(), copies.to_string(),
        ];
        if fit_to_page {
            args.push("-o".into());
            args.push("fit-to-page".into());
        }
        args.push(file.to_string());
        std::process::Command::new("lp")
            .args(&args)
            .spawn()
            .map_err(|e| format!("lp error: {}", e))?;
        Ok(())
    }
}

fn save_image_to_temp(pv: &PortValue) -> Result<String, String> {
    let img = match pv {
        PortValue::Image(arc) => arc.clone(),
        _ => return Err("No image data".into()),
    };
    if img.pixels.is_empty() {
        return Err("Empty image".into());
    }
    let path = std::env::temp_dir().join("patchwork_print.png");
    // Write RGBA pixels as PNG using the `image` crate
    let rgba = image::RgbaImage::from_raw(img.width, img.height, img.pixels.clone())
        .ok_or("Invalid image dimensions")?;
    rgba.save(&path).map_err(|e| e.to_string())?;
    path.to_str().map(|s| s.to_string()).ok_or("Bad temp path".into())
}

impl PrinterNode {
    fn do_print(&mut self, image_override: Option<PortValue>) {
        // Determine file to print
        let file: String = if !self.document_path.is_empty() {
            // Explicit document path takes priority
            self.document_path.clone()
        } else if let Some(pv) = image_override.as_ref() {
            // Image input — save to temp
            match save_image_to_temp(pv) {
                Ok(p) => p,
                Err(e) => { self.status = format!("Image error: {}", e); return; }
            }
        } else {
            self.status = "Nothing to print".into();
            return;
        };

        let printer = if self.selected_printer.is_empty() {
            // Use system default — omit -d flag by passing empty string sentinel
            String::new()
        } else {
            self.selected_printer.clone()
        };

        // Build command — allow empty printer to use system default (no -d flag)
        let result = if printer.is_empty() {
            #[cfg(not(target_os = "windows"))]
            {
                let mut args: Vec<String> = vec!["-n".into(), self.copies.to_string()];
                if self.fit_to_page { args.push("-o".into()); args.push("fit-to-page".into()); }
                args.push(file.clone());
                std::process::Command::new("lp").args(&args).spawn()
                    .map(|_| ()).map_err(|e| format!("lp error: {}", e))
            }
            #[cfg(target_os = "windows")]
            { Err("Select a printer on Windows".to_string()) }
        } else {
            send_to_printer(&printer, &file, self.copies, self.fit_to_page)
        };

        match result {
            Ok(_) => {
                let name = if printer.is_empty() { "default printer".to_string() } else { printer };
                self.status = format!("Sent → {}", name);
            }
            Err(e) => {
                self.status = format!("Error: {}", e);
            }
        }
    }
}

// ── NodeBehavior ──────────────────────────────────────────────────────────────

impl NodeBehavior for PrinterNode {
    fn title(&self)        -> &str    { "Printer" }
    fn type_tag(&self)     -> &str    { "printer" }
    fn color_hint(&self)   -> [u8; 3] { [180, 100, 220] }
    fn inline_ports(&self) -> bool    { true }
    fn min_width(&self)    -> Option<f32> { Some(220.0) }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Trigger", PortKind::Trigger),
            PortDef::new("Image",   PortKind::Image),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Status", PortKind::Text)]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Trigger detection is handled in render_with_context() so it runs
        // every frame even when an image input is connected (the image-input
        // check in app/mod.rs skips Dynamic evaluate() for image nodes).
        vec![(0, PortValue::Text(self.status.clone()))]
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "selected_printer": self.selected_printer,
            "document_path":    self.document_path,
            "copies":           self.copies,
            "fit_to_page":      self.fit_to_page,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(v) = state.get("selected_printer").and_then(|v| v.as_str()) { self.selected_printer = v.into(); }
        if let Some(v) = state.get("document_path").and_then(|v| v.as_str())    { self.document_path = v.into(); }
        if let Some(v) = state.get("copies").and_then(|v| v.as_u64())           { self.copies = v.clamp(1, 99) as u32; }
        if let Some(v) = state.get("fit_to_page").and_then(|v| v.as_bool())     { self.fit_to_page = v; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;

        // ── Lazy printer enumeration ──────────────────────────────────────
        if !self.printers_loaded {
            self.cached_printers = enumerate_printers();
            self.printers_loaded = true;
        }

        // ── Rising-edge trigger detection (done here, not in evaluate(), ──
        // ── because evaluate() is skipped when an image input is connected) ──
        {
            let trig = crate::graph::Graph::static_input_value(
                ctx.connections, ctx.values, node_id, 0
            ).as_float();
            let rising = trig > 0.5 && self.last_trigger <= 0.5;
            self.last_trigger = trig;
            if rising {
                let img_val = crate::graph::Graph::static_input_value(
                    ctx.connections, ctx.values, node_id, 1
                );
                let img = if matches!(img_val, PortValue::None) { None } else { Some(img_val) };
                self.do_print(img);
            }
        }

        let dim = egui::Color32::from_rgb(100, 100, 110);
        let accent = egui::Color32::from_rgb(180, 100, 220);

        // ── Trigger input port ────────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 0, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Trigger,
            );
            ui.label(egui::RichText::new("Trigger").small().color(dim));
        });

        // ── Image input port ──────────────────────────────────────────────
        let img_connected = ctx.connections.iter()
            .any(|c| c.to_node == node_id && c.to_port == 1);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 1, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Image,
            );
            let lbl = if img_connected { "Image ✓" } else { "Image" };
            let col  = if img_connected { egui::Color32::from_rgb(160, 120, 200) } else { dim };
            ui.label(egui::RichText::new(lbl).small().color(col));
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Printer dropdown ──────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Printer").small());
            let selected_text = if self.selected_printer.is_empty() {
                "System Default"
            } else {
                self.selected_printer.as_str()
            };
            egui::ComboBox::from_id_salt(egui::Id::new(("printer_sel", node_id)))
                .selected_text(selected_text)
                .width(130.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(self.selected_printer.is_empty(), "System Default").clicked() {
                        self.selected_printer.clear();
                    }
                    for p in self.cached_printers.clone() {
                        if ui.selectable_label(self.selected_printer == p, &p).clicked() {
                            self.selected_printer = p;
                        }
                    }
                });
            // Refresh button
            if ui.small_button("⟳").on_hover_text("Re-scan printers").clicked() {
                self.cached_printers = enumerate_printers();
            }
        });

        // ── Document file path ────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("File").small());
            let short = if self.document_path.is_empty() {
                "(image input)".to_string()
            } else {
                // Show just filename
                std::path::Path::new(&self.document_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| self.document_path.clone())
            };
            ui.label(egui::RichText::new(short).small().color(
                if self.document_path.is_empty() { dim } else { egui::Color32::WHITE }
            ));
            if ui.small_button("📂").on_hover_text("Browse for document").clicked() {
                if let Some(fp) = rfd::FileDialog::new()
                    .add_filter("PDF", &["pdf"])
                    .add_filter("Images", &["png", "jpg", "jpeg", "bmp"])
                    .add_filter("Text", &["txt"])
                    .add_filter("All files", &["*"])
                    .pick_file()
                {
                    self.document_path = fp.display().to_string();
                }
            }
            // Clear button
            if !self.document_path.is_empty() {
                if ui.small_button("✕").on_hover_text("Clear file (use image input)").clicked() {
                    self.document_path.clear();
                }
            }
        });

        // ── Copies + fit-to-page ──────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Copies").small());
            if ui.small_button("−").clicked() && self.copies > 1 { self.copies -= 1; }
            ui.label(egui::RichText::new(self.copies.to_string()).small().monospace());
            if ui.small_button("+").clicked() && self.copies < 99 { self.copies += 1; }
            ui.add_space(8.0);
            ui.checkbox(&mut self.fit_to_page, egui::RichText::new("Fit").small());
        });

        ui.add_space(4.0);

        // ── Print button + status ─────────────────────────────────────────
        ui.horizontal(|ui| {
            let is_ready = !self.status.starts_with("Printing");
            let btn = egui::Button::new(
                egui::RichText::new("▶ Print").small().color(accent)
            );
            if ui.add_enabled(is_ready, btn).clicked() {
                // Button path: grab current image input from graph values
                let img_val = crate::graph::Graph::static_input_value(
                    ctx.connections, ctx.values, node_id, 1
                );
                let img = if matches!(img_val, PortValue::None) { None } else { Some(img_val) };
                self.do_print(img);
            }

            // Status indicator
            let (status_text, status_color) = if self.status.starts_with("Error") {
                (self.status.as_str(), egui::Color32::from_rgb(220, 80, 80))
            } else if self.status.starts_with("Sent") {
                (self.status.as_str(), egui::Color32::from_rgb(80, 200, 100))
            } else {
                (self.status.as_str(), dim)
            };
            ui.label(egui::RichText::new(status_text).small().color(status_color));
        });

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Status output port ────────────────────────────────────────────
        crate::nodes::output_port_row(
            ui, "Status", &self.status, node_id, 0,
            ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects,
            PortKind::Text,
        );
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("printer", |state| {
        let mut n = PrinterNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
