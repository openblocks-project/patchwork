//! P4.0 / P4.1: Library publishing, package insertion, read-only package
//! tabs and Duplicate for Editing.

use super::*;
use crate::library::index::InstalledVersion;
use crate::library::package::{self, PublishRequest};

/// Where the publish dialog will put the new version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PublishTarget {
    /// New `local.<slug>` package.
    NewPackage,
    /// New version of an existing package (its id is kept).
    Existing(String),
}

/// State of the "Publish to Library" dialog.
#[derive(Debug, Clone)]
pub(super) struct PublishDialog {
    source: std::path::PathBuf,
    /// Converting an unversioned Library node (copy-only).
    converting: bool,
    name: String,
    target: PublishTarget,
    version: String,
    description: String,
    author: String,
    tags: String,
    error: Option<String>,
}

/// Palette actions on Library entries, applied after the palette closure.
pub(super) enum LibraryAction {
    Insert(Box<InstalledVersion>),
    Convert(std::path::PathBuf),
    Duplicate(std::path::PathBuf),
    /// P4.3.3: prepare a marketplace submission for this version.
    Share(Box<InstalledVersion>),
}

impl PatchworkApp {
    // ── Publishing ───────────────────────────────────────────────────────

    /// File → Publish to Library for the active custom-node tab.
    /// Requires a saved custom-node tab with no unsaved changes.
    pub(super) fn publish_active_custom_node(&mut self) {
        use crate::document::DocumentKind;

        let document = self.documents.active();
        let has_unsaved_changes = self.is_dirty || document.is_dirty;
        let read_only = document.read_only;
        let path = document.path.clone();
        if document.kind != DocumentKind::CustomNode {
            self.publish_failed("Only custom-node tabs can be published to the Library.");
            return;
        }
        if read_only {
            self.publish_failed(
                "This tab is already a published Library version. \
                 Use Duplicate for Editing, change the copy, then publish it.",
            );
            return;
        }
        let Some(path) = path.filter(|_| !has_unsaved_changes) else {
            self.publish_failed("Save the custom node before publishing it to the Library.");
            return;
        };

        // Validate the saved file rather than potentially unsaved graph state.
        let custom_node = match crate::document::CustomNodeFile::load(&path) {
            Ok(node) => node,
            Err(error) => {
                self.publish_failed(&format!("Cannot read the custom node: {error}"));
                return;
            }
        };
        let has_nested_nodes = custom_node
            .graph
            .nodes
            .values()
            .any(|node| matches!(&node.node_type, NodeType::CustomNodeInstance { .. }));
        // P4.2 Stage 3: nested custom nodes are bundled. Check them now so
        // the user learns which nodes must be published first before
        // filling in the dialog (publishing checks again).
        if has_nested_nodes {
            let checked = self.library_cache.root().and_then(|root| {
                crate::library::bundle::collect_dependencies(&custom_node, &root).map(|_| ())
            });
            if let Err(error) = checked {
                self.publish_failed(&error);
                return;
            }
        }
        self.open_publish_dialog(path, custom_node.name, false);
    }

    /// Open the publish dialog, pre-selecting an existing package with the
    /// same name or derived id (new version) or a new package at 1.0.0.
    pub(super) fn open_publish_dialog(
        &mut self,
        source: std::path::PathBuf,
        name: String,
        converting: bool,
    ) {
        if let Some(report) = self.library_cache.ensure_loaded() {
            self.report_library_migration(&report);
        }
        let derived = package::local_package_id(&name).ok();
        let existing = self.library_cache.packages().iter().find(|p| {
            Some(&p.id) == derived.as_ref()
                || p.latest().manifest.name.eq_ignore_ascii_case(name.trim())
        });
        let (target, version, description, author, tags) = match existing {
            Some(pkg) => {
                let latest = &pkg.latest().manifest;
                (
                    PublishTarget::Existing(pkg.id.clone()),
                    package::bump_patch(&pkg.latest().version).to_string(),
                    latest.description.clone(),
                    latest.author.clone(),
                    latest.tags.join(", "),
                )
            }
            None => (
                PublishTarget::NewPackage,
                "1.0.0".to_string(),
                String::new(),
                String::new(),
                String::new(),
            ),
        };
        self.publish_dialog = Some(PublishDialog {
            source,
            converting,
            name,
            target,
            version,
            description,
            author,
            tags,
            error: None,
        });
    }

