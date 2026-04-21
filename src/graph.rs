use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::node_trait::NodeBehavior;

fn default_true() -> bool { true }
fn default_bg_color() -> [u8; 3] { [20, 20, 20] }
fn default_text_color() -> [u8; 3] { [220, 220, 220] }
fn default_window_bg() -> [u8; 3] { [24, 24, 24] }
fn default_window_alpha() -> u8 { 240 }
fn default_grid_color() -> [u8; 3] { [28, 28, 28] }
fn default_slider_step() -> f32 { 0.01 }
fn default_slider_color() -> [u8; 3] { [80, 160, 255] }
fn default_grid_style() -> u8 { 2 } // Default: Dotted
fn default_rounding() -> f32 { 16.0 }
fn default_spacing() -> f32 { 4.0 }
fn default_wire_thickness() -> f32 { 6.0 }
fn default_canvas_w() -> f32 { 400.0 }
fn default_canvas_h() -> f32 { 300.0 }
fn default_wiggle_range() -> f32 { 1.0 }
fn default_wiggle_speed() -> f32 { 1.0 }
fn default_wiggle_signal() -> f32 { 0.0 }
fn default_resolution() -> u32 { 120 }
fn default_max_tokens() -> u32 { 1024 }
fn default_provider() -> String { "anthropic".into() }
fn default_model() -> String { "claude-sonnet-4-20250514".into() }
fn default_temperature() -> f32 { 0.7 }

pub type NodeId = u64;

// ── WGSL Viewer image input modes ──────────────────────────────────────────
//
// Each WGSL Viewer has two generic image input slots (`Input A`, `Input B`).
// Per-slot mode controls what gets bound to `image_a` / `image_b` in the
// shader bind group:
//
//   * `Unused`    → bound to a 1×1 black dummy texture
//   * `External`  → sampled from the connected upstream image port
//   * `LastFrame` → sampled from this viewer's previous frame (ping-pong feedback)
//
// See plan: ~/.claude/plans/wgsl-feedback-feature.md
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ImageInputMode {
    #[default]
    Unused,
    External,
    LastFrame,
}

impl ImageInputMode {
    pub fn label(&self) -> &'static str {
        match self {
            ImageInputMode::Unused => "Unused",
            ImageInputMode::External => "External",
            ImageInputMode::LastFrame => "Last frame",
        }
    }
}

// ── ML Model Presets ────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MlPreset {
    /// ImageNet-style classification (softmax over classes). Input: 224×224, NCHW, ImageNet norm.
    #[default]
    Classification,
    /// YOLO-style object detection. Outputs bounding boxes + class + confidence.
    /// Input: 640×640, NCHW, 0–1 norm. Output: [1, N, 5+classes] or [1, 5+classes, N].
    ObjectDetection,
    /// Pose estimation (e.g., MoveNet, MediaPipe Pose). Outputs keypoints.
    /// Input: 192×192 or 256×256, NHWC or NCHW, 0–1 norm.
    PoseEstimation,
    /// MediaPipe-style SSD hand/palm detection. Outputs anchors + scores.
    /// Input: 256×256, NCHW, 0–1 norm. Output: [1, N, 18] boxes+keypoints + [1, N, 1] scores.
    HandDetection,
    /// MediaPipe BlazeFace short-range face detection. Outputs anchors + scores.
    /// Input: 128×128, NHWC, [-1,1] norm. Output: [1, 896, 16] + [1, 896, 1].
    FaceDetection,
    /// Custom model — user sets input size and normalization manually.
    Custom,
}

impl MlPreset {
    pub fn all() -> &'static [MlPreset] {
        // Only show presets relevant to the generic ML Model node.
        // Hand/Face/Pose have dedicated tracking nodes now.
        &[MlPreset::Classification, MlPreset::ObjectDetection, MlPreset::Custom]
    }

    pub fn name(&self) -> &'static str {
        match self {
            MlPreset::Classification => "Classification",
            MlPreset::ObjectDetection => "Object Detection",
            MlPreset::PoseEstimation => "Pose Estimation",
            MlPreset::HandDetection => "Hand Detection",
            MlPreset::FaceDetection => "Face Detection",
            MlPreset::Custom => "Custom",
        }
    }

    /// Default input size for this preset
    pub fn input_size(&self) -> u32 {
        match self {
            MlPreset::Classification => 224,
            MlPreset::ObjectDetection => 640,
            MlPreset::PoseEstimation => 128,  // MediaPipe pose detector uses 128
            MlPreset::HandDetection => 256,   // MediaPipe palm detector uses 256×256
            MlPreset::FaceDetection => 128,   // BlazeFace short-range uses 128×128
            MlPreset::Custom => 224,
        }
    }

    /// Whether to use ImageNet normalization (mean/std) vs simple 0–1
    pub fn imagenet_norm(&self) -> bool {
        matches!(self, MlPreset::Classification)
    }

    /// Whether the model expects NHWC [1, H, W, 3] instead of NCHW [1, 3, H, W].
    /// Most ONNX models use NCHW. If wrong, the auto-detect retry will correct it.
    pub fn is_nhwc(&self) -> bool {
        false // Default to NCHW; auto-detect handles the rest
    }
}

// ── Image Data ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8, 4 bytes per pixel
}

impl ImageData {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self { width, height, pixels }
    }
    #[allow(dead_code)]
    pub fn solid(width: u32, height: u32, r: u8, g: u8, b: u8, a: u8) -> Self {
        let pixels = vec![r, g, b, a].repeat((width * height) as usize);
        Self { width, height, pixels }
    }
}

// ── Port Value ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum PortValue {
    Float(f32),
    Text(String),
    Image(Arc<ImageData>),
    /// GPU-resident image handle. The actual `wgpu::Texture` lives in
    /// `GpuTextureCache.entries[(node_id, port)]`; this is just plain data
    /// (no Arc, no GPU resources held by clones). Producers emit this when
    /// no immediate downstream consumer needs CPU pixels (Tier 2 GPU-handoff).
    GpuImage(GpuImageHandle),
    None,
}

/// Plain-data handle to a GPU texture stored in `GpuTextureCache`.
/// Cloning is cheap (no Arc, no texture clone). The texture's lifetime is
/// owned by the cache itself; the `frame_stamp` is used by the eval loop's
/// param cache to detect "the producer re-rendered this frame".
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct GpuImageHandle {
    pub node_id: NodeId,
    pub port: usize,
    pub width: u32,
    pub height: u32,
    pub frame_stamp: u64,
}

// Custom serde: Image serializes as None (pixel data is runtime-only)
impl Serialize for PortValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            PortValue::Float(v) => {
                use serde::ser::SerializeMap;
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("Float", v)?;
                m.end()
            }
            PortValue::Text(v) => {
                use serde::ser::SerializeMap;
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("Text", v)?;
                m.end()
            }
            // Image bytes and GpuImage handles are runtime-only; both
            // serialize as the "None" token. On project load, downstream
            // GPU-handoff handles regenerate naturally on the next eval pass.
            PortValue::Image(_) | PortValue::GpuImage(_) | PortValue::None => {
                s.serialize_str("None")
            }
        }
    }
}

impl<'de> Deserialize<'de> for PortValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        match &v {
            serde_json::Value::Object(m) => {
                if let Some(f) = m.get("Float") {
                    Ok(PortValue::Float(f.as_f64().unwrap_or(0.0) as f32))
                } else if let Some(t) = m.get("Text") {
                    Ok(PortValue::Text(t.as_str().unwrap_or("").to_string()))
                } else {
                    Ok(PortValue::None)
                }
            }
            _ => Ok(PortValue::None),
        }
    }
}

impl PartialEq for PortValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PortValue::Float(a), PortValue::Float(b)) => a == b,
            (PortValue::Text(a), PortValue::Text(b)) => a == b,
            (PortValue::Image(a), PortValue::Image(b)) => Arc::ptr_eq(a, b),
            (PortValue::GpuImage(a), PortValue::GpuImage(b)) => {
                a.node_id == b.node_id
                    && a.port == b.port
                    && a.width == b.width
                    && a.height == b.height
                    && a.frame_stamp == b.frame_stamp
            }
            (PortValue::None, PortValue::None) => true,
            _ => false,
        }
    }
}

impl std::fmt::Display for PortValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PortValue::Float(v) => write!(f, "{:.3}", v),
            PortValue::Text(s) => {
                if s.len() > 24 { write!(f, "\"{}...\"", &s[..24]) }
                else { write!(f, "\"{}\"", s) }
            }
            PortValue::Image(img) => write!(f, "[Image {}x{}]", img.width, img.height),
            PortValue::GpuImage(h) => write!(f, "[GPU Image {}x{}]", h.width, h.height),
            PortValue::None => write!(f, "\u{2014}"),
        }
    }
}

impl PortValue {
    pub fn as_float(&self) -> f32 {
        match self { PortValue::Float(v) => *v, _ => 0.0 }
    }
    pub fn as_image(&self) -> Option<&Arc<ImageData>> {
        match self { PortValue::Image(img) => Some(img), _ => None }
    }

    /// Returns `(width, height)` for any image port (CPU `Image` or GPU
    /// `GpuImage`). Use when callers only need dimensions and shouldn't
    /// care which side of the GPU/CPU boundary the data lives on.
    pub fn image_dimensions(&self) -> Option<(u32, u32)> {
        match self {
            PortValue::Image(img) => Some((img.width, img.height)),
            PortValue::GpuImage(h) => Some((h.width, h.height)),
            _ => None,
        }
    }

    /// Returns the `(producer_node_id, producer_port)` source if this is
    /// a `GpuImage`, else `None`. Used by GPU consumers that want
    /// zero-copy texture sampling via `tex_cache.get_node_output(...)`.
    pub fn gpu_source(&self) -> Option<(NodeId, usize)> {
        match self {
            PortValue::GpuImage(h) => Some((h.node_id, h.port)),
            _ => None,
        }
    }

    /// Materialise this port to a CPU `Arc<ImageData>`. For
    /// `PortValue::Image` returns the existing Arc. For
    /// `PortValue::GpuImage`, performs an on-demand readback via
    /// `tex_cache.readback_node_output(...)`. Use in CPU consumers
    /// that genuinely need pixel bytes (Crop, Transform, save-to-disk,
    /// ML, etc.).
    pub fn into_cpu_image(
        &self,
        tex_cache: &crate::gpu_image::GpuTextureCache,
        device: &eframe::wgpu::Device,
        queue: &eframe::wgpu::Queue,
    ) -> Option<Arc<ImageData>> {
        match self {
            PortValue::Image(img) => Some(img.clone()),
            PortValue::GpuImage(h) => {
                tex_cache.readback_node_output(h.node_id, h.port, device, queue)
            }
            _ => None,
        }
    }
}

/// Semantic type of a port — drives visual shape, color, brightness behavior, and type hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PortKind {
    /// Generic continuous float value (slider, math result, frequency, etc.)
    Number,
    /// Float known to be 0.0–1.0 (mix, phase, opacity, progress)
    Normalized,
    /// Momentary pulse 0→1→0 (timer trigger, key press, gate pass)
    Trigger,
    /// Sustained on/off boolean (toggle, active, running)
    Gate,
    /// String data (text, file path, JSON, URL)
    Text,
    /// Pixel bitmap (RGBA image, video frame)
    Image,
    /// Audio signal routing (synth→fx→speaker)
    Audio,
    /// Individual color channel (R, G, or B, 0–255)
    Color,
    /// Unknown / any type (fallback)
    Generic,
}

impl PortKind {
    /// Base color for this port kind (used for port fill and wire color)
    pub fn base_color(&self) -> [u8; 3] {
        match self {
            Self::Number      => [80, 100, 230],    // blue
            Self::Normalized  => [80, 100, 230],    // blue (same family as Number)
            Self::Trigger     => [255, 120, 40],    // orange
            Self::Gate        => [255, 120, 40],    // orange (same family as Trigger)
            Self::Text        => [60, 220, 80],     // green
            Self::Image       => [200, 30, 255],    // purple
            Self::Audio       => [255, 50, 50],     // red
            Self::Color       => [80, 100, 230],    // blue (same family as Number)
            Self::Generic     => [80, 100, 230],    // blue (same family as Number)
        }
    }

    /// Phosphor icon glyph for this port kind
    pub fn icon_glyph(&self) -> &'static str {
        match self {
            Self::Number      => crate::icons::MATH_OPERATIONS,
            Self::Normalized  => crate::icons::SLIDERS,
            Self::Trigger     => crate::icons::LIGHTNING,
            Self::Gate        => crate::icons::TOGGLE_RIGHT,
            Self::Text        => crate::icons::TEXT_T,
            Self::Image       => crate::icons::IMAGE,
            Self::Audio       => crate::icons::WAVEFORM,
            Self::Color       => crate::icons::PALETTE,
            Self::Generic     => "",
        }
    }

    /// Shape identifier for rendering
    /// 0=Circle, 1=RoundedSquare, 2=Triangle, 3=Diamond, 4=HalfMoon
    pub fn shape_id(&self) -> u8 {
        match self {
            Self::Number      => 0, // circle
            Self::Normalized  => 0, // circle (with inner dot)
            Self::Trigger     => 2, // triangle
            Self::Gate        => 5, // square
            Self::Text        => 1, // rounded square
            Self::Image       => 3, // diamond
            Self::Audio       => 0, // circle (with inner dot)
            Self::Color       => 0, // circle
            Self::Generic     => 0, // circle
        }
    }

    /// Convert from PortValue (runtime inference, used as fallback)
    pub fn from_value(val: &PortValue) -> Self {
        match val {
            PortValue::Float(_) => Self::Number,
            PortValue::Text(_) => Self::Text,
            PortValue::Image(_) | PortValue::GpuImage(_) => Self::Image,
            PortValue::None => Self::Generic,
        }
    }

    /// Check if two port kinds are compatible for connection.
    /// Rules are intentionally permissive — numeric types (Number, Normalized, Trigger, Gate, Color)
    /// can interconnect since they all carry float values. Audio and Image are exclusive.
    /// Generic is compatible with everything. Text only connects to Text or Generic.
    pub fn compatible(from: PortKind, to: PortKind) -> bool {
        use PortKind::*;
        if from == Generic || to == Generic { return true; }
        match (from, to) {
            // Numeric family: all interchangeable (they're all f32 underneath)
            (Number | Normalized | Trigger | Gate | Color, Number | Normalized | Trigger | Gate | Color) => true,
            // Audio only connects to Audio
            (Audio, Audio) => true,
            // Image only connects to Image
            (Image, Image) => true,
            // Text only connects to Text
            (Text, Text) => true,
            // Everything else is incompatible
            _ => false,
        }
    }
}

