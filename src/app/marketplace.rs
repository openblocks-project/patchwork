//! P4.3.1A/B: Marketplace catalogue browser and installation.
//!
//! Shows the configured catalogue (or its last good cached copy), with
//! search and per-version compatibility, capabilities and installed state.
//! Browsing and refreshing never install anything. Installing one exact
//! version needs an explicit confirmation in a consent dialog; it then
//! runs on a worker thread (see `library::marketplace::install`) and the
//! new version only appears in the Library palette: it is never inserted
//! into a project or evaluated automatically.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui;

use crate::library::marketplace::browse::{
    self, InstalledIndex, SearchQuery, Tone, capability_label, compatibility_label, install_label,
};
use crate::library::marketplace::cache::{self, CatalogueView, RefreshOutcome, default_cache_dir};
use crate::library::marketplace::fetch::{FetchError, HttpTransport};
use crate::library::marketplace::install::{InstallError, Installed, install};
use crate::library::marketplace::source::CatalogueLocation;
use crate::library::marketplace::{
    Catalogue, CataloguePackage, CatalogueVersion, Publisher, current_app_version,
};
use crate::library::trust::{MarketplaceOrigin, RecordOutcome, TrustStore, install_and_record};
use crate::settings::{MarketplaceSettings, Settings};

/// Results drawn at once (refine the search to see others).
const MAX_SHOWN: usize = 200;

enum Job {
    OpenCached,
    Refresh,
}

struct JobResult {
    location: CatalogueLocation,
    view: CatalogueView,
    outcome: Option<RefreshOutcome>,
}

/// A version the user asked to install, waiting for explicit consent.
#[derive(Debug, Clone)]
struct ConsentRequest {
    package: CataloguePackage,
    version: CatalogueVersion,
    publisher: Option<Publisher>,
    /// P4.3.2C: the verified catalogue (source and revision) the version
    /// was chosen from; recorded as its provenance.
    origin: MarketplaceOrigin,
}

/// An installation running on a worker thread.
struct InstallJob {
    label: String,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<(Installed, RecordOutcome), InstallError>>,
}

