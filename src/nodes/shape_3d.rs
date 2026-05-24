// 3D Shape — emits a procedural mesh (Cube / Sphere / Plane / Torus) or
// loads a `.gltf` / `.glb` file (Custom). Same `Vertex` layout as the
// procedural primitives so the 3D Render node treats every source uniformly.
//
// Drag-and-drop: dropping a `.gltf` / `.glb` file onto the canvas spawns a
// `Shape3DNode` pre-configured with `kind = Custom` and the dropped file
// path. See `src/app/io.rs::handle_file_drop`.

use crate::graph::{Graph, PortDef, PortKind, PortValue};
use crate::mesh::{
    build_model_matrix, load_gltf_or_glb, primitive_cube, primitive_plane,
    primitive_sphere, primitive_torus, MeshData, PlacedMesh,
};
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::nodes::inline_port_circle;
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShapeKind {
    Cube,
    Sphere,
    Plane,
    Torus,
    /// Loaded from a user-supplied `.gltf` or `.glb` file.
    Custom,
}

impl ShapeKind {
    pub const ALL: [ShapeKind; 5] = [
        ShapeKind::Cube,
        ShapeKind::Sphere,
        ShapeKind::Plane,
        ShapeKind::Torus,
        ShapeKind::Custom,
    ];
    pub fn label(&self) -> &'static str {
        match self {
            ShapeKind::Cube   => "Cube",
            ShapeKind::Sphere => "Sphere",
            ShapeKind::Plane  => "Plane",
            ShapeKind::Torus  => "Torus",
            ShapeKind::Custom => "Custom (GLTF/GLB)",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shape3DNode {
    #[serde(default = "default_kind")]
    pub kind: ShapeKind,
    #[serde(default = "default_size")]
    pub size: f32,
    #[serde(default = "default_segments")]
    pub segments: u32,
    #[serde(default = "default_minor_radius")]
    pub minor_radius: f32,
    /// Path to a `.gltf` / `.glb` file. Used only when `kind == Custom`.
    /// Persisted with the project so reopening reloads the same asset.
    #[serde(default)]
    pub custom_path: String,

    // ── World-space transform (model matrix is built from these). ──
    #[serde(default)]                pub pos_x: f32,
    #[serde(default)]                pub pos_y: f32,
    #[serde(default)]                pub pos_z: f32,
    #[serde(default = "default_one")] pub scale_x: f32,
    #[serde(default = "default_one")] pub scale_y: f32,
    #[serde(default = "default_one")] pub scale_z: f32,
    #[serde(default)]                pub rot_x: f32, // degrees
    #[serde(default)]                pub rot_y: f32,
    #[serde(default)]                pub rot_z: f32,

    /// Lazily-loaded mesh for `Custom` mode; invalidated when `custom_path`
    /// changes. Skipped on save — re-parsed from `custom_path` on load.
    #[serde(skip)]
    pub cached_custom: Option<(String, Arc<MeshData>)>,
    /// Last load error message for the inline error label, if any.
    #[serde(skip)]
    pub custom_error: Option<String>,
}

fn default_kind() -> ShapeKind { ShapeKind::Cube }
fn default_size() -> f32 { 1.0 }
fn default_segments() -> u32 { 32 }
fn default_minor_radius() -> f32 { 0.3 }
fn default_one() -> f32 { 1.0 }

// Port layout for the 9 transform input ports.
const PORT_POS_X: usize    = 0;
const PORT_POS_Y: usize    = 1;
const PORT_POS_Z: usize    = 2;
const PORT_SCALE_X: usize  = 3;
const PORT_SCALE_Y: usize  = 4;
const PORT_SCALE_Z: usize  = 5;
const PORT_ROT_X: usize    = 6;
const PORT_ROT_Y: usize    = 7;
const PORT_ROT_Z: usize    = 8;

impl Default for Shape3DNode {
    fn default() -> Self {
        Self {
            kind: ShapeKind::Cube,
            size: 1.0,
            segments: 32,
            minor_radius: 0.3,
            custom_path: String::new(),
            pos_x: 0.0, pos_y: 0.0, pos_z: 0.0,
            scale_x: 1.0, scale_y: 1.0, scale_z: 1.0,
            rot_x: 0.0, rot_y: 0.0, rot_z: 0.0,
            cached_custom: None,
            custom_error: None,
        }
    }
}

impl Shape3DNode {
    /// Pre-configure as a Custom mesh from a dropped or picked file.
    /// Used by the canvas drag-drop handler in `app/io.rs`.
    pub fn from_gltf_path(path: String) -> Self {
        Self {
            kind: ShapeKind::Custom,
            custom_path: path,
            ..Default::default()
        }
    }

    /// Resolve the mesh for the current `kind`, using the cache for Custom.
    fn resolve_mesh(&mut self) -> Option<Arc<MeshData>> {
        match self.kind {
            ShapeKind::Cube   => Some(primitive_cube(self.size)),
            ShapeKind::Plane  => Some(primitive_plane(self.size)),
            ShapeKind::Sphere => Some(primitive_sphere(
                self.size * 0.5, self.segments, self.segments.max(2),
            )),
            ShapeKind::Torus  => Some(primitive_torus(
                self.size * 0.5, self.minor_radius, self.segments, self.segments,
            )),
            ShapeKind::Custom => {
                if self.custom_path.is_empty() { return None; }
                if let Some((cached_path, mesh)) = &self.cached_custom {
                    if cached_path == &self.custom_path {
                        return Some(mesh.clone());
                    }
                }
                match load_gltf_or_glb(&self.custom_path) {
                    Some(mesh) => {
                        self.cached_custom = Some((self.custom_path.clone(), mesh.clone()));
                        self.custom_error = None;
                        Some(mesh)
                    }
                    None => {
                        self.cached_custom = None;
                        self.custom_error = Some(format!(
                            "Failed to load: {}",
                            std::path::Path::new(&self.custom_path)
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("(unknown)")
                        ));
                        None
                    }
                }
            }
        }
    }
}

impl NodeBehavior for Shape3DNode {
    fn title(&self)        -> &str    { "3D Shape" }
    fn type_tag(&self)     -> &str    { "shape_3d" }
    fn color_hint(&self)   -> [u8; 3] { [80, 180, 180] }
    fn inline_ports(&self) -> bool    { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Pos X",   PortKind::Number),
            PortDef::new("Pos Y",   PortKind::Number),
            PortDef::new("Pos Z",   PortKind::Number),
            PortDef::new("Scale X", PortKind::Number),
            PortDef::new("Scale Y", PortKind::Number),
            PortDef::new("Scale Z", PortKind::Number),
            PortDef::new("Rot X",   PortKind::Number),
            PortDef::new("Rot Y",   PortKind::Number),
            PortDef::new("Rot Z",   PortKind::Number),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Mesh", PortKind::Mesh)]
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let read = |idx: usize, fallback: f32| -> f32 {
            match inputs.get(idx) {
                Some(PortValue::Float(v)) => *v,
                _ => fallback,
            }
        };
        let position = [
            read(PORT_POS_X,   self.pos_x),
            read(PORT_POS_Y,   self.pos_y),
            read(PORT_POS_Z,   self.pos_z),
        ];
        let scale = [
            read(PORT_SCALE_X, self.scale_x),
            read(PORT_SCALE_Y, self.scale_y),
            read(PORT_SCALE_Z, self.scale_z),
        ];
        let rotation = [
            read(PORT_ROT_X,   self.rot_x),
            read(PORT_ROT_Y,   self.rot_y),
            read(PORT_ROT_Z,   self.rot_z),
        ];
        let transform = build_model_matrix(position, scale, rotation);

        match self.resolve_mesh() {
            Some(mesh) => vec![(0, PortValue::Mesh(PlacedMesh::with_transform(mesh, transform)))],
            None => vec![(0, PortValue::None)],
        }
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<Shape3DNode>(state.clone()) { *self = l; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("shape_3d_kind", ctx.node_id))
                .selected_text(self.kind.label())
                .show_ui(ui, |ui| {
                    for k in ShapeKind::ALL {
                        ui.selectable_value(&mut self.kind, k, k.label());
                    }
                });
        });

        match self.kind {
            ShapeKind::Cube | ShapeKind::Plane => {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Size").small());
                    ui.add(egui::DragValue::new(&mut self.size).speed(0.05).range(0.01..=20.0));
                });
            }
            ShapeKind::Sphere => {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Size").small());
                    ui.add(egui::DragValue::new(&mut self.size).speed(0.05).range(0.01..=20.0));
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Segments").small());
                    let mut seg = self.segments as i32;
                    if ui.add(egui::DragValue::new(&mut seg).speed(1.0).range(3..=128)).changed() {
                        self.segments = seg.max(3) as u32;
                    }
                });
            }
            ShapeKind::Torus => {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Size").small());
                    ui.add(egui::DragValue::new(&mut self.size).speed(0.05).range(0.01..=20.0));
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Segments").small());
                    let mut seg = self.segments as i32;
                    if ui.add(egui::DragValue::new(&mut seg).speed(1.0).range(3..=128)).changed() {
                        self.segments = seg.max(3) as u32;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Minor R").small());
                    ui.add(egui::DragValue::new(&mut self.minor_radius).speed(0.01).range(0.01..=2.0));
                });
            }
            ShapeKind::Custom => {
                self.render_custom_zone(ui);
            }
        }

        ui.separator();

        // ── Transform: position / scale / rotation ──
        // Each axis is a wire-able Float input port + DragValue fallback,
        // grouped 3-up under labelled rows for compactness.
        ui.collapsing("Transform", |ui| {
            transform_row(
                ui, ctx,
                "Pos",
                (PORT_POS_X, &mut self.pos_x),
                (PORT_POS_Y, &mut self.pos_y),
                (PORT_POS_Z, &mut self.pos_z),
                0.05,
                -50.0..=50.0,
            );
            transform_row(
                ui, ctx,
                "Scale",
                (PORT_SCALE_X, &mut self.scale_x),
                (PORT_SCALE_Y, &mut self.scale_y),
                (PORT_SCALE_Z, &mut self.scale_z),
                0.02,
                0.001..=20.0,
            );
            transform_row(
                ui, ctx,
                "Rot°",
                (PORT_ROT_X, &mut self.rot_x),
                (PORT_ROT_Y, &mut self.rot_y),
                (PORT_ROT_Z, &mut self.rot_z),
                0.5,
                -360.0..=360.0,
            );
        });

        ui.separator();

        // Output port row — single Mesh output
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Mesh").small());
            inline_port_circle(
                ui, ctx.node_id, 0, false,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Mesh,
            );
        });
    }
}

