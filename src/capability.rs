//! P4.3.2E2: capability enforcement at privileged operation boundaries.
//!
//! P4.3.2D/E1 decide *whether* a graph may run. This module decides *what*
//! a running graph may do. Every privileged operation (files, network,
//! devices, capture, other programs, system information, AI credentials,
//! script engines, native code) calls [`check`] at the operation boundary,
//! immediately before it acts. The check reads the **capability context**
//! in force on the calling thread:
//!
//! * the editor enters the context of the open document for the whole
//!   frame (`Authored` content: everything, the local-author policy;
//!   `Allowed` content: exactly the capabilities the user approved;
//!   anything else: nothing);
//! * a custom-node runtime enters its own context while it evaluates:
//!   its own approval (package grant, local-author policy, or the content
//!   ledger) **intersected** with the context of the graph that contains
//!   it, so a child never exceeds its parent and a parent's trust never
//!   lends a child more than the child was approved for;
//! * a worker thread started with [`spawn`] keeps the context of the code
//!   that started it, and checks it again before every operation;
//! * node construction (deserializing, cloning, pasting, undo) runs with
//!   an empty context, so initialization can never act on another
//!   document's rights.
//!
//! There is no implicit default: a thread without a context is denied
//! everything (fail closed). A context is **revoked** when its graph is
//! dropped or its grant shrinks (a revoked permission, a package that lost
//! its grant, a runtime rebuilt under a new decision); a revoked context
//! denies everything, so workers that captured it stop initiating
//! privileged operations at their next check.
//!
//! **Limits.** Checks are cooperative: they are calls placed at every
//! operation boundary found by the P4.3.2E2 audit (see
//! `docs/marketplace/architecture.md`), not an operating-system sandbox.
//! Native code (CLAP / Rust plugins) can do anything once loaded; only
//! loading it is gated.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

pub use crate::library::marketplace::Capability;

// ── Capability sets ──────────────────────────────────────────────────────

/// A set of capabilities (a bitset over [`Capability`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CapSet(u16);

const fn bit(capability: Capability) -> u16 {
    1 << (capability as u16)
}

impl CapSet {
    pub const EMPTY: CapSet = CapSet(0);
    pub const ALL: CapSet = CapSet((1 << Capability::ALL.len()) - 1);

    pub const fn of(capabilities: &[Capability]) -> CapSet {
        let mut bits = 0;
        let mut i = 0;
        while i < capabilities.len() {
            bits |= bit(capabilities[i]);
            i += 1;
        }
        CapSet(bits)
    }

    pub fn contains(self, capability: Capability) -> bool {
        self.0 & bit(capability) != 0
    }

    pub fn with(self, capability: Capability) -> CapSet {
        CapSet(self.0 | bit(capability))
    }

    pub fn union(self, other: CapSet) -> CapSet {
        CapSet(self.0 | other.0)
    }

    pub fn intersect(self, other: CapSet) -> CapSet {
        CapSet(self.0 & other.0)
    }

    pub fn minus(self, other: CapSet) -> CapSet {
        CapSet(self.0 & !other.0)
    }

    pub fn is_subset(self, other: CapSet) -> bool {
        self.0 & !other.0 == 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = Capability> {
        Capability::ALL
            .into_iter()
            .filter(move |c| self.contains(*c))
    }

    /// Stable names, as stored in trust records.
    pub fn names(self) -> Vec<String> {
        self.iter().map(|c| c.as_str().to_string()).collect()
    }

    /// Parse stored names; an unknown name is an error (never ignored).
    pub fn parse_names(names: &[String]) -> Result<CapSet, String> {
        names.iter().try_fold(CapSet::EMPTY, |set, name| {
            Capability::parse(name)
                .map(|c| set.with(c))
                .ok_or_else(|| format!("unknown capability '{name}'"))
        })
    }

    /// A readable list, e.g. "network access, files on this computer".
    pub fn describe(self) -> String {
        if self.is_empty() {
            return "none".to_string();
        }
        self.iter().map(describe).collect::<Vec<_>>().join(", ")
    }
}

impl FromIterator<Capability> for CapSet {
    fn from_iter<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        iter.into_iter().fold(CapSet::EMPTY, CapSet::with)
    }
}

