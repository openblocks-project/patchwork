//! P4.3.2A: placeholder for a dynamic node whose type is not available.
//!
//! When a graph contains a dynamic node (`{"Dynamic": {"inner":
//! {"type_tag": …, "state": …}}}`) whose `type_tag` is not registered in
//! this Patchwork, it becomes an `UnknownNode` instead of being silently
//! replaced by a working node:
//!
//! * It keeps the original serialized object verbatim and writes it back
//!   unchanged, so saving and reopening never lose its configuration, and a
//!   later Patchwork that has the type loads it normally.
//! * It is inert: no factory, constructor, `load_state` or setup hook of
//!   any real node runs; it evaluates to nothing and renders only a notice.
//! * It keeps its connections: its ports are sized from the wires attached
//!   to it when a graph is loaded (`Graph::fix_next_id`).

use serde_json::Value;

use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::NodeBehavior;

#[derive(Debug, Clone)]
pub struct UnknownNode {
    /// The original `inner` object, exactly as read.
    raw: Value,
    tag: String,
    title: String,
    /// Port types, learned from the ports wired to them (`Generic` when
    /// unknown). Only grows, so no wire ever loses its port.
    inputs: Vec<PortKind>,
    outputs: Vec<PortKind>,
}

impl UnknownNode {
    pub fn new(raw: Value) -> Self {
        let tag = raw
            .get("type_tag")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let title = if tag.is_empty() {
            "Missing node (no type)".to_string()
        } else {
            format!("Missing node: {tag}")
        };
        Self {
            raw,
            tag,
            title,
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
    }

    /// The original serialized node (`type_tag`, `state`, anything else).
    pub fn raw(&self) -> &Value {
        &self.raw
    }

    /// The original `state`, if any.
    #[allow(dead_code)]
    pub fn state(&self) -> Option<&Value> {
        self.raw.get("state")
    }

    /// Grow the placeholder ports so that every attached wire has a port.
    pub fn ensure_ports(&mut self, inputs: usize, outputs: usize) {
        if self.inputs.len() < inputs {
            self.inputs.resize(inputs, PortKind::Generic);
        }
        if self.outputs.len() < outputs {
            self.outputs.resize(outputs, PortKind::Generic);
        }
    }

    /// Give an untyped port the type of the port wired to it.
    pub fn learn_port_kind(&mut self, is_input: bool, index: usize, kind: PortKind) {
        let ports = if is_input {
            &mut self.inputs
        } else {
            &mut self.outputs
        };
        if ports.len() <= index {
            ports.resize(index + 1, PortKind::Generic);
        }
        if ports[index] == PortKind::Generic {
            ports[index] = kind;
        }
    }

    pub fn missing_message(&self) -> String {
        let what = if self.tag.is_empty() {
            "This node has no type".to_string()
        } else {
            format!(
                "Node type '{}' is not available in this version of Patchwork",
                self.tag
            )
        };
        format!(
            "{what}. Its settings and connections are kept unchanged, but it does nothing. \
             It works again in a Patchwork that provides this node type."
        )
    }
}

fn ports(prefix: &str, kinds: &[PortKind]) -> Vec<PortDef> {
    kinds
        .iter()
        .enumerate()
        .map(|(i, kind)| PortDef {
            name: format!("{prefix} {}", i + 1).into(),
            kind: *kind,
        })
        .collect()
}

impl NodeBehavior for UnknownNode {
    fn title(&self) -> &str {
        &self.title
    }

    fn inputs(&self) -> Vec<PortDef> {
        ports("in", &self.inputs)
    }

    fn outputs(&self) -> Vec<PortDef> {
        ports("out", &self.outputs)
    }

    fn color_hint(&self) -> [u8; 3] {
        [190, 70, 70]
    }

    /// Never evaluates: downstream nodes receive no value.
    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        Vec::new()
    }

    fn type_tag(&self) -> &str {
        &self.tag
    }

    fn save_state(&self) -> Value {
        self.raw.get("state").cloned().unwrap_or(Value::Null)
    }

    /// The original data is kept as read; nothing is applied.
    fn load_state(&mut self, _state: &Value) {}

    fn render_ui(&mut self, ui: &mut eframe::egui::Ui) {
        ui.label(
            eframe::egui::RichText::new(self.missing_message())
                .color(eframe::egui::Color32::from_rgb(255, 150, 150)),
        );
    }

    fn as_unknown(&self) -> Option<&UnknownNode> {
        Some(self)
    }

    fn as_unknown_mut(&mut self) -> Option<&mut UnknownNode> {
        Some(self)
    }
}

#[cfg(test)]
mod tests;