/// One labelled row of three wire-able Float ports + DragValues
/// (Pos / Scale / Rotation). The label sits ahead of the per-axis cells.
fn transform_row(
    ui: &mut egui::Ui,
    ctx: &mut RenderContext,
    label: &str,
    x: (usize, &mut f32),
    y: (usize, &mut f32),
    z: (usize, &mut f32),
    speed: f32,
    range: std::ops::RangeInclusive<f32>,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).small());
        for (port_idx, value) in [x, y, z] {
            let wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == port_idx);
            inline_port_circle(
                ui, ctx.node_id, port_idx, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Number,
            );
            if wired {
                let v = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, port_idx);
                ui.label(egui::RichText::new(format!("{}", v)).small().color(ui.visuals().hyperlink_color));
            } else {
                ui.add(egui::DragValue::new(value).speed(speed).range(range.clone()));
            }
        }
    });
}

impl Shape3DNode {
    /// Click-to-pick + drop-target zone for Custom (GLTF/GLB) mode.
    /// The app-level drop handler in `app/io.rs::handle_file_drop` covers
    /// the "drop on canvas to spawn a new node" flow; this in-node zone
    /// covers "drop on existing node to swap its file".
    fn render_custom_zone(&mut self, ui: &mut egui::Ui) {
        let zone_w = ui.available_width().max(160.0);
        let zone_h = 56.0;
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(zone_w, zone_h),
            egui::Sense::click(),
        );

