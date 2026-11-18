//! P4.3.2D: runtime authorization of packaged custom nodes.
//!
//! Every packaged custom-node runtime (a top-level instance or a nested
//! one, whatever created it: the palette, a project or its bundled
//! packages, session restore, paste, duplicate, undo, version switching or
//! MCP) is built in one place, `Graph::load_custom_runtime`, and only after
//! [`authorize_entry`] succeeded for the package **and every package it
//! depends on**:
//!
//! 1. static verification of the version folder (nothing is constructed);
//! 2. its exact content identity (`id`, `version`, tree digest), hashed
//!    from the bytes on disk;
//! 3. the trust record for exactly that identity;
//! 4. the record's capability classification version;
//! 5. a fresh capability scan of the hashed graphs;
//! 6. the execution grant, or the local-author policy below.
//!
//! The runtime is then built from the very text that was hashed
//! ([`Authorized::entry_text`]), never re-read from disk, so different bytes
//! cannot be authorized than the ones that run.
//!
//! **Local-author policy.** A package whose record says Patchwork itself
//! published exactly these bytes on this computer (`local_publish`
//! provenance), under the current classification, runs without a prompt
//! unless the user denied it. Everything else needs an explicit grant:
//! marketplace installs (installation consent is not execution consent),
//! imported or reviewed packages, packages bundled in projects, and any
//! package without a record, even one with no privileged capabilities. An
//! id prefix, a manifest field or a folder location never counts.
//!
//! A grant covers one exact identity, the classification it was given
//! under and the capabilities approved; a different digest, version,
//! classification or a capability outside the approved set needs a new
//! decision. A denial persists and is not offered again automatically.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{
    Grant, PackageIdentity, Provenance, StoreLoad, TrustStatus, TrustStore, generation, identify,
};
use crate::library::marketplace::Capability;
use crate::library::marketplace::scan::inventory_files;
use crate::library::resolve::{Fingerprint, PackageResolver, package_fingerprint};

/// Deepest dependency chain followed (as in custom-node recursion checks).
const MAX_DEPTH: usize = 8;

/// Which trust store decides, for one graph and its nested runtimes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustPolicy {
    /// No store configured: every package is blocked.
    Unconfigured,
    Store(TrustStore),
    /// Unit tests written before P4.3.2D (they do not exercise permissions).
    #[cfg(test)]
    AllowAll,
}

#[cfg(test)]
thread_local! {
    /// Tests that predate P4.3.2D build graphs without configuring a store;
    /// permission tests switch this off (and production has no such switch).
    pub(crate) static TEST_DEFAULT_ALLOW: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

impl TrustPolicy {
    /// Whether decisions follow the trust store (and so must be made again
    /// when it changes). Only the test-only "allow all" policy does not.
    pub fn follows_store(&self) -> bool {
        match self {
            TrustPolicy::Unconfigured | TrustPolicy::Store(_) => true,
            #[cfg(test)]
            TrustPolicy::AllowAll => false,
        }
    }
}

impl Default for TrustPolicy {
    fn default() -> Self {
        #[cfg(test)]
        if TEST_DEFAULT_ALLOW.with(|allow| allow.get()) {
            return TrustPolicy::AllowAll;
        }
        TrustPolicy::Unconfigured
    }
}

/// What the user needs to see to decide about a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSummary {
    pub dir: PathBuf,
    pub name: String,
    /// `id@version` (from the manifest; a claim until identified).
    pub label: String,
    /// `None` when the folder could not be identified.
    pub identity: Option<PackageIdentity>,
    /// Verified origin from the trust record, if any.
    pub provenance: Option<Provenance>,
    pub declared: Option<BTreeSet<Capability>>,
    pub detected: BTreeSet<Capability>,
    pub restricted: BTreeSet<String>,
    pub unclassified: BTreeSet<String>,
    /// P4.3.2E1: a loose custom-node file (not a package): `dir` is the
    /// file and this is its content digest.
    pub loose_digest: Option<String>,
}

impl PackageSummary {
    fn unidentified(dir: &Path) -> Self {
        let label = match (dir.parent().and_then(Path::file_name), dir.file_name()) {
            (Some(id), Some(version)) => {
                format!("{}@{}", id.to_string_lossy(), version.to_string_lossy())
            }
            _ => dir.display().to_string(),
        };
        Self {
            dir: dir.to_path_buf(),
            name: label.clone(),
            label,
            identity: None,
            provenance: None,
            declared: None,
            detected: BTreeSet::new(),
            restricted: BTreeSet::new(),
            unclassified: BTreeSet::new(),
            loose_digest: None,
        }
    }

