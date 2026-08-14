//! Jobs list apply path, debounced persist, and OS notify flush wiring for `DownloadApp`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::Context;

use super::DownloadApp;
use crate::download::Job;
use crate::notifications::{
    compose_balloon, filter_notify_edges, filter_pending_by_toggles, hard_os_eligible,
    in_app_summary_messages, soft_os_eligible, terminal_edges, InAppToastKind, PendingOsTerminal,
    TerminalKind,
};
use crate::persistence::save_jobs;
use crate::settings::OsNotifyMode;

/// Debounce progress-driven `state.json` writes; terminal transitions flush immediately.
const JOBS_SAVE_DEBOUNCE: Duration = Duration::from_secs(1);

impl DownloadApp {
    pub(crate) fn on_jobs_changed(&mut self, jobs: Arc<Vec<Job>>, cx: &mut Context<Self>) {
        if self.last_ui_update.elapsed() < Duration::from_millis(80) {
            self.pending_jobs = Some(jobs);
            return;
        }
        self.apply_jobs(jobs, cx);
    }

    pub(crate) fn apply_jobs(&mut self, jobs: Arc<Vec<Job>>, cx: &mut Context<Self>) {
        // Edge-detect BEFORE overwrite (Completed / Failed only — never Canceled).
        let edges = terminal_edges(&self.jobs, &jobs);
        let notify_edges = filter_notify_edges(
            &edges,
            self.settings.notify_on_complete,
            self.settings.notify_on_fail,
        );

        // Pipeline A — in-app toasts (immediate when window is visible).
        if !self.window_hidden_to_tray && !notify_edges.is_empty() {
            for (kind, message) in
                in_app_summary_messages(&notify_edges, self.settings.os_notify_mode)
            {
                match kind {
                    InAppToastKind::Info => self.show_toast(message, cx),
                    InAppToastKind::Error => self.show_error_toast(message, cx),
                }
            }
        }

        // Pipeline B — OS balloons (burst coalesce; hard eligibility at flush).
        if soft_os_eligible(self.settings.os_notify_mode) && !notify_edges.is_empty() {
            let pending: Vec<PendingOsTerminal> = notify_edges
                .iter()
                .map(PendingOsTerminal::from_edge)
                .collect();
            let now = Instant::now();
            if self.os_notify_buffer.enqueue(pending, now) {
                self.flush_os_notify(cx);
            }
        }

        let force_persist = jobs_need_immediate_persist(&self.jobs, &jobs);
        self.prune_selection(&jobs);
        self.jobs = jobs;
        self.last_ui_update = Instant::now();
        self.jobs_dirty = true;
        if force_persist {
            self.flush_jobs_save_now();
        } else {
            self.flush_jobs_save_if_due();
        }
        self.ipc.update_jobs(Arc::clone(&self.jobs));
        // Adopt bridge extension settings only when the user has no local preview
        // (unsaved toggles). Never clobber while extension_settings_dirty.
        self.sync_extension_settings_from_bridge(false);
        cx.notify();
    }

    pub(crate) fn flush_os_notify_if_due(&mut self, cx: &mut Context<Self>) {
        if self.os_notify_buffer.deadline_elapsed(Instant::now()) {
            self.flush_os_notify(cx);
        }
    }

    /// Flush OS coalesce buffer: re-check hard eligibility, compose one balloon, show.
    ///
    /// Arms the 2s burst window only after a balloon is actually shown. Hard-drops
    /// (e.g. WhenHiddenToTray while visible) do not delay the next solitary balloon.
    pub(crate) fn flush_os_notify(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let pending = self.os_notify_buffer.take_pending();
        if pending.is_empty() {
            return;
        }

        // Hard eligibility with *current* mode / visibility / toggles.
        // Do not arm burst on drop — visible completes under WhenHiddenToTray must
        // not tax the first real OS balloon after the user hides to tray.
        if !hard_os_eligible(self.settings.os_notify_mode, self.window_hidden_to_tray) {
            return;
        }

        let pending = filter_pending_by_toggles(
            pending,
            self.settings.notify_on_complete,
            self.settings.notify_on_fail,
        );
        if pending.is_empty() {
            return;
        }

        let Some(payload) = compose_balloon(&pending) else {
            return;
        };

        // Tray required for balloons; ensure lifetime when notify is on.
        self.sync_tray_lifetime(cx);
        if let Some(tray) = self.system_tray.as_ref() {
            // Allocate context only when we can actually show a balloon.
            let context_id = self.balloon_contexts.allocate(&payload);
            tray.show_notification(&payload.title, &payload.body, payload.level, context_id);
            self.os_notify_buffer.after_flush(now);
        } else {
            eprintln!("rusticdl: OS notification skipped (tray unavailable)");
            // Always mode skips success in-app because "OS covers success". If the
            // tray is missing, fall back so visible completes are not silent.
            if self.settings.os_notify_mode == OsNotifyMode::Always && !self.window_hidden_to_tray {
                self.fallback_in_app_for_missed_os_complete(&pending, cx);
            }
        }
    }

