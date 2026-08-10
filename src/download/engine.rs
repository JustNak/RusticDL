use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio::time::sleep;

use super::filesystem::{
    derive_filename_from_url, remove_partial, sanitize_filename, temp_path_for,
};
use super::handoff::{EnqueueOutcome, EnqueueStatus, HandoffAuth};
use super::http::{
    run_http_download, store_control, ProgressCallback, ProgressHint, ProgressUpdate,
};
use super::job::{DownloadError, DownloadOutcome, FailureCategory, Job, JobState, WorkerControl};
use super::urls::extract_http_urls;

/// Backoff schedule for auto-retry (indexed by attempt number - 1).
/// Longer delays help with flaky TLS / filter / CDN blips that browsers also hit.
const RETRY_DELAYS: [Duration; 8] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(45),
];

#[derive(Debug, Clone)]
pub enum EngineEvent {
    JobsChanged(Vec<Job>),
    Toast(String),
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum EngineCommand {
    Add {
        url: String,
        filename: Option<String>,
        directory: PathBuf,
        /// Browser session headers (memory-only; never persisted).
        handoff_auth: Option<HandoffAuth>,
        reply: Option<oneshot::Sender<EnqueueOutcome>>,
    },
    Pause(String),
    Resume(String),
    Cancel(String),
    Retry(String),
    Restart(String),
    Remove {
        id: String,
        delete_partial: bool,
    },
    PauseAll,
    ResumeAll,
    RetryAll,
    UpdateSettings {
        max_concurrent: u32,
        auto_retry: u32,
        speed_limit_kib: u32,
    },
    ReplaceJobs(Vec<Job>),
    Shutdown,
}

#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::UnboundedSender<EngineCommand>,
}

impl EngineHandle {
    pub fn send(&self, cmd: EngineCommand) {
        let _ = self.tx.send(cmd);
    }
}

struct EngineConfig {
    max_concurrent: u32,
    auto_retry: u32,
    speed_limit_kib: u32,
}

struct EngineInner {
    jobs: Vec<Job>,
    controls: HashMap<String, Arc<AtomicU8>>,
    active: HashMap<String, ()>,
    /// In-memory browser session headers keyed by job id (never written to disk).
    handoff_auth: HashMap<String, HandoffAuth>,
    config: EngineConfig,
    event_tx: mpsc::UnboundedSender<EngineEvent>,
    wake: Arc<Notify>,
}

pub fn spawn_engine(
    initial_jobs: Vec<Job>,
    max_concurrent: u32,
    auto_retry: u32,
    speed_limit_kib: u32,
) -> (EngineHandle, mpsc::UnboundedReceiver<EngineEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let wake = Arc::new(Notify::new());

    let mut jobs = initial_jobs;
    for job in &mut jobs {
        // Recover in-flight states after restart.
        if matches!(job.state, JobState::Starting | JobState::Downloading) {
            job.state = JobState::Queued;
            job.speed = 0;
            job.eta_secs = 0;
        }
    }

    let inner = Arc::new(Mutex::new(EngineInner {
        jobs,
        controls: HashMap::new(),
        active: HashMap::new(),
        handoff_auth: HashMap::new(),
        config: EngineConfig {
            max_concurrent: max_concurrent.max(1),
            auto_retry,
            speed_limit_kib,
        },
        event_tx,
        wake: wake.clone(),
    }));

    tokio::spawn(command_loop(inner.clone(), cmd_rx));
    tokio::spawn(scheduler_loop(inner));

    (EngineHandle { tx: cmd_tx }, event_rx)
}

async fn command_loop(
    inner: Arc<Mutex<EngineInner>>,
    mut cmd_rx: mpsc::UnboundedReceiver<EngineCommand>,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            EngineCommand::Shutdown => break,
            other => {
                handle_command(&inner, other).await;
            }
        }
    }
}