pub struct PortDef {
    pub name: std::borrow::Cow<'static, str>,
    pub kind: PortKind,
}

impl PortDef {
    /// Create a PortDef with a static name (zero-cost, no allocation)
    pub fn new(name: &'static str, kind: PortKind) -> Self {
        Self { name: std::borrow::Cow::Borrowed(name), kind }
    }
    /// Create a PortDef with a dynamic name (owned String, freed on drop)
    pub fn dynamic(name: String, kind: PortKind) -> Self {
        Self { name: std::borrow::Cow::Owned(name), kind }
    }
}

// ── MIDI mode ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MidiMode { Note, CC }

// ── Node types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeType {
    Slider {
        value: f32,
        min: f32,
        max: f32,
        #[serde(default = "default_slider_step")]
        step: f32,
        #[serde(default = "default_slider_color")]
        slider_color: [u8; 3],
        #[serde(default)]
        label: String,
    },
    /// Folder browser: lists files in a directory, click to open
    FolderBrowser {
        #[serde(default)]
        path: String,
        #[serde(default)]
        selected_file: String,
        #[serde(default)]
        search: String,
        /// Cached contents of `selected_file` — avoids re-reading the file
        /// from disk on every frame's `evaluate()` pass. Invalidated when
        /// `selected_file` changes. Legacy enum variant only; the trait-
        /// based `FolderBrowserNode` already has this cache.
        #[serde(skip)]
        cached_content: String,
        #[serde(skip)]
        cached_content_path: String,
    },
    WgslViewer {
        #[serde(default)]
        wgsl_code: String,
        #[serde(default)]
        uniform_names: Vec<String>,
        #[serde(default)]
        uniform_types: Vec<String>,
        #[serde(default)]
        uniform_values: Vec<f32>,
        #[serde(default)]
        uniform_min: Vec<f32>,
        #[serde(default)]
        uniform_max: Vec<f32>,
        #[serde(default = "default_canvas_w")]
        canvas_w: f32,
        #[serde(default = "default_canvas_h")]
        canvas_h: f32,
        #[serde(default = "default_resolution")]
        resolution: u32,
        #[serde(default)]
        expanded: bool,
        // ── Image input feedback (see plan: wgsl-feedback-feature.md) ──
        /// Mode for `Input A` (`image_a` in WGSL): Unused / External / LastFrame.
        #[serde(default)]
        image_a_mode: ImageInputMode,
        /// Mode for `Input B` (`image_b` in WGSL): Unused / External / LastFrame.
        #[serde(default)]
        image_b_mode: ImageInputMode,
        /// Set to true to clear ping-pong feedback textures on the next render.
        #[serde(skip)]
        feedback_reset_pending: bool,
        /// Whether the most recent shader compile succeeded (gates auto-reset).
        #[serde(skip)]
        last_compile_ok: bool,
        /// Hash of the shader currently being typed (for debounced auto-reset).
        #[serde(skip)]
        pending_shader_hash: u64,
        /// Millisecond timestamp when `pending_shader_hash` was last seen change.
        #[serde(skip)]
        pending_shader_since_ms: u64,
        /// Whether the inline "how to use feedback" hint has been dismissed for Input A.
        #[serde(skip)]
        image_a_hint_shown: bool,
        /// Whether the inline "how to use feedback" hint has been dismissed for Input B.
        #[serde(skip)]
        image_b_hint_shown: bool,
        /// Millisecond timestamp of last auto-reset event (for ~300ms button flash).
        #[serde(skip)]
        last_auto_reset_ms: u64,
    },
    MidiOut {
        port_name: String,
        mode: MidiMode,
        channel: u8,
        #[serde(default)]
        manual_d1: u8,
        #[serde(default)]
        manual_d2: u8,
    },
    MidiIn {
        port_name: String,
        channel: u8,
        note: u8,
        velocity: u8,
        #[serde(default)]
        log: Vec<String>,
    },
    Theme {
        dark_mode: bool,
        accent: [u8; 3],
        font_size: f32,
        #[serde(default = "default_bg_color")]
        bg_color: [u8; 3],
        #[serde(default = "default_text_color")]
        text_color: [u8; 3],
        #[serde(default = "default_window_bg")]
        window_bg: [u8; 3],
        #[serde(default = "default_window_alpha")]
        window_alpha: u8,
        #[serde(default = "default_grid_color")]
        grid_color: [u8; 3],
        /// Grid style: 0=Solid (no grid), 1=Square, 2=Dotted
        #[serde(default = "default_grid_style")]
        grid_style: u8,
        /// Wire style: 0=Bezier, 1=Straight, 2=Orthogonal, 3=Wiggly
        #[serde(default)]
        wire_style: u8,
        /// Wiggly wire: gravity sag (0=none, 1=heavy droop)
        #[serde(default)]
        wiggle_gravity: f32,
        /// Wiggly wire: amplitude range multiplier (0.1=tiny, 2.0=wild)
        #[serde(default = "default_wiggle_range")]
        wiggle_range: f32,
        /// Wiggly wire: speed multiplier (0.1=slow, 2.0=fast)
        #[serde(default = "default_wiggle_speed")]
        wiggle_speed: f32,
        /// Wiggly wire: how much incoming signal activity (value changes)
        /// modulates the wiggle amplitude/frequency. 0 = ignore activity,
        /// 1 = full activity-driven animation.
        #[serde(default = "default_wiggle_signal")]
        wiggle_signal: f32,
        #[serde(default = "default_rounding")]
        rounding: f32,
        #[serde(default = "default_spacing")]
        spacing: f32,
        #[serde(default)]
        use_hsl: bool,
        /// Wire thickness (default 2.0)
        #[serde(default = "default_wire_thickness")]
        wire_thickness: f32,
        /// Background image/video path (shown on canvas behind nodes)
        #[serde(default)]
        background_path: String,
    },
    Serial {
        port_name: String,
        baud_rate: u32,
        #[serde(default)]
        log: Vec<String>,
        #[serde(default)]
        last_line: String,
        #[serde(default)]
        send_buf: String,
    },
    Script {
        name: String,
        input_names: Vec<String>,
        output_names: Vec<String>,
        code: String,
        #[serde(default)]
        last_values: Vec<f32>,
        #[serde(default)]
        error: String,
        #[serde(default = "default_true")]
        continuous: bool,
        #[serde(default)]
        trigger: bool,
    },
    Console {
        #[serde(default)]
        messages: Vec<String>,
    },
    OscOut {
        host: String,
        port: u16,
        address: String,
        arg_count: usize,
    },
    OscIn {
        port: u16,
        address_filter: String,
        #[serde(default)]
        arg_count: usize,
        #[serde(default)]
        last_args: Vec<f32>,
        /// Text representation of last args (preserves strings, formatted numbers)
        #[serde(default)]
        last_args_text: Vec<String>,
        #[serde(default)]
        log: Vec<String>,
        #[serde(default)]
        listening: bool,
        /// Auto-discovered addresses: (address, arg_count, last_preview)
        #[serde(default)]
        discovered: Vec<(String, usize, String)>,
    },
    Palette {
        #[serde(default)]
        search: String,
    },
    HttpRequest {
        #[serde(default)]
        url: String,
        #[serde(default)]
        method: String,        // GET or POST
        #[serde(default)]
        headers: String,       // Custom headers, key: value per line
        #[serde(default)]
        response: String,
        #[serde(default)]
        status: String,        // "idle" / "200 OK" / "error: ..."
        #[serde(default)]
        auto_send: bool,
        #[serde(default)]
        last_hash: u64,
    },
    AiRequest {
        #[serde(default = "default_provider")]
        provider: String,      // "anthropic" / "openai" / "google"
        #[serde(default = "default_model")]
        model: String,
        #[serde(default)]
        system_prompt: String,
        #[serde(default)]
        user_prompt: String,
        #[serde(default)]
        response: String,
        #[serde(default)]
        status: String,
        #[serde(default = "default_max_tokens")]
        max_tokens: u32,
        #[serde(default = "default_temperature")]
        temperature: f32,
        #[serde(default)]
        api_key: String,
        #[serde(default)]
        response_type: u8,     // Text mode: 0=Text, 1=JSON, 2=Code, 3=WGSL, 4=HTML
        #[serde(default)]
        mode: u8,              // 0 = Text, 1 = Image
        #[serde(default)]
        last_trigger: f32,     // for rising-edge detection on trigger port
        // Legacy fields kept for backward compatibility
        #[serde(default)]
        api_key_name: String,
        #[serde(default)]
        custom_url: String,
    },
    FileMenu,
    ZoomControl {
        #[serde(default)]
        zoom_value: f32,
    },
    ObHub {
        #[serde(default)]
        port_name: String,
        #[serde(default)]
        selected_port: String,
        /// (device_type, id) pairs discovered from the hub — updated each frame
        #[serde(default)]
        detected_devices: Vec<(String, u8)>,
    },
    ObJoystick {
        #[serde(default = "default_device_id")]
        device_id: u8,
        #[serde(default)]
        hub_node_id: NodeId,
        /// LED strip label color [R, G, B] — sent to device for visual identification
        #[serde(default = "default_orb_color")]
        label_color: [u8; 3],
    },
    ObEncoder {
        #[serde(default = "default_device_id")]
        device_id: u8,
        #[serde(default)]
        hub_node_id: NodeId,
        #[serde(default = "default_orb_color")]
        label_color: [u8; 3],
    },
    ObMove {
        #[serde(default = "default_device_id")]
        device_id: u8,
        #[serde(default)]
        hub_node_id: NodeId,
    },
    ObBend {
        #[serde(default = "default_device_id")]
        device_id: u8,
        #[serde(default)]
        hub_node_id: NodeId,
        #[serde(default = "default_orb_color")]
        label_color: [u8; 3],
    },
    ObPressure {
        #[serde(default = "default_device_id")]
        device_id: u8,
        #[serde(default)]
        hub_node_id: NodeId,
        #[serde(default = "default_orb_color")]
        label_color: [u8; 3],
    },
    /// OB Distance — Time-of-Flight sensor (VL53L0X) reporting a normalized
    /// 0..1 value mapped from a fixed firmware-side range (30..1200 mm for
    /// the VL53L0X). For a different output range, chain a MapRange node.
    /// Same port shape as Bend/Pressure/Knob.
    ObDistance {
        #[serde(default = "default_device_id")]
        device_id: u8,
        #[serde(default)]
        hub_node_id: NodeId,
        #[serde(default = "default_orb_color")]
        label_color: [u8; 3],
    },
    /// OB Knob — single-axis rotary knob sensor reporting a normalized value
    /// under `dev.values["value"]`. Symmetric with Bend/Pressure/Distance:
    /// one Normalized output + one "Changed" trigger. `label_color` drives
    /// the device's LED if it has one (harmless no-op otherwise).
    ObKnob {
        #[serde(default = "default_device_id")]
        device_id: u8,
        #[serde(default)]
        hub_node_id: NodeId,
        #[serde(default = "default_orb_color")]
        label_color: [u8; 3],
    },
    /// OB Orb — ESP32 with 8 WS2812B LED strips (output) + IMU accelerometer/gyroscope (input).
    /// Mode 0 = Manual per-strip RGB, 1 = Manual uniform color + brightness, 2+ = Effects.
    ObOrb {
        #[serde(default = "default_device_id")]
        device_id: u8,
        #[serde(default)]
        hub_node_id: NodeId,
        #[serde(default)]
        mode: u8,
        #[serde(default = "default_orb_color")]
        color: [u8; 3],
        #[serde(default)]
        param1: f32,
        #[serde(default)]
        param2: f32,
        #[serde(default = "default_one")]
        speed: f32,
        #[serde(default = "default_orb_brightness")]
        brightness: f32,
    },
    Synth {
        #[serde(default)]
        waveform: crate::audio::Waveform,
        #[serde(default = "default_440")]
        frequency: f32,
        #[serde(default = "default_half")]
        amplitude: f32,
        #[serde(default = "default_true")]
        active: bool,
        /// FM modulation depth in Hz
        #[serde(default)]
        fm_depth: f32,
    },
    AudioPlayer {
        #[serde(default)]
        file_path: String,
        #[serde(default = "default_one")]
        volume: f32,
        #[serde(default)]
        looping: bool,
        /// Duration of the loaded audio file in seconds (computed on load)
        #[serde(default)]
        duration_secs: f64,
    },
    AudioPlaylist {
        #[serde(default)]
        tracks: Vec<String>,
        #[serde(default)]
        current_index: usize,
        #[serde(default = "default_one")]
        volume: f32,
        /// Loop the whole playlist (wraps from last to first)
        #[serde(default)]
        loop_playlist: bool,
        /// Duration of the currently loaded track in seconds
        #[serde(default)]
        duration_secs: f64,
    },
    AudioInput {
        #[serde(default)]
        selected_device: String,
        #[serde(default = "default_mic_gain")]
        gain: f32,
        #[serde(default)]
        active: bool,
        /// Auto-level (AGC) on the mic signal. OFF by default so the raw
        /// signal passes through clean for voice effects, sampling, and
        /// general use. Users can toggle ON for the Music Visualizer
        /// spectrogram where built-in mics (~ -40 dBFS) need auto-boost.
        #[serde(default)]
        agc_enabled: bool,
    },
    /// Real-time audio analysis — outputs amplitude, bass, mid, treble from the master mix.
    AudioAnalyzer,
    /// FFT-based spectrum analyzer — visualizes log-spaced frequency bins
    /// and emits peak/centroid frequencies plus an Image of the bar display.
    SpectrumAnalyzer,
    AudioDevice {
        #[serde(default)]
        selected_output: String,
        #[serde(default)]
        selected_input: String,
        #[serde(default = "default_point_eight")]
        master_volume: f32,
        /// Central DSP enable switch — audio only flows when this is true.
        #[serde(default)]
        enabled: bool,
    },
    // Individual audio effect nodes
    AudioDelay {
        #[serde(default = "default_delay_ms")]
        time_ms: f32,
        #[serde(default = "default_half")]
        feedback: f32,
    },
    AudioDistortion {
        #[serde(default = "default_distortion_drive")]
        drive: f32,
    },
    AudioPitchShift {
        #[serde(default)]  // 0.0 semitones = no shift
        semitones: f32,
    },
    AudioLowPass {
        #[serde(default = "default_lpf_cutoff")]
        cutoff: f32,
    },
    AudioHighPass {
        #[serde(default = "default_hpf_cutoff")]
        cutoff: f32,
    },
    AudioGain {
        #[serde(default = "default_one")]
        level: f32,
    },
    /// Schroeder reverb — room size, damping, wet/dry mix.
    AudioReverb {
        #[serde(default = "default_half")]
        room_size: f32,
        #[serde(default = "default_half")]
        damping: f32,
        #[serde(default = "default_reverb_mix")]
        mix: f32,
    },
    /// Parametric EQ — interactive frequency response curve with biquad filter bank.
    AudioEq {
        #[serde(default = "default_eq_points")]
        points: Vec<[f32; 2]>,
    },
    Speaker {
        #[serde(default)]
        active: bool,
        #[serde(default = "default_point_eight")]
        volume: f32,
        #[serde(default)]
        pan: f32,
        /// Hardware channel offset (0 = ch 1-2, 2 = ch 3-4, etc.)
        #[serde(default)]
        channel_offset: usize,
        /// Output device name override. Empty = use primary device from AudioDevice node.
        #[serde(default)]
        device: String,
    },
    /// Audio Mixer: variable number of audio input channels, each with a gain fader.
    /// Per-channel: audio input + gain control input. One mixed audio output.
    AudioMixer {
        /// Number of input channels (min 2)
        #[serde(default = "default_mixer_channels")]
        channel_count: usize,
        /// Per-channel gain (0.0 – 1.0)
        #[serde(default = "default_mixer_gains")]
        gains: Vec<f32>,
    },
    /// Audio Sampler — records audio from input, plays back with trim + trigger.
    AudioSampler {
        /// Maximum recording duration in seconds.
        #[serde(default = "default_sampler_duration")]
        record_duration: f32,
        /// Trim start in seconds.
        #[serde(default)]
        trim_start: f32,
        /// Trim end in seconds (0 = full recording).
        #[serde(default)]
        trim_end: f32,
        /// Playback volume.
        #[serde(default = "default_one")]
        volume: f32,
        /// Loop playback.
        #[serde(default)]
        looping: bool,
        /// Reverse playback direction.
        #[serde(default)]
        reverse: bool,
        /// Play port behavior:
        /// - 0 = Gate (default, current behavior): plays while port > 0.5, stops on low
        /// - 1 = Trig: rising edge always (re)starts from beginning; ignores low
        /// - 2 = Full: rising edge starts only if not already playing (true one-shot)
        #[serde(default)]
        play_mode: u8,
        /// When true, the second range port (port 5) is interpreted as a
        /// duration in seconds added to the start position; when false it's
        /// an absolute end position. UI label for the port flips accordingly.
        #[serde(default)]
        range_as_duration: bool,
        /// Playback speed multiplier. 1.0 = normal, 2.0 = double, 0.5 = half.
        /// Can be overridden by port 6 (Speed input).
        #[serde(default = "default_one")]
        speed: f32,
        /// Last seek position (0.0–1.0 normalized within trim range).
        /// Serialized so UI slider shows the last used value on reload.
        #[serde(default)]
        seek: f32,
    },
    /// CLAP audio plugin — loaded from a .clap file.
    ClapPlugin {
        #[serde(default)]
        plugin_path: String,
        #[serde(default)]
        plugin_name: String,
        /// Cached parameter names (from plugin, for UI display)
        #[serde(default)]
        param_names: Vec<String>,
        /// Cached parameter ranges: [min, max, default]
        #[serde(default)]
        param_ranges: Vec<[f64; 3]>,
        /// Cached parameter flags
        #[serde(default)]
        param_flags: Vec<u32>,
        /// Current parameter values (normalized 0..1 for storage, scaled on send)
        #[serde(default)]
        param_values: Vec<f32>,
        /// Per-param text labels for enum/stepped params (empty vec for continuous)
        #[serde(default)]
        param_labels: Vec<Vec<String>>,
        /// Whether this plugin is an instrument (synth) vs effect
        #[serde(default)]
        is_instrument: bool,
    },
    RustPlugin {
        #[serde(default)]
        input_names: Vec<String>,
        #[serde(default)]
        output_names: Vec<String>,
        #[serde(default)]
        code: String,
        #[serde(default)]
        last_values: Vec<f64>,
        #[serde(default)]
        error: String,
    },
    McpServer,
    HtmlViewer,
    // ── Image & Signal nodes ────────────────────────────────
    ImageNode {
        #[serde(default)]
        path: String,
        #[serde(default)]
        save_path: String,
        #[serde(skip)]
        image_data: Option<Arc<ImageData>>,
        #[serde(default = "default_preview_size")]
        preview_size: f32,
        /// Hash of last saved image to avoid re-saving unchanged data
        #[serde(skip)]
        last_save_hash: u64,
        /// Local cache file written when `path` is a remote URL (e.g.
        /// expiring DALL·E links). Persisted across sessions so the
        /// node survives URL expiry — on cold load we read the bytes
        /// from this file instead of re-fetching the dead URL.
        #[serde(default)]
        cached_file: String,
    },
    ImageEffects {
        #[serde(default = "default_one")]
        brightness: f32,
        #[serde(default = "default_one")]
        contrast: f32,
        #[serde(default = "default_one")]
        saturation: f32,
        #[serde(default)]
        hue: f32,
        #[serde(default)]
        exposure: f32,
        #[serde(default = "default_one")]
        gamma: f32,
    },
    Curve {
        #[serde(default = "default_curve_points")]
        points: Vec<[f32; 2]>,
        /// 0 = Lookup (X→Y), 1 = Played (time-driven).
        /// Legacy: 2 = LFO is auto-migrated to 1 + looping=true on first eval.
        #[serde(default)]
        mode: u8,
        /// Playback speed: 1.0 = 1 second to traverse full curve (Played only)
        #[serde(default = "default_one")]
        speed: f32,
        /// Whether playback loops at the end
        #[serde(default)]
        looping: bool,
        /// Manual gate state when no Gate input is wired (used for sustain hold).
        #[serde(default)]
        manual_gate: bool,
        /// Index of the curve point that acts as the sustain hold boundary.
        /// `None` = no sustain (A→D→R only). When set, Played mode pauses
        /// phase at `points[sustain_index].x` while the Gate input is high.
        #[serde(default)]
        sustain_index: Option<u8>,
        /// Current playback position 0→1 (runtime only)
        #[serde(skip)]
        phase: f32,
        /// Is envelope currently playing (runtime only)
        #[serde(skip)]
        playing: bool,
        /// Last trigger input value for rising-edge detection (runtime only)
        #[serde(skip)]
        last_trigger: f32,
        /// Previous frame's phase for step crossing detection.
        /// -1.0 = "fresh" (re-fire any point at x=0 on the next frame).
        #[serde(skip, default = "default_neg_one")]
        last_phase: f32,
        /// Index of the most recently crossed curve point. Drives Step
        /// Value (sample-and-hold). -1 = none crossed yet.
        #[serde(skip, default = "default_neg_one_i8")]
        last_step_index: i8,
        /// True when phase is currently held at the sustain marker.
        /// Distinct from `playing` so we resume on gate-low instead of
        /// starting fresh on the next trigger.
        #[serde(skip)]
        sustain_held: bool,
    },
    Draw {
        #[serde(default)]
        strokes: Vec<DrawStroke>,
        #[serde(default = "default_draw_size")]
        canvas_size: f32,
        #[serde(default)]
        color: [u8; 3],
        #[serde(default = "default_draw_width")]
        line_width: f32,
    },
    ColorCurves {
        #[serde(default = "default_curve_points")]
        master: Vec<[f32; 2]>,
        #[serde(default = "default_curve_points")]
        red: Vec<[f32; 2]>,
        #[serde(default = "default_curve_points")]
        green: Vec<[f32; 2]>,
        #[serde(default = "default_curve_points")]
        blue: Vec<[f32; 2]>,
        #[serde(default)]
        active_channel: u8,
    },
    VideoPlayer {
        #[serde(default)]
        path: String,
        #[serde(default)]
        playing: bool,
        #[serde(default)]
        looping: bool,
        #[serde(default = "default_video_w")]
        res_w: u32,
        #[serde(default = "default_video_h")]
        res_h: u32,
        #[serde(skip)]
        current_frame: Option<Arc<ImageData>>,
        #[serde(default)]
        duration: f32,
        #[serde(default = "default_speed")]
        speed: f32,
        #[serde(default)]
        status: String,
    },
    Camera {
        #[serde(default)]
        device_index: u32,
        #[serde(default = "default_video_w")]
        res_w: u32,
        #[serde(default = "default_video_h")]
        res_h: u32,
        #[serde(default)]
        active: bool,
        #[serde(skip)]
        current_frame: Option<Arc<ImageData>>,
        #[serde(default)]
        status: String,
    },
    MlModel {
        #[serde(default)]
        model_path: String,
        #[serde(default)]
        labels_path: String,
        #[serde(default = "default_confidence")]
        confidence: f32,
        #[serde(default)]
        preset: MlPreset,
        #[serde(default)]
        result_text: String,
        #[serde(default)]
        result_json: String,
        #[serde(skip)]
        annotated_frame: Option<std::sync::Arc<ImageData>>,
        #[serde(default)]
        status: String,
        #[serde(skip)]
        last_input_hash: u64,
        #[serde(default = "default_ml_interval")]
        interval_secs: f32,
        #[serde(skip)]
        last_inference_secs: f64,
    },
    /// Gate: Compare + pass/block in one node.
    /// Timer/Interval: periodic pulse every N seconds.
    /// Uses wall-clock reference time for drift-free tempo sync.
    Timer {
        #[serde(default = "default_one")]
        interval: f32,
        #[serde(default)]
        elapsed: f32,
        #[serde(default = "default_true")]
        running: bool,
        /// How long the trigger stays high (seconds)
        #[serde(default = "default_pulse_width")]
        pulse_width: f32,
        /// Wall-clock reference time (seconds since app start) when timer was started/resumed.
        /// elapsed is computed as `now - ref_time + paused_elapsed`.
        /// Skipped in serialization — re-initialized on load.
        #[serde(skip)]
        ref_time: f64,
        /// Accumulated elapsed time when paused (so we can resume seamlessly).
        #[serde(skip)]
        paused_elapsed: f64,
        /// Whether ref_time has been initialized this session.
        #[serde(skip)]
        time_initialized: bool,
        /// Fine phase offset for sync mode (via Phase In port). Persisted.
        #[serde(default)]
        phase_offset: f32,
        /// Previous shifted-phase value for sync edge detection.
        #[serde(skip)]
        last_sync_phase: f32,
    },
    /// Sample & Hold: capture value on trigger rising edge, hold until next
    SampleHold {
        #[serde(default)]
        held_float: f32,
        #[serde(default)]
        held_text: String,
        /// Whether the last held value was text (true) or float (false)
        #[serde(default)]
        is_text: bool,
        /// Last trigger value for rising-edge detection
        #[serde(default)]
        last_trigger: f32,
        /// History of held float values for staircase visualization
        #[serde(default)]
        history: Vec<f32>,
    },
    /// Network Send — broadcasts values to connected peers via iroh-gossip.
    NetworkSend {
        #[serde(default)]
        schema: Vec<NetPortSchema>,
        /// 0 = Ephemeral, 1 = Permanent
        #[serde(default)]
        identity_mode: u8,
        #[serde(default)]
        persistent_secret: Option<String>,
        #[serde(default)]
        label: String,
        #[serde(default)]
        last_link: Option<String>,
        #[serde(default)]
        last_peer_count: usize,
        #[serde(default)]
        initialized: bool,
    },
    /// Network Receive — receives values from a peer via iroh-gossip link.
    NetworkReceive {
        #[serde(default)]
        link: String,
        #[serde(default)]
        imported_schema: Vec<NetPortSchema>,
        #[serde(default)]
        last_values: Vec<f32>,
        #[serde(default)]
        last_values_text: Vec<String>,
        #[serde(default)]
        last_sender: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        connected: bool,
    },
    /// Trait-based node — standalone struct implementing NodeBehavior.
    /// This is the plugin pathway: external nodes use this variant.
    /// Serialization is handled by DynNode's custom Serialize/Deserialize impl
    /// (type_tag + save_state → JSON, reconstructed via NODE_REGISTRY on load).
    Dynamic {
        inner: DynNode,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetPortSchema {
    pub name: String,
    pub kind: NetPortKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum NetPortKind {
    Float,
    Text,
    Image,
}

/// Wrapper around Box<dyn NodeBehavior> that implements Clone + Debug.
/// Serde is handled via the NodeRegistry (type_tag + save_state/load_state).
pub struct DynNode {
    pub node: Box<dyn NodeBehavior>,
}

impl Clone for DynNode {
    fn clone(&self) -> Self {
        // Reconstruct from serialized state via the registry
        let tag = self.node.type_tag();
        let state = self.node.save_state();
        if let Some(cloned) = crate::node_trait::NODE_REGISTRY.lock().ok().and_then(|r| r.create(tag, &state)) {
            DynNode { node: cloned }
        } else {
            // Fallback: create a placeholder
            DynNode { node: Box::new(crate::nodes::add_node::AddNode::default()) }
        }
    }
}

impl std::fmt::Debug for DynNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DynNode({})", self.node.title())
    }
}

