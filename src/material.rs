// Material definition consumed by the 3D Render node.
//
// The Material node emits a JSON string on a Text port; the 3D Render node
// parses it back into `MaterialDef`. `MaterialDef` is therefore an
// internal renderer-side representation, *not* a `PortValue` payload.
// See `feedback_new_node_checklist.md` for the typed-vs-JSON rationale.

use crate::graph::NodeId;
use serde::{Deserialize, Serialize};

// v1: single `albedo_tex` field at top level.
// v2: all maps collected under `maps: { albedo, roughness, metallic, ao, emission }`.
const SCHEMA_VERSION: u32 = 2;
const SCHEMA_KIND: &str = "material";

/// Built-in shading models.  The 3D Render node selects a fragment-shader
/// branch based on this value, uploaded as a `u32` in the uniform block.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShaderType {
    /// Blinn-Phong with diffuse + specular + ambient.
    Lit,
    /// Cel-shaded: stepped diffuse, hard rim outline.
    Toon,
    /// No lighting — emit albedo (× texture) directly.
    Unlit,
}

impl ShaderType {
    pub const ALL: [ShaderType; 3] = [ShaderType::Lit, ShaderType::Toon, ShaderType::Unlit];
    pub fn label(&self) -> &'static str {
        match self {
            ShaderType::Lit   => "Lit",
            ShaderType::Toon  => "Toon",
            ShaderType::Unlit => "Unlit",
        }
    }
    /// Wire encoding sent to the shader.
    pub fn shader_id(&self) -> u32 {
        match self {
            ShaderType::Lit   => 0,
            ShaderType::Toon  => 1,
            ShaderType::Unlit => 2,
        }
    }
    pub fn json_tag(&self) -> &'static str {
        match self {
            ShaderType::Lit   => "lit",
            ShaderType::Toon  => "toon",
            ShaderType::Unlit => "unlit",
        }
    }
    pub fn from_json_tag(tag: &str) -> Option<Self> {
        match tag {
            "lit"   => Some(ShaderType::Lit),
            "toon"  => Some(ShaderType::Toon),
            "unlit" => Some(ShaderType::Unlit),
            _ => None,
        }
    }
}

impl Default for ShaderType {
    fn default() -> Self { ShaderType::Lit }
}

/// Reference to an upstream GPU texture (resolved each frame via
/// `gpu_image::frame_snapshot_get_view`). Only the producer's
/// `(node_id, port)` and dimensions are persisted in the JSON; the live
/// `frame_stamp` is intentionally excluded so the JSON stays stable
/// frame-to-frame and the consumer's cache doesn't thrash.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialTexRef {
    pub node_id: NodeId,
    pub port:    usize,
    pub width:   u32,
    pub height:  u32,
}

/// All texture-map slots on a material. Each is `None` by default, in
/// which case the corresponding scalar/color in `MaterialDef` is used
/// directly. When `Some`, the texture is multiplied with the scalar.
///
/// Sampling conventions (single-channel slots): roughness / metallic /
/// AO read from the texture's red channel. For ORM-style packed maps
/// (R=AO, G=Roughness, B=Metallic) you'd pre-split via a Channel Pick
/// node — keeping per-slot sampling as `.r` keeps grayscale-image use
/// trivial.
#[derive(Clone, Debug, Default)]
pub struct MaterialMaps {
    pub albedo:    Option<MaterialTexRef>,
    pub roughness: Option<MaterialTexRef>,
    pub metallic:  Option<MaterialTexRef>,
    pub ao:        Option<MaterialTexRef>,
    pub emission:  Option<MaterialTexRef>,
}

#[derive(Clone, Debug)]
pub struct MaterialDef {
    pub shader_type: ShaderType,
    pub albedo:      [f32; 4],
    pub roughness:   f32,
    pub metallic:    f32,
    pub emission:    [f32; 3],
    /// All texture-map slots. Decoded from JSON; 3D Render resolves
    /// each `wgpu::TextureView` per-frame from the GPU texture cache.
    pub maps:        MaterialMaps,
    pub double_sided: bool,
}

impl Default for MaterialDef {
    fn default() -> Self {
        Self {
            shader_type:   ShaderType::Lit,
            albedo:        [0.85, 0.85, 0.85, 1.0],
            roughness:     0.5,
            metallic:      0.0,
            emission:      [0.0, 0.0, 0.0],
            maps:          MaterialMaps::default(),
            double_sided:  false,
        }
    }
}

