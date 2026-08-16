//! Multi-segment download orchestrator.
//!
//! Parallel Range workers write into one `.part` via [`SegmentFileWriter`].
//! Progress is **map-authoritative** (`sum(written)`); never `metadata_len`
//! after a map exists (preallocate would report the full file size).
//!
//! Convert multi→single only when every segment `written == 0`. After any
//! `written > 0`, an unusable Range is a Resume error and the map is kept.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::StatusCode;
use tokio::time::sleep;

use super::client::download_client;
use super::conn_budget::{host_key_for_budget, ConnectionBudget};
use super::context::TransferContext;
use super::eta::EtaSmoother;
use super::filesystem::{
    ensure_parent_directory, is_untracked_preallocate_hole, metadata_len, move_to_final_path,
};
use super::http::{
    content_range_size_mismatch, control_outcome, progress_percent, reconnect_backoff,
    run_http_download_with_ctx, send_segment_get, should_retry_status, sleep_interruptible,
    RECONNECT_MAX,
};
use super::job::{
    download_error, ContentValidators, DownloadError, DownloadOutcome, FailureCategory,
    TransferMode,
};
use super::progress::{
    CommitIdentity, MapUpdate, ProgressHint, ProgressTick, TransferEvent, TransferEventCallback,
};
use super::resume::{
    resume_oracle, ResumeOracle, FALLBACK_LEGACY_PARTIAL, FALLBACK_MAP_INCONSISTENT,
    FALLBACK_MAP_MISSING,
};
use super::segment::{partition, SegmentMap, SegmentState};
use super::segment_io::{try_preallocate, SegmentFileWriter};
use super::verify::verify_sha256_if_expected;

const CONTROL_POLL: Duration = Duration::from_millis(200);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(400);

/// User-visible Resume error when a v1 map is missing or inconsistent.
pub(crate) const RESUME_RESTART_MESSAGE: &str = "Multi-part incomplete; Restart required.";
pub(crate) const RANGE_IGNORED_MESSAGE: &str =
    "Server ignored Range on a multi-segment resume. Use Restart.";

/// Outcome of the multi-start map step (reuse vs fresh partition).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparedMap {
    Reuse(SegmentMap),
    Fresh(SegmentMap),
}

/// Choose an existing consistent map or partition a new one. Never repartitions
/// a present map and never resets `written`.
pub(crate) fn prepare_segment_map(
    job: &super::job::Job,
    total_bytes: u64,
    max_segments: u32,
) -> Result<PreparedMap, DownloadError> {
    if let Some(map) = job.segment_map.as_ref() {
        if map_consistent_with_total(map, total_bytes, &job.validators) {
            return Ok(PreparedMap::Reuse(map.clone()));
        }
        return Err(resume_restart_required());
    }
    if job.transfer_format_version >= 1 {
        return Err(resume_restart_required());
    }
    Ok(PreparedMap::Fresh(partition(total_bytes, max_segments)))
}

pub(crate) fn resume_restart_required() -> DownloadError {
    download_error(
        FailureCategory::Resume,
        RESUME_RESTART_MESSAGE.into(),
        false,
    )
}

pub(crate) fn map_consistent_with_total(
    map: &SegmentMap,
    total_bytes: u64,
    validators: &ContentValidators,
) -> bool {
    map.is_consistent()
        && map.total_bytes == total_bytes
        && validators
            .expected_size
            .map(|size| size == total_bytes)
            .unwrap_or(true)
}

pub(crate) fn all_written_zero(map: &SegmentMap) -> bool {
    map.segments.iter().all(|segment| segment.written == 0)
}

/// Convert multi→single when every segment still has `written == 0`.
pub(crate) fn may_convert_multi_to_single(map: &SegmentMap) -> bool {
    all_written_zero(map)
}

/// Run multi-segment transfer. Converts to single-stream only when every
/// segment still has `written == 0`.
pub async fn run_multi_segment_download(
    ctx: &mut TransferContext,
) -> Result<DownloadOutcome, DownloadError> {
    if matches!(resume_oracle(&ctx.job), ResumeOracle::LegacySingle) {
        return fallback_to_single(ctx, FALLBACK_LEGACY_PARTIAL).await;
    }

    // Disk check only for v0 / no-map (single-stream semantics).
    if ctx.job.segment_map.is_none() && ctx.job.transfer_format_version == 0 {
        let on_disk = metadata_len(&ctx.job.temp_path).await.unwrap_or(0);
        if is_untracked_preallocate_hole(&ctx.job, on_disk) {
            // Crash window after set_len, before map persist — do not metadata_len resume.
            let _ = tokio::fs::remove_file(&ctx.job.temp_path).await;
        } else if on_disk > 0 {
            return fallback_to_single(ctx, FALLBACK_LEGACY_PARTIAL).await;
        }
    }

    let total = known_total(&ctx.job)?;
    let prepared = match prepare_segment_map(&ctx.job, total, ctx.multi_max_segments) {
        Ok(prepared) => prepared,
        Err(error) => {
            let reason = match ctx.job.segment_map.as_ref() {
                None => FALLBACK_MAP_MISSING,
                Some(_) => FALLBACK_MAP_INCONSISTENT,
            };
            ctx.job.fallback_reason = Some(reason.to_string());
            if let Err(message) = ctx
                .committer
                .commit(
                    &mut ctx.job,
                    CommitIdentity {
                        fallback_reason: Some(reason.to_string()),
                        ..Default::default()
                    },
                )
                .await
            {
                return Err(download_error(FailureCategory::Internal, message, false));
            }
            return Err(error);
        }
    };
    let reused = matches!(prepared, PreparedMap::Reuse(_));
    let mut map = match prepared {
        PreparedMap::Reuse(map) | PreparedMap::Fresh(map) => map,
    };

    // version=1 on the in-memory job *before* any set_len so same-process resume is safe.
    apply_multi_identity(ctx, &map, total);
    commit_multi_identity(ctx, &map).await?;

    if let Err(message) = ensure_parent_directory(&ctx.job.target_path).await {
        return fail_before_workers(
            ctx,
            reused,
            false,
            download_error(FailureCategory::Disk, message, false),
        )
        .await;
    }
    if let Err(message) = ensure_parent_directory(&ctx.job.temp_path).await {
        return fail_before_workers(
            ctx,
            reused,
            false,
            download_error(FailureCategory::Disk, message, false),
        )
        .await;
    }

    let remaining = total.saturating_sub(map.written_sum());
    let did_preallocate = if remaining == 0 {
        map.preallocated
    } else {
        match try_preallocate(&ctx.job.temp_path, total, remaining).await {
            Ok(true) => {
                map.preallocated = true;
                ctx.job.segment_map = Some(map.clone());
                commit_multi_identity(ctx, &map).await?;
                true
            }
            Ok(false) => false,
            Err(message) => {
                return fail_before_workers(
                    ctx,
                    reused,
                    false,
                    download_error(FailureCategory::Disk, message, false),
                )
                .await;
            }
        }
    };

    let temp_path = ctx.job.temp_path.clone();
    let writer =
        match tokio::task::spawn_blocking(move || SegmentFileWriter::open(&temp_path)).await {
            Ok(Ok(writer)) => Arc::new(writer),
            Ok(Err(error)) => {
                return fail_before_workers(
                    ctx,
                    reused,
                    did_preallocate,
                    download_error(
                        FailureCategory::Disk,
                        format!("Could not open multi-segment file: {error}"),
                        false,
                    ),
                )
                .await;
            }
            Err(error) => {
                return fail_before_workers(
                    ctx,
                    reused,
                    did_preallocate,
                    download_error(
                        FailureCategory::Disk,
                        format!("Could not open multi-segment file: {error}"),
                        false,
                    ),
                )
                .await;
            }
        };

    if map.written_sum() >= total && total > 0 {
        return finalize_completed(ctx, writer, &map).await;
    }

    let result = run_segment_workers(ctx, writer.clone(), map).await;

    match result {
        Ok((DownloadOutcome::Completed, map)) => finalize_completed(ctx, writer, &map).await,
        Ok((outcome, map)) => {
            persist_map_exit(ctx, &map, 0).await?;
            if matches!(outcome, DownloadOutcome::Paused) {
                let flush = writer.clone();
                let _ = tokio::task::spawn_blocking(move || flush.flush_sync_data()).await;
            }
            drop(writer);
            Ok(outcome)
        }
        Err((error, map)) => {
            persist_map_exit(ctx, &map, 0).await?;
            if may_convert_multi_to_single(&map) {
                // Writer handle blocks DeleteFile on Windows.
                drop(writer);
                remove_unwritten_partial(ctx).await?;
                fallback_to_single(ctx, fallback_reason_for(&error)).await
            } else {
                drop(writer);
                Err(error)
            }
        }
    }
}