impl std::fmt::Debug for CapSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set()
            .entries(self.iter().map(Capability::as_str))
            .finish()
    }
}

/// What a capability allows, for messages.
pub fn describe(capability: Capability) -> &'static str {
    match capability {
        Capability::Filesystem => "files on this computer",
        Capability::Network => "network access",
        Capability::Devices => "hardware devices",
        Capability::MediaCapture => "the camera or microphone",
        Capability::Scripting => "running scripts or shaders",
        Capability::Process => "running other programs",
        Capability::SystemInfo => "system information",
        Capability::AiCredentials => "your AI service keys",
        Capability::NativeCode => "native code plugins",
    }
}

// ── Contexts ─────────────────────────────────────────────────────────────

const COUNTERS: usize = Capability::ALL.len();

struct Inner {
    granted: CapSet,
    /// Whose rights these are ("this project", "package x@1.0.0#…").
    label: String,
    revoked: AtomicBool,
    /// Operations checked against this context (for tests and diagnostics).
    allowed: [AtomicU32; COUNTERS],
    denied: [AtomicU32; COUNTERS],
}

/// The rights of one running graph. Cheap to clone (shared).
#[derive(Clone)]
pub struct Context(Arc<Inner>);

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("label", &self.0.label)
            .field("granted", &self.0.granted)
            .field("revoked", &self.is_revoked())
            .finish()
    }
}

impl Context {
    pub fn new(granted: CapSet, label: impl Into<String>) -> Context {
        Context(Arc::new(Inner {
            granted,
            label: label.into(),
            revoked: AtomicBool::new(false),
            allowed: Default::default(),
            denied: Default::default(),
        }))
    }

    /// A context that allows nothing.
    pub fn none(label: impl Into<String>) -> Context {
        Context::new(CapSet::EMPTY, label)
    }

    pub fn granted(&self) -> CapSet {
        self.0.granted
    }

    pub fn label(&self) -> &str {
        &self.0.label
    }

    /// What this context allows now (nothing once revoked).
    pub fn effective(&self) -> CapSet {
        if self.is_revoked() {
            CapSet::EMPTY
        } else {
            self.0.granted
        }
    }

    pub fn revoke(&self) {
        self.0.revoked.store(true, Ordering::SeqCst);
    }

