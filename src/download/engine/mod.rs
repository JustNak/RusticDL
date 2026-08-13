use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio::time::{sleep, sleep_until, Instant as TokioInstant};

use super::bandwidth::GlobalBandwidthLimiter;
use super::filesystem::{apply_partial_progress_from_disk, metadata_len, remove_partial};
use super::handoff::{EnqueueOutcome, HandoffAuth};
use super::http::{
    run_http_download, store_control, ProgressCallback, ProgressHint, ProgressUpdate,
};
use super::job::{DownloadOutcome, Job, JobState, WorkerControl};
use crate::settings::Settings;

mod commands;

/// Live engine knobs (from Settings). Multi fields are stored early for later PRs.
#[derive(Debug, Clone)]
#[allow(dead_code)] // multi_* reserved until segment orchestrator lands
pub struct EngineRuntimeConfig {
    pub max_concurrent: u32,
    pub auto_retry: u32,
    pub speed_limit_kib: u32,
    pub fsync_on_pause: bool,
    pub multi_connection_enabled: bool,
    pub multi_max_segments: u32,
    pub multi_min_bytes: u64,
    pub max_total_connections: u32,
    pub max_connections_per_host: u32,
}

impl EngineRuntimeConfig {
    pub fn from_settings(s: &Settings) -> Self {
        let mut cfg = Self {
            max_concurrent: s.max_concurrent_downloads,
            auto_retry: s.auto_retry_attempts,
            speed_limit_kib: s.speed_limit_kib_per_second,
            fsync_on_pause: s.fsync_on_pause,
            multi_connection_enabled: s.multi_connection_enabled,
            multi_max_segments: s.multi_max_segments,
            multi_min_bytes: s.multi_min_bytes,
            max_total_connections: s.max_total_connections,
            max_connections_per_host: s.max_connections_per_host,
        };
        cfg.sanitize();
        cfg
    }

    pub fn sanitize(&mut self) {
        self.max_concurrent = self.max_concurrent.clamp(1, 64);
        self.auto_retry = self.auto_retry.min(100);
        // 0 = unlimited; no upper clamp needed for practical UI values.
        self.multi_max_segments = self.multi_max_segments.clamp(1, 16);
        self.multi_min_bytes = self.multi_min_bytes.clamp(1024 * 1024, 1024 * 1024 * 1024);
        self.max_total_connections = self.max_total_connections.clamp(1, 256);
        self.max_connections_per_host = self.max_connections_per_host.clamp(1, 64);
        // Per-host cannot exceed process-wide total (multi orchestrator will rely on this).
        self.max_connections_per_host = self
            .max_connections_per_host
            .min(self.max_total_connections);
    }

    pub fn speed_limit_bytes_per_second(&self) -> Option<u64> {
        if self.speed_limit_kib == 0 {
            None
        } else {
            Some(self.speed_limit_kib as u64 * 1024)
        }
    }
}

impl Default for EngineRuntimeConfig {
    fn default() -> Self {
        Self::from_settings(&Settings::default())
    }
}

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

/// Progress patches are applied at most this often.
const PROGRESS_COALESCE: Duration = Duration::from_millis(200);

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
    UpdateSettings(EngineRuntimeConfig),
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
    pub(super) config: EngineRuntimeConfig,
    pub(super) limiter: Arc<GlobalBandwidthLimiter>,
    event_tx: mpsc::UnboundedSender<EngineEvent>,
    wake: Arc<Notify>,
}

