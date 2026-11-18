//! P4.3.2E1: provenance of projects and loose custom-node files.
//!
//! Packages have exact, immutable identities (P4.3.2C/D). Projects and loose
//! `.pwnode` files are edited all the time, so their trust follows the
//! *content* Patchwork wrote:
//!
//! * **Identity** is a SHA-256 over the canonical JSON of the graph content
//!   (the `graph` of a project or session file, or a whole `.pwnode`), with
//!   objects in sorted key order. File names, folders, timestamps, other
//!   project fields (zoom, pinned nodes) and any provenance claims inside
//!   the file are ignored. Identical content is the same content wherever
//!   it is copied; one changed byte of graph content is different content.
//! * The **content ledger** `~/.patchwork/trust/content.json` (next to the
//!   package trust store, outside every project and the Library) records
//!   per content digest one decision:
//!   - `authored`: written by Patchwork from content that was itself
//!     authored on this computer (a new project, or content opened as
//!     authored);
//!   - `allowed`: the user explicitly allowed it to run, or Patchwork wrote
//!     it from content the user had allowed (its lineage);
//!   - `denied`: the user refused to run it (never replaced automatically).
//! * Writing content that is **not** runnable (unverified or denied)
//!   records nothing: saving an imported project in Patchwork never makes
//!   it trusted.
//! * Content without a record has no established provenance: it was
//!   brought from another computer or source, edited outside Patchwork, or
//!   saved by a Patchwork older than P4.3.2E1. These cases cannot be told
//!   apart; all of them need the user's decision.
//!
//! **Limits.** The ledger is a local record, not a signature: software
//! running as the user can edit it, and a decision only says that this
//! computer's user authored or accepted the content, not that it is safe.
//! It does not cover package runtimes, which keep their own independent
//! authorization; a project-level decision never allows a package or a
//! loose custom node it uses.
//!
//! **P4.3.2E2: approved capabilities.** An `allowed` decision records the
//! capabilities the user approved (format 2). Allowed content runs with
//! exactly those (see `crate::capability`); authored content runs under the
//! local-author policy (every capability). Saved copies inherit the set.
//! Entries written by P4.3.2E1 (format 1) carry no set: their content is
//! allowed to run with no privileged capability until approved again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{is_hex64, now};
use crate::capability::CapSet;

pub const CONTENT_SCHEMA: &str = "patchwork.trust.content";
/// P4.3.2E2: format 2 adds `approved` to `allowed` entries. Format 1 is
/// still read (its `allowed` entries approve no capabilities).
pub const CONTENT_FORMAT: u32 = 2;
pub const CONTENT_FILE: &str = "content.json";
const MAX_LEDGER_BYTES: u64 = 16 * 1024 * 1024;
/// Oldest `authored`/`allowed` entries are dropped beyond this (their
/// content then simply needs a decision again). Denials are kept.
pub const MAX_ENTRIES: usize = 20_000;

/// What a digest identifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    /// The graph of a project (or a restored session).
    Project,
    /// A whole loose `.pwnode` file.
    CustomNode,
}

impl ContentKind {
    fn tag(self) -> &'static [u8] {
        match self {
            ContentKind::Project => b"project",
            ContentKind::CustomNode => b"custom_node",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ContentKind::Project => "project",
            ContentKind::CustomNode => "custom node",
        }
    }
}

/// A recorded decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentDecision {
    Authored,
    /// P4.3.2E2: with the capabilities the user approved.
    Allowed(CapSet),
    Denied,
}

/// How a decision is written in the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DecisionTag {
    Authored,
    Allowed,
    Denied,
}

// ── Identity ─────────────────────────────────────────────────────────────

