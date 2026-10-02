use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use super::super::bandwidth::GlobalBandwidthLimiter;
use super::super::context::TransferContext;
use super::super::fetch::{sleep_interruptible, store_control};
use super::super::filesystem::{metadata_len, reconcile_from_oracle, remove_partial};
use super::super::handoff::HandoffAuth;
use super::super::job::{DownloadError, DownloadOutcome, Job, JobState, WorkerControl};
use super::super::progress::{TransferEvent, TransferEventCallback};
use super::super::resume::{resume_oracle, ResumeOracle};
use super::super::transfer::run_transfer;
use super::persist::persist_live_jobs;
use super::{
    apply_failed_lifecycle, bump_jobs, clear_live_metrics, emit_jobs_locked, find_job_mut,
    spawn_progress_pump, EngineIdentity, EngineInner,
};

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

pub(super) fn start_worker(inner: Arc<Mutex<EngineInner>>, job_id: String) {
    tokio::spawn(async move {
        let (job_snapshot, control, limiter, handoff_auth) = {
            let mut guard = inner.lock().await;
            // Reuse the scheduler Arc so a Pause stored during Starting is not dropped.
            let control = guard
                .controls
                .entry(job_id.clone())
                .or_insert_with(|| Arc::new(AtomicU8::new(0)))
                .clone();
            guard.active.insert(job_id.clone(), ());
            let job = match guard.jobs.iter().find(|j| j.id == job_id) {
                Some(j) => j.clone(),
                None => {
                    drop(guard);
                    finalize_worker(&inner, &job_id, &control, Ok(DownloadOutcome::Canceled)).await;
                    return;
                }
            };
            let limiter = guard.limiter.clone();
            let auth = guard.handoff_auth.get(&job_id).cloned();
            (job, control, limiter, auth)
        };

        let final_result = run_attempts(
            inner.clone(),
            job_id.clone(),
            job_snapshot,
            control.clone(),
            limiter,
            handoff_auth,
        )
        .await;

        finalize_worker(&inner, &job_id, &control, final_result).await;
    });
}

async fn run_attempts(
    inner: Arc<Mutex<EngineInner>>,
    job_id: String,
    job_snapshot: Job,
    control: Arc<AtomicU8>,
    limiter: Arc<GlobalBandwidthLimiter>,
    handoff_auth: Option<HandoffAuth>,
) -> Result<DownloadOutcome, DownloadError> {
    let mut attempt_job = job_snapshot;
    let mut retry_attempts = attempt_job.retry_attempts;

    loop {
        reconcile_for_attempt(&inner, &job_id, &mut attempt_job).await;

        if control.load(Ordering::Relaxed) == 0 {
            store_control(&control, WorkerControl::Continue);
        }

        let (progress_tx, progress_rx) = tokio::sync::mpsc::unbounded_channel::<TransferEvent>();
        let progress_pump = spawn_progress_pump(inner.clone(), job_id.clone(), progress_rx);
        let on_progress: TransferEventCallback = Arc::new(move |event: TransferEvent| {
            let _ = progress_tx.send(event);
        });
        let committer = Arc::new(EngineIdentity {
            inner: inner.clone(),
        });

        let (config, conn_budget, occupied_paths) = {
            let guard = inner.lock().await;
            let occupied_paths = guard
                .jobs
                .iter()
                .filter(|job| job.id != job_id)
                .flat_map(|job| [job.target_path.clone(), job.temp_path.clone()])
                .collect();
            (
                guard.config.clone(),
                guard.conn_budget.clone(),
                occupied_paths,
            )
        };
        let mut ctx = TransferContext::from_runtime(
            attempt_job.clone(),
            control.clone(),
            on_progress.clone(),
            handoff_auth.clone(),
            limiter.clone(),
            conn_budget,
            committer,
            &config,
        );
        ctx.occupied_paths = occupied_paths;
        let attempt_result = run_transfer(ctx).await;

        drop(on_progress);
        let _ = progress_pump.await;

        match attempt_result {
            Ok(outcome) => break Ok(outcome),
            Err(error) => {
                let (requeue, max_retry, recorded, temp_path, count_disk, downloaded, total) = {
                    let guard = inner.lock().await;
                    let requeue = guard.requeue_on_cancel.contains_key(&job_id);
                    let job = guard.jobs.iter().find(|job| job.id == job_id);
                    let recorded = job.map(durable_progress).unwrap_or(0);
                    let count_disk = job.is_some_and(single_stream_disk_counts);
                    let temp_path = job.map(|job| job.temp_path.clone());
                    let downloaded = job.map(|job| job.downloaded_bytes).unwrap_or(0);
                    let total = job.map(|job| job.total_bytes).unwrap_or(0);
                    (
                        requeue,
                        guard.config.auto_retry,
                        recorded,
                        temp_path,
                        count_disk,
                        downloaded,
                        total,
                    )
                };
                let on_disk = if count_disk {
                    match temp_path.as_ref() {
                        Some(path) => Some(metadata_len(path).await.unwrap_or(0)),
                        None => Some(0),
                    }
                } else {
                    None
                };
                let on_disk = on_disk.filter(|&n| !sparse_preallocate_hole(downloaded, total, n));
                let progressed =
                    attempt_made_progress(durable_progress(&attempt_job), recorded, on_disk);
                if requeue {
                    break Ok(DownloadOutcome::Canceled);
                }
                if progressed {
                    retry_attempts = 0;
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
                            bump_jobs(&mut guard);
                            emit_jobs_locked(&guard);
                        }
                    }
                    let _ = persist_live_jobs(&inner).await;
                    match sleep_interruptible(&control, delay).await {
                        Some(outcome) => break Ok(outcome),
                        None => {}
                    }
                    {
                        let guard = inner.lock().await;
                        if let Some(job) = guard.jobs.iter().find(|j| j.id == job_id) {
                            attempt_job = job.clone();
                        }
                    }
                    continue;
                }
                {
                    let mut guard = inner.lock().await;
                    if let Some(job) = find_job_mut(&mut guard.jobs, &job_id) {
                        if progressed {
                            job.retry_attempts = 0;
                            bump_jobs(&mut guard);
                        }
                    }
                }
                break Err(error);
            }
        }
    }
}

