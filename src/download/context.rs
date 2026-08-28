use std::sync::atomic::AtomicU8;
use std::sync::Arc;

use super::bandwidth::GlobalBandwidthLimiter;
use super::conn_budget::ConnectionBudget;
use super::engine::EngineRuntimeConfig;
use super::handoff::HandoffAuth;
use super::job::Job;
use super::progress::{IdentityCommit, TransferEventCallback};

#[derive(Clone)]
pub struct TransferContext {
    pub job: Job,
    pub control: Arc<AtomicU8>,
    pub on_progress: TransferEventCallback,
    pub committer: Arc<dyn IdentityCommit>,
    pub handoff_auth: Option<HandoffAuth>,
    pub limiter: Arc<GlobalBandwidthLimiter>,
    pub multi_min_bytes: u64,
    pub multi_max_segments: u32,
    pub multi_connection_enabled: bool,
    pub conn_budget: Arc<ConnectionBudget>,
    pub resolved_url: String,
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
