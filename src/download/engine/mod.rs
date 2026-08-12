use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio::time::sleep;

use super::filesystem::remove_partial;
use super::handoff::{EnqueueOutcome, HandoffAuth};
use super::http::{
    run_http_download, store_control, ProgressCallback, ProgressHint, ProgressUpdate,
};
use super::job::{DownloadOutcome, Job, JobState, WorkerControl};

mod commands;

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
    /// Stop a job. When `delete_partial` is true, remove the `.part` file after
    /// the worker exits (or immediately if no worker is running).
    Cancel {
        id: String,
        delete_partial: bool,
    },
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

pub(super) struct EngineConfig {
    max_concurrent: u32,
    auto_retry: u32,
    speed_limit_kib: u32,
}

pub(super) struct EngineInner {
    jobs: Vec<Job>,
    controls: HashMap<String, Arc<AtomicU8>>,
    active: HashMap<String, ()>,
    /// In-memory browser session headers keyed by job id (never written to disk).
    handoff_auth: HashMap<String, HandoffAuth>,
    /// When a worker exits with Canceled, re-queue instead of marking Canceled.
    /// Used by Restart so an in-flight cancel does not stick the job in Canceled.
    requeue_on_cancel: HashMap<String, ()>,
    /// Partial paths to delete after a still-running worker exits (Remove).
    pending_partial_deletes: HashMap<String, PathBuf>,
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
        requeue_on_cancel: HashMap::new(),
        pending_partial_deletes: HashMap::new(),
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
                commands::handle_command(&inner, other).await;
            }
        }
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

        // Serialize progress updates so out-of-order ticks cannot regress speed/bytes.
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ProgressUpdate>();
        let progress_inner = inner.clone();
        let progress_job_id = job_id.clone();
        let progress_pump = tokio::spawn(async move {
            while let Some(update) = progress_rx.recv().await {
                apply_progress(&progress_inner, &progress_job_id, update).await;
            }
        });
        let on_progress: ProgressCallback = Arc::new(move |update: ProgressUpdate| {
            let _ = progress_tx.send(update);
        });

        let mut attempt_job = job_snapshot;
        let mut retry_attempts = attempt_job.retry_attempts;

        let final_result = loop {
            {
                let mut guard = inner.lock().await;
                let restarting = guard.requeue_on_cancel.contains_key(&job_id);
                if !restarting {
                    if let Some(job) = find_job_mut(&mut guard.jobs, &job_id) {
                        job.state = JobState::Downloading;
                        job.error = None;
                        emit_jobs_locked(&guard);
                    }
                }
            }

            // Reset control to continue for each attempt unless user paused/canceled.
            if control.load(Ordering::Relaxed) == 0 {
                store_control(&control, WorkerControl::Continue);
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
                    // Restart requested mid-flight: stop retrying and exit as canceled.
                    {
                        let guard = inner.lock().await;
                        if guard.requeue_on_cancel.contains_key(&job_id) {
                            break Ok(DownloadOutcome::Canceled);
                        }
                    }
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

        // Stop accepting progress before applying the terminal state.
        drop(on_progress);
        let _ = progress_pump.await;

        let partial_to_delete = {
            let mut guard = inner.lock().await;
            guard.active.remove(&job_id);
            let requeue = guard.requeue_on_cancel.remove(&job_id).is_some();
            let partial_to_delete = guard.pending_partial_deletes.remove(&job_id);

            if requeue {
                // Restart already reset the job to Queued; do not overwrite with Canceled.
                if let Some(job) = find_job_mut(&mut guard.jobs, &job_id) {
                    if !matches!(job.state, JobState::Queued) {
                        job.state = JobState::Queued;
                    }
                    job.speed = 0;
                    job.eta_secs = 0;
                }
            } else {
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
            }
            guard.controls.remove(&job_id);
            emit_jobs_locked(&guard);
            guard.wake.notify_one();
            partial_to_delete
        };

        if let Some(path) = partial_to_delete {
            remove_partial(&path).await;
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

pub(super) fn find_job_mut<'a>(jobs: &'a mut [Job], id: &str) -> Option<&'a mut Job> {
    jobs.iter_mut().find(|j| j.id == id)
}

pub(super) fn emit_jobs_locked(guard: &EngineInner) {
    let _ = guard
        .event_tx
        .send(EngineEvent::JobsChanged(guard.jobs.clone()));
}

pub(super) async fn emit_toast(inner: &Arc<Mutex<EngineInner>>, message: String) {
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
