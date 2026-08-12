//! Staged self-update flow extracted from `DownloadApp`.
//!
//! Toast stages (interactive + silent when an update exists):
//! 1. **Checking for update…**
//! 2a. **You're up to date** — or —
//! 2b. **Update available vX.Y.Z** `[Update]` → **Restart to update** `[Restart]`
//! 3. On Restart: flush state, spawn **RusticDL Updater**, quit.
//! 4. Updater downloads, runs NSIS `/S`, relaunches the main app.
//!
//! Channel (`UpdateChannel`) selects Stable vs Nightly GitHub Releases.
//! In-flight checks are invalidated when the channel changes.

use std::time::Duration;

use gpui::Context;

use super::toast::{ToastActionKind, ToastKind};
use super::DownloadApp;
use crate::branding::{APP_VERSION, UPDATER_NAME};
use crate::settings::UpdateChannel;
use crate::updater::{
    check_for_update, launch_updater, LaunchUpdaterOpts, UpdateCheck, UpdateInfo,
};

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
        self.update_check_gen = self.update_check_gen.wrapping_add(1);
        let check_gen = self.update_check_gen;
        self.update_busy = true;
        if interactive {
            self.replace_update_toast("Checking for update…", ToastKind::Info, None, cx);
        }
        cx.notify();
        let channel = self.settings.update_channel;
        spawn_update_check(interactive, channel, check_gen, cx);
    }

    pub(crate) fn on_update_check_finished(
        &mut self,
        interactive: bool,
        check_gen: u64,
        result: Result<UpdateCheck, String>,
        cx: &mut Context<Self>,
    ) {
        // Channel switch (or a newer check / apply) invalidates this completion.
        if check_gen != self.update_check_gen {
            return;
        }
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

    pub(crate) fn begin_apply_update(&mut self, info: UpdateInfo, cx: &mut Context<Self>) {
        if self.update_busy {
            self.show_toast("An update is already in progress…", cx);
            return;
        }
        // Invalidate any in-flight check so a late result cannot clear busy mid-handoff.
        self.update_check_gen = self.update_check_gen.wrapping_add(1);
        self.update_busy = true;
        self.begin_apply_update_inner(info, cx);
    }

    /// Persist state, spawn RusticDL Updater, then quit so files can be replaced.
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

        let opts = LaunchUpdaterOpts {
            download_url: info.setup_download_url.clone(),
            from_version,
            to_version: info.latest_version.clone(),
            release_page: info.html_url.clone(),
            setup_size: info.setup_size,
        };

        if let Err(message) = launch_updater(&opts) {
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
pub(crate) fn spawn_update_check(
    interactive: bool,
    channel: UpdateChannel,
    check_gen: u64,
    cx: &mut Context<DownloadApp>,
) {
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
            .and_then(|rt| rt.block_on(check_for_update(channel)));
        let _ = tx.send_blocking(result);
    });

    cx.spawn(async move |this, cx| {
        let result = rx
            .recv()
            .await
            .unwrap_or_else(|_| Err("Update check was cancelled unexpectedly.".into()));
        let _ = this.update(cx, |app, cx| {
            app.on_update_check_finished(interactive, check_gen, result, cx);
        });
    })
    .detach();
}
