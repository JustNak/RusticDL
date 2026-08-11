//! Engine command handlers (Add, Pause, Remove, settings, …).

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::super::filesystem::{
    allocate_unique_download_paths, derive_filename_from_url, remove_partial, sanitize_filename,
};
use super::super::handoff::{EnqueueOutcome, EnqueueStatus};
use super::super::http::store_control;
use super::super::job::{Job, JobState, WorkerControl};
use super::super::urls::extract_http_urls;
use super::{emit_jobs_locked, emit_toast, find_job_mut, EngineCommand, EngineInner};

pub(super) async fn handle_command(inner: &Arc<Mutex<EngineInner>>, cmd: EngineCommand) {
    match cmd {
        EngineCommand::Add {
            url,
            filename,
            directory,
            handoff_auth,
            reply,
        } => {
            // Split newlines/spaces and glued pastes (…tokenhttps://other…).
            let mut urls = extract_http_urls(&url);
            if urls.is_empty() {
                let trimmed = url.trim().to_string();
                if trimmed.is_empty() {
                    emit_toast(inner, "URL is empty.".into()).await;
                    return;
                }
                urls.push(trimmed);
            }

            let mut added = 0u32;
            let mut last_error: Option<String> = None;
            // Insert newest-first while preserving paste order (first URL ends up on top).
            let mut new_jobs = Vec::new();
            let mut first_outcome: Option<EnqueueOutcome> = None;

            {
                let mut guard = inner.lock().await;
                let mut occupied_targets: Vec<PathBuf> = guard
                    .jobs
                    .iter()
                    .map(|job| job.target_path.clone())
                    .collect();
                let mut occupied_temps: Vec<PathBuf> =
                    guard.jobs.iter().map(|job| job.temp_path.clone()).collect();
                occupied_temps.extend(guard.pending_partial_deletes.values().cloned());

                for (i, url) in urls.into_iter().enumerate() {
                    if url::Url::parse(&url).is_err() {
                        last_error = Some(format!("Invalid URL: {url}"));
                        continue;
                    }
                    if !(url.starts_with("http://") || url.starts_with("https://")) {
                        last_error = Some("Only HTTP and HTTPS URLs are supported.".into());
                        continue;
                    }

                    let preferred = if i == 0 {
                        filename
                            .as_ref()
                            .map(|f| sanitize_filename(f.trim()))
                            .filter(|f| !f.is_empty())
                            .or_else(|| derive_filename_from_url(&url))
                            .unwrap_or_else(|| "download.bin".into())
                    } else {
                        derive_filename_from_url(&url).unwrap_or_else(|| "download.bin".into())
                    };

                    let (name, target, temp) = allocate_unique_download_paths(
                        &directory,
                        &preferred,
                        &occupied_targets,
                        &occupied_temps,
                    );
                    occupied_targets.push(target.clone());
                    occupied_temps.push(temp.clone());

                    let job = Job::new(url, name.clone(), target, temp);
                    if i == 0 {
                        first_outcome = Some(EnqueueOutcome {
                            job_id: job.id.clone(),
                            filename: name,
                            status: EnqueueStatus::Queued,
                        });
                    }
                    new_jobs.push(job);
                    added += 1;
                }

                if added == 0 {
                    drop(guard);
                    emit_toast(
                        inner,
                        last_error.unwrap_or_else(|| "No valid download URLs found.".into()),
                    )
                    .await;
                    // Leave reply dropped: IPC already validates URLs; UI Add path has no reply.
                    return;
                }

                // Reverse so the first pasted URL is the first job in the list (insert 0 order).
                for job in new_jobs.into_iter().rev() {
                    if let Some(auth) = handoff_auth.as_ref() {
                        // Attach browser session auth only to the primary (first) URL.
                        if first_outcome
                            .as_ref()
                            .is_some_and(|outcome| outcome.job_id == job.id)
                        {
                            guard.handoff_auth.insert(job.id.clone(), auth.clone());
                        }
                    }
                    guard.jobs.insert(0, job);
                }
                emit_jobs_locked(&guard);
                guard.wake.notify_one();
            }

            if let Some(reply) = reply {
                if let Some(outcome) = first_outcome {
                    let _ = reply.send(outcome);
                }
            }

            if added > 1 {
                emit_toast(
                    inner,
                    format!("Added {added} downloads (split multi-URL paste)."),
                )
                .await;
            }
        }
        EngineCommand::Pause(id) => {
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
        EngineCommand::Resume(id) => {
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
        EngineCommand::Cancel(id) => {
            let mut guard = inner.lock().await;
            if let Some(ctrl) = guard.controls.get(&id) {
                store_control(ctrl, WorkerControl::Canceled);
            }
            if let Some(job) = find_job_mut(&mut guard.jobs, &id) {
                if !job.state.is_terminal() || job.state == JobState::Paused {
                    if !matches!(job.state, JobState::Downloading | JobState::Starting) {
                        job.state = JobState::Canceled;
                        job.speed = 0;
                        job.eta_secs = 0;
                    }
                }
            }
            emit_jobs_locked(&guard);
        }
        EngineCommand::Retry(id) => {
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
        EngineCommand::Restart(id) => {
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
            }
            emit_jobs_locked(&guard);
            guard.wake.notify_one();
        }
        EngineCommand::Remove { id, delete_partial } => {
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
        EngineCommand::PauseAll => {
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
        EngineCommand::ResumeAll => {
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
        EngineCommand::RetryAll => {
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
        EngineCommand::UpdateSettings {
            max_concurrent,
            auto_retry,
            speed_limit_kib,
        } => {
            let mut guard = inner.lock().await;
            guard.config.max_concurrent = max_concurrent.max(1);
            guard.config.auto_retry = auto_retry;
            guard.config.speed_limit_kib = speed_limit_kib;
            guard.wake.notify_one();
        }
        EngineCommand::ReplaceJobs(jobs) => {
            let mut guard = inner.lock().await;
            guard.jobs = jobs;
            emit_jobs_locked(&guard);
            guard.wake.notify_one();
        }
        EngineCommand::Shutdown => {}
    }
}