/// Digest of a JSON value in canonical form (sorted object keys, compact).
pub fn digest_value(kind: ContentKind, value: &serde_json::Value) -> String {
    let canonical = serde_json::to_vec(value).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(b"patchwork-content\0");
    hasher.update(kind.tag());
    hasher.update(b"\0");
    hasher.update(&canonical);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The content digest of a project or session file's text: its `graph`
/// (or the whole value for a bare graph file).
pub fn project_digest(text: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let graph = match value.get("graph") {
        Some(graph) if graph.is_object() => graph,
        _ => &value,
    };
    Ok(digest_value(ContentKind::Project, graph))
}

/// The content digest of a `.pwnode` file's text.
pub fn custom_node_digest(text: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    Ok(digest_value(ContentKind::CustomNode, &value))
}

pub fn digest_text(kind: ContentKind, text: &str) -> Result<String, String> {
    match kind {
        ContentKind::Project => project_digest(text),
        ContentKind::CustomNode => custom_node_digest(text),
    }
}

// ── What an open document's decision is bound to (P4.3.2F) ─────────────

/// P4.3.2F: the exact identity an open document's run decision is bound
/// to, and the decision last applied to it. When a grant or decision
/// changes (a new trust generation), every open document is checked
/// against its basis again, active or not: a denial or revocation stops it
/// at once (its context is revoked), a narrower approval narrows it, and
/// only an explicit grant for exactly this identity lets it run again.
/// Never saved; undo/redo keep the live document's basis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentBasis {
    /// A project or loose custom node, by the content digest it was opened
    /// (or last saved, or decided) as, with the ledger decision seen for it
    /// (`None`: none, or the ledger could not be read).
    Content {
        kind: ContentKind,
        digest: String,
        seen: Option<ContentDecision>,
    },
    /// A published package version open in a tab, by its entry file, with
    /// the rights its authorization gave (`None`: refused).
    Package {
        entry: PathBuf,
        seen: Option<CapSet>,
    },
}

/// The rights a decision grants (nothing when there is none).
fn decision_rights(decision: Option<ContentDecision>) -> CapSet {
    match decision {
        Some(ContentDecision::Authored) => CapSet::ALL,
        Some(ContentDecision::Allowed(approved)) => approved,
        Some(ContentDecision::Denied) | None => CapSet::EMPTY,
    }
}

/// P4.3.2F: an open document's rights after its basis changed from
/// granting `before` to granting `after`: what was withdrawn is removed,
/// what was explicitly added is added, and the rest (e.g. capabilities
/// held back from an MCP edit) stays as it was.
fn follow_rights(current: CapSet, before: CapSet, after: CapSet) -> CapSet {
    current
        .minus(before.minus(after))
        .union(after.minus(before))
}

/// P4.3.2F: apply the ledger's current decision `now` for the content an
/// open document is bound to (`basis`). Returns whether its state changed.
/// `note` explains a missing decision (e.g. a damaged ledger).
///
/// * Denied: the document stops (nodes, wires and editing are kept).
/// * No decision (revoked, or the ledger is unreadable): a document that
///   was only allowed stops and needs a decision again; content authored
///   in this session keeps running (its lineage is this editor's).
/// * Allowed: a running document follows the change (narrowed, or widened
///   by exactly what was explicitly added); a stopped one runs again with
///   the approved set (an explicit grant of exactly this content).
/// * Authored: never restores a stopped document by itself.
pub fn follow_decision(
    state: &mut ContentState,
    basis: &mut ContentBasis,
    now: Option<ContentDecision>,
    note: Option<String>,
) -> bool {
    let ContentBasis::Content { kind, digest, seen } = basis else {
        return false;
    };
    if *seen == now {
        return false;
    }
    let before = std::mem::replace(seen, now);
    let next = match now {
        Some(ContentDecision::Denied) => ContentState::Denied {
            kind: *kind,
            digest: digest.clone(),
        },
        None if *state == ContentState::Authored => return false,
        None => ContentState::Unverified {
            kind: *kind,
            digest: Some(digest.clone()),
            note,
        },
        Some(ContentDecision::Authored) => return false,
        Some(ContentDecision::Allowed(after)) if state.runnable() => {
            let rights = follow_rights(state.capabilities(), decision_rights(before), after);
            if *state == ContentState::Authored && rights == CapSet::ALL {
                return false;
            }
            ContentState::Allowed(rights)
        }
        Some(ContentDecision::Allowed(after)) => ContentState::Allowed(after),
    };
    if *state == next {
        return false;
    }
    *state = next;
    true
}

