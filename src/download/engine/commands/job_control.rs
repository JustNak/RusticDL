//! Per-job control commands (pause, resume, cancel, retry, restart, remove).

use std::sync::Arc;

use tokio::sync::Mutex;

use super::super::super::filesystem::remove_partial;
use super::super::super::http::store_control;
use super::super::super::job::{JobState, WorkerControl};
use super::super::{emit_jobs_locked, find_job_mut, EngineInner};

pub(super) async fn pause(inner: &Arc<Mutex<EngineInner>>, id: String) {
    let mut guard = inner.lock().await;
    if let Some(ctrl) = guard.controls.get(&id) {
        store_control(ctrl, WorkerControl::Paused);
    }
    if let Some(job) = find_job_mut(&mut guard.jobs, &id) {
        if matches!(
            job.state,
            JobState::Queued | JobState::Starting | JobState::Downloading
        ) {
            if job.state == JobState::Queued {
                job.state = JobState::Paused;
                job.speed = 0;
                job.eta_secs = 0;
            }
        }
    }
    emit_jobs_locked(&guard);
}

pub(super) async fn resume(inner: &Arc<Mutex<EngineInner>>, id: String) {
    let mut guard = inner.lock().await;
    if let Some(job) = find_job_mut(&mut guard.jobs, &id) {
        if matches!(job.state, JobState::Paused | JobState::Canceled) {
            job.state = JobState::Queued;
            job.error = None;
            job.failure_category = None;
            job.speed = 0;
            if let Some(ctrl) = guard.controls.get(&id) {
                store_control(ctrl, WorkerControl::Continue);
            }
        }
    }
    emit_jobs_locked(&guard);
    guard.wake.notify_one();
}

pub(super) async fn cancel(inner: &Arc<Mutex<EngineInner>>, id: String, delete_partial: bool) {
    let immediate_partial = {
        let mut guard = inner.lock().await;
        if let Some(ctrl) = guard.controls.get(&id) {
            store_control(ctrl, WorkerControl::Canceled);
        }
        let worker_running = guard.active.contains_key(&id);
        let temp_path = guard
            .jobs
            .iter()
            .find(|j| j.id == id)
            .map(|j| j.temp_path.clone());

        let Some(job) = find_job_mut(&mut guard.jobs, &id) else {
            emit_jobs_locked(&guard);
            return;
        };

        // Already terminal: optional leftover .part cleanup only.
        let immediate = if job.state.is_terminal() {
            if delete_partial && !worker_running {
                temp_path
            } else {
                None
            }
        } else {
            // Queued / Paused: mark Canceled now. In-flight Starting/Downloading:
            // worker finalizer sets Canceled after control flag is observed.
            if !matches!(job.state, JobState::Downloading | JobState::Starting) {
                job.state = JobState::Canceled;
                job.speed = 0;
                job.eta_secs = 0;
            }

            // Always drop handoff auth when canceling a non-running job.
            // In-flight workers clear it in their finalizer.
            if !worker_running {
                guard.handoff_auth.remove(&id);
            }

            if delete_partial {
                if worker_running {
                    if let Some(path) = temp_path {
                        guard.pending_partial_deletes.insert(id.clone(), path);
                    }
                    None
                } else {
                    temp_path
                }
            } else {
                None
            }
        };

        emit_jobs_locked(&guard);
        immediate
    };

    if let Some(path) = immediate_partial {
        remove_partial(&path).await;
    }
}

pub(super) async fn retry(inner: &Arc<Mutex<EngineInner>>, id: String) {
    let mut guard = inner.lock().await;
    if let Some(job) = find_job_mut(&mut guard.jobs, &id) {
        if matches!(job.state, JobState::Failed | JobState::Canceled) {
            job.state = JobState::Queued;
            job.error = None;
            job.failure_category = None;
            job.retry_attempts = 0;
            job.speed = 0;
            job.eta_secs = 0;
        }
    }
    emit_jobs_locked(&guard);
    guard.wake.notify_one();
}

pub(super) async fn restart(inner: &Arc<Mutex<EngineInner>>, id: String) {
    let temp_path = {
        let guard = inner.lock().await;
        guard
            .jobs
            .iter()
            .find(|j| j.id == id)
            .map(|j| j.temp_path.clone())
    };
    if let Some(path) = temp_path {
        remove_partial(&path).await;
    }
    let mut guard = inner.lock().await;
    if let Some(ctrl) = guard.controls.get(&id) {
        store_control(ctrl, WorkerControl::Canceled);
    }
    // If a worker is still active, the finalizer must not stick the job in Canceled.
    if guard.active.contains_key(&id) {
        guard.requeue_on_cancel.insert(id.clone(), ());
    }
    if let Some(job) = find_job_mut(&mut guard.jobs, &id) {
        job.state = JobState::Queued;
        job.progress = 0.0;
        job.downloaded_bytes = 0;
        job.total_bytes = 0;
        job.speed = 0;
        job.eta_secs = 0;
        job.error = None;
        job.failure_category = None;
        job.retry_attempts = 0;
        // Lifecycle: Restart clears validators + transfer format + metrics.
        job.clear_transfer_identity();
        job.resume_supported = false;
    }
    emit_jobs_locked(&guard);
    guard.wake.notify_one();
}

pub(super) async fn remove(inner: &Arc<Mutex<EngineInner>>, id: String, delete_partial: bool) {
    let (temp_path, worker_still_running) = {
        let mut guard = inner.lock().await;
        if let Some(ctrl) = guard.controls.get(&id) {
            store_control(ctrl, WorkerControl::Canceled);
        }
        let path = guard
            .jobs
            .iter()
            .find(|j| j.id == id)
            .map(|j| j.temp_path.clone());
        let worker_still_running = guard.active.contains_key(&id);
        guard.jobs.retain(|j| j.id != id);
        guard.handoff_auth.remove(&id);
        guard.requeue_on_cancel.remove(&id);
        // Keep the active slot until the worker exits so concurrency stays accurate
        // and the worker is not racing a deleted .part path.
        if !worker_still_running {
            guard.controls.remove(&id);
            guard.pending_partial_deletes.remove(&id);
        } else if delete_partial {
            if let Some(path) = path.clone() {
                guard.pending_partial_deletes.insert(id.clone(), path);
            }
        }
        emit_jobs_locked(&guard);
        (path, worker_still_running)
    };
    if delete_partial && !worker_still_running {
        if let Some(path) = temp_path {
            remove_partial(&path).await;
        }
    }
}
