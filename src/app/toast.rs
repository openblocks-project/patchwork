//! Top-center toast notifications.
//!
//! Lightweight, non-blocking feedback for events the user can't see (a
//! silently-rejected connection, a save failure, etc). Toasts auto-dismiss
//! after `DEFAULT_DURATION` seconds; sticky toasts (`duration: None`) stay
//! until the user closes them. Every toast has a manual close (✕) button so
//! the auto-dismiss never traps the user.

use eframe::egui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastSeverity {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub severity: ToastSeverity,
    /// Wall-clock seconds since `app_start_instant`.
    pub spawned_at: f64,
    /// `None` = sticky (no auto-dismiss).
    pub duration: Option<f64>,
}

const DEFAULT_DURATION: f64 = 2.0;
const FADE_OUT_SECS: f64 = 0.3;

impl super::PatchworkApp {
    /// 2-second info toast at top-center.
    #[allow(dead_code)]
    pub fn toast_info(&mut self, text: impl Into<String>) {
        self.push_toast(text.into(), ToastSeverity::Info, Some(DEFAULT_DURATION));
    }

    /// 2-second warning toast — used for recoverable issues like a
    /// type-mismatched wire drop.
    pub fn toast_warn(&mut self, text: impl Into<String>) {
        self.push_toast(text.into(), ToastSeverity::Warn, Some(DEFAULT_DURATION));
    }

    /// 2-second error toast.
    #[allow(dead_code)]
    pub fn toast_error(&mut self, text: impl Into<String>) {
        self.push_toast(text.into(), ToastSeverity::Error, Some(DEFAULT_DURATION));
    }

    /// Toast that stays until the user closes it. Useful for things that
    /// need a persistent indicator (an unrecoverable error, a long-running
    /// operation status). The user dismisses via the ✕ button.
    #[allow(dead_code)]
    pub fn toast_sticky(&mut self, text: impl Into<String>, severity: ToastSeverity) {
        self.push_toast(text.into(), severity, None);
    }

    fn push_toast(&mut self, text: String, severity: ToastSeverity, duration: Option<f64>) {
        let now = self.app_start_instant.elapsed().as_secs_f64();
        // Coalesce: if the most recent toast carries the same text+severity,
        // refresh it instead of stacking duplicates. Stops a held-down failing
        // action from filling the column with the same message.
        if let Some(last) = self.toasts.last_mut() {
            if last.text == text && last.severity == severity {
                last.spawned_at = now;
                last.duration = duration;
                return;
            }
        }
        self.toasts.push(Toast { text, severity, spawned_at: now, duration });
    }

    /// Drop expired toasts and render the rest at top-center, stacked.
    /// Called once per frame from `update()`.
    pub(super) fn render_toasts(&mut self, ctx: &egui::Context) {
        let now = self.app_start_instant.elapsed().as_secs_f64();
        // Drop expired (auto-dismiss) toasts before render — keeps indices
        // stable for the close-by-button pass below.
        self.toasts.retain(|t| match t.duration {
            Some(d) => now < t.spawned_at + d,
            None => true,
        });
        if self.toasts.is_empty() { return; }
        // Repaint while toasts are live so the fade-out animates and
        // auto-dismiss fires even on an idle canvas.
        ctx.request_repaint();

        let mut close_index: Option<usize> = None;
        for (i, toast) in self.toasts.iter().enumerate() {
            let alpha = match toast.duration {
                Some(d) => {
                    let remaining = toast.spawned_at + d - now;
                    ((remaining / FADE_OUT_SECS).clamp(0.0, 1.0)) as f32
                }
                None => 1.0,
            };
            // Info toasts borrow the theme accent so they feel branded.
            // Warn/Error stay semantic (yellow/red) — meaning is part of
            // their value.
            let accent = crate::nodes::theme::current_accent_arr(ctx);
            let (bg, border) = match toast.severity {
                ToastSeverity::Info => (
                    egui::Color32::from_rgba_unmultiplied(
                        accent[0] / 4 + 20,
                        accent[1] / 4 + 20,
                        accent[2] / 4 + 20,
                        (220.0 * alpha) as u8,
                    ),
                    egui::Color32::from_rgba_unmultiplied(
                        accent[0], accent[1], accent[2], (160.0 * alpha) as u8,
                    ),
                ),
                ToastSeverity::Warn => (
                    egui::Color32::from_rgba_unmultiplied(120, 90, 30, (220.0 * alpha) as u8),
                    egui::Color32::from_rgba_unmultiplied(240, 190, 80, (160.0 * alpha) as u8),
                ),
                ToastSeverity::Error => (
                    egui::Color32::from_rgba_unmultiplied(140, 40, 40, (230.0 * alpha) as u8),
                    egui::Color32::from_rgba_unmultiplied(240, 100, 100, (160.0 * alpha) as u8),
                ),
            };
            let fg = egui::Color32::from_rgba_unmultiplied(255, 255, 255, (255.0 * alpha) as u8);

            let stack_offset = 12.0 + (i as f32) * 44.0;
            let area_id = egui::Id::new(("patchwork_toast", i));
            egui::Area::new(area_id)
                .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, stack_offset))
                .order(egui::Order::Foreground)
                .interactable(true)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(bg)
                        .stroke(egui::Stroke::new(1.0, border))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.colored_label(fg, &toast.text);
                                ui.add_space(8.0);
                                if ui.small_button(egui::RichText::new("✕").color(fg)).clicked() {
                                    close_index = Some(i);
                                }
                            });
                        });
                });
        }

        if let Some(i) = close_index {
            if i < self.toasts.len() {
                self.toasts.remove(i);
            }
        }
    }
}