/// P4.3.2F: the same for a package version open in a tab, given what its
/// authorization gives now (`Err`: refused, with the reason).
pub fn follow_package(
    state: &mut ContentState,
    basis: &mut ContentBasis,
    now: Result<CapSet, String>,
) -> bool {
    let ContentBasis::Package { seen, .. } = basis else {
        return false;
    };
    let after = now.as_ref().ok().copied();
    if *seen == after {
        return false;
    }
    let before = std::mem::replace(seen, after);
    let next = match now {
        Err(why) => ContentState::Unverified {
            kind: ContentKind::CustomNode,
            digest: None,
            note: Some(why),
        },
        Ok(after) if state.runnable() => ContentState::Allowed(follow_rights(
            state.capabilities(),
            before.unwrap_or(CapSet::EMPTY),
            after,
        )),
        Ok(after) => ContentState::Allowed(after),
    };
    if *state == next {
        return false;
    }
    *state = next;
    true
}

// ── State of an open graph ───────────────────────────────────────────────

/// Whether the top-level graph open in the editor may run. Kept in memory
/// with the graph (never saved). Deserialized graphs start `Unverified`;
/// `Graph::new()` (a graph created in this editor) starts `Authored`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentState {
    /// Created here, or opened content recorded as authored. Runs under
    /// the local-author policy (every capability).
    Authored,
    /// Allowed by the user (this content or the content it came from),
    /// with the capabilities the user approved (P4.3.2E2).
    Allowed(CapSet),
    Denied {
        kind: ContentKind,
        digest: String,
    },
    /// Loaded from `digest`; the ledger has not been consulted yet.
    Pending {
        kind: ContentKind,
        digest: String,
    },
    /// No established provenance. `digest` is the content it was opened
    /// from, when known (an allow or deny is then recorded for it).
    Unverified {
        kind: ContentKind,
        digest: Option<String>,
        /// Why (e.g. the ledger cannot be read).
        note: Option<String>,
    },
}

impl Default for ContentState {
    fn default() -> Self {
        ContentState::Unverified {
            kind: ContentKind::Project,
            digest: None,
            note: None,
        }
    }
}

impl ContentState {
    /// Content loaded from `text` (Pending), or Unverified if it cannot be
    /// identified.
    pub fn loaded(kind: ContentKind, text: &str) -> Self {
        match digest_text(kind, text) {
            Ok(digest) => ContentState::Pending { kind, digest },
            Err(_) => ContentState::Unverified {
                kind,
                digest: None,
                note: None,
            },
        }
    }

    pub fn runnable(&self) -> bool {
        matches!(self, ContentState::Authored | ContentState::Allowed(_))
    }

    /// P4.3.2E2: what this content may do when it runs.
    pub fn capabilities(&self) -> CapSet {
        match self {
            ContentState::Authored => CapSet::ALL,
            ContentState::Allowed(approved) => *approved,
            _ => CapSet::EMPTY,
        }
    }

    /// The decision recorded for content Patchwork writes from this state.
    pub fn lineage(&self) -> Option<ContentDecision> {
        match self {
            ContentState::Authored => Some(ContentDecision::Authored),
            ContentState::Allowed(approved) => Some(ContentDecision::Allowed(*approved)),
            _ => None,
        }
    }

    /// After Patchwork wrote this graph's content as `digest`.
    pub fn after_write(&mut self, kind: ContentKind, digest: String) {
        match self {
            ContentState::Authored | ContentState::Allowed(_) => {}
            ContentState::Denied { .. } => {
                *self = ContentState::Unverified {
                    kind,
                    digest: Some(digest),
                    note: None,
                }
            }
            ContentState::Pending { .. } | ContentState::Unverified { .. } => {
                *self = ContentState::Unverified {
                    kind,
                    digest: Some(digest),
                    note: None,
                }
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            ContentState::Authored => "Created on this computer".to_string(),
            ContentState::Allowed(approved) if *approved == CapSet::ALL => {
                "Allowed by you".to_string()
            }
            ContentState::Allowed(approved) => {
                format!("Allowed by you (may use: {})", approved.describe())
            }
            ContentState::Denied { .. } => "Denied".to_string(),
            ContentState::Pending { .. } => "Checking…".to_string(),
            ContentState::Unverified {
                note: Some(note), ..
            } => {
                format!("Origin not verified ({note})")
            }
            ContentState::Unverified { .. } => "Origin not verified".to_string(),
        }
    }
}

// ── The ledger file ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryFile {
    kind: ContentKind,
    sha256: String,
    decision: DecisionTag,
    /// P4.3.2E2 (format 2): the approved capabilities of `allowed` entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approved: Option<Vec<String>>,
    recorded_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerFile {
    schema: String,
    format: u32,
    entries: Vec<EntryFile>,
}

/// Validated ledger contents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    entries: BTreeMap<(ContentKind, String), (ContentDecision, u64)>,
}

