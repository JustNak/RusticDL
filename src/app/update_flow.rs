//! Staged self-update flow extracted from `DownloadApp`.
//!
//! Toast stages (interactive + silent when an update exists):
//! 1. **Checking for update…**
//! 2a. **You're up to date** — or —
//! 2b. **Update available vX.Y.Z** `[Update]` → **Restart to update** `[Restart]`
//! 3. On Restart: flush state, snapshot What’s new, spawn **RusticDL Updater**, quit.
//! 4. Updater downloads, runs NSIS `/S`, relaunches the main app.
//! 5. **What’s new** — post-relaunch dialog with the release changelog.

use std::time::Duration;

use gpui::{
    div, px, Context, InteractiveElement, ParentElement, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    dialog::DialogButtonProps,
    h_flex, v_flex, ActiveTheme, Sizable, WindowExt,
};

use super::toast::{ToastActionKind, ToastKind};
use super::DownloadApp;
use crate::branding::{APP_VERSION, UPDATER_NAME};
use crate::persistence::{clear_pending_whats_new, save_pending_whats_new, PendingWhatsNew};
use crate::updater::{
    check_for_update, launch_updater, open_url, LaunchUpdaterOpts, UpdateCheck, UpdateInfo,
};

/// Scroll height for the post-update changelog body.
const WHATS_NEW_NOTES_MAX_H: f32 = 200.0;

impl DownloadApp {
    /// Label for the single update action (check or advance cached release).
    pub(crate) fn update_action_label(&self) -> String {
        if let Some(info) = &self.available_update {
            format!("Update available v{}", info.latest_version)
        } else {
            "Check for updates".into()
        }
    }

    /// Brand menu / About: check when unknown, else show restart confirmation toast.
    pub(crate) fn begin_update_action(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.update_busy {
            self.show_toast("An update is already in progress…", cx);
            return;
        }
        if self.available_update.is_some() {
            self.show_restart_to_update_toast(cx);
            return;
        }
        self.begin_update_check(true, cx);
    }

    /// Manual or silent GitHub Releases update check (never installs).
    pub(crate) fn begin_update_check(&mut self, interactive: bool, cx: &mut Context<Self>) {
        if self.update_busy {
            if interactive {
                self.show_toast("An update check is already running…", cx);
            }
            return;
        }
        self.update_busy = true;
        if interactive {
            self.replace_update_toast("Checking for update…", ToastKind::Info, None, cx);
        }
        cx.notify();
        spawn_update_check(interactive, cx);
    }