#[derive(Default)]
pub(super) struct MarketplaceWindow {
    pub open: bool,
    initialized: bool,
    location: Option<CatalogueLocation>,
    editing_source: bool,
    edit: MarketplaceSettings,
    source_error: Option<String>,
    view: CatalogueView,
    outcome: Option<RefreshOutcome>,
    pending: Option<Receiver<JobResult>>,
    query: SearchQuery,
    /// P4.3.1B: awaiting the user's confirmation.
    consent: Option<ConsentRequest>,
    installing: Option<InstallJob>,
    /// Result of the last installation, shown in the window.
    last_install: Option<(String, Tone)>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn run_job(job: Job, location: &CatalogueLocation) -> JobResult {
    let (view, outcome) = match (job, default_cache_dir()) {
        (Job::OpenCached, Ok(dir)) => (cache::open_cached(&dir, location), None),
        (Job::OpenCached, Err(error)) => (
            CatalogueView {
                cache_problem: Some(error),
                ..CatalogueView::default()
            },
            None,
        ),
        (Job::Refresh, Ok(dir)) => match HttpTransport::github() {
            Ok(transport) => {
                let (view, outcome) = cache::refresh(&transport, &dir, location, now());
                (view, Some(outcome))
            }
            Err(error) => (
                cache::open_cached(&dir, location),
                Some(RefreshOutcome::FetchFailed(error)),
            ),
        },
        (Job::Refresh, Err(error)) => (
            CatalogueView::default(),
            Some(RefreshOutcome::FetchFailed(FetchError::Network(error))),
        ),
    };
    JobResult {
        location: location.clone(),
        view,
        outcome,
    }
}

fn tone_color(ui: &egui::Ui, tone: Tone) -> egui::Color32 {
    match tone {
        Tone::Normal => ui.visuals().text_color(),
        Tone::Good => egui::Color32::from_rgb(110, 200, 130),
        Tone::Warning => egui::Color32::from_rgb(255, 200, 100),
        Tone::Error => egui::Color32::from_rgb(255, 120, 120),
    }
}

fn toned(ui: &mut egui::Ui, text: impl Into<String>, tone: Tone) {
    let color = tone_color(ui, tone);
    ui.label(egui::RichText::new(text.into()).color(color));
}

impl MarketplaceWindow {
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    fn start(&mut self, ctx: &egui::Context, job: Job) {
        let Some(location) = self.location.clone() else {
            return;
        };
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(run_job(job, &location));
            ctx.request_repaint();
        });
        self.pending = Some(rx);
    }

    /// Pick up a finished job (ignored if the source changed meanwhile).
    fn poll(&mut self) {
        let Some(rx) = &self.pending else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.pending = None;
                if self.location.as_ref() == Some(&result.location) {
                    self.view = result.view;
                    self.outcome = result.outcome;
                }
            }
            Err(TryRecvError::Disconnected) => self.pending = None,
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Forget everything shown for the previous source (its catalogue,
    /// refresh result, pending job and any pending consent).
    fn reset(&mut self, location: Option<CatalogueLocation>) {
        self.location = location;
        self.view = CatalogueView::default();
        self.outcome = None;
        self.pending = None;
        self.consent = None;
    }

    fn set_location(&mut self, ctx: &egui::Context, location: Option<CatalogueLocation>) {
        self.reset(location);
        self.start(ctx, Job::OpenCached);
    }

    /// Ask for consent to install `version` (nothing is installed yet).
    fn request_install(&mut self, catalogue: &Catalogue, id: &str, version: &semver::Version) {
        let Some(package) = catalogue.package(id) else {
            return;
        };
        let Some(version) = package.versions.get(version) else {
            return;
        };
        // Only from the catalogue currently shown for the current source.
        let (Some(location), Some(catalogue_sha256)) =
            (self.location.clone(), self.view.catalogue_sha256.clone())
        else {
            return;
        };
        if self.view.catalogue.as_ref() != Some(catalogue) {
            return;
        }
        self.consent = Some(ConsentRequest {
            package: package.clone(),
            version: version.clone(),
            publisher: catalogue.publishers.get(&package.namespace).cloned(),
            origin: MarketplaceOrigin {
                location,
                generated_at: catalogue.generated_at,
                catalogue_sha256,
            },
        });
    }

    /// First open: read the configured source and show its cache.
    fn initialize(&mut self, ctx: &egui::Context) {
        self.initialized = true;
        let settings = Settings::load().marketplace;
        self.edit = settings.clone();
        match CatalogueLocation::from_settings(&settings) {
            Ok(location) => {
                self.editing_source = location.is_none();
                self.set_location(ctx, location);
            }
            Err(error) => {
                self.source_error = Some(error);
                self.editing_source = true;
            }
        }
    }

    fn save_source(&mut self, ctx: &egui::Context) {
        let edit = self.edit.clone();
        let location = if edit.owner.trim().is_empty() && edit.repo.trim().is_empty() {
            None
        } else {
            match CatalogueLocation::new(&edit.owner, &edit.repo, &edit.branch) {
                Ok(location) => Some(location),
                Err(error) => {
                    self.source_error = Some(error);
                    return;
                }
            }
        };
        let mut settings = Settings::load();
        settings.marketplace = match &location {
            Some(l) => MarketplaceSettings {
                owner: l.source().owner().to_string(),
                repo: l.source().repo().to_string(),
                branch: l.branch().to_string(),
            },
            None => MarketplaceSettings::default(),
        };
        if let Err(error) = settings.save() {
            self.source_error = Some(format!("Cannot save the setting: {error}"));
            return;
        }
        self.source_error = None;
        self.editing_source = location.is_none();
        self.set_location(ctx, location);
    }

    fn source_editor(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(
            "Catalogue repository (GitHub). Patchwork reads \
             raw.githubusercontent.com/<owner>/<repo>/<branch>/index.json \
             and nothing else.",
        );
        egui::Grid::new("marketplace_source").show(ui, |ui| {
            ui.label("Owner");
            ui.text_edit_singleline(&mut self.edit.owner);
            ui.end_row();
            ui.label("Repository");
            ui.text_edit_singleline(&mut self.edit.repo);
            ui.end_row();
            ui.label("Branch");
            ui.add(egui::TextEdit::singleline(&mut self.edit.branch).hint_text("main"));
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                self.save_source(ctx);
            }
            if self.location.is_some() && ui.button("Cancel").clicked() {
                self.editing_source = false;
                self.source_error = None;
            }
        });
        if let Some(error) = &self.source_error {
            toned(ui, error.clone(), Tone::Error);
        }
    }

    fn package_details(
        ui: &mut egui::Ui,
        package: &CataloguePackage,
        publisher: Option<&crate::library::marketplace::Publisher>,
        installed: &InstalledIndex,
        app: &semver::Version,
        can_install: bool,
    ) -> Option<semver::Version> {
        let mut requested = None;
        match publisher {
            Some(p) => ui.label(format!("Publisher: {} (@{})", p.display_name, p.github)),
            None => ui.label(format!("Publisher: {}", package.namespace)),
        };
        ui.label(format!("Author: {}", package.author));
        if !package.tags.is_empty() {
            ui.label(format!("Tags: {}", package.tags.join(", ")));
        }
        if let Some(license) = &package.license {
            ui.label(format!("License: {license}"));
        }
        if let Some(docs) = &package.documentation {
            ui.horizontal(|ui| {
                ui.label("Documentation:");
                ui.add(egui::Label::new(docs.as_str()).selectable(true));
            });
        }
        if !package.description.is_empty() {
            ui.add_space(4.0);
            ui.label(package.description.as_str());
        }
        ui.add_space(4.0);
        egui::Grid::new(("marketplace_versions", &package.id))
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Version");
                ui.strong("Compatibility");
                ui.strong("Capabilities");
                ui.strong("Library");
                ui.label("");
                ui.end_row();
                for version in package.versions.values().rev() {
                    ui.label(version.version.to_string());
                    let (text, tone) = compatibility_label(version, app);
                    toned(ui, text, tone);
                    if version.capabilities.is_empty() {
                        ui.label("None");
                    } else {
                        let labels: Vec<String> =
                            version.capabilities.iter().map(capability_label).collect();
                        toned(ui, labels.join(", "), Tone::Warning);
                    }
                    let state = installed.state(&package.id, &version.version);
                    let (text, tone) = install_label(state);
                    toned(ui, text, tone);
                    let installable = version.is_installable(app);
                    let enabled = can_install && installable && state.is_none();
                    let response = ui.add_enabled(enabled, egui::Button::new("Install…"));
                    let response = if state.is_some() {
                        response.on_disabled_hover_text("This version is already in the Library.")
                    } else if !installable {
                        response.on_disabled_hover_text("Not available for this Patchwork.")
                    } else if !can_install {
                        response.on_disabled_hover_text("Another installation is in progress.")
                    } else {
                        response.on_hover_text(
                            "Review what it can access before anything is downloaded.",
                        )
                    };
                    if response.clicked() {
                        requested = Some(version.version.clone());
                    }
                    ui.end_row();
                }
            });
        requested
    }
}

