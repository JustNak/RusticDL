//! Browser handoff capture pollers extracted from `DownloadApp`.
//!
//! Dual-invoked from the shell `new` 80ms timer and from `Render` so prompts
//! and progress HUDs still appear when the main UI is idle or minimized.

use gpui::Context;

use super::DownloadApp;
use crate::download::JobState;
use crate::prompt_window::{
    open_browser_complete_window, open_browser_progress_window, open_browser_prompt_window,
};

impl DownloadApp {
    /// Poll for browser ask-mode handoffs and open a dedicated prompt window.
    ///
    /// Safe to call from the main window render loop or a background timer so
    /// prompts still appear when the main UI is idle or minimized.
    pub(crate) fn poll_browser_prompt(&mut self, cx: &mut Context<Self>) {
        // Clear tracking when the prompt was resolved, timed out, or the window closed.
        if let Some(id) = self.browser_prompt_open_id.clone() {
            if !self.ipc.is_prompt_pending(&id) {
                self.browser_prompt_open_id = None;
            } else {
                // One confirm at a time.
                return;
            }
        }

        let Some(prompt) = self.ipc.claim_next_prompt_for_ui() else {
            return;
        };
        let prompt_id = prompt.id.clone();
        let opened = open_browser_prompt_window(
            prompt,
            self.ipc.clone(),
            self.engine.clone(),
            &self.settings,
            cx,
        );
        if opened.is_some() {
            self.browser_prompt_open_id = Some(prompt_id);
        } else {
            self.browser_prompt_open_id = None;
        }
    }

    /// Open floating progress HUDs for browser handoffs (auto mode + confirm morph fallback).
    pub(crate) fn poll_browser_progress(&mut self, cx: &mut Context<Self>) {
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
            return;
        }

        for job_id in self.ipc.take_pending_progress_jobs() {
            let _ = open_browser_progress_window(
                job_id,
                self.ipc.clone(),
                self.engine.clone(),
                &self.settings,
                cx,
            );
        }
    }

    /// If Progress was closed early, re-open a Complete HUD when the job finishes.
    pub(crate) fn poll_browser_complete(&mut self, cx: &mut Context<Self>) {
        // Match enqueue / progress poll: committed bridge setting only.
        if !self.ipc.show_progress_after_handoff() {
            self.browser_watch_complete_ids.clear();
            return;
        }
        if self.browser_watch_complete_ids.is_empty() {
            return;
        }

        let mut still_watch = Vec::new();
        for job_id in self.browser_watch_complete_ids.drain(..) {
            let Some(job) = self.jobs.iter().find(|j| j.id == job_id) else {
                // Job removed — stop watching.
                continue;
            };
            match job.state {
                JobState::Completed => {
                    // Progress HUD still open will morph itself; avoid a second window.
                    if self.ipc.is_progress_hud_owned(&job_id) {
                        still_watch.push(job_id);
                    } else {
                        let opened = open_browser_complete_window(
                            job.clone(),
                            self.ipc.clone(),
                            self.engine.clone(),
                            &self.settings,
                            cx,
                        );
                        // Keep watching on open failure so Complete can retry.
                        if opened.is_none() {
                            still_watch.push(job_id);
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
