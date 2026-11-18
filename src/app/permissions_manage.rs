//! P4.3.2F: managing recorded permissions (Library → Permissions →
//! "Recorded permissions").
//!
//! Lists every decision Patchwork relies on, with the exact content each is
//! bound to, and lets the user change it:
//! * **Projects and custom-node files** (content ledger): allowed and
//!   denied content by digest, with the approved capabilities. An approval
//!   can be revoked (the content needs a decision again) or changed;
//!   approving anything beyond what was approved before, including
//!   allowing denied content again, needs an explicit acknowledgement.
//! * **Packages** (trust store): grants by `id@version` and tree digest. A
//!   grant can be revoked, denied or narrowed; widening always goes through
//!   the package review (which rescans the content). A denial can be lifted
//!   so the package is offered for review again (that grants nothing).
//! * **Native plugins**: pinned CLAP binaries and approved Rust plugin
//!   sources; either approval can be withdrawn.
//! * **Recovery**: damaged trust records (moved aside, never trusted
//!   again), staging folders left by interrupted installations, and
//!   granted marketplace packages the catalogue withdrew.
//!
//! Every change is a trust-store write, which starts a new trust
//! generation: open documents, shown or not, follow it at once (see
//! `provenance::apply_trust_changes`); loaded native plugins check their
//! approval again. Nothing here ever grants more without the user's
//! explicit acknowledgement.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use eframe::egui;

use crate::capability::{CapSet, Capability};
use crate::library::trust::content::{ContentDecision, ContentKind, LedgerEntry};
use crate::library::trust::{
    Grant, PackageIdentity, StoreLoad, TrustRecord, TrustStore, WithdrawnGrant,
};

/// What the window shows, read when the records change (not every frame).
pub(super) struct Snapshot {
    generation: u64,
    native_generation: u64,
    pub content: Result<Vec<LedgerEntry>, String>,
    pub packages: Result<Vec<TrustRecord>, String>,
    pub native: Result<Vec<(String, String)>, String>,
    pub sources: Vec<(String, crate::plugin_trust::TrustEntry)>,
    pub staging: Vec<PathBuf>,
    pub withdrawn: Vec<WithdrawnGrant>,
}

/// A change the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ManageAction {
    RevokeContent(ContentKind, String),
    SetContent {
        kind: ContentKind,
        digest: String,
        approved: CapSet,
        acknowledged: bool,
    },
    RevokePackage(PackageIdentity),
    DenyPackage(PackageIdentity),
    LiftDenial(PackageIdentity),
    NarrowPackage(PackageIdentity, BTreeSet<Capability>),
    RevokeNative(String),
    RevokeSource(String),
    MoveLedgerAside,
    MoveStoreAside,
    MovePluginApprovalsAside,
    RemoveStaging(PathBuf),
}

fn unavailable<T>() -> Result<T, String> {
    Err("Permissions are unavailable (no home folder).".to_string())
}

/// Read every record (`store`: the package trust store; the content ledger
/// is next to it).
pub(super) fn snapshot(
    store: Option<&TrustStore>,
    library_root: Option<&Path>,
    catalogue: Option<(
        &crate::library::marketplace::Catalogue,
        &crate::library::marketplace::source::CatalogueLocation,
    )>,
) -> Snapshot {
    let (content, packages, withdrawn) = match store {
        Some(store) => {
            let load = store.load();
            let withdrawn = match (&load, catalogue) {
                (StoreLoad::Loaded(loaded), Some((catalogue, location))) => {
                    crate::library::trust::withdrawn_grants(loaded, catalogue, location)
                }
                _ => Vec::new(),
            };
            let packages = match load {
                StoreLoad::Missing => Ok(Vec::new()),
                StoreLoad::Loaded(loaded) => Ok(loaded.records().cloned().collect()),
                StoreLoad::Damaged(why) => Err(why),
            };
            (store.content_ledger().entries(), packages, withdrawn)
        }
        None => (unavailable(), unavailable(), Vec::new()),
    };
    Snapshot {
        generation: crate::library::trust::generation(),
        native_generation: crate::plugin_trust::native_generation(),
        content,
        packages,
        native: crate::plugin_trust::native_pins(),
        sources: {
            let mut sources = crate::plugin_trust::list_trusted();
            sources.sort_by(|a, b| a.0.cmp(&b.0));
            sources
        },
        staging: library_root
            .map(crate::library::package::staging_leftovers)
            .unwrap_or_default(),
        withdrawn,
    }
}

