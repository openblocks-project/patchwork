//! P4.2 Stage 5: explicit custom-node version switching.
//!
//! A switch is planned first (read-only: the target version is resolved and
//! verified with the same resolver and integrity checks evaluation uses, and
//! the effect on connections is computed with the same port remapping that
//! interface synchronisation uses), then applied in one step. Applying
//! changes the version, entry, compatibility path, ports and connections of
//! exactly one instance and drops its runtime. Nothing is upgraded
//! automatically and nothing falls back to another version.

use std::collections::BTreeSet;

use super::{Graph, NodeId, NodeType, remap_custom_port};
use crate::document::CustomNodePort;
use crate::library::package::{self, PACKAGES_DIR};
use crate::library::resolve::InstancePackageRef;

/// One installed version of the instance's package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionChoice {
    /// The version folder name (a stable `MAJOR.MINOR.PATCH`).
    pub version: String,
    /// Reference to switch to; `None` when its manifest cannot be read.
    pub reference: Option<InstancePackageRef>,
    /// The version the instance uses now (shown, not selectable).
    pub current: bool,
    /// `Err(reason)` for missing, damaged, incompatible or unsafe versions.
    pub status: Result<(), String>,
}

impl VersionChoice {
    pub fn selectable(&self) -> bool {
        !self.current && self.status.is_ok() && self.reference.is_some()
    }
}

/// Why an existing external port does not carry over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortChange {
    /// No port with this name in the target version.
    Removed,
    /// Same name, different type.
    TypeChanged { to: String },
    /// Could not be matched unambiguously (e.g. duplicate names).
    Unmatched,
}

/// An external port of the current version that the switch would lose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffectedPort {
    pub name: String,
    pub kind: String,
    pub is_input: bool,
    pub change: PortChange,
    /// Connections on this port that would be removed.
    pub connections: usize,
}

/// A connection the switch would remove, exactly as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedWire {
    pub from_node: NodeId,
    pub from_port: usize,
    pub to_node: NodeId,
    pub to_port: usize,
    /// The instance port it is attached to.
    pub port: String,
    pub is_input: bool,
}

/// What a plan was made from: the exact node (runtime identity, full
/// serialized state) and every connection attached to it. A plan is only
/// applied while this is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchContext {
    /// Unique per created node; a node recreated with the same id differs.
    pub token: u64,
    /// The serialized node (id, type with file/package/ports, position).
    pub node_state: String,
    /// Attached connections `(from, from_port, to, to_port, label)`, sorted.
    pub connections: Vec<(NodeId, usize, NodeId, usize, String)>,
}

/// A verified, ready-to-apply switch of one instance.
#[derive(Debug, Clone)]
pub struct VersionSwitchPlan {
    pub instance: NodeId,
    /// The node and connections the plan was computed from.
    pub context: SwitchContext,
    /// Exactly the connections the switch removes.
    pub removed: Vec<RemovedWire>,
    pub from: InstancePackageRef,
    pub to: InstancePackageRef,
    /// Resolved, verified entry of the target version.
    pub file: String,
    pub inputs: Vec<CustomNodePort>,
    pub outputs: Vec<CustomNodePort>,
    /// Ports of the current version that do not carry over.
    pub affected: Vec<AffectedPort>,
    /// Total connections the switch would remove.
    pub removed_connections: usize,
}

impl VersionSwitchPlan {
    /// A switch that loses ports or connections must be confirmed.
    pub fn needs_confirmation(&self) -> bool {
        !self.affected.is_empty()
    }

    /// Identical in every respect: same node (identity and state), same
    /// attached connections, same target, and exactly the same wires
    /// removed. Only then may a confirmation be applied as shown.
    pub fn same_effect(&self, other: &VersionSwitchPlan) -> bool {
        self.instance == other.instance
            && self.context == other.context
            && self.from == other.from
            && self.to == other.to
            && self.file == other.file
            && self.inputs == other.inputs
            && self.outputs == other.outputs
            && self.affected == other.affected
            && self.removed == other.removed
    }

    /// The same node object (not just the same id).
    pub fn same_node(&self, other: &VersionSwitchPlan) -> bool {
        self.instance == other.instance && self.context.token == other.context.token
    }
}

fn affected_ports(
    old: &[CustomNodePort],
    new: &[CustomNodePort],
    is_input: bool,
    connections_on: impl Fn(usize) -> usize,
) -> Vec<AffectedPort> {
    let mut affected = Vec::new();
    for (index, port) in old.iter().enumerate() {
        if remap_custom_port(old, new, index).is_some() {
            continue;
        }
        let same_name: Vec<&CustomNodePort> = new.iter().filter(|p| p.name == port.name).collect();
        let change = match same_name.as_slice() {
            [] => PortChange::Removed,
            [only] if only.kind != port.kind => PortChange::TypeChanged {
                to: only.kind.clone(),
            },
            _ => PortChange::Unmatched,
        };
        affected.push(AffectedPort {
            name: port.name.clone(),
            kind: port.kind.clone(),
            is_input,
            change,
            connections: connections_on(index),
        });
    }
    affected
}