    /// In-app Info for complete edges when Always OS path could not show a balloon.
    fn fallback_in_app_for_missed_os_complete(
        &mut self,
        pending: &[PendingOsTerminal],
        cx: &mut Context<Self>,
    ) {
        let completes: Vec<&PendingOsTerminal> = pending
            .iter()
            .filter(|p| p.kind == TerminalKind::Complete)
            .collect();
        if completes.is_empty() {
            return;
        }
        let message = if completes.len() == 1 {
            format!("Download complete: {}", completes[0].filename)
        } else {
            format!("{} downloads finished", completes.len())
        };
        self.show_toast(message, cx);
    }

    pub(crate) fn flush_jobs_save_if_due(&mut self) {
        if !self.jobs_dirty {
            return;
        }
        if self.last_jobs_save.elapsed() < JOBS_SAVE_DEBOUNCE {
            return;
        }
        self.flush_jobs_save_now();
    }

    pub(crate) fn flush_jobs_save_now(&mut self) {
        if !self.jobs_dirty {
            return;
        }
        self.jobs_dirty = false;
        self.last_jobs_save = Instant::now();
        let _ = save_jobs(&self.paths, &self.jobs);
    }
}

/// Persist immediately on membership, state, version, or structural map changes.
/// `written`-only map ticks stay on the 1s debounce (§2.8 accepted lag).
fn jobs_need_immediate_persist(previous: &[Job], next: &[Job]) -> bool {
    if previous.len() != next.len() {
        return true;
    }
    use std::collections::HashMap;
    let prev: HashMap<&str, &Job> = previous.iter().map(|job| (job.id.as_str(), job)).collect();
    for job in next {
        match prev.get(job.id.as_str()) {
            None => return true,
            Some(prev_job)
                if prev_job.state != job.state
                    || prev_job.transfer_format_version != job.transfer_format_version
                    || segment_map_structure_changed(&prev_job.segment_map, &job.segment_map) =>
            {
                return true;
            }
            _ => {}
        }
    }
    previous
        .iter()
        .any(|job| !next.iter().any(|n| n.id == job.id))
}

/// Force persist on Some/None or bounds/state/preallocated diffs — not `written`.
fn segment_map_structure_changed(
    previous: &Option<crate::download::segment::SegmentMap>,
    next: &Option<crate::download::segment::SegmentMap>,
) -> bool {
    match (previous, next) {
        (None, None) => false,
        (Some(_), None) | (None, Some(_)) => true,
        (Some(a), Some(b)) => !a.structure_eq(b),
    }
}

#[cfg(test)]
mod tests {
    use super::jobs_need_immediate_persist;
    use crate::download::segment::{Segment, SegmentMap, SegmentState};
    use crate::download::{Job, JobState};
    use std::path::PathBuf;

    fn sample_job(id: &str, state: JobState) -> Job {
        let mut job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        job.id = id.into();
        job.state = state;
        job
    }

    fn two_seg_map(written0: u64, written1: u64) -> SegmentMap {
        SegmentMap {
            total_bytes: 1000,
            segment_count: 2,
            segments: vec![
                Segment {
                    index: 0,
                    start: 0,
                    end: 499,
                    written: written0,
                    state: SegmentState::Active,
                },
                Segment {
                    index: 1,
                    start: 500,
                    end: 999,
                    written: written1,
                    state: SegmentState::Pending,
                },
            ],
            preallocated: true,
        }
    }

    #[test]
    fn persist_skips_pure_progress_ticks() {
        let prev = vec![sample_job("a", JobState::Downloading)];
        let mut next = vec![sample_job("a", JobState::Downloading)];
        next[0].downloaded_bytes = 50;
        next[0].progress = 5.0;
        assert!(!jobs_need_immediate_persist(&prev, &next));
    }

    #[test]
    fn persist_forces_on_state_change() {
        let prev = vec![sample_job("a", JobState::Downloading)];
        let next = vec![sample_job("a", JobState::Paused)];
        assert!(jobs_need_immediate_persist(&prev, &next));
    }

    #[test]
    fn persist_forces_on_transfer_format_version_change() {
        let prev = vec![sample_job("a", JobState::Downloading)];
        let mut next = vec![sample_job("a", JobState::Downloading)];
        next[0].transfer_format_version = 1;
        assert!(jobs_need_immediate_persist(&prev, &next));
    }

    #[test]
    fn persist_forces_on_segment_map_structural_diff() {
        let mut prev = vec![sample_job("a", JobState::Downloading)];
        prev[0].transfer_format_version = 1;
        prev[0].segment_map = Some(two_seg_map(10, 0));

        let mut next = prev.clone();
        assert!(!jobs_need_immediate_persist(&prev, &next));

        // written-only tick: debounce (same bounds/state/preallocated).
        next[0].segment_map = Some(two_seg_map(20, 0));
        assert!(!jobs_need_immediate_persist(&prev, &next));

        // SegmentState change with same written: structural — force persist.
        let mut state_changed = two_seg_map(10, 0);
        state_changed.segments[0].state = SegmentState::Completed;
        next[0].segment_map = Some(state_changed);
        assert!(jobs_need_immediate_persist(&prev, &next));

        next[0].segment_map = None;
        assert!(jobs_need_immediate_persist(&prev, &next));
    }

    #[test]
    fn persist_forces_on_membership_change() {
        let prev = vec![sample_job("a", JobState::Queued)];
        let next = vec![
            sample_job("a", JobState::Queued),
            sample_job("b", JobState::Queued),
        ];
        assert!(jobs_need_immediate_persist(&prev, &next));
    }
}
