//! Bulk queue commands (pause all, resume all, retry all).

use std::sync::Arc;

use tokio::sync::Mutex;

use super::super::super::http::store_control;
use super::super::super::job::{JobState, WorkerControl};
use super::super::{emit_jobs_locked, EngineInner};

pub(super) async fn pause_all(inner: &Arc<Mutex<EngineInner>>) {
    let mut guard = inner.lock().await;
    let pause_ids: Vec<String> = guard
        .jobs
        .iter()
        .filter(|job| {
            matches!(
                job.state,
                JobState::Queued | JobState::Starting | JobState::Downloading
            )
        })
        .map(|job| job.id.clone())
        .collect();
    for id in &pause_ids {
        if let Some(ctrl) = guard.controls.get(id) {
            store_control(ctrl, WorkerControl::Paused);
        }
    }
    for job in &mut guard.jobs {
        if pause_ids.iter().any(|id| id == &job.id) && job.state == JobState::Queued {
            job.state = JobState::Paused;
            job.speed = 0;
            job.eta_secs = 0;
        }
    }
    emit_jobs_locked(&guard);
}

pub(super) async fn resume_all(inner: &Arc<Mutex<EngineInner>>) {
    let mut guard = inner.lock().await;
    for job in &mut guard.jobs {
        if matches!(job.state, JobState::Paused) {
            job.state = JobState::Queued;
            job.error = None;
            job.speed = 0;
        }
    }
    emit_jobs_locked(&guard);
    guard.wake.notify_one();
}

pub(super) async fn retry_all(inner: &Arc<Mutex<EngineInner>>) {
    let mut guard = inner.lock().await;
    let mut any = false;
    for job in &mut guard.jobs {
        if matches!(job.state, JobState::Failed | JobState::Canceled) {
            job.state = JobState::Queued;
            job.error = None;
            job.failure_category = None;
            job.retry_attempts = 0;
            job.speed = 0;
            job.eta_secs = 0;
            any = true;
        }
    }
    if any {
        emit_jobs_locked(&guard);
        guard.wake.notify_one();
    }
}
