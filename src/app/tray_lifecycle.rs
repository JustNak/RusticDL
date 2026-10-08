use gpui::{Context, Window};
use tokio::sync::oneshot;

use super::DownloadApp;
use crate::download::{open_path, EngineCommand};
use crate::hyprland;
use crate::notifications::{spawn_session_notify, BalloonOutcome};
use crate::prompt_window::close_capture_window;
use crate::settings::OsNotifyMode;
use crate::tray::{
    hide_main_window, main_window_hwnd, reassert_tray_hide, show_main_window,
    show_main_window_hwnd, SystemTray, TrayEvent,
};

/// What closing the main window should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseAction {
    /// Flush and exit the process.
    Quit,
    /// Hide the window and keep running behind the tray icon.
    HideToTray,
    /// No tray to restore from: minimize (still reachable from the taskbar/dock)
    /// and keep running.
    Minimize,
    /// Hyprland without a tray host: still hide (special workspace) and keep
    /// running. Relaunch, the extension, and Ctrl+Q after restoring remain.
    HideWithoutTray,
}

/// Pick the close behaviour. Pure so the platform matrix is unit-tested.
///
/// A window may only vanish when something can bring it back (the tray).
/// Without a tray, desktops with a working minimize keep the app alive in the
/// taskbar. Hyprland has no minimize, so it hides to a special workspace even
/// without a tray; a relaunch (single-instance) or an extension action that
/// needs the UI brings the window back. Windows and macOS keep their old
/// behaviour.
pub(crate) fn decide_close_action(
    close_to_background: bool,
    tray_available: bool,
    linux: bool,
    hyprland: bool,
) -> CloseAction {
    if !close_to_background {
        CloseAction::Quit
    } else if tray_available {
        CloseAction::HideToTray
    } else if linux && hyprland {
        CloseAction::HideWithoutTray
    } else if linux {
        CloseAction::Minimize
    } else {
        CloseAction::Quit
    }
}

/// Ctrl+Q quits on Linux, the one place a hidden or tray-less app needs an
/// in-app way out. Other platforms already have the tray menu / OS conventions.
pub(crate) fn is_quit_chord(keystroke: &gpui::Keystroke) -> bool {
    let m = &keystroke.modifiers;
    cfg!(target_os = "linux")
        && keystroke.key == "q"
        && m.control
        && !m.alt
        && !m.shift
        && !m.platform
}

/// Whether a tray icon should exist.
///
/// Linux shows OS notifications through `notify-send`, so the notification
/// mode alone must not put an icon in the user's panel.
pub(crate) fn tray_wanted(
    close_to_tray: bool,
    window_hidden: bool,
    os_notify_mode: OsNotifyMode,
) -> bool {
    close_to_tray
        || window_hidden
        || (!cfg!(target_os = "linux") && os_notify_mode != OsNotifyMode::Off)
}

impl DownloadApp {
    pub(crate) fn handle_window_should_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.force_quit {
            self.flush_window_layout_now();
            return true;
        }
        if self.settings.close_to_tray {
            self.ensure_tray(cx);
        }

