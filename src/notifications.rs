//! Terminal-job notification policy: edge detect, dual pipelines (in-app + OS),
//! OS burst coalesce, and balloon click context mapping.
//!
//! See design plan A1 (Windows completion notifications).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::download::{Job, JobState};
use crate::settings::OsNotifyMode;
use crate::tray::NotifyLevel;

/// Burst window after any OS flush; edges arriving inside it are held and merged.
pub const OS_BURST_WINDOW: Duration = Duration::from_secs(2);
/// Flush immediately when the OS pending buffer reaches this size.
pub const OS_HIGH_WATER: usize = 20;
/// Retain balloon click contexts for late clicks.
pub const BALLOON_CONTEXT_CAP: usize = 8;

/// Kind of terminal transition we may notify on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalKind {
    Complete,
    Fail,
}

/// One non-terminal → Completed/Failed edge (Canceled is never emitted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalEdge {
    pub job_id: String,
    pub kind: TerminalKind,
    pub filename: String,
    pub error: Option<String>,
    /// Snapshot of `target_path` at edge time (for open-on-click).
    pub target_path: PathBuf,
}

/// Soft-eligible OS candidate (prefs already applied at enqueue).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOsTerminal {
    pub kind: TerminalKind,
    pub filename: String,
    pub error: Option<String>,
    pub job_id: String,
    pub target_path: Option<PathBuf>,
}

impl From<&TerminalEdge> for PendingOsTerminal {
    fn from(edge: &TerminalEdge) -> Self {
        Self {
            kind: edge.kind,
            filename: edge.filename.clone(),
            error: edge.error.clone(),
            job_id: edge.job_id.clone(),
            target_path: match edge.kind {
                TerminalKind::Complete => Some(edge.target_path.clone()),
                TerminalKind::Fail => None,
            },
        }
    }
}

/// What a balloon click should do beyond showing the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalloonOutcome {
    SingleComplete,
    SingleFail,
    Coalesced,
}

/// Opaque tray context + policy payload for balloon clicks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalloonClickContext {
    pub context_id: u64,
    pub kind: BalloonOutcome,
    pub job_id: Option<String>,
    pub target_path: Option<PathBuf>,
}

/// Composed balloon ready for `SystemTray::show_notification`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsBalloon {
    pub title: String,
    pub body: String,
    pub level: NotifyLevel,
    pub context: BalloonClickContext,
}

/// Result of pushing soft-eligible edges into the OS coalesce buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoalesceAction {
    /// Flush `pending` now (solitary edge, high-water, or deadline).
    FlushNow,
    /// Hold; wait for deadline tick or more edges.
    Wait,
    /// Nothing pending.
    None,
}

/// OS-only coalesce buffer (Pipeline B). In-app toasts never touch this.
#[derive(Debug, Default)]
pub struct OsCoalesceState {
    pub pending: Vec<PendingOsTerminal>,
    pub coalesce_deadline: Option<Instant>,
    pub burst_open_until: Option<Instant>,
}

impl OsCoalesceState {
    pub fn is_burst_open(&self, now: Instant) -> bool {
        self.burst_open_until
            .map(|until| now < until)
            .unwrap_or(false)
    }

    /// Append soft-eligible edges and decide whether to flush immediately.
    pub fn push_edges(
        &mut self,
        edges: impl IntoIterator<Item = PendingOsTerminal>,
        now: Instant,
    ) -> CoalesceAction {
        let before = self.pending.len();
        self.pending.extend(edges);
        let added = self.pending.len() - before;
        if added == 0 {
            return self.poll_deadline(now);
        }

        if self.pending.len() >= OS_HIGH_WATER {
            return CoalesceAction::FlushNow;
        }

        let burst_open = self.is_burst_open(now);

        // Solitary edge outside a burst window → immediate OS notify.
        if self.pending.len() == 1 && !burst_open {
            return CoalesceAction::FlushNow;
        }

        // Multi-edge and/or open burst: hold until deadline (or high-water).
        if self.coalesce_deadline.is_none() {
            self.coalesce_deadline = Some(if burst_open {
                self.burst_open_until
                    .unwrap_or_else(|| now + OS_BURST_WINDOW)
            } else {
                now + OS_BURST_WINDOW
            });
        }

        CoalesceAction::Wait
    }

