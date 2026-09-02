use std::sync::Arc;

use tokio::sync::Mutex;

use super::super::super::filesystem::remove_partial;
use super::super::super::http::store_control;
use super::super::super::job::{FailureCategory, Job, JobState, WorkerControl};
use super::super::super::multi::RESUME_RESTART_MESSAGE;
use super::super::super::resume::{resume_oracle, FALLBACK_MAP_INCONSISTENT, FALLBACK_MAP_MISSING};
use super::super::persist::persist_live_jobs;
use super::super::{emit_jobs_locked, find_job_mut, EngineInner};

/// v1 map missing/inconsistent → fail Resume immediately (do not invent ranges).
pub(super) fn fail_if_resume_map_unusable(job: &mut Job) -> bool {
    if !resume_oracle(job).is_resume_error() {
        return false;
    }
    job.state = JobState::Failed;
    job.error = Some(RESUME_RESTART_MESSAGE.into());
    job.failure_category = Some(FailureCategory::Resume);
    job.mark_finished();
    let reason = if job.segment_map.is_none() {
        FALLBACK_MAP_MISSING
    } else {
        FALLBACK_MAP_INCONSISTENT
    };
    job.fallback_reason = Some(reason.to_string());
    job.speed = 0;
    job.eta_secs = 0;
    job.active_connections = 0;
    true
}

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
            if fail_if_resume_map_unusable(job) {
                emit_jobs_locked(&guard);
                return;
            }
            job.state = JobState::Queued;
            job.error = None;
            job.failure_category = None;
            job.clear_finished();
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

        let immediate = if job.state.is_terminal() {
            if delete_partial {
                job.clear_partial_and_identity();
            }
            if delete_partial && !worker_running {
                temp_path
            } else {
                None
            }
        } else {
            if !matches!(job.state, JobState::Downloading | JobState::Starting) {
                job.state = JobState::Canceled;
                job.mark_finished();
                job.speed = 0;
                job.eta_secs = 0;
            }
            job.active_connections = 0;
            if delete_partial {
                job.clear_partial_and_identity();
            }

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

    if delete_partial {
        let _ = persist_live_jobs(inner).await;
    }

    if let Some(path) = immediate_partial {
        remove_partial(&path).await;
    }
}

pub(super) async fn retry(inner: &Arc<Mutex<EngineInner>>, id: String) {
    let mut guard = inner.lock().await;
    if let Some(job) = find_job_mut(&mut guard.jobs, &id) {
        if matches!(job.state, JobState::Failed | JobState::Canceled) {
            if fail_if_resume_map_unusable(job) {
                emit_jobs_locked(&guard);
                return;
            }
            job.state = JobState::Queued;
            job.error = None;
            job.failure_category = None;
            job.clear_finished();
            job.retry_attempts = 0;
            job.speed = 0;
            job.eta_secs = 0;
        }
    }
    emit_jobs_locked(&guard);
    guard.wake.notify_one();
}

pub(super) async fn restart(inner: &Arc<Mutex<EngineInner>>, id: String) {
    let (immediate_partial, worker_running) = {
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

        if let Some(job) = find_job_mut(&mut guard.jobs, &id) {
            job.state = JobState::Queued;
            job.progress = 0.0;
            job.downloaded_bytes = 0;
            job.total_bytes = 0;
            job.speed = 0;
            job.eta_secs = 0;
            job.error = None;
            job.failure_category = None;
            job.clear_finished();
            job.retry_attempts = 0;
            job.clear_transfer_identity();
            job.resume_supported = false;
        }

        let immediate = if worker_running {
            guard.requeue_on_cancel.insert(id.clone(), ());
            if let Some(path) = temp_path {
                guard.pending_partial_deletes.insert(id.clone(), path);
            }
            None
        } else {
            if let Some(path) = temp_path.clone() {
                guard.pending_partial_deletes.insert(id.clone(), path);
            }
            temp_path
        };

        emit_jobs_locked(&guard);
        if worker_running {
            guard.wake.notify_one();
        }
        (immediate, worker_running)
    };
    let _ = persist_live_jobs(inner).await;
    if let Some(path) = immediate_partial {
        remove_partial(&path).await;
    }
    if !worker_running {
        let mut guard = inner.lock().await;
        guard.pending_partial_deletes.remove(&id);
        guard.wake.notify_one();
    }
}

pub(super) async fn remove(
    inner: &Arc<Mutex<EngineInner>>,
    id: String,
    delete_partial: bool,
    delete_file: bool,
) {
    let (temp_path, target_path, worker_still_running) = {
        let mut guard = inner.lock().await;
        if let Some(ctrl) = guard.controls.get(&id) {
            store_control(ctrl, WorkerControl::Canceled);
        }
        let paths = guard
            .jobs
            .iter()
            .find(|j| j.id == id)
            .map(|j| (j.temp_path.clone(), j.target_path.clone()));
        let worker_still_running = guard.active.contains_key(&id);
        guard.jobs.retain(|j| j.id != id);
        guard.handoff_auth.remove(&id);
        guard.requeue_on_cancel.remove(&id);
        if !worker_still_running {
            guard.controls.remove(&id);
            guard.pending_partial_deletes.remove(&id);
            guard.pending_final_deletes.remove(&id);
        } else if delete_partial {
            if let Some((path, _)) = paths.clone() {
                guard.pending_partial_deletes.insert(id.clone(), path);
            }
        }
        if worker_still_running && delete_file {
            guard.pending_final_deletes.insert(id.clone(), ());
        }
        emit_jobs_locked(&guard);
        match paths {
            Some((temp, target)) => (Some(temp), Some(target), worker_still_running),
            None => (None, None, worker_still_running),
        }
    };
    if !worker_still_running {
        if delete_partial {
            if let Some(path) = temp_path {
                remove_partial(&path).await;
            }
        }
        if delete_file {
            if let Some(path) = target_path {
                if path.is_file() {
                    remove_partial(&path).await;
                }
            }
        }
    }
}