impl Graph {
    /// Identity and exact state of node `id` and its attached connections.
    pub fn switch_context(&self, id: NodeId) -> Option<SwitchContext> {
        let node = self.nodes.get(&id)?;
        let mut connections: Vec<(NodeId, usize, NodeId, usize, String)> = self
            .connections
            .iter()
            .filter(|c| c.from_node == id || c.to_node == id)
            .map(|c| {
                (
                    c.from_node,
                    c.from_port,
                    c.to_node,
                    c.to_port,
                    c.label.clone(),
                )
            })
            .collect();
        connections.sort();
        Some(SwitchContext {
            token: node.token,
            node_state: serde_json::to_string(node).unwrap_or_default(),
            connections,
        })
    }

    /// The packaged instance `id`'s current reference and ports.
    fn packaged_instance(
        &self,
        id: NodeId,
    ) -> Result<(InstancePackageRef, Vec<CustomNodePort>, Vec<CustomNodePort>), String> {
        match self.nodes.get(&id).map(|n| &n.node_type) {
            Some(NodeType::CustomNodeInstance {
                package: Some(package),
                inputs,
                outputs,
                ..
            }) => Ok((package.clone(), inputs.clone(), outputs.clone())),
            Some(NodeType::CustomNodeInstance { .. }) => Err(
                "This custom node is not a versioned package (reinsert it from the Library to \
                 use versions)."
                    .to_string(),
            ),
            _ => Err("The node is not a custom node.".to_string()),
        }
    }

    /// Installed versions of the package used by instance `id`, from this
    /// project's `packages/` and the Library, each verified with the
    /// evaluation resolver (project copy first). Sorted by version.
    pub fn available_versions(&mut self, id: NodeId) -> Vec<VersionChoice> {
        let Ok((current, _, _)) = self.packaged_instance(id) else {
            return Vec::new();
        };
        let current_version = package::parse_stable_version(&current.version).ok();
        let roots: Vec<std::path::PathBuf> = [
            self.package_resolver.project_dir(),
            self.package_resolver.library_root(),
        ]
        .into_iter()
        .flatten()
        .map(std::path::Path::to_path_buf)
        .collect();

        let mut versions = BTreeSet::new();
        for root in &roots {
            let Ok(entries) = std::fs::read_dir(root.join(PACKAGES_DIR).join(&current.id)) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Ok(version) = package::parse_stable_version(&name) {
                    if entry.path().is_dir() {
                        versions.insert(version);
                    }
                }
            }
        }
        if let Some(version) = &current_version {
            versions.insert(version.clone()); // always shown, even if gone
        }

