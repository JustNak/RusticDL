//! Attempt-scoped transfer inputs shared by preflight, planner, single-stream,
//! and multi-segment workers.

use std::sync::atomic::AtomicU8;
use std::sync::Arc;

use super::bandwidth::GlobalBandwidthLimiter;
use super::conn_budget::ConnectionBudget;
use super::engine::EngineRuntimeConfig;
use super::handoff::HandoffAuth;
use super::job::Job;
use super::progress::{IdentityCommit, TransferEventCallback};

/// Attempt-local context. `resolved_url` is pinned after a successful redirect
/// chain so subsequent requests do not re-walk independent redirect races.
#[derive(Clone)]
pub struct TransferContext {
    pub job: Job,
    pub control: Arc<AtomicU8>,
    pub on_progress: TransferEventCallback,
    pub committer: Arc<dyn IdentityCommit>,
    /// Memory-only browser session headers (snapshot from `EngineInner`).
    pub handoff_auth: Option<HandoffAuth>,
    pub limiter: Arc<GlobalBandwidthLimiter>,
    /// Planner input: files smaller than this never qualify for multi.
    pub multi_min_bytes: u64,
    /// Fresh multi partition cap (clamped 1–16 by settings).
    pub multi_max_segments: u32,
    /// Planner kill switch. A live consistent map still forces Multi.
    pub multi_connection_enabled: bool,
    /// Process-wide HTTP body budget (global + per-host).
    pub conn_budget: Arc<ConnectionBudget>,
    /// Attempt-local; updated only when redirect follow succeeds. Init = `job.url`.
    pub resolved_url: String,
    /// Set after a preflight attempt this run so `run_transfer` + single do not double-probe.
    pub preflight_done: bool,
}

impl TransferContext {
    #[allow(clippy::too_many_arguments)]
    pub fn from_runtime(
        job: Job,
        control: Arc<AtomicU8>,
        on_progress: TransferEventCallback,
        handoff_auth: Option<HandoffAuth>,
        limiter: Arc<GlobalBandwidthLimiter>,
        conn_budget: Arc<ConnectionBudget>,
        committer: Arc<dyn IdentityCommit>,
        config: &EngineRuntimeConfig,
    ) -> Self {
        let resolved_url = job.url.clone();
        Self {
            job,
            control,
            on_progress,
            committer,
            handoff_auth,
            limiter,
            multi_min_bytes: config.multi_min_bytes,
            multi_max_segments: config.multi_max_segments,
            multi_connection_enabled: config.multi_connection_enabled,
            conn_budget,
            resolved_url,
            preflight_done: false,
        }
    }
}