impl super::PatchworkApp {
    /// Open the Marketplace window.
    pub(super) fn open_marketplace(&mut self) {
        self.marketplace.open = true;
    }

    pub(super) fn render_marketplace(&mut self, ctx: &egui::Context) {
        // An installation keeps running (and is reported) while closed.
        self.poll_install();
        if !self.marketplace.open {
            self.marketplace.consent = None;
            return;
        }
        if !self.marketplace.initialized {
            self.marketplace.initialize(ctx);
        }
        self.marketplace.poll();
        if let Some(report) = self.library_cache.ensure_loaded() {
            self.report_library_migration(&report);
        }
        let installed = InstalledIndex::from_packages(self.library_cache.packages());
        let app = current_app_version();
        let now = now();

        let mut open = self.marketplace.open;
        let mut install_request: Option<(String, semver::Version)> = None;
        let window = &mut self.marketplace;
        egui::Window::new("Marketplace")
            .open(&mut open)
            .default_size([640.0, 520.0])
            .resizable(true)
            .show(ctx, |ui| {
                // Source.
                ui.horizontal(|ui| {
                    match &window.location {
                        Some(location) => ui.label(format!("Catalogue: {}", location.label())),
                        None => ui.label("No catalogue source configured."),
                    };
                    if !window.editing_source && ui.small_button("Change…").clicked() {
                        window.editing_source = true;
                    }
                });
                if window.editing_source {
                    window.source_editor(ui, ctx);
                    ui.separator();
                }
                if window.location.is_none() {
                    return;
                }

                // Status and refresh.
                ui.horizontal(|ui| {
                    let busy = window.busy();
                    if ui
                        .add_enabled(!busy, egui::Button::new("⟳ Refresh"))
                        .on_hover_text("Download the latest catalogue (index.json only)")
                        .clicked()
                    {
                        window.start(ctx, Job::Refresh);
                    }
                    if busy {
                        ui.spinner();
                    }
                });
                let (status, tone) =
                    browse::status_line(&window.view, window.outcome.as_ref(), now);
                toned(ui, status, tone);
                if let Some(job) = &window.installing {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Installing {}…", job.label));
                        if ui.button("Cancel").clicked() {
                            job.cancel.store(true, Ordering::SeqCst);
                        }
                    });
                } else if let Some((message, tone)) = &window.last_install {
                    toned(ui, message.clone(), *tone);
                }
                ui.separator();

                let Some(catalogue) = window.view.catalogue.as_ref() else {
                    return;
                };

                // Search.
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut window.query.text)
                            .hint_text("Search name, id, publisher, tags (tag:audio)")
                            .desired_width(280.0),
                    );
                    ui.checkbox(&mut window.query.compatible_only, "Compatible only");
                    ui.checkbox(&mut window.query.installed_only, "Installed only");
                });
                let found = browse::search(catalogue, &window.query, &app, &installed);
                ui.label(format!(
                    "{} of {} packages",
                    found.len(),
                    catalogue.packages.len()
                ));
                let can_install = window.installing.is_none();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for package in found.iter().take(MAX_SHOWN) {
                        let mark = if installed.has_any(&package.id) {
                            " ✓"
                        } else {
                            ""
                        };
                        egui::CollapsingHeader::new(format!(
                            "{} — {}{mark}",
                            package.name, package.id
                        ))
                        .id_salt(("marketplace_package", &package.id))
                        .show(ui, |ui| {
                            if let Some(version) = MarketplaceWindow::package_details(
                                ui,
                                package,
                                catalogue.publishers.get(&package.namespace),
                                &installed,
                                &app,
                                can_install,
                            ) {
                                install_request = Some((package.id.clone(), version));
                            }
                        });
                    }
                    if found.len() > MAX_SHOWN {
                        ui.label(format!(
                            "Showing the first {MAX_SHOWN}; refine the search to see more."
                        ));
                    }
                });
            });
        self.marketplace.open = open;
        if let Some((id, version)) = install_request {
            if let Some(catalogue) = self.marketplace.view.catalogue.clone() {
                self.marketplace.request_install(&catalogue, &id, &version);
            }
        }
        self.render_install_consent(ctx);
    }

    /// The consent dialog: what is installed, from whom, and what it can
    /// access. Installation starts only on "Install".
    fn render_install_consent(&mut self, ctx: &egui::Context) {
        let Some(request) = self.marketplace.consent.clone() else {
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Install Marketplace Package")
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.heading(request.package.name.as_str());
                egui::Grid::new("marketplace_consent").show(ui, |ui| {
                    ui.label("Package");
                    ui.label(request.package.id.as_str());
                    ui.end_row();
                    ui.label("Version");
                    ui.label(request.version.version.to_string());
                    ui.end_row();
                    ui.label("Publisher");
                    match &request.publisher {
                        Some(p) => ui.label(format!("{} (@{})", p.display_name, p.github)),
                        None => ui.label(request.package.namespace.as_str()),
                    };
                    ui.end_row();
                    ui.label("Author");
                    ui.label(request.package.author.as_str());
                    ui.end_row();
                });
                ui.separator();
                if request.version.capabilities.is_empty() {
                    ui.label("This version declares no privileged capabilities.");
                } else {
                    ui.label("This version can use:");
                    for capability in &request.version.capabilities {
                        toned(
                            ui,
                            format!("• {}", capability_label(capability)),
                            Tone::Warning,
                        );
                    }
                }
                ui.add_space(6.0);
                toned(
                    ui,
                    "Installing does not sandbox nodes. Once you use this node in a \
                     project, it runs with the same access to your computer as Patchwork \
                     itself, including everything listed above. Only install packages \
                     from publishers you trust.",
                    Tone::Warning,
                );
                ui.add_space(4.0);
                ui.label(
                    "Before installing, Patchwork checks the download against the \
                     catalogue checksum, verifies every file and scans every node graph, \
                     and refuses the package if it uses more than it declares. Nothing \
                     runs during installation, and the package is not added to any project.",
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Install").clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            self.marketplace.consent = None;
        } else if confirm {
            self.marketplace.consent = None;
            self.start_install(ctx, request);
        }
    }

    fn start_install(&mut self, ctx: &egui::Context, request: ConsentRequest) {
        if self.marketplace.installing.is_some() {
            return;
        }
        let root = match self.library_cache.root() {
            Ok(root) => root,
            Err(error) => {
                self.toast_warn(format!("Cannot install: {error}"));
                return;
            }
        };
        let label = format!("{}@{}", request.package.id, request.version.version);
        crate::system_log::log(format!("Marketplace: installing {label}"));
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let app = current_app_version();
            let result = HttpTransport::github()
                .map_err(InstallError::Download)
                .and_then(|transport| match TrustStore::default_location() {
                    // P4.3.2C: provenance is recorded only after a
                    // successful installation, for the installed content.
                    Ok(store) => install_and_record(
                        &store,
                        &transport,
                        &root,
                        &request.package,
                        &request.version,
                        &request.origin,
                        &app,
                        &flag,
                    ),
                    Err(error) => install(
                        &transport,
                        &root,
                        &request.package,
                        &request.version,
                        &app,
                        &flag,
                    )
                    .map(|installed| (installed, Err(error))),
                });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.marketplace.last_install = None;
        self.marketplace.installing = Some(InstallJob { label, cancel, rx });
    }

    /// Report a finished installation and refresh the Library palette.
    fn poll_install(&mut self) {
        let Some(job) = &self.marketplace.installing else {
            return;
        };
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(InstallError::Io(
                "the installation stopped unexpectedly".to_string(),
            )),
        };
        let label = job.label.clone();
        self.marketplace.installing = None;
        let (message, tone) = match result {
            Ok((_, Ok(_))) => {
                // Shown in the Library palette on its next scan; nothing is
                // inserted into a project or evaluated.
                self.library_cache.invalidate();
                self.trust_review.invalidate();
                (
                    format!("Installed {label}. It is in the Library palette."),
                    Tone::Good,
                )
            }
            Ok((_, Err(why))) => {
                self.library_cache.invalidate();
                self.trust_review.invalidate();
                (
                    format!(
                        "Installed {label}, but its marketplace origin could not be recorded \
                         ({why}). It is listed in Library → Package Trust Review."
                    ),
                    Tone::Warning,
                )
            }
            Err(InstallError::Cancelled) => (
                format!("Installation of {label} was cancelled."),
                Tone::Normal,
            ),
            Err(error) => (format!("{label} was not installed. {error}"), Tone::Error),
        };
        match tone {
            Tone::Good => {
                crate::system_log::log(format!("Marketplace: {message}"));
                self.toast_info(message.clone());
            }
            Tone::Error | Tone::Warning => {
                crate::system_log::warn(format!("Marketplace: {message}"));
                self.toast_warn(message.clone());
            }
            _ => crate::system_log::log(format!("Marketplace: {message}")),
        }
        self.marketplace.last_install = Some((message, tone));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::marketplace::parse_catalogue;

    const EXAMPLE: &str = include_str!("../../docs/marketplace/index.example.json");

    fn location(branch: &str) -> CatalogueLocation {
        CatalogueLocation::new("patchwork-nodes", "catalogue", branch).unwrap()
    }

    fn catalogue() -> Catalogue {
        parse_catalogue(EXAMPLE.as_bytes(), location("main").source()).unwrap()
    }

    #[test]
    fn results_for_a_previous_source_are_never_shown() {
        let mut window = MarketplaceWindow::default();
        window.reset(Some(location("dev")));
        let (tx, rx) = std::sync::mpsc::channel();
        window.pending = Some(rx);
        tx.send(JobResult {
            location: location("main"),
            view: CatalogueView {
                catalogue: Some(catalogue()),
                fetched_at: Some(5),
                checked_at: Some(5),
                from_cache: true,
                cache_problem: None,
                catalogue_sha256: None,
            },
            outcome: Some(RefreshOutcome::Updated),
        })
        .unwrap();
        window.poll();
        assert!(window.pending.is_none());
        assert!(
            window.view.catalogue.is_none(),
            "another source's catalogue"
        );
        assert!(window.outcome.is_none());

        // The current source's result is shown.
        let (tx, rx) = std::sync::mpsc::channel();
        window.pending = Some(rx);
        tx.send(JobResult {
            location: location("dev"),
            view: CatalogueView {
                catalogue: Some(catalogue()),
                ..CatalogueView::default()
            },
            outcome: None,
        })
        .unwrap();
        window.poll();
        assert!(window.view.catalogue.is_some());
    }

    #[test]
    fn changing_the_source_clears_what_was_shown() {
        let mut window = MarketplaceWindow::default();
        window.reset(Some(location("main")));
        window.view.catalogue = Some(catalogue());
        window.view.catalogue_sha256 = Some("a".repeat(64));
        window.outcome = Some(RefreshOutcome::Updated);
        window.request_install(
            &catalogue(),
            "patchwork.lfo",
            &semver::Version::new(1, 0, 0),
        );
        assert!(window.consent.is_some());
        window.reset(Some(location("dev")));
        assert!(window.view.catalogue.is_none());
        assert!(window.outcome.is_none());
        assert!(
            window.consent.is_none(),
            "no consent carried to another source"
        );
    }

    #[test]
    fn choosing_install_only_asks_for_consent() {
        let catalogue = catalogue();
        let shown = |catalogue: &Catalogue| {
            let mut window = MarketplaceWindow::default();
            window.reset(Some(location("main")));
            window.view.catalogue = Some(catalogue.clone());
            window.view.catalogue_sha256 = Some("a".repeat(64));
            window
        };
        let mut window = shown(&catalogue);
        window.request_install(
            &catalogue,
            "acme-audio.gain",
            &semver::Version::new(1, 2, 0),
        );
        let request = window.consent.as_ref().expect("consent requested");
        assert_eq!(request.package.id, "acme-audio.gain");
        assert_eq!(request.version.version, semver::Version::new(1, 2, 0));
        assert_eq!(request.publisher.as_ref().unwrap().github, "Acme-Audio");
        assert_eq!(request.origin.location, location("main"));
        assert_eq!(request.origin.catalogue_sha256, "a".repeat(64));
        assert_eq!(request.origin.generated_at, catalogue.generated_at);
        assert!(
            window.installing.is_none(),
            "nothing starts without confirmation"
        );
        // P4.3.2C: never from a catalogue that is not the verified one shown.
        let mut window = shown(&catalogue);
        window.view.catalogue_sha256 = None;
        window.request_install(
            &catalogue,
            "acme-audio.gain",
            &semver::Version::new(1, 2, 0),
        );
        assert!(window.consent.is_none(), "no verified catalogue revision");
        let mut window = MarketplaceWindow::default();
        window.request_install(
            &catalogue,
            "acme-audio.gain",
            &semver::Version::new(1, 2, 0),
        );
        assert!(window.consent.is_none(), "no source");
        // Unknown packages or versions are ignored.
        let mut window = shown(&catalogue);
        window.request_install(
            &catalogue,
            "acme-audio.gain",
            &semver::Version::new(9, 9, 9),
        );
        window.request_install(&catalogue, "nobody.nothing", &semver::Version::new(1, 0, 0));
        assert!(window.consent.is_none());
    }
}