impl Ledger {
    pub fn decision(&self, kind: ContentKind, digest: &str) -> Option<ContentDecision> {
        self.entries
            .get(&(kind, digest.to_string()))
            .map(|(decision, _)| *decision)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let file: LedgerFile =
            serde_json::from_slice(bytes).map_err(|e| format!("it is not valid: {e}"))?;
        if file.schema != CONTENT_SCHEMA {
            return Err("it is not a Patchwork content ledger".to_string());
        }
        if file.format != CONTENT_FORMAT && file.format != 1 {
            return Err(format!(
                "it uses format {}, which this Patchwork does not read",
                file.format
            ));
        }
        if file.entries.len() > MAX_ENTRIES * 2 {
            return Err("it has too many entries".to_string());
        }
        let mut ledger = Ledger::default();
        for entry in file.entries {
            if !is_hex64(&entry.sha256) {
                return Err("an entry has a malformed digest".to_string());
            }
            let decision = match (entry.decision, entry.approved, file.format) {
                (DecisionTag::Authored, None, _) => ContentDecision::Authored,
                (DecisionTag::Denied, None, _) => ContentDecision::Denied,
                // P4.3.2E1 entries: allowed to run, no capability approved.
                (DecisionTag::Allowed, None, 1) => ContentDecision::Allowed(CapSet::EMPTY),
                (DecisionTag::Allowed, Some(names), 2) => ContentDecision::Allowed(
                    CapSet::parse_names(&names).map_err(|e| format!("an entry has {e}"))?,
                ),
                (DecisionTag::Allowed, None, _) => {
                    return Err("an allowed entry has no approved capabilities".to_string());
                }
                (_, Some(_), _) => {
                    return Err("an entry approves capabilities it cannot have".to_string());
                }
            };
            if ledger
                .entries
                .insert((entry.kind, entry.sha256), (decision, entry.recorded_at))
                .is_some()
            {
                return Err("it lists a content digest twice".to_string());
            }
        }
        Ok(ledger)
    }

    fn to_json(&self) -> Result<Vec<u8>, String> {
        let file = LedgerFile {
            schema: CONTENT_SCHEMA.to_string(),
            format: CONTENT_FORMAT,
            entries: self
                .entries
                .iter()
                .map(|((kind, sha256), (decision, recorded_at))| {
                    let (decision, approved) = match decision {
                        ContentDecision::Authored => (DecisionTag::Authored, None),
                        ContentDecision::Allowed(set) => (DecisionTag::Allowed, Some(set.names())),
                        ContentDecision::Denied => (DecisionTag::Denied, None),
                    };
                    EntryFile {
                        kind: *kind,
                        sha256: sha256.clone(),
                        decision,
                        approved,
                        recorded_at: *recorded_at,
                    }
                })
                .collect(),
        };
        serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())
    }

    /// Drop the oldest non-denied entries beyond [`MAX_ENTRIES`].
    fn prune(&mut self) {
        if self.entries.len() <= MAX_ENTRIES {
            return;
        }
        let mut removable: Vec<((ContentKind, String), u64)> = self
            .entries
            .iter()
            .filter(|(_, (decision, _))| *decision != ContentDecision::Denied)
            .map(|(key, (_, at))| (key.clone(), *at))
            .collect();
        removable.sort_by_key(|(_, at)| *at);
        let excess = self.entries.len() - MAX_ENTRIES;
        for (key, _) in removable.into_iter().take(excess) {
            self.entries.remove(&key);
        }
    }
}