fn known_total(job: &super::job::Job) -> Result<u64, DownloadError> {
    if let Some(map) = job.segment_map.as_ref() {
        if map.total_bytes > 0 {
            return Ok(map.total_bytes);
        }
    }
    if job.total_bytes > 0 {
        return Ok(job.total_bytes);
    }
    if let Some(size) = job.validators.expected_size {
        if size > 0 {
            return Ok(size);
        }
    }
    Err(download_error(
        FailureCategory::Internal,
        "Multi-connection requires a known file size.".into(),
        false,
    ))
}

fn apply_multi_identity(ctx: &mut TransferContext, map: &SegmentMap, total: u64) {
    ctx.job.transfer_format_version = 1;
    ctx.job.segment_map = Some(map.clone());
    ctx.job.transfer_mode = Some(TransferMode::Multi);
    ctx.job.total_bytes = total;
    ctx.job.downloaded_bytes = map.written_sum();
    ctx.job.resume_supported = true;
    ctx.job.progress = progress_percent(ctx.job.downloaded_bytes, total);
}

fn multi_identity_commit(map: &SegmentMap) -> CommitIdentity {
    let downloaded = map.written_sum();
    CommitIdentity {
        downloaded_bytes: Some(downloaded),
        total_bytes: Some(map.total_bytes),
        progress: Some(progress_percent(downloaded, map.total_bytes)),
        resume_supported: Some(true),
        transfer_format_version: Some(1),
        transfer_mode: Some(TransferMode::Multi),
        map: MapUpdate::Set(map.clone()),
        ..Default::default()
    }
}

async fn commit_multi_identity(
    ctx: &mut TransferContext,
    map: &SegmentMap,
) -> Result<(), DownloadError> {
    ctx.committer
        .commit(&mut ctx.job, multi_identity_commit(map))
        .await
        .map_err(|message| download_error(FailureCategory::Internal, message, false))
}

async fn persist_map_exit(
    ctx: &mut TransferContext,
    map: &SegmentMap,
    active: u32,
) -> Result<(), DownloadError> {
    ctx.job.segment_map = Some(map.clone());
    ctx.job.downloaded_bytes = map.written_sum();
    ctx.job.progress = progress_percent(map.written_sum(), map.total_bytes);
    commit_multi_identity(ctx, map).await?;
    (ctx.on_progress)(TransferEvent::Tick(ProgressTick {
        downloaded_bytes: Some(map.written_sum()),
        total_bytes: Some(map.total_bytes),
        progress: Some(progress_percent(map.written_sum(), map.total_bytes)),
        active_connections: Some(active),
        ..Default::default()
    }));
    Ok(())
}

/// One-shot toast when multi actually converts to single-stream.
const MULTI_FALLBACK_TOAST: &str = "Fell back to a single connection.";

/// Roll back v1 map. `continue_as_single` publishes live `active_connections=1`
/// and a convert toast (skipped when this reason was already recorded).
/// Hard-fail (`false`) stays at 0 connections and never toasts.
async fn rollback_multi_identity(
    ctx: &mut TransferContext,
    reason: &str,
    continue_as_single: bool,
) {
    let should_toast = continue_as_single && ctx.job.fallback_reason.as_deref() != Some(reason);
    ctx.job.transfer_format_version = 0;
    ctx.job.segment_map = None;
    ctx.job.transfer_mode = Some(TransferMode::Single);
    ctx.job.fallback_reason = Some(reason.to_string());
    let active = if continue_as_single { 1 } else { 0 };
    ctx.job.active_connections = active;
    let _ = ctx
        .committer
        .commit(
            &mut ctx.job,
            CommitIdentity {
                transfer_format_version: Some(0),
                map: MapUpdate::Clear,
                transfer_mode: Some(TransferMode::Single),
                fallback_reason: Some(reason.to_string()),
                ..Default::default()
            },
        )
        .await;
    (ctx.on_progress)(TransferEvent::Tick(ProgressTick {
        active_connections: Some(active),
        ..Default::default()
    }));
    if should_toast {
        (ctx.on_progress)(TransferEvent::Toast(MULTI_FALLBACK_TOAST.to_string()));
    }
}

async fn fail_before_workers(
    ctx: &mut TransferContext,
    reused: bool,
    did_preallocate: bool,
    error: DownloadError,
) -> Result<DownloadOutcome, DownloadError> {
    if !reused {
        if did_preallocate {
            // No writer handle yet (open failed or never opened).
            let _ = tokio::fs::remove_file(&ctx.job.temp_path).await;
        }
        rollback_multi_identity(ctx, "multi_start_failed", false).await;
    }
    Err(error)
}

async fn fallback_to_single(
    ctx: &mut TransferContext,
    reason: &str,
) -> Result<DownloadOutcome, DownloadError> {
    rollback_multi_identity(ctx, reason, true).await;
    run_http_download_with_ctx(ctx).await
}

/// Drop a zero-written preallocate hole so single-stream does not resume from `metadata_len`.
async fn remove_unwritten_partial(ctx: &TransferContext) -> Result<(), DownloadError> {
    if ctx.job.temp_path.exists() {
        tokio::fs::remove_file(&ctx.job.temp_path)
            .await
            .map_err(|io_err| {
                download_error(
                    FailureCategory::Disk,
                    format!(
                        "Could not remove preallocated file before single-stream fallback: {io_err}"
                    ),
                    false,
                )
            })?;
    }
    Ok(())
}

fn fallback_reason_for(error: &DownloadError) -> &'static str {
    match error.category {
        FailureCategory::Http => "multi_http_fallback",
        FailureCategory::Network => "multi_network_fallback",
        FailureCategory::Resume => "multi_resume_fallback",
        FailureCategory::Disk => "multi_disk_fallback",
        FailureCategory::Internal => "multi_internal_fallback",
    }
}

struct SharedMulti {
    map: Mutex<SegmentMap>,
    active: AtomicU32,
    reconnects: AtomicU32,
    window: Mutex<SpeedWindow>,
}

struct SpeedWindow {
    start: Instant,
    bytes: u64,
    eta: EtaSmoother,
}

fn lock_map(map: &Mutex<SegmentMap>) -> std::sync::MutexGuard<'_, SegmentMap> {
    map.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn record_window_bytes(shared: &SharedMulti, n: u64) {
    let mut window = shared
        .window
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    window.bytes = window.bytes.saturating_add(n);
}

/// Sample the shared 400ms window and feed the smoother. `None` if another
/// worker already reset this window (elapsed still below the interval).
fn take_due_sample(shared: &SharedMulti, remaining: u64) -> Option<(u64, u64)> {
    let mut window = shared
        .window
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if window.start.elapsed() < PROGRESS_INTERVAL {
        return None;
    }
    let elapsed = window.start.elapsed().as_secs_f64().max(0.001);
    let speed = (window.bytes as f64 / elapsed) as u64;
    window.start = Instant::now();
    window.bytes = 0;
    let (_, eta) = window.eta.observe(speed, remaining);
    Some((speed, eta))
}

fn last_smoothed(shared: &SharedMulti) -> (Option<u64>, Option<u64>) {
    let window = shared
        .window
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    (window.eta.last_speed(), window.eta.last_eta())
}

struct SegmentTask {
    index: u32,
    job_url: String,
    pinned_url: String,
    host: String,
    control: Arc<std::sync::atomic::AtomicU8>,
    on_progress: TransferEventCallback,
    handoff: Option<super::handoff::HandoffAuth>,
    limiter: Arc<super::bandwidth::GlobalBandwidthLimiter>,
    budget: Arc<ConnectionBudget>,
    writer: Arc<SegmentFileWriter>,
    shared: Arc<SharedMulti>,
    validators: ContentValidators,
}

