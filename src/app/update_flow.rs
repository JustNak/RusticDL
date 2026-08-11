//! Staged self-update flow extracted from `DownloadApp`.
//!
//! 1. **Check** — query GitHub Releases in-process.
//! 2. **Available** — show version / notes dialog (no auto-install).
//! 3. **Install and restart** — user confirms handoff.
//! 4. Flush state, spawn **RusticDL Updater**, quit.
//! 5. Updater downloads, runs NSIS `/S`, relaunches the main app.
//!
//! Silent startup checks only toast + cache; they never open a dialog.

use std::time::Duration;

use gpui::{div, Context, ParentElement, Styled, Window};
use gpui_component::{
    button::ButtonVariant, dialog::DialogButtonProps, v_flex, ActiveTheme, WindowExt,
};

use super::DownloadApp;
use crate::branding::{APP_NAME, APP_VERSION, UPDATER_NAME};
use crate::format::format_bytes;
use crate::updater::{
    check_for_update, launch_updater, LaunchUpdaterOpts, UpdateCheck, UpdateInfo,
};

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
                    self.pending_show_update_dialog = true;
                    let size_hint = info
                        .setup_size
                        .map(|n| format!(" · {}", format_bytes(n)))
                        .unwrap_or_default();
                    self.show_toast(
                        format!(
                            "Update available: v{} (you have v{}){size_hint}.",
                            info.latest_version, info.current_version
                        ),
                        cx,
                    );
                } else {
                    self.pending_show_update_dialog = false;
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
        // Don't stack over an already-open dialog (e.g. About).
        if window.has_active_dialog(cx) {
            return;
        }
        let Some(info) = self.available_update.clone() else {
            self.pending_show_update_dialog = false;
            return;
        };
        self.pending_show_update_dialog = false;
        self.open_update_available_dialog(info, window, cx);
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

        let active_count = self
            .jobs
            .iter()
            .filter(|j| j.state.is_active())
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
            .map(|n| n.to_string());
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
            let muted = cx.theme().muted_foreground;
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

            if active_count > 0 {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!(
                            "{active_count} active download(s) will be interrupted. Resume is supported where possible after restart."
                        )),
                );
            }

            dialog
                .title(title)
                .confirm()
                .overlay_closable(true)
                .keyboard(true)
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

    pub(crate) fn begin_apply_update(&mut self, info: UpdateInfo, cx: &mut Context<Self>) {
        if self.update_busy {
            self.show_toast("An update is already in progress…", cx);
            return;
        }
        self.update_busy = true;
        self.begin_apply_update_inner(info, cx);
    }

    /// Persist state, spawn RusticDL Updater, then quit so files can be replaced.
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

        let opts = LaunchUpdaterOpts {
            download_url: info.setup_download_url.clone(),
            from_version,
            to_version: info.latest_version.clone(),
            release_page: info.html_url.clone(),
            setup_size: info.setup_size,
        };

        if let Err(message) = launch_updater(&opts) {
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