        let hovering_drop = ui.input(|i| !i.raw.hovered_files.is_empty());
        let stroke_color = if response.hovered() || hovering_drop {
            egui::Color32::from_rgb(120, 200, 220)
        } else {
            egui::Color32::from_gray(80)
        };
        let fill = if hovering_drop {
            egui::Color32::from_rgba_unmultiplied(120, 200, 220, 30)
        } else {
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 6)
        };
        ui.painter().rect_filled(rect, 6.0, fill);
        ui.painter().rect_stroke(
            rect, 6.0,
            egui::Stroke::new(1.0, stroke_color),
            egui::epaint::StrokeKind::Inside,
        );

        let line_color = ui.visuals().text_color();
        let dim = egui::Color32::from_gray(140);
        let label_main: String;
        let label_sub: String;
        if self.custom_path.is_empty() {
            label_main = "Click or drop .gltf / .glb".into();
            label_sub  = "to load a 3D model".into();
        } else {
            let fname = std::path::Path::new(&self.custom_path)
                .file_name().and_then(|s| s.to_str())
                .unwrap_or(&self.custom_path);
            label_main = fname.to_string();
            label_sub  = match (&self.custom_error, &self.cached_custom) {
                (Some(err), _) => err.clone(),
                (None, Some((_, mesh))) => format!(
                    "{} verts · {} tris",
                    mesh.vertices.len(),
                    mesh.indices.len() / 3,
                ),
                _ => "click to reload".into(),
            };
        }
        let center = rect.center();
        ui.painter().text(
            center - egui::vec2(0.0, 8.0),
            egui::Align2::CENTER_CENTER,
            label_main,
            egui::FontId::proportional(11.0),
            line_color,
        );
        ui.painter().text(
            center + egui::vec2(0.0, 8.0),
            egui::Align2::CENTER_CENTER,
            label_sub,
            egui::FontId::proportional(9.0),
            dim,
        );

        // Click → file picker. (Drop-on-node is handled at the app level in
        // `app/io.rs::handle_file_drop` — it detects which node the drop
        // landed on and swaps the path in place rather than spawning a
        // fresh node, so the in-node drop zone is purely visual feedback.)
        if response.clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("3D model", &["gltf", "glb"])
                .pick_file()
            {
                let new_path = path.display().to_string();
                if new_path != self.custom_path {
                    self.custom_path = new_path;
                    self.cached_custom = None;
                    self.custom_error = None;
                }
            }
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("shape_3d", |state| {
        let mut n = Shape3DNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