    /// Check if a previously armed deadline has elapsed.
    pub fn poll_deadline(&self, now: Instant) -> CoalesceAction {
        if self.pending.is_empty() {
            return CoalesceAction::None;
        }
        if self.pending.len() >= OS_HIGH_WATER {
            return CoalesceAction::FlushNow;
        }
        match self.coalesce_deadline {
            Some(deadline) if now >= deadline => CoalesceAction::FlushNow,
            Some(_) => CoalesceAction::Wait,
            None => CoalesceAction::FlushNow,
        }
    }

    /// Take all pending items and open the post-flush burst window.
    ///
    /// Call after either firing or dropping an OS flush.
    pub fn take_for_flush(&mut self, now: Instant) -> Vec<PendingOsTerminal> {
        let items = std::mem::take(&mut self.pending);
        self.coalesce_deadline = None;
        self.burst_open_until = Some(now + OS_BURST_WINDOW);
        items
    }
}

/// Ring buffer of recent balloon click contexts.
#[derive(Debug, Default)]
pub struct BalloonContextMap {
    next_id: u64,
    pub contexts: VecDeque<BalloonClickContext>,
}

impl BalloonContextMap {
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    pub fn push(&mut self, ctx: BalloonClickContext) {
        self.contexts.push_back(ctx);
        while self.contexts.len() > BALLOON_CONTEXT_CAP {
            self.contexts.pop_front();
        }
    }

    pub fn lookup(&self, context_id: u64) -> Option<&BalloonClickContext> {
        self.contexts.iter().find(|c| c.context_id == context_id)
    }
}

/// Diff previous vs next job snapshots for terminal Complete/Failed edges.
///
/// Canceled is intentionally excluded. Intermediate retry states are not terminal.
pub fn terminal_edges(previous: &[Job], next: &[Job]) -> Vec<TerminalEdge> {
    let prev: HashMap<&str, JobState> = previous
        .iter()
        .map(|j| (j.id.as_str(), j.state))
        .collect();

    let mut edges = Vec::new();
    for job in next {
        let prev_state = prev.get(job.id.as_str()).copied();
        let was_non_terminal = match prev_state {
            None => true,
            Some(s) => !matches!(
                s,
                JobState::Completed | JobState::Failed | JobState::Canceled
            ),
        };
        if !was_non_terminal {
            continue;
        }
        match job.state {
            JobState::Completed => edges.push(TerminalEdge {
                job_id: job.id.clone(),
                kind: TerminalKind::Complete,
                filename: job.filename.clone(),
                error: None,
                target_path: job.target_path.clone(),
            }),
            JobState::Failed => edges.push(TerminalEdge {
                job_id: job.id.clone(),
                kind: TerminalKind::Fail,
                filename: job.filename.clone(),
                error: job.error.clone(),
                target_path: job.target_path.clone(),
            }),
            // Canceled: never notify.
            _ => {}
        }
    }
    edges
}

/// Filter edges by user notify toggles (applies to both pipelines).
pub fn filter_by_notify_prefs(
    edges: &[TerminalEdge],
    notify_on_complete: bool,
    notify_on_fail: bool,
) -> Vec<TerminalEdge> {
    edges
        .iter()
        .filter(|e| match e.kind {
            TerminalKind::Complete => notify_on_complete,
            TerminalKind::Fail => notify_on_fail,
        })
        .cloned()
        .collect()
}

/// Soft OS eligibility at enqueue (mode not Off). Hard check is at flush.
pub fn os_soft_eligible(mode: OsNotifyMode) -> bool {
    mode != OsNotifyMode::Off
}

/// Hard OS eligibility re-checked at flush time.
pub fn os_hard_eligible(mode: OsNotifyMode, window_hidden_to_tray: bool) -> bool {
    match mode {
        OsNotifyMode::Off => false,
        OsNotifyMode::WhenHiddenToTray => window_hidden_to_tray,
        OsNotifyMode::Always => true,
    }
}