    /// "Origin not verified" unless a trusted workflow recorded it.
    pub fn origin_label(&self) -> String {
        match &self.provenance {
            Some(provenance @ (Provenance::LocalPublish | Provenance::Marketplace { .. })) => {
                format!("Verified: {}", provenance.label())
            }
            _ => "Origin not verified".to_string(),
        }
    }
}

/// Why a package may not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    /// No trust store is configured for this graph.
    Unconfigured,
    /// The trust store cannot be read; nothing is trusted.
    StoreDamaged(String),
    /// No decision yet.
    NeedsPermission,
    /// The user denied it.
    Denied,
    /// A decision exists but no longer applies (classification or
    /// capabilities changed); it needs a new decision.
    Stale(String),
    /// Another content of the same `id@version` is recorded.
    Conflict,
    /// The folder failed verification or cannot be identified.
    Unidentifiable(String),
    /// A package it depends on is blocked.
    Dependency(Box<Blocked>),
}

/// A package that may not run, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    pub package: PackageSummary,
    pub reason: BlockReason,
}

impl Blocked {
    /// The package that actually needs attention (a blocked dependency
    /// rather than the parent that uses it).
    pub fn innermost(&self) -> &Blocked {
        match &self.reason {
            BlockReason::Dependency(inner) => inner.innermost(),
            _ => self,
        }
    }

    /// Whether the user can decide about it now (counts in the banner).
    /// Denied packages are not offered again automatically.
    pub fn needs_review(&self) -> bool {
        let inner = self.innermost();
        (inner.package.identity.is_some() || inner.package.loose_digest.is_some())
            && matches!(
                inner.reason,
                BlockReason::NeedsPermission | BlockReason::Stale(_)
            )
    }

    /// Shown on the node.
    pub fn message(&self) -> String {
        let label = &self.package.label;
        match &self.reason {
            BlockReason::Unconfigured => {
                format!("Needs permission: {label} (package permissions are unavailable)")
            }
            BlockReason::StoreDamaged(why) => {
                format!("Needs permission: {label} (the trust store is damaged: {why})")
            }
            BlockReason::NeedsPermission => format!(
                "Needs permission: {label} has not been allowed to run. Review it in \
                 Library → Permissions."
            ),
            BlockReason::Denied => format!("Blocked: running {label} was denied."),
            BlockReason::Stale(why) => {
                format!("Needs permission: {label} must be reviewed again ({why}).")
            }
            BlockReason::Conflict => format!(
                "Blocked: a different package with the same id and version as {label} is \
                 already recorded."
            ),
            BlockReason::Unidentifiable(why) => {
                format!("Blocked: {label} cannot be verified: {why}")
            }
            BlockReason::Dependency(inner) => {
                format!(
                    "{label} is blocked by a package it uses. {}",
                    inner.message()
                )
            }
        }
    }
}

/// How an authorized package was allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizedBy {
    Grant,
    LocalAuthor,
    #[cfg(test)]
    TestPolicy,
}

/// A package (and its dependencies) allowed to run, with the exact entry
/// text that was hashed.
#[derive(Debug, Clone)]
pub struct Authorized {
    pub summary: PackageSummary,
    pub identity: PackageIdentity,
    /// Grant or local-author policy (checked by the permission tests).
    #[allow(dead_code)]
    pub by: AuthorizedBy,
    pub entry_text: String,
    /// P4.3.2E2: what this package's own graph may do: the approved
    /// capabilities of its grant (with those of the restricted node types
    /// the grant covered), or everything under the local-author policy.
    /// Its runtime gets this intersected with its parent's rights.
    pub capabilities: crate::capability::CapSet,
}

/// The version folder whose entry is `entry`: the nearest enclosing folder
/// with a package manifest naming that file as its entry.
pub fn package_dir_for_entry(entry: &Path) -> Option<PathBuf> {
    let dir = entry.parent()?;
    let manifest = crate::library::package::read_manifest(dir).ok()?;
    (entry.file_name() == Some(std::ffi::OsStr::new(&manifest.entry))).then(|| dir.to_path_buf())
}