impl MaterialDef {
    pub fn solid_color(rgb: [f32; 3]) -> Self {
        Self {
            albedo: [rgb[0], rgb[1], rgb[2], 1.0],
            ..Default::default()
        }
    }

    /// Build the JSON payload emitted by the Material node.
    pub fn to_json(&self) -> serde_json::Value {
        let tex_json = |t: Option<MaterialTexRef>| -> serde_json::Value {
            match t {
                Some(t) => serde_json::json!({
                    "node_id": t.node_id,
                    "port":    t.port,
                    "w":       t.width,
                    "h":       t.height,
                }),
                None => serde_json::Value::Null,
            }
        };
        serde_json::json!({
            "v":           SCHEMA_VERSION,
            "kind":        SCHEMA_KIND,
            "shader":      self.shader_type.json_tag(),
            "albedo":      self.albedo,
            "roughness":   self.roughness,
            "metallic":    self.metallic,
            "emission":    self.emission,
            "double_sided": self.double_sided,
            "maps": {
                "albedo":    tex_json(self.maps.albedo),
                "roughness": tex_json(self.maps.roughness),
                "metallic":  tex_json(self.maps.metallic),
                "ao":        tex_json(self.maps.ao),
                "emission":  tex_json(self.maps.emission),
            },
        })
    }

    /// Parse a Material payload from a Text port. Returns `None` if the
    /// string isn't valid JSON, isn't the right kind, or is a future
    /// schema version we don't understand.
    pub fn from_json(s: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        if v.get("kind").and_then(|x| x.as_str()) != Some(SCHEMA_KIND) {
            return None;
        }
        if v.get("v").and_then(|x| x.as_u64()) != Some(SCHEMA_VERSION as u64) {
            return None;
        }

        let shader = v.get("shader")
            .and_then(|x| x.as_str())
            .and_then(ShaderType::from_json_tag)
            .unwrap_or_default();

        let read_array = |key: &str, fallback: [f32; 4]| -> [f32; 4] {
            let mut out = fallback;
            if let Some(arr) = v.get(key).and_then(|x| x.as_array()) {
                for (i, slot) in out.iter_mut().enumerate() {
                    if let Some(f) = arr.get(i).and_then(|x| x.as_f64()) {
                        *slot = f as f32;
                    }
                }
            }
            out
        };
        let read_array3 = |key: &str, fallback: [f32; 3]| -> [f32; 3] {
            let mut out = fallback;
            if let Some(arr) = v.get(key).and_then(|x| x.as_array()) {
                for (i, slot) in out.iter_mut().enumerate() {
                    if let Some(f) = arr.get(i).and_then(|x| x.as_f64()) {
                        *slot = f as f32;
                    }
                }
            }
            out
        };
        let read_f32 = |key: &str, fallback: f32| -> f32 {
            v.get(key).and_then(|x| x.as_f64()).map(|f| f as f32).unwrap_or(fallback)
        };
        let read_bool = |key: &str, fallback: bool| -> bool {
            v.get(key).and_then(|x| x.as_bool()).unwrap_or(fallback)
        };
        let read_tex = |slot: &serde_json::Value| -> Option<MaterialTexRef> {
            if slot.is_null() { return None; }
            let node_id = slot.get("node_id").and_then(|x| x.as_u64())?;
            let port    = slot.get("port").and_then(|x| x.as_u64())? as usize;
            let width   = slot.get("w").and_then(|x| x.as_u64())? as u32;
            let height  = slot.get("h").and_then(|x| x.as_u64())? as u32;
            Some(MaterialTexRef { node_id, port, width, height })
        };

        let maps = if let Some(maps_obj) = v.get("maps") {
            MaterialMaps {
                albedo:    maps_obj.get("albedo").and_then(read_tex),
                roughness: maps_obj.get("roughness").and_then(read_tex),
                metallic:  maps_obj.get("metallic").and_then(read_tex),
                ao:        maps_obj.get("ao").and_then(read_tex),
                emission:  maps_obj.get("emission").and_then(read_tex),
            }
        } else {
            MaterialMaps::default()
        };

        Some(Self {
            shader_type:  shader,
            albedo:       read_array("albedo", [0.85, 0.85, 0.85, 1.0]),
            roughness:    read_f32("roughness", 0.5),
            metallic:     read_f32("metallic", 0.0),
            emission:     read_array3("emission", [0.0, 0.0, 0.0]),
            double_sided: read_bool("double_sided", false),
            maps,
        })
    }
}