/// Whether Pipeline A should emit an in-app toast for this kind when the window is visible.
pub fn in_app_for_kind(mode: OsNotifyMode, kind: TerminalKind) -> Option<InAppToastKind> {
    match (mode, kind) {
        (OsNotifyMode::Always, TerminalKind::Complete) => None,
        (_, TerminalKind::Complete) => Some(InAppToastKind::Info),
        (_, TerminalKind::Fail) => Some(InAppToastKind::Error),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InAppToastKind {
    Info,
    Error,
}

/// Aggregated in-app toast lines for one apply (at most one per kind).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InAppToast {
    pub kind: InAppToastKind,
    pub message: String,
}

/// Build immediate in-app toasts for eligible edges (visible window only).
pub fn compose_in_app_toasts(mode: OsNotifyMode, edges: &[TerminalEdge]) -> Vec<InAppToast> {
    let mut completes: Vec<&TerminalEdge> = Vec::new();
    let mut fails: Vec<&TerminalEdge> = Vec::new();
    for e in edges {
        match e.kind {
            TerminalKind::Complete => {
                if in_app_for_kind(mode, TerminalKind::Complete).is_some() {
                    completes.push(e);
                }
            }
            TerminalKind::Fail => {
                if in_app_for_kind(mode, TerminalKind::Fail).is_some() {
                    fails.push(e);
                }
            }
        }
    }

    let mut out = Vec::new();
    if !completes.is_empty() {
        let message = if completes.len() == 1 {
            format!("Download complete: {}", completes[0].filename)
        } else {
            format!("{} downloads finished", completes.len())
        };
        out.push(InAppToast {
            kind: InAppToastKind::Info,
            message,
        });
    }
    if !fails.is_empty() {
        let message = if fails.len() == 1 {
            match &fails[0].error {
                Some(err) if !err.is_empty() => {
                    format!("Download failed: {} — {}", fails[0].filename, err)
                }
                _ => format!("Download failed: {}", fails[0].filename),
            }
        } else {
            format!("{} downloads failed", fails.len())
        };
        out.push(InAppToast {
            kind: InAppToastKind::Error,
            message,
        });
    }
    out
}

/// Re-filter pending OS items by current notify toggles (at flush).
pub fn refilter_pending(
    pending: Vec<PendingOsTerminal>,
    notify_on_complete: bool,
    notify_on_fail: bool,
) -> Vec<PendingOsTerminal> {
    pending
        .into_iter()
        .filter(|p| match p.kind {
            TerminalKind::Complete => notify_on_complete,
            TerminalKind::Fail => notify_on_fail,
        })
        .collect()
}

/// Compose a single OS balloon from a non-empty pending buffer + allocate context.
pub fn compose_os_balloon(pending: &[PendingOsTerminal], context_id: u64) -> Option<OsBalloon> {
    if pending.is_empty() {
        return None;
    }

    let completes: Vec<&PendingOsTerminal> = pending
        .iter()
        .filter(|p| p.kind == TerminalKind::Complete)
        .collect();
    let fails: Vec<&PendingOsTerminal> = pending
        .iter()
        .filter(|p| p.kind == TerminalKind::Fail)
        .collect();
    let c = completes.len();
    let f = fails.len();

    let (title, body, level, outcome, job_id, target_path) = if c == 1 && f == 0 {
        let item = completes[0];
        (
            "Download complete".to_string(),
            item.filename.clone(),
            NotifyLevel::Info,
            BalloonOutcome::SingleComplete,
            Some(item.job_id.clone()),
            item.target_path.clone(),
        )
    } else if c == 0 && f == 1 {
        let item = fails[0];
        let body = match &item.error {
            Some(err) if !err.is_empty() => format!("{} — {}", item.filename, err),
            _ => item.filename.clone(),
        };
        (
            "Download failed".to_string(),
            body,
            NotifyLevel::Error,
            BalloonOutcome::SingleFail,
            Some(item.job_id.clone()),
            None,
        )
    } else if c >= 1 && f == 0 {
        (
            "Downloads complete".to_string(),
            format!("{c} downloads finished"),
            NotifyLevel::Info,
            BalloonOutcome::Coalesced,
            None,
            None,
        )
    } else if c == 0 && f >= 1 {
        (
            "Downloads failed".to_string(),
            format!("{f} downloads failed"),
            NotifyLevel::Error,
            BalloonOutcome::Coalesced,
            None,
            None,
        )
    } else {
        // Mixed completes + fails → single combined balloon.
        (
            "Downloads finished".to_string(),
            format!("{c} finished, {f} failed"),
            NotifyLevel::Info,
            BalloonOutcome::Coalesced,
            None,
            None,
        )
    };

    Some(OsBalloon {
        title,
        body,
        level,
        context: BalloonClickContext {
            context_id,
            kind: outcome,
            job_id,
            target_path,
        },
    })
}

/// Whether the tray icon is required for current settings / window state.
pub fn tray_needed(
    close_to_tray: bool,
    window_hidden_to_tray: bool,
    started_minimized: bool,
    os_notify_mode: OsNotifyMode,
) -> bool {
    close_to_tray
        || window_hidden_to_tray
        || started_minimized
        || os_notify_mode != OsNotifyMode::Off
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::Job;
    use std::path::PathBuf;

    fn job(id: &str, state: JobState, name: &str) -> Job {
        let mut j = Job::new(
            format!("https://example.com/{name}"),
            name.to_string(),
            PathBuf::from(format!("C:/dl/{name}")),
            PathBuf::from(format!("C:/dl/{name}.part")),
        );
        j.id = id.to_string();
        j.state = state;
        if state == JobState::Failed {
            j.error = Some("network error".into());
        }
        j
    }

    #[test]
    fn terminal_edges_complete_and_fail_only() {
        let prev = vec![
            job("a", JobState::Downloading, "a.zip"),
            job("b", JobState::Downloading, "b.zip"),
            job("c", JobState::Downloading, "c.zip"),
            job("d", JobState::Completed, "d.zip"),
        ];
        let next = vec![
            job("a", JobState::Completed, "a.zip"),
            job("b", JobState::Failed, "b.zip"),
            job("c", JobState::Canceled, "c.zip"),
            job("d", JobState::Completed, "d.zip"),
        ];
        let edges = terminal_edges(&prev, &next);
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].kind, TerminalKind::Complete);
        assert_eq!(edges[0].job_id, "a");
        assert_eq!(edges[1].kind, TerminalKind::Fail);
        assert_eq!(edges[1].job_id, "b");
    }

    #[test]
    fn canceled_never_in_edges() {
        let prev = vec![job("x", JobState::Downloading, "x.bin")];
        let next = vec![job("x", JobState::Canceled, "x.bin")];
        assert!(terminal_edges(&prev, &next).is_empty());
    }

    #[test]
    fn filter_notify_prefs() {
        let edges = vec![
            TerminalEdge {
                job_id: "1".into(),
                kind: TerminalKind::Complete,
                filename: "a".into(),
                error: None,
                target_path: PathBuf::from("a"),
            },
            TerminalEdge {
                job_id: "2".into(),
                kind: TerminalKind::Fail,
                filename: "b".into(),
                error: Some("err".into()),
                target_path: PathBuf::from("b"),
            },
        ];
        assert_eq!(filter_by_notify_prefs(&edges, true, true).len(), 2);
        assert_eq!(filter_by_notify_prefs(&edges, true, false).len(), 1);
        assert_eq!(filter_by_notify_prefs(&edges, false, true).len(), 1);
        assert!(filter_by_notify_prefs(&edges, false, false).is_empty());
    }

    #[test]
    fn in_app_matrix_visible() {
        assert_eq!(
            in_app_for_kind(OsNotifyMode::WhenHiddenToTray, TerminalKind::Complete),
            Some(InAppToastKind::Info)
        );
        assert_eq!(
            in_app_for_kind(OsNotifyMode::WhenHiddenToTray, TerminalKind::Fail),
            Some(InAppToastKind::Error)
        );
        assert_eq!(
            in_app_for_kind(OsNotifyMode::Always, TerminalKind::Complete),
            None
        );
        assert_eq!(
            in_app_for_kind(OsNotifyMode::Always, TerminalKind::Fail),
            Some(InAppToastKind::Error)
        );
        assert_eq!(
            in_app_for_kind(OsNotifyMode::Off, TerminalKind::Complete),
            Some(InAppToastKind::Info)
        );
        assert_eq!(
            in_app_for_kind(OsNotifyMode::Off, TerminalKind::Fail),
            Some(InAppToastKind::Error)
        );
    }

    #[test]
    fn solitary_edge_flushes_immediately() {
        let mut state = OsCoalesceState::default();
        let now = Instant::now();
        let edge = PendingOsTerminal {
            kind: TerminalKind::Complete,
            filename: "solo.zip".into(),
            error: None,
            job_id: "1".into(),
            target_path: Some(PathBuf::from("C:/dl/solo.zip")),
        };
        assert_eq!(
            state.push_edges(std::iter::once(edge), now),
            CoalesceAction::FlushNow
        );
    }

    #[test]
    fn multi_edge_same_apply_waits_then_coalesces() {
        let mut state = OsCoalesceState::default();
        let now = Instant::now();
        let edges = vec![
            PendingOsTerminal {
                kind: TerminalKind::Complete,
                filename: "a.zip".into(),
                error: None,
                job_id: "1".into(),
                target_path: Some(PathBuf::from("a")),
            },
            PendingOsTerminal {
                kind: TerminalKind::Complete,
                filename: "b.zip".into(),
                error: None,
                job_id: "2".into(),
                target_path: Some(PathBuf::from("b")),
            },
        ];
        assert_eq!(state.push_edges(edges, now), CoalesceAction::Wait);
        assert_eq!(state.pending.len(), 2);
        assert!(state.coalesce_deadline.is_some());

        let later = now + OS_BURST_WINDOW + Duration::from_millis(1);
        assert_eq!(state.poll_deadline(later), CoalesceAction::FlushNow);
        let taken = state.take_for_flush(later);
        let balloon = compose_os_balloon(&taken, 1).unwrap();
        assert_eq!(balloon.title, "Downloads complete");
        assert_eq!(balloon.body, "2 downloads finished");
        assert_eq!(balloon.context.kind, BalloonOutcome::Coalesced);
    }

    #[test]
    fn burst_window_holds_next_solitary() {
        let mut state = OsCoalesceState::default();
        let t0 = Instant::now();
        let edge = PendingOsTerminal {
            kind: TerminalKind::Complete,
            filename: "a.zip".into(),
            error: None,
            job_id: "1".into(),
            target_path: Some(PathBuf::from("a")),
        };
        assert_eq!(
            state.push_edges(std::iter::once(edge), t0),
            CoalesceAction::FlushNow
        );
        let _ = state.take_for_flush(t0);
        assert!(state.is_burst_open(t0 + Duration::from_millis(100)));

        let edge2 = PendingOsTerminal {
            kind: TerminalKind::Fail,
            filename: "b.zip".into(),
            error: Some("x".into()),
            job_id: "2".into(),
            target_path: None,
        };
        let t1 = t0 + Duration::from_millis(200);
        assert_eq!(
            state.push_edges(std::iter::once(edge2), t1),
            CoalesceAction::Wait
        );
    }

    #[test]
    fn high_water_flushes() {
        let mut state = OsCoalesceState::default();
        let now = Instant::now();
        state.burst_open_until = Some(now + OS_BURST_WINDOW);
        let mut edges = Vec::new();
        for i in 0..OS_HIGH_WATER {
            edges.push(PendingOsTerminal {
                kind: TerminalKind::Complete,
                filename: format!("{i}.bin"),
                error: None,
                job_id: format!("{i}"),
                target_path: Some(PathBuf::from(format!("{i}"))),
            });
        }
        assert_eq!(state.push_edges(edges, now), CoalesceAction::FlushNow);
    }

    #[test]
    fn hard_eligibility() {
        assert!(!os_hard_eligible(OsNotifyMode::Off, true));
        assert!(!os_hard_eligible(OsNotifyMode::WhenHiddenToTray, false));
        assert!(os_hard_eligible(OsNotifyMode::WhenHiddenToTray, true));
        assert!(os_hard_eligible(OsNotifyMode::Always, false));
        assert!(os_hard_eligible(OsNotifyMode::Always, true));
    }

    #[test]
    fn compose_mixed_balloon() {
        let pending = vec![
            PendingOsTerminal {
                kind: TerminalKind::Complete,
                filename: "a".into(),
                error: None,
                job_id: "1".into(),
                target_path: Some(PathBuf::from("a")),
            },
            PendingOsTerminal {
                kind: TerminalKind::Fail,
                filename: "b".into(),
                error: Some("e".into()),
                job_id: "2".into(),
                target_path: None,
            },
        ];
        let b = compose_os_balloon(&pending, 42).unwrap();
        assert_eq!(b.title, "Downloads finished");
        assert_eq!(b.body, "1 finished, 1 failed");
        assert_eq!(b.level, NotifyLevel::Info);
        assert_eq!(b.context.kind, BalloonOutcome::Coalesced);
        assert_eq!(b.context.context_id, 42);
    }

    #[test]
    fn compose_single_complete_open_path() {
        let pending = vec![PendingOsTerminal {
            kind: TerminalKind::Complete,
            filename: "file.zip".into(),
            error: None,
            job_id: "j1".into(),
            target_path: Some(PathBuf::from("C:/dl/file.zip")),
        }];
        let b = compose_os_balloon(&pending, 7).unwrap();
        assert_eq!(b.context.kind, BalloonOutcome::SingleComplete);
        assert_eq!(
            b.context.target_path.as_deref(),
            Some(std::path::Path::new("C:/dl/file.zip"))
        );
        assert_eq!(b.context.job_id.as_deref(), Some("j1"));
    }

    #[test]
    fn balloon_context_map_caps_at_8() {
        let mut map = BalloonContextMap::default();
        for _ in 0..12 {
            let id = map.alloc_id();
            map.push(BalloonClickContext {
                context_id: id,
                kind: BalloonOutcome::Coalesced,
                job_id: None,
                target_path: None,
            });
        }
        assert_eq!(map.contexts.len(), BALLOON_CONTEXT_CAP);
        assert!(map.lookup(11).is_some());
        assert!(map.lookup(0).is_none());
    }

    #[test]
    fn tray_needed_includes_os_notify() {
        assert!(tray_needed(false, false, false, OsNotifyMode::Always));
        assert!(tray_needed(
            false,
            false,
            false,
            OsNotifyMode::WhenHiddenToTray
        ));
        assert!(!tray_needed(false, false, false, OsNotifyMode::Off));
        assert!(tray_needed(true, false, false, OsNotifyMode::Off));
    }

    #[test]
    fn in_app_aggregates_multi() {
        let edges = vec![
            TerminalEdge {
                job_id: "1".into(),
                kind: TerminalKind::Complete,
                filename: "a".into(),
                error: None,
                target_path: PathBuf::from("a"),
            },
            TerminalEdge {
                job_id: "2".into(),
                kind: TerminalKind::Complete,
                filename: "b".into(),
                error: None,
                target_path: PathBuf::from("b"),
            },
            TerminalEdge {
                job_id: "3".into(),
                kind: TerminalKind::Fail,
                filename: "c".into(),
                error: Some("x".into()),
                target_path: PathBuf::from("c"),
            },
        ];
        let toasts = compose_in_app_toasts(OsNotifyMode::WhenHiddenToTray, &edges);
        assert_eq!(toasts.len(), 2);
        assert_eq!(toasts[0].message, "2 downloads finished");
        assert_eq!(toasts[1].message, "Download failed: c — x");
    }

    #[test]
    fn always_mode_skips_success_in_app() {
        let edges = vec![TerminalEdge {
            job_id: "1".into(),
            kind: TerminalKind::Complete,
            filename: "a".into(),
            error: None,
            target_path: PathBuf::from("a"),
        }];
        let toasts = compose_in_app_toasts(OsNotifyMode::Always, &edges);
        assert!(toasts.is_empty());
    }
}
