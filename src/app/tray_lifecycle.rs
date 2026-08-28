
use gpui::{Context, Window};

use super::DownloadApp;
use crate::download::open_path;
use crate::notifications::BalloonOutcome;
use crate::prompt_window::close_capture_window;
use crate::settings::OsNotifyMode;
use crate::tray::{
    hide_main_window, main_window_hwnd, show_main_window, show_main_window_hwnd, SystemTray,
    TrayEvent,
};

impl DownloadApp {
    pub(crate) fn handle_window_should_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.force_quit || !self.settings.close_to_tray {
            self.flush_window_layout_now();
            self.flush_jobs_save_now();
            return true;
        }

        self.ensure_tray(cx);
        if self.system_tray.is_none() {
            self.flush_window_layout_now();
            self.flush_jobs_save_now();
            return true;
        }

        self.flush_window_layout_now();
        self.flush_jobs_save_if_due();
        let hwnd = main_window_hwnd(window);
        if hwnd != 0 {
            self.main_hwnd = hwnd;
        }
        hide_main_window(window);
        self.window_hidden_to_tray = true;
        self.close_capture_huds(cx);
        cx.notify();
        false
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
        self.flush_jobs_save_now();
        self.stop_tray_nonblocking();
        cx.quit();
    }

    pub(crate) fn sync_tray_lifetime(&mut self, cx: &mut Context<Self>) {
        let needed = self.settings.close_to_tray
            || self.window_hidden_to_tray
            || self.settings.os_notify_mode != OsNotifyMode::Off;
        if needed {
            self.ensure_tray(cx);
        } else {
            self.stop_tray();
        }
    }

    pub(crate) fn handle_tray_event(&mut self, event: TrayEvent, cx: &mut Context<Self>) {
        match event {
            TrayEvent::ShowWindow => {
                self.restore_main_window_now();
                self.pending_tray_show = true;
                cx.notify();
            }
            TrayEvent::Exit => {
                self.force_quit_app(cx);
            }
            TrayEvent::BalloonUserClick { context_id } => {
                self.restore_main_window_now();
                self.pending_tray_show = true;
                self.pending_balloon_click = Some(context_id);
                cx.notify();
            }
        }
        self.ipc.wake_ui();
    }

    /// Restore the main window using the cached HWND (no GPUI Window required).
    fn restore_main_window_now(&mut self) {
        self.window_hidden_to_tray = false;
        if self.main_hwnd != 0 {
            show_main_window_hwnd(self.main_hwnd);
        }
    }

    pub(crate) fn poll_hidden_window_actions(&mut self, cx: &mut Context<Self>) {
        if self.ipc.take_show_window_request() {
            self.restore_main_window_now();
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
