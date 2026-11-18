//! P4.3.1B: capability scanner for marketplace artifacts.
//!
//! Runs on the raw JSON of every `.pwnode` in an artifact (the entry and
//! every bundled dependency) **before** anything is deserialized into node
//! objects. This matters: a normal load silently replaces an unknown
//! dynamic node with an `AddNode`, and constructs nodes through registry
//! factories.
//!
//! Classification is an explicit allowlist and fails closed: a node type
//! (enum variant or dynamic `type_tag`) that is not listed here is refused.
//! Unit tests keep the tables in sync with `NodeType` and the node
//! registry, so a new node type cannot be installed from the marketplace
//! until someone classifies it.
//!
//! The classification is conservative: when a node *may* touch files, the
//! network, devices, capture or code, it counts as using that capability.

use std::collections::BTreeSet;

use serde_json::Value;

use super::Capability;
use Capability::{
    AiCredentials as AIC, Devices as DEV, Filesystem as FS, MediaCapture as CAP, Network as NET,
    Process as PROC, Scripting as SCR, SystemInfo as SYS,
};

/// P4.3.2B: version of the node classification below. Bump it whenever a
/// node type is (re)classified, so decisions recorded against an older
/// classification (future trust records) are reconsidered.
///
/// * 1 — P4.3.1B: initial name-based classification.
/// * 2 — P4.3.2B: source-audited; adds `process`, `system_info` and
///   `ai_credentials`.
pub const CLASSIFICATION_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeClass {
    /// No privileged capability.
    Pure,
    Uses(&'static [Capability]),
    /// Never installable from the marketplace.
    Forbidden(&'static str),
}

use NodeClass::{Forbidden, Pure, Uses};

const NATIVE: &str = "loads native code";

/// Every `NodeType` variant (kept in sync with graph.rs by a test).
/// `Dynamic` is classified by its `type_tag` instead.
pub const VARIANTS: &[(&str, NodeClass)] = &[
    ("CustomInput", Pure),
    ("CustomOutput", Pure),
    // Its graph is another .pwnode in the artifact, scanned separately.
    ("CustomNodeInstance", Pure),
    ("Slider", Pure),
    ("FolderBrowser", Uses(&[FS])),
    ("WgslViewer", Uses(&[SCR])),
    ("MidiOut", Uses(&[DEV])),
    ("MidiIn", Uses(&[DEV])),
    ("Theme", Pure),
    ("Serial", Uses(&[DEV])),
    ("Script", Uses(&[SCR])),
    ("Console", Pure),
    ("OscOut", Uses(&[NET])),
    ("OscIn", Uses(&[NET])),
    ("DmxOut", Uses(&[DEV, NET])),
    ("DmxIn", Uses(&[DEV, NET])),
    ("ArtNetOut", Uses(&[NET])),
    ("ArtNetIn", Uses(&[NET])),
    ("Palette", Pure),
    ("HttpRequest", Uses(&[NET])),
    ("AiRequest", Uses(&[NET, AIC, FS])),
    ("FileMenu", Uses(&[FS])),
    ("ZoomControl", Pure),
    ("ObHub", Uses(&[DEV, NET])),
    ("ObJoystick", Uses(&[DEV])),
    ("ObEncoder", Uses(&[DEV])),
    ("ObMove", Uses(&[DEV])),
    ("ObBend", Uses(&[DEV])),
    ("ObPressure", Uses(&[DEV])),
    ("ObDistance", Uses(&[DEV])),
    ("ObKnob", Uses(&[DEV])),
    ("ObLight", Uses(&[DEV])),
    ("ObMotor", Uses(&[DEV])),
    ("ObOrb", Uses(&[DEV])),
    ("Synth", Pure),
    ("AudioPlayer", Uses(&[FS])),
    ("AudioPlaylist", Uses(&[FS])),
    ("AudioInput", Uses(&[CAP, PROC])),
    ("AudioAnalyzer", Uses(&[CAP])),
    ("SpectrumAnalyzer", Uses(&[CAP])),
    ("AudioDevice", Uses(&[DEV])),
    ("AudioDelay", Pure),
    ("AudioDistortion", Pure),
    ("AudioPitchShift", Pure),
    ("AudioLowPass", Pure),
    ("AudioHighPass", Pure),
    ("AudioGain", Pure),
    ("AudioReverb", Pure),
    ("AudioEq", Pure),
    ("Speaker", Pure),
    ("AudioMixer", Pure),
    ("AudioSampler", Uses(&[FS])),
    ("ClapPlugin", Forbidden(NATIVE)),
    ("RustPlugin", Forbidden(NATIVE)),
    (
        "McpServer",
        Forbidden("lets external tools control Patchwork"),
    ),
    ("HtmlViewer", Uses(&[NET, SCR])),
    ("ImageNode", Uses(&[FS, NET])),
    ("ImageEffects", Pure),
    ("Curve", Pure),
    ("Draw", Pure),
    ("ColorCurves", Pure),
    ("VideoPlayer", Uses(&[FS, NET, PROC])),
    ("Camera", Uses(&[CAP, PROC])),
    ("MlModel", Uses(&[FS])),
    ("Timer", Pure),
    ("SampleHold", Pure),
    ("NetworkSend", Uses(&[NET])),
    ("NetworkReceive", Uses(&[NET])),
];

/// Every registered dynamic node `type_tag` (kept in sync with the node
/// registry by a test).
pub const DYNAMIC_TAGS: &[(&str, NodeClass)] = &[
    ("add", Pure),
    ("multiply", Pure),
    ("time", Pure),
    ("noise", Pure),
    ("display", Pure),
    ("color", Pure),
    ("comment", Pure),
    ("timer", Pure),
    ("gate", Pure),
    ("isChanged", Pure),
    ("sample_hold", Pure),
    ("key_input", Uses(&[DEV])),
    ("button", Pure),
    ("pack", Pure),
    ("mouse_tracker", Uses(&[DEV])),
    ("map_range", Pure),
    ("string_format", Pure),
    ("json_extract", Pure),
    ("text_editor", Pure),
    ("draw", Pure),
    ("visual_output", Pure),
    ("file", Uses(&[FS])),
    ("file_menu", Uses(&[FS])),
    ("folder_browser", Uses(&[FS])),
    ("html_viewer", Uses(&[NET, SCR])),
    ("console", Pure),
    ("monitor", Uses(&[SYS, PROC])),
    ("zoom_control", Pure),
    ("midi_note", Uses(&[DEV])),
    ("crop", Pure),
    ("transform", Pure),
    ("image_style", Pure),
    ("kaleidoscope", Pure),
    ("color_channel", Pure),
    ("settings", Forbidden("changes Patchwork settings")),
    ("wgsl_presets", Uses(&[SCR])),
    // Registered by the WGSL presets with the same shader factory.
    ("formula_preset_picker", Uses(&[SCR])),
    ("route", Pure),
    ("switch", Pure),
    ("tts", Uses(&[FS, PROC])),
    ("terminal", Forbidden("runs system commands")),
    ("text_ops", Pure),
    ("printer", Uses(&[DEV, FS, PROC])),
    ("keypoint_extract", Pure),
    ("smoother", Pure),
    ("point_2d", Pure),
    ("fill", Pure),
    ("text_to_image", Uses(&[FS])),
    ("image_scanner", Uses(&[FS, DEV])),
    ("frame_recorder", Uses(&[FS])),
    ("noise_removal", Uses(&[FS])),
    ("voice_effects", Pure),
    ("audio_analyzer", Uses(&[CAP])),
    ("music_visualizer", Pure),
    ("spectral_synth", Pure),
    ("neural_audio", Uses(&[FS])),
    ("web_app", Uses(&[NET, SCR, PROC])),
    ("web_stream", Uses(&[NET, PROC])),
    ("websocket", Uses(&[NET])),
    ("clock", Pure),
    ("hand_detection", Uses(&[FS])),
    ("face_detection", Uses(&[FS])),
    ("pose_detection", Uses(&[FS])),
    ("pose_match", Pure),
    ("regress", Pure),
    ("json_fields", Pure),
    ("math", Pure),
    ("blend", Pure),
    ("select", Pure),
    (
        "ai_config",
        Forbidden("reads stored AI service credentials"),
    ),
    ("text_gen", Uses(&[NET, AIC])),
    ("image_gen", Uses(&[NET, AIC, FS])),
    ("code_gen", Uses(&[NET, AIC, SCR])),
    ("video_in", Uses(&[CAP, NET, FS, PROC])),
    ("video_out", Uses(&[DEV, FS, NET, PROC])),
    ("shape_3d", Uses(&[FS])),
    ("material", Pure),
    ("render_3d", Pure),
];

fn lookup(table: &[(&str, NodeClass)], name: &str) -> Option<NodeClass> {
    table.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

/// Classify one serialized `node_type` value.
fn classify(node_type: &Value) -> Result<(String, NodeClass), String> {
    let (variant, body) = match node_type {
        Value::String(name) => (name.as_str(), None),
        Value::Object(map) if map.len() == 1 => {
            let (name, body) = map.iter().next().expect("one entry");
            (name.as_str(), Some(body))
        }
        _ => return Err("a node has an unreadable type".to_string()),
    };
    if variant == "Dynamic" {
        let tag = body
            .and_then(|b| b.get("inner"))
            .and_then(|i| i.get("type_tag"))
            .and_then(Value::as_str)
            .ok_or_else(|| "a dynamic node has no type_tag (unclassifiable)".to_string())?;
        let class =
            lookup(DYNAMIC_TAGS, tag).ok_or_else(|| format!("unsupported node type '{tag}'"))?;
        return Ok((tag.to_string(), class));
    }
    let class =
        lookup(VARIANTS, variant).ok_or_else(|| format!("unsupported node type '{variant}'"))?;
    Ok((variant.to_string(), class))
}

/// Capabilities used by one `.pwnode` (its graph's nodes). Refuses
/// forbidden, unknown and unreadable node types.
pub fn scan_pwnode(path: &str, text: &str) -> Result<BTreeSet<Capability>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("{path} is not valid JSON: {e}"))?;
    let nodes = value
        .get("graph")
        .and_then(|g| g.get("nodes"))
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{path} has no node graph"))?;
    let mut found = BTreeSet::new();
    for node in nodes.values() {
        let node_type = node
            .get("node_type")
            .ok_or_else(|| format!("{path}: a node has no type"))?;
        let (name, class) = classify(node_type).map_err(|e| format!("{path}: {e}"))?;
        match class {
            Pure => {}
            Uses(capabilities) => found.extend(capabilities.iter().copied()),
            Forbidden(why) => {
                return Err(format!(
                    "{path}: node type '{name}' {why} and cannot be installed from the marketplace"
                ));
            }
        }
    }
    Ok(found)
}

/// Capabilities used by all `.pwnode` files of an artifact.
pub fn scan_files<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<BTreeSet<Capability>, String> {
    let mut found = BTreeSet::new();
    for (path, text) in files {
        if path.ends_with(".pwnode") {
            found.extend(scan_pwnode(path, text)?);
        }
    }
    Ok(found)
}

/// P4.3.2C: what the graphs of a local package use. Unlike
/// [`scan_files`] it never refuses: node types that the marketplace forbids
/// or that are not classified are listed, so a trust record can describe
/// packages published or kept on this computer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory {
    pub capabilities: BTreeSet<Capability>,
    /// Node types never allowed from the marketplace (native code, …).
    pub restricted: BTreeSet<String>,
    /// Node types this classification does not know.
    pub unclassified: BTreeSet<String>,
}

