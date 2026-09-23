use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, Mutex};
use tokio::time::sleep;

use super::super::super::http::store_control;
use super::super::super::job::{JobState, WorkerControl};
use super::super::persist::persist_live_jobs;
use super::super::{bump_jobs, emit_jobs_locked, EngineInner};
use super::job_control::fail_if_resume_map_unusable;

pub(super) async fn pause_all(inner: &Arc<Mutex<EngineInner>>) {
    let queued_paused = {
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
        let mut queued_paused = false;
        for job in &mut guard.jobs {
            if pause_ids.iter().any(|id| id == &job.id) && job.state == JobState::Queued {
                job.state = JobState::Paused;
                job.speed = 0;
                job.eta_secs = 0;
                queued_paused = true;
            }
        }
        if queued_paused {
            bump_jobs(&mut guard);
        }
        emit_jobs_locked(&guard);
        queued_paused
    };
    if queued_paused {
        let _ = persist_live_jobs(inner).await;
    }
}

pub(super) async fn drain(inner: &Arc<Mutex<EngineInner>>, ack: Option<oneshot::Sender<()>>) {
    pause_all(inner).await;
    // Ack only after workers leave `active` (their pause sync + map commit) and
    // that snapshot has been written. A timer must not succeed while a worker
    // is still inside sync.
    loop {
        let (wake, empty) = {
            let guard = inner.lock().await;
            (guard.wake.clone(), guard.active.is_empty())
        };
        if empty {
            break;
        }
        tokio::select! {
            _ = wake.notified() => {}
            _ = sleep(Duration::from_millis(50)) => {}
        }
    }
    match persist_live_jobs(inner).await {
        Ok(()) => {
            if let Some(ack) = ack {
                let _ = ack.send(());
            }
        }
        Err(error) => {
            super::super::emit_toast(
                inner,
                format!("Could not save the queue ({error}). Quit was not completed."),
            )
            .await;
        }
    }
}

pub(super) async fn resume_all(inner: &Arc<Mutex<EngineInner>>) {
    let mutated = {
        let mut guard = inner.lock().await;
        let mut mutated = false;
        for job in &mut guard.jobs {
            if matches!(job.state, JobState::Paused) {
                if fail_if_resume_map_unusable(job) {
                    mutated = true;
                    continue;
                }
                job.state = JobState::Queued;
                job.error = None;
                job.clear_finished();
                job.speed = 0;
                mutated = true;
            }
        }
        if mutated {
            bump_jobs(&mut guard);
            emit_jobs_locked(&guard);
            guard.wake.notify_one();
        }
        mutated
    };
    if mutated {
        let _ = persist_live_jobs(inner).await;
    }
}

pub(super) async fn retry_all(inner: &Arc<Mutex<EngineInner>>) {
    let any = {
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
            bump_jobs(&mut guard);
            emit_jobs_locked(&guard);
            guard.wake.notify_one();
        }
        any
    };
    if any {
        let _ = persist_live_jobs(inner).await;
    }
}