impl serde::Serialize for DynNode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(2))?;
        map.serialize_entry("type_tag", self.node.type_tag())?;
        map.serialize_entry("state", &self.node.save_state())?;
        map.end()
    }
}

impl<'de> serde::Deserialize<'de> for DynNode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let tag = v.get("type_tag").and_then(|t| t.as_str()).unwrap_or("add");
        let state = v.get("state").cloned().unwrap_or(serde_json::Value::Null);
        let node_opt = crate::node_trait::NODE_REGISTRY.lock().ok().and_then(|r| r.create(tag, &state));
        if let Some(node) = node_opt {
            Ok(DynNode { node })
        } else {
            Ok(DynNode { node: Box::new(crate::nodes::add_node::AddNode::default()) })
        }
    }
}

fn default_pulse_width() -> f32 { 0.1 }
fn default_confidence() -> f32 { 0.05 }
fn default_ml_interval() -> f32 { 0.5 }
fn default_video_w() -> u32 { 640 }
fn default_video_h() -> u32 { 480 }
fn default_speed() -> f32 { 1.0 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawStroke {
    pub points: Vec<[f32; 2]>,
    pub color: [u8; 3],
    pub width: f32,
}

fn default_device_id() -> u8 { 1 }
fn default_orb_color() -> [u8; 3] { [255, 255, 255] }
fn default_orb_brightness() -> f32 { 1.0 }
fn default_preview_size() -> f32 { 150.0 }
fn default_draw_size() -> f32 { 200.0 }
fn default_draw_width() -> f32 { 2.0 }
fn default_curve_points() -> Vec<[f32; 2]> { vec![[0.0, 0.0], [1.0, 1.0]] }
fn default_neg_one() -> f32 { -1.0 }
fn default_neg_one_i8() -> i8 { -1 }
/// Flat EQ: 5 points at 0dB (y=0.5) across the frequency range
fn default_eq_points() -> Vec<[f32; 2]> { vec![[0.0, 0.5], [0.25, 0.5], [0.5, 0.5], [0.75, 0.5], [1.0, 0.5]] }
fn default_sampler_duration() -> f32 { 5.0 }
fn default_mixer_channels() -> usize { 2 }
fn default_mixer_gains() -> Vec<f32> { vec![0.8, 0.8] }
fn default_440() -> f32 { 440.0 }
fn default_half() -> f32 { 0.5 }
fn default_reverb_mix() -> f32 { 0.3 }
fn default_one() -> f32 { 1.0 }
fn default_mic_gain() -> f32 { 1.0 }
fn default_point_eight() -> f32 { 0.8 }
fn default_delay_ms() -> f32 { 250.0 }
fn default_distortion_drive() -> f32 { 4.0 }
fn default_lpf_cutoff() -> f32 { 1000.0 }
fn default_hpf_cutoff() -> f32 { 200.0 }

impl NodeBehavior for NodeType {
    fn title(&self) -> &str {
        match self {
            NodeType::Slider { .. } => "Slider",
            NodeType::FolderBrowser { .. } => "Folder",
            NodeType::WgslViewer { .. } => "WGSL Viewer",
            NodeType::MidiOut { .. } => "MIDI Out",
            NodeType::MidiIn { .. } => "MIDI In",
            NodeType::Theme { .. } => "Theme",
            NodeType::Serial { .. } => "Serial",
            NodeType::Script { .. } => "Script",
            NodeType::Console { .. } => "Console",
            NodeType::OscOut { .. } => "OSC Out",
            NodeType::OscIn { .. } => "OSC In",
            NodeType::Palette { .. } => "Node Palette",
            NodeType::HttpRequest { .. } => "HTTP Request",
            NodeType::AiRequest { .. } => "AI Request",
            NodeType::FileMenu => "File",
            NodeType::ZoomControl { .. } => "Zoom",
            NodeType::ObHub { .. } => "OB Hub",
            NodeType::ObJoystick { .. } => "OB Joystick",
            // UI label only — internal protocol type stays "encoder" so all
            // existing hub-based flows, saved projects, and mesh forwarding
            // continue working unchanged.
            NodeType::ObEncoder { .. } => "OB Wheel",
            NodeType::ObMove { .. } => "OB Move",
            NodeType::ObBend { .. } => "OB Bend",
            NodeType::ObPressure { .. } => "OB Pressure",
            NodeType::ObDistance { .. } => "OB Distance",
            NodeType::ObKnob { .. } => "OB Knob",
            NodeType::ObOrb { .. } => "OB Orb",
            NodeType::Synth { .. } => "Synth",
            NodeType::AudioPlayer { .. } => "Audio Player",
            NodeType::AudioPlaylist { .. } => "Playlist",
            NodeType::AudioInput { .. } => "Microphone",
            NodeType::AudioAnalyzer => "Audio Analyzer",
            NodeType::SpectrumAnalyzer => "Spectrum Analyzer",
            NodeType::AudioDevice { .. } => "Audio Manager",
            NodeType::AudioDelay { .. } => "Delay",
            NodeType::AudioDistortion { .. } => "Distortion",
            NodeType::AudioPitchShift { .. } => "Pitch Shift",
            NodeType::AudioReverb { .. } => "Reverb",
            NodeType::AudioLowPass { .. } => "Low Pass",
            NodeType::AudioHighPass { .. } => "High Pass",
            NodeType::AudioGain { .. } => "Gain",
            NodeType::AudioEq { .. } => "EQ",
            NodeType::Speaker { .. } => "Speaker",
            NodeType::AudioMixer { .. } => "Mixer",
            NodeType::AudioSampler { .. } => "Audio Sampler",
            NodeType::ClapPlugin { plugin_name, .. } => if plugin_name.is_empty() { "CLAP Plugin" } else { plugin_name.as_str() },
            NodeType::RustPlugin { .. } => "Rust Plugin",
            NodeType::McpServer => "MCP Server",
            NodeType::HtmlViewer => "HTML Viewer",
            NodeType::ImageNode { .. } => "Image",
            NodeType::ImageEffects { .. } => "Image Effects",
            NodeType::Curve { .. } => "Curve",
            NodeType::Draw { .. } => "Draw",
            NodeType::ColorCurves { .. } => "Color Curves",
            NodeType::VideoPlayer { .. } => "Video Player",
            NodeType::Camera { .. } => "Camera",
            NodeType::MlModel { .. } => "ML Model",
            NodeType::Timer { .. } => "Timer",
            NodeType::SampleHold { .. } => "Sample & Hold",
            NodeType::NetworkSend { .. } => "Net Send",
            NodeType::NetworkReceive { .. } => "Net Receive",
            NodeType::Dynamic { inner } => inner.node.title(),
        }
    }

    fn inputs(&self) -> Vec<PortDef> {
        use PortKind::*;
        match self {
            NodeType::Slider { .. } => vec![PortDef::new("In", Number), PortDef::new("Min", Number), PortDef::new("Max", Number)],
            NodeType::FolderBrowser { .. } => vec![],
            NodeType::WgslViewer { uniform_names, uniform_types, .. } => {
                let mut ports = vec![PortDef::new("WGSL", Text)];
                for (i, n) in uniform_names.iter().enumerate() {
                    let t = uniform_types.get(i).map(|s| s.as_str()).unwrap_or("float");
                    if t == "color" {
                        ports.push(PortDef::dynamic(format!("{} R", n), Color));
                        ports.push(PortDef::dynamic(format!("{} G", n), Color));
                        ports.push(PortDef::dynamic(format!("{} B", n), Color));
                    } else {
                        ports.push(PortDef::dynamic(n.clone(), Number));
                    }
                }
                // Image input slots — appended last so existing port indices for
                // the WGSL/uniform ports remain stable. See feedback plan §1/§7.
                ports.push(PortDef::new("Input A", Image));
                ports.push(PortDef::new("Input B", Image));
                ports
            }
            NodeType::MidiOut { mode, .. } => match mode {
                MidiMode::Note => vec![PortDef::new("Channel", Number), PortDef::new("Note", Number), PortDef::new("Velocity", Number)],
                MidiMode::CC => vec![PortDef::new("Channel", Number), PortDef::new("CC#", Number), PortDef::new("Value", Number)],
            },
            NodeType::MidiIn { .. } => vec![],
            NodeType::Theme { .. } => vec![
                PortDef::new("BG R", Color), PortDef::new("BG G", Color), PortDef::new("BG B", Color),
                PortDef::new("Text R", Color), PortDef::new("Text G", Color), PortDef::new("Text B", Color),
                PortDef::new("Accent R", Color), PortDef::new("Accent G", Color), PortDef::new("Accent B", Color),
                PortDef::new("Win R", Color), PortDef::new("Win G", Color), PortDef::new("Win B", Color),
                PortDef::new("Grid R", Color), PortDef::new("Grid G", Color), PortDef::new("Grid B", Color),
                PortDef::new("Font Size", Number),
                PortDef::new("Rounding", Number),
                PortDef::new("Spacing", Number),
                PortDef::new("Win Alpha", Normalized),
                PortDef::new("Background", Text),
                PortDef::new("Wire", Number),
                PortDef::new("BG Image", Image),
                PortDef::new("W Gravity", Normalized),
                PortDef::new("W Range", Normalized),
                PortDef::new("W Speed", Normalized),
                PortDef::new("W Signal", Normalized),
            ],
            NodeType::Serial { .. } => vec![PortDef::new("Send", Text)],
            NodeType::Console { .. } => vec![PortDef::new("Log", Generic)],
            NodeType::OscOut { arg_count, .. } => {
                (0..*arg_count).map(|i| PortDef::dynamic(format!("Arg {}", i), Generic)).collect()
            }
            NodeType::OscIn { .. } => vec![],
            NodeType::Palette { .. } => vec![],
            NodeType::HttpRequest { .. } => vec![PortDef::new("URL", Text), PortDef::new("Body", Text), PortDef::new("Headers", Text)],
            NodeType::AiRequest { .. } => vec![
                PortDef::new("System", Text),
                PortDef::new("Prompt", Text),
                PortDef::new("Send", Trigger),
                PortDef::new("Image", Image),
            ],
            NodeType::FileMenu => vec![],
            NodeType::ZoomControl { .. } => vec![PortDef::new("Zoom", Number)],
            NodeType::ObHub { .. } => vec![PortDef::new("Command", Text)],
            NodeType::ObJoystick { .. } => vec![],
            NodeType::ObEncoder { .. } => vec![],
            NodeType::ObMove { .. } => vec![],
            NodeType::ObBend { .. } => vec![],
            NodeType::ObPressure { .. } => vec![],
            NodeType::ObDistance { .. } => vec![],
            NodeType::ObKnob { .. } => vec![],
            NodeType::ObOrb { .. } => {
                vec![PortDef::new("Drive", Number)]
            },
            NodeType::Synth { .. } => vec![PortDef::new("Freq", Number), PortDef::new("Amp", Normalized), PortDef::new("Gate", Gate), PortDef::new("FM Wt", Normalized)],
            NodeType::AudioPlayer { .. } => vec![PortDef::new("Play", Trigger), PortDef::new("Volume", Normalized), PortDef::new("Seek", Normalized), PortDef::new("Speed", Number)],
            NodeType::AudioPlaylist { .. } => vec![
                PortDef::new("Play",   Trigger),
                PortDef::new("Stop",   Trigger),
                PortDef::new("Next",   Trigger),
                PortDef::new("Prev",   Trigger),
                PortDef::new("Index",  Number),
                PortDef::new("Volume", Normalized),
            ],
            NodeType::AudioInput { .. } => vec![PortDef::new("Gain", Normalized)],
            NodeType::AudioAnalyzer => vec![PortDef::new("Audio", Audio)],
            NodeType::SpectrumAnalyzer => vec![PortDef::new("Audio", Audio)],
            NodeType::AudioDevice { .. } => vec![],
            NodeType::AudioDelay { .. } => vec![PortDef::new("Audio", Audio), PortDef::new("Time", Number), PortDef::new("Feedback", Normalized)],
            NodeType::AudioDistortion { .. } => vec![PortDef::new("Audio", Audio), PortDef::new("Drive", Number)],
            NodeType::AudioPitchShift { .. } => vec![PortDef::new("Audio", Audio), PortDef::new("Semitones", Number)],
            NodeType::AudioReverb { .. } => vec![PortDef::new("Audio", Audio), PortDef::new("Room", Normalized), PortDef::new("Damp", Normalized), PortDef::new("Mix", Normalized)],
            NodeType::AudioLowPass { .. } => vec![PortDef::new("Audio", Audio), PortDef::new("Cutoff", Number)],
            NodeType::AudioHighPass { .. } => vec![PortDef::new("Audio", Audio), PortDef::new("Cutoff", Number)],
            NodeType::AudioGain { .. } => vec![PortDef::new("Audio", Audio), PortDef::new("Level", Number)],
            NodeType::AudioEq { .. } => vec![PortDef::new("Audio", Audio)],
            NodeType::Speaker { .. } => vec![PortDef::new("L/Mono", Audio), PortDef::new("R", Audio), PortDef::new("Volume", Normalized), PortDef::new("Pan", Number)],
            NodeType::AudioMixer { channel_count, .. } => {
                // Per channel: Audio input + Gain control input
                let mut ports = Vec::new();
                for i in 0..*channel_count {
                    ports.push(PortDef::dynamic(format!("Ch{}", i + 1), Audio));
                    ports.push(PortDef::dynamic(format!("Gain{}", i + 1), Normalized));
                }
                ports
            }
            NodeType::AudioSampler { range_as_duration, .. } => vec![
                PortDef::new("Audio", Audio),
                PortDef::new("Rec", Trigger),
                PortDef::new("Play", Trigger),
                PortDef::new("Vol", Normalized),
                PortDef::new("Start", Number),
                if *range_as_duration {
                    PortDef::new("Dur", Number)
                } else {
                    PortDef::new("End", Number)
                },
                PortDef::new("Speed", Number),
                PortDef::new("Seek", Normalized),
            ],
            NodeType::ClapPlugin { param_names, is_instrument, .. } => {
                let mut ports = Vec::new();
                if *is_instrument {
                    ports.push(PortDef::new("Note", Number));
                    ports.push(PortDef::new("Vel", Normalized));
                    ports.push(PortDef::new("Gate", Gate));
                } else {
                    ports.push(PortDef::new("Audio", Audio));
                }
                for name in param_names {
                    ports.push(PortDef::dynamic(name.clone(), Normalized));
                }
                ports
            }
            NodeType::RustPlugin { input_names, .. } => {
                input_names.iter().map(|n| PortDef::dynamic(n.clone(), Generic)).collect()
            }
            NodeType::McpServer => vec![],
            NodeType::HtmlViewer => vec![PortDef::new("HTML", Text)],
            NodeType::ImageNode { .. } => vec![
                PortDef::new("Image In", Image),
                PortDef::new("Src", Text),
            ],
            NodeType::ImageEffects { .. } => vec![
                PortDef::new("Image", Image), PortDef::new("Brightness", Normalized), PortDef::new("Contrast", Normalized),
                PortDef::new("Saturation", Normalized), PortDef::new("Hue", Number), PortDef::new("Exposure", Number), PortDef::new("Gamma", Number),
            ],
            NodeType::Curve { .. } => vec![
                PortDef::new("X", Normalized), PortDef::new("Trigger", Trigger),
                PortDef::new("Speed", Number), PortDef::new("Gate", Gate),
            ],
            NodeType::Draw { .. } => vec![],
            NodeType::ColorCurves { .. } => vec![PortDef::new("Image", Image)],
            NodeType::VideoPlayer { .. } => vec![],
            NodeType::Camera { .. } => vec![],
            NodeType::MlModel { .. } => vec![PortDef::new("Image", Image)],
            NodeType::Timer { .. } => vec![
                PortDef::new("Interval", Number),
                PortDef::new("BPM", Number),
                PortDef::new("Phase", Normalized),   // sync: 0..1 from Clock or another Timer
                PortDef::new("Offset", Normalized),   // overrides inline phase_offset when wired
            ],
            NodeType::SampleHold { .. } => vec![PortDef::new("Value", Generic), PortDef::new("Trigger", Trigger)],
            NodeType::NetworkSend { schema, .. } => {
                schema.iter().map(|s| PortDef::dynamic(s.name.clone(), match s.kind {
                    NetPortKind::Float => Number,
                    NetPortKind::Text => Text,
                    NetPortKind::Image => Image,
                })).collect()
            }
            NodeType::NetworkReceive { .. } => vec![],
            NodeType::Script { input_names, continuous, .. } => {
                let mut ports: Vec<PortDef> = Vec::new();
                if !continuous { ports.push(PortDef::new("Exec", Trigger)); }
                ports.push(PortDef::new("Code", Text));
                for n in input_names {
                    ports.push(PortDef::dynamic(n.clone(), Generic));
                }
                ports
            }
            NodeType::Dynamic { inner } => inner.node.inputs(),
        }
    }

    fn outputs(&self) -> Vec<PortDef> {
        use PortKind::*;
        match self {
            NodeType::Slider { .. } => vec![PortDef::new("Value", Number)],
            NodeType::FolderBrowser { .. } => vec![PortDef::new("Path", Text), PortDef::new("Name", Text), PortDef::new("Content", Text)],
            NodeType::WgslViewer { .. } => vec![PortDef::new("Image", Image)],
            NodeType::MidiOut { .. } => vec![],
            NodeType::MidiIn { .. } => vec![PortDef::new("Channel", Number), PortDef::new("Note", Number), PortDef::new("Velocity", Number)],
            NodeType::Theme { .. } => vec![
                PortDef::new("BG R", Color), PortDef::new("BG G", Color), PortDef::new("BG B", Color),
                PortDef::new("Text R", Color), PortDef::new("Text G", Color), PortDef::new("Text B", Color),
                PortDef::new("Accent R", Color), PortDef::new("Accent G", Color), PortDef::new("Accent B", Color),
            ],
            NodeType::Serial { .. } => vec![PortDef::new("Send", Text)],
            NodeType::Console { .. } => vec![],
            NodeType::OscOut { .. } => vec![],
            NodeType::OscIn { arg_count, .. } => {
                let mut ports: Vec<PortDef> = (0..*arg_count).map(|i| PortDef::dynamic(format!("Arg {}", i), Generic)).collect();
                ports.push(PortDef::new("Raw", Text));
                ports.push(PortDef::new("Address", Text));
                ports
            }
            NodeType::Script { output_names, .. } => {
                output_names.iter().map(|n| PortDef::dynamic(n.clone(), Generic)).collect()
            }
            NodeType::Palette { .. } => vec![],
            NodeType::HttpRequest { .. } => vec![PortDef::new("Response", Text), PortDef::new("Status", Text)],
            NodeType::AiRequest { .. } => vec![PortDef::new("Response", Text), PortDef::new("Status", Text)],
            NodeType::FileMenu => vec![],
            NodeType::ZoomControl { .. } => vec![PortDef::new("Zoom", Number)],
            NodeType::ObHub { detected_devices, .. } => {
                let mut ports = Vec::new();
                let mut sorted = detected_devices.clone();
                sorted.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
                for (dtype, id) in &sorted {
                    match dtype.as_str() {
                        "joystick" => {
                            ports.push(PortDef::dynamic(format!("j{}_x", id), Normalized));
                            ports.push(PortDef::dynamic(format!("j{}_y", id), Normalized));
                            ports.push(PortDef::dynamic(format!("j{}_btn", id), Gate));
                        }
                        "encoder" => {
                            ports.push(PortDef::dynamic(format!("e{}_turn", id), Number));
                            ports.push(PortDef::dynamic(format!("e{}_click", id), Gate));
                            ports.push(PortDef::dynamic(format!("e{}_pos", id), Number));
                        }
                        "move" => {
                            ports.push(PortDef::dynamic(format!("m{}_ax", id), Number));
                            ports.push(PortDef::dynamic(format!("m{}_ay", id), Number));
                            ports.push(PortDef::dynamic(format!("m{}_az", id), Number));
                            ports.push(PortDef::dynamic(format!("m{}_gx", id), Number));
                            ports.push(PortDef::dynamic(format!("m{}_gy", id), Number));
                            ports.push(PortDef::dynamic(format!("m{}_gz", id), Number));
                        }
                        "bend" => {
                            ports.push(PortDef::dynamic(format!("b{}_val", id), Normalized));
                        }
                        "pressure" => {
                            ports.push(PortDef::dynamic(format!("p{}_val", id), Normalized));
                        }
                        "distance" => {
                            ports.push(PortDef::dynamic(format!("d{}_val", id), Normalized));
                        }
                        "knob" => {
                            ports.push(PortDef::dynamic(format!("k{}_val", id), Normalized));
                        }
                        "orb" => {
                            ports.push(PortDef::dynamic(format!("orb{}_ax", id), Number));
                            ports.push(PortDef::dynamic(format!("orb{}_ay", id), Number));
                            ports.push(PortDef::dynamic(format!("orb{}_az", id), Number));
                            ports.push(PortDef::dynamic(format!("orb{}_gx", id), Number));
                            ports.push(PortDef::dynamic(format!("orb{}_gy", id), Number));
                            ports.push(PortDef::dynamic(format!("orb{}_gz", id), Number));
                        }
                        other => {
                            ports.push(PortDef::dynamic(format!("{}{}_{}", &other[..1], id, "val"), Number));
                        }
                    }
                }
                if ports.is_empty() {
                    ports.push(PortDef::new("(no devices)", Generic));
                }
                ports
            },
            NodeType::ObMove { .. } => vec![
                PortDef::new("Accel X", Number), PortDef::new("Accel Y", Number), PortDef::new("Accel Z", Number),
                PortDef::new("Gyro X", Number), PortDef::new("Gyro Y", Number), PortDef::new("Gyro Z", Number),
                PortDef::new("Changed", Trigger),
            ],
            NodeType::ObBend { .. } => vec![PortDef::new("Value", Normalized), PortDef::new("Changed", Trigger)],
            NodeType::ObPressure { .. } => vec![PortDef::new("Value", Normalized), PortDef::new("Changed", Trigger)],
            NodeType::ObDistance { .. } => vec![
                PortDef::new("Value", Normalized),
                PortDef::new("Distance (mm)", Number),
                PortDef::new("Changed", Trigger),
            ],
            NodeType::ObKnob { .. } => vec![PortDef::new("Value", Normalized), PortDef::new("Changed", Trigger)],
            NodeType::ObJoystick { .. } => vec![PortDef::new("X", Normalized), PortDef::new("Y", Normalized), PortDef::new("Button", Gate), PortDef::new("Changed", Trigger)],
            NodeType::ObEncoder { .. } => vec![PortDef::new("Turn", Number), PortDef::new("Click", Gate), PortDef::new("Position", Number), PortDef::new("Changed", Trigger)],
            NodeType::ObOrb { .. } => vec![
                PortDef::new("Accel X", Number), PortDef::new("Accel Y", Number), PortDef::new("Accel Z", Number),
                PortDef::new("Gyro X", Number), PortDef::new("Gyro Y", Number), PortDef::new("Gyro Z", Number),
                PortDef::new("Changed", Trigger),
            ],
            NodeType::Synth { .. } => vec![PortDef::new("Audio", Audio)],
            NodeType::AudioPlayer { .. } => vec![PortDef::new("Audio", Audio), PortDef::new("Progress", Normalized)],
            NodeType::AudioPlaylist { .. } => vec![
                PortDef::new("Audio",    Audio),
                PortDef::new("Progress", Normalized),
                PortDef::new("Index",    Number),
                PortDef::new("Name",     Text),
            ],
            NodeType::AudioInput { .. } => vec![PortDef::new("Audio", Audio)],
            NodeType::AudioAnalyzer => vec![
                PortDef::new("Audio", Audio),
                PortDef::new("Amp", Normalized), PortDef::new("Peak", Normalized),
                PortDef::new("Bass", Normalized), PortDef::new("Mid", Normalized),
                PortDef::new("Treble", Normalized),
            ],
            NodeType::SpectrumAnalyzer => vec![
                PortDef::new("Image", Image),
                PortDef::new("Peak Hz", Number),
                PortDef::new("Centroid Hz", Number),
                PortDef::new("Energy", Normalized),
            ],
            NodeType::AudioDevice { .. } => vec![],
            NodeType::AudioDelay { .. } => vec![PortDef::new("Audio", Audio)],
            NodeType::AudioDistortion { .. } => vec![PortDef::new("Audio", Audio)],
            NodeType::AudioPitchShift { .. } => vec![PortDef::new("Audio", Audio)],
            NodeType::AudioReverb { .. } => vec![PortDef::new("Audio", Audio)],
            NodeType::AudioLowPass { .. } => vec![PortDef::new("Audio", Audio)],
            NodeType::AudioHighPass { .. } => vec![PortDef::new("Audio", Audio)],
            NodeType::AudioGain { .. } => vec![PortDef::new("Audio", Audio)],
            NodeType::AudioEq { .. } => vec![PortDef::new("Audio", Audio)],
            NodeType::Speaker { .. } => vec![],
            NodeType::AudioMixer { .. } => vec![PortDef::new("Mix", Audio)],
            NodeType::AudioSampler { .. } => vec![PortDef::new("Audio", Audio), PortDef::new("Progress", Normalized)],
            NodeType::ClapPlugin { .. } => vec![PortDef::new("Audio", Audio)],
            NodeType::RustPlugin { output_names, .. } => {
                output_names.iter().map(|n| PortDef::dynamic(n.clone(), Generic)).collect()
            }
            NodeType::McpServer => vec![],
            NodeType::HtmlViewer => vec![PortDef::new("URL", Text)],
            NodeType::ImageNode { .. } => vec![PortDef::new("Image", Image)],
            NodeType::ImageEffects { .. } => vec![PortDef::new("Image", Image)],
            NodeType::Curve { .. } => vec![
                PortDef::new("Y", Normalized), PortDef::new("Phase", Normalized),
                PortDef::new("End", Trigger), PortDef::new("Image", Image),
                PortDef::new("Step Value", Normalized), PortDef::new("Step Trigger", Trigger),
            ],
            NodeType::Draw { .. } => vec![PortDef::new("Image", Image), PortDef::new("Points", Text)],
            NodeType::ColorCurves { .. } => vec![PortDef::new("Image", Image)],
            NodeType::VideoPlayer { .. } => vec![PortDef::new("Frame", Image), PortDef::new("Progress", Normalized)],
            NodeType::Camera { .. } => vec![PortDef::new("Frame", Image)],
            NodeType::MlModel { .. } => vec![PortDef::new("Annotated", Image), PortDef::new("Result", Text), PortDef::new("JSON", Text)],
            NodeType::Timer { .. } => vec![PortDef::new("Trigger", Trigger), PortDef::new("Phase", Normalized), PortDef::new("BPM", Number)],
            NodeType::SampleHold { .. } => vec![PortDef::new("Out", Generic), PortDef::new("Trigger", Trigger)],
            NodeType::NetworkSend { .. } => vec![],
            NodeType::NetworkReceive { imported_schema, .. } => {
                let mut ports: Vec<PortDef> = imported_schema.iter().map(|s| {
                    PortDef::dynamic(s.name.clone(), match s.kind {
                        NetPortKind::Float => Number,
                        NetPortKind::Text => Text,
                        NetPortKind::Image => Image,
                    })
                }).collect();
                ports.push(PortDef::new("Status", Text));
                ports
            }
            NodeType::Dynamic { inner } => inner.node.outputs(),
        }
    }

    fn color_hint(&self) -> [u8; 3] {
        match self {
            NodeType::Slider { .. } => [80, 160, 255],
            NodeType::FolderBrowser { .. } => [140, 160, 200],
            NodeType::WgslViewer { .. } => [220, 140, 60],
            NodeType::MidiOut { .. } => [60, 180, 180],
            NodeType::MidiIn { .. } => [80, 200, 160],
            NodeType::Theme { .. } => [255, 180, 80],
            NodeType::Serial { .. } => [200, 180, 60],
            NodeType::Script { .. } => [150, 100, 200],
            NodeType::Console { .. } => [100, 150, 100],
            NodeType::OscOut { .. } => [220, 120, 60],
            NodeType::OscIn { .. } => [60, 160, 220],
            NodeType::Palette { .. } => [120, 120, 180],
            NodeType::HttpRequest { .. } => [60, 180, 120],
            NodeType::AiRequest { .. } => [180, 100, 255],
            NodeType::FileMenu => [200, 200, 200],
            NodeType::ZoomControl { .. } => [160, 160, 160],
            NodeType::ObHub { .. } => [40, 180, 120],
            NodeType::ObJoystick { .. } => [80, 160, 255],
            NodeType::ObEncoder { .. } => [200, 140, 80],
            NodeType::ObMove { .. } => [100, 160, 200],
            NodeType::ObBend { .. } => [160, 200, 100],
            NodeType::ObPressure { .. } => [200, 100, 160],
            NodeType::ObDistance { .. } => [200, 200, 60],
            NodeType::ObKnob { .. } => [140, 200, 220],
            NodeType::ObOrb { .. } => [60, 200, 200],
            NodeType::Synth { .. } => [100, 220, 180],
            NodeType::AudioPlayer { .. } => [180, 100, 220],
            NodeType::AudioPlaylist { .. } => [140, 80, 200],
            NodeType::AudioInput { .. } => [220, 80, 120],
            NodeType::AudioAnalyzer => [255, 180, 60],
            NodeType::SpectrumAnalyzer => [255, 200, 90],
            NodeType::AudioDevice { .. } => [220, 180, 100],
            NodeType::AudioDelay { .. } => [180, 120, 200],
            NodeType::AudioDistortion { .. } => [220, 80, 80],
            NodeType::AudioPitchShift { .. } => [80, 200, 160],
            NodeType::AudioReverb { .. } => [120, 140, 220],
            NodeType::AudioLowPass { .. } => [100, 160, 200],
            NodeType::AudioHighPass { .. } => [200, 160, 100],
            NodeType::AudioGain { .. } => [160, 200, 100],
            NodeType::AudioEq { .. } => [200, 160, 255],
            NodeType::Speaker { .. } => [80, 200, 80],
            NodeType::AudioMixer { .. } => [160, 120, 220],
            NodeType::AudioSampler { .. } => [220, 120, 80],
            NodeType::ClapPlugin { .. } => [160, 80, 255],
            NodeType::RustPlugin { .. } => [255, 120, 50],
            NodeType::McpServer => [120, 200, 255],
            NodeType::HtmlViewer => [60, 180, 220],
            NodeType::ImageNode { .. } => [200, 140, 220],
            NodeType::ImageEffects { .. } => [180, 120, 200],
            NodeType::Curve { .. } => [100, 200, 160],
            NodeType::Draw { .. } => [200, 180, 100],
            NodeType::ColorCurves { .. } => [220, 140, 160],
            NodeType::VideoPlayer { .. } => [220, 80, 140],
            NodeType::Camera { .. } => [80, 200, 140],
            NodeType::MlModel { .. } => [200, 80, 255],
            NodeType::Timer { .. } => [100, 200, 180],
            NodeType::SampleHold { .. } => [120, 200, 160],
            NodeType::NetworkSend { .. } => [80, 200, 160],
            NodeType::NetworkReceive { .. } => [160, 80, 200],
            NodeType::Dynamic { inner } => inner.node.color_hint(),
        }
    }

    /// Whether this node renders its ports inline within the content
    /// instead of as separate lists at top/bottom.
    fn inline_ports(&self) -> bool {
        match self {
            NodeType::Dynamic { inner } => inner.node.inline_ports(),
            _ => matches!(self, NodeType::Theme { .. } | NodeType::MidiOut { .. } | NodeType::Synth { .. } | NodeType::WgslViewer { .. } | NodeType::ImageEffects { .. } | NodeType::Slider { .. } | NodeType::HttpRequest { .. } | NodeType::AiRequest { .. } | NodeType::AudioDelay { .. } | NodeType::AudioDistortion { .. } | NodeType::AudioLowPass { .. } | NodeType::AudioHighPass { .. } | NodeType::AudioGain { .. } | NodeType::AudioReverb { .. } | NodeType::AudioEq { .. } | NodeType::AudioPlayer { .. } | NodeType::AudioPlaylist { .. } | NodeType::Timer { .. } | NodeType::SampleHold { .. } | NodeType::Curve { .. } | NodeType::AudioMixer { .. } | NodeType::Speaker { .. } | NodeType::AudioInput { .. } | NodeType::AudioSampler { .. } | NodeType::ClapPlugin { .. } | NodeType::Console { .. } | NodeType::ObOrb { .. } | NodeType::AudioAnalyzer | NodeType::SpectrumAnalyzer),
        }
    }

    /// Whether this node skips the standard egui::Window and renders itself completely custom.
    fn custom_render(&self) -> bool {
        match self {
            NodeType::Dynamic { inner } => inner.node.custom_render(),
            _ => matches!(self, NodeType::Slider { .. }),
        }
    }

    fn no_title(&self) -> bool {
        match self {
            NodeType::Dynamic { inner } => inner.node.no_title(),
            _ => false,
        }
    }

    fn min_width(&self) -> Option<f32> {
        match self {
            NodeType::Dynamic { inner } => inner.node.min_width(),
            _ => None,
        }
    }

    fn render_background(&self, painter: &eframe::egui::Painter, rect: eframe::egui::Rect) -> Option<eframe::egui::Frame> {
        match self {
            NodeType::Dynamic { inner } => inner.node.render_background(painter, rect),
            _ => None,
        }
    }

    fn type_tag(&self) -> &str {
        // Legacy enum nodes use "builtin:{title}" as their type tag.
        // Migrated standalone structs use their own stable tag.
        "builtin"
    }

    /// Whether this node needs CPU pixel bytes on the given image input
    /// port. GPU-only consumers return false to opt into the
    /// `PortValue::GpuImage` zero-copy handoff path. CPU-only consumers
    /// (Crop CPU fallback, Camera, ML, etc.) return the default `true`,
    /// forcing the upstream producer to readback into CPU bytes.
    fn needs_cpu_image_input(&self, port: usize) -> bool {
        match self {
            // GPU-resident effect chain — never needs CPU bytes for image inputs
            NodeType::ImageEffects { .. } => false,
            NodeType::ColorCurves { .. } => false,
            // ImageNode display path uses the GPU display callback (renders
            // straight from gpu_tex_cache) so it doesn't need CPU pixels.
            // The save-to-disk handler reads back on demand via into_cpu_image.
            NodeType::ImageNode { .. } => false,
            NodeType::Dynamic { inner } => inner.node.needs_cpu_image_input(port),
            // Default-true for everything else — safest direction.
            _ => true,
        }
    }
}

// ── Node & Connection ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node { pub id: NodeId, pub node_type: NodeType, pub pos: [f32; 2] }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub from_node: NodeId, pub from_port: usize,
    pub to_node: NodeId, pub to_port: usize,
    /// Optional user label displayed on the wire
    #[serde(default)]
    pub label: String,
}