fn durable_progress(job: &Job) -> u64 {
    job.segment_map
        .as_ref()
        .map(|map| map.written_sum())
        .unwrap_or(job.downloaded_bytes)
}

fn single_stream_disk_counts(job: &Job) -> bool {
    job.segment_map.is_none() && job.transfer_format_version == 0
}

/// A multi start that `set_len`s the `.part` before the map is saved. File
/// length is not downloaded bytes. Unknown totals are not holes.
fn sparse_preallocate_hole(downloaded: u64, total: u64, on_disk: u64) -> bool {
    downloaded == 0 && total > 0 && on_disk >= total
}

fn attempt_made_progress(before: u64, recorded: u64, on_disk: Option<u64>) -> bool {
    let current = on_disk.map(|n| recorded.max(n)).unwrap_or(recorded);
    current > before
}

/// Multi / Restart skip metadata_len so a sparse `.part` cannot lie.
async fn reconcile_for_attempt(
    inner: &Arc<Mutex<EngineInner>>,
    job_id: &str,
    attempt_job: &mut Job,
) {
    let (temp_path, need_disk) = {
        let guard = inner.lock().await;
        match guard.jobs.iter().find(|j| j.id == job_id) {
            Some(job) => (
                Some(job.temp_path.clone()),
                matches!(
                    resume_oracle(job),
                    ResumeOracle::FreshSingle | ResumeOracle::LegacySingle
                ),
            ),
            None => (None, false),
        }
    };
    let on_disk = if need_disk {
        Some(match temp_path.as_ref() {
            Some(path) => metadata_len(path).await.unwrap_or(0),
            None => 0,
        })
    } else {
        None
    };
    {
        let mut guard = inner.lock().await;
        let restarting = guard.requeue_on_cancel.contains_key(job_id);
        if !restarting {
            if let Some(job) = find_job_mut(&mut guard.jobs, job_id) {
                reconcile_from_oracle(job, on_disk);
                job.state = JobState::Downloading;
                job.error = None;
                *attempt_job = job.clone();
                emit_jobs_locked(&guard);
            }
        }
    }
}