    pub fn is_revoked(&self) -> bool {
        self.0.revoked.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub fn same(&self, other: &Context) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Check one operation against this context.
    pub fn check(&self, capability: Capability) -> Result<(), Denied> {
        let index = capability as usize;
        if self.effective().contains(capability) {
            self.0.allowed[index].fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        self.0.denied[index].fetch_add(1, Ordering::Relaxed);
        Err(deny(Denied {
            capability,
            scope: self.0.label.clone(),
            revoked: self.is_revoked(),
        }))
    }

    /// Make this the context of the current thread until the guard drops.
    pub fn enter(&self) -> Entered {
        let previous = CURRENT.with(|current| current.replace(Some(self.clone())));
        Entered { previous }
    }

    /// Operations allowed so far for `capability`.
    #[cfg(test)]
    pub fn allowed_count(&self, capability: Capability) -> u32 {
        self.0.allowed[capability as usize].load(Ordering::Relaxed)
    }

    /// Operations denied so far for `capability`.
    #[cfg(test)]
    pub fn denied_count(&self, capability: Capability) -> u32 {
        self.0.denied[capability as usize].load(Ordering::Relaxed)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

/// Restores the previous context of the thread when dropped.
#[must_use = "the context is left as soon as the guard is dropped"]
pub struct Entered {
    previous: Option<Context>,
}

impl Drop for Entered {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT.with(|current| *current.borrow_mut() = previous);
    }
}

/// The context in force on this thread, if any.
pub fn current() -> Option<Context> {
    CURRENT.with(|current| current.borrow().clone())
}

/// Unit tests written before P4.3.2E2 run graphs without contexts; they
/// keep their behaviour (as with P4.3.2D's test-only allow-all policy).
/// Permission tests switch it off. Production has no such switch.
#[cfg(test)]
fn test_default_allow() -> bool {
    crate::library::trust::authorize::TEST_DEFAULT_ALLOW.with(|allow| allow.get())
}

/// What the current thread may do.
pub fn effective() -> CapSet {
    match current() {
        Some(context) => context.effective(),
        None => {
            #[cfg(test)]
            if test_default_allow() {
                return CapSet::ALL;
            }
            CapSet::EMPTY
        }
    }
}

/// Check one privileged operation, at its boundary, against the context
/// of the current thread. Without a context nothing is allowed.
pub fn check(capability: Capability) -> Result<(), Denied> {
    match current() {
        Some(context) => context.check(capability),
        None => {
            #[cfg(test)]
            if test_default_allow() {
                return Ok(());
            }
            Err(deny(Denied {
                capability,
                scope: "code running outside any permission context".to_string(),
                revoked: false,
            }))
        }
    }
}

/// Check several capabilities (all must be allowed).
pub fn check_all(capabilities: &[Capability]) -> Result<(), Denied> {
    capabilities.iter().try_for_each(|c| check(*c))
}

/// [`check`] on behalf of `node` (a manager acting for a node): a denial
/// is shown on that node.
pub fn check_for(node: crate::graph::NodeId, capability: Capability) -> Result<(), Denied> {
    check(capability).inspect_err(|denied| {
        crate::node_errors::report_for(node, denied.to_string());
    })
}

/// Whether the current thread may still use `capability`, without
/// reporting. For worker loops that stop (and release what they hold)
/// once their context is revoked.
pub fn still_allowed(capability: Capability) -> bool {
    effective().contains(capability)
}

/// Run `f` with no rights at all (node construction, cloning, …).
pub fn without_rights<T>(f: impl FnOnce() -> T) -> T {
    let _entered = Context::none("node construction").enter();
    f()
}

/// Start a worker thread that keeps the caller's context. Every privileged
/// operation the worker performs is checked against it, so revoking it
/// stops the worker's privileged operations.
pub fn spawn<F, T>(f: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(bind(f))
}

/// Like [`spawn`], with a thread name.
pub fn spawn_named<F, T>(name: &str, f: F) -> std::io::Result<std::thread::JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(bind(f))
}

/// Wrap `f` so that it runs under the caller's context, wherever it runs.
pub fn bind<F, T>(f: F) -> impl FnOnce() -> T + Send + 'static
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let context = current();
    #[cfg(test)]
    let allow = test_default_allow();
    move || {
        #[cfg(test)]
        crate::library::trust::authorize::TEST_DEFAULT_ALLOW.with(|a| a.set(allow));
        let _entered = context.as_ref().map(Context::enter);
        f()
    }
}

// ── Denials ──────────────────────────────────────────────────────────────

/// A refused operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied {
    pub capability: Capability,
    pub scope: String,
    pub revoked: bool,
}

impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.revoked {
            write!(
                f,
                "Not permitted: {} (permission was withdrawn)",
                describe(self.capability)
            )
        } else {
            write!(
                f,
                "Not permitted: {} (not approved for {})",
                describe(self.capability),
                self.scope
            )
        }
    }
}

impl From<Denied> for String {
    fn from(denied: Denied) -> String {
        denied.to_string()
    }
}

/// Most recent distinct denials, for the Permissions window.
static RECENT: Mutex<VecDeque<Denied>> = Mutex::new(VecDeque::new());
/// Denials already written to the system log (once per scope and kind).
static LOGGED: Mutex<Option<HashSet<(String, Capability)>>> = Mutex::new(None);
const RECENT_MAX: usize = 32;
const LOGGED_MAX: usize = 1024;