async fn run_segment_workers(
    ctx: &mut TransferContext,
    writer: Arc<SegmentFileWriter>,
    map: SegmentMap,
) -> Result<(DownloadOutcome, SegmentMap), (DownloadError, SegmentMap)> {
    let client = match download_client() {
        Ok(client) => client,
        Err(error) => return Err((error, map)),
    };

    let host = host_key_for_budget(&ctx.resolved_url);
    let shared = Arc::new(SharedMulti {
        map: Mutex::new(map.clone()),
        active: AtomicU32::new(0),
        reconnects: AtomicU32::new(ctx.job.reconnect_count),
        window: Mutex::new(SpeedWindow {
            start: Instant::now(),
            bytes: 0,
            eta: EtaSmoother::new(),
        }),
    });

    let mut handles = Vec::new();
    for segment in &map.segments {
        if segment.written >= segment.length() {
            continue;
        }
        let task = SegmentTask {
            index: segment.index,
            job_url: ctx.job.url.clone(),
            pinned_url: ctx.resolved_url.clone(),
            host: host.clone(),
            control: ctx.control.clone(),
            on_progress: ctx.on_progress.clone(),
            handoff: ctx.handoff_auth.clone(),
            limiter: ctx.limiter.clone(),
            budget: ctx.conn_budget.clone(),
            writer: writer.clone(),
            shared: shared.clone(),
            validators: ctx.job.validators.clone(),
        };
        let client = client.clone();
        handles.push(tokio::spawn(async move { run_segment(client, task).await }));
    }

    if handles.is_empty() {
        return Ok((DownloadOutcome::Completed, map));
    }

    let results = futures_util::future::join_all(handles).await;
    let map = lock_map(&shared.map).clone();
    ctx.job.reconnect_count = shared.reconnects.load(Ordering::Relaxed);
    ctx.job.segment_map = Some(map.clone());
    ctx.job.downloaded_bytes = map.written_sum();

    if let Some(outcome) = control_outcome(&ctx.control) {
        return Ok((outcome, map));
    }

    let mut first_error = None;
    for result in results {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
            Err(join) => {
                if first_error.is_none() {
                    first_error = Some(download_error(
                        FailureCategory::Internal,
                        format!("Segment worker failed: {join}"),
                        false,
                    ));
                }
            }
        }
    }

    if map.segments.iter().all(|s| s.written >= s.length()) {
        return Ok((DownloadOutcome::Completed, map));
    }

    if let Some(error) = first_error {
        return Err((error, map));
    }

    Err((
        download_error(
            FailureCategory::Resume,
            "Multi-connection failed; use Restart.".into(),
            false,
        ),
        map,
    ))
}

async fn run_segment(client: reqwest::Client, task: SegmentTask) -> Result<(), DownloadError> {
    let permit = match task
        .budget
        .acquire_interruptible(&task.host, &task.control)
        .await
    {
        Ok(permit) => permit,
        Err(_) => return Ok(()),
    };

    task.active_add();
    emit_progress(&task, false);
    let result = run_segment_loop(&client, &task).await;
    task.active_sub();
    emit_progress(&task, false);
    drop(permit);
    result
}

impl SegmentTask {
    fn active_add(&self) {
        self.shared.active.fetch_add(1, Ordering::Relaxed);
    }

    fn active_sub(&self) {
        self.shared.active.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn run_segment_loop(
    client: &reqwest::Client,
    task: &SegmentTask,
) -> Result<(), DownloadError> {
    let mut short_reconnects = 0u32;

    loop {
        if control_outcome(&task.control).is_some() {
            return Ok(());
        }

        let (start, end, written) = {
            let map = lock_map(&task.shared.map);
            let segment = map.segments.get(task.index as usize).ok_or_else(|| {
                download_error(
                    FailureCategory::Internal,
                    "Segment index missing from map.".into(),
                    false,
                )
            })?;
            (segment.start, segment.end, segment.written)
        };

        if written >= end.saturating_sub(start).saturating_add(1) {
            mark_segment(&task.shared, task.index, |s| {
                s.state = SegmentState::Completed;
            });
            return Ok(());
        }

        {
            let mut map = lock_map(&task.shared.map);
            if let Some(segment) = map.segments.get_mut(task.index as usize) {
                if segment.state != SegmentState::Completed {
                    segment.state = SegmentState::Active;
                }
            }
        }

        let range_start = start.saturating_add(written);
        let fetch = send_segment_get(
            client,
            &task.job_url,
            &task.pinned_url,
            range_start,
            end,
            &task.validators,
            task.handoff.as_ref(),
        )
        .await;

        let response = match fetch {
            Ok(response) => response,
            Err(error) => {
                if control_outcome(&task.control).is_some() {
                    return Ok(());
                }
                if try_segment_reconnect(task, &error, &mut short_reconnects).await? {
                    continue;
                }
                if control_outcome(&task.control).is_some() {
                    return Ok(());
                }
                mark_segment(&task.shared, task.index, |s| {
                    s.state = SegmentState::Failed;
                });
                return Err(error);
            }
        };

        if control_outcome(&task.control).is_some() {
            return Ok(());
        }

        let status = response.status();
        if status.is_redirection() {
            let error = download_error(
                FailureCategory::Network,
                "Unexpected redirect on segment request.".into(),
                true,
            );
            if try_segment_reconnect(task, &error, &mut short_reconnects).await? {
                continue;
            }
            if control_outcome(&task.control).is_some() {
                return Ok(());
            }
            mark_segment(&task.shared, task.index, |s| {
                s.state = SegmentState::Failed;
            });
            return Err(error);
        }

        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            mark_segment(&task.shared, task.index, |s| {
                s.state = SegmentState::Failed;
            });
            return Err(download_error(
                FailureCategory::Http,
                format!(
                    "Download failed with HTTP {status}. Access denied — the link may require a browser session, cookies, or a fresh token."
                ),
                false,
            ));
        }

        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            mark_segment(&task.shared, task.index, |s| {
                s.state = SegmentState::Failed;
            });
            return Err(download_error(
                FailureCategory::Resume,
                format!(
                    "Server rejected resume at {range_start} bytes. Use Restart to download from zero."
                ),
                false,
            ));
        }

        if status == StatusCode::OK && range_start > 0 {
            // 200 is a full entity. Never write file-from-zero at a non-zero offset
            // (non-first segment, mid-segment resume, or If-Range mismatch).
            mark_segment(&task.shared, task.index, |s| {
                s.state = SegmentState::Failed;
            });
            return Err(download_error(
                FailureCategory::Resume,
                RANGE_IGNORED_MESSAGE.into(),
                false,
            ));
        }

        if !(status.is_success() || status == StatusCode::PARTIAL_CONTENT) {
            let retryable = should_retry_status(status);
            let error = download_error(
                FailureCategory::Http,
                format!("Download failed with HTTP {status}."),
                retryable,
            );
            if retryable && try_segment_reconnect(task, &error, &mut short_reconnects).await? {
                continue;
            }
            if control_outcome(&task.control).is_some() {
                return Ok(());
            }
            mark_segment(&task.shared, task.index, |s| {
                s.state = SegmentState::Failed;
            });
            return Err(error);
        }

        let parsed_range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(super::filesystem::parse_content_range);

        if status == StatusCode::PARTIAL_CONTENT {
            match parsed_range {
                Some((got_start, _end, _total)) if got_start == range_start => {}
                Some((got_start, _end, _total)) => {
                    mark_segment(&task.shared, task.index, |s| {
                        s.state = SegmentState::Failed;
                    });
                    return Err(download_error(
                        FailureCategory::Resume,
                        format!(
                            "Unexpected resume range (got start {got_start}, expected {range_start}). Use Restart."
                        ),
                        false,
                    ));
                }
                None if range_start > 0 => {
                    mark_segment(&task.shared, task.index, |s| {
                        s.state = SegmentState::Failed;
                    });
                    return Err(download_error(
                        FailureCategory::Resume,
                        "Missing or invalid Content-Range on partial response. Use Restart.".into(),
                        false,
                    ));
                }
                None => {}
            }
        }

        let range_total = parsed_range.and_then(|(_s, _e, total)| total);
        if let Some((total, expected)) =
            content_range_size_mismatch(range_total, task.validators.expected_size)
        {
            mark_segment(&task.shared, task.index, |s| {
                s.state = SegmentState::Failed;
            });
            return Err(download_error(
                FailureCategory::Resume,
                format!("Remote size changed ({total} bytes vs expected {expected}). Use Restart."),
                false,
            ));
        }

        match stream_segment_body(response, task, range_start, end).await {
            Ok(true) => {
                mark_segment(&task.shared, task.index, |s| {
                    s.state = SegmentState::Completed;
                });
                emit_progress(task, false);
                return Ok(());
            }
            Ok(false) => return Ok(()), // pause/cancel
            Err(error) => {
                if control_outcome(&task.control).is_some() {
                    return Ok(());
                }
                if try_segment_reconnect(task, &error, &mut short_reconnects).await? {
                    continue;
                }
                if control_outcome(&task.control).is_some() {
                    return Ok(());
                }
                mark_segment(&task.shared, task.index, |s| {
                    s.state = SegmentState::Failed;
                });
                return Err(error);
            }
        }
    }
}

