//! Self-update check / apply flow extracted from `DownloadApp`.
//!
//! Check stays in-process. Apply hands off to **RusticDL Updater**, which shows
//! progress, runs the NSIS setup, and relaunches the main app.

use std::time::Duration;

use gpui::Context;

use super::DownloadApp;
use crate::branding::{APP_NAME, APP_VERSION, UPDATER_NAME};
use crate::format::format_bytes;
use crate::updater::{
    check_for_update, launch_updater, LaunchUpdaterOpts, UpdateCheck, UpdateInfo,
};

impl DownloadApp {
    /// Label for the single update action (check or install cached release).
    pub(crate) fn update_action_label(&self) -> String {
        if let Some(info) = &self.available_update {
            format!("Install update v{}", info.latest_version)
        } else {
            "Check for updates".into()
        }
    }

    /// One click: install a known update, or check GitHub and install if newer.
    pub(crate) fn begin_one_click_update(&mut self, cx: &mut Context<Self>) {
        if self.update_busy {
            self.show_toast("An update is already in progress…", cx);
            return;
        }
        if let Some(info) = self.available_update.clone() {
            self.begin_apply_update(info, cx);
            return;
        }
        self.begin_update_check(true, cx);
    }

    /// Manual or silent GitHub Releases update check.
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
                if interactive {
                    // One click: hand off to the updater (no second prompt).
                    let size_hint = info
                        .setup_size
                        .map(|n| format!(" ({})", format_bytes(n)))
                        .unwrap_or_default();
                    self.show_toast(
                        format!(
                            "{} — starting {UPDATER_NAME}{size_hint}…",
                            info.release_name
                        ),
                        cx,
                    );
                    // Keep `update_busy` true across check → handoff.
                    self.begin_apply_update_inner(info, cx);
                } else {
                    self.update_busy = false;
                    let size_hint = info
                        .setup_size
                        .map(|n| format!(" · {}", format_bytes(n)))
                        .unwrap_or_default();
                    self.show_toast(
                        format!(
                            "Update available: v{} (you have v{}){size_hint}. Click “Install update” in the {} menu once.",
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