pub fn inventory_files<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<Inventory, String> {
    let mut inventory = Inventory::default();
    for (path, text) in files {
        if !path.ends_with(".pwnode") {
            continue;
        }
        let value: Value =
            serde_json::from_str(text).map_err(|e| format!("{path} is not valid JSON: {e}"))?;
        let nodes = value
            .get("graph")
            .and_then(|g| g.get("nodes"))
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{path} has no node graph"))?;
        for node in nodes.values() {
            let Some(node_type) = node.get("node_type") else {
                inventory.unclassified.insert("(no type)".to_string());
                continue;
            };
            match classify(node_type) {
                Ok((_, Pure)) => {}
                Ok((_, Uses(capabilities))) => {
                    inventory.capabilities.extend(capabilities.iter().copied())
                }
                Ok((name, Forbidden(_))) => {
                    inventory.restricted.insert(name);
                }
                Err(_) => {
                    inventory.unclassified.insert(unclassified_name(node_type));
                }
            }
        }
    }
    Ok(inventory)
}

// ── P4.3.2E2: what a graph may exercise when it runs ─────────────────────

/// Capabilities exercised at run time by node types the marketplace never
/// installs (`Forbidden`), which local projects may still contain. Kept in
/// sync with the `Forbidden` entries above by a test.
pub const RESTRICTED_RUNTIME: &[(&str, &[Capability])] = &[
    ("ClapPlugin", &[Capability::NativeCode, FS]),
    ("RustPlugin", &[Capability::NativeCode, PROC, FS]),
    // Shows the log of the editor's own MCP service (stdio); MCP edits are
    // handled by the editor (see `app::provenance::after_mcp_edit`).
    ("McpServer", &[]),
    ("settings", &[FS]),
    ("terminal", &[PROC]),
    ("ai_config", &[AIC, FS]),
];

