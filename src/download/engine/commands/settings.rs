use std::sync::Arc;

use tokio::sync::Mutex;

use super::super::super::job::Job;
use super::super::{emit_jobs_locked, EngineInner, EngineRuntimeConfig};

pub(super) async fn update_settings(
    inner: &Arc<Mutex<EngineInner>>,
    mut config: EngineRuntimeConfig,
) {
    config.sanitize();
    let (limiter, limit, budget, max_total, max_per_host) = {
        let mut guard = inner.lock().await;
        let limit = config.speed_limit_bytes_per_second();
        let budget = guard.conn_budget.clone();
        let max_total = config.max_total_connections;
        let max_per_host = config.max_connections_per_host;
        guard.config = config;
        let limiter = guard.limiter.clone();
        guard.wake.notify_one();
        (limiter, limit, budget, max_total, max_per_host)
    };
    budget.update_limits(max_total, max_per_host).await;
    limiter.set_limit(limit).await;
}

#[cfg(test)]
pub(super) async fn replace_jobs(inner: &Arc<Mutex<EngineInner>>, jobs: Vec<Job>) {
    let mut guard = inner.lock().await;
    guard.jobs = jobs;
    emit_jobs_locked(&guard);
    guard.wake.notify_one();
}
