//! Machine-learning subsystem.
//!
//! This tree houses shared ML primitives used by multiple node types:
//! bundled model bytes, (future) execution-provider configuration, model
//! registry, pipeline abstraction, and task-specific nodes (e.g. Pose Match).
//!
//! Legacy inference code still lives in `crate::nodes::ml_model`; it re-exports
//! bundled bytes from here so existing importers in the tracker nodes continue
//! to compile without churn.

pub mod bundled;
pub mod ep;
pub mod match_node;
pub mod regress_node;