/// `true` when the segment is complete; `false` on pause/cancel.
async fn stream_segment_body(
    response: reqwest::Response,
    task: &SegmentTask,
    mut offset: u64,
    end: u64,
) -> Result<bool, DownloadError> {
    let mut stream = response.bytes_stream();
    let mut last_progress = Instant::now();

    loop {
        if let Some(outcome) = control_outcome(&task.control) {
            if matches!(outcome, DownloadOutcome::Paused) {
                let writer = task.writer.clone();
                let _ = tokio::task::spawn_blocking(move || writer.flush_sync_data()).await;
            }
            emit_progress(task, false);
            return Ok(false);
        }

        let next = tokio::select! {
            item = stream.next() => item,
            _ = sleep(CONTROL_POLL) => {
                continue;
            }
        };

        let Some(chunk_result) = next else {
            break;
        };

        let chunk = match chunk_result {
            Ok(chunk) => chunk,
            Err(error) => {
                let retryable = error.is_timeout()
                    || error.is_connect()
                    || error.is_request()
                    || error.is_body()
                    || error.is_decode();
                return Err(download_error(
                    FailureCategory::Network,
                    format!("Download stream failed: {error}"),
                    retryable,
                ));
            }
        };

        if chunk.is_empty() {
            continue;
        }

        let acquired = task
            .limiter
            .acquire(chunk.len(), Some(task.control.as_ref()))
            .await;

        let writer = task.writer.clone();
        let data_len = chunk.len();
        let write_offset = offset;
        let n = tokio::task::spawn_blocking(move || writer.write_at(write_offset, &chunk[..], end))
            .await
            .map_err(|error| {
                download_error(
                    FailureCategory::Disk,
                    format!("Segment write task failed: {error}"),
                    false,
                )
            })?
            .map_err(|error| {
                download_error(
                    FailureCategory::Disk,
                    format!("Could not write download data: {error}"),
                    false,
                )
            })?;

        if n == 0 {
            break;
        }

        offset = offset.saturating_add(n as u64);
        record_window_bytes(&task.shared, n as u64);

        let (written, length, capped) = {
            let mut map = lock_map(&task.shared.map);
            if let Some(segment) = map.segments.get_mut(task.index as usize) {
                segment.written = offset.saturating_sub(segment.start).min(segment.length());
                if segment.written >= segment.length() {
                    segment.state = SegmentState::Completed;
                }
                (
                    segment.written,
                    segment.length(),
                    n < data_len || segment.written >= segment.length(),
                )
            } else {
                (0, 0, n < data_len)
            }
        };

        if !acquired {
            emit_progress(task, false);
            return Ok(false);
        }

        // End-cap truncated the chunk (or segment is full) — do not write_at past end.
        if capped || written >= length {
            break;
        }

        if last_progress.elapsed() >= PROGRESS_INTERVAL {
            emit_progress(task, true);
            last_progress = Instant::now();
        }
    }

    if let Some(outcome) = control_outcome(&task.control) {
        if matches!(outcome, DownloadOutcome::Paused) {
            let writer = task.writer.clone();
            let _ = tokio::task::spawn_blocking(move || writer.flush_sync_data()).await;
        }
        emit_progress(task, false);
        return Ok(false);
    }

    let (written, needed) = {
        let map = lock_map(&task.shared.map);
        map.segments
            .get(task.index as usize)
            .map(|segment| (segment.written, segment.length()))
            .unwrap_or((0, 0))
    };
    if written < needed {
        return Err(download_error(
            FailureCategory::Network,
            format!("Download incomplete ({written} of {needed} bytes in segment)."),
            true,
        ));
    }

    Ok(true)
}

/// Returns `true` when the caller should retry the segment GET.
async fn try_segment_reconnect(
    task: &SegmentTask,
    error: &DownloadError,
    short_reconnects: &mut u32,
) -> Result<bool, DownloadError> {
    if *short_reconnects >= RECONNECT_MAX
        || !error.retryable
        || !matches!(
            error.category,
            FailureCategory::Network | FailureCategory::Http
        )
    {
        return Ok(false);
    }

    let next = *short_reconnects + 1;
    if sleep_interruptible(&task.control, reconnect_backoff(next))
        .await
        .is_some()
    {
        return Ok(false);
    }

    *short_reconnects = next;
    let total = task.shared.reconnects.fetch_add(1, Ordering::Relaxed) + 1;
    emit_progress(task, false);
    (task.on_progress)(TransferEvent::Tick(ProgressTick {
        reconnect_count: Some(total),
        state_hint: Some(ProgressHint::Starting),
        ..Default::default()
    }));
    Ok(true)
}

fn mark_segment(shared: &SharedMulti, index: u32, f: impl FnOnce(&mut super::segment::Segment)) {
    let mut map = lock_map(&shared.map);
    if let Some(segment) = map.segments.get_mut(index as usize) {
        f(segment);
    }
}

fn emit_progress(task: &SegmentTask, sample_window: bool) {
    let (downloaded, total, written) = {
        let map = lock_map(&task.shared.map);
        let downloaded = map.written_sum();
        let total = map.total_bytes;
        let written = map.segments.iter().map(|segment| segment.written).collect();
        (downloaded, total, written)
    };
    let remaining = total.saturating_sub(downloaded);
    let (speed, eta) = if sample_window {
        take_due_sample(&task.shared, remaining)
            .map(|(s, e)| (Some(s), Some(e)))
            .unwrap_or_else(|| last_smoothed(&task.shared))
    } else {
        last_smoothed(&task.shared)
    };
    (task.on_progress)(TransferEvent::Tick(ProgressTick {
        downloaded_bytes: Some(downloaded),
        total_bytes: Some(total),
        speed,
        eta_secs: eta,
        progress: Some(progress_percent(downloaded, total)),
        state_hint: Some(ProgressHint::Downloading),
        segment_written: Some(written),
        active_connections: Some(task.shared.active.load(Ordering::Relaxed)),
        ..Default::default()
    }));
}

