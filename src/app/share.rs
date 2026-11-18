//! P4.3.3: Library → (package) → Share to Marketplace.
//!
//! Prepares a submission folder for the public catalogue repository from a
//! version published on this computer. Patchwork never signs in to GitHub
//! and never asks for a password, token or login: the publisher submits
//! the folder through a pull request in the browser, where GitHub
//! identifies them and the repository's review and CI decide. See
//! `library::marketplace::submit`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use eframe::egui;

use crate::library::index::InstalledVersion;
use crate::library::marketplace::source::CatalogueLocation;
use crate::library::marketplace::submit::{
    Prepared, ShareRequest, contribution_url, describe_dependencies, prepare, request_from,
    write_submission,
};

/// The Share to Marketplace window.
pub(super) struct ShareWindow {
    version: InstalledVersion,
    namespace: String,
    name: String,
    display_name: String,
    description: String,
    author: String,
    tags: String,
    license: String,
    documentation: String,
    min_patchwork: String,
    preview: Option<PathBuf>,
    /// The result of the last check (cleared by any change).
    checked: Option<Result<Prepared, String>>,
    /// The folder written, or why not.
    exported: Option<Result<PathBuf, String>>,
}

impl ShareWindow {
    pub(super) fn new(version: InstalledVersion) -> Self {
        let defaults = request_from(&version.manifest);
        Self {
            namespace: defaults.namespace,
            name: defaults.name,
            display_name: defaults.display_name,
            description: defaults.description,
            author: defaults.author,
            tags: defaults.tags.join(", "),
            license: String::new(),
            documentation: String::new(),
            min_patchwork: defaults.min_patchwork.to_string(),
            preview: None,
            checked: None,
            exported: None,
            version,
        }
    }

    fn request(&self) -> Result<ShareRequest, String> {
        let optional = |text: &str| {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        };
        Ok(ShareRequest {
            namespace: self.namespace.trim().to_string(),
            name: self.name.trim().to_string(),
            display_name: self.display_name.trim().to_string(),
            description: self.description.trim().to_string(),
            author: self.author.trim().to_string(),
            tags: crate::library::package::parse_tags(&self.tags),
            license: optional(&self.license),
            documentation: optional(&self.documentation),
            min_patchwork: crate::library::package::parse_stable_version(self.min_patchwork.trim())
                .map_err(|e| format!("Minimum Patchwork: {e}"))?,
            preview: self.preview.clone(),
        })
    }
}

/// The configured marketplace catalogue and its cached copy.
fn configured_catalogue() -> Result<
    (
        CatalogueLocation,
        Option<crate::library::marketplace::Catalogue>,
    ),
    String,
> {
    let location =
        CatalogueLocation::from_settings(&crate::settings::Settings::load().marketplace)?
            .ok_or_else(|| {
                "No marketplace catalogue is configured. Choose one in Library → Marketplace first."
                    .to_string()
            })?;
    let catalogue = crate::library::marketplace::cache::default_cache_dir()
        .ok()
        .and_then(|dir| crate::library::marketplace::cache::open_cached(&dir, &location).catalogue);
    Ok((location, catalogue))
}

fn check(window: &ShareWindow) -> Result<Prepared, String> {
    let request = window.request()?;
    let (location, catalogue) = configured_catalogue()?;
    let store = match super::permissions::editor_trust_policy() {
        crate::library::trust::authorize::TrustPolicy::Store(store) => store,
        _ => return Err("Permissions are unavailable (no home folder).".to_string()),
    };
    prepare(
        &window.version.dir,
        &request,
        &store,
        &location,
        catalogue.as_ref(),
        &crate::library::marketplace::current_app_version(),
    )
}

impl super::PatchworkApp {
    pub(super) fn open_share_dialog(&mut self, version: InstalledVersion) {
        self.share = Some(ShareWindow::new(version));
    }