fn restricted_capabilities(name: &str) -> crate::capability::CapSet {
    RESTRICTED_RUNTIME
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, caps)| crate::capability::CapSet::of(caps))
        // Not listed: treated as able to do anything.
        .unwrap_or(crate::capability::CapSet::ALL)
}

/// The capabilities restricted node types (by name) exercise.
pub fn restricted_runtime_capabilities<'a>(
    names: impl IntoIterator<Item = &'a String>,
) -> crate::capability::CapSet {
    names
        .into_iter()
        .fold(crate::capability::CapSet::EMPTY, |set, name| {
            set.union(restricted_capabilities(name))
        })
}

/// What the nodes of one graph value (`{"nodes": …}`) may exercise when it
/// runs, including restricted node types. Nested custom nodes are not
/// included: each is authorized on its own. Node types this
/// classification does not know load as inert placeholders and add
/// nothing.
pub fn runtime_capabilities_of_graph(graph: &Value) -> crate::capability::CapSet {
    let mut set = crate::capability::CapSet::EMPTY;
    let Some(nodes) = graph.get("nodes").and_then(Value::as_object) else {
        return set;
    };
    for node in nodes.values() {
        let Some(node_type) = node.get("node_type") else {
            continue;
        };
        match classify(node_type) {
            Ok((_, Pure)) | Err(_) => {}
            Ok((_, Uses(capabilities))) => {
                set = set.union(crate::capability::CapSet::of(capabilities))
            }
            Ok((name, Forbidden(_))) => set = set.union(restricted_capabilities(&name)),
        }
    }
    set
}