impl Snapshot {
    /// The records changed since this was read.
    pub(super) fn is_outdated(&self) -> bool {
        self.generation != crate::library::trust::generation()
            || self.native_generation != crate::plugin_trust::native_generation()
    }
}

/// Carry out `action`. Returns what was done (for the log).
pub(super) fn apply(
    action: &ManageAction,
    store: &TrustStore,
    library_root: Option<&Path>,
) -> Result<String, String> {
    let ledger = store.content_ledger();
    match action {
        ManageAction::RevokeContent(kind, digest) => {
            ledger.revoke(*kind, digest)?;
            Ok(format!(
                "the decision for {} {} was revoked",
                kind.label(),
                short(digest)
            ))
        }
        ManageAction::SetContent {
            kind,
            digest,
            approved,
            acknowledged,
        } => {
            ledger.set_approved(*kind, digest, *approved, *acknowledged)?;
            Ok(format!(
                "{} {} may now use: {}",
                kind.label(),
                short(digest),
                approved.describe()
            ))
        }
        ManageAction::RevokePackage(identity) => {
            store.revoke_grant(identity)?;
            Ok(format!("{} is no longer allowed to run", identity.label()))
        }
        ManageAction::DenyPackage(identity) => {
            store.deny_recorded(identity)?;
            Ok(format!("{} was denied", identity.label()))
        }
        ManageAction::LiftDenial(identity) => {
            store.withdraw_denial(identity)?;
            Ok(format!(
                "{} is no longer denied; it will be offered for review",
                identity.label()
            ))
        }
        ManageAction::NarrowPackage(identity, approved) => {
            store.narrow_grant(identity, approved.clone())?;
            Ok(format!(
                "{} may now use: {}",
                identity.label(),
                CapSet::from_iter(approved.iter().copied()).describe()
            ))
        }
        ManageAction::RevokeNative(path) => {
            crate::plugin_trust::revoke_native_pin(path)?;
            Ok(format!("the approval of {path} was withdrawn"))
        }
        ManageAction::RevokeSource(sha) => {
            if !crate::plugin_trust::revoke_trust(sha) {
                return Err("This plugin source has no approval.".to_string());
            }
            Ok(format!(
                "the approval of plugin source {} was withdrawn",
                short(sha)
            ))
        }
        ManageAction::MoveLedgerAside => {
            let aside = ledger.move_damaged_aside()?;
            Ok(format!(
                "the damaged content ledger was moved to {}",
                aside.display()
            ))
        }
        ManageAction::MoveStoreAside => {
            let aside = store.move_damaged_aside()?;
            Ok(format!(
                "the damaged trust store was moved to {}",
                aside.display()
            ))
        }
        ManageAction::MovePluginApprovalsAside => {
            let aside = crate::plugin_trust::move_damaged_aside()?;
            Ok(format!(
                "the damaged plugin approvals were moved to {}",
                aside.display()
            ))
        }
        ManageAction::RemoveStaging(path) => {
            let root = library_root.ok_or_else(|| "The Library is unavailable.".to_string())?;
            crate::library::package::remove_staging_leftover(root, path)?;
            Ok(format!(
                "the installation leftover {} was removed",
                path.display()
            ))
        }
    }
}

fn short(digest: &str) -> &str {
    &digest[..digest.len().min(12)]
}