        versions
            .into_iter()
            .map(|version| {
                let is_current = Some(&version) == current_version.as_ref();
                // The manifest of the copy the resolver would use names the
                // entry (it may differ between versions).
                let manifest = roots.iter().find_map(|root| {
                    let dir = package::version_dir(root, &current.id, &version);
                    dir.exists().then(|| package::read_manifest(&dir))
                });
                let reference = match (&manifest, is_current) {
                    (_, true) => Some(current.clone()),
                    (Some(Ok(m)), false) => Some(InstancePackageRef {
                        id: current.id.clone(),
                        version: version.to_string(),
                        entry: m.entry.clone(),
                    }),
                    _ => None,
                };
                let status = match (&reference, &manifest) {
                    (_, None) => Err("Not installed in this project or the Library.".to_string()),
                    (None, Some(Err(e))) => Err(format!("Damaged: {e}")),
                    (Some(reference), _) => match reference.to_ref() {
                        Ok(exact) => self
                            .package_resolver
                            .resolve(&exact)
                            .map(|_| ())
                            .map_err(|e| e.to_string()),
                        Err(e) => Err(e),
                    },
                    (None, Some(Ok(_))) => Err("Unreadable package.".to_string()),
                };
                VersionChoice {
                    version: version.to_string(),
                    reference,
                    current: is_current,
                    status,
                }
            })
            .collect()
    }

    /// Plan switching instance `id` to `target` (same package, another
    /// exact version). Read-only. Fails, without side effects, when the
    /// target is the current version, another package, or missing,
    /// damaged, incompatible or unsafe.
    pub fn plan_version_switch(
        &mut self,
        id: NodeId,
        target: &InstancePackageRef,
    ) -> Result<VersionSwitchPlan, String> {
        let (current, old_inputs, old_outputs) = self.packaged_instance(id)?;
        if target.id != current.id {
            return Err(format!(
                "{target} is a different package; only versions of {} can be selected.",
                current.id
            ));
        }
        let exact = target.to_ref()?;
        if package::parse_stable_version(&current.version).ok() == Some(exact.version.clone())
            && target.entry == current.entry
        {
            return Err(format!("This node already uses {current}."));
        }
        // Same resolver and integrity checks as evaluation; exact version,
        // no fallback to another one.
        let resolved = self
            .package_resolver
            .resolve(&exact)
            .map_err(|e| format!("{target} can't be used: {e}"))?;
        // P4.3.2D: read statically; the target version is authorized (and
        // constructed) only when its runtime is activated after the switch.
        let interface = crate::document::read_custom_node_interface_static(&resolved.entry_path)
            .map_err(|e| format!("{target} can't be used: {e}"))?;

        let connections = &self.connections;
        let mut affected = affected_ports(&old_inputs, &interface.inputs, true, |port| {
            connections
                .iter()
                .filter(|c| c.to_node == id && c.to_port == port)
                .count()
        });
        affected.extend(affected_ports(
            &old_outputs,
            &interface.outputs,
            false,
            |port| {
                connections
                    .iter()
                    .filter(|c| c.from_node == id && c.from_port == port)
                    .count()
            },
        ));
        let removed_connections = affected.iter().map(|p| p.connections).sum();
        // The exact wires on the affected ports.
        let mut removed: Vec<RemovedWire> = Vec::new();
        for (index, port) in old_inputs.iter().enumerate() {
            if remap_custom_port(&old_inputs, &interface.inputs, index).is_none() {
                removed.extend(
                    self.connections
                        .iter()
                        .filter(|c| c.to_node == id && c.to_port == index)
                        .map(|c| RemovedWire {
                            from_node: c.from_node,
                            from_port: c.from_port,
                            to_node: c.to_node,
                            to_port: c.to_port,
                            port: port.name.clone(),
                            is_input: true,
                        }),
                );
            }
        }
        for (index, port) in old_outputs.iter().enumerate() {
            if remap_custom_port(&old_outputs, &interface.outputs, index).is_none() {
                removed.extend(
                    self.connections
                        .iter()
                        .filter(|c| c.from_node == id && c.from_port == index)
                        .map(|c| RemovedWire {
                            from_node: c.from_node,
                            from_port: c.from_port,
                            to_node: c.to_node,
                            to_port: c.to_port,
                            port: port.name.clone(),
                            is_input: false,
                        }),
                );
            }
        }
        let context = self
            .switch_context(id)
            .ok_or_else(|| "The node no longer exists.".to_string())?;
        Ok(VersionSwitchPlan {
            instance: id,
            context,
            removed,
            from: current,
            to: InstancePackageRef::from_ref(&exact),
            file: resolved.entry_path.to_string_lossy().into_owned(),
            inputs: interface.inputs,
            outputs: interface.outputs,
            affected,
            removed_connections,
        })
    }

    /// Apply a plan made from the current graph (see
    /// [`Graph::plan_version_switch`]): version, entry, compatibility path,
    /// ports and connections change together, and the instance's runtime
    /// state is dropped so it reloads the selected version.
    ///
    /// All checks happen first: unless the node is the same node object in
    /// exactly the planned state with exactly the planned connections, it
    /// refuses and changes nothing. After that the update cannot fail.
    pub fn apply_version_switch(
        &mut self,
        plan: &VersionSwitchPlan,
    ) -> Result<super::CustomPortSyncReport, String> {
        if self.switch_context(plan.instance).as_ref() != Some(&plan.context) {
            return Err(
                "The node or its connections changed since the switch was previewed.".to_string(),
            );
        }
        match self.packaged_instance(plan.instance) {
            Ok((current, _, _)) if current == plan.from => {}
            _ => return Err("The node changed since the switch was planned.".to_string()),
        }
        if let Some(NodeType::CustomNodeInstance { file, package, .. }) =
            self.nodes.get_mut(&plan.instance).map(|n| &mut n.node_type)
        {
            *file = plan.file.clone();
            *package = Some(plan.to.clone());
        }
        let report =
            self.sync_custom_ports(plan.instance, plan.inputs.clone(), plan.outputs.clone());
        self.custom_runtimes.remove(&plan.instance);
        self.custom_runtime_modified.remove(&plan.instance);
        self.custom_runtime_failures.remove(&plan.instance);
        self.custom_package_paths.remove(&plan.instance);
        self.custom_file_in_package.remove(&plan.instance);
        self.topo_dirty = true;
        self.audio_topology_dirty = true;
        Ok(report)
    }
}

#[cfg(test)]
mod tests;
