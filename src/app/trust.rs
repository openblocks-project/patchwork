//! P4.3.2C: Package Trust Review (the one-time Library migration).
//!
//! Library packages that existed before the trust store, or whose content
//! no longer matches their record, have no provenance. This window lists
//! them so the user can review each one. Nothing is decided automatically:
//! a `local.*` id or a manifest `source` is shown as a claim only, and a
//! package becomes recorded (as "imported", origin not verified) only when
//! the user accepts it. The user can also deny a package, defer the review
//! for this session or decline the migration altogether. Package files are
//! never changed, and projects keep working whatever is decided: nothing is
//! enforced in this stage.
//!
//! The Library is identified on a worker thread (it hashes every package).

use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui;

use crate::library::trust::{
    LegacyReview, PackageIdentity, ReviewDecision, ReviewItem, StoreLoad, TrustStatus, TrustStore,
    apply_review, review_library,
};

struct ReviewResult {
    load: StoreLoad,
    items: Vec<ReviewItem>,
}

#[derive(Default)]
pub(super) struct TrustReviewWindow {
    pub open: bool,
    /// The automatic check at start-up has been started.
    auto_started: bool,
    /// The current result was requested by the automatic check.
    auto_pending: bool,
    pending: Option<Receiver<Result<ReviewResult, String>>>,
    result: Option<ReviewResult>,
    /// The Library or store changed; recompute when next shown.
    stale: bool,
    message: Option<String>,
}

impl TrustReviewWindow {
    /// Publishing or installing changed the Library.
    pub(super) fn invalidate(&mut self) {
        self.stale = true;
    }

    fn start(&mut self, ctx: &egui::Context, root: std::path::PathBuf, auto: bool) {
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = TrustStore::default_location().map(|store| {
                let load = store.load();
                // The start-up check only needs the Library when the
                // migration is still pending (or the store is unusable).
                let settled = matches!(
                    &load,
                    StoreLoad::Loaded(store) if store.legacy_review != LegacyReview::Pending
                );
                let items = if auto && settled {
                    Vec::new()
                } else {
                    review_library(&root, &load)
                };
                ReviewResult { load, items }
            });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.pending = Some(rx);
        self.auto_pending = auto;
        self.stale = false;
    }

    /// Whether the start-up check should open the window: an unusable
    /// store, or a migration that is still pending with packages to review.
    fn wants_attention(result: &ReviewResult) -> bool {
        match &result.load {
            StoreLoad::Damaged(_) => true,
            StoreLoad::Missing => !result.items.is_empty(),
            StoreLoad::Loaded(store) => {
                store.legacy_review == LegacyReview::Pending && !result.items.is_empty()
            }
        }
    }
}

fn status_text(status: &TrustStatus) -> String {
    status.label()
}

impl super::PatchworkApp {
    pub(super) fn open_trust_review(&mut self) {
        self.trust_review.open = true;
        self.trust_review.stale = true;
    }