fn deny(denied: Denied) -> Denied {
    crate::node_errors::report(denied.to_string());
    if let Ok(mut recent) = RECENT.lock() {
        if !recent.contains(&denied) {
            if recent.len() >= RECENT_MAX {
                recent.pop_front();
            }
            recent.push_back(denied.clone());
        }
    }
    let first = LOGGED
        .lock()
        .map(|mut logged| {
            let logged = logged.get_or_insert_with(HashSet::new);
            logged.len() < LOGGED_MAX && logged.insert((denied.scope.clone(), denied.capability))
        })
        .unwrap_or(false);
    if first {
        crate::system_log::warn(format!("Permissions: {denied}."));
    }
    denied
}

/// Recently refused operations (newest last).
pub fn recent_denials() -> Vec<Denied> {
    RECENT
        .lock()
        .map(|recent| recent.iter().cloned().collect())
        .unwrap_or_default()
}

pub fn clear_recent_denials() {
    if let Ok(mut recent) = RECENT.lock() {
        recent.clear();
    }
}

// ── The context owned by a graph ─────────────────────────────────────────

/// The context of one graph (`Graph::capabilities`). A clone of a graph
/// (undo snapshot, clipboard) starts without one; dropping the graph
/// revokes it.
#[derive(Default)]
pub struct Scope {
    context: Option<Context>,
}

impl Clone for Scope {
    fn clone(&self) -> Self {
        Scope::default()
    }
}

impl std::fmt::Debug for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.context.fmt(f)
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        self.revoke();
    }
}

impl Scope {
    /// The context granting `granted`, reused while unchanged. When the
    /// grant changes a new context replaces it, and the old one is revoked
    /// unless the new grant includes everything it allowed.
    pub fn ensure(&mut self, granted: CapSet, label: &str) -> Context {
        if let Some(context) = &self.context {
            if !context.is_revoked() && context.granted() == granted && context.label() == label {
                return context.clone();
            }
            if !context.granted().is_subset(granted) || context.label() != label {
                context.revoke();
            }
        }
        let context = Context::new(granted, label);
        self.context = Some(context.clone());
        context
    }

    pub fn context(&self) -> Option<&Context> {
        self.context.as_ref()
    }

    pub fn revoke(&mut self) {
        if let Some(context) = self.context.take() {
            context.revoke();
        }
    }
}

// ── The contexts of the open documents ───────────────────────────────────

/// The context of each open document, by document id. A document keeps
/// its context while it is open (undo, redo and tab switches keep it);
/// closing the document revokes it, by identity, whether or not the other
/// open documents were ever activated.
#[derive(Default)]
pub struct DocumentScopes {
    scopes: std::collections::HashMap<u64, Scope>,
}

impl DocumentScopes {
    /// The context of document `id`, granting `granted` (see
    /// [`Scope::ensure`]).
    pub fn rights(&mut self, id: u64, granted: CapSet, label: &str) -> Context {
        self.scopes.entry(id).or_default().ensure(granted, label)
    }

    /// Revoke the context of every document not in `open` (closed ones).
    /// Returns how many were revoked.
    pub fn retain_open(&mut self, open: impl IntoIterator<Item = u64>) -> usize {
        let open: HashSet<u64> = open.into_iter().collect();
        let before = self.scopes.len();
        // Dropping a scope revokes its context.
        self.scopes.retain(|id, _| open.contains(id));
        before - self.scopes.len()
    }

    /// Bring an existing context in line with the document's current
    /// rights without entering it (a narrower grant revokes the old one).
    /// Documents that never ran have no context and are left alone.
    pub fn refresh(&mut self, id: u64, granted: CapSet, label: &str) {
        if let Some(scope) = self.scopes.get_mut(&id) {
            if scope.context().is_some() {
                scope.ensure(granted, label);
            }
        }
    }

    #[cfg(test)]
    pub fn context(&self, id: u64) -> Option<&Context> {
        self.scopes.get(&id)?.context()
    }
}

#[cfg(test)]
mod tests;
