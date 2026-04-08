//! Per-node error tracking — surfaces evaluation failures to the UI and console.
//!
//! Two surfaces consume this module:
//!  1. The canvas renderer paints a red border + ⚠ badge on any node whose
//!     id has an entry in the global error map, with a tooltip showing the
//!     message on hover.
//!  2. `crate::system_log` receives an `error()` line for each new failure
//!     so it shows up in the Console node automatically.
//!
//! Two ways to record an error:
//!  - **Implicit (panics):** `Graph::evaluate` wraps each `evaluate_node`
//!    call in `catch_unwind`. If a node panics, the panic message is stored
//!    against that node's id.
//!  - **Explicit:** A node implementation can call
//!    `node_errors::report("...")` from inside its `evaluate()` body. The
//!    current node id is set by the graph evaluator via `set_current_node`
//!    on a thread-local just before the call, so `report()` knows which
//!    node to attribute the error to.
//!
//! Errors are auto-cleared at the start of every `evaluate_node` call —
//! a node that succeeds (no panic, no explicit report) clears its own
//! error from the previous frame. Persistent failures stay sticky because
//! they re-occur every frame.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::graph::NodeId;

#[derive(Clone, Debug)]
pub struct NodeError {
    pub message: String,
    #[allow(dead_code)]
    pub timestamp_ms: u64,
}

static ERRORS: Mutex<Option<HashMap<NodeId, NodeError>>> = Mutex::new(None);

thread_local! {
    static CURRENT_NODE: Cell<Option<NodeId>> = const { Cell::new(None) };
}

fn with_map<R>(f: impl FnOnce(&mut HashMap<NodeId, NodeError>) -> R) -> R {
    let mut guard = ERRORS.lock().expect("node_errors mutex poisoned");
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

/// Set the node id that subsequent `report()` calls (on this thread) should
/// be attributed to. Pass `None` after the node finishes evaluating.
pub fn set_current_node(id: Option<NodeId>) {
    CURRENT_NODE.with(|c| c.set(id));
}

/// Report an error against the currently-evaluating node (set via
/// `set_current_node`). No-op if no current node is set.
#[allow(dead_code)]
pub fn report(message: impl Into<String>) {
    if let Some(id) = CURRENT_NODE.with(|c| c.get()) {
        report_for(id, message);
    }
}

/// Record an error for `node_id`. Replaces any prior error for that node
/// and writes a line to the system log so the Console node picks it up.
pub fn report_for(node_id: NodeId, message: impl Into<String>) {
    let msg = message.into();
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    // Only push to system_log if the message is new or changed for this node,
    // so a node that fails every frame doesn't spam the console.
    let is_new_or_changed = with_map(|m| {
        let changed = m.get(&node_id).map(|e| e.message != msg).unwrap_or(true);
        m.insert(node_id, NodeError { message: msg.clone(), timestamp_ms });
        changed
    });
    if is_new_or_changed {
        crate::system_log::error(format!("Node #{}: {}", node_id, msg));
    }
}

/// Clear the error (if any) for `node_id`. Called at the start of every
/// `evaluate_node` so a recovered node loses its red badge.
pub fn clear(node_id: NodeId) {
    with_map(|m| { m.remove(&node_id); });
}

/// Clear errors for a deleted node so the badge doesn't linger if the
/// id is reused later.
pub fn remove_for(node_id: NodeId) {
    with_map(|m| { m.remove(&node_id); });
}

/// Look up the current error for a node, if any.
pub fn get(node_id: NodeId) -> Option<NodeError> {
    with_map(|m| m.get(&node_id).cloned())
}
