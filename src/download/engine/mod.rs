use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU8;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio::time::{sleep, sleep_until, Instant as TokioInstant};

use super::bandwidth::GlobalBandwidthLimiter;
use super::conn_budget::ConnectionBudget;
use super::filesystem::FilenameConflictPolicy;
use super::handoff::{EnqueueOutcome, HandoffAuth};
use super::job::{Job, JobState};
use super::progress::{apply_tick, ProgressTick, TransferEvent};
use crate::settings::Settings;

mod commands;
mod persist;
mod worker;

pub(crate) use persist::EngineIdentity;
pub use persist::{FileJobStore, JobStore, MemoryJobStore};

/// Live engine knobs (from Settings).
#[derive(Debug, Clone)]
pub struct EngineRuntimeConfig {
    pub max_concurrent: u32,
    pub auto_retry: u32,
    pub speed_limit_kib: u32,
    pub multi_max_segments: u32,
    pub multi_min_bytes: u64,
    pub max_total_connections: u32,
    pub max_connections_per_host: u32,
    pub multi_connection_enabled: bool,
}

impl EngineRuntimeConfig {
    pub fn from_settings(s: &Settings) -> Self {
        let mut cfg = Self {
            max_concurrent: s.max_concurrent_downloads,
            auto_retry: s.auto_retry_attempts,
            speed_limit_kib: s.speed_limit_kib_per_second,
            multi_max_segments: s.multi_max_segments,
            multi_min_bytes: s.multi_min_bytes,
            max_total_connections: s.max_total_connections,
            max_connections_per_host: s.max_connections_per_host,
            multi_connection_enabled: s.multi_connection_enabled,
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

/// Progress patches are applied at most this often.
const PROGRESS_COALESCE: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub enum EngineEvent {
    JobsChanged(Arc<Vec<Job>>),
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
        /// Same-name file policy (Ask-mode overwrite vs default uniquify).
        conflict: FilenameConflictPolicy,
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
    /// Drop a job from the queue. `delete_partial` removes leftover `.part`
    /// files; `delete_file` also deletes the completed download on disk.
    Remove {
        id: String,
        delete_partial: bool,
        delete_file: bool,
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

    #[cfg(test)]
    pub(crate) fn stub() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        std::mem::forget(rx);
        Self { tx }
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
    /// Partial paths to delete after a still-running worker exits (Cancel/Restart/Remove).
    pending_partial_deletes: HashMap<String, PathBuf>,
    pub(super) config: EngineRuntimeConfig,
    pub(super) limiter: Arc<GlobalBandwidthLimiter>,
    pub(super) conn_budget: Arc<ConnectionBudget>,
    event_tx: mpsc::UnboundedSender<EngineEvent>,
    wake: Arc<Notify>,
    store: Arc<dyn JobStore>,
    persist_tx: mpsc::Sender<persist::PersistReq>,
}

pub fn spawn_engine(
    initial_jobs: Vec<Job>,
    config: EngineRuntimeConfig,
    store: Arc<dyn JobStore>,
) -> (EngineHandle, mpsc::UnboundedReceiver<EngineEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (persist_tx, persist_rx) = mpsc::channel(64);
    let wake = Arc::new(Notify::new());

    let mut config = config;
    config.sanitize();
    let limiter = GlobalBandwidthLimiter::new(config.speed_limit_bytes_per_second());
    let conn_budget = ConnectionBudget::new(
        config.max_total_connections,
        config.max_connections_per_host,
    );

    let mut jobs = initial_jobs;
    for job in &mut jobs {
        // Recover in-flight states after restart.
        if matches!(job.state, JobState::Starting | JobState::Downloading) {
            job.state = JobState::Queued;
            clear_live_metrics(job);
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
        conn_budget,
        event_tx,
        wake: wake.clone(),
        store,
        persist_tx,
    }));

    tokio::spawn(persist::persist_actor(inner.clone(), persist_rx));
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
            worker::start_worker(inner.clone(), id);
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

/// Coalesce ticks then apply at most every `PROGRESS_COALESCE`. Toasts flush now.
fn spawn_progress_pump(
    inner: Arc<Mutex<EngineInner>>,
    job_id: String,
    mut progress_rx: mpsc::UnboundedReceiver<TransferEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut pending: Option<ProgressTick> = None;
        let mut flush_at: Option<TokioInstant> = None;

        loop {
            match flush_at {
                None => match progress_rx.recv().await {
                    Some(TransferEvent::Tick(tick)) => {
                        coalesce_push(&mut pending, tick);
                        flush_at = Some(TokioInstant::now() + PROGRESS_COALESCE);
                    }
                    Some(TransferEvent::Toast(message)) => {
                        emit_toast(&inner, message).await;
                    }
                    None => {
                        if let Some(tick) = pending.take() {
                            apply_tick_event(&inner, &job_id, tick).await;
                        }
                        break;
                    }
                },
                Some(deadline) => {
                    tokio::select! {
                        item = progress_rx.recv() => {
                            match item {
                                Some(TransferEvent::Tick(tick)) => {
                                    coalesce_push(&mut pending, tick);
                                }
                                Some(TransferEvent::Toast(message)) => {
                                    emit_toast(&inner, message).await;
                                }
                                None => {
                                    if let Some(tick) = pending.take() {
                                        apply_tick_event(&inner, &job_id, tick).await;
                                    }
                                    break;
                                }
                            }
                        }
                        _ = sleep_until(deadline) => {
                            if let Some(tick) = pending.take() {
                                apply_tick_event(&inner, &job_id, tick).await;
                            }
                            flush_at = None;
                        }
                    }
                }
            }
        }
    })
}

/// Merge `tick` into the coalesce buffer (later wins on Some).
fn coalesce_push(pending: &mut Option<ProgressTick>, tick: ProgressTick) {
    *pending = Some(match pending.take() {
        Some(prev) => prev.merge(tick),
        None => tick,
    });
}

async fn apply_tick_event(inner: &Arc<Mutex<EngineInner>>, id: &str, tick: ProgressTick) {
    let mut guard = inner.lock().await;
    if let Some(job) = find_job_mut(&mut guard.jobs, id) {
        if apply_tick(job, tick) {
            emit_jobs_locked(&guard);
        }
    }
}

/// Zero live transfer metrics when a worker leaves the job (every finalizer path).
fn clear_live_metrics(job: &mut Job) {
    job.speed = 0;
    job.eta_secs = 0;
    job.active_connections = 0;
}

/// Failed multi: retain map + version for resume reuse. Do not call `on_completed`.
fn apply_failed_lifecycle(job: &mut Job, error: super::job::DownloadError) {
    job.state = JobState::Failed;
    job.error = Some(error.message);
    job.failure_category = Some(error.category);
    job.mark_finished();
    clear_live_metrics(job);
}

pub(super) fn find_job_mut<'a>(jobs: &'a mut [Job], id: &str) -> Option<&'a mut Job> {
    jobs.iter_mut().find(|j| j.id == id)
}

