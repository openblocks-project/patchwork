use std::collections::HashSet;
use eframe::egui::{self, RichText};
use crate::nodes::ScrollAreaExt;
use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── JsonFieldsNode ────────────────────────────────────────────────────────────
//
// Generic JSON demultiplexer with dynamic output ports.
//
// Accepts any JSON string, displays a collapsible tree view of all JSON
// values, and lets the user pin specific leaf paths as named output ports.
//
// Works with:
//   • Hand Tracking JSON  (hand.wrist.x, hand.bbox.x1, …)
//   • REST API responses  (data.users[0].name, …)
//   • Any arbitrary JSON

#[derive(Debug, Clone)]
pub struct JsonFieldsNode {
    /// Persisted — paths the user has pinned as output ports
    pub pinned_paths: Vec<String>,
    /// Runtime — parsed JSON tree
    parsed: Option<serde_json::Value>,
    /// Runtime — current value string for each pinned path
    output_vals: Vec<String>,
    /// Runtime — which tree nodes are expanded (by path prefix)
    expanded: HashSet<String>,
}

impl Default for JsonFieldsNode {
    fn default() -> Self {
        Self {
            pinned_paths: Vec::new(),
            parsed:       None,
            output_vals:  Vec::new(),
            expanded:     HashSet::new(),
        }
    }
}

// ── Value at path ─────────────────────────────────────────────────────────────

fn get_value_at_path<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cur = root;
    for part in path_segments(path) {
        match part {
            Seg::Key(k) => {
                if let serde_json::Value::Object(map) = cur {
                    cur = map.get(k)?;
                } else {
                    return None;
                }
            }
            Seg::Idx(i) => {
                if let serde_json::Value::Array(arr) = cur {
                    cur = arr.get(i)?;
                } else {
                    return None;
                }
            }
        }
    }
    Some(cur)
}

enum Seg<'a> { Key(&'a str), Idx(usize) }

fn path_segments(path: &str) -> Vec<Seg<'_>> {
    let mut segs = Vec::new();
    let mut remaining = path;
    while !remaining.is_empty() {
        if remaining.starts_with('[') {
            // array index
            let end = remaining.find(']').unwrap_or(remaining.len() - 1);
            if let Ok(i) = remaining[1..end].parse::<usize>() {
                segs.push(Seg::Idx(i));
            }
            remaining = &remaining[end + 1..];
            if remaining.starts_with('.') { remaining = &remaining[1..]; }
        } else {
            let dot = remaining.find('.').unwrap_or(remaining.len());
            let bracket = remaining.find('[').unwrap_or(remaining.len());
            let end = dot.min(bracket);
            segs.push(Seg::Key(&remaining[..end]));
            remaining = &remaining[end..];
            if remaining.starts_with('.') { remaining = &remaining[1..]; }
        }
    }
    segs
}

fn leaf_value_str(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Null       => Some("null".into()),
        serde_json::Value::Bool(b)    => Some(b.to_string()),
        serde_json::Value::Number(n)  => Some(n.to_string()),
        serde_json::Value::String(s)  => Some(s.clone()),
        _                             => None,
    }
}

fn is_numeric_str(s: &str) -> bool {
    s.parse::<f64>().is_ok()
}

