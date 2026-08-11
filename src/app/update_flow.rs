//! Self-update check / download flow extracted from `DownloadApp`.

use std::time::Duration;

use gpui::Context;

use super::DownloadApp;
use crate::branding::APP_NAME;
use crate::format::format_bytes;
use crate::updater::{check_for_update, download_installer, launch_installer, UpdateCheck};

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
            self.begin_download_update(info.setup_download_url, cx);
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
                if interactive {
                    self.update_busy = false;
                    self.show_toast(
                        format!("You're up to date (v{current}; latest is v{latest})."),
                        cx,
                    );
                }
            }
            Ok(UpdateCheck::Available(info)) => {
                self.available_update = Some(info.clone());
                if interactive {
                    // One click: go straight to silent download + install (no second prompt).
                    let size_hint = info
                        .setup_size
                        .map(|n| format!(" ({})", format_bytes(n)))
                        .unwrap_or_default();
                    self.show_toast(
                        format!(
                            "{} — downloading and installing{size_hint}…",
                            info.release_name
                        ),
                        cx,
                    );
                    // Keep `update_busy` true across check → download.
                    self.begin_download_update_inner(info.setup_download_url, cx);
                } else {
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
                if interactive {
                    self.update_busy = false;
                    self.show_error_toast(message, cx);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn begin_download_update(&mut self, download_url: String, cx: &mut Context<Self>) {
        if self.update_busy {
            self.show_toast("An update is already in progress…", cx);
            return;
        }
        self.update_busy = true;
        self.begin_download_update_inner(download_url, cx);
    }

    /// Download the setup binary, persist state, launch silently (`/S /R`), then quit.
    ///
    /// Launch happens only after flush so the installer's silent kill cannot race a dirty save.
    pub(crate) fn begin_download_update_inner(
        &mut self,
        download_url: String,
        cx: &mut Context<Self>,
    ) {
        self.show_toast("Downloading update from GitHub…", cx);
        cx.notify();

        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("Could not start download runtime: {e}"))
                .and_then(|rt| rt.block_on(download_installer(&download_url)));
            let _ = tx.send_blocking(result);
        });

        cx.spawn(async move |this, cx| {
            let result = rx
                .recv()
                .await
                .unwrap_or_else(|_| Err("Update download was cancelled unexpectedly.".into()));
            let _ = this.update(cx, |app, cx| {
                match result {
                    Ok(installer_path) => {
                        // Persist before spawn: silent NSIS may KillProcess before Drop runs.
                        app.flush_jobs_save_now();
                        app.flush_window_layout_now();
                        if let Err(message) = launch_installer(&installer_path, true) {
                            app.update_busy = false;
                            app.show_error_toast(message, cx);
                            cx.notify();
                            return;
                        }
                        app.show_toast("Installing update — RusticDL will restart…", cx);
                        // Bypass close-to-tray so quit actually tears down the process.
                        app.force_quit = true;
                        app.stop_tray();
                        cx.notify();
                        cx.quit();
                    }
                    Err(message) => {
                        app.update_busy = false;
                        app.show_error_toast(message, cx);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
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