pub fn spawn_engine(
    initial_jobs: Vec<Job>,
    config: EngineRuntimeConfig,
) -> (EngineHandle, mpsc::UnboundedReceiver<EngineEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let wake = Arc::new(Notify::new());

    let mut config = config;
    config.sanitize();
    let limiter = GlobalBandwidthLimiter::new(config.speed_limit_bytes_per_second());

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
        config,
        limiter,
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
        let (job_snapshot, control, limiter, handoff_auth) = {
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
            let limiter = guard.limiter.clone();
            let auth = guard.handoff_auth.get(&job_id).cloned();
            (job, control, limiter, auth)
        };

        let mut attempt_job = job_snapshot;
        let mut retry_attempts = attempt_job.retry_attempts;

        // Per-attempt progress pump: drain (flush pending) after each attempt so
        // restart/retry state writes cannot race a deferred coalesce window.
        let final_result = loop {
            // Disk is authoritative for single-stream (v0) resume. Snapshot path
            // under the lock, then await metadata without holding it. v1+ is
            // map-authoritative — skip metadata_len so a sparse `.part` cannot lie.
            let (temp_path, fsync_on_pause, skip_disk) = {
                let guard = inner.lock().await;
                let job = guard.jobs.iter().find(|j| j.id == job_id);
                (
                    job.map(|j| j.temp_path.clone()),
                    guard.config.fsync_on_pause,
                    job.map(|j| j.transfer_format_version >= 1).unwrap_or(false),
                )
            };
            let on_disk = if skip_disk {
                None
            } else {
                Some(match temp_path.as_ref() {
                    Some(path) => metadata_len(path).await.unwrap_or(0),
                    None => 0,
                })
            };
            {
                let mut guard = inner.lock().await;
                let restarting = guard.requeue_on_cancel.contains_key(&job_id);
                if !restarting {
                    if let Some(job) = find_job_mut(&mut guard.jobs, &job_id) {
                        if let Some(on_disk) = on_disk {
                            apply_partial_progress_from_disk(job, on_disk);
                        }
                        job.state = JobState::Downloading;
                        job.error = None;
                        attempt_job = job.clone();
                        emit_jobs_locked(&guard);
                    }
                }
            }

            // Reset control to continue for each attempt unless user paused/canceled.
            if control.load(Ordering::Relaxed) == 0 {
                store_control(&control, WorkerControl::Continue);
            }

            let (progress_tx, progress_rx) = mpsc::unbounded_channel::<ProgressUpdate>();
            let progress_pump = spawn_progress_pump(inner.clone(), job_id.clone(), progress_rx);
            let on_progress: ProgressCallback = Arc::new(move |update: ProgressUpdate| {
                let _ = progress_tx.send(update);
            });

            let attempt_result = run_http_download(
                &attempt_job,
                limiter.clone(),
                control.clone(),
                on_progress.clone(),
                handoff_auth.as_ref(),
                fsync_on_pause,
            )
            .await;

            // Flush remaining patches before any post-attempt state mutation.
            drop(on_progress);
            let _ = progress_pump.await;

            match attempt_result {
                Ok(outcome) => break Ok(outcome),
                Err(error) => {
                    // Restart requested mid-flight: stop retrying and exit as canceled.
                    {
                        let guard = inner.lock().await;
                        if guard.requeue_on_cancel.contains_key(&job_id) {
                            break Ok(DownloadOutcome::Canceled);
                        }
                    }
                    // Re-read live auto_retry so UpdateSettings applies to the next failure.
                    let max_retry = {
                        let guard = inner.lock().await;
                        guard.config.auto_retry
                    };
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

/// Coalesce progress patches (merge Option fields) then apply at most every
/// `PROGRESS_COALESCE`. Immediate flush when the channel closes.
fn spawn_progress_pump(
    inner: Arc<Mutex<EngineInner>>,
    job_id: String,
    mut progress_rx: mpsc::UnboundedReceiver<ProgressUpdate>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut pending: Option<ProgressUpdate> = None;
        let mut flush_at: Option<TokioInstant> = None;

        loop {
            match flush_at {
                None => match progress_rx.recv().await {
                    Some(update) => {
                        coalesce_push(&mut pending, update);
                        flush_at = Some(TokioInstant::now() + PROGRESS_COALESCE);
                    }
                    None => {
                        if let Some(update) = pending.take() {
                            apply_progress(&inner, &job_id, update).await;
                        }
                        break;
                    }
                },
                Some(deadline) => {
                    tokio::select! {
                        item = progress_rx.recv() => {
                            match item {
                                Some(update) => {
                                    // Deadline already set ⇒ pending is Some.
                                    coalesce_push(&mut pending, update);
                                    // Keep existing deadline so the first patch opens the window.
                                }
                                None => {
                                    if let Some(update) = pending.take() {
                                        apply_progress(&inner, &job_id, update).await;
                                    }
                                    break;
                                }
                            }
                        }
                        _ = sleep_until(deadline) => {
                            if let Some(update) = pending.take() {
                                apply_progress(&inner, &job_id, update).await;
                            }
                            flush_at = None;
                        }
                    }
                }
            }
        }
    })
}

/// Merge `update` into the coalesce buffer (later wins on Some).
fn coalesce_push(pending: &mut Option<ProgressUpdate>, update: ProgressUpdate) {
    *pending = Some(match pending.take() {
        Some(prev) => prev.merge(update),
        None => update,
    });
}

async fn apply_progress(inner: &Arc<Mutex<EngineInner>>, id: &str, update: ProgressUpdate) {
    let mut guard = inner.lock().await;
    let Some(job) = find_job_mut(&mut guard.jobs, id) else {
        return;
    };

    if !apply_progress_patch(job, update) {
        return;
    }

    emit_jobs_locked(&guard);
}

/// Apply a partial progress patch. `None` fields leave the job value unchanged.
/// Returns false if the job is not in an in-flight transfer state (no mutation).
///
/// Only `Starting` / `Downloading` accept progress: rejects `Queued` (restart),
/// `Paused`, and terminal states so deferred coalesce cannot resurrect progress
/// after external lifecycle writes.
fn apply_progress_patch(job: &mut Job, update: ProgressUpdate) -> bool {
    if !matches!(job.state, JobState::Starting | JobState::Downloading) {
        return false;
    }

    // state_hint: None ⇒ do not change job.state.
    if let Some(hint) = update.state_hint {
        match hint {
            ProgressHint::Starting => {
                job.state = JobState::Starting;
            }
            ProgressHint::Downloading => {
                job.state = JobState::Downloading;
            }
        }
    }

    if let Some(v) = update.downloaded_bytes {
        job.downloaded_bytes = v;
    }
    if let Some(v) = update.total_bytes {
        job.total_bytes = v;
    }
    if let Some(v) = update.speed {
        job.speed = v;
    }
    if let Some(v) = update.eta_secs {
        job.eta_secs = v;
    }
    if let Some(v) = update.progress {
        job.progress = v;
    }
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
    // Field-wise merge: sparse captures must not wipe stored ETag/LM (CDN 206 quirk).
    if let Some(validators) = update.validators {
        job.validators.merge_present(validators);
    }
    if let Some(version) = update.transfer_format_version {
        job.transfer_format_version = version;
    }
    if let Some(n) = update.active_connections {
        job.active_connections = n;
    }
    if let Some(n) = update.reconnect_count {
        job.reconnect_count = n;
    }
    if let Some(mode) = update.transfer_mode {
        job.transfer_mode = Some(mode);
    }
    if let Some(reason) = update.fallback_reason {
        job.fallback_reason = Some(reason);
    }

    true
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

#[cfg(test)]
mod tests {
    use super::super::job::{ContentValidators, TransferMode};
    use super::*;
    use std::path::PathBuf;

    fn sample_job(state: JobState) -> Job {
        let target = PathBuf::from("C:\\downloads\\file.bin");
        let temp = PathBuf::from("C:\\downloads\\file.bin.part");
        let mut job = Job::new(
            "https://example.com/file.bin".into(),
            "file.bin".into(),
            target,
            temp,
        );
        job.state = state;
        job.downloaded_bytes = 10;
        job.total_bytes = 100;
        job.speed = 1;
        job.eta_secs = 90;
        job.progress = 10.0;
        job
    }

    fn test_inner(
        job: Job,
        event_tx: mpsc::UnboundedSender<EngineEvent>,
    ) -> Arc<Mutex<EngineInner>> {
        Arc::new(Mutex::new(EngineInner {
            jobs: vec![job],
            controls: HashMap::new(),
            active: HashMap::new(),
            handoff_auth: HashMap::new(),
            requeue_on_cancel: HashMap::new(),
            pending_partial_deletes: HashMap::new(),
            config: EngineRuntimeConfig::default(),
            limiter: GlobalBandwidthLimiter::new(None),
            event_tx,
            wake: Arc::new(Notify::new()),
        }))
    }

    #[test]
    fn state_hint_none_does_not_clobber_state() {
        let mut job = sample_job(JobState::Downloading);
        let ok = apply_progress_patch(
            &mut job,
            ProgressUpdate {
                downloaded_bytes: Some(40),
                total_bytes: None,
                speed: Some(8),
                eta_secs: Some(7),
                progress: Some(40.0),
                filename: None,
                target_path: None,
                temp_path: None,
                resume_supported: None,
                state_hint: None,
                ..Default::default()
            },
        );
        assert!(ok);
        assert_eq!(job.state, JobState::Downloading);
        assert_eq!(job.downloaded_bytes, 40);
        assert_eq!(job.total_bytes, 100); // unchanged
        assert_eq!(job.speed, 8);
        assert_eq!(job.eta_secs, 7);
        assert_eq!(job.progress, 40.0);
    }

    #[test]
    fn state_hint_none_preserves_starting() {
        let mut job = sample_job(JobState::Starting);
        apply_progress_patch(
            &mut job,
            ProgressUpdate {
                downloaded_bytes: Some(0),
                speed: Some(0),
                state_hint: None,
                ..Default::default()
            },
        );
        assert_eq!(job.state, JobState::Starting);
    }

    #[test]
    fn state_hint_some_transitions_to_downloading() {
        let mut job = sample_job(JobState::Starting);
        apply_progress_patch(
            &mut job,
            ProgressUpdate::downloading_tick(1, 100, 1, 99, 1.0),
        );
        assert_eq!(job.state, JobState::Downloading);
    }

    #[test]
    fn apply_progress_skips_terminal_jobs() {
        let mut job = sample_job(JobState::Completed);
        let before = job.downloaded_bytes;
        let ok = apply_progress_patch(
            &mut job,
            ProgressUpdate::downloading_tick(99, 100, 1, 0, 99.0),
        );
        assert!(!ok);
        assert_eq!(job.downloaded_bytes, before);
        assert_eq!(job.state, JobState::Completed);
    }

    /// Restart zeros job to Queued; deferred coalesce must not resurrect progress.
    #[test]
    fn apply_progress_skips_queued_jobs() {
        let mut job = sample_job(JobState::Queued);
        job.downloaded_bytes = 0;
        job.total_bytes = 0;
        job.progress = 0.0;
        let ok = apply_progress_patch(
            &mut job,
            ProgressUpdate::downloading_tick(50, 100, 10, 5, 50.0),
        );
        assert!(!ok);
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.downloaded_bytes, 0);
        assert_eq!(job.total_bytes, 0);
        assert_eq!(job.progress, 0.0);
    }

    #[test]
    fn apply_progress_skips_paused_jobs() {
        let mut job = sample_job(JobState::Paused);
        let before = job.downloaded_bytes;
        let ok = apply_progress_patch(
            &mut job,
            ProgressUpdate::downloading_tick(99, 100, 1, 0, 99.0),
        );
        assert!(!ok);
        assert_eq!(job.downloaded_bytes, before);
        assert_eq!(job.state, JobState::Paused);
    }

    #[test]
    fn option_none_scalars_leave_job_unchanged() {
        let mut job = sample_job(JobState::Downloading);
        apply_progress_patch(
            &mut job,
            ProgressUpdate {
                filename: Some("renamed.bin".into()),
                ..Default::default()
            },
        );
        assert_eq!(job.filename, "renamed.bin");
        assert_eq!(job.downloaded_bytes, 10);
        assert_eq!(job.total_bytes, 100);
        assert_eq!(job.speed, 1);
        assert_eq!(job.eta_secs, 90);
        assert_eq!(job.progress, 10.0);
        assert_eq!(job.state, JobState::Downloading);
    }

    #[test]
    fn apply_progress_sets_validators_and_preserves_on_none() {
        let mut job = sample_job(JobState::Starting);
        let validators = ContentValidators {
            etag: Some("\"etag-1\"".into()),
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            expected_size: Some(100),
        };
        apply_progress_patch(
            &mut job,
            ProgressUpdate {
                validators: Some(validators.clone()),
                state_hint: Some(ProgressHint::Starting),
                ..Default::default()
            },
        );
        assert_eq!(job.validators, validators);

        // Speed tick with validators: None must not clear stored validators.
        apply_progress_patch(
            &mut job,
            ProgressUpdate::downloading_tick(20, 100, 5, 16, 20.0),
        );
        assert_eq!(job.validators, validators);
        assert_eq!(job.downloaded_bytes, 20);
    }

    /// Empty / sparse validator patches must not wipe persisted ETag/LM.
    #[test]
    fn apply_progress_empty_or_sparse_validators_preserve_identity() {
        let mut job = sample_job(JobState::Downloading);
        job.validators = ContentValidators {
            etag: Some("\"keep\"".into()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".into()),
            expected_size: Some(100),
        };

        // None = unchanged
        apply_progress_patch(
            &mut job,
            ProgressUpdate {
                validators: None,
                ..Default::default()
            },
        );
        assert_eq!(job.validators.etag.as_deref(), Some("\"keep\""));

        // Empty Some (bug if wholesale replace) — field-wise merge is no-op
        apply_progress_patch(
            &mut job,
            ProgressUpdate {
                validators: Some(ContentValidators::default()),
                ..Default::default()
            },
        );
        assert_eq!(job.validators.etag.as_deref(), Some("\"keep\""));
        assert_eq!(
            job.validators.last_modified.as_deref(),
            Some("Wed, 21 Oct 2015 07:28:00 GMT")
        );

        // Size-only capture updates expected_size, keeps ETag/LM
        apply_progress_patch(
            &mut job,
            ProgressUpdate {
                validators: Some(ContentValidators {
                    etag: None,
                    last_modified: None,
                    expected_size: Some(999),
                }),
                ..Default::default()
            },
        );
        assert_eq!(job.validators.etag.as_deref(), Some("\"keep\""));
        assert_eq!(
            job.validators.last_modified.as_deref(),
            Some("Wed, 21 Oct 2015 07:28:00 GMT")
        );
        assert_eq!(job.validators.expected_size, Some(999));
    }

    #[test]
    fn apply_progress_sets_metrics_placeholders() {
        let mut job = sample_job(JobState::Downloading);
        apply_progress_patch(
            &mut job,
            ProgressUpdate {
                transfer_format_version: Some(1),
                active_connections: Some(4),
                reconnect_count: Some(2),
                transfer_mode: Some(TransferMode::Multi),
                fallback_reason: Some("planner".into()),
                ..Default::default()
            },
        );
        assert_eq!(job.transfer_format_version, 1);
        assert_eq!(job.active_connections, 4);
        assert_eq!(job.reconnect_count, 2);
        assert_eq!(job.transfer_mode, Some(TransferMode::Multi));
        assert_eq!(job.fallback_reason.as_deref(), Some("planner"));
    }

    /// Multiple patches merge into one pending; take drains once.
    #[test]
    fn coalesce_push_merges_then_take_flushes_once() {
        let mut pending: Option<ProgressUpdate> = None;
        coalesce_push(
            &mut pending,
            ProgressUpdate {
                downloaded_bytes: Some(10),
                total_bytes: Some(100),
                filename: Some("a.bin".into()),
                state_hint: Some(ProgressHint::Starting),
                ..Default::default()
            },
        );
        coalesce_push(
            &mut pending,
            ProgressUpdate::downloading_tick(50, 100, 5, 10, 50.0),
        );
        coalesce_push(
            &mut pending,
            ProgressUpdate {
                speed: Some(9),
                ..Default::default()
            },
        );

        let flushed = pending.take().expect("pending after pushes");
        assert!(pending.is_none());
        assert_eq!(flushed.downloaded_bytes, Some(50));
        assert_eq!(flushed.total_bytes, Some(100));
        assert_eq!(flushed.speed, Some(9)); // latest wins
        assert_eq!(flushed.eta_secs, Some(10));
        assert_eq!(flushed.progress, Some(50.0));
        assert_eq!(flushed.filename.as_deref(), Some("a.bin")); // preserved
        assert_eq!(flushed.state_hint, Some(ProgressHint::Downloading));
    }

    /// Pump applies pending when the channel closes, then stops (terminal flush).
    #[tokio::test]
    async fn progress_pump_flushes_pending_on_channel_close() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut job = sample_job(JobState::Downloading);
        job.id = "pump-test".into();
        let job_id = job.id.clone();

        let inner = test_inner(job, event_tx);

        let (tx, rx) = mpsc::unbounded_channel();
        let pump = spawn_progress_pump(inner.clone(), job_id.clone(), rx);

        // Buffer two patches then close: merge + flush-on-close (no wait for deadline).
        tx.send(ProgressUpdate::downloading_tick(10, 100, 1, 90, 10.0))
            .unwrap();
        tx.send(ProgressUpdate::downloading_tick(40, 100, 4, 15, 40.0))
            .unwrap();
        drop(tx);

        pump.await.expect("pump join");

        let guard = inner.lock().await;
        let job = guard.jobs.iter().find(|j| j.id == job_id).unwrap();
        // Final values from merged pending (later tick wins scalars).
        assert_eq!(job.downloaded_bytes, 40);
        assert_eq!(job.speed, 4);
        assert_eq!(job.progress, 40.0);
        assert_eq!(job.state, JobState::Downloading);
        drop(guard);

        // At least one JobsChanged from the flush path.
        let mut emits = 0;
        while event_rx.try_recv().is_ok() {
            emits += 1;
        }
        assert!(emits >= 1, "expected flush emit(s), got {emits}");
    }

    /// Deferred patch after restart zeroed the job must not clobber Queued.
    #[tokio::test]
    async fn progress_pump_does_not_apply_when_job_queued() {
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let mut job = sample_job(JobState::Queued);
        job.id = "queued-test".into();
        job.downloaded_bytes = 0;
        job.total_bytes = 0;
        job.progress = 0.0;
        job.speed = 0;
        let job_id = job.id.clone();

        let inner = test_inner(job, event_tx);

        let (tx, rx) = mpsc::unbounded_channel();
        let pump = spawn_progress_pump(inner.clone(), job_id.clone(), rx);
        tx.send(ProgressUpdate::downloading_tick(80, 100, 10, 2, 80.0))
            .unwrap();
        drop(tx);
        pump.await.expect("pump join");

        let guard = inner.lock().await;
        let job = guard.jobs.iter().find(|j| j.id == job_id).unwrap();
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.downloaded_bytes, 0);
        assert_eq!(job.progress, 0.0);
        assert_eq!(job.speed, 0);
    }
}
