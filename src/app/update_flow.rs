//! Staged self-update flow extracted from `DownloadApp`.
//!
//! 1. **Check** — query GitHub Releases in-process.
//! 2. **Available** — show version / notes dialog (no auto-install).
//! 3. **Install and restart** — user confirms handoff.
//! 4. Flush state, snapshot What’s new, spawn **RusticDL Updater**, quit.
//! 5. Updater downloads, runs NSIS `/S`, relaunches the main app.
//! 6. **What’s new** — post-relaunch dialog with the release changelog.
//!
//! Silent startup checks only toast + cache; they never open a dialog.

use std::time::Duration;

use gpui::{
    div, px, Context, InteractiveElement, ParentElement, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{
    button::{Button, ButtonVariant, ButtonVariants},
    dialog::DialogButtonProps,
    h_flex, v_flex, ActiveTheme, Sizable, WindowExt,
};

use super::DownloadApp;
use crate::branding::{APP_NAME, APP_VERSION, UPDATER_NAME};
use crate::download::JobState;
use crate::format::format_bytes;
use crate::persistence::{clear_pending_whats_new, save_pending_whats_new, PendingWhatsNew};
use crate::updater::{
    check_for_update, launch_updater, open_url, LaunchUpdaterOpts, UpdateCheck, UpdateInfo,
};

/// Cap release notes in the consent dialog so multi-line Markdown bodies stay readable.
const DIALOG_NOTES_MAX_CHARS: usize = 280;
/// Scroll height for the post-update changelog body.
const WHATS_NEW_NOTES_MAX_H: f32 = 200.0;

impl DownloadApp {
    /// Label for the single update action (check or open cached release dialog).
    pub(crate) fn update_action_label(&self) -> String {
        if let Some(info) = &self.available_update {
            format!("Install update v{}", info.latest_version)
        } else {
            "Check for updates".into()
        }
    }

    /// Brand menu / About: check when unknown, else reopen the available-update dialog.
    pub(crate) fn begin_update_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update_busy {
            self.show_toast("An update is already in progress…", cx);
            return;
        }
        if let Some(info) = self.available_update.clone() {
            self.open_update_available_dialog(info, window, cx);
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
            self.show_toast("Checking GitHub for updates…", cx);
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
            Ok(UpdateCheck::UpToDate { current, latest }) => {
                self.available_update = None;
                self.pending_show_update_dialog = false;
                self.update_busy = false;
                if interactive {
                    self.show_toast(
                        format!("You're up to date (v{current}; latest is v{latest})."),
                        cx,
                    );
                }
            }
            Ok(UpdateCheck::Available(info)) => {
                self.available_update = Some(info.clone());
                self.update_busy = false;
                if interactive {
                    // Open the version dialog on the next frame that has a Window.
                    // No toast here: the dialog is the signal (menu label covers deferral).
                    self.pending_show_update_dialog = true;
                } else {
                    // Never clear a pending interactive dialog here: a late silent
                    // completion must not suppress consent after the user checked.
                    let size_hint = info
                        .setup_size
                        .map(|n| format!(" · {}", format_bytes(n)))
                        .unwrap_or_default();
                    self.show_toast(
                        format!(
                            "Update available: v{} (you have v{}){size_hint}. Click “Install update” in the {} menu.",
                            info.latest_version, info.current_version, APP_NAME
                        ),
                        cx,
                    );
                }
            }
            Err(message) => {
                self.update_busy = false;
                if interactive {
                    self.show_error_toast(message, cx);
                }
            }
        }
        cx.notify();
    }

    /// Apply any deferred “update available” dialog once a `Window` is available.
    pub(crate) fn apply_pending_update_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.pending_show_update_dialog {
            return;
        }
        // Don't stack over an already-open dialog (e.g. About). Menu label still
        // switches to “Install update v…”, and this dialog opens when About closes.
        if window.has_active_dialog(cx) {
            return;
        }
        // Prefer post-update What’s new when both are somehow pending.
        if self.pending_show_whats_new {
            return;
        }
        let Some(info) = self.available_update.clone() else {
            self.pending_show_update_dialog = false;
            return;
        };
        self.pending_show_update_dialog = false;
        self.open_update_available_dialog(info, window, cx);
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

    /// Single dialog: version details + consent to close, install, and reopen.
    pub(crate) fn open_update_available_dialog(
        &mut self,
        info: UpdateInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // User is looking at the dialog; cancel any deferred open.
        self.pending_show_update_dialog = false;

        // Only warn for transfers that will actually stop mid-flight.
        let transferring_count = self
            .jobs
            .iter()
            .filter(|j| matches!(j.state, JobState::Starting | JobState::Downloading))
            .count();
        let size_line = info
            .setup_size
            .map(|n| format!("Installer size: {}.", format_bytes(n)))
            .unwrap_or_else(|| "Installer size: unknown.".into());
        let notes = info
            .notes
            .as_ref()
            .map(|n| n.trim())
            .filter(|n| !n.is_empty())
            .map(|n| truncate_dialog_notes(n, DIALOG_NOTES_MAX_CHARS));
        let title = format!("Update to v{}?", info.latest_version);
        let release_name = info.release_name.clone();
        let current = if info.current_version.trim().is_empty() {
            APP_VERSION.to_string()
        } else {
            info.current_version.clone()
        };
        let latest = info.latest_version.clone();
        let app_view = cx.entity().clone();
        let info_for_ok = info;

        window.open_dialog(cx, move |dialog, _, cx| {
            // `open_dialog` builder is `Fn` (may rebuild); clone owned strings each time.
            let theme = cx.theme();
            let muted = theme.muted_foreground;
            let warning = theme.warning;
            let app_view = app_view.clone();
            let info_for_ok = info_for_ok.clone();
            let title = title.clone();
            let release_name = release_name.clone();
            let current = current.clone();
            let latest = latest.clone();
            let size_line = size_line.clone();
            let notes = notes.clone();

            let mut body = v_flex()
                .gap_2()
                .child(div().text_sm().child(release_name))
                .child(
                    div().text_sm().child(format!(
                        "You have v{current}. Version v{latest} is available."
                    )),
                )
                .child(div().text_xs().text_color(muted).child(size_line));

            if let Some(notes) = notes {
                body = body.child(
                    div()
                        .id("update-notes")
                        .max_h(px(96.))
                        .overflow_y_scroll()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("Notes: {notes}")),
                );
            }

            body = body.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!(
                        "{APP_NAME} will close, {UPDATER_NAME} will install the update, and the app will reopen."
                    )),
            );

            if transferring_count > 0 {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(warning)
                        .child(format!(
                            "Warning: {transferring_count} download(s) in progress will be interrupted. Resume is supported where possible after restart."
                        )),
                );
            }

            dialog
                .title(title)
                .confirm()
                .overlay_closable(true)
                .keyboard(true)
                .w(px(420.))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Install and restart")
                        .ok_variant(ButtonVariant::Primary),
                )
                .child(body)
                .on_ok(move |_, _window, cx| {
                    app_view.update(cx, |app, cx| {
                        app.begin_apply_update(info_for_ok.clone(), cx);
                    });
                    true
                })
        });
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
        self.show_toast(
            format!("Handing off to {UPDATER_NAME} — RusticDL will restart…"),
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
            self.show_error_toast(message, cx);
            cx.notify();
            return;
        }

        // Bypass close-to-tray so quit actually tears down the process.
        self.force_quit = true;
        self.stop_tray();
        cx.notify();
        cx.quit();
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

fn truncate_dialog_notes(notes: &str, max_chars: usize) -> String {
    // Collapse Markdown-ish whitespace so headers/lists don't inflate the dialog.
    let compact: String = notes
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let mut out: String = compact.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
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

    #[test]
    fn truncate_dialog_notes_collapses_lines() {
        let raw = "Line one\n\nLine two";
        assert_eq!(truncate_dialog_notes(raw, 100), "Line one Line two");
    }
}