// ── Blocked-state cache ─────────────────────────────────────────────────

/// A blocked decision is reused (without hashing) while the package folder
/// metadata, the store file and the trust generation are unchanged. Only
/// blocked results are cached: an authorization is always recomputed from
/// the bytes, immediately before the runtime is built.
#[derive(Clone)]
struct CachedBlock {
    folder: Fingerprint,
    store: Option<(std::time::SystemTime, u64)>,
    generation: u64,
    blocked: Blocked,
}

static BLOCK_CACHE: Mutex<Option<HashMap<(PathBuf, PathBuf), CachedBlock>>> = Mutex::new(None);

fn store_stamp(store: &TrustStore) -> Option<(std::time::SystemTime, u64)> {
    std::fs::metadata(&store.path)
        .ok()
        .and_then(|m| m.modified().ok().map(|t| (t, m.len())))
}

fn cached_block(store: &TrustStore, dir: &Path) -> Option<Blocked> {
    let cache = BLOCK_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let entry = cache
        .as_ref()?
        .get(&(store.path.clone(), dir.to_path_buf()))?;
    (entry.generation == generation()
        && entry.store == store_stamp(store)
        && entry.folder == package_fingerprint(dir))
    .then(|| entry.blocked.clone())
}

fn remember(store: &TrustStore, dir: &Path, result: &Result<Authorized, Blocked>) {
    let mut cache = BLOCK_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    let key = (store.path.clone(), dir.to_path_buf());
    match result {
        Err(blocked) => {
            if cache.len() > 4096 {
                cache.clear();
            }
            cache.insert(
                key,
                CachedBlock {
                    folder: package_fingerprint(dir),
                    store: store_stamp(store),
                    generation: generation(),
                    blocked: blocked.clone(),
                },
            );
        }
        Ok(_) => {
            cache.remove(&key);
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Number of full authorizations (hashing) on this thread.
    pub(crate) static FULL_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// ── Authorization ───────────────────────────────────────────────────────

/// Authorize one version folder (not its dependencies).
pub fn authorize_dir(policy: &TrustPolicy, dir: &Path) -> Result<Authorized, Blocked> {
    let store = match policy {
        TrustPolicy::Unconfigured => {
            return Err(Blocked {
                package: PackageSummary::unidentified(dir),
                reason: BlockReason::Unconfigured,
            });
        }
        TrustPolicy::Store(store) => Some(store),
        #[cfg(test)]
        TrustPolicy::AllowAll => None,
    };
    if let Some(store) = store {
        if let Some(blocked) = cached_block(store, dir) {
            return Err(blocked);
        }
    }
    let result = decide(store, dir);
    if let Some(store) = store {
        remember(store, dir, &result);
    }
    result
}

fn decide(store: Option<&TrustStore>, dir: &Path) -> Result<Authorized, Blocked> {
    #[cfg(test)]
    FULL_CHECKS.with(|count| count.set(count.get() + 1));
    let unidentifiable = |why: String| Blocked {
        package: PackageSummary::unidentified(dir),
        reason: BlockReason::Unidentifiable(why),
    };
    // 1–2: static verification and the exact identity of the bytes on disk.
    let package = identify(dir, true).map_err(unidentifiable)?;
    let entry_text = package
        .tree
        .graphs
        .iter()
        .find(|(path, _)| *path == package.manifest.entry)
        .map(|(_, text)| text.clone())
        .ok_or_else(|| unidentifiable("the package has no entry graph".to_string()))?;
    // 5: rescan what the hashed graphs use.
    let inventory = inventory_files(
        package
            .tree
            .graphs
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str())),
    )
    .map_err(unidentifiable)?;
    let mut summary = PackageSummary {
        dir: dir.to_path_buf(),
        name: package.manifest.name.clone(),
        label: package.identity.label(),
        identity: Some(package.identity.clone()),
        provenance: None,
        declared: None,
        detected: inventory.capabilities.clone(),
        restricted: inventory.restricted.clone(),
        unclassified: inventory.unclassified.clone(),
        loose_digest: None,
    };
    let authorized = |summary: PackageSummary, by, capabilities| Authorized {
        summary,
        identity: package.identity.clone(),
        by,
        entry_text: entry_text.clone(),
        capabilities,
    };
    let Some(store) = store else {
        #[cfg(test)]
        return Ok(authorized(
            summary,
            AuthorizedBy::TestPolicy,
            crate::capability::CapSet::ALL,
        ));
        #[cfg(not(test))]
        unreachable!("only the test policy has no store");
    };
    let blocked = |summary: PackageSummary, reason| {
        Err(Blocked {
            package: summary,
            reason,
        })
    };
    // 3–4: the record for exactly this identity, under this classification.
    let load = store.load();
    if let StoreLoad::Damaged(why) = &load {
        return blocked(summary, BlockReason::StoreDamaged(why.clone()));
    }
    let record = match load.status(&package.identity) {
        TrustStatus::Unrecorded => return blocked(summary, BlockReason::NeedsPermission),
        TrustStatus::ContentChanged => return blocked(summary, BlockReason::Conflict),
        TrustStatus::Reclassified(record) => {
            summary.provenance = Some(record.provenance.clone());
            summary.declared = record.declared.clone();
            let reason = if record.grant == Grant::Denied {
                BlockReason::Denied
            } else {
                BlockReason::Stale("the capability classification changed".to_string())
            };
            return blocked(summary, reason);
        }
        TrustStatus::Recorded(record) => record,
    };
    summary.provenance = Some(record.provenance.clone());
    summary.declared = record.declared.clone();
    // 6: the grant.
    if record.grant == Grant::Denied {
        return blocked(summary, BlockReason::Denied);
    }
    let within_record = inventory.capabilities.is_subset(&record.detected)
        && inventory.restricted.is_subset(&record.restricted)
        && inventory.unclassified.is_subset(&record.unclassified);
    if !within_record {
        return blocked(
            summary,
            BlockReason::Stale("it uses capabilities that were not reviewed".to_string()),
        );
    }
    match record.grant {
        Grant::Granted => {
            let approved = record.approved.clone().unwrap_or_default();
            if inventory.capabilities.is_subset(&approved) {
                // Exactly what was approved, plus what the restricted node
                // types listed in the reviewed record exercise.
                let rights = approved
                    .iter()
                    .copied()
                    .collect::<crate::capability::CapSet>()
                    .union(
                        crate::library::marketplace::scan::restricted_runtime_capabilities(
                            &record.restricted,
                        ),
                    );
                Ok(authorized(summary, AuthorizedBy::Grant, rights))
            } else {
                blocked(
                    summary,
                    BlockReason::Stale("it uses capabilities that were not approved".to_string()),
                )
            }
        }
        Grant::NotGranted if record.provenance == Provenance::LocalPublish => Ok(authorized(
            summary,
            AuthorizedBy::LocalAuthor,
            crate::capability::CapSet::ALL,
        )),
        Grant::NotGranted => blocked(summary, BlockReason::NeedsPermission),
        Grant::Denied => unreachable!("handled above"),
    }
}

/// Authorize the package whose entry is `entry`, then every package it
/// depends on (resolved exactly as its runtime will resolve them). Nothing
/// is constructed. The parent is blocked when any dependency is.
pub fn authorize_entry(
    policy: &TrustPolicy,
    entry: &Path,
    resolver: &PackageResolver,
) -> Result<Authorized, Blocked> {
    let Some(dir) = package_dir_for_entry(entry) else {
        let dir = entry.parent().unwrap_or(entry);
        return Err(Blocked {
            package: PackageSummary::unidentified(dir),
            reason: BlockReason::Unidentifiable(
                "the file is not the entry of a package version".to_string(),
            ),
        });
    };
    let authorized = authorize_dir(policy, &dir)?;
    let mut visited = HashSet::from([dir]);
    check_dependencies(
        policy,
        &authorized.entry_text,
        entry,
        resolver,
        1,
        &mut visited,
    )
    .map_err(|dependency| Blocked {
        package: authorized.summary.clone(),
        reason: BlockReason::Dependency(Box::new(dependency)),
    })?;
    Ok(authorized)
}

fn check_dependencies(
    policy: &TrustPolicy,
    text: &str,
    entry: &Path,
    resolver: &PackageResolver,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), Blocked> {
    let here = entry.parent().unwrap_or(Path::new("."));
    let fail = |why: String| Blocked {
        package: PackageSummary::unidentified(here),
        reason: BlockReason::Unidentifiable(why),
    };
    if depth > MAX_DEPTH {
        return Err(fail(format!(
            "custom node nesting exceeds maximum depth of {MAX_DEPTH}"
        )));
    }
    let raw = crate::document::read_custom_node_static_text(text).map_err(&fail)?;
    // Nested instances resolve against the outermost package containing
    // this entry (its bundled deps/ first), exactly like the runtime.
    let mut child = resolver.clone();
    child.set_bundle(crate::library::package::containing_package_root(entry));
    for instance in raw.instances {
        let path = match &instance.package {
            Some(stored) => child
                .resolve_instance(&instance.file, Some(stored))
                .map_err(|e| fail(e.to_string()))?,
            None => {
                let file = Path::new(&instance.file);
                if file.is_absolute() {
                    file.to_path_buf()
                } else {
                    here.join(file)
                }
            }
        };
        if instance.package.is_none()
            && !crate::library::package::is_inside_published_package(&path)
        {
            // A plain custom-node file outside any package (not a package
            // runtime; checked by the custom-node recursion rules).
            continue;
        }
        let Some(dir) = package_dir_for_entry(&path) else {
            return Err(fail(format!(
                "{} is not the entry of a package version",
                path.display()
            )));
        };
        if !visited.insert(dir.clone()) {
            continue;
        }
        let dependency = authorize_dir(policy, &dir)?;
        check_dependencies(
            policy,
            &dependency.entry_text,
            &path,
            resolver,
            depth + 1,
            visited,
        )
        .map_err(|inner| Blocked {
            package: dependency.summary.clone(),
            reason: BlockReason::Dependency(Box::new(inner)),
        })?;
    }
    Ok(())
}

// ── Loose custom-node files (P4.3.2E1) ──────────────────────────────────

#[cfg(test)]
thread_local! {
    /// Loose custom-node files read for authorization on this thread.
    pub(crate) static LOOSE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Why a loose custom node did not get a runtime.
#[derive(Debug, Clone)]
pub enum LooseRefusal {
    /// Not allowed to run.
    Blocked(Blocked),
    /// Unreadable or not a custom node (an ordinary load failure).
    Failed(String),
}

/// What [`authorize_loose`] allowed: the content digest, the exact text
/// that was checked (the runtime is built from it) and its own rights
/// (P4.3.2E2: everything when authored here, else what the user approved).
pub type LooseAuthorized = (String, String, crate::capability::CapSet);

/// Authorize a loose `.pwnode` (outside any package): its content must be
/// recorded as authored on this computer or allowed by the user in the
/// content ledger.
pub fn authorize_loose(policy: &TrustPolicy, path: &Path) -> Result<LooseAuthorized, LooseRefusal> {
    use super::content::{ContentKind, ContentState, custom_node_digest};
    #[cfg(test)]
    LOOSE_READS.with(|count| count.set(count.get() + 1));
    let text = std::fs::read_to_string(path)
        .map_err(|e| LooseRefusal::Failed(format!("Failed to read custom node: {e}")))?;
    let digest = custom_node_digest(&text)
        .map_err(|e| LooseRefusal::Failed(format!("Failed to parse custom node: {e}")))?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let blocked = |reason: BlockReason| {
        LooseRefusal::Blocked(Blocked {
            package: PackageSummary {
                dir: path.to_path_buf(),
                name: name.clone(),
                label: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                identity: None,
                provenance: None,
                declared: None,
                detected: inventory_files([(path.to_string_lossy().as_ref(), text.as_str())])
                    .map(|i| i.capabilities)
                    .unwrap_or_default(),
                restricted: BTreeSet::new(),
                unclassified: BTreeSet::new(),
                loose_digest: Some(digest.clone()),
            },
            reason,
        })
    };
    let store = match policy {
        TrustPolicy::Unconfigured => return Err(blocked(BlockReason::Unconfigured)),
        TrustPolicy::Store(store) => store,
        #[cfg(test)]
        TrustPolicy::AllowAll => return Ok((digest, text, crate::capability::CapSet::ALL)),
    };
    match store
        .content_ledger()
        .classify(ContentKind::CustomNode, &digest)
    {
        state @ (ContentState::Authored | ContentState::Allowed(_)) => {
            Ok((digest, text, state.capabilities()))
        }
        ContentState::Denied { .. } => Err(blocked(BlockReason::Denied)),
        ContentState::Unverified {
            note: Some(why), ..
        } => Err(blocked(BlockReason::StoreDamaged(why))),
        _ => Err(blocked(BlockReason::NeedsPermission)),
    }
}