/// [`runtime_capabilities_of_graph`] for a project, session or `.pwnode`
/// file's text (its `graph`).
pub fn runtime_capabilities_of_text(
    path: &str,
    text: &str,
) -> Result<crate::capability::CapSet, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("{path} is not valid JSON: {e}"))?;
    let graph = match value.get("graph") {
        Some(graph) if graph.is_object() => graph,
        _ => &value,
    };
    Ok(runtime_capabilities_of_graph(graph))
}

/// The name of a node type `classify` rejected (for display only).
fn unclassified_name(node_type: &Value) -> String {
    let name = match node_type {
        Value::String(name) => name.clone(),
        Value::Object(map) if map.len() == 1 => {
            let (name, body) = map.iter().next().expect("one entry");
            body.get("inner")
                .and_then(|i| i.get("type_tag"))
                .and_then(Value::as_str)
                .unwrap_or(name)
                .to_string()
        }
        _ => "(unreadable)".to_string(),
    };
    name.chars().take(64).collect()
}

/// Fail closed when the artifact uses more than the catalogue declares.
pub fn check_declared(
    declared: &BTreeSet<Capability>,
    found: &BTreeSet<Capability>,
) -> Result<(), String> {
    let missing: Vec<&str> = found.difference(declared).map(|c| c.as_str()).collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "The package uses capabilities it does not declare: {}.",
            missing.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_restricted_node_type_has_runtime_capabilities() {
        for (name, class) in VARIANTS.iter().chain(DYNAMIC_TAGS) {
            if let Forbidden(_) = class {
                assert!(
                    RESTRICTED_RUNTIME.iter().any(|(n, _)| n == name),
                    "{name} needs an entry in RESTRICTED_RUNTIME"
                );
            }
        }
        for (name, _) in RESTRICTED_RUNTIME {
            assert!(
                matches!(
                    lookup(VARIANTS, name).or_else(|| lookup(DYNAMIC_TAGS, name)),
                    Some(Forbidden(_))
                ),
                "{name} is not a restricted node type"
            );
        }
        let graph = json!({"nodes": {
            "1": {"node_type": {"HttpRequest": {}}},
            "2": {"node_type": {"Dynamic": {"inner": {"type_tag": "terminal"}}}},
            "3": {"node_type": {"Dynamic": {"inner": {"type_tag": "not-a-node"}}}},
            "4": {"node_type": "Synth"},
        }});
        assert_eq!(
            runtime_capabilities_of_graph(&graph),
            crate::capability::CapSet::of(&[Capability::Network, Capability::Process])
        );
    }

    const GRAPH_RS: &str = include_str!("../../graph.rs");

    /// Variant names of `pub enum NodeType` as written in graph.rs.
    fn node_type_variants() -> Vec<String> {
        let start = GRAPH_RS.find("pub enum NodeType {").expect("NodeType");
        let body = &GRAPH_RS[start..];
        let end = body
            .find("\n}\n")
            .or_else(|| body.find("\n}\r\n"))
            .expect("end");
        body[..end]
            .lines()
            .skip(1)
            .filter_map(|line| {
                let rest = line.strip_prefix("    ")?;
                if rest.starts_with(' ') {
                    return None;
                }
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect();
                name.starts_with(|c: char| c.is_ascii_uppercase())
                    .then_some(name)
            })
            .collect()
    }

    #[test]
    fn every_node_type_variant_is_classified() {
        let variants = node_type_variants();
        assert!(variants.len() > 50, "parsed {variants:?}");
        for variant in &variants {
            if variant == "Dynamic" {
                continue;
            }
            assert!(
                lookup(VARIANTS, variant).is_some(),
                "NodeType::{variant} is not classified for the marketplace"
            );
        }
        for (name, _) in VARIANTS {
            assert!(variants.iter().any(|v| v == name), "stale entry {name}");
        }
    }

    #[test]
    fn every_registered_dynamic_node_is_classified() {
        let tags = crate::node_trait::NODE_REGISTRY.lock().unwrap().type_tags();
        assert!(tags.len() > 50);
        for tag in &tags {
            assert!(
                lookup(DYNAMIC_TAGS, tag).is_some(),
                "dynamic node '{tag}' is not classified for the marketplace"
            );
        }
        for (name, _) in DYNAMIC_TAGS {
            assert!(tags.iter().any(|t| t == name), "stale entry {name}");
        }
    }

    fn pwnode(nodes: Vec<Value>) -> String {
        let map: serde_json::Map<String, Value> = nodes
            .into_iter()
            .enumerate()
            .map(|(i, node_type)| {
                (
                    i.to_string(),
                    json!({"id": i, "node_type": node_type, "pos": [0.0, 0.0]}),
                )
            })
            .collect();
        json!({"version": 1, "name": "T", "graph": {"nodes": map, "connections": []}}).to_string()
    }

    fn dynamic(tag: &str) -> Value {
        json!({"Dynamic": {"inner": {"type_tag": tag, "state": {}}}})
    }

    #[test]
    fn capabilities_are_collected_from_variants_and_dynamic_nodes() {
        let text = pwnode(vec![
            json!({"Slider": {}}),
            json!({"HttpRequest": {"url": "x"}}),
            json!({"Camera": {}}),
            dynamic("file"),
            dynamic("add"),
        ]);
        let found = scan_pwnode("a.pwnode", &text).unwrap();
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            vec![
                Capability::Filesystem,
                Capability::Network,
                Capability::MediaCapture,
                // P4.3.2B: Camera captures through ffmpeg.
                Capability::Process
            ]
        );
        assert!(
            scan_pwnode("a.pwnode", &pwnode(vec![json!({"Slider": {}})]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn forbidden_unknown_and_unclassifiable_nodes_are_refused() {
        for (node, needle) in [
            (json!({"RustPlugin": {}}), "native code"),
            (json!({"ClapPlugin": {}}), "native code"),
            (dynamic("terminal"), "system commands"),
            (dynamic("ai_config"), "credentials"),
            (
                json!({"TeleportNode": {}}),
                "unsupported node type 'TeleportNode'",
            ),
            (dynamic("evil"), "unsupported node type 'evil'"),
            (json!({"Dynamic": {"inner": {"state": {}}}}), "no type_tag"),
            (json!({"Dynamic": {}}), "no type_tag"),
            (json!(42), "unreadable type"),
            (json!({"Slider": {}, "Camera": {}}), "unreadable type"),
        ] {
            let error = scan_pwnode("x.pwnode", &pwnode(vec![node.clone()])).unwrap_err();
            assert!(error.contains(needle), "{node}: {error}");
        }
        assert!(
            scan_pwnode("x.pwnode", "{}")
                .unwrap_err()
                .contains("no node graph")
        );
        assert!(scan_pwnode("x.pwnode", "not json").is_err());
    }

    #[test]
    fn under_declaration_fails_closed() {
        let declared: BTreeSet<Capability> = [Capability::Network].into();
        let found: BTreeSet<Capability> = [Capability::Network, Capability::Filesystem].into();
        assert!(
            check_declared(&declared, &found)
                .unwrap_err()
                .contains("filesystem")
        );
        assert!(
            check_declared(&found, &declared).is_ok(),
            "over-declaring is allowed"
        );
        assert!(check_declared(&BTreeSet::new(), &BTreeSet::new()).is_ok());
    }

    // ── P4.3.2B: source audit guard ─────────────────────────────────────

    /// Privileged APIs and the capability each one needs.
    const SIGNALS: &[(&str, Capability)] = &[
        ("std::fs::", FS),
        ("File::open", FS),
        ("File::create", FS),
        ("read_dir(", FS),
        ("gltf::import", FS),
        ("image::open", FS),
        ("reqwest", NET),
        ("TcpStream", NET),
        ("TcpListener", NET),
        ("UdpSocket", NET),
        ("tungstenite", NET),
        ("HttpAction::", NET),
        ("play_pipe_audio", NET),
        ("serialport", DEV),
        ("midir", DEV),
        ("hidapi", DEV),
        ("build_input_stream", CAP),
        ("Command::new", PROC),
        ("open_externally", PROC),
        ("rhai::", SCR),
        ("sandboxed_engine", SCR),
        ("sysinfo::", SYS),
        ("ai_shared::", AIC),
    ];

    /// Reviewed exceptions: the privileged call is only reachable through
    /// an explicit action in the node's own UI (a file dialog or an "Open
    /// in editor" button), which graph content cannot trigger, or it reads
    /// a fixed application location.
    const REVIEWED: &[(&str, &str, &str)] = &[
        (
            "text_editor_node.rs",
            "std::fs::",
            "Open…/Save as… dialogs and the editor temp file",
        ),
        (
            "text_editor_node.rs",
            "open_externally",
            "\"Open in editor\" button",
        ),
        ("theme.rs", "std::fs::", "theme Save…/Open… dialogs"),
        (
            "script.rs",
            "std::fs::",
            "script Load/Save dialogs and the editor temp file",
        ),
        ("script.rs", "open_externally", "\"Open in editor\" button"),
        (
            "wgsl_presets.rs",
            "std::fs::",
            "reads the user's own preset folder",
        ),
        (
            "wgsl_presets.rs",
            "read_dir(",
            "reads the user's own preset folder",
        ),
        (
            "web_app_node.rs",
            "std::fs::",
            "\"Open in editor\" temp file",
        ),
        (
            "web_app_node.rs",
            "UdpSocket",
            "local address discovery for the LAN server",
        ),
        (
            "math_formula.rs",
            "rhai::",
            "expression-only Rhai under FORMULA_LIMITS: no statements, imports or eval",
        ),
        (
            "math_formula.rs",
            "sandboxed_engine",
            "expression-only Rhai under FORMULA_LIMITS: no statements, imports or eval",
        ),
    ];

    fn snake_case(name: &str) -> String {
        let mut out = String::new();
        for (i, c) in name.chars().enumerate() {
            if c.is_ascii_uppercase() && i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        }
        out
    }

    fn capabilities(class: NodeClass) -> Option<BTreeSet<Capability>> {
        match class {
            Pure => Some(BTreeSet::new()),
            Uses(caps) => Some(caps.iter().copied().collect()),
            Forbidden(_) => None,
        }
    }

    /// Strip `#[cfg(test)]` modules and line comments, so only shipped code
    /// is inspected.
    fn shipped_code(source: &str) -> String {
        let code = match source.find("#[cfg(test)]\nmod tests") {
            Some(at) => &source[..at],
            None => source,
        };
        code.lines()
            .map(|line| match line.find("//") {
                Some(at) => &line[..at],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn privileged_operations_match_the_classification() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let register = regex::Regex::new(r#"register(?:_[a-z]+)?\(\s*"([A-Za-z0-9_]+)""#).unwrap();
        let mut checked = 0;
        let mut problems = Vec::new();
        for dir in ["nodes", "ml"] {
            for entry in std::fs::read_dir(root.join(dir)).unwrap().flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let file = path.file_name().unwrap().to_string_lossy().into_owned();
                let source = std::fs::read_to_string(&path)
                    .unwrap()
                    .replace("\r\n", "\n");
                // The node types implemented in this file.
                let mut classes: Vec<(String, NodeClass)> = register
                    .captures_iter(&source)
                    .filter_map(|c| {
                        let tag = c[1].to_string();
                        lookup(DYNAMIC_TAGS, &tag).map(|class| (tag, class))
                    })
                    .collect();
                for (variant, class) in VARIANTS {
                    if format!("{}.rs", snake_case(variant)) == file {
                        classes.push((variant.to_string(), *class));
                    }
                }
                if classes.is_empty() {
                    continue;
                }
                let code = shipped_code(&source);
                for (name, class) in &classes {
                    let Some(allowed) = capabilities(*class) else {
                        continue; // refused anyway
                    };
                    checked += 1;
                    for (signal, needed) in SIGNALS {
                        if !code.contains(signal) || allowed.contains(needed) {
                            continue;
                        }
                        let reviewed = REVIEWED.iter().any(|(f, s, _)| *f == file && s == signal);
                        if !reviewed {
                            problems.push(format!(
                                "{file}: node '{name}' uses `{signal}` but is not classified as {}",
                                needed.as_str()
                            ));
                        }
                    }
                }
            }
        }
        assert!(checked > 60, "only {checked} node types were inspected");
        assert!(problems.is_empty(), "\n{}", problems.join("\n"));
        // Every reviewed exception still exists (no stale entries).
        for (file, signal, _) in REVIEWED {
            let source = std::fs::read_to_string(root.join("nodes").join(file)).unwrap();
            assert!(
                shipped_code(&source).contains(signal),
                "stale exception {file} {signal}"
            );
        }
    }

    #[test]
    fn corrected_classifications() {
        let caps = |table: &[(&str, NodeClass)], name: &str| {
            capabilities(lookup(table, name).unwrap()).unwrap()
        };
        let set = |c: &[Capability]| c.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(caps(DYNAMIC_TAGS, "shape_3d"), set(&[FS]));
        assert_eq!(caps(DYNAMIC_TAGS, "tts"), set(&[FS, PROC]));
        assert_eq!(caps(DYNAMIC_TAGS, "monitor"), set(&[SYS, PROC]));
        for ai in ["text_gen", "image_gen", "code_gen"] {
            assert!(caps(DYNAMIC_TAGS, ai).contains(&AIC), "{ai}");
        }
        assert!(caps(VARIANTS, "AiRequest").contains(&AIC));
        assert_eq!(caps(DYNAMIC_TAGS, "web_stream"), set(&[NET, PROC]));
        assert!(caps(DYNAMIC_TAGS, "html_viewer").contains(&NET));
        assert!(caps(VARIANTS, "Camera").contains(&PROC));
        assert!(CLASSIFICATION_VERSION >= 2);
        // The new capabilities are first-class.
        for c in [PROC, SYS, AIC] {
            assert_eq!(Capability::parse(c.as_str()), Some(c));
        }
    }

    #[test]
    fn nested_dependency_capabilities_are_found() {
        let leaf = pwnode(vec![dynamic("monitor")]);
        let root = pwnode(vec![
            json!({"CustomNodeInstance": {"file": "deps/x/1.0.0/X.pwnode", "inputs": [], "outputs": []}}),
        ]);
        let found = scan_files([
            ("Root.pwnode", root.as_str()),
            ("deps/x/1.0.0/X.pwnode", leaf.as_str()),
            ("deps/x/1.0.0/package.json", "{}"),
        ])
        .unwrap();
        assert_eq!(found, [SYS, PROC].into_iter().collect());
    }

    /// The assumption behind static payload validation (document
    /// `read_custom_node_static`): built-in variants are plain data.
    #[test]
    fn built_in_node_payloads_deserialize_without_running_code() {
        let start = GRAPH_RS.find("pub enum NodeType {").unwrap();
        let body = &GRAPH_RS[start..];
        let body = &body[..body.find("\n}\n").or_else(|| body.find("\n}\r\n")).unwrap()];
        assert!(
            !body.contains("deserialize_with"),
            "custom deserializer in NodeType"
        );
        assert!(
            !body.contains("serde(with"),
            "custom deserializer in NodeType"
        );
        assert_eq!(
            body.matches(": DynNode").count(),
            1,
            "only Dynamic holds runtime nodes"
        );
    }

    /// Servers meant for this machine only listen on loopback.
    #[test]
    fn local_servers_listen_on_loopback_only() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("nodes");
        for file in ["html_viewer.rs", "html_viewer_node.rs", "websocket_node.rs"] {
            let code = shipped_code(&std::fs::read_to_string(root.join(file)).unwrap());
            assert!(
                code.contains("TcpListener::bind(\"127.0.0.1:0\")"),
                "{file}"
            );
            assert!(
                !code.contains("0.0.0.0"),
                "{file} listens beyond this machine"
            );
        }
        // The LAN servers are classified as network nodes.
        for tag in ["web_stream", "web_app"] {
            assert!(
                capabilities(lookup(DYNAMIC_TAGS, tag).unwrap())
                    .unwrap()
                    .contains(&NET)
            );
        }
    }
}