fn child_count(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::Object(m) => m.len(),
        serde_json::Value::Array(a)  => a.len(),
        _                            => 0,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

impl JsonFieldsNode {
    fn emit_pinned_vals(&self) -> Vec<(usize, PortValue)> {
        self.pinned_paths.iter().enumerate().map(|(i, _)| {
            let val_str = self.output_vals.get(i).map(|s| s.as_str()).unwrap_or("0");
            let pv = if let Ok(f) = val_str.parse::<f64>() {
                PortValue::Float(f as f32)
            } else {
                PortValue::Text(val_str.to_string())
            };
            (i, pv)
        }).collect()
    }

    fn refresh_output_vals(&mut self) {
        if let Some(ref root) = self.parsed {
            self.output_vals = self.pinned_paths.iter().map(|path| {
                get_value_at_path(root, path)
                    .and_then(|v| leaf_value_str(v))
                    .unwrap_or_else(|| "0".to_string())
            }).collect();
        }
    }

    /// Auto-expand the first level of the tree when new JSON arrives
    fn auto_expand_root(&mut self) {
        if let Some(ref root) = self.parsed {
            match root {
                serde_json::Value::Object(map) => {
                    // Expand root + any single-child paths
                    self.expanded.insert("".to_string());
                    for (k, v) in map {
                        if !matches!(v, serde_json::Value::Null | serde_json::Value::Bool(_)
                            | serde_json::Value::Number(_) | serde_json::Value::String(_)) {
                            self.expanded.insert(k.clone());
                        }
                    }
                }
                serde_json::Value::Array(_) => {
                    self.expanded.insert("".to_string());
                }
                _ => {}
            }
        }
    }
}

// ── Tree rendering ────────────────────────────────────────────────────────────

type PortPositions = std::collections::HashMap<(crate::graph::NodeId, usize, bool), eframe::egui::Pos2>;

struct TreeCtx<'a> {
    node_id:      crate::graph::NodeId,
    pinned_paths: &'a mut Vec<String>,
    output_vals:  &'a mut Vec<String>,
    expanded:     &'a mut HashSet<String>,
    pin_req:      &'a mut Option<(String, String)>,  // (path, leaf_val) to pin
    remove_req:   &'a mut Option<String>,            // path to unpin
    connections:  &'a [crate::graph::Connection],
    port_positions: &'a mut PortPositions,
    dragging_from: &'a mut Option<(crate::graph::NodeId, usize, bool)>,
    pending_disconnects: &'a mut Vec<(crate::graph::NodeId, usize)>,
}

fn render_tree(
    ui: &mut egui::Ui,
    val: &serde_json::Value,
    path: &str,       // dotted path so far (empty = root)
    key_label: &str,  // display label for this node
    depth: usize,
    ctx: &mut TreeCtx<'_>,
) {
    let dim    = egui::Color32::from_rgb(110, 110, 120);
    let green  = egui::Color32::from_rgb(80, 210, 130);
    let blue   = egui::Color32::from_rgb(120, 180, 255);
    let orange = egui::Color32::from_rgb(255, 190, 80);
    let indent = depth as f32 * 12.0;

    match val {
        // ── Leaf ──────────────────────────────────────────────────────────
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {
            let val_str = leaf_value_str(val).unwrap_or_default();
            let is_pinned = ctx.pinned_paths.contains(&path.to_string());
            let is_num    = is_numeric_str(&val_str);
            let val_color = if is_num { orange } else { egui::Color32::from_rgb(200,200,200) };

            ui.horizontal(|ui| {
                ui.add_space(indent);
                // Pin toggle
                if is_pinned {
                    if ui.small_button(RichText::new("−").color(egui::Color32::from_rgb(255,100,100))).clicked() {
                        *ctx.remove_req = Some(path.to_string());
                    }
                } else if ui.small_button(RichText::new("+").color(green)).clicked() {
                    *ctx.pin_req = Some((path.to_string(), val_str.clone()));
                }
                // Key name
                let key_col = if is_pinned { green } else { blue };
                ui.label(RichText::new(key_label).small().monospace().color(key_col));
                ui.label(RichText::new(":").small().color(dim));
                ui.label(RichText::new(truncate(&val_str, 18)).small().monospace().color(val_color));

                // If pinned, show the output port circle at right
                if is_pinned {
                    if let Some(port_idx) = ctx.pinned_paths.iter().position(|p| p == path) {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            crate::nodes::inline_port_circle(
                                ui, ctx.node_id, port_idx, false,
                                ctx.connections, ctx.port_positions,
                                ctx.dragging_from, ctx.pending_disconnects,
                                if is_num { PortKind::Number } else { PortKind::Text },
                            );
                        });
                    }
                }
            });
        }

        // ── Object ────────────────────────────────────────────────────────
        serde_json::Value::Object(map) => {
            let n = map.len();
            let is_expanded = ctx.expanded.contains(path);
            let chevron = if is_expanded { "▼" } else { "▶" };

            ui.horizontal(|ui| {
                ui.add_space(indent);
                if ui.small_button(RichText::new(chevron).color(dim)).clicked() {
                    if is_expanded {
                        ctx.expanded.remove(path);
                    } else {
                        ctx.expanded.insert(path.to_string());
                    }
                }
                ui.label(RichText::new(key_label).small().monospace().strong().color(blue));
                ui.label(RichText::new(format!(" {{{}}}", n)).small().color(dim));
            });

            if is_expanded {
                for (k, v) in map {
                    let child_path = if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{}.{}", path, k)
                    };
                    render_tree(ui, v, &child_path, k, depth + 1, ctx);
                }
            }
        }

        // ── Array ─────────────────────────────────────────────────────────
        serde_json::Value::Array(arr) => {
            let n = arr.len();
            let is_expanded = ctx.expanded.contains(path);
            let chevron = if is_expanded { "▼" } else { "▶" };

            ui.horizontal(|ui| {
                ui.add_space(indent);
                if ui.small_button(RichText::new(chevron).color(dim)).clicked() {
                    if is_expanded {
                        ctx.expanded.remove(path);
                    } else {
                        ctx.expanded.insert(path.to_string());
                    }
                }
                ui.label(RichText::new(key_label).small().monospace().strong().color(blue));
                ui.label(RichText::new(format!(" [{}]", n)).small().color(dim));
            });

            if is_expanded {
                // Limit array display to 20 items to avoid flooding
                let show = n.min(20);
                for (i, v) in arr.iter().take(show).enumerate() {
                    let child_path = format!("{}[{}]", path, i);
                    let label = format!("[{}]", i);
                    render_tree(ui, v, &child_path, &label, depth + 1, ctx);
                }
                if n > 20 {
                    ui.horizontal(|ui| {
                        ui.add_space(indent + 12.0);
                        ui.label(RichText::new(format!("… {} more", n - 20)).small().color(dim).italics());
                    });
                }
            }
        }
    }
}