/// The window's own state.
#[derive(Default)]
pub(super) struct ManageUi {
    pub snapshot: Option<Snapshot>,
    /// The capability set being edited, by record key.
    editing: Option<(String, CapSet)>,
    acknowledged: HashSet<String>,
    show_authored: bool,
}

impl ManageUi {
    pub(super) fn needs_refresh(&self) -> bool {
        self.snapshot.as_ref().is_none_or(Snapshot::is_outdated)
    }

    pub(super) fn after_action(&mut self) {
        self.snapshot = None;
        self.editing = None;
        self.acknowledged.clear();
    }
}

fn capability_set_of(set: &BTreeSet<Capability>) -> CapSet {
    set.iter().copied().collect()
}

/// Tick boxes for a capability set limited to `offered`.
fn capability_boxes(ui: &mut egui::Ui, set: &mut CapSet, offered: CapSet) {
    ui.horizontal_wrapped(|ui| {
        for capability in offered.iter() {
            let mut on = set.contains(capability);
            if ui
                .checkbox(&mut on, crate::capability::describe(capability))
                .changed()
            {
                *set = if on {
                    set.with(capability)
                } else {
                    set.minus(CapSet::of(&[capability]))
                };
            }
        }
    });
}

/// An acknowledgement box (remembered per key).
fn acknowledge(
    ui: &mut egui::Ui,
    acknowledged: &mut HashSet<String>,
    key: &str,
    text: &str,
) -> bool {
    let mut ticked = acknowledged.contains(key);
    if ui.checkbox(&mut ticked, text).changed() {
        if ticked {
            acknowledged.insert(key.to_string());
        } else {
            acknowledged.remove(key);
        }
    }
    ticked
}

