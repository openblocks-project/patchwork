//! P6 Stage 2.1: the Player's run decision and first-run consent.
//!
//! One decision for every Player launch (`--player` and exported apps),
//! taken **before** the frame that would evaluate the graph:
//!
//! - **Run**: the project is authored or allowed on this computer (its
//!   exact content), it is the content that was exported (or the user has
//!   acknowledged the change), and no package it uses is waiting for a
//!   decision.
//! - **Consent**: something needs the user's explicit approval: the
//!   project itself (not yet allowed here), a change since export, or
//!   packages it uses. Nothing runs until the user ticks "I trust this
//!   content and allow it to run" and presses **Allow to run**; the
//!   approval is then recorded with Patchwork's existing trust mechanisms
//!   (the project's exact content and the capabilities it uses; each
//!   package separately, by its own content).
//! - **Refused**: the project was denied here. It does not run, and the
//!   Player does not offer to change that (the editor's Permissions window
//!   does).
//!
//! Trust is only ever read from this computer's records. Nothing here
//! looks at files in the project or export folder for decisions.

use crate::capability::CapSet;
use crate::library::trust::authorize::{BlockReason, Blocked};
use crate::library::trust::content::ContentState;

/// What the Player does this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Run,
    Consent(ConsentRequest),
    Refused { title: String, detail: String },
}

/// Everything the user is asked to approve, as shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentRequest {
    pub project_name: String,
    /// The content identity (digest of the project's graph), when known.
    pub content_digest: Option<String>,
    /// The digest recorded at export, when it differs from the content.
    pub changed_from: Option<String>,
    /// The project itself needs to be allowed (it is not yet allowed here).
    pub project_decision: bool,
    /// What the project's own nodes use (what allowing it approves).
    pub permissions: CapSet,
    /// Packages / custom-node files waiting for a decision (each approved
    /// separately, by its own content).
    pub packages: Vec<Blocked>,
    /// Packages that cannot run and cannot be allowed here (denied, damaged
    /// records, unidentifiable), shown so the user knows.
    pub unavailable: Vec<Blocked>,
}

/// The facts the decision is taken from.
#[derive(Debug, Clone)]
pub struct GateInput<'a> {
    pub project_name: &'a str,
    /// The project's content state, resolved against this computer's ledger.
    pub content: &'a ContentState,
    /// The digest the project was loaded as.
    pub digest: Option<&'a str>,
    /// The digest recorded in the manifest (exported apps).
    pub expected: Option<&'a str>,
    /// The user approved running despite a change since export.
    pub change_acknowledged: bool,
    /// What the project's nodes use.
    pub uses: CapSet,
    /// Refusals found by authorizing the project's custom nodes (without
    /// loading them).
    pub blocked: &'a [Blocked],
}

/// One entry per package content (innermost blocked package).
fn unique(blocked: &[Blocked]) -> Vec<Blocked> {
    let mut out: Vec<Blocked> = Vec::new();
    for b in blocked {
        let inner = b.innermost().clone();
        let key = package_key(&inner);
        if !out.iter().any(|o| package_key(o) == key) {
            out.push(inner);
        }
    }
    out
}

fn package_key(b: &Blocked) -> (std::path::PathBuf, Option<String>) {
    (
        b.package.dir.clone(),
        b.package
            .identity
            .as_ref()
            .map(|i| i.tree_sha256.clone())
            .or_else(|| b.package.loose_digest.clone()),
    )
}

/// Decide what the Player may do. Pure: reads nothing, writes nothing.
pub fn decide(input: &GateInput<'_>) -> Gate {
    if let ContentState::Denied { .. } = input.content {
        return Gate::Refused {
            title: "This project was denied on this computer".to_string(),
            detail: "It will not run. To change this decision, open it in Patchwork \
                     (Permissions)."
                .to_string(),
        };
    }
    let changed_from = match (input.digest, input.expected) {
        (Some(actual), Some(expected)) if actual != expected => Some(expected.to_string()),
        (None, Some(expected)) => Some(expected.to_string()),
        _ => None,
    };
    let packages: Vec<Blocked> = unique(input.blocked)
        .into_iter()
        .filter(Blocked::needs_review)
        .collect();
    let unavailable: Vec<Blocked> = unique(input.blocked)
        .into_iter()
        .filter(|b| !b.needs_review())
        .collect();
    let project_decision = !input.content.runnable();
    let change_pending = changed_from.is_some() && !input.change_acknowledged;
    if !project_decision && !change_pending && packages.is_empty() {
        return Gate::Run;
    }
    if project_decision && input.digest.is_none() {
        // Without an identity no decision could be recorded for it.
        return Gate::Refused {
            title: "This project cannot be identified".to_string(),
            detail: "Its content could not be read as a project, so it cannot be \
                     allowed to run."
                .to_string(),
        };
    }
    Gate::Consent(ConsentRequest {
        project_name: input.project_name.to_string(),
        content_digest: input.digest.map(str::to_string),
        changed_from,
        project_decision,
        permissions: input.uses,
        packages,
        unavailable,
    })
}

/// Run `evaluate` only when the gate says so. The Player's frame goes
/// through this: unauthorized content never reaches graph evaluation.
pub fn run_if_permitted<T>(gate: &Gate, evaluate: impl FnOnce() -> T) -> Option<T> {
    match gate {
        Gate::Run => Some(evaluate()),
        Gate::Consent(_) | Gate::Refused { .. } => None,
    }
}

/// How a package's state is described on the consent screen.
pub fn package_reason(blocked: &Blocked) -> String {
    match &blocked.reason {
        BlockReason::NeedsPermission => "Needs permission".to_string(),
        BlockReason::Stale(why) => format!("Needs a new decision: {why}"),
        BlockReason::Denied => "Denied on this computer: it will not run".to_string(),
        BlockReason::Conflict => {
            "Blocked: another package with the same id and version is recorded".to_string()
        }
        BlockReason::StoreDamaged(why) => format!("Blocked: the trust records are damaged ({why})"),
        BlockReason::Unconfigured => "Blocked: permissions are unavailable".to_string(),
        BlockReason::Unidentifiable(why) => format!("Blocked: cannot be verified ({why})"),
        BlockReason::Dependency(inner) => package_reason(inner),
    }
}
