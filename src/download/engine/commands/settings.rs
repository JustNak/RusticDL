//! Settings and job-list replacement commands.

use std::sync::Arc;

use tokio::sync::Mutex;

use super::super::super::job::Job;
use super::super::{emit_jobs_locked, EngineInner, EngineRuntimeConfig};

pub(super) async fn update_settings(
    inner: &Arc<Mutex<EngineInner>>,
    mut config: EngineRuntimeConfig,
) {
    config.sanitize();
    let limiter = {
        let mut guard = inner.lock().await;
        let limit = config.speed_limit_bytes_per_second();
        guard.config = config;
        let limiter = guard.limiter.clone();
        guard.wake.notify_one();
        (limiter, limit)
    };
    // Hot-set outside the engine lock; wakes blocked acquires.
    limiter.0.set_limit(limiter.1).await;
}

pub(super) async fn replace_jobs(inner: &Arc<Mutex<EngineInner>>, jobs: Vec<Job>) {
    let mut guard = inner.lock().await;
    guard.jobs = jobs;
    emit_jobs_locked(&guard);
    guard.wake.notify_one();
}