/// Result of reading the ledger.
/// P4.3.2F: one recorded decision, as listed in the Permissions window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    pub kind: ContentKind,
    pub digest: String,
    pub decision: ContentDecision,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerLoad {
    Missing,
    Loaded(Ledger),
    /// Unusable: nothing counts as authored or allowed, and it is never
    /// overwritten automatically.
    Damaged(String),
}

/// The content ledger file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentLedger {
    path: PathBuf,
}

impl ContentLedger {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> LedgerLoad {
        let meta = match std::fs::symlink_metadata(&self.path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LedgerLoad::Missing,
            Err(e) => return LedgerLoad::Damaged(format!("cannot be read: {e}")),
        };
        if !meta.is_file() || meta.len() > MAX_LEDGER_BYTES {
            return LedgerLoad::Damaged("it is not a regular file of a sane size".to_string());
        }
        match std::fs::read(&self.path) {
            Ok(bytes) => match Ledger::parse(&bytes) {
                Ok(ledger) => LedgerLoad::Loaded(ledger),
                Err(why) => LedgerLoad::Damaged(format!("the content ledger is damaged: {why}")),
            },
            Err(e) => LedgerLoad::Damaged(format!("cannot be read: {e}")),
        }
    }

    /// The state of content just loaded as `digest`.
    /// P4.3.2F: the recorded decision for exactly this content (`Err`: the
    /// ledger cannot be read).
    pub fn decision(
        &self,
        kind: ContentKind,
        digest: &str,
    ) -> Result<Option<ContentDecision>, String> {
        match self.load() {
            LedgerLoad::Missing => Ok(None),
            LedgerLoad::Loaded(ledger) => Ok(ledger.decision(kind, digest)),
            LedgerLoad::Damaged(why) => Err(why),
        }
    }

    pub fn classify(&self, kind: ContentKind, digest: &str) -> ContentState {
        let unverified = |note: Option<String>| ContentState::Unverified {
            kind,
            digest: Some(digest.to_string()),
            note,
        };
        match self.load() {
            LedgerLoad::Missing => unverified(None),
            LedgerLoad::Damaged(why) => unverified(Some(why)),
            LedgerLoad::Loaded(ledger) => match ledger.decision(kind, digest) {
                Some(ContentDecision::Authored) => ContentState::Authored,
                Some(ContentDecision::Allowed(approved)) => ContentState::Allowed(approved),
                Some(ContentDecision::Denied) => ContentState::Denied {
                    kind,
                    digest: digest.to_string(),
                },
                None => unverified(None),
            },
        }
    }

    /// Record a decision. Automatic records (`authored`, `allowed` lineage
    /// on save) never replace a denial; `explicit` user decisions do.
    pub fn record(
        &self,
        kind: ContentKind,
        digest: &str,
        decision: ContentDecision,
        explicit: bool,
    ) -> Result<(), String> {
        if !is_hex64(digest) {
            return Err("malformed content digest".to_string());
        }
        let _guard = super::STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut ledger = match self.load() {
            LedgerLoad::Missing => Ledger::default(),
            LedgerLoad::Loaded(ledger) => ledger,
            LedgerLoad::Damaged(why) => return Err(format!("{why}. Nothing was recorded.")),
        };
        let key = (kind, digest.to_string());
        if !explicit && ledger.decision(kind, digest) == Some(ContentDecision::Denied) {
            return Ok(());
        }
        ledger.entries.insert(key, (decision, now()));
        ledger.prune();
        super::write_atomically(&self.path, &ledger.to_json()?)
    }