    pub(super) fn render_share_dialog(&mut self, ctx: &egui::Context) {
        let Some(window) = self.share.as_mut() else {
            return;
        };
        let mut open = true;
        let mut run_check = false;
        let mut export = false;
        let mut open_guide = false;
        let mut open_folder: Option<PathBuf> = None;
        let manifest = window.version.manifest.clone();
        egui::Window::new("Share to Marketplace")
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} — {}@{} (published on this computer)",
                    manifest.name, manifest.id, window.version.version
                ));
                ui.label(
                    "Sharing prepares a submission folder for the public catalogue. You submit \
                     it yourself through a GitHub pull request in your browser; Patchwork never \
                     asks for your GitHub password, token or login. Nothing in the package is \
                     run.",
                );
                ui.separator();
                let mut changed = false;
                egui::Grid::new("share_form").num_columns(2).show(ui, |ui| {
                    let mut field = |ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str| {
                        ui.label(label);
                        changed |= ui
                            .add(egui::TextEdit::singleline(value).hint_text(hint).desired_width(320.0))
                            .changed();
                        ui.end_row();
                    };
                    field(ui, "Namespace", &mut window.namespace, "your GitHub login, lowercase");
                    field(ui, "Package name", &mut window.name, "e.g. stereo-gain");
                    field(ui, "Display name", &mut window.display_name, "");
                    field(ui, "Author", &mut window.author, "");
                    field(ui, "Tags", &mut window.tags, "comma-separated");
                    field(ui, "License", &mut window.license, "SPDX, e.g. MIT (optional)");
                    field(ui, "Documentation", &mut window.documentation, "https://… (optional)");
                    field(ui, "Minimum Patchwork", &mut window.min_patchwork, "");
                    ui.label("Description");
                    changed |= ui
                        .add(egui::TextEdit::multiline(&mut window.description).desired_rows(3).desired_width(320.0))
                        .changed();
                    ui.end_row();
                    ui.label("Preview");
                    ui.horizontal(|ui| {
                        match &window.preview {
                            Some(path) => ui.label(path.display().to_string()),
                            None => ui.weak("none (optional)"),
                        };
                        if ui.small_button("Choose…").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("Image", &["png", "jpg", "jpeg", "webp"])
                                .pick_file()
                            {
                                window.preview = Some(path);
                                changed = true;
                            }
                        }
                        if window.preview.is_some() && ui.small_button("Remove").clicked() {
                            window.preview = None;
                            changed = true;
                        }
                    });
                    ui.end_row();
                });
                ui.weak(format!(
                    "Public id: {}.{} — the namespace must be your GitHub login. Patchwork does \
                     not check that you own it; the catalogue repository does, from the account \
                     that opens the pull request. The local package {} is not changed.",
                    window.namespace.trim(),
                    window.name.trim(),
                    manifest.id
                ));
                if changed {
                    window.checked = None;
                    window.exported = None;
                }
                ui.separator();
                if ui.button("Check").clicked() {
                    run_check = true;
                }
                match &window.checked {
                    None => {}
                    Some(Err(why)) => {
                        ui.colored_label(ui.visuals().error_fg_color, why);
                    }
                    Some(Ok(prepared)) => {
                        let version = &prepared.listing.versions[0];
                        egui::Grid::new("share_summary").num_columns(2).show(ui, |ui| {
                            let mut row = |label: &str, value: String| {
                                ui.label(label);
                                ui.label(value);
                                ui.end_row();
                            };
                            row("Package", format!("{}@{}", prepared.public_id, prepared.version));
                            row("Name", prepared.listing.name.clone());
                            row("Publisher", format!("namespace '{}' (author {})", prepared.namespace, prepared.listing.author));
                            row("Tags", if prepared.listing.tags.is_empty() { "none".into() } else { prepared.listing.tags.join(", ") });
                            row(
                                "Capabilities",
                                crate::capability::CapSet::from_iter(prepared.capabilities.iter().copied()).describe(),
                            );
                            row("Dependencies", describe_dependencies(prepared));
                            row(
                                "Compatibility",
                                format!(
                                    "Patchwork {} or newer; package format {}, bundle format {}",
                                    version.min_patchwork, version.manifest_format, version.bundle_format
                                ),
                            );
                            row("Documentation", prepared.listing.documentation.clone().unwrap_or_else(|| "none".into()));
                            row("Preview", prepared.preview.as_ref().map(|(n, b)| format!("{n} ({} bytes)", b.len())).unwrap_or_else(|| "none".into()));
                            row("Artifact", format!("{} bytes, {} files", prepared.size, prepared.file_count));
                        });
                        ui.monospace(format!("SHA-256 {}", prepared.sha256));
                        for note in &prepared.notes {
                            ui.colored_label(ui.visuals().warn_fg_color, note);
                        }
                        if ui.button("Create submission folder…").clicked() {
                            export = true;
                        }
                    }
                }
                match &window.exported {
                    Some(Ok(path)) => {
                        ui.label(format!("Written to {}", path.display()));
                        if ui.button("Open the folder").clicked() {
                            open_folder = Some(path.clone());
                        }
                        ui.label(
                            "Next: fork the catalogue repository, copy the folder's \
                             repository/ contents into your fork, and open a pull request. \
                             SUBMISSION.md in the folder has the exact steps.",
                        );
                    }
                    Some(Err(why)) => {
                        ui.colored_label(ui.visuals().error_fg_color, why);
                    }
                    None => {}
                }
                ui.separator();
                if ui.button("Open the contribution guide in your browser").clicked() {
                    open_guide = true;
                }
            });

        let Some(window) = self.share.as_mut() else {
            return;
        };
        if run_check {
            window.checked = Some(check(window));
            window.exported = None;
        }
        if export {
            if let (Some(Ok(prepared)), Some(dir)) =
                (&window.checked, rfd::FileDialog::new().pick_folder())
            {
                let result = configured_catalogue().and_then(|(location, _)| {
                    write_submission(prepared, &location, &dir, &AtomicBool::new(false))
                });
                match &result {
                    Ok(path) => crate::system_log::log(format!(
                        "Marketplace submission for {}@{} written to {}.",
                        prepared.public_id,
                        prepared.version,
                        path.display()
                    )),
                    Err(why) => {
                        crate::system_log::warn(format!("Marketplace submission failed: {why}"))
                    }
                }
                window.exported = Some(result);
            }
        }
        if open_guide {
            match configured_catalogue() {
                Ok((location, _)) => {
                    crate::external_process::open_externally(contribution_url(&location))
                }
                Err(why) => window.exported = Some(Err(why)),
            }
        }
        if let Some(path) = open_folder {
            crate::external_process::open_externally(path);
        }
        if !open {
            self.share = None;
        }
    }
}