// ── Graph ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: HashMap<NodeId, Node>,
    pub connections: Vec<Connection>,
    next_id: u64,
    /// Last evaluate timestamp for computing real dt (not serialized)
    #[serde(skip)]
    last_eval_time: f64,
    /// Topologically sorted node evaluation order (rebuilt when graph topology changes)
    #[serde(skip)]
    topo_order: Vec<NodeId>,
    /// Nodes detected to be in dependency cycles (appended after acyclic nodes)
    #[serde(skip)]
    cyclic_nodes: Vec<NodeId>,
    /// True when nodes/connections changed and topo_order must be rebuilt before next eval
    /// Uses skip_serializing + default_true so it starts as true after deserialization
    #[serde(skip_serializing, default = "default_true")]
    topo_dirty: bool,
    /// True when audio topology changed (connection/node add/remove, Speaker toggle, Select switch).
    /// The audio chain compiler reads this and recompiles when true.
    #[serde(skip_serializing, default = "default_true")]
    pub audio_topology_dirty: bool,
    /// True when audio parameters changed (slider moved, knob turned).
    /// Only requires ParamStore update, not full chain recompilation.
    #[serde(skip)]
    #[allow(dead_code)] pub audio_params_dirty: bool,
}

impl Graph {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            connections: Vec::new(),
            next_id: 1,
            last_eval_time: 0.0,
            topo_order: Vec::new(),
            cyclic_nodes: Vec::new(),
            topo_dirty: true,
            audio_topology_dirty: true,
            audio_params_dirty: false,
        }
    }
    /// Get the PortKind for a specific port on a node.
    /// `is_output`: true for output ports, false for input ports.
    pub fn port_kind(&self, node_id: NodeId, port_idx: usize, is_output: bool) -> Option<PortKind> {
        let node = self.nodes.get(&node_id)?;
        let ports = if is_output { node.node_type.outputs() } else { node.node_type.inputs() };
        ports.get(port_idx).map(|p| p.kind)
    }

    /// Fix next_id after deserialization to prevent ID collisions.
    /// Sets next_id to one past the highest existing node ID.
    pub fn fix_next_id(&mut self) {
        self.next_id = self.nodes.keys().max().copied().unwrap_or(0) + 1;
    }

    pub fn add_node(&mut self, node_type: NodeType, pos: [f32; 2]) -> NodeId {
        let id = self.next_id; self.next_id += 1;
        crate::system_log::log(format!("Added {} (id:{})", node_type.title(), id));
        self.nodes.insert(id, Node { id, node_type, pos });
        self.topo_dirty = true;
        self.audio_topology_dirty = true;
        id
    }
    pub fn remove_node(&mut self, id: NodeId) {
        if let Some(node) = self.nodes.get(&id) {
            crate::system_log::log(format!("Removed {} (id:{})", node.node_type.title(), id));
        }
        self.nodes.remove(&id);
        self.connections.retain(|c| c.from_node != id && c.to_node != id);
        crate::node_errors::remove_for(id);
        self.topo_dirty = true;
        self.audio_topology_dirty = true;
    }
    pub fn remove_connections_to_port(&mut self, node_id: NodeId, port: usize) {
        self.connections.retain(|c| !(c.to_node == node_id && c.to_port == port));
        self.topo_dirty = true;
        self.audio_topology_dirty = true;
    }
    /// Mark topology caches as dirty. Call after manually mutating `connections`.
    pub fn mark_topo_dirty(&mut self) {
        self.topo_dirty = true;
        self.audio_topology_dirty = true;
    }
    pub fn add_connection(&mut self, from_node: NodeId, from_port: usize, to_node: NodeId, to_port: usize) {
        // ── Self-loop guard for WGSL Viewer image input ports ──
        // Self-feedback is expressed via the per-port "Last frame" dropdown,
        // not via a wire to the viewer's own output (refinement A in the
        // feedback plan). Reject such wires here so the UI can never reach an
        // ambiguous state.
        if from_node == to_node {
            if let Some(node) = self.nodes.get(&from_node) {
                if let NodeType::WgslViewer { .. } = &node.node_type {
                    // Resolve port name; if it matches one of the image inputs, drop.
                    let inputs = node.node_type.inputs();
                    if let Some(p) = inputs.get(to_port) {
                        if p.name == "Input A" || p.name == "Input B" {
                            crate::system_log::log(
                                "Self-loop on WGSL Viewer image input rejected. \
                                 Use the Last frame option in the Input dropdown instead."
                                    .to_string(),
                            );
                            return;
                        }
                    }
                }
            }
        }
        self.connections.retain(|c| !(c.to_node == to_node && c.to_port == to_port));
        self.connections.push(Connection { from_node, from_port, to_node, to_port, label: String::new() });
        self.topo_dirty = true;
        self.audio_topology_dirty = true;
    }

    /// Rebuild topological evaluation order using Kahn's algorithm.
    /// Called automatically before evaluate() when topo_dirty is set.
    ///
    /// Result: acyclic nodes in dependency order, then any cyclic nodes appended.
    /// For a stable graph this runs at most once between topology changes (add/remove node/wire).
    fn rebuild_topo_order(&mut self) {
        use std::collections::{VecDeque, HashSet};

        let node_ids: Vec<NodeId> = self.nodes.keys().copied().collect();

        // Build a unique set of dependency edges: (producer, consumer).
        // Multiple connections A→B on different ports count as a single edge.
        let mut edge_set: HashSet<(NodeId, NodeId)> = HashSet::new();
        for conn in &self.connections {
            if self.nodes.contains_key(&conn.from_node)
                && self.nodes.contains_key(&conn.to_node)
                && conn.from_node != conn.to_node   // ignore self-loops
            {
                edge_set.insert((conn.from_node, conn.to_node));
            }
        }

        // Build per-node in_degree and successor list from the unique edges.
        let mut in_degree: HashMap<NodeId, usize> = node_ids.iter().map(|&id| (id, 0)).collect();
        let mut successors: HashMap<NodeId, Vec<NodeId>> = node_ids.iter().map(|&id| (id, Vec::new())).collect();
        for &(from, to) in &edge_set {
            if let Some(d) = in_degree.get_mut(&to) { *d += 1; }
            if let Some(s) = successors.get_mut(&from) { s.push(to); }
        }

        // Seed with all zero-in-degree nodes, sorted for deterministic ordering.
        let mut zero: Vec<NodeId> = node_ids.iter().filter(|&&id| in_degree[&id] == 0).copied().collect();
        zero.sort_unstable();
        let mut queue: VecDeque<NodeId> = zero.into_iter().collect();

        let mut sorted: Vec<NodeId> = Vec::with_capacity(node_ids.len());
        let mut in_sorted: HashSet<NodeId> = HashSet::new();

        while let Some(nid) = queue.pop_front() {
            sorted.push(nid);
            in_sorted.insert(nid);
            // Clone successor list so we can mutably borrow in_degree simultaneously.
            let succs: Vec<NodeId> = successors[&nid].clone();
            let mut newly_zero: Vec<NodeId> = Vec::new();
            for succ in succs {
                if let Some(deg) = in_degree.get_mut(&succ) {
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 { newly_zero.push(succ); }
                }
            }
            newly_zero.sort_unstable();   // keep deterministic
            queue.extend(newly_zero);
        }

        // Any node not emitted by Kahn is in a cycle — append in sorted order.
        let mut cyclic: Vec<NodeId> = node_ids.iter()
            .filter(|&&id| !in_sorted.contains(&id))
            .copied()
            .collect();
        cyclic.sort_unstable();
        sorted.extend_from_slice(&cyclic);

        self.topo_order = sorted;
        self.cyclic_nodes = cyclic;
        self.topo_dirty = false;
    }

    /// Return the cached topological evaluation order, rebuilding it
    /// first if the graph topology has changed. Used by the image
    /// evaluation pass in `app/mod.rs` so deeper chains
    /// (Source → Blend → Effects → Blend → Effects → Display) propagate
    /// in a single pass instead of one frame per hop, and so iteration
    /// is deterministic across runs (HashMap order is hash-seed
    /// dependent and was producing visible flicker on reload).
    pub fn ensure_topo_order(&mut self) -> Vec<NodeId> {
        if self.topo_dirty || self.topo_order.is_empty() {
            self.rebuild_topo_order();
        }
        self.topo_order.clone()
    }

    /// Re-evaluate with pre-existing values (for injected hardware data).
    /// Evaluates in topological order so downstream nodes see fresh values in one pass.
    pub fn evaluate_with_existing(&mut self, values: &mut HashMap<(NodeId, usize), PortValue>, _now_secs: f64) {
        if self.topo_dirty || self.topo_order.is_empty() {
            self.rebuild_topo_order();
        }
        let eval_order = self.topo_order.clone();
        // Two passes: first follows topo order for acyclic chains; second catches any
        // remaining propagation for nodes that receive injected values indirectly.
        for _ in 0..2 {
            for &id in &eval_order {
                let inputs = self.collect_inputs(id, values);
                let node = match self.nodes.get_mut(&id) { Some(n) => n, None => continue };
                match &mut node.node_type {
                    NodeType::Slider { value, min, max, .. } => {
                        if let Some(PortValue::Float(v)) = inputs.get(1) { *min = *v; }
                        if let Some(PortValue::Float(v)) = inputs.get(2) { *max = *v; }
                        if let Some(PortValue::Float(v)) = inputs.first() { *value = *v; }
                        values.insert((id, 0), PortValue::Float(*value));
                    }
                    _ => {}
                }
            }
        }
    }

    /// Per-node evaluation kernel — contains all node logic.
    /// Extracted so that topo-ordered and cyclic-fallback passes share the same code.
    /// `continue` in the original match block is replaced by `return` here.
    ///
    /// Wraps the kernel in `catch_unwind` so a panicking node can't crash the
    /// whole app — the panic message is recorded against the node id via
    /// `crate::node_errors`, and the graph keeps evaluating the rest of the
    /// nodes. Successful evaluations clear any prior error for the node so
    /// the red badge disappears once it recovers.
    fn evaluate_node(
        id: NodeId,
        node_type: &mut NodeType,
        inputs: &[PortValue],
        values: &mut HashMap<(NodeId, usize), PortValue>,
        real_dt: f32,
        now_secs: f64,
    ) {
        crate::node_errors::clear(id);
        crate::node_errors::set_current_node(Some(id));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::evaluate_node_inner(id, node_type, inputs, values, real_dt, now_secs);
        }));
        crate::node_errors::set_current_node(None);
        if let Err(payload) = result {
            let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "panic during node evaluation".to_string()
            };
            crate::node_errors::report_for(id, format!("panic: {}", msg));
        }
    }

    /// Inner evaluation kernel — wrapped by `evaluate_node` for panic safety.
    fn evaluate_node_inner(
        id: NodeId,
        node_type: &mut NodeType,
        inputs: &[PortValue],
        values: &mut HashMap<(NodeId, usize), PortValue>,
        real_dt: f32,
        now_secs: f64,
    ) {
        match node_type {
                    NodeType::Slider { value, min, max, .. } => {
                        // Override min/max from inputs if connected
                        if let Some(PortValue::Float(v)) = inputs.get(1) { *min = *v; }
                        if let Some(PortValue::Float(v)) = inputs.get(2) { *max = *v; }
                        // Override value from input — also update the stored value so slider UI moves
                        if let Some(PortValue::Float(v)) = inputs.first() {
                            *value = *v;
                        }
                        values.insert((id, 0), PortValue::Float(*value));
                    }
                    NodeType::Timer { interval, elapsed, running, pulse_width,
                                       ref_time, paused_elapsed, time_initialized,
                                       phase_offset, last_sync_phase } => {
                        // Override interval from input port 0
                        if let Some(pv) = inputs.first() {
                            let v = pv.as_float();
                            if v > 0.0 { *interval = v; }
                        }
                        // Override interval from BPM input port 1
                        if let Some(pv) = inputs.get(1) {
                            let bpm_in = pv.as_float();
                            if bpm_in > 0.0 {
                                *interval = 60.0 / bpm_in;
                            }
                        }

                        // ── Sync mode: Phase In (port 2) ─────────────────
                        // When wired, Timer locks to the external phase and
                        // skips the wall-clock path entirely.
                        if let Some(PortValue::Float(ext_phase)) = inputs.get(2) {
                            let offset = match inputs.get(3) {
                                Some(PortValue::Float(v)) => *v,
                                _ => *phase_offset,
                            };
                            let shifted = (ext_phase + offset).fract();
                            let shifted = if shifted < 0.0 { shifted + 1.0 } else { shifted };
                            let trigger = if shifted < *last_sync_phase
                                             && (*last_sync_phase - shifted) > 0.5
                                             && *running
                            { 1.0 } else { 0.0 };
                            *last_sync_phase = shifted;
                            let bpm = 60.0 / interval.max(0.01);
                            values.insert((id, 0), PortValue::Float(trigger));
                            values.insert((id, 1), PortValue::Float(shifted));
                            values.insert((id, 2), PortValue::Float(bpm));
                            return; // skip wall-clock path
                        }

                        // ── Free-running wall-clock timing ────────────────
                        if !*time_initialized {
                            if *running {
                                *ref_time = now_secs;
                                *paused_elapsed = *elapsed as f64;
                            }
                            *time_initialized = true;
                        }

                        if *running {
                            *elapsed = ((now_secs - *ref_time) + *paused_elapsed) as f32;
                        }

                        let safe_interval = interval.max(0.01);
                        let phase = (*elapsed % safe_interval) / safe_interval;
                        let is_pulse = phase < (*pulse_width / safe_interval);
                        let trigger = if is_pulse && *running { 1.0 } else { 0.0 };
                        let bpm = 60.0 / safe_interval;
                        values.insert((id, 0), PortValue::Float(trigger));
                        values.insert((id, 1), PortValue::Float(phase));
                        values.insert((id, 2), PortValue::Float(bpm));
                    }
                    NodeType::SampleHold { held_float, held_text, is_text, last_trigger, history } => {
                        // Input 0 = Value, Input 1 = Trigger
                        let trigger_val = inputs.get(1).map(|v| v.as_float()).unwrap_or(0.0);
                        let rising_edge = trigger_val > 0.5 && *last_trigger <= 0.5;
                        *last_trigger = trigger_val;

                        if rising_edge {
                            if let Some(val) = inputs.first() {
                                match val {
                                    PortValue::Float(f) => {
                                        *held_float = *f;
                                        *is_text = false;
                                        history.push(*f);
                                        while history.len() > 40 { history.remove(0); }
                                    }
                                    PortValue::Text(t) => {
                                        *held_text = t.clone();
                                        *is_text = true;
                                    }
                                    _ => {}
                                }
                            }
                        }

                        // Output held value
                        if *is_text {
                            values.insert((id, 0), PortValue::Text(held_text.clone()));
                        } else {
                            values.insert((id, 0), PortValue::Float(*held_float));
                        }
                        // Trigger echo: 1.0 on rising edge frame, else 0.0
                        values.insert((id, 1), PortValue::Float(if rising_edge { 1.0 } else { 0.0 }));
                    }
                    NodeType::Curve {
                        points, mode, speed, looping, manual_gate, sustain_index,
                        phase, playing, last_trigger, last_phase,
                        last_step_index, sustain_held,
                    } => {
                        // ── Backward-compat migration: legacy LFO mode ──
                        if *mode == 2 {
                            *mode = 1;
                            *looping = true;
                        }
                        // Sustain index invariant: clear if it points past the
                        // end (e.g. user removed the marked point).
                        if let Some(idx) = *sustain_index {
                            if idx as usize >= points.len() { *sustain_index = None; }
                        }

                        // Override speed from input port 2
                        if let Some(pv) = inputs.get(2) {
                            let v = pv.as_float();
                            if v > 0.0 { *speed = v; }
                        }
                        // Gate input (port 3) — falls back to manual_gate checkbox
                        // when nothing is wired. The renderer hides the checkbox
                        // while the port is wired so the manual value is dormant.
                        let gate_input_high = inputs.get(3).map(|v| v.as_float() > 0.5).unwrap_or(false);
                        let gate_high = gate_input_high || *manual_gate;

                        let prev_phase = *last_phase;

                        let x_pos = match *mode {
                            0 => {
                                // ── Lookup mode: X input drives Y, no playback ──
                                *playing = false;
                                *sustain_held = false;
                                inputs.first().map(|v| v.as_float()).unwrap_or(0.0).clamp(0.0, 1.0)
                            }
                            _ => {
                                // ── Played mode: trigger-driven playback ──
                                let trig_val = inputs.get(1).map(|v| v.as_float()).unwrap_or(0.0);
                                let rising = trig_val > 0.5 && *last_trigger <= 0.5;
                                *last_trigger = trig_val;

                                if rising {
                                    *phase = 0.0;
                                    *playing = true;
                                    *sustain_held = false;
                                    *last_phase = -1.0;     // re-fire crossings from x=0
                                    *last_step_index = -1;
                                }

                                if *playing {
                                    // Sustain logic: clamp at the marked point
                                    // while gate is held high.
                                    if let Some(idx) = *sustain_index {
                                        let sx = points[idx as usize][0];
                                        if *sustain_held {
                                            if !gate_high { *sustain_held = false; }
                                        } else if gate_high
                                            && *phase < sx
                                            && *phase + real_dt * *speed >= sx
                                        {
                                            *phase = sx;
                                            *sustain_held = true;
                                        }
                                    }

                                    if !*sustain_held {
                                        *phase += real_dt * *speed;
                                        if *phase >= 1.0 {
                                            if *looping {
                                                *phase %= 1.0;
                                                // Wrap: reset crossing tracker so the
                                                // first points of the new cycle re-fire.
                                                *last_phase = -1.0;
                                                *last_step_index = -1;
                                            } else {
                                                *phase = 1.0;
                                                *playing = false;
                                            }
                                        }
                                    }
                                }
                                *phase
                            }
                        };

                        let y_val = crate::nodes::curve::evaluate_curve(points, x_pos);

                        // ── Step crossing detection (Played mode only) ──
                        let mut step_trigger = 0.0f32;
                        if *mode != 0 && *playing {
                            // Note: we use the local prev_phase (not *last_phase)
                            // because *last_phase was reset to -1.0 above on
                            // trigger / wrap to force the new-cycle re-fire.
                            let scan_prev = if *last_phase < 0.0 { -1.0 } else { prev_phase };
                            for (i, pt) in points.iter().enumerate() {
                                let px = pt[0];
                                if px > scan_prev && px <= x_pos {
                                    step_trigger = 1.0;
                                    *last_step_index = i as i8;
                                }
                            }
                        }
                        let step_value = if *mode != 0 && *last_step_index >= 0 {
                            points.get(*last_step_index as usize).map(|p| p[1]).unwrap_or(0.0)
                        } else { 0.0 };
                        *last_phase = x_pos;

                        let at_end = !*playing && *phase >= 1.0 && *mode != 0;

                        values.insert((id, 0), PortValue::Float(y_val));
                        values.insert((id, 1), PortValue::Float(x_pos));
                        values.insert((id, 2), PortValue::Float(if at_end { 1.0 } else { 0.0 }));
                        // (id, 3) Image handled in app.rs image pass
                        values.insert((id, 4), PortValue::Float(step_value));
                        values.insert((id, 5), PortValue::Float(step_trigger));
                    }
                    NodeType::FolderBrowser { selected_file, cached_content, cached_content_path, .. } => {
                        // Output the selected file path and name
                        values.insert((id, 0), PortValue::Text(selected_file.clone()));
                        let name = std::path::Path::new(selected_file.as_str())
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        values.insert((id, 1), PortValue::Text(name));
                        // Read content, cached by selected_file path.
                        // Previously re-read from disk every frame — pure waste
                        // of syscalls + CPU for the 99.99% of frames where the
                        // selection hasn't changed.
                        if !selected_file.is_empty() {
                            if cached_content_path != selected_file {
                                *cached_content = std::fs::read_to_string(selected_file.as_str()).unwrap_or_default();
                                *cached_content_path = selected_file.clone();
                            }
                            values.insert((id, 2), PortValue::Text(cached_content.clone()));
                        } else {
                            cached_content.clear();
                            cached_content_path.clear();
                        }
                    }
                    NodeType::MidiIn { channel, note, velocity, .. } => {
                        values.insert((id, 0), PortValue::Float(*channel as f32));
                        values.insert((id, 1), PortValue::Float(*note as f32));
                        values.insert((id, 2), PortValue::Float(*velocity as f32));
                    }
                    NodeType::Theme { bg_color, text_color, accent, .. } => {
                        values.insert((id, 0), PortValue::Float(bg_color[0] as f32));
                        values.insert((id, 1), PortValue::Float(bg_color[1] as f32));
                        values.insert((id, 2), PortValue::Float(bg_color[2] as f32));
                        values.insert((id, 3), PortValue::Float(text_color[0] as f32));
                        values.insert((id, 4), PortValue::Float(text_color[1] as f32));
                        values.insert((id, 5), PortValue::Float(text_color[2] as f32));
                        values.insert((id, 6), PortValue::Float(accent[0] as f32));
                        values.insert((id, 7), PortValue::Float(accent[1] as f32));
                        values.insert((id, 8), PortValue::Float(accent[2] as f32));
                    }
                    NodeType::Serial { last_line, .. } => {
                        values.insert((id, 0), PortValue::Text(last_line.clone()));
                    }
                    NodeType::Script { input_names, output_names, code, last_values, error, continuous, trigger, .. } => {
                        // Code port: if Code input is connected, use that; else use inline code
                        // Port layout: [Exec? (if manual)] [Code] [user inputs...]
                        let code_port_idx: usize = if *continuous { 0 } else { 1 };
                        let code_from_port = match inputs.get(code_port_idx) {
                            Some(PortValue::Text(s)) if !s.is_empty() => Some(s.clone()),
                            _ => None,
                        };
                        let effective_code = code_from_port.as_deref().unwrap_or(code.as_str());

                        if effective_code.is_empty() || output_names.is_empty() {
                            for (i, v) in last_values.iter().enumerate() {
                                values.insert((id, i), PortValue::Float(*v));
                            }
                            return; // emit last values and skip eval for this node
                        }

                        // In manual mode, only run if triggered
                        let should_run = if *continuous {
                            true
                        } else {
                            let exec_val = inputs.first().map(|v| v.as_float()).unwrap_or(0.0);
                            let fired = exec_val > 0.5 || *trigger;
                            *trigger = false;
                            fired
                        };

                        if !should_run {
                            for (i, v) in last_values.iter().enumerate() {
                                values.insert((id, i), PortValue::Float(*v));
                            }
                            return; // emit last values and skip eval for this node
                        }

                        let engine = rhai::Engine::new();
                        // User inputs start after [Exec?] [Code] ports
                        let input_offset: usize = code_port_idx + 1;
                        let in_vars: Vec<String> = input_names.iter().enumerate().map(|(i, name)| {
                            let val = inputs.get(i + input_offset).map(|v| v.as_float()).unwrap_or(0.0);
                            format!("let {} = {};", name, val)
                        }).collect();
                        // Declare output variables initialized to 0.0
                        let out_vars: Vec<String> = output_names.iter()
                            .map(|name| format!("let {} = 0.0;", name))
                            .collect();
                        // After user code, collect output variables into array
                        let collect_outputs = format!("[{}]",
                            output_names.join(", ")
                        );
                        let full_script = format!(
                            "{}\n{}\n{}\n{}",
                            in_vars.join("\n"),
                            out_vars.join("\n"),
                            effective_code,
                            collect_outputs
                        );
                        match engine.eval::<rhai::Array>(&full_script) {
                            Ok(arr) => {
                                error.clear();
                                last_values.clear();
                                for (i, val) in arr.iter().enumerate() {
                                    if i < output_names.len() {
                                        let f = val.as_float().unwrap_or(0.0) as f32;
                                        values.insert((id, i), PortValue::Float(f));
                                        last_values.push(f);
                                    }
                                }
                            }
                            Err(e) => {
                                *error = e.to_string();
                            }
                        }
                    }
                    NodeType::HttpRequest { response, status, .. } => {
                        values.insert((id, 0), PortValue::Text(response.clone()));
                        // Parse status code from status string (e.g., "200 OK" → 200.0)
                        let code = status.split_whitespace().next()
                            .and_then(|s| s.parse::<f32>().ok())
                            .unwrap_or(0.0);
                        values.insert((id, 1), PortValue::Float(code));
                    }
                    NodeType::AiRequest { response, status, mode: _, .. } => {
                        // v1: always emit URL/text on Response port. v2 will
                        // swap the Image-mode arm to PortValue::Image once
                        // an auto-fetch path exists.
                        values.insert((id, 0), PortValue::Text(response.clone()));
                        let code = if status.contains("done") { 1.0 }
                            else if status.contains("error") { -1.0 }
                            else if status.contains("thinking") { 0.5 }
                            else { 0.0 };
                        values.insert((id, 1), PortValue::Float(code));
                    }
                    NodeType::Console { messages } => {
                        // Log input value as a timestamped message
                        if let Some(val) = inputs.first() {
                            let text = format!("{}", val);
                            if !text.is_empty() && text != "—" {
                                // Only log if value changed (avoid flooding)
                                let is_new = messages.last().map(|last| {
                                    // Strip timestamp prefix for comparison
                                    let last_msg = last.splitn(2, "] ").nth(1).unwrap_or(last);
                                    last_msg != text
                                }).unwrap_or(true);
                                if is_new {
                                    let secs = now_secs as u32;
                                    let mins = secs / 60;
                                    let s = secs % 60;
                                    messages.push(format!("[{:02}:{:02}] {}", mins, s, text));
                                    // Cap at 200 messages
                                    if messages.len() > 200 {
                                        messages.drain(..messages.len() - 200);
                                    }
                                }
                            }
                        }
                    }
                    NodeType::OscOut { .. } => {}
                    NodeType::OscIn { last_args, last_args_text, arg_count, address_filter, .. } => {
                        for i in 0..*arg_count {
                            let v = last_args.get(i).copied().unwrap_or(0.0);
                            values.insert((id, i), PortValue::Float(v));
                        }
                        // Raw text output: all args joined
                        let raw = last_args_text.join(", ");
                        values.insert((id, *arg_count), PortValue::Text(raw));
                        // Address output
                        values.insert((id, *arg_count + 1), PortValue::Text(address_filter.clone()));
                    }
                    NodeType::NetworkSend { .. } => {}
                    NodeType::NetworkReceive { imported_schema, last_values, last_values_text, status, .. } => {
                        for (i, s) in imported_schema.iter().enumerate() {
                            match s.kind {
                                NetPortKind::Float => {
                                    let v = last_values.get(i).copied().unwrap_or(0.0);
                                    values.insert((id, i), PortValue::Float(v));
                                }
                                NetPortKind::Text => {
                                    let t = last_values_text.get(i).cloned().unwrap_or_default();
                                    values.insert((id, i), PortValue::Text(t));
                                }
                                NetPortKind::Image => {
                                    // Image data is handled via last_values_text as base64
                                    // For now, output as text
                                    let t = last_values_text.get(i).cloned().unwrap_or_default();
                                    values.insert((id, i), PortValue::Text(t));
                                }
                            }
                        }
                        // Status output (last port)
                        values.insert((id, imported_schema.len()), PortValue::Text(status.clone()));
                    }
                    NodeType::Dynamic { inner } => {
                        let results = inner.node.evaluate(inputs);
                        for (port, value) in results {
                            values.insert((id, port), value);
                        }
                    }
                    _ => {}
                }
    }

    /// Evaluate the graph. `now_secs` is the monotonic wall-clock time in seconds
    /// (from `std::time::Instant` elapsed since app start). Used by Timer for drift-free tempo.
    pub fn evaluate(&mut self, now_secs: f64) -> HashMap<(NodeId, usize), PortValue> {
        // Compute real frame dt from wall clock (clamped to avoid huge jumps).
        let real_dt = if self.last_eval_time > 0.0 {
            ((now_secs - self.last_eval_time) as f32).clamp(0.0001, 0.25)
        } else {
            1.0 / 60.0  // first-frame fallback
        };
        self.last_eval_time = now_secs;

        // Rebuild topo order only when graph topology has changed.
        if self.topo_dirty || self.topo_order.is_empty() {
            self.rebuild_topo_order();
        }
        // Clone ID vecs into locals so we can mutably borrow self.nodes inside the loop.
        let eval_order = self.topo_order.clone();
        let cyclic_ids = self.cyclic_nodes.clone();

        let mut values: HashMap<(NodeId, usize), PortValue> = HashMap::new();

        // ── Single topo-ordered pass ─────────────────────────────────────────
        // All predecessors of any node are evaluated before it, so one pass is
        // correct and O(n) for any acyclic graph.
        for &id in &eval_order {
            let inputs = self.collect_inputs(id, &values);
            if let Some(node) = self.nodes.get_mut(&id) {
                Self::evaluate_node(id, &mut node.node_type, &inputs, &mut values, real_dt, now_secs);
            }
        }

        // ── Extra passes for cyclic nodes (feedback loops) ───────────────────
        // Kahn's algorithm could not linearize these nodes. Two extra passes let
        // values propagate around short cycles without running the full graph again.
        if !cyclic_ids.is_empty() {
            for _ in 0..2 {
                for &id in &cyclic_ids {
                    let inputs = self.collect_inputs(id, &values);
                    if let Some(node) = self.nodes.get_mut(&id) {
                        Self::evaluate_node(id, &mut node.node_type, &inputs, &mut values, real_dt, now_secs);
                    }
                }
            }
        }

        values
    }

    pub fn collect_inputs(&self, node_id: NodeId, values: &HashMap<(NodeId, usize), PortValue>) -> Vec<PortValue> {
        let num = self.nodes.get(&node_id).map(|n| n.node_type.inputs().len()).unwrap_or(0);
        let mut inputs = vec![PortValue::None; num];
        for conn in &self.connections {
            if conn.to_node == node_id && conn.to_port < num {
                if let Some(val) = values.get(&(conn.from_node, conn.from_port)) {
                    inputs[conn.to_port] = val.clone();
                }
            }
        }
        inputs
    }

    /// Returns the (producer_node_id, producer_port) source for each input port
    /// of `node_id`, or None for unconnected ports. Mirrors `collect_inputs`
    /// but returns identities rather than values, for GPU texture cache lookup.
    pub fn collect_input_sources(&self, node_id: NodeId) -> Vec<Option<(NodeId, usize)>> {
        let num = self.nodes.get(&node_id).map(|n| n.node_type.inputs().len()).unwrap_or(0);
        let mut sources = vec![None; num];
        for conn in &self.connections {
            if conn.to_node == node_id && conn.to_port < num {
                sources[conn.to_port] = Some((conn.from_node, conn.from_port));
            }
        }
        sources
    }

    /// Returns true if any node connected downstream of
    /// `(producer_id, producer_port)` requires CPU pixel bytes on its
    /// receiving port. GPU producers query this to decide whether to
    /// readback their output texture (`PortValue::Image`) or emit a
    /// zero-copy `PortValue::GpuImage` handle.
    ///
    /// Kept for ad-hoc / one-shot queries (e.g. from node-local render
    /// paths that don't have the per-frame cache in scope). The image-
    /// evaluation loop in `app/mod.rs` uses `cpu_consumer_producers`
    /// instead, which is O(edges) per frame regardless of call count.
    #[allow(dead_code)]
    pub fn has_cpu_consumer(&self, producer_id: NodeId, producer_port: usize) -> bool {
        for conn in &self.connections {
            if conn.from_node == producer_id && conn.from_port == producer_port {
                if let Some(consumer) = self.nodes.get(&conn.to_node) {
                    if consumer.node_type.needs_cpu_image_input(conn.to_port) {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn static_input_value(connections: &[Connection], values: &HashMap<(NodeId, usize), PortValue>, node_id: NodeId, port_idx: usize) -> PortValue {
        for c in connections {
            if c.to_node == node_id && c.to_port == port_idx {
                return values.get(&(c.from_node, c.from_port)).cloned().unwrap_or(PortValue::None);
            }
        }
        PortValue::None
    }
}

/// Walk a JSON value using dot-separated path (e.g., "choices.0.message.content")
pub fn extract_json_path_pub(json_str: &str, path: &str) -> String {
    extract_json_path(json_str, path)
}
fn extract_json_path(json_str: &str, path: &str) -> String {
    let Ok(mut val) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return format!("(parse error)");
    };
    for segment in path.split('.') {
        val = if let Ok(idx) = segment.parse::<usize>() {
            val.get(idx).cloned().unwrap_or(serde_json::Value::Null)
        } else {
            val.get(segment).cloned().unwrap_or(serde_json::Value::Null)
        };
    }
    match &val {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}