    pub(super) fn render_trust_review(&mut self, ctx: &egui::Context) {
        let window = &mut self.trust_review;
        if !window.auto_started {
            window.auto_started = true;
            if let Ok(root) = self.library_cache.root() {
                window.start(ctx, root, true);
            }
        }
        if let Some(rx) = &window.pending {
            match rx.try_recv() {
                Ok(result) => {
                    window.pending = None;
                    match result {
                        Ok(result) => {
                            if window.auto_pending {
                                if TrustReviewWindow::wants_attention(&result) {
                                    window.open = true;
                                    window.result = Some(result);
                                } else {
                                    // Not a full review; computed again
                                    // when the window is opened.
                                    window.stale = true;
                                }
                            } else {
                                window.result = Some(result);
                            }
                        }
                        Err(error) => window.message = Some(error),
                    }
                    window.auto_pending = false;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => window.pending = None,
            }
        }
        if !window.open {
            return;
        }
        if window.stale && window.pending.is_none() {
            if let Ok(root) = self.library_cache.root() {
                window.start(ctx, root, false);
            }
        }

        let mut open = window.open;
        let mut decision: Option<(std::path::PathBuf, PackageIdentity, ReviewDecision)> = None;
        let mut recheck: Option<std::path::PathBuf> = None;
        let mut set_state: Option<LegacyReview> = None;
        let mut move_aside = false;
        let mut close = false;
        egui::Window::new("Package Trust Review")
            .open(&mut open)
            .default_width(560.0)
            .default_height(420.0)
            .show(ctx, |ui| {
                ui.label(
                    "Patchwork now records where each Library package came from: published \
                     on this computer or installed from the marketplace. Packages that were \
                     already in the Library have no such record. Review them here. A package \
                     id or manifest field is only a claim and is never used as proof.",
                );
                ui.label(
                    "Nothing is blocked in this version: your projects keep working whatever \
                     you decide, and package files are never changed.",
                );
                ui.separator();
                if let Some(message) = &window.message {
                    ui.colored_label(ui.visuals().warn_fg_color, message.as_str());
                }
                if window.pending.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Checking Library packages…");
                    });
                    return;
                }
                let Some(result) = &window.result else {
                    return;
                };
                if let StoreLoad::Damaged(why) = &result.load {
                    ui.colored_label(ui.visuals().error_fg_color, why.as_str());
                    ui.label(
                        "Until this is resolved no package is treated as recorded, and no new \
                         records are saved.",
                    );
                    if ui
                        .button("Move the damaged file aside and start a new trust store")
                        .clicked()
                    {
                        move_aside = true;
                    }
                    return;
                }
                if result.items.is_empty() {
                    ui.label("Every Library package has a current record.");
                }
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        for item in &result.items {
                            ui.group(|ui| {
                                ui.strong(format!("{} — {}@{}", item.name, item.id, item.version));
                                ui.label(status_text(&item.status));
                                ui.weak(format!(
                                    "Manifest claims source \"{}\" (not verified).",
                                    item.claimed_source
                                ));
                                match &item.identity {
                                    Ok((identity, inventory)) => {
                                        let caps: Vec<&str> = inventory
                                            .capabilities
                                            .iter()
                                            .map(|c| c.as_str())
                                            .collect();
                                        ui.label(if caps.is_empty() {
                                            "Uses no privileged capabilities.".to_string()
                                        } else {
                                            format!("Uses: {}", caps.join(", "))
                                        });
                                        if !inventory.restricted.is_empty() {
                                            ui.colored_label(
                                                ui.visuals().warn_fg_color,
                                                format!(
                                                    "Restricted nodes: {}",
                                                    inventory
                                                        .restricted
                                                        .iter()
                                                        .cloned()
                                                        .collect::<Vec<_>>()
                                                        .join(", ")
                                                ),
                                            );
                                        }
                                        if !inventory.unclassified.is_empty() {
                                            ui.colored_label(
                                                ui.visuals().warn_fg_color,
                                                format!(
                                                    "Unknown node types: {}",
                                                    inventory
                                                        .unclassified
                                                        .iter()
                                                        .cloned()
                                                        .collect::<Vec<_>>()
                                                        .join(", ")
                                                ),
                                            );
                                        }
                                        ui.horizontal(|ui| {
                                            if matches!(item.status, TrustStatus::Reclassified(_)) {
                                                if ui
                                                    .button("Re-check capabilities")
                                                    .on_hover_text(
                                                        "Keep its recorded origin and scan it \
                                                         with the current classification.",
                                                    )
                                                    .clicked()
                                                {
                                                    recheck = Some(item.dir.clone());
                                                }
                                            } else {
                                                if ui
                                                    .button("Accept")
                                                    .on_hover_text(
                                                        "Record this exact content as a reviewed \
                                                         package (origin not verified).",
                                                    )
                                                    .clicked()
                                                {
                                                    decision = Some((
                                                        item.dir.clone(),
                                                        identity.clone(),
                                                        ReviewDecision::Accept,
                                                    ));
                                                }
                                                if ui
                                                    .button("Deny")
                                                    .on_hover_text(
                                                        "Record that you do not trust this \
                                                         content.",
                                                    )
                                                    .clicked()
                                                {
                                                    decision = Some((
                                                        item.dir.clone(),
                                                        identity.clone(),
                                                        ReviewDecision::Deny,
                                                    ));
                                                }
                                            }
                                        });
                                    }
                                    Err(why) => {
                                        ui.colored_label(
                                            ui.visuals().warn_fg_color,
                                            format!("Cannot be identified: {why}"),
                                        );
                                    }
                                }
                            });
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .button("Later")
                        .on_hover_text("Ask again the next time Patchwork starts.")
                        .clicked()
                    {
                        close = true;
                    }
                    if ui
                        .button("Don't migrate")
                        .on_hover_text(
                            "Stop asking. The packages stay without a record (untrusted); you \
                             can review them later from Library → Package Trust Review.",
                        )
                        .clicked()
                    {
                        set_state = Some(LegacyReview::Declined);
                    }
                    if ui.button("Done").clicked() {
                        set_state = Some(LegacyReview::Completed);
                    }
                });
            });

        let store = TrustStore::default_location();
        let mut changed = false;
        if let Some((dir, identity, choice)) = decision {
            match store
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|store| apply_review(store, &dir, &identity, choice))
            {
                Ok(record) => {
                    crate::system_log::log(format!(
                        "Trust review: {} recorded ({:?}).",
                        record.identity.label(),
                        record.grant
                    ));
                    self.trust_review.message = None;
                }
                Err(why) => self.trust_review.message = Some(why),
            }
            changed = true;
        }
        if let Some(dir) = recheck {
            match store
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|s| s.reclassify(&dir))
            {
                Ok(record) => crate::system_log::log(format!(
                    "Trust review: {} re-checked under the current classification.",
                    record.identity.label()
                )),
                Err(why) => self.trust_review.message = Some(why),
            }
            changed = true;
        }
        if move_aside {
            match store
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|s| s.move_damaged_aside())
            {
                Ok(path) => {
                    let text = format!(
                        "The damaged trust store was kept as {}. A new one will be started.",
                        path.display()
                    );
                    crate::system_log::warn(text.clone());
                    self.trust_review.message = Some(text);
                }
                Err(why) => self.trust_review.message = Some(why),
            }
            changed = true;
        }
        if let Some(state) = set_state {
            match store
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|s| s.set_legacy_review(state))
            {
                Ok(()) => close = true,
                Err(why) => self.trust_review.message = Some(why),
            }
        }
        if changed {
            self.trust_review.stale = true;
        }
        self.trust_review.open = open && !close;
    }
}