// ── NodeBehavior ──────────────────────────────────────────────────────────────

impl NodeBehavior for JsonFieldsNode {
    fn title(&self)      -> &str   { "Unpack (JSON)" }
    fn type_tag(&self)   -> &str   { "json_fields" }
    fn color_hint(&self) -> [u8;3] { [80, 160, 220] }
    fn min_width(&self)  -> Option<f32> { Some(260.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("JSON", PortKind::Text)]
    }

    fn outputs(&self) -> Vec<PortDef> {
        self.pinned_paths.iter().enumerate().map(|(i, path)| {
            let kind = self.output_vals.get(i)
                .map(|v| if is_numeric_str(v) { PortKind::Number } else { PortKind::Text })
                .unwrap_or(PortKind::Text);
            PortDef::dynamic(path.clone(), kind)
        }).collect()
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        if let Some(PortValue::Text(s)) = inputs.get(0) {
            if !s.is_empty() {
                let was_empty = self.parsed.is_none();
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
                    self.parsed = Some(v);
                    if was_empty {
                        self.auto_expand_root();
                    }
                }
                self.refresh_output_vals();
            }
        }
        self.emit_pinned_vals()
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim  = egui::Color32::from_rgb(110, 110, 120);
        let blue = egui::Color32::from_rgb(80, 180, 255);