    pub(crate) fn on_update_check_finished(
        &mut self,
        interactive: bool,
        result: Result<UpdateCheck, String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(UpdateCheck::UpToDate { .. }) => {
                self.available_update = None;
                self.update_busy = false;
                if interactive {
                    self.replace_update_toast("You're up to date", ToastKind::Info, None, cx);
                } else {
                    // Drop the checking toast if a silent check somehow set one.
                    self.clear_update_toast(cx);
                }
            }
            Ok(UpdateCheck::Available(info)) => {
                self.available_update = Some(info.clone());
                self.update_busy = false;
                // Interactive and silent: toast with [Update] so the user can continue
                // without hunting the brand menu.
                self.show_update_available_toast(&info, cx);
            }
            Err(message) => {
                self.update_busy = false;
                if interactive {
                    self.replace_update_toast(message, ToastKind::Error, None, cx);
                } else {
                    self.clear_update_toast(cx);
                }
            }
        }
        cx.notify();
    }

    /// “Update available vX.Y.Z” with an Update action button.
    pub(crate) fn show_update_available_toast(
        &mut self,
        info: &UpdateInfo,
        cx: &mut Context<Self>,
    ) {
        self.replace_update_toast(
            format!("Update available v{}", info.latest_version),
            ToastKind::Info,
            Some(("Update", ToastActionKind::ConfirmUpdate)),
            cx,
        );
    }

    /// “Restart to update” with a Restart action button.
    pub(crate) fn show_restart_to_update_toast(&mut self, cx: &mut Context<Self>) {
        if self.available_update.is_none() {
            return;
        }
        self.replace_update_toast(
            "Restart to update",
            ToastKind::Info,
            Some(("Restart", ToastActionKind::RestartToUpdate)),
            cx,
        );
    }

    /// Handle primary actions from update toasts.
    pub(crate) fn on_update_toast_action(&mut self, kind: ToastActionKind, cx: &mut Context<Self>) {
        match kind {
            ToastActionKind::ConfirmUpdate => {
                if self.available_update.is_some() {
                    self.show_restart_to_update_toast(cx);
                }
            }
            ToastActionKind::RestartToUpdate => {
                let Some(info) = self.available_update.clone() else {
                    self.show_toast("No update is ready to install.", cx);
                    return;
                };
                self.begin_apply_update(info, cx);
            }
        }
    }

    /// Open the post-update changelog once a `Window` is free.
    pub(crate) fn apply_pending_whats_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_show_whats_new {
            return;
        }
        if window.has_active_dialog(cx) {
            return;
        }
        let Some(pending) = self.pending_whats_new.clone() else {
            self.pending_show_whats_new = false;
            return;
        };
        self.pending_show_whats_new = false;
        self.open_whats_new_dialog(pending, window, cx);
    }

    /// Tasteful post-update changelog (Esc / mouse-back / outside / Close).
    pub(crate) fn open_whats_new_dialog(
        &mut self,
        pending: PendingWhatsNew,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Ack on open so Esc / mouse-back via `close_dialog` (no on_close) cannot re-show.
        self.ack_whats_new(cx);

        let to = pending.to_version.clone();
        let from = pending.from_version.clone();
        let release_name = pending.release_name.clone();
        let html_url = pending.html_url.clone();
        let notes_display = pending
            .notes
            .as_ref()
            .map(|n| format_changelog_notes(n))
            .filter(|n| !n.is_empty());
        let title = format!("Updated to v{to}");
        let has_url = !html_url.trim().is_empty();

        window.open_dialog(cx, move |dialog, window, cx| {
            let theme = cx.theme().clone();
            let muted = theme.muted_foreground;
            let title = title.clone();
            let from = from.clone();
            let release_name = release_name.clone();
            let html_url = html_url.clone();
            let notes_display = notes_display.clone();

            let est_h = if notes_display.is_some() {
                380.0
            } else {
                240.0
            };
            let view_h = window.viewport_size().height.to_f64() as f32;
            let max_top = (view_h - est_h - 20.0).max(24.0);
            let margin_top = ((view_h - est_h) * 0.5).clamp(24.0, max_top);

            let mut body = v_flex().gap_3().child(
                div()
                    .text_sm()
                    .child(format!("You were on v{from}. Here’s what changed.")),
            );

            if !release_name.trim().is_empty() {
                body = body.child(div().text_xs().text_color(muted).child(release_name));
            }

            if let Some(notes) = notes_display {
                body = body.child(
                    div()
                        .id("whats-new-notes")
                        .w_full()
                        .max_h(px(WHATS_NEW_NOTES_MAX_H))
                        .overflow_y_scroll()
                        .p_2()
                        .rounded(theme.radius_lg)
                        .border_1()
                        .border_color(theme.border.opacity(0.4))
                        .bg(theme.popover.opacity(0.55))
                        .text_xs()
                        .text_color(muted)
                        .child(notes),
                );
            } else {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("No release notes were included for this version."),
                );
            }

            if has_url {
                let url = html_url.clone();
                body = body.child(
                    h_flex().child(
                        Button::new("whats-new-open-release")
                            .ghost()
                            .small()
                            .label("Open full notes")
                            .on_click(move |_, _, _| {
                                let _ = open_url(&url);
                            }),
                    ),
                );
            }

            dialog
                .title(title)
                .alert()
                // alert() disables outside-click; re-enable for light dismiss UX.
                .overlay_closable(true)
                .keyboard(true)
                .w(px(460.))
                .margin_top(px(margin_top))
                .border_color(theme.border.opacity(0.32))
                .button_props(DialogButtonProps::default().ok_text("Close"))
                .child(body)
        });
    }

    /// Drop the on-disk snapshot so the dialog does not reappear next launch.
    pub(crate) fn ack_whats_new(&mut self, _cx: &mut Context<Self>) {
        self.pending_whats_new = None;
        self.pending_show_whats_new = false;
        let _ = clear_pending_whats_new(&self.paths);
    }

    pub(crate) fn begin_apply_update(&mut self, info: UpdateInfo, cx: &mut Context<Self>) {
        if self.update_busy {
            self.show_toast("An update is already in progress…", cx);
            return;
        }
        self.update_busy = true;
        self.begin_apply_update_inner(info, cx);
    }

    /// Persist state, snapshot What’s new, spawn RusticDL Updater, then quit.
    pub(crate) fn begin_apply_update_inner(&mut self, info: UpdateInfo, cx: &mut Context<Self>) {
        self.replace_update_toast(
            format!("Handing off to {UPDATER_NAME}…"),
            ToastKind::Info,
            None,
            cx,
        );
        cx.notify();

        // Persist before spawn/quit so a kill during install cannot race a dirty save.
        self.flush_jobs_save_now();
        self.flush_window_layout_now();

        let from_version = if info.current_version.trim().is_empty() {
            APP_VERSION.to_string()
        } else {
            info.current_version.clone()
        };

        // Snapshot notes now so the relaunched binary can show them without GitHub.
        let pending = PendingWhatsNew {
            from_version: from_version.clone(),
            to_version: info.latest_version.clone(),
            release_name: info.release_name.clone(),
            html_url: info.html_url.clone(),
            notes: info.notes.clone(),
        };
        let _ = save_pending_whats_new(&self.paths, &pending);

        let opts = LaunchUpdaterOpts {
            download_url: info.setup_download_url.clone(),
            from_version,
            to_version: info.latest_version.clone(),
            release_page: info.html_url.clone(),
            setup_size: info.setup_size,
        };

        if let Err(message) = launch_updater(&opts) {
            // Handoff failed — discard the snapshot so a normal start does not
            // claim an update that never applied.
            let _ = clear_pending_whats_new(&self.paths);
            self.update_busy = false;
            self.replace_update_toast(message, ToastKind::Error, None, cx);
            cx.notify();
            return;
        }

        // Bypass close-to-tray / hidden-window paint so quit actually tears down.
        self.force_quit_app(cx);
    }
}