pub(super) fn emit_jobs_locked(guard: &EngineInner) {
    let _ = guard
        .event_tx
        .send(EngineEvent::JobsChanged(Arc::new(guard.jobs.clone())));
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
    use super::super::job::{download_error, ContentValidators, FailureCategory};
    use super::super::progress::{CommitIdentity, IdentityCommit, MapUpdate, ProgressHint};
    use super::super::segment::{Segment, SegmentMap, SegmentState};
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
        let (persist_tx, persist_rx) = mpsc::channel(32);
        let inner = Arc::new(Mutex::new(EngineInner {
            jobs: vec![job],
            controls: HashMap::new(),
            active: HashMap::new(),
            handoff_auth: HashMap::new(),
            requeue_on_cancel: HashMap::new(),
            pending_partial_deletes: HashMap::new(),
            config: EngineRuntimeConfig::default(),
            limiter: GlobalBandwidthLimiter::new(None),
            conn_budget: ConnectionBudget::new(32, 8),
            event_tx,
            wake: Arc::new(Notify::new()),
            store: Arc::new(MemoryJobStore::default()),
            persist_tx,
        }));
        tokio::spawn(persist::persist_actor(inner.clone(), persist_rx));
        inner
    }

    #[test]
    fn state_hint_none_does_not_clobber_state() {
        let mut job = sample_job(JobState::Downloading);
        let ok = apply_tick(
            &mut job,
            ProgressTick {
                downloaded_bytes: Some(40),
                total_bytes: None,
                speed: Some(8),
                eta_secs: Some(7),
                progress: Some(40.0),
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
        apply_tick(
            &mut job,
            ProgressTick {
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
        apply_tick(&mut job, ProgressTick::downloading(1, 100, 1, 99, 1.0));
        assert_eq!(job.state, JobState::Downloading);
    }

    #[test]
    fn apply_tick_skips_terminal_jobs() {
        let mut job = sample_job(JobState::Completed);
        let before = job.downloaded_bytes;
        let ok = apply_tick(&mut job, ProgressTick::downloading(99, 100, 1, 0, 99.0));
        assert!(!ok);
        assert_eq!(job.downloaded_bytes, before);
        assert_eq!(job.state, JobState::Completed);
    }

    /// Restart zeros job to Queued; deferred coalesce must not resurrect progress.
    #[test]
    fn apply_tick_skips_queued_jobs() {
        let mut job = sample_job(JobState::Queued);
        job.downloaded_bytes = 0;
        job.total_bytes = 0;
        job.progress = 0.0;
        let ok = apply_tick(&mut job, ProgressTick::downloading(50, 100, 10, 5, 50.0));
        assert!(!ok);
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.downloaded_bytes, 0);
        assert_eq!(job.total_bytes, 0);
        assert_eq!(job.progress, 0.0);
    }

    #[test]
    fn apply_tick_skips_paused_jobs() {
        let mut job = sample_job(JobState::Paused);
        let before = job.downloaded_bytes;
        let ok = apply_tick(&mut job, ProgressTick::downloading(99, 100, 1, 0, 99.0));
        assert!(!ok);
        assert_eq!(job.downloaded_bytes, before);
        assert_eq!(job.state, JobState::Paused);
    }

    #[test]
    fn option_none_scalars_leave_job_unchanged() {
        let mut job = sample_job(JobState::Downloading);
        apply_tick(
            &mut job,
            ProgressTick {
                active_connections: Some(1),
                ..Default::default()
            },
        );
        assert_eq!(job.active_connections, 1);
        assert_eq!(job.downloaded_bytes, 10);
        assert_eq!(job.total_bytes, 100);
        assert_eq!(job.speed, 1);
        assert_eq!(job.eta_secs, 90);
        assert_eq!(job.progress, 10.0);
        assert_eq!(job.state, JobState::Downloading);
    }

    #[test]
    fn apply_tick_does_not_clear_validators_or_map() {
        let mut job = sample_job(JobState::Downloading);
        let validators = ContentValidators {
            etag: Some("\"etag-1\"".into()),
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            expected_size: Some(100),
        };
        let map = SegmentMap {
            total_bytes: 1000,
            segment_count: 1,
            segments: vec![Segment {
                index: 0,
                start: 0,
                end: 999,
                written: 10,
                state: SegmentState::Active,
            }],
            preallocated: true,
        };
        job.validators = validators.clone();
        job.segment_map = Some(map.clone());
        apply_tick(&mut job, ProgressTick::downloading(20, 100, 5, 16, 20.0));
        assert_eq!(job.validators, validators);
        assert_eq!(job.segment_map, Some(map));
        assert_eq!(job.downloaded_bytes, 20);
    }

    #[test]
    fn apply_tick_sets_live_metrics() {
        let mut job = sample_job(JobState::Downloading);
        apply_tick(
            &mut job,
            ProgressTick {
                active_connections: Some(4),
                reconnect_count: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(job.active_connections, 4);
        assert_eq!(job.reconnect_count, 2);
        assert_eq!(job.transfer_format_version, 0);
        assert!(job.transfer_mode.is_none());
    }

    #[test]
    fn apply_failed_lifecycle_retains_map() {
        let mut job = sample_job(JobState::Downloading);
        let map = SegmentMap {
            total_bytes: 1000,
            segment_count: 1,
            segments: vec![Segment {
                index: 0,
                start: 0,
                end: 999,
                written: 40,
                state: SegmentState::Active,
            }],
            preallocated: true,
        };
        job.transfer_format_version = 1;
        job.segment_map = Some(map.clone());
        job.reconnect_count = 4;
        job.validators.etag = Some("\"keep\"".into());

        apply_failed_lifecycle(
            &mut job,
            download_error(FailureCategory::Network, "boom".into(), true),
        );

        assert_eq!(job.state, JobState::Failed);
        assert_eq!(job.segment_map, Some(map));
        assert_eq!(job.transfer_format_version, 1);
        assert_eq!(job.reconnect_count, 4);
        assert_eq!(job.validators.etag.as_deref(), Some("\"keep\""));
        assert_eq!(job.active_connections, 0);
        assert_eq!(job.error.as_deref(), Some("boom"));
        assert_eq!(job.failure_category, Some(FailureCategory::Network));
    }

    #[test]
    fn worker_finalizer_zeros_active_connections() {
        let mut job = sample_job(JobState::Downloading);
        job.active_connections = 4;
        job.speed = 1200;
        job.eta_secs = 30;
        clear_live_metrics(&mut job);
        assert_eq!(job.active_connections, 0);
        assert_eq!(job.speed, 0);
        assert_eq!(job.eta_secs, 0);
    }

    /// Multiple ticks merge into one pending; take drains once.
    #[test]
    fn coalesce_push_merges_then_take_flushes_once() {
        let mut pending: Option<ProgressTick> = None;
        coalesce_push(
            &mut pending,
            ProgressTick {
                downloaded_bytes: Some(10),
                total_bytes: Some(100),
                state_hint: Some(ProgressHint::Starting),
                ..Default::default()
            },
        );
        coalesce_push(
            &mut pending,
            ProgressTick::downloading(50, 100, 5, 10, 50.0),
        );
        coalesce_push(
            &mut pending,
            ProgressTick {
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
        tx.send(TransferEvent::Tick(ProgressTick::downloading(
            10, 100, 1, 90, 10.0,
        )))
        .unwrap();
        tx.send(TransferEvent::Tick(ProgressTick::downloading(
            40, 100, 4, 15, 40.0,
        )))
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
        tx.send(TransferEvent::Tick(ProgressTick::downloading(
            80, 100, 10, 2, 80.0,
        )))
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

    #[tokio::test]
    async fn persist_tick_without_written_does_not_roll_map_backward_after_commit() {
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let mut job = sample_job(JobState::Downloading);
        job.id = "persist-stale-written".into();
        job.segment_map = Some(SegmentMap {
            total_bytes: 100,
            segment_count: 1,
            segments: vec![Segment {
                index: 0,
                start: 0,
                end: 99,
                written: 0,
                state: SegmentState::Active,
            }],
            preallocated: true,
        });
        let job_id = job.id.clone();
        let inner = test_inner(job, event_tx);

        let committer = EngineIdentity {
            inner: inner.clone(),
        };
        let mut worker_job = {
            let guard = inner.lock().await;
            guard.jobs.iter().find(|j| j.id == job_id).unwrap().clone()
        };
        let mut committed = worker_job.segment_map.clone().unwrap();
        committed.segments[0].written = 100;
        committer
            .commit(
                &mut worker_job,
                CommitIdentity {
                    downloaded_bytes: Some(100),
                    map: MapUpdate::Set(committed),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let (tx, rx) = mpsc::unbounded_channel();
        let pump = spawn_progress_pump(inner.clone(), job_id.clone(), rx);
        tx.send(TransferEvent::Tick(ProgressTick {
            segment_written: Some(vec![80]),
            ..Default::default()
        }))
        .unwrap();
        tx.send(TransferEvent::Tick(ProgressTick {
            downloaded_bytes: Some(100),
            segment_written: None,
            ..Default::default()
        }))
        .unwrap();
        drop(tx);
        pump.await.expect("pump join");

        let guard = inner.lock().await;
        let job = guard.jobs.iter().find(|j| j.id == job_id).unwrap();
        assert_eq!(job.downloaded_bytes, 100);
        assert_eq!(job.segment_map.as_ref().unwrap().segments[0].written, 100);
    }

    #[tokio::test]
    async fn engine_identity_skips_set_after_restart_requeue() {
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let mut job = sample_job(JobState::Queued);
        job.id = "restart-set".into();
        job.downloaded_bytes = 0;
        job.total_bytes = 0;
        job.progress = 0.0;
        job.clear_transfer_identity();
        let job_id = job.id.clone();
        let inner = test_inner(job.clone(), event_tx);
        inner
            .lock()
            .await
            .requeue_on_cancel
            .insert(job_id.clone(), ());

        let committer = EngineIdentity {
            inner: inner.clone(),
        };
        committer
            .commit(
                &mut job,
                CommitIdentity {
                    downloaded_bytes: Some(50),
                    transfer_format_version: Some(1),
                    map: MapUpdate::Set(SegmentMap {
                        total_bytes: 1000,
                        segment_count: 1,
                        segments: vec![Segment {
                            index: 0,
                            start: 0,
                            end: 999,
                            written: 50,
                            state: SegmentState::Active,
                        }],
                        preallocated: true,
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let guard = inner.lock().await;
        let canonical = guard.jobs.iter().find(|j| j.id == job_id).unwrap();
        assert!(canonical.segment_map.is_none());
        assert_eq!(canonical.transfer_format_version, 0);
        assert_eq!(canonical.downloaded_bytes, 0);
    }

    #[tokio::test]
    async fn engine_identity_skips_set_after_cancel_delete_partial() {
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let mut job = sample_job(JobState::Downloading);
        job.id = "cancel-set".into();
        job.clear_transfer_identity();
        let job_id = job.id.clone();
        let temp_path = job.temp_path.clone();
        let inner = test_inner(job.clone(), event_tx);
        inner
            .lock()
            .await
            .pending_partial_deletes
            .insert(job_id.clone(), temp_path);

        let committer = EngineIdentity {
            inner: inner.clone(),
        };
        committer
            .commit(
                &mut job,
                CommitIdentity {
                    downloaded_bytes: Some(50),
                    transfer_format_version: Some(1),
                    map: MapUpdate::Set(SegmentMap {
                        total_bytes: 1000,
                        segment_count: 1,
                        segments: vec![Segment {
                            index: 0,
                            start: 0,
                            end: 999,
                            written: 50,
                            state: SegmentState::Active,
                        }],
                        preallocated: true,
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let guard = inner.lock().await;
        let canonical = guard.jobs.iter().find(|j| j.id == job_id).unwrap();
        assert!(canonical.segment_map.is_none());
        assert_eq!(canonical.transfer_format_version, 0);
    }

    #[tokio::test]
    async fn engine_identity_patches_canonical_job_without_state_change() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut job = sample_job(JobState::Queued);
        job.id = "commit-test".into();
        let inner = test_inner(job.clone(), event_tx);
        let committer = EngineIdentity {
            inner: inner.clone(),
        };
        committer
            .commit(
                &mut job,
                CommitIdentity {
                    transfer_format_version: Some(1),
                    map: super::super::progress::MapUpdate::Set(SegmentMap {
                        total_bytes: 1000,
                        segment_count: 1,
                        segments: vec![Segment {
                            index: 0,
                            start: 0,
                            end: 999,
                            written: 0,
                            state: SegmentState::Pending,
                        }],
                        preallocated: false,
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.transfer_format_version, 1);
        assert!(job.segment_map.is_some());

        let guard = inner.lock().await;
        let canonical = guard.jobs.iter().find(|j| j.id == "commit-test").unwrap();
        assert_eq!(canonical.state, JobState::Queued);
        assert_eq!(canonical.transfer_format_version, 1);
        assert!(canonical.segment_map.is_some());
        drop(guard);

        assert!(matches!(
            event_rx.try_recv(),
            Ok(EngineEvent::JobsChanged(_))
        ));
    }
}