pub(super) fn render(ui: &mut egui::Ui, state: &mut ManageUi) -> Option<ManageAction> {
    let mut action = None;
    let Some(snapshot) = &state.snapshot else {
        ui.weak("Reading…");
        return None;
    };
    let warn = ui.visuals().warn_fg_color;

    // ── Recovery ──
    let mut recovery = Vec::new();
    if let Err(why) = &snapshot.content {
        recovery.push((why.clone(), ManageAction::MoveLedgerAside));
    }
    if let Err(why) = &snapshot.packages {
        recovery.push((why.clone(), ManageAction::MoveStoreAside));
    }
    if let Err(why) = &snapshot.native {
        recovery.push((why.clone(), ManageAction::MovePluginApprovalsAside));
    }
    if !recovery.is_empty() || !snapshot.staging.is_empty() || !snapshot.withdrawn.is_empty() {
        ui.group(|ui| {
            ui.strong("Needs attention");
            for (why, fix) in recovery {
                ui.colored_label(warn, why);
                ui.label(
                    "While these records are damaged nothing they covered is trusted. Moving \
                     the file aside keeps it for inspection and starts empty records: \
                     everything they covered needs your decision again. Your projects and \
                     packages are not changed.",
                );
                if ui.button("Move the damaged file aside").clicked() {
                    action = Some(fix);
                }
            }
            for path in &snapshot.staging {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "Interrupted installation (never loaded): {}",
                        path.display()
                    ));
                    if ui.small_button("Remove").clicked() {
                        action = Some(ManageAction::RemoveStaging(path.clone()));
                    }
                });
            }
            for withdrawn in &snapshot.withdrawn {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        warn,
                        format!(
                            "{} is allowed to run, but the marketplace {}.",
                            withdrawn.identity.label(),
                            withdrawn.reason
                        ),
                    );
                    if ui.small_button("Revoke").clicked() {
                        action = Some(ManageAction::RevokePackage(withdrawn.identity.clone()));
                    }
                });
            }
        });
    }

    // ── Projects and custom-node files ──
    egui::CollapsingHeader::new("Projects and custom-node files")
        .default_open(true)
        .show(ui, |ui| {
            let entries = match &snapshot.content {
                Ok(entries) => entries,
                Err(_) => {
                    ui.weak("Unavailable (see above).");
                    return;
                }
            };
            let authored = entries
                .iter()
                .filter(|e| e.decision == ContentDecision::Authored)
                .count();
            ui.checkbox(
                &mut state.show_authored,
                format!("Also show content created on this computer ({authored})"),
            );
            let shown: Vec<&LedgerEntry> = entries
                .iter()
                .filter(|e| state.show_authored || e.decision != ContentDecision::Authored)
                .collect();
            if shown.is_empty() {
                ui.weak("No decisions recorded.");
            }
            egui::ScrollArea::vertical()
                .id_salt("manage_content")
                .max_height(220.0)
                .show(ui, |ui| {
                    for entry in shown {
                        let key = format!("content:{:?}:{}", entry.kind, entry.digest);
                        ui.group(|ui| {
                            let (status, rights) = match entry.decision {
                                ContentDecision::Authored => {
                                    ("Created on this computer".to_string(), CapSet::ALL)
                                }
                                ContentDecision::Allowed(set) => {
                                    (format!("Allowed (may use: {})", set.describe()), set)
                                }
                                ContentDecision::Denied => ("Denied".to_string(), CapSet::EMPTY),
                            };
                            ui.strong(format!("{} — {status}", entry.kind.label()));
                            ui.monospace(format!("Content digest: {}", entry.digest));
                            match &mut state.editing {
                                Some((editing, set)) if *editing == key => {
                                    capability_boxes(ui, set, CapSet::ALL);
                                    let set = *set;
                                    let widens = !set.is_subset(rights);
                                    let ticked = !widens
                                        || acknowledge(
                                            ui,
                                            &mut state.acknowledged,
                                            &key,
                                            "I trust this exact content and allow it what is \
                                             ticked",
                                        );
                                    if widens {
                                        ui.colored_label(warn, super::permissions::NOT_SANDBOXED);
                                    }
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add_enabled(ticked, egui::Button::new("Apply"))
                                            .clicked()
                                        {
                                            action = Some(ManageAction::SetContent {
                                                kind: entry.kind,
                                                digest: entry.digest.clone(),
                                                approved: set,
                                                acknowledged: widens && ticked,
                                            });
                                        }
                                        if ui.button("Cancel").clicked() {
                                            state.editing = None;
                                        }
                                    });
                                }
                                _ => {
                                    ui.horizontal(|ui| {
                                        let edit = if entry.decision == ContentDecision::Denied {
                                            "Allow again…"
                                        } else {
                                            "Change…"
                                        };
                                        if ui.button(edit).clicked() {
                                            // Starts from what is approved
                                            // now (nothing for denied).
                                            state.editing = Some((key.clone(), rights));
                                        }
                                        if ui.button("Revoke").clicked() {
                                            action = Some(ManageAction::RevokeContent(
                                                entry.kind,
                                                entry.digest.clone(),
                                            ));
                                        }
                                    });
                                }
                            }
                        });
                    }
                });
        });

    // ── Packages ──
    egui::CollapsingHeader::new("Packages")
        .default_open(true)
        .show(ui, |ui| {
            let records = match &snapshot.packages {
                Ok(records) => records,
                Err(_) => {
                    ui.weak("Unavailable (see above).");
                    return;
                }
            };
            if records.is_empty() {
                ui.weak("No packages recorded.");
            }
            egui::ScrollArea::vertical()
                .id_salt("manage_packages")
                .max_height(220.0)
                .show(ui, |ui| {
                    for record in records {
                        let identity = &record.identity;
                        let key = format!("package:{}", identity.tree_sha256);
                        ui.group(|ui| {
                            let status = match (record.grant, &record.approved) {
                                (Grant::Granted, Some(approved)) => format!(
                                    "Allowed (may use: {})",
                                    capability_set_of(approved).describe()
                                ),
                                (Grant::Granted, None) => "Allowed".to_string(),
                                (Grant::NotGranted, _) => "Not allowed to run".to_string(),
                                (Grant::Denied, _) => "Denied".to_string(),
                            };
                            ui.strong(format!("{} — {status}", identity.label()));
                            ui.label(record.provenance.label());
                            ui.monospace(format!("Tree digest: {}", identity.tree_sha256));
                            ui.label(format!(
                                "Uses: {}",
                                capability_set_of(&record.detected).describe()
                            ));
                            match (record.grant, &record.approved, &mut state.editing) {
                                (Grant::Granted, Some(approved), Some((editing, set)))
                                    if *editing == key =>
                                {
                                    // Narrowing only: widening goes
                                    // through the package review.
                                    capability_boxes(ui, set, capability_set_of(approved));
                                    let set = *set;
                                    if !capability_set_of(&record.detected).is_subset(set) {
                                        ui.colored_label(
                                            warn,
                                            "It uses capabilities outside this set, so it \
                                             will not run until it is reviewed again.",
                                        );
                                    }
                                    ui.horizontal(|ui| {
                                        if ui.button("Apply").clicked() {
                                            action = Some(ManageAction::NarrowPackage(
                                                identity.clone(),
                                                set.iter().collect(),
                                            ));
                                        }
                                        if ui.button("Cancel").clicked() {
                                            state.editing = None;
                                        }
                                    });
                                }
                                (Grant::Granted, approved, _) => {
                                    ui.horizontal(|ui| {
                                        if let Some(approved) = approved {
                                            if ui.button("Narrow…").clicked() {
                                                state.editing = Some((
                                                    key.clone(),
                                                    capability_set_of(approved),
                                                ));
                                            }
                                        }
                                        if ui.button("Revoke").clicked() {
                                            action =
                                                Some(ManageAction::RevokePackage(identity.clone()));
                                        }
                                        if ui.button("Deny").clicked() {
                                            action =
                                                Some(ManageAction::DenyPackage(identity.clone()));
                                        }
                                    });
                                }
                                (Grant::Denied, _, _) => {
                                    if acknowledge(
                                        ui,
                                        &mut state.acknowledged,
                                        &key,
                                        "Offer this package for review again",
                                    ) && ui.button("Lift the denial").clicked()
                                    {
                                        action = Some(ManageAction::LiftDenial(identity.clone()));
                                    }
                                }
                                (Grant::NotGranted, _, _) => {
                                    if ui.button("Deny").clicked() {
                                        action = Some(ManageAction::DenyPackage(identity.clone()));
                                    }
                                }
                            }
                        });
                    }
                });
        });

    // ── Native plugins ──
    egui::CollapsingHeader::new("Native plugins").show(ui, |ui| {
        match &snapshot.native {
            Ok(pins) if pins.is_empty() => {
                ui.weak("No CLAP plugin approvals.");
            }
            Ok(pins) => {
                for (path, sha) in pins {
                    ui.horizontal(|ui| {
                        if sha == crate::plugin_trust::REVOKED {
                            ui.label(format!("CLAP {path} — approval withdrawn"));
                        } else {
                            ui.label(format!("CLAP {path}"));
                            ui.monospace(crate::plugin_trust::short_hash(sha));
                            if ui.small_button("Withdraw").clicked() {
                                action = Some(ManageAction::RevokeNative(path.clone()));
                            }
                        }
                    });
                }
            }
            Err(_) => {
                ui.weak("Unavailable (see above).");
            }
        }
        for (sha, entry) in &snapshot.sources {
            ui.horizontal(|ui| {
                ui.label(format!("Rust plugin source: {}", entry.description));
                ui.monospace(crate::plugin_trust::short_hash(sha));
                if ui.small_button("Withdraw").clicked() {
                    action = Some(ManageAction::RevokeSource(sha.clone()));
                }
            });
        }
    });
    action
}
