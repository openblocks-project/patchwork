//! P4.3.2D: package execution permissions.
//!
//! Package runtimes that are not authorized (see
//! `library::trust::authorize`) stay blocked: their nodes, ports and wires
//! are kept, nothing inside runs, and the node is labelled "Needs
//! permission". Instead of one popup per package, a single non-blocking
//! banner counts the packages waiting for a decision and opens the
//! **Permissions** window, where each package can be inspected and
//! allowed or denied individually. Allowing requires ticking an explicit
//! acknowledgement; it is never combined with installation consent. A
//! denial persists and is not offered again until the user allows the
//! package from this window.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use eframe::egui;

use crate::library::trust::TrustStore;
use crate::library::trust::authorize::{BlockReason, Blocked, TrustPolicy};
use crate::library::trust::content::{ContentKind, ContentState};

/// The trust policy for the editor's graphs (`~/.patchwork/trust`), or
/// "unconfigured" (everything blocked) without a home folder.
pub(super) fn editor_trust_policy() -> TrustPolicy {
    static POLICY: std::sync::OnceLock<TrustPolicy> = std::sync::OnceLock::new();
    POLICY
        .get_or_init(|| match TrustStore::default_location() {
            Ok(store) => TrustPolicy::Store(store),
            Err(error) => {
                crate::system_log::warn(format!("Package permissions are unavailable: {error}"));
                TrustPolicy::Unconfigured
            }
        })
        .clone()
}

#[derive(Default)]
pub(super) struct PermissionsWindow {
    pub open: bool,
    /// The banner was dismissed while this many packages were pending.
    dismissed_at: Option<usize>,
    /// Acknowledgement ticked, per package digest.
    acknowledged: HashSet<String>,
    message: Option<String>,
    /// Packages refused outside a graph (opening a package in a tab).
    refused: Vec<Blocked>,
    /// P4.3.2E2: the banner was dismissed for these unapproved capabilities.
    dismissed_unapproved: Option<crate::capability::CapSet>,
    /// P4.3.2F: recorded permissions, as listed for management.
    manage: super::permissions_manage::ManageUi,
}

/// P4.3.2F: read every recorded permission (and the configured
/// marketplace catalogue's cached copy, for withdrawn versions).
fn read_records() -> super::permissions_manage::Snapshot {
    let store = match editor_trust_policy() {
        TrustPolicy::Store(store) => Some(store),
        _ => None,
    };
    let root = crate::library::library_root().ok();
    let location = crate::library::marketplace::source::CatalogueLocation::from_settings(
        &crate::settings::Settings::load().marketplace,
    )
    .ok()
    .flatten();
    let view = match (
        &location,
        crate::library::marketplace::cache::default_cache_dir(),
    ) {
        (Some(location), Ok(dir)) => Some(crate::library::marketplace::cache::open_cached(
            &dir, location,
        )),
        _ => None,
    };
    let catalogue = match (&view, &location) {
        (Some(view), Some(location)) => view.catalogue.as_ref().map(|c| (c, location)),
        _ => None,
    };
    super::permissions_manage::snapshot(store.as_ref(), root.as_deref(), catalogue)
}

impl PermissionsWindow {
    pub(super) fn add_refused(&mut self, blocked: Blocked) {
        self.refused.push(blocked);
    }
}

/// One package per folder and content (innermost blocked package).
fn unique_blocked(all: Vec<Blocked>) -> Vec<Blocked> {
    let mut by_key: BTreeMap<(PathBuf, String), Blocked> = BTreeMap::new();
    for blocked in all {
        let inner = blocked.innermost().clone();
        let digest = inner
            .package
            .identity
            .as_ref()
            .map(|i| i.tree_sha256.clone())
            .unwrap_or_default();
        by_key
            .entry((inner.package.dir.clone(), digest))
            .or_insert(inner);
    }
    by_key.into_values().collect()
}

