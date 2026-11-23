//! P6 Stage 2.3: File → Export as App → Windows Portable…
//!
//! The dialog collects what [`export::export_portable`] needs (application
//! name, destination, Video Out, ffmpeg when the project needs it), shows
//! every problem before anything is written, and runs the export on its own
//! thread ([`export::ExportJob`]) so large media never freezes the editor.
//! Only one export runs at a time; an existing export is only replaced
//! after an explicit confirmation; cancelling or failing leaves nothing
//! behind. The open project is only read.

use std::path::{Path, PathBuf};

use eframe::egui;

use super::export::{
    self, ExportEnvironment, ExportJob, ExportPurpose, ExportReport, ExportRequest, FfmpegSource,
    RuntimeSource,
};
use crate::graph::{Graph, NodeId};

/// What the form says about the chosen export folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Existing {
    Nothing,
    /// An earlier Patchwork export (has `player.json`): replaced only after
    /// confirmation.
    EarlierExport,
    /// Something else: never replaced.
    Other,
}

/// The dialog's fields (no UI).
#[derive(Debug, Clone, Default)]
pub(super) struct ExportForm {
    /// The saved project folder.
    pub source: PathBuf,
    pub app_name: String,
    /// The folder the export folder (named after the app) is created in.
    pub parent: Option<PathBuf>,
    /// The project's Video Out (Window / Display) nodes.
    pub outputs: Vec<NodeId>,
    pub output: Option<NodeId>,
    pub needs_ffmpeg: bool,
    pub ffmpeg: Option<PathBuf>,
    pub ffmpeg_licences: Vec<PathBuf>,
    pub replace_confirmed: bool,
    /// P6 Stage 2.6: distributable (default) or internal test build.
    pub purpose: ExportPurpose,
}

/// The suffix of an internal test build's folder, so it is never mistaken
/// for (or replaces) a distributable export of the same app.
pub(super) const INTERNAL_TEST_FOLDER_SUFFIX: &str = " (INTERNAL TEST - NOT FOR DISTRIBUTION)";