async fn finalize_completed(
    ctx: &mut TransferContext,
    writer: Arc<SegmentFileWriter>,
    map: &SegmentMap,
) -> Result<DownloadOutcome, DownloadError> {
    let writer_flush = writer.clone();
    tokio::task::spawn_blocking(move || writer_flush.flush_sync_data())
        .await
        .map_err(|error| {
            download_error(
                FailureCategory::Disk,
                format!("Could not flush download file: {error}"),
                false,
            )
        })?
        .map_err(|error| {
            download_error(
                FailureCategory::Disk,
                format!("Could not flush download file: {error}"),
                false,
            )
        })?;
    drop(writer);

    if let Err(error) =
        verify_sha256_if_expected(&ctx.job.temp_path, ctx.job.expected_sha256.as_deref()).await
    {
        // Hash fail is a Failed transfer: keep .part and retain the completed map.
        persist_map_exit(ctx, map, 0).await?;
        return Err(error);
    }

    let final_path = move_to_final_path(
        &ctx.job.temp_path,
        &ctx.job.target_path,
        ctx.job.replace_existing,
    )
    .await
    .map_err(|message| download_error(FailureCategory::Disk, message, false))?;

    let downloaded = map.written_sum().max(map.total_bytes);
    ctx.job.downloaded_bytes = downloaded;
    let temp_path = ctx.job.temp_path.clone();
    ctx.committer
        .commit(
            &mut ctx.job,
            CommitIdentity {
                downloaded_bytes: Some(downloaded),
                total_bytes: Some(map.total_bytes.max(downloaded)),
                progress: Some(100.0),
                filename: final_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string()),
                target_path: Some(final_path),
                temp_path: Some(temp_path),
                resume_supported: Some(true),
                ..Default::default()
            },
        )
        .await
        .map_err(|message| download_error(FailureCategory::Internal, message, false))?;
    Ok(DownloadOutcome::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::bandwidth::GlobalBandwidthLimiter;
    use crate::download::engine::EngineRuntimeConfig;
    use crate::download::filesystem::{looks_like_preallocate_hole, reconcile_partial_progress};
    use crate::download::handoff::{HandoffAuth, HandoffAuthHeader};
    use crate::download::job::Job;
    use crate::download::progress::{NoopIdentity, TestProgress};
    use crate::download::segment::{Segment, MIN_SEGMENT_SIZE};
    use crate::download::transfer::run_transfer;
    use crate::download::verify::{sha256_hex, SHA256_EMPTY};
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU8;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn sample_job() -> Job {
        Job::new(
            "https://example.com/file.bin".into(),
            "file.bin".into(),
            PathBuf::from("C:\\dl\\file.bin"),
            PathBuf::from("C:\\dl\\file.bin.part"),
        )
    }

    fn test_ctx(
        job: Job,
        on_progress: TransferEventCallback,
        handoff: Option<HandoffAuth>,
        max_segments: u32,
    ) -> TransferContext {
        test_ctx_commit(
            job,
            on_progress,
            handoff,
            max_segments,
            Arc::new(NoopIdentity),
        )
    }

    fn test_ctx_commit(
        job: Job,
        on_progress: TransferEventCallback,
        handoff: Option<HandoffAuth>,
        max_segments: u32,
        committer: Arc<dyn crate::download::progress::IdentityCommit>,
    ) -> TransferContext {
        TransferContext::from_runtime(
            job,
            Arc::new(AtomicU8::new(0)),
            on_progress,
            handoff,
            GlobalBandwidthLimiter::new(None),
            ConnectionBudget::new(32, 8),
            committer,
            &EngineRuntimeConfig {
                multi_min_bytes: 1,
                multi_max_segments: max_segments,
                ..Default::default()
            },
        )
    }

    fn two_seg_map(written0: u64, written1: u64) -> SegmentMap {
        let total = 2 * MIN_SEGMENT_SIZE;
        SegmentMap {
            total_bytes: total,
            segment_count: 2,
            segments: vec![
                Segment {
                    index: 0,
                    start: 0,
                    end: MIN_SEGMENT_SIZE - 1,
                    written: written0,
                    state: SegmentState::Active,
                },
                Segment {
                    index: 1,
                    start: MIN_SEGMENT_SIZE,
                    end: total - 1,
                    written: written1,
                    state: SegmentState::Pending,
                },
            ],
            preallocated: true,
        }
    }

    #[test]
    fn prepare_reuses_consistent_map_and_preserves_written() {
        let mut job = sample_job();
        let map = two_seg_map(100, 50);
        let bounds = (
            map.segments[0].start,
            map.segments[0].end,
            map.segments[1].start,
            map.segments[1].end,
        );
        job.segment_map = Some(map);
        job.transfer_format_version = 1;
        job.total_bytes = 2 * MIN_SEGMENT_SIZE;
        job.validators.expected_size = Some(2 * MIN_SEGMENT_SIZE);

        let prepared = prepare_segment_map(&job, 2 * MIN_SEGMENT_SIZE, 8).unwrap();
        let PreparedMap::Reuse(reused) = prepared else {
            panic!("expected reuse");
        };
        assert_eq!(reused.segments[0].written, 100);
        assert_eq!(reused.segments[1].written, 50);
        assert_eq!(
            (
                reused.segments[0].start,
                reused.segments[0].end,
                reused.segments[1].start,
                reused.segments[1].end
            ),
            bounds
        );
    }

    #[test]
    fn prepare_failed_resume_preserves_bounds_like_fresh_start() {
        // Failed mid-map → Resume uses the same helper and must not re-partition.
        let mut failed = sample_job();
        let map = two_seg_map(4096, 0);
        failed.segment_map = Some(map.clone());
        failed.transfer_format_version = 1;
        failed.total_bytes = map.total_bytes;
        failed.validators.expected_size = Some(map.total_bytes);

        let prepared = prepare_segment_map(&failed, map.total_bytes, 2).unwrap();
        match prepared {
            PreparedMap::Reuse(reused) => {
                assert_eq!(reused.segments[0].start, map.segments[0].start);
                assert_eq!(reused.segments[0].end, map.segments[0].end);
                assert_eq!(reused.segments[0].written, 4096);
                assert_eq!(reused.segments[1].written, 0);
            }
            PreparedMap::Fresh(_) => panic!("must not re-partition after Failed"),
        }
    }

    #[test]
    fn prepare_fresh_when_no_map() {
        let mut job = sample_job();
        job.total_bytes = 2 * MIN_SEGMENT_SIZE;
        let prepared = prepare_segment_map(&job, 2 * MIN_SEGMENT_SIZE, 2).unwrap();
        let PreparedMap::Fresh(map) = prepared else {
            panic!("expected fresh");
        };
        assert_eq!(map.segment_count, 2);
        assert!(all_written_zero(&map));
        assert!(!map.preallocated);
    }

    #[test]
    fn prepare_inconsistent_or_v1_missing_is_resume() {
        let mut job = sample_job();
        job.transfer_format_version = 1;
        let err = prepare_segment_map(&job, 1000, 2).unwrap_err();
        assert_eq!(err.category, FailureCategory::Resume);

        job.segment_map = Some(two_seg_map(0, 0));
        job.validators.expected_size = Some(99);
        let err = prepare_segment_map(&job, 2 * MIN_SEGMENT_SIZE, 2).unwrap_err();
        assert_eq!(err.category, FailureCategory::Resume);
    }

    #[test]
    fn convert_only_when_all_written_zero() {
        assert!(all_written_zero(&two_seg_map(0, 0)));
        assert!(may_convert_multi_to_single(&two_seg_map(0, 0)));
        assert!(!all_written_zero(&two_seg_map(1, 0)));
        assert!(!may_convert_multi_to_single(&two_seg_map(1, 0)));
        // Prefix-complete (first segment full, rest empty) is not a zero-write convert.
        assert!(!may_convert_multi_to_single(&two_seg_map(
            MIN_SEGMENT_SIZE,
            0
        )));
    }

    #[test]
    fn resume_oracle_legacy_and_map_errors() {
        let mut job = sample_job();
        job.downloaded_bytes = 10;
        assert_eq!(resume_oracle(&job), ResumeOracle::LegacySingle);
        assert_eq!(
            resume_oracle(&job).fallback_reason(),
            Some(FALLBACK_LEGACY_PARTIAL)
        );

        job.downloaded_bytes = 0;
        job.transfer_format_version = 1;
        assert_eq!(resume_oracle(&job), ResumeOracle::RestartRequired);
        assert!(resume_oracle(&job).is_resume_error());

        job.segment_map = Some(SegmentMap {
            total_bytes: 100,
            segment_count: 2,
            segments: vec![],
            preallocated: false,
        });
        assert_eq!(resume_oracle(&job), ResumeOracle::RestartRequired);
    }

    #[test]
    fn legacy_partial_blocks_multi() {
        let mut job = sample_job();
        job.downloaded_bytes = 10;
        job.transfer_format_version = 0;
        assert!(matches!(resume_oracle(&job), ResumeOracle::LegacySingle));
        job.downloaded_bytes = 0;
        assert!(matches!(resume_oracle(&job), ResumeOracle::FreshSingle));
        job.segment_map = Some(two_seg_map(10, 0));
        job.downloaded_bytes = 10;
        job.transfer_format_version = 1;
        assert!(matches!(resume_oracle(&job), ResumeOracle::Multi { .. }));
    }

    #[tokio::test]
    async fn map_present_progress_ignores_metadata_len() {
        let dir =
            std::env::temp_dir().join(format!("rusticdl-multi-meta-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("file.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        // Preallocated-style: file len == total.
        std::fs::write(&temp, vec![0u8; 10_000]).unwrap();

        let mut job = Job::new(
            "https://example.com/file.bin".into(),
            "file.bin".into(),
            target,
            temp,
        );
        job.transfer_format_version = 1;
        job.total_bytes = 10_000;
        job.downloaded_bytes = 9_999;
        job.segment_map = Some(SegmentMap {
            total_bytes: 10_000,
            segment_count: 2,
            segments: vec![
                Segment {
                    index: 0,
                    start: 0,
                    end: 4_999,
                    written: 100,
                    state: SegmentState::Active,
                },
                Segment {
                    index: 1,
                    start: 5_000,
                    end: 9_999,
                    written: 50,
                    state: SegmentState::Pending,
                },
            ],
            preallocated: true,
        });

        let result = reconcile_partial_progress(&mut job).await;
        assert!(result.used_map_sum);
        assert!(!result.used_metadata_len);
        assert_eq!(job.downloaded_bytes, 150);

        let prepared = prepare_segment_map(&job, 10_000, 8).unwrap();
        match prepared {
            PreparedMap::Reuse(map) => assert_eq!(map.written_sum(), 150),
            PreparedMap::Fresh(_) => panic!("must reuse"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn mock_multi_range_assembles_file() {
        let body: Vec<u8> = (0..2 * MIN_SEGMENT_SIZE as usize)
            .map(|i| (i % 251) as u8)
            .collect();
        let (base, seen, _handle) = spawn_range_server(body.clone(), RangeServeMode::Honest).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-multi-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/file.bin");
        let job = Job::new(url, "out.bin".into(), target.clone(), temp);

        let progress = TestProgress::new();
        let ctx = TransferContext::from_runtime(
            job,
            Arc::new(AtomicU8::new(0)),
            progress.callback(),
            None,
            GlobalBandwidthLimiter::new(None),
            ConnectionBudget::new(8, 4),
            progress.identity.clone(),
            &EngineRuntimeConfig {
                multi_min_bytes: 1,
                multi_max_segments: 2,
                ..Default::default()
            },
        );

        let outcome = run_transfer(ctx).await.expect("multi transfer");
        assert!(matches!(outcome, DownloadOutcome::Completed));

        let data = std::fs::read(&target).expect("final");
        assert_eq!(data, body);

        let seen = seen.lock().unwrap();
        let ranges: Vec<_> = seen
            .iter()
            .filter(|r| r.starts_with("GET ") && r.to_ascii_lowercase().contains("range: bytes="))
            .cloned()
            .collect();
        assert!(
            ranges.len() >= 2,
            "expected per-segment Range GETs, got {seen:?}"
        );

        let snaps = progress.snapshots();
        assert!(snaps
            .iter()
            .any(|job| job.transfer_mode == Some(TransferMode::Multi)));
        assert!(snaps
            .iter()
            .any(|job| job.transfer_format_version == 1 && job.segment_map.is_some()));
        let published = progress.events();
        let mid_downloading = published.iter().filter_map(|event| match event {
            TransferEvent::Tick(tick)
                if tick.state_hint == Some(ProgressHint::Downloading)
                    && tick.progress.is_some_and(|pct| pct < 100.0) =>
            {
                Some(tick)
            }
            _ => None,
        });
        for tick in mid_downloading {
            if let Some(bytes) = tick.downloaded_bytes {
                assert!(
                    bytes < body.len() as u64,
                    "pre-complete tick must not use file len ({bytes} >= {})",
                    body.len()
                );
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn resume_reuses_map_and_skips_completed_segment() {
        let total = 2 * MIN_SEGMENT_SIZE as usize;
        let body: Vec<u8> = (0..total).map(|i| (i % 199) as u8).collect();
        let (base, seen, _handle) = spawn_range_server(body.clone(), RangeServeMode::Honest).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-multi-rs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));

        // Simulate Failed mid-map: first segment fully written on disk.
        let mut part = vec![0u8; total];
        part[..MIN_SEGMENT_SIZE as usize].copy_from_slice(&body[..MIN_SEGMENT_SIZE as usize]);
        std::fs::write(&temp, &part).unwrap();

        let url = format!("{base}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target.clone(), temp);
        job.total_bytes = total as u64;
        job.transfer_format_version = 1;
        job.resume_supported = true;
        job.validators.expected_size = Some(total as u64);
        job.segment_map = Some(two_seg_map(MIN_SEGMENT_SIZE, 0));
        job.downloaded_bytes = MIN_SEGMENT_SIZE;

        let on_progress: TransferEventCallback = Arc::new(|_: TransferEvent| {});
        let ctx = test_ctx(job, on_progress, None, 2);

        let outcome = run_transfer(ctx).await.expect("resume multi");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).unwrap(), body);

        let seen = seen.lock().unwrap();
        let body_ranges: Vec<_> = seen
            .iter()
            .filter(|r| {
                let l = r.to_ascii_lowercase();
                l.starts_with("get ")
                    && l.contains("range: bytes=")
                    && !l.contains("range: bytes=0-0")
            })
            .cloned()
            .collect();
        // Must not re-fetch the completed first half (start 0 .. 1MiB-1).
        for req in &body_ranges {
            let lower = req.to_ascii_lowercase();
            assert!(
                !lower.contains(&format!("range: bytes=0-{}", MIN_SEGMENT_SIZE - 1)),
                "completed segment was re-fetched: {req}"
            );
        }
        assert!(
            body_ranges.iter().any(|r| r
                .to_ascii_lowercase()
                .contains(&format!("range: bytes={}-", MIN_SEGMENT_SIZE))),
            "expected remaining segment Range, got {body_ranges:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn handoff_cookie_applied_to_segment_gets() {
        let body: Vec<u8> = (0..64 * 1024).map(|i| (i % 17) as u8).collect();
        let (base, seen, _handle) = spawn_range_server(body.clone(), RangeServeMode::Honest).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-multi-ho-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/auth.bin");
        let job = Job::new(url, "out.bin".into(), target.clone(), temp);

        let auth = HandoffAuth {
            headers: vec![HandoffAuthHeader {
                name: "Cookie".into(),
                value: "sid=abc123".into(),
            }],
        };
        let on_progress: TransferEventCallback = Arc::new(|_: TransferEvent| {});
        let ctx = test_ctx(job, on_progress, Some(auth), 1);

        let outcome = run_transfer(ctx).await.expect("handoff multi");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).unwrap(), body);

        let seen = seen.lock().unwrap();
        let gets: Vec<_> = seen
            .iter()
            .filter(|r| {
                r.starts_with("GET ") && !r.to_ascii_lowercase().contains("range: bytes=0-0")
            })
            .cloned()
            .collect();
        assert!(!gets.is_empty(), "expected segment GET, got {seen:?}");
        for req in &gets {
            assert!(
                req.to_ascii_lowercase().contains("cookie: sid=abc123"),
                "segment GET missing handoff cookie: {req}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preallocate_hole_is_not_legacy_partial() {
        assert!(looks_like_preallocate_hole(0, 10_000, 10_000));
        assert!(!looks_like_preallocate_hole(100, 10_000, 10_000));
        assert!(!looks_like_preallocate_hole(0, 50, 10_000));
        // First-start crash: Add snapshot still has total=0.
        assert!(looks_like_preallocate_hole(0, 10_000, 0));
        assert!(!looks_like_preallocate_hole(0, 0, 0));
    }

    #[tokio::test]
    async fn unknown_total_hole_is_deleted_not_completed_as_zeros() {
        let body: Vec<u8> = (0..64 * 1024).map(|i| (i % 89) as u8).collect();
        let (base, _seen, _handle) = spawn_range_server(body.clone(), RangeServeMode::Honest).await;

        let dir =
            std::env::temp_dir().join(format!("rusticdl-multi-hole-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        // First-start crash: set_len already applied, state.json still total=0 / downloaded=0 / v0.
        std::fs::write(&temp, vec![0u8; body.len()]).unwrap();

        let url = format!("{base}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target.clone(), temp.clone());
        job.total_bytes = 0;
        job.downloaded_bytes = 0;
        job.transfer_format_version = 0;

        let on_progress: TransferEventCallback = Arc::new(|_: TransferEvent| {});
        let ctx = test_ctx(job, on_progress, None, 1);

        let outcome = run_transfer(ctx)
            .await
            .expect("hole must not complete as zeros");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        let data = std::fs::read(&target).expect("final");
        assert_eq!(data, body, "must re-download after deleting the hole");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn multi_sha256_match_renames() {
        let body: Vec<u8> = (0..2 * MIN_SEGMENT_SIZE as usize)
            .map(|i| (i % 251) as u8)
            .collect();
        let expected = sha256_hex(&body);
        let (base, _seen, _handle) = spawn_range_server(body.clone(), RangeServeMode::Honest).await;

        let dir =
            std::env::temp_dir().join(format!("rusticdl-multi-sha-ok-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target.clone(), temp.clone());
        job.expected_sha256 = Some(expected);

        let on_progress: TransferEventCallback = Arc::new(|_: TransferEvent| {});
        let ctx = test_ctx(job, on_progress, None, 2);

        let outcome = run_transfer(ctx).await.expect("hash match");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).unwrap(), body);
        assert!(!temp.exists(), "match must rename .part");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn multi_sha256_mismatch_keeps_part_and_map() {
        let body: Vec<u8> = (0..2 * MIN_SEGMENT_SIZE as usize)
            .map(|i| (i % 251) as u8)
            .collect();
        let (base, _seen, _handle) = spawn_range_server(body.clone(), RangeServeMode::Honest).await;

        let dir =
            std::env::temp_dir().join(format!("rusticdl-multi-sha-bad-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target.clone(), temp.clone());
        job.expected_sha256 = Some(SHA256_EMPTY.into());

        let progress = TestProgress::new();
        let ctx = test_ctx_commit(job, progress.callback(), None, 2, progress.identity.clone());

        let err = run_transfer(ctx)
            .await
            .expect_err("hash mismatch must fail");
        assert_eq!(err.category, FailureCategory::Internal);
        assert!(err.message.contains("SHA-256 mismatch"));
        assert!(temp.exists(), "hash fail must keep .part");
        assert!(!target.exists(), "hash fail must not rename");

        let snaps = progress.snapshots();
        let last_map = snaps.iter().rev().find_map(|job| job.segment_map.as_ref());
        let map = last_map.expect("hash fail must retain segment map");
        assert_eq!(map.written_sum(), body.len() as u64);
        assert!(snaps.iter().any(|job| job.transfer_format_version == 1));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn two_hundred_on_nonzero_segment_falls_back_to_single() {
        let total = 2 * MIN_SEGMENT_SIZE as usize;
        let body: Vec<u8> = (0..total).map(|i| (i % 211) as u8).collect();
        let (base, _seen, _handle) =
            spawn_range_server(body.clone(), RangeServeMode::FullBodyOnNonzeroRange).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-multi-200-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/file.bin");
        let job = Job::new(url, "out.bin".into(), target.clone(), temp.clone());

        let progress = TestProgress::new();
        let ctx = test_ctx_commit(job, progress.callback(), None, 2, progress.identity.clone());

        let outcome = run_transfer(ctx)
            .await
            .expect("ignored Range must fall back to single-stream");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).expect("final file"), body);

        let snaps = progress.snapshots();
        // Mid-probe GET bytes=1-1 sees the ignored Range (200) and stays single
        // before workers start — do not enter multi then convert.
        assert!(snaps.iter().any(|job| {
            job.fallback_reason.as_deref() == Some("ranges_unsupported")
                && job.transfer_mode == Some(TransferMode::Single)
        }));
        assert!(progress.events().iter().any(|event| {
            matches!(
                event,
                TransferEvent::Toast(msg) if msg
                    == "Multi-connection unavailable for this large file; using a single connection."
            )
        }));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn range_ignored_on_resume_salvages_prefix() {
        let total = 2 * MIN_SEGMENT_SIZE as usize;
        let body: Vec<u8> = (0..total).map(|i| (i % 193) as u8).collect();
        let (base, _seen, _handle) =
            spawn_range_server(body.clone(), RangeServeMode::FullBodyOnNonzeroRange).await;

        let dir =
            std::env::temp_dir().join(format!("rusticdl-multi-200r-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));

        let mut part = vec![0u8; total];
        part[..MIN_SEGMENT_SIZE as usize].copy_from_slice(&body[..MIN_SEGMENT_SIZE as usize]);
        std::fs::write(&temp, &part).unwrap();

        let url = format!("{base}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target.clone(), temp.clone());
        job.total_bytes = total as u64;
        job.transfer_format_version = 1;
        job.resume_supported = true;
        job.validators.expected_size = Some(total as u64);
        job.segment_map = Some(two_seg_map(MIN_SEGMENT_SIZE, 0));
        job.downloaded_bytes = MIN_SEGMENT_SIZE;

        let progress = TestProgress::new();
        let ctx = test_ctx_commit(job, progress.callback(), None, 2, progress.identity.clone());

        let err = run_transfer(ctx)
            .await
            .expect_err("Range ignored after written > 0 must be Resume, not convert");
        assert_eq!(err.category, FailureCategory::Resume);
        assert_eq!(err.message, RANGE_IGNORED_MESSAGE);
        assert!(!target.exists(), "must not complete as single-stream");
        assert_eq!(
            std::fs::metadata(&temp).map(|m| m.len()).unwrap_or(0),
            total as u64,
            ".part must stay preallocated length; no set_len(prefix)"
        );

        let snaps = progress.snapshots();
        assert!(
            snaps.iter().all(|job| job.segment_map.is_some()),
            "map must be retained, not cleared"
        );
        assert!(
            snaps.iter().all(|job| job.transfer_format_version != 0),
            "must not roll back version after written > 0"
        );
        assert!(
            !snaps.iter().any(|job| {
                job.fallback_reason.as_deref() == Some("multi_resume_fallback")
                    || job.transfer_mode == Some(TransferMode::Single)
            }),
            "must not enter single-stream fallback"
        );
        let last_map = snaps
            .iter()
            .rev()
            .find_map(|job| job.segment_map.clone())
            .expect("map must stay published");
        assert_eq!(last_map.segments[0].written, MIN_SEGMENT_SIZE);
        assert_eq!(last_map.segments[1].written, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn convert_to_single_only_when_written_zero_removes_prealloc() {
        let total = 2 * MIN_SEGMENT_SIZE as usize;
        let body: Vec<u8> = vec![0xABu8; total];
        let (base, _seen, _handle) = spawn_range_server(body, RangeServeMode::ForbiddenBody).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-multi-403-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/file.bin");
        let job = Job::new(url, "out.bin".into(), target, temp.clone());

        let on_progress: TransferEventCallback = Arc::new(|_: TransferEvent| {});
        let ctx = test_ctx(job, on_progress, None, 2);

        let result = run_transfer(ctx).await;
        assert!(result.is_err(), "403 should fail after fallback");
        if temp.exists() {
            let len = std::fs::metadata(&temp).map(|m| m.len()).unwrap_or(0);
            assert_ne!(
                len, total as u64,
                "preallocated hole must not remain for single-stream metadata_len"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn convert_to_single_publishes_live_connection_and_one_shot_toast() {
        let total = 2 * MIN_SEGMENT_SIZE as usize;
        let body: Vec<u8> = vec![0xABu8; total];
        let (base, _seen, _handle) = spawn_range_server(body, RangeServeMode::ForbiddenBody).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-multi-fb-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/file.bin");
        let job = Job::new(url, "out.bin".into(), target, temp);

        let progress = TestProgress::new();
        let ctx = test_ctx_commit(job, progress.callback(), None, 2, progress.identity.clone());

        let result = run_transfer(ctx).await;
        assert!(result.is_err(), "403 should fail after fallback");

        let snaps = progress.snapshots();
        let rollback = snaps
            .iter()
            .find(|job| job.fallback_reason.as_deref() == Some("multi_http_fallback"))
            .expect("convert should publish fallback_reason");
        assert_eq!(rollback.transfer_mode, Some(TransferMode::Single));
        let events = progress.events();
        assert!(
            events.iter().any(|event| matches!(
                event,
                TransferEvent::Tick(tick) if tick.active_connections == Some(1)
            )),
            "continuing as single must keep a live connection"
        );
        let toasts: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                TransferEvent::Toast(msg) => Some(msg.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(toasts.len(), 1, "convert toast must be one-shot");
        assert_eq!(toasts[0], MULTI_FALLBACK_TOAST);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn convert_retry_same_reason_skips_toast() {
        let total = 2 * MIN_SEGMENT_SIZE as usize;
        let body: Vec<u8> = vec![0xABu8; total];
        let (base, _seen, _handle) = spawn_range_server(body, RangeServeMode::ForbiddenBody).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-multi-fb2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target, temp);
        job.fallback_reason = Some("multi_http_fallback".into());

        let progress = TestProgress::new();
        let ctx = test_ctx_commit(job, progress.callback(), None, 2, progress.identity.clone());

        let _ = run_transfer(ctx).await;

        let events = progress.events();
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, TransferEvent::Toast(_))),
            "same convert reason must not re-toast"
        );
        assert!(progress
            .snapshots()
            .iter()
            .any(|job| job.fallback_reason.as_deref() == Some("multi_http_fallback")));
        assert!(events.iter().any(|event| matches!(
            event,
            TransferEvent::Tick(tick) if tick.active_connections == Some(1)
        )));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fail_before_workers_zeros_connections_without_toast() {
        let total = 2 * MIN_SEGMENT_SIZE as usize;
        let body: Vec<u8> = vec![0xABu8; total];
        let (base, _seen, _handle) = spawn_range_server(body, RangeServeMode::Honest).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-multi-ff-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Parent is a file so ensure_parent_directory fails after map attach.
        let blocker = dir.join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let target = blocker.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/file.bin");
        let job = Job::new(url, "out.bin".into(), target, temp);

        let progress = TestProgress::new();
        let ctx = test_ctx_commit(job, progress.callback(), None, 2, progress.identity.clone());

        let result = run_transfer(ctx).await;
        assert!(result.is_err(), "start checklist should fail");

        assert!(progress
            .snapshots()
            .iter()
            .any(|job| job.fallback_reason.as_deref() == Some("multi_start_failed")));
        let events = progress.events();
        assert!(events.iter().any(|event| matches!(
            event,
            TransferEvent::Tick(tick) if tick.active_connections == Some(0)
        )));
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, TransferEvent::Toast(_))),
            "hard fail must not toast"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn non_prefix_failure_keeps_map() {
        // First segment already written (prefix complete). Remaining segment fails.
        // Must retain the map — no multi→single conversion, no invented Range.
        let total = 2 * MIN_SEGMENT_SIZE as usize;
        let body: Vec<u8> = (0..total).map(|i| (i % 173) as u8).collect();
        let (base, _seen, _handle) =
            spawn_range_server(body.clone(), RangeServeMode::FailNonPrefix).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-multi-np-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));

        let mut part = vec![0u8; total];
        part[..MIN_SEGMENT_SIZE as usize].copy_from_slice(&body[..MIN_SEGMENT_SIZE as usize]);
        std::fs::write(&temp, &part).unwrap();

        let url = format!("{base}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target, temp.clone());
        job.total_bytes = total as u64;
        job.transfer_format_version = 1;
        job.resume_supported = true;
        job.validators.expected_size = Some(total as u64);
        job.segment_map = Some(two_seg_map(MIN_SEGMENT_SIZE, 0));
        job.downloaded_bytes = MIN_SEGMENT_SIZE;

        let progress = TestProgress::new();
        let ctx = test_ctx_commit(job, progress.callback(), None, 2, progress.identity.clone());

        let err = run_transfer(ctx)
            .await
            .expect_err("non-prefix failure must not convert to single");
        assert_ne!(
            err.category,
            FailureCategory::Internal,
            "expected transfer error, got {err:?}"
        );

        let snaps = progress.snapshots();
        assert!(
            snaps.iter().all(|job| job.segment_map.is_some()),
            "must not clear the map on non-prefix failure"
        );
        assert!(
            snaps.iter().all(|job| job.transfer_format_version != 0),
            "must not roll back version on non-prefix failure"
        );
        let last_map = snaps
            .iter()
            .rev()
            .find_map(|job| job.segment_map.clone())
            .expect("map must stay published");
        assert_eq!(last_map.segments[0].start, 0);
        assert_eq!(last_map.segments[0].end, MIN_SEGMENT_SIZE - 1);
        assert_eq!(last_map.segments[0].written, MIN_SEGMENT_SIZE);
        assert_eq!(last_map.segments[1].start, MIN_SEGMENT_SIZE);
        assert_eq!(last_map.segments[1].end, total as u64 - 1);
        assert!(
            !snaps.iter().any(|job| {
                job.fallback_reason.as_deref() == Some("multi_http_fallback")
                    || job.fallback_reason.as_deref() == Some("multi_network_fallback")
            }),
            "convert fallback_reason must not be set when written > 0"
        );
        assert!(temp.exists(), "non-prefix failure must keep the .part");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[derive(Clone, Copy)]
    enum RangeServeMode {
        Honest,
        /// 200 + full entity when Range start > 0 (ignored Range / If-Range mismatch).
        FullBodyOnNonzeroRange,
        /// Body GETs return 403 (convert-to-single path).
        ForbiddenBody,
        /// Fail Range requests that start at/after the first segment (prefix already done).
        FailNonPrefix,
    }

    async fn spawn_range_server(
        body: Vec<u8>,
        mode: RangeServeMode,
    ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_task = seen.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 16 * 1024];
                let mut collected = Vec::new();
                loop {
                    let n = match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    collected.extend_from_slice(&buf[..n]);
                    if collected.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&collected).to_string();
                seen_task.lock().unwrap().push(req.clone());

                if req.starts_with("HEAD ") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: {}\r\n\
\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }

                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }

                if matches!(mode, RangeServeMode::ForbiddenBody) {
                    let reply = "HTTP/1.1 403 Forbidden\r\n\
Connection: close\r\n\
Content-Length: 0\r\n\
\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }

                let range = parse_test_range(&req);
                if matches!(mode, RangeServeMode::FailNonPrefix) {
                    if range.is_some_and(|(start, _)| start >= MIN_SEGMENT_SIZE) {
                        let reply = "HTTP/1.1 416 Range Not Satisfiable\r\n\
Connection: close\r\n\
Content-Length: 0\r\n\
\r\n";
                        let _ = socket.write_all(reply.as_bytes()).await;
                        let _ = socket.shutdown().await;
                        continue;
                    }
                }

                let (start, end) = match range {
                    Some((start, Some(end))) => (start as usize, end as usize),
                    Some((start, None)) => (start as usize, body.len().saturating_sub(1)),
                    None => (0, body.len().saturating_sub(1)),
                };
                let end = end.min(body.len().saturating_sub(1));
                let start = start.min(end + 1);

                if matches!(mode, RangeServeMode::FullBodyOnNonzeroRange) && start > 0 {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Content-Length: {}\r\n\
\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                    let _ = socket.shutdown().await;
                    continue;
                }

                let slice = if start < body.len() {
                    &body[start..=end]
                } else {
                    &[]
                };
                let reply = format!(
                    "HTTP/1.1 206 Partial Content\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Range: bytes {start}-{end}/{}\r\n\
Content-Length: {}\r\n\
\r\n",
                    body.len(),
                    slice.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(slice).await;
                let _ = socket.shutdown().await;
            }
        });
        (format!("http://{addr}"), seen, handle)
    }

    fn parse_test_range(req: &str) -> Option<(u64, Option<u64>)> {
        let lower = req.to_ascii_lowercase();
        let line = lower.lines().find(|l| l.starts_with("range:"))?;
        let spec = line.split_once(':')?.1.trim();
        let spec = spec.strip_prefix("bytes=")?;
        let (start, end) = spec.split_once('-')?;
        let start = start.parse().ok()?;
        let end = if end.is_empty() {
            None
        } else {
            Some(end.parse().ok()?)
        };
        Some((start, end))
    }
}