fn capability_list(
    set: &std::collections::BTreeSet<crate::library::marketplace::Capability>,
) -> String {
    if set.is_empty() {
        "none".to_string()
    } else {
        set.iter()
            .map(|c| {
                crate::library::marketplace::browse::capability_label(
                    &crate::library::marketplace::DeclaredCapability::Known(*c),
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

enum Action {
    Allow(Blocked),
    Deny(Blocked),
}

/// The digest a decision is bound to: a package's tree digest or a loose
/// custom node's content digest.
fn decision_digest(package: &crate::library::trust::authorize::PackageSummary) -> Option<String> {
    package
        .identity
        .as_ref()
        .map(|i| i.tree_sha256.clone())
        .or_else(|| package.loose_digest.clone())
}

fn reason_text(reason: &BlockReason) -> String {
    match reason {
        BlockReason::NeedsPermission => "Needs permission".to_string(),
        BlockReason::Stale(why) => format!("Needs a new decision: {why}"),
        BlockReason::Denied => "Denied".to_string(),
        BlockReason::Conflict => {
            "Blocked: a different package with the same id and version is already recorded"
                .to_string()
        }
        BlockReason::StoreDamaged(why) => format!(
            "Blocked: the trust records are damaged ({why}). Use Library → Package Trust Review."
        ),
        BlockReason::Unconfigured => "Blocked: permissions are unavailable".to_string(),
        BlockReason::Unidentifiable(why) => format!("Blocked: cannot be verified ({why})"),
        BlockReason::Dependency(inner) => reason_text(&inner.reason),
    }
}

pub(super) const NOT_SANDBOXED: &str = "Running is not sandboxed: its nodes get the same access to your \
    files, network, devices and programs as Patchwork itself.";

impl super::PatchworkApp {
    pub(super) fn open_permissions(&mut self) {
        self.permissions.open = true;
    }

    /// The banner (above the canvas) and the review window.
    pub(super) fn render_permissions(&mut self, ctx: &egui::Context) {
        super::provenance::resolve_graph(&mut self.graph);
        let document_state = self.graph.content.clone();
        let document_blocked = !document_state.runnable();
        let document_kind = match self.documents.active().kind {
            crate::document::DocumentKind::Project => "project",
            crate::document::DocumentKind::CustomNode => "custom node",
        };
        // P4.3.2E2: what the document's own nodes use, and what of it the
        // document may not do (allowed content with unapproved capabilities).
        let document_uses = self.document_uses();
        let unapproved = self.unapproved_capabilities();
        let denials = crate::capability::recent_denials();
        let mut all = self.graph.blocked_packages();
        all.extend(self.permissions.refused.iter().cloned());
        let blocked = unique_blocked(all);
        let pending = blocked.iter().filter(|b| b.needs_review()).count();
        if pending == 0 {
            self.permissions.dismissed_at = None;
        }
        // One banner for everything waiting, never a popup.
        let unapproved_shown =
            !unapproved.is_empty() && self.permissions.dismissed_unapproved != Some(unapproved);
        let show_banner = !self.permissions.open
            && (document_blocked
                || unapproved_shown
                || (pending > 0 && self.permissions.dismissed_at != Some(pending)));
        if show_banner {
            let mut review = false;
            let mut dismiss = false;
            let mut parts = Vec::new();
            if document_blocked {
                parts.push(match &document_state {
                    ContentState::Denied { .. } => {
                        format!("This {document_kind} was denied and is not running")
                    }
                    _ => format!("This {document_kind} is not running: its origin is not verified"),
                });
            }
            if unapproved_shown {
                parts.push(format!(
                    "This {document_kind} uses {} without your approval",
                    unapproved.describe()
                ));
            }
            if pending > 0 {
                parts.push(format!(
                    "{pending} item{} need{} permission to run",
                    if pending == 1 { "" } else { "s" },
                    if pending == 1 { "s" } else { "" },
                ));
            }
            egui::TopBottomPanel::top("package_permission_banner").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("🔒 {}", parts.join(" · ")))
                            .color(egui::Color32::from_rgb(255, 190, 80)),
                    );
                    if ui.button("Review").clicked() {
                        review = true;
                    }
                    if !document_blocked && ui.small_button("Dismiss").clicked() {
                        dismiss = true;
                    }
                });
            });
            if review {
                self.permissions.open = true;
            }
            if dismiss {
                self.permissions.dismissed_at = Some(pending);
                self.permissions.dismissed_unapproved = Some(unapproved);
            }
        }
        if !self.permissions.open {
            return;
        }

        if self.permissions.manage.needs_refresh() {
            self.permissions.manage.snapshot = Some(read_records());
        }
        // P4.3.2F: the exact identity the open document's decision is
        // bound to.
        let document_identity = match &self.graph.content_basis {
            Some(crate::library::trust::content::ContentBasis::Content { digest, .. }) => {
                Some(format!("Content digest: {digest}"))
            }
            Some(crate::library::trust::content::ContentBasis::Package { entry, .. }) => {
                Some(format!("Package entry: {}", entry.display()))
            }
            None => None,
        };
        let mut manage_action = None;
        let mut open = true;
        let mut action: Option<Action> = None;
        let mut document_decision: Option<bool> = None;
        let mut approve_capabilities = false;
        let mut clear_denials = false;
        let window = &mut self.permissions;
        egui::Window::new("Permissions")
            .open(&mut open)
            .default_width(600.0)
            .default_height(480.0)
            .show(ctx, |ui| {
                ui.label(
                    "Projects, custom nodes and packages run only when Patchwork can tell \
                     they were created on this computer, or after you allow them. Content \
                     brought from elsewhere, edited outside Patchwork or saved by an older \
                     Patchwork needs your decision once.",
                );
                if let Some(message) = &window.message {
                    ui.colored_label(ui.visuals().warn_fg_color, message.as_str());
                }
                ui.separator();
                // ── The open document ──
                ui.group(|ui| {
                    ui.strong(format!("This {document_kind}"));
                    ui.label(document_state.label());
                    ui.label(format!("Its nodes use: {}", document_uses.describe()));
                    if !document_blocked {
                        match &document_identity {
                            Some(identity) => {
                                ui.monospace(identity);
                            }
                            None => {
                                ui.weak("Created in this session (not saved yet).");
                            }
                        }
                        // P4.3.2E2: capabilities used but never approved
                        // (added later, by hand or by MCP, or allowed by an
                        // older Patchwork) are refused until approved here.
                        if unapproved.is_empty() {
                            return;
                        }
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!(
                                "Not approved: {}. Its nodes cannot do these until you \
                                 approve them.",
                                unapproved.describe()
                            ),
                        );
                        ui.colored_label(ui.visuals().warn_fg_color, NOT_SANDBOXED);
                        let key = "capabilities:".to_string();
                        let mut ticked = window.acknowledged.contains(&key);
                        if ui
                            .checkbox(&mut ticked, "I approve these capabilities for this document")
                            .changed()
                        {
                            if ticked {
                                window.acknowledged.insert(key.clone());
                            } else {
                                window.acknowledged.remove(&key);
                            }
                        }
                        if ui
                            .add_enabled(ticked, egui::Button::new("Approve"))
                            .clicked()
                        {
                            approve_capabilities = true;
                        }
                        return;
                    }
                    ui.label(format!(
                        "Allowing it approves exactly these capabilities: {}.",
                        document_uses.describe()
                    ));
                    let digest = match &document_state {
                        ContentState::Unverified { digest, .. } => digest.clone(),
                        ContentState::Denied { digest, .. }
                        | ContentState::Pending { digest, .. } => Some(digest.clone()),
                        _ => None,
                    };
                    match &digest {
                        Some(digest) => {
                            ui.monospace(format!("Content digest: {digest}"));
                        }
                        None => {
                            ui.weak("Its content is not identified; allowing it lasts until you close it.");
                        }
                    }
                    ui.label(
                        "Its nodes, wires and settings are kept and can be edited, but nothing \
                         in it runs. Allowing it does not allow the packages or custom-node \
                         files it uses; they are listed separately.",
                    );
                    ui.colored_label(ui.visuals().warn_fg_color, NOT_SANDBOXED);
                    let key = format!("document:{}", digest.clone().unwrap_or_default());
                    let mut ticked = window.acknowledged.contains(&key);
                    if ui
                        .checkbox(&mut ticked, "I trust this content and allow it to run")
                        .changed()
                    {
                        if ticked {
                            window.acknowledged.insert(key.clone());
                        } else {
                            window.acknowledged.remove(&key);
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(ticked, egui::Button::new("Allow to run"))
                            .clicked()
                        {
                            document_decision = Some(true);
                        }
                        if !matches!(document_state, ContentState::Denied { .. })
                            && ui.button("Deny").clicked()
                        {
                            document_decision = Some(false);
                        }
                    });
                });
                // P4.3.2E2: operations refused recently (newest first).
                if !denials.is_empty() {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.strong("Recently refused");
                        if ui.small_button("Clear").clicked() {
                            clear_denials = true;
                        }
                    });
                    for denied in denials.iter().rev().take(8) {
                        ui.small(denied.to_string());
                    }
                }
                // P4.3.2F: every recorded decision, and recovery.
                ui.add_space(4.0);
                egui::CollapsingHeader::new("Recorded permissions").show(ui, |ui| {
                    if let Some(chosen) = super::permissions_manage::render(ui, &mut window.manage)
                    {
                        manage_action = Some(chosen);
                    }
                });
                if blocked.is_empty() {
                    return;
                }
                ui.add_space(4.0);
                ui.strong("Used by this document");
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for item in &blocked {
                        let package = &item.package;
                        ui.group(|ui| {
                            let kind = if package.loose_digest.is_some() {
                                "Custom node file"
                            } else {
                                "Package"
                            };
                            ui.strong(format!("{kind}: {} — {}", package.name, package.label));
                            ui.label(reason_text(&item.reason));
                            ui.label(package.origin_label());
                            if package.loose_digest.is_some() {
                                ui.weak(package.dir.display().to_string());
                            }
                            let Some(digest) = decision_digest(package) else {
                                return;
                            };
                            ui.monospace(format!("Content digest: {digest}"));
                            if let Some(declared) = &package.declared {
                                ui.label(format!(
                                    "Declared capabilities: {}",
                                    capability_list(declared)
                                ));
                            }
                            ui.label(format!(
                                "Detected capabilities: {}",
                                capability_list(&package.detected)
                            ));
                            for (label, set) in [
                                ("Restricted node types", &package.restricted),
                                ("Unclassified node types", &package.unclassified),
                            ] {
                                if !set.is_empty() {
                                    ui.colored_label(
                                        ui.visuals().warn_fg_color,
                                        format!(
                                            "{label}: {}",
                                            set.iter().cloned().collect::<Vec<_>>().join(", ")
                                        ),
                                    );
                                }
                            }
                            let decidable = matches!(
                                item.reason,
                                BlockReason::NeedsPermission
                                    | BlockReason::Stale(_)
                                    | BlockReason::Denied
                            );
                            if !decidable {
                                return;
                            }
                            ui.colored_label(ui.visuals().warn_fg_color, NOT_SANDBOXED);
                            let mut ticked = window.acknowledged.contains(&digest);
                            if ui
                                .checkbox(
                                    &mut ticked,
                                    "I trust this exact content and allow it to run",
                                )
                                .changed()
                            {
                                if ticked {
                                    window.acknowledged.insert(digest.clone());
                                } else {
                                    window.acknowledged.remove(&digest);
                                }
                            }
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(ticked, egui::Button::new("Allow to run"))
                                    .clicked()
                                {
                                    action = Some(Action::Allow(item.clone()));
                                }
                                if item.reason != BlockReason::Denied
                                    && ui.button("Deny").clicked()
                                {
                                    action = Some(Action::Deny(item.clone()));
                                }
                            });
                        });
                    }
                });
            });

        if clear_denials {
            crate::capability::clear_recent_denials();
        }
        if let Some(chosen) = manage_action {
            let result = match editor_trust_policy() {
                TrustPolicy::Store(store) => super::permissions_manage::apply(
                    &chosen,
                    &store,
                    crate::library::library_root().ok().as_deref(),
                ),
                _ => Err("Permissions are unavailable.".to_string()),
            };
            match result {
                Ok(done) => {
                    crate::system_log::log(format!("Permissions: {done}."));
                    self.permissions.message = None;
                    self.permissions.manage.after_action();
                    // Open documents follow at once (shown or not).
                    self.apply_trust_changes();
                }
                Err(why) => self.permissions.message = Some(why),
            }
        }
        if approve_capabilities {
            match self.approve_active_capabilities() {
                Ok(added) => {
                    crate::system_log::log(format!(
                        "Permissions: approved {} for this {document_kind}.",
                        added.describe()
                    ));
                    self.permissions.message = None;
                    self.permissions.acknowledged.remove("capabilities:");
                }
                Err(why) => self.permissions.message = Some(why),
            }
        }
        if let Some(allow) = document_decision {
            match self.decide_active_content(allow) {
                Ok(()) => {
                    crate::system_log::log(format!(
                        "Permissions: this {document_kind} was {}.",
                        if allow { "allowed" } else { "denied" }
                    ));
                    self.permissions.message = None;
                    self.permissions
                        .acknowledged
                        .retain(|k| !k.starts_with("document:"));
                }
                Err(why) => self.permissions.message = Some(why),
            }
        }
        if let Some(action) = action {
            let (blocked, allow) = match action {
                Action::Allow(blocked) => (blocked, true),
                Action::Deny(blocked) => (blocked, false),
            };
            let package = blocked.package;
            let result = match (
                editor_trust_policy(),
                &package.identity,
                &package.loose_digest,
            ) {
                (TrustPolicy::Store(store), Some(identity), _) => if allow {
                    store.grant(&package.dir, identity)
                } else {
                    store.deny(&package.dir, identity)
                }
                .map(|record| {
                    format!(
                        "{} (digest {})",
                        record.identity.label(),
                        identity.tree_sha256
                    )
                }),
                (TrustPolicy::Store(store), None, Some(digest)) => store
                    .content_ledger()
                    .decide_file(ContentKind::CustomNode, &package.dir, digest, allow)
                    .map(|()| format!("{} (digest {digest})", package.dir.display())),
                _ => Err("Permissions are unavailable.".to_string()),
            };
            match result {
                Ok(what) => {
                    let verb = if allow { "allowed" } else { "denied" };
                    crate::system_log::log(format!("Permissions: {what} {verb}."));
                    self.permissions.message = None;
                    if let Some(digest) = decision_digest(&package) {
                        self.permissions.acknowledged.remove(&digest);
                        self.permissions.refused.retain(|b| {
                            decision_digest(&b.innermost().package).as_ref() != Some(&digest)
                        });
                    }
                }
                Err(why) => self.permissions.message = Some(why),
            }
        }
        self.permissions.open = open;
    }
}