impl ExportForm {
    /// The form for the project saved at `source`, whose graph is `graph`.
    pub fn for_project(source: &Path, graph: &Graph) -> Self {
        let outputs = super::player::window_outputs(graph);
        let output = match outputs.as_slice() {
            [only] => Some(*only),
            _ => None,
        };
        let app_name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            source: source.to_path_buf(),
            app_name,
            // Chosen by the user (step 4): the project's own folder would
            // collide with the export, which is named after the project.
            parent: None,
            outputs,
            output,
            needs_ffmpeg: export::needs_ffmpeg(graph),
            ..Self::default()
        }
    }

    /// The export folder: `<parent>\<application name>`, labelled for an
    /// internal test build.
    pub fn destination(&self) -> Option<PathBuf> {
        let parent = self.parent.as_ref()?;
        (!self.app_name.is_empty()).then(|| match self.purpose {
            ExportPurpose::Distribution => parent.join(&self.app_name),
            ExportPurpose::InternalTest => {
                parent.join(format!("{}{INTERNAL_TEST_FOLDER_SUFFIX}", self.app_name))
            }
        })
    }

    pub fn existing(&self) -> Existing {
        let Some(dest) = self.destination() else {
            return Existing::Nothing;
        };
        match std::fs::symlink_metadata(&dest) {
            Err(_) => Existing::Nothing,
            Ok(meta)
                if meta.is_dir()
                    && !meta.file_type().is_symlink()
                    && super::player::manifest::manifest_present(&dest) =>
            {
                Existing::EarlierExport
            }
            Ok(_) => Existing::Other,
        }
    }

    /// What stops the export from starting (empty: it can start). The
    /// exporter checks everything again, and more, before writing.
    pub fn problems(&self, unsaved: bool, runtime: &Result<RuntimeSource, String>) -> Vec<String> {
        let mut out = Vec::new();
        if unsaved {
            out.push(
                "The project has unsaved changes. Save it first: the saved project is what \
                 is exported."
                    .to_string(),
            );
        }
        if let Err(e) = export::check_app_name(&self.app_name) {
            out.push(e);
        }
        match &self.parent {
            None => out.push("Choose where to create the export.".to_string()),
            Some(parent) if !parent.is_dir() => {
                out.push(format!("The folder {} does not exist.", parent.display()))
            }
            Some(_) => {}
        }
        if let Some(dest) = self.destination() {
            if dest.starts_with(&self.source) || self.source.starts_with(&dest) {
                out.push(
                    "The export folder cannot be the project folder, or inside it. Choose \
                     another location or name."
                        .to_string(),
                );
            }
        }
        match (self.outputs.as_slice(), self.output) {
            ([], _) => out.push(
                "The project has no Video Out node with the Window / Display destination, so \
                 the app would show nothing. Add one and save."
                    .to_string(),
            ),
            (_, None) => out.push(
                "The project has several Video Out (Window / Display) nodes. Choose the one \
                 the app shows."
                    .to_string(),
            ),
            (outputs, Some(id)) if !outputs.contains(&id) => {
                out.push(format!("Video Out #{id} is not in the project."))
            }
            _ => {}
        }
        if self.needs_ffmpeg {
            if self.ffmpeg.is_none() {
                out.push(
                    "The project plays or captures video, which needs ffmpeg. Choose a \
                     redistributable ffmpeg.exe; none is downloaded or taken from this \
                     computer."
                        .to_string(),
                );
            }
            if self.ffmpeg_licences.is_empty() {
                out.push("Choose the licence notices of that ffmpeg build.".to_string());
            }
        }
        match self.existing() {
            Existing::Nothing => {}
            Existing::EarlierExport if self.replace_confirmed => {}
            Existing::EarlierExport => out.push(format!(
                "{} already contains an export. Confirm replacing it, or choose another name \
                 or folder.",
                self.destination().unwrap_or_default().display()
            )),
            Existing::Other => out.push(format!(
                "{} already exists and is not a Patchwork export; it will not be replaced. \
                 Choose another name or folder.",
                self.destination().unwrap_or_default().display()
            )),
        }
        if let Err(e) = runtime {
            out.push(e.clone());
        }
        // P6 Stage 2.4/2.6: a distributable build needs every embedded
        // component cleared (the exporter refuses it too).
        if self.purpose == ExportPurpose::Distribution {
            let blockers = licensing_blockers();
            if !blockers.is_empty() {
                out.extend(blockers);
                out.push(
                    "To export for internal development and testing only, choose \
                     Build: Internal test build."
                        .to_string(),
                );
            }
        }
        out
    }

    /// The exporter request for this form.
    pub fn request(&self, runtime: &RuntimeSource) -> Option<ExportRequest> {
        Some(ExportRequest {
            source_project: self.source.clone(),
            destination: self.destination()?,
            app_name: self.app_name.clone(),
            output: self.output,
            runtime: runtime.clone(),
            ffmpeg: match (&self.ffmpeg, self.needs_ffmpeg) {
                (Some(exe), true) => Some(FfmpegSource {
                    executable: exe.clone(),
                    licence_files: self.ffmpeg_licences.clone(),
                }),
                _ => None,
            },
            overwrite: self.existing() == Existing::EarlierExport && self.replace_confirmed,
            purpose: self.purpose,
        })
    }
}

/// Embedded components not cleared for redistribution: no distributable
/// app can be exported while any is listed
/// (`export::runtime::UNRESOLVED_COMPONENTS`).
pub(super) fn licensing_blockers() -> Vec<String> {
    export::runtime::UNRESOLVED_COMPONENTS
        .iter()
        .map(|(what, why)| {
            format!(
                "Licensing: Patchwork embeds {what} in every app, and {why}. Distributable \
                 exports are disabled until this is resolved (licenses/third-party/README.md)."
            )
        })
        .collect()
}