    /// The user decided about the file at `path` after reviewing content
    /// `reviewed`: the file is read again and must still be that content.
    /// Allowing approves exactly the capabilities that content uses
    /// (P4.3.2E2).
    /// Read, change and atomically rewrite the ledger. Refuses to touch a
    /// damaged ledger (it is never written over).
    fn update<T>(
        &self,
        change: impl FnOnce(&mut Ledger) -> Result<T, String>,
    ) -> Result<T, String> {
        let _guard = super::STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut ledger = match self.load() {
            LedgerLoad::Missing => Ledger::default(),
            LedgerLoad::Loaded(ledger) => ledger,
            LedgerLoad::Damaged(why) => return Err(format!("{why}. Nothing was changed.")),
        };
        let value = change(&mut ledger)?;
        super::write_atomically(&self.path, &ledger.to_json()?)?;
        Ok(value)
    }

    /// P4.3.2F: every recorded decision, newest first (`Err`: damaged).
    pub fn entries(&self) -> Result<Vec<LedgerEntry>, String> {
        let ledger = match self.load() {
            LedgerLoad::Missing => return Ok(Vec::new()),
            LedgerLoad::Loaded(ledger) => ledger,
            LedgerLoad::Damaged(why) => return Err(why),
        };
        let mut entries: Vec<LedgerEntry> = ledger
            .entries
            .into_iter()
            .map(|((kind, digest), (decision, recorded_at))| LedgerEntry {
                kind,
                digest,
                decision,
                recorded_at,
            })
            .collect();
        entries.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
        Ok(entries)
    }

    /// P4.3.2F: withdraw the decision for exactly this content. It then
    /// needs a decision again (open documents bound to it stop at once,
    /// unless authored in this session).
    pub fn revoke(&self, kind: ContentKind, digest: &str) -> Result<(), String> {
        self.update(|ledger| {
            ledger
                .entries
                .remove(&(kind, digest.to_string()))
                .map(|_| ())
                .ok_or_else(|| "Nothing is recorded for this content.".to_string())
        })
    }

    /// P4.3.2F: change what exactly this content may do. Narrowing an
    /// approval needs nothing more; approving any capability it did not
    /// have (including allowing denied content again) needs the user's
    /// explicit acknowledgement. Never widens anything by itself.
    pub fn set_approved(
        &self,
        kind: ContentKind,
        digest: &str,
        approved: CapSet,
        acknowledged: bool,
    ) -> Result<ContentDecision, String> {
        self.update(|ledger| {
            let key = (kind, digest.to_string());
            let current = ledger
                .entries
                .get(&key)
                .map(|(decision, _)| *decision)
                .ok_or_else(|| "Nothing is recorded for this content.".to_string())?;
            if !approved.is_subset(decision_rights(Some(current))) && !acknowledged {
                return Err(
                    "Allowing more than before needs your explicit acknowledgement.".to_string(),
                );
            }
            let decision = ContentDecision::Allowed(approved);
            ledger.entries.insert(key, (decision, now()));
            Ok(decision)
        })
    }

    /// P4.3.2F: move a damaged ledger aside (kept for inspection) so a new,
    /// empty one is used. Nothing it recorded is trusted again: every
    /// project and custom node it covered needs a decision again. Refuses
    /// when the ledger is readable.
    pub fn move_damaged_aside(&self) -> Result<PathBuf, String> {
        let _guard = super::STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(self.load(), LedgerLoad::Damaged(_)) {
            return Err("The content ledger is not damaged.".to_string());
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let aside = self
            .path
            .with_file_name(format!("content.damaged-{nanos}.json"));
        std::fs::rename(&self.path, &aside)
            .map_err(|e| format!("Cannot move the damaged content ledger: {e}"))?;
        super::bump_generation();
        Ok(aside)
    }

    pub fn decide_file(
        &self,
        kind: ContentKind,
        path: &Path,
        reviewed: &str,
        allow: bool,
    ) -> Result<(), String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        if digest_text(kind, &text)? != reviewed {
            return Err(format!(
                "{} changed after it was reviewed; review it again.",
                path.display()
            ));
        }
        let decision = if allow {
            ContentDecision::Allowed(
                crate::library::marketplace::scan::runtime_capabilities_of_text(
                    &path.to_string_lossy(),
                    &text,
                )?,
            )
        } else {
            ContentDecision::Denied
        };
        self.record(kind, reviewed, decision, true)
    }
}

#[cfg(test)]
mod tests;
