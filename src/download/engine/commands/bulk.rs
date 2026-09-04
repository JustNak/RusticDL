use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, Mutex};
use tokio::time::{sleep, Instant};

use super::super::super::http::store_control;
use super::super::super::job::{JobState, WorkerControl};
use super::super::{emit_jobs_locked, EngineInner};
use super::job_control::fail_if_resume_map_unusable;

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

pub(super) async fn drain(inner: &Arc<Mutex<EngineInner>>, ack: Option<oneshot::Sender<()>>) {
    pause_all(inner).await;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let (wake, empty) = {
            let guard = inner.lock().await;
            (guard.wake.clone(), guard.active.is_empty())
        };
        if empty {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let slice = Duration::from_millis(50).min(deadline - now);
        tokio::select! {
            _ = wake.notified() => {}
            _ = sleep(slice) => {}
        }
    }
    if let Some(ack) = ack {
        let _ = ack.send(());
    }
}

pub(super) async fn resume_all(inner: &Arc<Mutex<EngineInner>>) {
    let mut guard = inner.lock().await;
    for job in &mut guard.jobs {
        if matches!(job.state, JobState::Paused) {
            if fail_if_resume_map_unusable(job) {
                continue;
            }
            job.state = JobState::Queued;
            job.error = None;
            job.clear_finished();
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
            if fail_if_resume_map_unusable(job) {
                any = true;
                continue;
            }
            job.state = JobState::Queued;
            job.error = None;
            job.failure_category = None;
            job.clear_finished();
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