/// Run a GitHub Releases update check on a background thread and deliver the result to the UI.
pub(crate) fn spawn_update_check(interactive: bool, cx: &mut Context<DownloadApp>) {
    let delay = if interactive {
        Duration::from_millis(0)
    } else {
        Duration::from_secs(4)
    };

    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("Could not start update runtime: {e}"))
            .and_then(|rt| rt.block_on(check_for_update()));
        let _ = tx.send_blocking(result);
    });

    cx.spawn(async move |this, cx| {
        let result = rx
            .recv()
            .await
            .unwrap_or_else(|_| Err("Update check was cancelled unexpectedly.".into()));
        let _ = this.update(cx, |app, cx| {
            app.on_update_check_finished(interactive, result, cx);
        });
    })
    .detach();
}

/// Format GitHub release Markdown lightly for a readable plain-text changelog.
fn format_changelog_notes(notes: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for raw in notes.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            // Collapse runs of blank lines to a single spacer at most.
            if lines.last().is_some_and(|l| !l.is_empty()) {
                lines.push(String::new());
            }
            continue;
        }
        // Horizontal rules / HTML comments — skip.
        if trimmed
            .chars()
            .all(|c| c == '-' || c == '*' || c == '_' || c.is_whitespace())
            && trimmed.len() >= 3
        {
            continue;
        }
        if trimmed.starts_with("<!--") {
            continue;
        }

        let mut line = trimmed.to_string();
        // ATX headers → plain title text.
        if line.starts_with('#') {
            line = line.trim_start_matches('#').trim().to_string();
            if line.is_empty() {
                continue;
            }
        }
        // Unescape common emphasis markers for quieter display.
        line = line.replace("**", "").replace("__", "");
        // Normalize list markers to a bullet.
        if let Some(rest) = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| line.strip_prefix("+ "))
        {
            line = format!("• {rest}");
        } else if let Some(rest) = line.strip_prefix("-").or_else(|| line.strip_prefix("*")) {
            let rest = rest.trim_start();
            if !rest.is_empty() {
                line = format!("• {rest}");
            }
        }

        lines.push(line);
    }
    // Trim trailing blank lines.
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_changelog_preserves_structure() {
        let raw = r#"## What's new

- Fix tray exit
- Add What's new dialog

### Notes
**Important** change
"#;
        let out = format_changelog_notes(raw);
        assert!(out.contains("What's new"));
        assert!(out.contains("• Fix tray exit"));
        assert!(out.contains("• Add What's new dialog"));
        assert!(out.contains("Important change"));
        assert!(!out.contains("##"));
        assert!(!out.contains("**"));
    }

    #[test]
    fn format_changelog_skips_rules() {
        let out = format_changelog_notes("---\n- item\n***");
        assert_eq!(out, "• item");
    }
}
