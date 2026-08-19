use std::path::PathBuf;

use crate::tray::NotifyLevel;

/// Burst window after any OS flush; edges arriving inside it are held and merged.
pub const OS_BURST_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);
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

impl PendingOsTerminal {
    pub fn from_edge(edge: &TerminalEdge) -> Self {
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

/// Composed balloon ready for tray show + context allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalloonPayload {
    pub title: String,
    pub body: String,
    pub level: NotifyLevel,
    pub kind: BalloonOutcome,
    pub job_id: Option<String>,
    pub target_path: Option<PathBuf>,
}

/// In-app toast severity for Pipeline A.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InAppToastKind {
    Info,
    Error,
}