    /// Validate the dialog fields. Returns the request or a reason.
    fn validate_publish_dialog(&self, dialog: &PublishDialog) -> Result<PublishRequest, String> {
        let version = package::parse_stable_version(&dialog.version)?;
        let existing_id = match &dialog.target {
            PublishTarget::NewPackage => {
                let id = package::local_package_id(&dialog.name)?;
                if self.library_cache.packages().iter().any(|p| p.id == id) {
                    return Err(format!(
                        "Package {id} already exists. Choose it as the target to publish a new version."
                    ));
                }
                None
            }
            PublishTarget::Existing(id) => {
                let pkg = self.library_cache.packages().iter().find(|p| &p.id == id);
                if let Some(pkg) = pkg {
                    if pkg.versions.iter().any(|v| v.version == version) {
                        return Err(format!("{id} {version} already exists."));
                    }
                }
                Some(id.clone())
            }
        };
        if dialog.name.trim().is_empty() {
            return Err("Enter a name.".to_string());
        }
        // L2: a version folder may exist even if it is damaged and therefore
        // not listed; it is never overwritten, so ask for another version.
        let id = match &existing_id {
            Some(id) => id.clone(),
            None => package::local_package_id(&dialog.name)?,
        };
        if let Ok(root) = self.library_cache.root() {
            if package::version_dir(&root, &id, &version).exists() {
                return Err(format!(
                    "A folder for {id} {version} already exists (it may be damaged). Choose another version."
                ));
            }
        }
        Ok(PublishRequest {
            existing_id,
            name: dialog.name.trim().to_string(),
            version: version.to_string(),
            description: dialog.description.clone(),
            author: dialog.author.clone(),
            tags: package::parse_tags(&dialog.tags),
        })
    }

    /// Draw the publish dialog.
    pub(super) fn render_publish_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.publish_dialog.take() else {
            return;
        };
        let validation = self.validate_publish_dialog(&dialog);
        let latest_for_target = match &dialog.target {
            PublishTarget::Existing(id) => self
                .library_cache
                .packages()
                .iter()
                .find(|p| &p.id == id)
                .map(|p| p.latest().version.clone()),
            PublishTarget::NewPackage => None,
        };
        let package_ids: Vec<(String, String)> = self
            .library_cache
            .packages()
            .iter()
            .map(|p| (p.id.clone(), p.latest().manifest.name.clone()))
            .collect();