/// The open dialog.
pub(super) struct ExportDialog {
    form: ExportForm,
    runtime: Result<RuntimeSource, String>,
    job: Option<ExportJob>,
    report: Option<ExportReport>,
    start_error: Option<String>,
}

impl ExportDialog {
    pub fn open(source: &Path, graph: &Graph) -> Self {
        let runtime = std::env::current_exe()
            .map_err(|e| format!("Patchwork's own folder cannot be found: {e}"))
            .and_then(|exe| export::runtime::discover(&exe));
        Self {
            form: ExportForm::for_project(source, graph),
            runtime,
            job: None,
            report: None,
            start_error: None,
        }
    }

    pub fn running(&self) -> bool {
        self.job.is_some()
    }

    /// Draw the dialog. Returns `false` when it closed.
    pub fn show(&mut self, ctx: &egui::Context, unsaved: bool) -> bool {
        if let Some(job) = &mut self.job {
            if let Some(report) = job.poll() {
                self.report = Some(report.clone());
                self.job = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        let mut keep_open = true;
        let running = self.running();
        if !running && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            keep_open = false;
        }
        egui::Modal::new(egui::Id::new("export_app_modal")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("Export as App — Windows Portable");
            ui.label(
                egui::RichText::new(format!("Project: {}", self.form.source.display())).weak(),
            );
            ui.add_space(8.0);
            ui.add_enabled_ui(!running, |ui| self.fields(ui));
            ui.add_space(8.0);
            self.notices(ui);
            ui.add_space(8.0);
            let problems = self.form.problems(unsaved, &self.runtime);
            if self.form.purpose == ExportPurpose::InternalTest {
                ui.colored_label(
                    egui::Color32::from_rgb(240, 170, 60),
                    egui::RichText::new(format!("⚠ {}", export::runtime::INTERNAL_TEST_LABEL))
                        .strong(),
                );
                ui.label(
                    "For internal development and testing only. The export contains components \
                     not cleared for redistribution; its folder, its notices and the app's \
                     window are labelled, and it must not be given to anyone outside testing.",
                );
            }
            if !running && self.report.is_none() {
                for problem in &problems {
                    ui.colored_label(egui::Color32::from_rgb(230, 120, 110), problem);
                }
            }
            if let Some(error) = &self.start_error {
                ui.colored_label(egui::Color32::from_rgb(230, 120, 110), error);
            }
            if let Some(job) = &self.job {
                let progress = job.progress();
                let fraction = if progress.total_bytes > 0 {
                    progress.done_bytes as f32 / progress.total_bytes as f32
                } else {
                    0.0
                };
                ui.label(&progress.step);
                ui.add(egui::ProgressBar::new(fraction).show_percentage());
            }
            if let Some(report) = &self.report {
                Self::show_report(ui, report);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if running {
                    if ui.button("Cancel export").clicked() {
                        if let Some(job) = &self.job {
                            job.cancel();
                        }
                    }
                    ui.spinner();
                    return;
                }
                let finished = self.report.as_ref().is_some_and(ExportReport::succeeded);
                if !finished
                    && ui
                        .add_enabled(problems.is_empty(), egui::Button::new("Export"))
                        .clicked()
                {
                    self.start();
                }
                if ui
                    .button(if finished { "Close" } else { "Cancel" })
                    .clicked()
                {
                    keep_open = false;
                }
            });
        });
        keep_open || self.running()
    }

    fn fields(&mut self, ui: &mut egui::Ui) {
        let before = (
            self.form.app_name.clone(),
            self.form.parent.clone(),
            self.form.output,
            self.form.purpose,
        );
        egui::Grid::new("export_app_fields")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Application name");
                ui.add(egui::TextEdit::singleline(&mut self.form.app_name).desired_width(360.0));
                ui.end_row();

                // P6 Stage 2.6: what the build is for.
                ui.label("Build");
                ui.vertical(|ui| {
                    ui.radio_value(
                        &mut self.form.purpose,
                        ExportPurpose::Distribution,
                        "Distributable (every component must be cleared)",
                    );
                    ui.radio_value(
                        &mut self.form.purpose,
                        ExportPurpose::InternalTest,
                        "Internal test build — NOT FOR DISTRIBUTION",
                    );
                });
                ui.end_row();

                ui.label("Location");
                ui.horizontal(|ui| {
                    let shown = self
                        .form
                        .parent
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(not chosen)".to_string());
                    ui.label(shown);
                    if ui.button("Choose…").clicked() {
                        let mut dialog = rfd::FileDialog::new().set_title("Export location");
                        if let Some(start) =
                            self.form.parent.as_deref().or(self.form.source.parent())
                        {
                            dialog = dialog.set_directory(start);
                        }
                        if let Some(dir) = dialog.pick_folder() {
                            self.form.parent = Some(dir);
                        }
                    }
                });
                ui.end_row();

                if let Some(dest) = self.form.destination() {
                    ui.label("Export folder");
                    ui.label(dest.display().to_string());
                    ui.end_row();
                }

                if self.form.outputs.len() > 1 {
                    ui.label("Video Out");
                    let selected = self
                        .form
                        .output
                        .map(|id| format!("Video Out #{id}"))
                        .unwrap_or_else(|| "Choose…".to_string());
                    egui::ComboBox::from_id_salt("export_app_output")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for id in self.form.outputs.clone() {
                                ui.selectable_value(
                                    &mut self.form.output,
                                    Some(id),
                                    format!("Video Out #{id}"),
                                );
                            }
                        });
                    ui.end_row();
                }

                if self.form.needs_ffmpeg {
                    ui.label("ffmpeg.exe");
                    ui.horizontal(|ui| {
                        ui.label(
                            self.form
                                .ffmpeg
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "(not chosen)".to_string()),
                        );
                        if ui.button("Choose…").clicked() {
                            if let Some(file) = rfd::FileDialog::new()
                                .set_title("A redistributable ffmpeg.exe")
                                .add_filter("ffmpeg", &["exe"])
                                .pick_file()
                            {
                                self.form.ffmpeg = Some(file);
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("ffmpeg licences");
                    ui.horizontal(|ui| {
                        ui.label(format!("{} file(s)", self.form.ffmpeg_licences.len()));
                        if ui.button("Choose…").clicked() {
                            if let Some(files) = rfd::FileDialog::new()
                                .set_title("The licence notices of that ffmpeg build")
                                .pick_files()
                            {
                                self.form.ffmpeg_licences = files;
                            }
                        }
                    });
                    ui.end_row();
                }
            });
        if self.form.existing() == Existing::EarlierExport {
            ui.checkbox(
                &mut self.form.replace_confirmed,
                "Replace the existing export in that folder",
            );
        }
        let after = (
            self.form.app_name.clone(),
            self.form.parent.clone(),
            self.form.output,
            self.form.purpose,
        );
        if before != after {
            // A different destination needs its own confirmation, and an
            // earlier result no longer describes the form.
            if before.0 != after.0 || before.1 != after.1 || before.3 != after.3 {
                self.form.replace_confirmed = false;
            }
            self.report = None;
            self.start_error = None;
        }
    }

    fn notices(&self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("What the export contains, and licences")
            .default_open(false)
            .show(ui, |ui| {
                match &self.runtime {
                    Ok(runtime) => {
                        ui.label(format!(
                            "Runtime: {} (no editor, no console window)",
                            runtime.executable.display()
                        ));
                        for dir in &runtime.dependency_dirs {
                            ui.label(format!("Visual C++ runtime from: {}", dir.display()));
                        }
                    }
                    Err(e) => {
                        ui.colored_label(egui::Color32::from_rgb(230, 120, 110), e);
                    }
                }
                ui.label(
                    "• Patchwork is GPL-3.0. The app carries the licence and SOURCE.txt, naming \
                     the exact source commit of its runtime. If you distribute the app, you \
                     must also make that corresponding source available.",
                );
                ui.label(
                    "• DirectML (Microsoft DirectML licence terms) and the Visual C++ runtime \
                     (Visual Studio Distributable Code: requires a licensed Visual Studio) are \
                     included unmodified, with their notices in licenses/.",
                );
                ui.label(
                    "• ffmpeg is included only when the project needs it, from the file you \
                     choose, with the notices you choose.",
                );
                ui.label(
                    "• Never included: trust records, keys, settings or session data. The app \
                     asks for permission the first time it runs on each computer.",
                );
            });
    }

    fn show_report(ui: &mut egui::Ui, report: &ExportReport) {
        if let Some(dest) = report.destination.as_ref().filter(|_| report.succeeded()) {
            if report.internal_test_build {
                ui.colored_label(
                    egui::Color32::from_rgb(240, 170, 60),
                    egui::RichText::new(format!(
                        "{}: do not distribute this app.",
                        export::runtime::INTERNAL_TEST_LABEL
                    ))
                    .strong(),
                );
            }
            ui.colored_label(
                egui::Color32::from_rgb(120, 200, 130),
                format!("Exported to {}", dest.display()),
            );
            ui.label(format!(
                "Run {} in that folder. It opens straight into the Player.",
                report
                    .files
                    .iter()
                    .find(|f| f.ends_with(".exe") && !f.contains('/') && *f != "ffmpeg.exe")
                    .cloned()
                    .unwrap_or_default()
            ));
            if ui.button("Show in Explorer").clicked() {
                let _ = open::that(dest);
            }
        } else if report.cancelled {
            ui.label("The export was cancelled. Nothing was written.");
        } else {
            ui.colored_label(
                egui::Color32::from_rgb(230, 120, 110),
                "Nothing was exported:",
            );
            for error in &report.errors {
                ui.colored_label(egui::Color32::from_rgb(230, 120, 110), format!("• {error}"));
            }
        }
        for warning in &report.warnings {
            ui.colored_label(
                egui::Color32::from_rgb(230, 190, 90),
                format!("⚠ {warning}"),
            );
        }
    }

    fn start(&mut self) {
        self.report = None;
        self.start_error = None;
        let Ok(runtime) = &self.runtime else {
            return;
        };
        let Some(request) = self.form.request(runtime) else {
            return;
        };
        match ExportJob::start(request, ExportEnvironment::this_computer()) {
            Ok(job) => self.job = Some(job),
            Err(e) => self.start_error = Some(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{DynNode, NodeType};
    use crate::library::package::tests::TempDir;

    fn video_out() -> NodeType {
        let node = crate::node_trait::NODE_REGISTRY
            .lock()
            .unwrap()
            .create(
                "video_out",
                &serde_json::json!({ "sink": "Window", "destination": "", "fullscreen": false }),
            )
            .unwrap();
        NodeType::Dynamic {
            inner: DynNode { node },
        }
    }

    fn runtime_ok() -> Result<RuntimeSource, String> {
        Ok(RuntimeSource {
            executable: PathBuf::from("C:\\pw\\patchwork-player.exe"),
            dependency_dirs: Vec::new(),
        })
    }

    /// The form's problems apart from licensing (tested separately).
    fn blocking(form: &ExportForm) -> Vec<String> {
        let licensing = licensing_blockers();
        form.problems(false, &runtime_ok())
            .into_iter()
            .filter(|p| !licensing.contains(p) && !p.contains("Build: Internal test build"))
            .collect()
    }

    fn setup(tag: &str, outputs: usize) -> (TempDir, Graph, PathBuf) {
        let tmp = TempDir::new(tag);
        let project = tmp.0.join("Light Show");
        std::fs::create_dir_all(&project).unwrap();
        let mut graph = Graph::new();
        for i in 0..outputs {
            graph.add_node(video_out(), [i as f32 * 100.0, 0.0]);
        }
        (tmp, graph, project)
    }

    #[test]
    fn defaults_come_from_the_project() {
        let (tmp, graph, project) = setup("dlg_defaults", 1);
        let mut form = ExportForm::for_project(&project, &graph);
        assert_eq!(form.app_name, "Light Show");
        assert_eq!(form.outputs.len(), 1);
        assert_eq!(form.output, form.outputs.first().copied());
        assert!(!form.needs_ffmpeg);
        // The location is the user's choice.
        assert_eq!(form.parent, None);
        assert_eq!(form.destination(), None);
        assert!(
            form.problems(false, &runtime_ok())
                .iter()
                .any(|p| p.contains("Choose where"))
        );
        // Never the project folder itself (or inside it).
        form.parent = Some(tmp.0.clone());
        assert!(
            form.problems(false, &runtime_ok())
                .iter()
                .any(|p| p.contains("cannot be the project folder"))
        );
        form.parent = Some(project.clone());
        assert!(
            form.problems(false, &runtime_ok())
                .iter()
                .any(|p| p.contains("cannot be the project folder"))
        );
        let exports = tmp.0.join("exports");
        std::fs::create_dir_all(&exports).unwrap();
        form.parent = Some(exports.clone());
        assert_eq!(form.destination(), Some(exports.join("Light Show")));
        assert!(blocking(&form).is_empty());
        let request = form.request(&runtime_ok().unwrap()).unwrap();
        assert_eq!(request.destination, exports.join("Light Show"));
        assert!(!request.overwrite);
        assert!(request.ffmpeg.is_none());
    }

    #[test]
    fn validation_reports_every_problem() {
        let (_tmp, graph, project) = setup("dlg_validate", 0);
        let mut form = ExportForm::for_project(&project, &graph);
        form.app_name = "bad:name".to_string();
        form.parent = None;
        let problems = form.problems(true, &Err("no runtime".to_string()));
        let has = |s: &str| problems.iter().any(|p| p.contains(s));
        assert!(has("unsaved changes"), "{problems:?}");
        assert!(has("not allowed in a file name"), "{problems:?}");
        assert!(has("Choose where"), "{problems:?}");
        assert!(has("no Video Out"), "{problems:?}");
        assert!(has("no runtime"), "{problems:?}");
        assert!(form.request(&runtime_ok().unwrap()).is_none());
        form.app_name = "CON".to_string();
        assert!(
            form.problems(false, &runtime_ok())
                .iter()
                .any(|p| p.contains("reserved"))
        );
        form.app_name = String::new();
        assert!(
            form.problems(false, &runtime_ok())
                .iter()
                .any(|p| p.contains("empty"))
        );
    }

    #[test]
    fn several_outputs_need_a_choice() {
        let (tmp, graph, project) = setup("dlg_outputs", 2);
        let mut form = ExportForm::for_project(&project, &graph);
        form.parent = Some(tmp.0.clone());
        form.app_name = "Show App".to_string();
        assert_eq!(form.outputs.len(), 2);
        assert_eq!(form.output, None);
        assert!(
            form.problems(false, &runtime_ok())
                .iter()
                .any(|p| p.contains("Choose the one"))
        );
        form.output = Some(form.outputs[1]);
        assert!(blocking(&form).is_empty());
        assert_eq!(
            form.request(&runtime_ok().unwrap()).unwrap().output,
            Some(form.outputs[1])
        );
        form.output = Some(9999);
        assert!(
            form.problems(false, &runtime_ok())
                .iter()
                .any(|p| p.contains("#9999"))
        );
    }

    #[test]
    fn existing_destination_needs_confirmation() {
        let (tmp, graph, project) = setup("dlg_existing", 1);
        let mut form = ExportForm::for_project(&project, &graph);
        form.parent = Some(tmp.0.clone());
        let dest = tmp.0.join("Light Show Export");
        form.app_name = "Light Show Export".to_string();
        std::fs::create_dir_all(&dest).unwrap();
        // Not an export: never replaced, confirmation or not.
        assert_eq!(form.existing(), Existing::Other);
        form.replace_confirmed = true;
        assert!(
            form.problems(false, &runtime_ok())
                .iter()
                .any(|p| p.contains("not a Patchwork export"))
        );
        assert!(!form.request(&runtime_ok().unwrap()).unwrap().overwrite);
        // An earlier export: only with confirmation.
        std::fs::write(dest.join("player.json"), "{}").unwrap();
        form.replace_confirmed = false;
        assert_eq!(form.existing(), Existing::EarlierExport);
        assert!(
            form.problems(false, &runtime_ok())
                .iter()
                .any(|p| p.contains("Confirm replacing"))
        );
        assert!(!form.request(&runtime_ok().unwrap()).unwrap().overwrite);
        form.replace_confirmed = true;
        assert!(blocking(&form).is_empty());
        assert!(form.request(&runtime_ok().unwrap()).unwrap().overwrite);
    }

    #[test]
    fn licensing_blockers_are_shown_before_exporting() {
        let blockers = licensing_blockers();
        assert_eq!(blockers.len(), export::runtime::UNRESOLVED_COMPONENTS.len());
        assert!(
            blockers
                .iter()
                .all(|b| b.contains("Distributable exports are disabled"))
        );
    }

    #[test]
    fn a_distributable_build_is_blocked_and_an_internal_test_build_is_labelled() {
        let (tmp, graph, project) = setup("dlg_purpose", 1);
        let mut form = ExportForm::for_project(&project, &graph);
        form.parent = Some(tmp.0.clone());
        // The default is a distributable build, never silently internal.
        assert_eq!(form.purpose, ExportPurpose::Distribution);
        assert_eq!(form.destination(), Some(tmp.0.join("Light Show")));
        let problems = form.problems(false, &runtime_ok());
        if export::runtime::UNRESOLVED_COMPONENTS.is_empty() {
            assert!(problems.is_empty(), "{problems:?}");
        } else {
            for blocker in licensing_blockers() {
                assert!(problems.contains(&blocker), "{problems:?}");
            }
            assert!(problems.iter().any(|p| p.contains("Internal test build")));
        }
        // Internal test build: allowed, in its own labelled folder (it never
        // replaces a distributable export of the same name).
        form.purpose = ExportPurpose::InternalTest;
        assert!(form.problems(false, &runtime_ok()).is_empty());
        let dest = tmp
            .0
            .join("Light Show (INTERNAL TEST - NOT FOR DISTRIBUTION)");
        assert_eq!(form.destination(), Some(dest.clone()));
        let request = form.request(&runtime_ok().unwrap()).unwrap();
        assert_eq!(request.purpose, ExportPurpose::InternalTest);
        assert_eq!(request.destination, dest);
        assert_eq!(request.app_name, "Light Show");
    }

    #[test]
    fn ffmpeg_is_asked_for_only_when_needed() {
        let (tmp, mut graph, project) = setup("dlg_ffmpeg", 1);
        let form = ExportForm::for_project(&project, &graph);
        assert!(!form.needs_ffmpeg);
        let player: NodeType =
            serde_json::from_value(serde_json::json!({ "VideoPlayer": { "path": "clip.mp4" } }))
                .unwrap();
        graph.add_node(player, [0.0, 200.0]);
        let mut form = ExportForm::for_project(&project, &graph);
        form.parent = Some(tmp.0.clone());
        form.app_name = "Video App".to_string();
        assert!(form.needs_ffmpeg);
        let problems = form.problems(false, &runtime_ok());
        assert!(
            problems.iter().any(|p| p.contains("needs ffmpeg")),
            "{problems:?}"
        );
        assert!(problems.iter().any(|p| p.contains("licence notices")));
        form.ffmpeg = Some(PathBuf::from("C:\\tools\\ffmpeg.exe"));
        form.ffmpeg_licences = vec![PathBuf::from("C:\\tools\\LICENSE.txt")];
        assert!(blocking(&form).is_empty());
        let request = form.request(&runtime_ok().unwrap()).unwrap();
        assert_eq!(request.ffmpeg.unwrap().licence_files.len(), 1);
    }
}
