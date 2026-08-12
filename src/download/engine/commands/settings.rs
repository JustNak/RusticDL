//! Settings and job-list replacement commands.

use std::sync::Arc;

use tokio::sync::Mutex;

use super::super::super::job::Job;
use super::super::{emit_jobs_locked, EngineInner};

pub(super) async fn update_settings(
    inner: &Arc<Mutex<EngineInner>>,
    max_concurrent: u32,
    auto_retry: u32,
    speed_limit_kib: u32,
) {
    let mut guard = inner.lock().await;
    guard.config.max_concurrent = max_concurrent.max(1);
    guard.config.auto_retry = auto_retry;
    guard.config.speed_limit_kib = speed_limit_kib;
    guard.wake.notify_one();
}

pub(super) async fn replace_jobs(inner: &Arc<Mutex<EngineInner>>, jobs: Vec<Job>) {
    let mut guard = inner.lock().await;
    guard.jobs = jobs;
    emit_jobs_locked(&guard);
    guard.wake.notify_one();
}