        // ── JSON input port ───────────────────────────────────────────────
        let json_connected = ctx.connections.iter()
            .any(|c| c.to_node == node_id && c.to_port == 0);

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 0, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Text,
            );
            let lbl = if json_connected { "JSON ✓" } else { "JSON" };
            let col = if json_connected { blue } else { dim };
            ui.label(RichText::new(lbl).small().color(col));
        });

        // Live parse from connected input
        let json_val = crate::graph::Graph::static_input_value(
            ctx.connections, ctx.values, node_id, 0,
        );
        if let PortValue::Text(ref s) = json_val {
            if !s.is_empty() {
                let was_empty = self.parsed.is_none();
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
                    self.parsed = Some(v);
                    if was_empty {
                        self.auto_expand_root();
                    }
                }
                self.refresh_output_vals();
            }
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Tree view ─────────────────────────────────────────────────────
        match &self.parsed {
            None => {
                ui.label(
                    RichText::new(if json_connected { "Waiting for data…" } else { "Connect a JSON source" })
                        .small().color(dim).italics()
                );
            }
            Some(_) => {
                // We need to clone to avoid borrow issues during tree render
                let root = self.parsed.clone().unwrap();
                let total_leaves = count_leaves(&root);

                ui.label(RichText::new(format!("JSON tree  ({} fields)", total_leaves))
                    .small().strong().color(dim));
                ui.add_space(2.0);

                // Collapse/expand all buttons
                ui.horizontal(|ui| {
                    if ui.small_button("Expand all").clicked() {
                        collect_all_paths(&root, "", &mut self.expanded);
                    }
                    if ui.small_button("Collapse all").clicked() {
                        self.expanded.clear();
                    }
                });
                ui.add_space(2.0);

                let mut pin_req: Option<(String, String)> = None;
                let mut remove_req: Option<String> = None;

                egui::ScrollArea::vertical()
                    .id_salt(egui::Id::new(("jf_tree_scroll", node_id)))
                    .max_height(200.0)
                    .show_pannable(ui, |ui| {
                        let mut tree_ctx = TreeCtx {
                            node_id,
                            pinned_paths:        &mut self.pinned_paths,
                            output_vals:         &mut self.output_vals,
                            expanded:            &mut self.expanded,
                            pin_req:             &mut pin_req,
                            remove_req:          &mut remove_req,
                            connections:         ctx.connections,
                            port_positions:      ctx.port_positions,
                            dragging_from:       ctx.dragging_from,
                            pending_disconnects: ctx.pending_disconnects,
                        };

                        // Render root
                        match &root {
                            serde_json::Value::Object(map) => {
                                let n = map.len();
                                let is_root_expanded = tree_ctx.expanded.contains("");
                                let chevron = if is_root_expanded { "▼" } else { "▶" };
                                ui.horizontal(|ui| {
                                    if ui.small_button(
                                        RichText::new(chevron)
                                            .color(egui::Color32::from_rgb(110,110,120))
                                    ).clicked() {
                                        if is_root_expanded {
                                            tree_ctx.expanded.remove("");
                                        } else {
                                            tree_ctx.expanded.insert("".to_string());
                                        }
                                    }
                                    ui.label(RichText::new("{ }").small().monospace().strong()
                                        .color(egui::Color32::from_rgb(120,180,255)));
                                    ui.label(RichText::new(format!(" {} keys", n))
                                        .small().color(egui::Color32::from_rgb(110,110,120)));
                                });
                                if is_root_expanded {
                                    for (k, v) in map {
                                        render_tree(ui, v, k, k, 1, &mut tree_ctx);
                                    }
                                }
                            }
                            serde_json::Value::Array(arr) => {
                                let n = arr.len();
                                let is_root_expanded = tree_ctx.expanded.contains("");
                                let chevron = if is_root_expanded { "▼" } else { "▶" };
                                ui.horizontal(|ui| {
                                    if ui.small_button(
                                        RichText::new(chevron)
                                            .color(egui::Color32::from_rgb(110,110,120))
                                    ).clicked() {
                                        if is_root_expanded {
                                            tree_ctx.expanded.remove("");
                                        } else {
                                            tree_ctx.expanded.insert("".to_string());
                                        }
                                    }
                                    ui.label(RichText::new("[ ]").small().monospace().strong()
                                        .color(egui::Color32::from_rgb(120,180,255)));
                                    ui.label(RichText::new(format!(" {} items", n))
                                        .small().color(egui::Color32::from_rgb(110,110,120)));
                                });
                                if is_root_expanded {
                                    let show = n.min(20);
                                    for (i, v) in arr.iter().take(show).enumerate() {
                                        let child_path = format!("[{}]", i);
                                        let label = format!("[{}]", i);
                                        render_tree(ui, v, &child_path, &label, 1, &mut tree_ctx);
                                    }
                                }
                            }
                            // Root is scalar (unlikely but handled)
                            leaf => {
                                let val_str = leaf_value_str(leaf).unwrap_or_default();
                                ui.label(RichText::new(&val_str).small().monospace()
                                    .color(egui::Color32::from_rgb(200,200,200)));
                            }
                        }
                    });

                // Apply pin/unpin requests after borrow ends
                if let Some((path, val)) = pin_req {
                    if !self.pinned_paths.contains(&path) {
                        self.pinned_paths.push(path);
                        self.output_vals.push(val);
                    }
                }
                if let Some(path) = remove_req {
                    if let Some(idx) = self.pinned_paths.iter().position(|p| p == &path) {
                        self.pinned_paths.remove(idx);
                        if idx < self.output_vals.len() {
                            self.output_vals.remove(idx);
                        }
                    }
                }

                // ── Pinned outputs ────────────────────────────────────────
                if !self.pinned_paths.is_empty() {
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(2.0);
                    ui.label(RichText::new("Pinned outputs").small().strong().color(dim));
                    ui.add_space(2.0);

                    let mut remove_pinned: Option<usize> = None;
                    for (i, path) in self.pinned_paths.iter().enumerate() {
                        let val = self.output_vals.get(i).map(|s| s.as_str()).unwrap_or("0");
                        ui.horizontal(|ui| {
                            if ui.small_button(
                                RichText::new("−").color(egui::Color32::from_rgb(255,100,100))
                            ).clicked() {
                                remove_pinned = Some(i);
                            }
                            ui.label(
                                RichText::new(truncate(path, 22))
                                    .small().monospace()
                                    .color(egui::Color32::from_rgb(180, 220, 255))
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                crate::nodes::inline_port_circle(
                                    ui, node_id, i, false,
                                    ctx.connections, ctx.port_positions,
                                    ctx.dragging_from, ctx.pending_disconnects,
                                    if is_numeric_str(val) { PortKind::Number } else { PortKind::Text },
                                );
                                ui.label(
                                    RichText::new(truncate(val, 14))
                                        .small().monospace()
                                        .color(egui::Color32::from_rgb(80, 210, 130))
                                );
                            });
                        });
                    }
                    if let Some(idx) = remove_pinned {
                        self.pinned_paths.remove(idx);
                        if idx < self.output_vals.len() {
                            self.output_vals.remove(idx);
                        }
                    }
                }
            }
        }
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "pinned_paths": self.pinned_paths,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(arr) = state.get("pinned_paths").and_then(|v| v.as_array()) {
            self.pinned_paths = arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            self.output_vals = vec!["0".to_string(); self.pinned_paths.len()];
        }
    }
}

// ── Tree helpers ──────────────────────────────────────────────────────────────

fn count_leaves(val: &serde_json::Value) -> usize {
    match val {
        serde_json::Value::Object(map) => map.values().map(count_leaves).sum(),
        serde_json::Value::Array(arr)  => arr.iter().map(count_leaves).sum(),
        _                              => 1,
    }
}

fn collect_all_paths(val: &serde_json::Value, prefix: &str, out: &mut HashSet<String>) {
    match val {
        serde_json::Value::Object(map) => {
            out.insert(prefix.to_string());
            for (k, v) in map {
                let child = if prefix.is_empty() { k.clone() } else { format!("{}.{}", prefix, k) };
                collect_all_paths(v, &child, out);
            }
        }
        serde_json::Value::Array(arr) => {
            out.insert(prefix.to_string());
            for (i, v) in arr.iter().enumerate() {
                let child = format!("{}[{}]", prefix, i);
                collect_all_paths(v, &child, out);
            }
        }
        _ => {}
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("json_fields", |state| {
        let mut n = JsonFieldsNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