pub(super) async fn finalize_worker(
    inner: &Arc<Mutex<EngineInner>>,
    job_id: &str,
    control: &Arc<AtomicU8>,
    final_result: Result<DownloadOutcome, DownloadError>,
) {
    let (partial_to_delete, produced_to_delete, defer_start) = {
        let mut guard = inner.lock().await;
        let requeue = guard.requeue_on_cancel.contains_key(job_id);
        let has_partial = guard.pending_partial_deletes.contains_key(job_id);
        let has_final = guard.pending_final_deletes.remove(job_id).is_some();
        let job_present = guard.jobs.iter().any(|job| job.id == job_id);
        let defer_start = requeue && has_partial;
        let discard_produced = requeue || has_final || (has_partial && job_present);

        guard.active.remove(job_id);
        if !defer_start {
            guard.requeue_on_cancel.remove(job_id);
        }
        let partial_to_delete = if defer_start {
            guard.pending_partial_deletes.get(job_id).cloned()
        } else {
            guard.pending_partial_deletes.remove(job_id)
        };
        // Transfer-side discards must `clear_produced_file` after deleting, or
        // this would remove a later job that uniquified into the free name.
        let produced = guard.produced_files.remove(job_id);
        let produced_to_delete = if discard_produced { produced } else { None };

        if requeue {
            if let Some(job) = find_job_mut(&mut guard.jobs, job_id) {
                if !matches!(job.state, JobState::Queued) {
                    job.state = JobState::Queued;
                }
                clear_live_metrics(job);
            }
        } else if has_partial && matches!(final_result, Ok(DownloadOutcome::Completed)) {
            if let Some(job) = find_job_mut(&mut guard.jobs, job_id) {
                job.state = JobState::Canceled;
                job.mark_finished();
                clear_live_metrics(job);
                job.clear_partial_and_identity();
            }
            guard.handoff_auth.remove(job_id);
        } else {
            match final_result {
                Ok(DownloadOutcome::Completed) => {
                    if let Some(job) = find_job_mut(&mut guard.jobs, job_id) {
                        job.state = JobState::Completed;
                        job.progress = 100.0;
                        job.error = None;
                        job.on_completed();
                        clear_live_metrics(job);
                    }
                    guard.handoff_auth.remove(job_id);
                }
                Ok(DownloadOutcome::Paused) => {
                    if let Some(job) = find_job_mut(&mut guard.jobs, job_id) {
                        job.state = JobState::Paused;
                        clear_live_metrics(job);
                    }
                }
                Ok(DownloadOutcome::Canceled) => {
                    if let Some(job) = find_job_mut(&mut guard.jobs, job_id) {
                        job.state = JobState::Canceled;
                        job.mark_finished();
                        clear_live_metrics(job);
                        if partial_to_delete.is_some() {
                            job.clear_partial_and_identity();
                        }
                    }
                    guard.handoff_auth.remove(job_id);
                }
                Err(error) => {
                    let clear_auth = !matches!(control.load(Ordering::Relaxed), 1);
                    if let Some(job) = find_job_mut(&mut guard.jobs, job_id) {
                        match control.load(Ordering::Relaxed) {
                            1 => {
                                job.state = JobState::Paused;
                                clear_live_metrics(job);
                            }
                            2 => {
                                job.state = JobState::Canceled;
                                job.mark_finished();
                                clear_live_metrics(job);
                                if partial_to_delete.is_some() {
                                    job.clear_partial_and_identity();
                                }
                            }
                            _ => {
                                apply_failed_lifecycle(job, error);
                            }
                        }
                    }
                    if clear_auth {
                        guard.handoff_auth.remove(job_id);
                    }
                }
            }
        }
        guard.controls.remove(job_id);
        bump_jobs(&mut guard);
        emit_jobs_locked(&guard);
        if !defer_start {
            guard.wake.notify_one();
        }
        (partial_to_delete, produced_to_delete, defer_start)
    };

    let _ = persist_live_jobs(inner).await;

    if let Some(path) = partial_to_delete {
        remove_partial(&path).await;
    }
    if let Some(path) = produced_to_delete {
        remove_partial(&path).await;
    }

    if defer_start {
        let mut guard = inner.lock().await;
        guard.pending_partial_deletes.remove(job_id);
        guard.requeue_on_cancel.remove(job_id);
        guard.wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::fetch::{sleep_interruptible, CONTROL_PAUSED};
    use std::sync::atomic::AtomicU8;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::timeout;

    #[test]
    fn attempt_progress_counts_uncheckpointed_single_stream_bytes() {
        assert!(!attempt_made_progress(0, 0, Some(0)));
        assert!(attempt_made_progress(0, 0, Some(40)));
        assert!(attempt_made_progress(10, 10, Some(25)));
        assert!(!attempt_made_progress(25, 10, Some(25)));
        assert!(attempt_made_progress(10, 40, None));
        assert!(!sparse_preallocate_hole(0, 0, 100));
        assert!(sparse_preallocate_hole(0, 100, 100));
        assert!(!sparse_preallocate_hole(1, 100, 100));
    }

    #[test]
    fn durable_progress_uses_segment_written_sum() {
        use crate::download::job::Job;
        use crate::download::segment::{Segment, SegmentMap, SegmentState};
        let mut job = Job::new(
            "https://example.com/file.bin".into(),
            "file.bin".into(),
            std::path::PathBuf::from("file.bin"),
            std::path::PathBuf::from("file.bin.part"),
        );
        job.downloaded_bytes = 5;
        job.segment_map = Some(SegmentMap {
            total_bytes: 100,
            segment_count: 1,
            segments: vec![Segment {
                index: 0,
                start: 0,
                end: 99,
                written: 20,
                state: SegmentState::Active,
            }],
            preallocated: true,
        });
        assert_eq!(super::durable_progress(&job), 20);
        job.segment_map = None;
        assert_eq!(super::durable_progress(&job), 5);
    }

    #[tokio::test]
    async fn retry_delay_pause_returns_paused_without_waiting_full_delay() {
        let control = Arc::new(AtomicU8::new(0));
        let control_wait = control.clone();
        let waiter = tokio::spawn(async move {
            match sleep_interruptible(control_wait.as_ref(), Duration::from_secs(45)).await {
                Some(outcome) => Ok(outcome),
                None => Err("delay completed"),
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        control.store(CONTROL_PAUSED, Ordering::Relaxed);
        let outcome = timeout(Duration::from_secs(1), waiter)
            .await
            .expect("pause must interrupt retry delay")
            .expect("join")
            .expect("Paused");
        assert_eq!(outcome, DownloadOutcome::Paused);
    }
}