        let action = decide_close_action(
            self.settings.close_to_tray,
            self.system_tray.is_some(),
            cfg!(target_os = "linux"),
            hyprland::is_hyprland(),
        );
        match action {
            CloseAction::Quit => {
                self.force_quit_app(cx);
                return false;
            }
            CloseAction::HideToTray => {
                self.flush_window_layout_now();
                self.remember_main_window(window);
                hide_main_window(window);
                self.window_hidden_to_tray = true;
                self.close_capture_huds(cx);
            }
            CloseAction::HideWithoutTray => {
                self.flush_window_layout_now();
                self.remember_main_window(window);
                hide_main_window(window);
                self.window_hidden_to_tray = true;
                self.close_capture_huds(cx);
                self.notify_hidden_without_tray();
            }
            CloseAction::Minimize => {
                self.flush_window_layout_now();
                self.remember_main_window(window);
                window.minimize_window();
            }
        }
        cx.notify();
        false
    }

    /// One desktop notification per session so a trayless hide is not a
    /// silent disappearance. Best-effort: `notify-send` may be missing.
    fn notify_hidden_without_tray(&mut self) {
        if self.no_tray_hide_notified {
            return;
        }
        self.no_tray_hide_notified = true;
        spawn_session_notify(
            "RusticDL is still running",
            "No system tray was found. Downloads and the browser extension keep working. Launch RusticDL again to bring the window back.",
        );
    }

    fn remember_main_window(&mut self, window: &Window) {
        let hwnd = main_window_hwnd(window);
        if hwnd != 0 {
            self.main_hwnd = hwnd;
        }
        self.main_window = Some(window.window_handle());
    }

    pub(crate) fn close_capture_huds(&mut self, cx: &mut Context<Self>) {
        self.ipc.request_close_capture_windows();
        self.browser_watch_complete_ids.clear();
        for handle in self.capture_windows.drain(..) {
            close_capture_window(&handle, cx);
        }
    }

    fn ensure_tray(&mut self, cx: &mut Context<Self>) {
        if self.system_tray.is_some() {
            return;
        }
        let (tray_tx, tray_rx) = async_channel::unbounded::<TrayEvent>();
        self.system_tray = SystemTray::start(tray_tx);
        if self.system_tray.is_none() {
            return;
        }
        cx.spawn(async move |this, cx| {
            while let Ok(event) = tray_rx.recv().await {
                let result = this.update(cx, |app, cx| app.handle_tray_event(event, cx));
                if result.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn stop_tray(&mut self) {
        self.system_tray = None;
    }

    fn stop_tray_nonblocking(&mut self) {
        if let Some(tray) = self.system_tray.take() {
            let _ = std::thread::Builder::new()
                .name("rusticdl-tray-shutdown".into())
                .spawn(move || drop(tray));
        }
    }

    /// Fully quit: flush state, tear down tray, and exit the app process loop.
    ///
    /// Must not wait on main-window `Render` — when hidden to tray the HWND
    /// often stops painting, so a deferred "pending exit" never runs.
    pub(crate) fn force_quit_app(&mut self, cx: &mut Context<Self>) {
        self.force_quit = true;
        self.flush_window_layout_now();
        let (ack_tx, ack_rx) = oneshot::channel();
        self.engine.send(EngineCommand::Drain { ack: Some(ack_tx) });
        cx.spawn(async move |this, cx| {
            if ack_rx.await.is_err() {
                return;
            }
            let _ = this.update(cx, |app, cx| {
                app.stop_tray_nonblocking();
                cx.quit();
            });
        })
        .detach();
    }

    pub(crate) fn sync_tray_lifetime(&mut self, cx: &mut Context<Self>) {
        if tray_wanted(
            self.settings.close_to_tray,
            self.window_hidden_to_tray,
            self.settings.os_notify_mode,
        ) {
            self.ensure_tray(cx);
        } else {
            self.stop_tray();
        }
    }

    pub(crate) fn handle_tray_event(&mut self, event: TrayEvent, cx: &mut Context<Self>) {
        match event {
            TrayEvent::ShowWindow => {
                self.restore_main_window_now(cx);
                self.pending_tray_show = true;
                cx.notify();
            }
            TrayEvent::Exit => {
                self.force_quit_app(cx);
            }
            TrayEvent::BalloonUserClick { context_id } => {
                self.restore_main_window_now(cx);
                self.pending_tray_show = true;
                self.pending_balloon_click = Some(context_id);
                cx.notify();
            }
        }
        self.ipc.wake_ui();
    }

    /// Restore the main window without waiting for a render: a hidden window
    /// often stops painting. Windows uses the cached HWND; Linux asks Hyprland
    /// to pull the window back and activates it through the stored GPUI handle.
    fn restore_main_window_now(&mut self, cx: &mut Context<Self>) {
        self.window_hidden_to_tray = false;
        if self.main_hwnd != 0 {
            show_main_window_hwnd(self.main_hwnd);
        }
        #[cfg(target_os = "linux")]
        {
            hyprland::show_main_windows();
            if let Some(handle) = self.main_window {
                cx.defer(move |cx| {
                    let _ = handle.update(cx, |_, window, _| window.activate_window());
                });
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = cx;
    }

    pub(crate) fn poll_hidden_window_actions(&mut self, cx: &mut Context<Self>) {
        if self.ipc.take_show_window_request() {
            self.restore_main_window_now(cx);
            self.pending_tray_show = true;
            cx.notify();
        }
    }

    pub(crate) fn apply_pending_tray_actions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hwnd = main_window_hwnd(window);
        if hwnd != 0 {
            self.main_hwnd = hwnd;
        }
        if self.pending_tray_show {
            self.pending_tray_show = false;
            self.window_hidden_to_tray = false;
            show_main_window(window);
        } else if self.window_hidden_to_tray {
            reassert_tray_hide(self.main_hwnd);
        }
        if let Some(context_id) = self.pending_balloon_click.take() {
            self.handle_balloon_click(context_id, cx);
        }
    }

    fn handle_balloon_click(&mut self, context_id: u64, cx: &mut Context<Self>) {
        let Some(ctx) = self.balloon_contexts.lookup(context_id).cloned() else {
            return;
        };
        if ctx.kind != BalloonOutcome::SingleComplete {
            return;
        }
        let Some(path) = ctx.target_path else {
            return;
        };
        if let Err(msg) = open_path(&path) {
            self.show_error_toast(format!("Could not open file: {msg}"), cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_to_background_off_always_quits() {
        for tray in [false, true] {
            for (linux, hypr) in [(false, false), (true, false), (true, true)] {
                assert_eq!(
                    decide_close_action(false, tray, linux, hypr),
                    CloseAction::Quit
                );
            }
        }
    }

    #[test]
    fn tray_present_hides_everywhere() {
        for (linux, hypr) in [(false, false), (true, false), (true, true)] {
            assert_eq!(
                decide_close_action(true, true, linux, hypr),
                CloseAction::HideToTray
            );
        }
    }

    #[test]
    fn linux_without_tray_minimizes_unless_hyprland() {
        assert_eq!(
            decide_close_action(true, false, true, false),
            CloseAction::Minimize
        );
    }

    #[test]
    fn hyprland_without_tray_hides_instead_of_quitting() {
        assert_eq!(
            decide_close_action(true, false, true, true),
            CloseAction::HideWithoutTray
        );
        assert_eq!(
            decide_close_action(false, false, true, true),
            CloseAction::Quit
        );
    }

    #[test]
    fn windows_and_macos_without_tray_still_quit() {
        assert_eq!(
            decide_close_action(true, false, false, false),
            CloseAction::Quit
        );
    }

    #[test]
    fn quit_chord_is_plain_ctrl_q_on_linux() {
        let parse = |s: &str| gpui::Keystroke::parse(s).expect("keystroke");
        assert_eq!(is_quit_chord(&parse("ctrl-q")), cfg!(target_os = "linux"));
        assert!(!is_quit_chord(&parse("q")));
        assert!(!is_quit_chord(&parse("ctrl-shift-q")));
        assert!(!is_quit_chord(&parse("ctrl-w")));
    }

    #[test]
    fn tray_wanted_follows_close_setting_and_hidden_state() {
        assert!(tray_wanted(true, false, OsNotifyMode::Off));
        assert!(tray_wanted(false, true, OsNotifyMode::Off));
        assert!(!tray_wanted(false, false, OsNotifyMode::Off));
    }

    #[test]
    fn notify_mode_only_forces_tray_off_linux() {
        assert_eq!(
            tray_wanted(false, false, OsNotifyMode::Always),
            !cfg!(target_os = "linux")
        );
    }
}