async fn handle_command(inner: &Arc<Mutex<EngineInner>>, cmd: EngineCommand) {
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

            for (i, url) in urls.into_iter().enumerate() {
                if url::Url::parse(&url).is_err() {
                    last_error = Some(format!("Invalid URL: {url}"));
                    continue;
                }
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    last_error = Some("Only HTTP and HTTPS URLs are supported.".into());
                    continue;
                }

                let name = if i == 0 {
                    filename
                        .as_ref()
                        .map(|f| sanitize_filename(f.trim()))
                        .filter(|f| !f.is_empty())
                        .or_else(|| derive_filename_from_url(&url))
                        .unwrap_or_else(|| "download.bin".into())
                } else {
                    derive_filename_from_url(&url).unwrap_or_else(|| "download.bin".into())
                };

                let target = directory.join(&name);
                let temp = temp_path_for(&target);
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
                emit_toast(
                    inner,
                    last_error.unwrap_or_else(|| "No valid download URLs found.".into()),
                )
                .await;
                return;
            }

            {
                let mut guard = inner.lock().await;
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
            let temp_path = {
                let mut guard = inner.lock().await;
                if let Some(ctrl) = guard.controls.get(&id) {
                    store_control(ctrl, WorkerControl::Canceled);
                }
                let path = guard
                    .jobs
                    .iter()
                    .find(|j| j.id == id)
                    .map(|j| j.temp_path.clone());
                guard.jobs.retain(|j| j.id != id);
                guard.controls.remove(&id);
                guard.active.remove(&id);
                guard.handoff_auth.remove(&id);
                emit_jobs_locked(&guard);
                path
            };
            if delete_partial {
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

async fn scheduler_loop(inner: Arc<Mutex<EngineInner>>) {
    loop {
        let to_start = {
            let mut guard = inner.lock().await;
            let max = guard.config.max_concurrent as usize;
            let active = guard.active.len();
            if active >= max {
                Vec::new()
            } else {
                let slots = max - active;
                let active_ids: std::collections::HashSet<String> =
                    guard.active.keys().cloned().collect();
                let mut ids = Vec::new();
                for job in &mut guard.jobs {
                    if ids.len() >= slots {
                        break;
                    }
                    if job.state == JobState::Queued && !active_ids.contains(&job.id) {
                        job.state = JobState::Starting;
                        ids.push(job.id.clone());
                    }
                }
                if !ids.is_empty() {
                    emit_jobs_locked(&guard);
                }
                ids
            }
        };

        for id in to_start {
            start_worker(inner.clone(), id);
        }

        let wake = {
            let guard = inner.lock().await;
            guard.wake.clone()
        };
        tokio::select! {
            _ = wake.notified() => {}
            _ = sleep(Duration::from_millis(500)) => {}
        }
    }
}

fn start_worker(inner: Arc<Mutex<EngineInner>>, job_id: String) {
    tokio::spawn(async move {
        let (job_snapshot, control, speed_limit, max_retry, handoff_auth) = {
            let mut guard = inner.lock().await;
            let control = Arc::new(AtomicU8::new(0));
            guard.controls.insert(job_id.clone(), control.clone());
            guard.active.insert(job_id.clone(), ());
            let job = match guard.jobs.iter().find(|j| j.id == job_id) {
                Some(j) => j.clone(),
                None => {
                    guard.active.remove(&job_id);
                    return;
                }
            };
            let speed = if guard.config.speed_limit_kib == 0 {
                None
            } else {
                Some(guard.config.speed_limit_kib as u64 * 1024)
            };
            let max_retry = guard.config.auto_retry;
            let auth = guard.handoff_auth.get(&job_id).cloned();
            (job, control, speed, max_retry, auth)
        };

        let inner_progress = inner.clone();
        let progress_id = job_id.clone();
        let on_progress: ProgressCallback = Arc::new(move |update: ProgressUpdate| {
            let inner = inner_progress.clone();
            let id = progress_id.clone();
            // Blocking lock is not available; spawn a quick update task.
            tokio::spawn(async move {
                apply_progress(&inner, &id, update).await;
            });
        });

        let mut attempt_job = job_snapshot;
        let mut retry_attempts = attempt_job.retry_attempts;

        let final_result = loop {
            {
                let mut guard = inner.lock().await;
                if let Some(job) = find_job_mut(&mut guard.jobs, &job_id) {
                    job.state = JobState::Downloading;
                    job.error = None;
                    emit_jobs_locked(&guard);
                }
            }

            // Reset control to continue for each attempt unless user paused/canceled.
            if control.load(Ordering::Relaxed) == 0 {
                store_control(&control, WorkerControl::Continue);
            } else if control.load(Ordering::Relaxed) != 0 {
                // User already set pause/cancel.
            }

            match run_http_download(
                &attempt_job,
                speed_limit,
                control.clone(),
                on_progress.clone(),
                handoff_auth.as_ref(),
            )
            .await
            {
                Ok(outcome) => break Ok(outcome),
                Err(error) => {
                    let can_retry = error.retryable && retry_attempts < max_retry;
                    if can_retry {
                        retry_attempts += 1;
                        let delay_idx = (retry_attempts as usize - 1).min(RETRY_DELAYS.len() - 1);
                        let delay = RETRY_DELAYS[delay_idx];
                        {
                            let mut guard = inner.lock().await;
                            if let Some(job) = find_job_mut(&mut guard.jobs, &job_id) {
                                job.retry_attempts = retry_attempts;
                                job.state = JobState::Starting;
                                job.error = Some(format!(
                                    "Retry {retry_attempts}/{max_retry} in {}s: {}",
                                    delay.as_secs().max(1),
                                    error.message
                                ));
                                emit_jobs_locked(&guard);
                            }
                        }
                        sleep(delay).await;
                        if control.load(Ordering::Relaxed) != 0 {
                            break Err(error);
                        }
                        // Refresh paths/filename from latest job state.
                        {
                            let guard = inner.lock().await;
                            if let Some(job) = guard.jobs.iter().find(|j| j.id == job_id) {
                                attempt_job = job.clone();
                            }
                        }
                        continue;
                    }
                    break Err(error);
                }
            }
        };

        {
            let mut guard = inner.lock().await;
            guard.active.remove(&job_id);
            match final_result {
                Ok(DownloadOutcome::Completed) => {
                    if let Some(job) = find_job_mut(&mut guard.jobs, &job_id) {
                        job.state = JobState::Completed;
                        job.progress = 100.0;
                        job.speed = 0;
                        job.eta_secs = 0;
                        job.error = None;
                    }
                    guard.handoff_auth.remove(&job_id);
                }
                Ok(DownloadOutcome::Paused) => {
                    if let Some(job) = find_job_mut(&mut guard.jobs, &job_id) {
                        job.state = JobState::Paused;
                        job.speed = 0;
                        job.eta_secs = 0;
                    }
                }
                Ok(DownloadOutcome::Canceled) => {
                    if let Some(job) = find_job_mut(&mut guard.jobs, &job_id) {
                        job.state = JobState::Canceled;
                        job.speed = 0;
                        job.eta_secs = 0;
                    }
                    guard.handoff_auth.remove(&job_id);
                }
                Err(error) => {
                    let clear_auth = match control.load(Ordering::Relaxed) {
                        1 => false,
                        _ => true,
                    };
                    if let Some(job) = find_job_mut(&mut guard.jobs, &job_id) {
                        // If user paused/canceled during retry wait, prefer that.
                        match control.load(Ordering::Relaxed) {
                            1 => {
                                job.state = JobState::Paused;
                                job.speed = 0;
                            }
                            2 => {
                                job.state = JobState::Canceled;
                                job.speed = 0;
                            }
                            _ => {
                                job.state = JobState::Failed;
                                job.error = Some(error.message);
                                job.failure_category = Some(error.category);
                                job.speed = 0;
                                job.eta_secs = 0;
                            }
                        }
                    }
                    if clear_auth {
                        guard.handoff_auth.remove(&job_id);
                    }
                }
            }
            guard.controls.remove(&job_id);
            emit_jobs_locked(&guard);
            guard.wake.notify_one();
        }
    });
}

async fn apply_progress(inner: &Arc<Mutex<EngineInner>>, id: &str, update: ProgressUpdate) {
    let mut guard = inner.lock().await;
    let Some(job) = find_job_mut(&mut guard.jobs, id) else {
        return;
    };

    if matches!(
        job.state,
        JobState::Completed | JobState::Canceled | JobState::Failed
    ) {
        return;
    }

    match update.state_hint {
        ProgressHint::Starting => {
            if job.state != JobState::Paused {
                job.state = JobState::Starting;
            }
        }
        ProgressHint::Downloading => {
            if !matches!(job.state, JobState::Paused | JobState::Canceled) {
                job.state = JobState::Downloading;
            }
        }
    }

    job.downloaded_bytes = update.downloaded_bytes;
    job.total_bytes = update.total_bytes;
    job.speed = update.speed;
    job.eta_secs = update.eta_secs;
    job.progress = update.progress;

    if let Some(name) = update.filename {
        job.filename = name;
    }
    if let Some(path) = update.target_path {
        job.target_path = path;
    }
    if let Some(path) = update.temp_path {
        job.temp_path = path;
    }
    if let Some(resume) = update.resume_supported {
        job.resume_supported = resume;
    }

    emit_jobs_locked(&guard);
}

fn find_job_mut<'a>(jobs: &'a mut [Job], id: &str) -> Option<&'a mut Job> {
    jobs.iter_mut().find(|j| j.id == id)
}

fn emit_jobs_locked(guard: &EngineInner) {
    let _ = guard
        .event_tx
        .send(EngineEvent::JobsChanged(guard.jobs.clone()));
}

async fn emit_toast(inner: &Arc<Mutex<EngineInner>>, message: String) {
    let guard = inner.lock().await;
    let _ = guard.event_tx.send(EngineEvent::Toast(message));
}

pub fn open_path(path: &Path) -> Result<(), String> {
    open::that(path).map_err(|e| format!("Could not open path: {e}"))
}

pub fn reveal_in_folder(path: &Path) -> Result<(), String> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            return open_path(parent);
        }
        return Err("Path does not exist.".into());
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("Could not reveal file: {e}"))?;
        return Ok(());
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(parent) = path.parent() {
            open_path(parent)
        } else {
            open_path(path)
        }
    }
}

// Silence unused import warning for FailureCategory in some builds
#[allow(dead_code)]
fn _unused(_: FailureCategory, _: DownloadError) {}