        let mut open = !ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let mut publish = false;
        egui::Modal::new(egui::Id::new("publish_library_modal")).show(ctx, |ui| {
            ui.heading(if dialog.converting {
                "Convert to Library Package"
            } else {
                "Publish to Library"
            });
            ui.label(
                egui::RichText::new(dialog.source.display().to_string())
                    .small()
                    .weak(),
            );
            if dialog.converting {
                ui.label(
                    egui::RichText::new("The original unversioned file is kept unchanged.")
                        .small(),
                );
            }
            ui.add_space(6.0);
            egui::Grid::new("publish_library_grid")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    ui.add(egui::TextEdit::singleline(&mut dialog.name).desired_width(260.0));
                    ui.end_row();

                    ui.label("Package");
                    let new_label = format!(
                        "New package ({})",
                        package::local_package_id(&dialog.name)
                            .unwrap_or_else(|_| "local.…".to_string())
                    );
                    let selected = match &dialog.target {
                        PublishTarget::NewPackage => new_label.clone(),
                        PublishTarget::Existing(id) => format!("New version of {id}"),
                    };
                    egui::ComboBox::from_id_salt("publish_target")
                        .selected_text(selected)
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut dialog.target,
                                PublishTarget::NewPackage,
                                new_label,
                            );
                            for (id, name) in &package_ids {
                                ui.selectable_value(
                                    &mut dialog.target,
                                    PublishTarget::Existing(id.clone()),
                                    format!("New version of {name} ({id})"),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label("Version");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut dialog.version).desired_width(90.0));
                        if let Some(latest) = &latest_for_target {
                            ui.label(egui::RichText::new(format!("latest {latest}")).small().weak());
                        }
                    });
                    ui.end_row();

                    ui.label("Description");
                    ui.add(egui::TextEdit::singleline(&mut dialog.description).desired_width(260.0));
                    ui.end_row();

                    ui.label("Author");
                    ui.add(egui::TextEdit::singleline(&mut dialog.author).desired_width(260.0));
                    ui.end_row();

                    ui.label("Tags");
                    ui.add(
                        egui::TextEdit::singleline(&mut dialog.tags)
                            .desired_width(260.0)
                            .hint_text("comma separated"),
                    );
                    ui.end_row();
                });

            let lower = match (&latest_for_target, package::parse_stable_version(&dialog.version)) {
                (Some(latest), Ok(v)) => v < *latest,
                _ => false,
            };
            let red = egui::Color32::from_rgb(255, 120, 120);
            if let Err(message) = &validation {
                ui.label(egui::RichText::new(message).small().color(red));
            } else if lower {
                ui.label(
                    egui::RichText::new("This version is lower than the latest one.")
                        .small()
                        .color(egui::Color32::from_rgb(255, 200, 100)),
                );
            }
            if let Some(error) = &dialog.error {
                ui.label(egui::RichText::new(error).small().color(red));
            }
            ui.label(
                egui::RichText::new(
                    "Published versions are read-only; existing projects keep the version they use.",
                )
                .small()
                .weak(),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(validation.is_ok(), egui::Button::new("Publish"))
                    .clicked()
                {
                    publish = true;
                }
                if ui.button("Cancel").clicked() {
                    open = false;
                }
            });
        });

        if open && publish {
            if let Ok(request) = self.validate_publish_dialog(&dialog) {
                match self.publish_package_request(&dialog.source, &request) {
                    Ok(()) => open = false,
                    Err(error) => dialog.error = Some(error),
                }
            }
        }
        if open {
            self.publish_dialog = Some(dialog);
        }
    }

    fn publish_package_request(
        &mut self,
        source: &std::path::Path,
        request: &PublishRequest,
    ) -> Result<(), String> {
        let root = self.library_cache.root()?;
        // P4.3.2C: record `local_publish` provenance for the exact content
        // published. A failure to record never undoes the publish; the
        // version is then listed in Package Trust Review.
        let result = match crate::library::trust::TrustStore::default_location() {
            Ok(store) => crate::library::trust::publish_and_record(&store, &root, source, request),
            Err(error) => package::publish_package(&root, source, request).map(|p| (p, Err(error))),
        };
        match result {
            Ok((published, recorded)) => {
                self.library_cache.invalidate();
                self.trust_review.invalidate();
                if let Err(why) = recorded {
                    let text = format!(
                        "Published {}@{}, but its provenance could not be recorded: {why}",
                        published.manifest.id, published.version
                    );
                    crate::system_log::warn(text.clone());
                    self.toast_warn(text);
                }
                crate::system_log::log(format!(
                    "Published {}@{} to the Library: {}",
                    published.manifest.id,
                    published.version,
                    published.dir.display()
                ));
                self.toast_info(format!(
                    "Published '{}' {} to the Library.",
                    published.manifest.name, published.version
                ));
                Ok(())
            }
            Err(error) => {
                let message = error.to_string();
                crate::system_log::error(format!("Publish to Library failed: {message}"));
                Err(message)
            }
        }
    }

    /// P4.0 flat publish (kept for compatibility; the UI now publishes
    /// versioned packages).
    #[allow(dead_code)]
    pub(super) fn publish_custom_node_to_library(&mut self, path: &std::path::Path) {
        match crate::library::publish_to_library(path) {
            Ok(destination) => {
                self.library_cache.invalidate();
                crate::system_log::log(format!("Published to Library: {}", destination.display()));
                let name = destination
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.toast_info(format!("Published '{name}' to the Library."));
            }
            Err(error) => self.publish_failed(&error.to_string()),
        }
    }

    fn publish_failed(&mut self, message: &str) {
        crate::system_log::error(format!("Publish to Library failed: {message}"));
        self.toast_error(format!("Publish to Library failed: {message}"));
    }

    /// Tell the user about a legacy Library migration (details are in the
    /// System Log).
    pub(super) fn report_library_migration(&mut self, report: &crate::library::MigrationReport) {
        let Some(summary) = report.summary() else {
            return;
        };
        if report.has_problems() || !report.renamed.is_empty() {
            self.toast_warn(summary);
        } else {
            self.toast_info(summary);
        }
    }

    // ── Palette actions ──────────────────────────────────────────────────

    pub(super) fn apply_library_action(&mut self, ctx: &egui::Context, action: LibraryAction) {
        match action {
            LibraryAction::Insert(version) => self.insert_library_package(ctx, &version),
            LibraryAction::Convert(path) => {
                if !path.is_file() {
                    self.library_cache.invalidate();
                    self.toast_warn(format!(
                        "Custom node file no longer exists: {}",
                        path.display()
                    ));
                    return;
                }
                let name = crate::document::CustomNodeFile::load(&path)
                    .map(|f| f.name)
                    .unwrap_or_else(|_| {
                        path.file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    });
                self.open_publish_dialog(path, name, true);
            }
            LibraryAction::Duplicate(path) => self.duplicate_for_editing(path),
            LibraryAction::Share(version) => self.open_share_dialog(*version),
        }
    }

    /// Insert a package version after re-verifying its checksum.
    fn insert_library_package(&mut self, ctx: &egui::Context, version: &InstalledVersion) {
        match package::verify_version_dir_static(&version.dir, true) {
            Ok(verified) => {
                // P4.2 Stage 2: record the exact package version portably.
                let reference = crate::library::resolve::InstancePackageRef {
                    id: verified.manifest.id.clone(),
                    version: verified.version.to_string(),
                    entry: verified.manifest.entry.clone(),
                };
                self.insert_custom_node_with_package(ctx, &verified.entry_path, Some(reference));
            }
            Err(error) => {
                self.library_cache.invalidate();
                crate::system_log::error(format!("Cannot insert Library package: {error}"));
                self.toast_error(error);
            }
        }
    }

    // ── Read-only package tabs ───────────────────────────────────────────

    /// Banner above the canvas for a read-only package tab.
    pub(super) fn render_read_only_banner(&mut self, ctx: &egui::Context) {
        let document = self.documents.active();
        if !document.read_only {
            return;
        }
        let Some(path) = document.path.clone() else {
            return;
        };
        let mut duplicate = false;
        egui::TopBottomPanel::top("read_only_package_banner").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(
                        "🔒 Published Library version — read-only. \
                         Projects using it are never changed.",
                    )
                    .color(egui::Color32::from_rgb(255, 200, 100)),
                );
                if ui.button("Duplicate for Editing…").clicked() {
                    duplicate = true;
                }
            });
        });
        if duplicate {
            self.duplicate_for_editing(path);
        }
    }

    /// Copy a (published) custom node to a user-chosen file and open the
    /// copy in an editable tab.
    pub(super) fn duplicate_for_editing(&mut self, source: std::path::PathBuf) {
        // Creates and opens a document: not while the quit dialog is open.
        if self.documents.is_locked() {
            return;
        }
        // P4.3.2D: a duplicate is a plain, editable file that runs without
        // package permissions, so a package may only be duplicated once it
        // is allowed to run (and copying it constructs its nodes).
        // P4.3.2E2: the copy keeps exactly the package's rights.
        let mut package_rights = crate::capability::CapSet::EMPTY;
        if crate::library::package::is_inside_published_package(&source) {
            let policy = super::permissions::editor_trust_policy();
            let mut resolver =
                crate::library::resolve::PackageResolver::new(None, self.library_cache.root().ok());
            resolver.set_trust(policy.clone());
            match crate::library::trust::authorize::authorize_entry(&policy, &source, &resolver) {
                Ok(authorized) => package_rights = authorized.capabilities,
                Err(blocked) => {
                    let message = blocked.message();
                    crate::system_log::warn(format!("Not duplicated: {message}"));
                    self.toast_warn(message);
                    self.permissions.add_refused(blocked);
                    self.permissions.open = true;
                    return;
                }
            }
        }
        let stem = source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Custom Node".to_string());
        let mut dialog = rfd::FileDialog::new()
            .set_title("Duplicate for Editing")
            .add_filter("Patchwork Custom Node", &["pwnode"])
            .set_file_name(format!("{stem} copy.pwnode"));
        let start_dir = self
            .project_path
            .as_ref()
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
            .or_else(dirs::document_dir);
        if let Some(dir) = start_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(destination) = dialog.save_file() else {
            return;
        };
        let destination = crate::graph::group::with_pwnode_extension(destination);

        let mut overwrite = false;
        if destination.exists() {
            let canonical = std::fs::canonicalize(&destination).ok();
            let open_in_tab = canonical.is_some()
                && self.documents.documents.iter().any(|doc| {
                    doc.path
                        .as_ref()
                        .and_then(|p| std::fs::canonicalize(p).ok())
                        == canonical
                });
            if open_in_tab {
                self.toast_error("That file is open in a tab. Close it or choose another file.");
                return;
            }
            let confirmed = rfd::MessageDialog::new()
                .set_title("Replace File?")
                .set_description(format!(
                    "{} already exists. Replace it with the copy?",
                    destination.display()
                ))
                .set_level(rfd::MessageLevel::Warning)
                .set_buttons(rfd::MessageButtons::YesNo)
                .show();
            if confirmed != rfd::MessageDialogResult::Yes {
                return;
            }
            overwrite = true;
        }

        match package::duplicate_for_editing_with_digest(&source, &destination, overwrite) {
            Ok((source_digest, copy_digest)) => {
                // P4.3.2E1: the copy inherits the provenance of its source:
                // an authorized package (checked above) or a loose file as
                // recorded in the content ledger.
                let source_state = if package::is_inside_published_package(&source) {
                    crate::library::trust::content::ContentState::Allowed(package_rights)
                } else {
                    let mut state = crate::library::trust::content::ContentState::Pending {
                        kind: crate::library::trust::content::ContentKind::CustomNode,
                        digest: source_digest,
                    };
                    super::provenance::resolve(&mut state);
                    state
                };
                super::provenance::record_written(
                    &source_state,
                    crate::library::trust::content::ContentKind::CustomNode,
                    &copy_digest,
                );
                crate::system_log::log(format!(
                    "Duplicated {} for editing as {}",
                    source.display(),
                    destination.display()
                ));
                self.project_custom_nodes.push(destination.clone());
                self.documents.clear_resolved_paths();
                self.open_custom_node_for_edit(destination);
                self.toast_info("Editable copy created. Publish it as a new version when ready.");
            }
            Err(error) => {
                crate::system_log::error(format!("Duplicate for Editing failed: {error}"));
                self.toast_error(format!("Duplicate for Editing failed: {error}"));
            }
        }
    }
}
