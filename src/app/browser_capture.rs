//! Browser handoff capture pollers extracted from `DownloadApp`.
//!
//! Invoked from the shell 80ms timer (not from `Render`). Opening a native
//! window during paint re-enters GPUI's draw path and crashes on Windows.
//! At most one new capture window is created per tick so two HUDs never
//! `open_window` in the same update.

use gpui::{Context, WindowHandle};
use gpui_component::Root;

use super::DownloadApp;
use crate::download::JobState;
use crate::prompt_window::{
    open_browser_complete_window, open_browser_progress_window, open_browser_prompt_window,
};

/// Confirm / Progress HUDs are independent of the main HWND.
///
/// PR 133 parked the hidden idle apply/notify path and closed leftover
/// Complete HUDs on tray hide. Gating this poll on `window_hidden_to_tray`
/// also blocked Confirm (and needed Progress) while the main window is
/// `SW_HIDE`. Idle park still skips clone+notify; this poll is a cheap
/// queue check and may open a separate capture window.
pub(crate) fn should_poll_capture_huds(_window_hidden_to_tray: bool) -> bool {
    true
}

impl DownloadApp {
    /// Open the floating progress HUD for a queue job that is still downloading.
    ///
    /// No-ops when the job is missing or not in-flight. If a HUD already owns
    /// this job (browser handoff or a previous open), tell the user instead of
    /// stacking a second window.
    pub(crate) fn open_job_progress_popup(&mut self, job_id: &str, cx: &mut Context<Self>) {
        let Some(job) = self.jobs.iter().find(|j| j.id == job_id) else {
            return;
        };
        if !job.state.can_show_progress_popup() {
            return;
        }
        if self.ipc.is_progress_hud_owned(job_id) {
            self.show_toast("Progress window is already open.", cx);
            return;
        }
        if !self.remember_capture_window(open_browser_progress_window(
            job_id.to_string(),
            self.ipc.clone(),
            self.engine.clone(),
            &self.settings,
            cx,
        )) {
            self.show_toast("Could not open the progress window.", cx);
        }
    }

    fn remember_capture_window(&mut self, handle: Option<WindowHandle<Root>>) -> bool {
        if let Some(handle) = handle {
            self.capture_windows.push(handle);
            true
        } else {
            false
        }
    }

    /// Open at most one pending confirm / progress / complete HUD this tick.
    ///
    /// Must run while the main window is tray-hidden (`SW_HIDE`). Capture
    /// still arrives over IPC; the user sees it through these windows.
    pub(crate) fn poll_browser_capture(&mut self, cx: &mut Context<Self>) {
        if self.open_next_browser_prompt(cx) {
            return;
        }
        if self.open_next_browser_progress(cx) {
            return;
        }
        self.open_next_browser_complete(cx);
    }

    /// Poll for browser ask-mode handoffs and open a dedicated prompt window.
    ///
    /// Returns true when a new window was created (caller should stop this tick).
    fn open_next_browser_prompt(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(prompt) = self.ipc.claim_next_prompt_for_ui() else {
            return false;
        };
        self.remember_capture_window(open_browser_prompt_window(
            prompt,
            self.ipc.clone(),
            self.engine.clone(),
            &self.settings,
            cx,
        ))
    }

    /// Open one floating progress HUD for a browser handoff (auto mode + morph fallback).
    fn open_next_browser_progress(&mut self, cx: &mut Context<Self>) -> bool {
        // Always adopt watch ids so Complete re-open works for Confirm morph too.
        for job_id in self.ipc.take_progress_watch_jobs() {
            if !self
                .browser_watch_complete_ids
                .iter()
                .any(|id| id == &job_id)
            {
                self.browser_watch_complete_ids.push(job_id);
            }
        }

        // Use committed bridge setting (same source as enqueue), not draft UI toggles.
        if !self.ipc.show_progress_after_handoff() {
            // Drain open-queue so ids do not pile up while the setting is off.
            let _ = self.ipc.take_pending_progress_jobs();
            return false;
        }

        let Some(job_id) = self.ipc.take_pending_progress_jobs_n(1).into_iter().next() else {
            return false;
        };
        self.remember_capture_window(open_browser_progress_window(
            job_id,
            self.ipc.clone(),
            self.engine.clone(),
            &self.settings,
            cx,
        ))
    }

    /// If Progress was closed early, re-open one Complete HUD when a watched job finishes.
    fn open_next_browser_complete(&mut self, cx: &mut Context<Self>) {
        // Match enqueue / progress poll: committed bridge setting only.
        if !self.ipc.show_progress_after_handoff() {
            self.browser_watch_complete_ids.clear();
            return;
        }
        if self.browser_watch_complete_ids.is_empty() {
            return;
        }

        let pending: Vec<String> = self.browser_watch_complete_ids.drain(..).collect();
        let mut still_watch = Vec::new();
        let mut opened = false;
        for job_id in pending {
            let Some(job) = self.jobs.iter().find(|j| j.id == job_id).cloned() else {
                // Job removed — stop watching.
                continue;
            };
            match job.state {
                JobState::Completed => {
                    // Progress HUD still open will morph itself; avoid a second window.
                    if self.ipc.is_progress_hud_owned(&job_id) || opened {
                        still_watch.push(job_id);
                    } else {
                        let handle_ok = self.remember_capture_window(open_browser_complete_window(
                            job,
                            self.ipc.clone(),
                            self.engine.clone(),
                            &self.settings,
                            cx,
                        ));
                        // Keep watching on open failure so Complete can retry.
                        if !handle_ok {
                            still_watch.push(job_id);
                        } else {
                            opened = true;
                        }
                    }
                }
                JobState::Failed | JobState::Canceled => {
                    // Terminal non-success: do not show Complete.
                }
                _ => still_watch.push(job_id),
            }
        }
        self.browser_watch_complete_ids = still_watch;
    }
}

#[cfg(test)]
mod tests {
    use super::should_poll_capture_huds;

    #[test]
    fn capture_huds_are_polled_while_tray_hidden() {
        // Regression: PR 133 gated poll_browser_capture on !window_hidden_to_tray.
        assert!(
            should_poll_capture_huds(true),
            "Confirm / Progress must open while the main window is SW_HIDE"
        );
        assert!(should_poll_capture_huds(false));
    }
}
