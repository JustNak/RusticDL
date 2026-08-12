use futures_util::StreamExt;
use reqwest::header::{
    ACCEPT_ENCODING, ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_RANGE, ETAG, IF_RANGE,
    LAST_MODIFIED, RANGE, REFERER,
};
use reqwest::{Client, StatusCode, Version};
use std::error::Error as StdError;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio::sync::Notify;
use tokio::time::sleep;

use super::bandwidth::GlobalBandwidthLimiter;
use super::client::{download_client, referer_for_url};
use super::conn_budget::ConnectionBudget;
use super::filesystem::{
    ensure_parent_directory, metadata_len, move_to_final_path, parse_content_disposition_filename,
    parse_content_range, sanitize_filename,
};
use super::handoff::{handoff_auth_for_request_url, is_allowed_handoff_header, HandoffAuth};
use super::job::{
    download_error, ContentValidators, DownloadError, DownloadOutcome, FailureCategory, Job,
    TransferMode, WorkerControl,
};
use super::segment::SegmentMap;
use super::verify::verify_sha256_if_expected;

const PROGRESS_INTERVAL: Duration = Duration::from_millis(400);
const CONTROL_POLL: Duration = Duration::from_millis(200);
/// Timeout for HEAD / Range 0-0 preflight probes (shared with `preflight`).
pub(crate) const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(8);
pub(crate) const MAX_REDIRECTS: u32 = 10;

/// Nested mid-transfer reconnect before worker-level `RETRY_DELAYS`.
pub(crate) const RECONNECT_MAX: u32 = 5;
pub(crate) const RECONNECT_BASE: Duration = Duration::from_millis(200);
pub(crate) const RECONNECT_CAP: Duration = Duration::from_secs(2);

const CONTROL_CONTINUE: u8 = 0;
const CONTROL_PAUSED: u8 = 1;
const CONTROL_CANCELED: u8 = 2;

/// Partial progress patch. `None` = leave the field unchanged on apply/merge.
/// Structured fields (`validators`, metrics) stay sparse `None` on speed ticks so
/// coalesce never clears them.
#[derive(Debug, Clone, Default)]
pub struct ProgressUpdate {
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub speed: Option<u64>,
    pub eta_secs: Option<u64>,
    pub progress: Option<f64>,
    pub filename: Option<String>,
    pub target_path: Option<std::path::PathBuf>,
    pub temp_path: Option<std::path::PathBuf>,
    pub resume_supported: Option<bool>,
    pub state_hint: Option<ProgressHint>,
    pub validators: Option<ContentValidators>,
    /// When `Some(true)`, `validators` **replaces** job identity (not `merge_present`).
    /// Used on 200 full-replace so stale ETags cannot thrash subsequent resumes.
    pub replace_validators: Option<bool>,
    pub transfer_format_version: Option<u32>,
    pub active_connections: Option<u32>,
    pub reconnect_count: Option<u32>,
    pub transfer_mode: Option<TransferMode>,
    pub fallback_reason: Option<String>,
    /// One-shot UI toast (engine emits `EngineEvent::Toast`; not stored on Job).
    pub toast: Option<String>,
    /// None = unchanged. Speed ticks must not clear a stored map.
    pub segment_map: Option<SegmentMap>,
    /// When `Some(true)`, clear `job.segment_map` (multi→single rollback).
    pub clear_segment_map: Option<bool>,
    /// Engine `apply_progress` notifies this after the patch is applied (or skipped).
    pub persist_ack: Option<Arc<Notify>>,
}

const FULL_REPLACE_NOTICE: &str =
    "Remote file changed or server ignored resume; restarting download from the beginning.";

impl ProgressUpdate {
    /// Merge two patches in order: `later` wins on `Some` fields (`later.or(self)`).
    pub fn merge(self, later: Self) -> Self {
        Self {
            downloaded_bytes: later.downloaded_bytes.or(self.downloaded_bytes),
            total_bytes: later.total_bytes.or(self.total_bytes),
            speed: later.speed.or(self.speed),
            eta_secs: later.eta_secs.or(self.eta_secs),
            progress: later.progress.or(self.progress),
            filename: later.filename.or(self.filename),
            target_path: later.target_path.or(self.target_path),
            temp_path: later.temp_path.or(self.temp_path),
            resume_supported: later.resume_supported.or(self.resume_supported),
            state_hint: later.state_hint.or(self.state_hint),
            validators: later.validators.or(self.validators),
            replace_validators: later.replace_validators.or(self.replace_validators),
            transfer_format_version: later
                .transfer_format_version
                .or(self.transfer_format_version),
            active_connections: later.active_connections.or(self.active_connections),
            reconnect_count: later.reconnect_count.or(self.reconnect_count),
            transfer_mode: later.transfer_mode.or(self.transfer_mode),
            fallback_reason: later.fallback_reason.or(self.fallback_reason),
            toast: later.toast.or(self.toast),
            segment_map: later.segment_map.or(self.segment_map),
            clear_segment_map: later.clear_segment_map.or(self.clear_segment_map),
            persist_ack: later.persist_ack.or(self.persist_ack),
        }
    }

    /// Starting metadata tick (paths/filename/resume/validators + zero speed).
    pub fn starting_tick(
        downloaded: u64,
        total: u64,
        filename: Option<String>,
        target_path: Option<std::path::PathBuf>,
        temp_path: Option<std::path::PathBuf>,
        resume_supported: Option<bool>,
        validators: Option<ContentValidators>,
    ) -> Self {
        Self {
            downloaded_bytes: Some(downloaded),
            total_bytes: Some(total),
            speed: Some(0),
            eta_secs: Some(0),
            progress: Some(progress_percent(downloaded, total)),
            filename,
            target_path,
            temp_path,
            resume_supported,
            state_hint: Some(ProgressHint::Starting),
            validators,
            replace_validators: None,
            transfer_format_version: None,
            active_connections: Some(1),
            reconnect_count: None,
            transfer_mode: None,
            fallback_reason: None,
            toast: None,
            segment_map: None,
            clear_segment_map: None,
            persist_ack: None,
        }
    }

    /// Periodic downloading scalar tick (no path/filename/validator changes).
    pub fn downloading_tick(
        downloaded: u64,
        total: u64,
        speed: u64,
        eta: u64,
        progress: f64,
    ) -> Self {
        Self {
            downloaded_bytes: Some(downloaded),
            total_bytes: Some(total),
            speed: Some(speed),
            eta_secs: Some(eta),
            progress: Some(progress),
            filename: None,
            target_path: None,
            temp_path: None,
            resume_supported: None,
            state_hint: Some(ProgressHint::Downloading),
            validators: None,
            replace_validators: None,
            transfer_format_version: None,
            active_connections: None,
            reconnect_count: None,
            transfer_mode: None,
            fallback_reason: None,
            toast: None,
            segment_map: None,
            clear_segment_map: None,
            persist_ack: None,
        }
    }

    /// Final completion patch (100%, zero speed, final paths).
    pub fn completed_tick(
        downloaded: u64,
        total: u64,
        filename: Option<String>,
        target_path: Option<std::path::PathBuf>,
        temp_path: Option<std::path::PathBuf>,
        resume_supported: Option<bool>,
    ) -> Self {
        Self {
            downloaded_bytes: Some(downloaded),
            total_bytes: Some(total),
            speed: Some(0),
            eta_secs: Some(0),
            progress: Some(100.0),
            filename,
            target_path,
            temp_path,
            resume_supported,
            state_hint: Some(ProgressHint::Downloading),
            validators: None,
            replace_validators: None,
            transfer_format_version: None,
            active_connections: None,
            reconnect_count: None,
            transfer_mode: None,
            fallback_reason: None,
            toast: None,
            segment_map: None,
            clear_segment_map: None,
            persist_ack: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressHint {
    Starting,
    Downloading,
}

pub type ProgressCallback = Arc<dyn Fn(ProgressUpdate) + Send + Sync>;

/// Default multi min-size used when a context is built without engine settings.
pub const DEFAULT_MULTI_MIN_BYTES: u64 = 5 * 1024 * 1024;

/// Attempt-scoped transfer inputs shared by preflight, single-stream, reconnect, and
/// (later) multi-segment workers. `resolved_url` is pinned after a successful redirect
/// chain so subsequent requests do not re-walk independent redirect races.
#[derive(Clone)]
pub struct TransferContext {
    pub job: Job,
    pub control: Arc<AtomicU8>,
    pub on_progress: ProgressCallback,
    /// Memory-only browser session headers (snapshot from `EngineInner`).
    pub handoff_auth: Option<HandoffAuth>,
    pub limiter: Arc<GlobalBandwidthLimiter>,
    /// Planner input: master switch for multi-segment routing.
    pub multi_connection_enabled: bool,
    /// Planner input: files smaller than this never qualify for multi.
    pub multi_min_bytes: u64,
    /// Fresh multi partition cap (clamped 1–16 by settings).
    pub multi_max_segments: u32,
    /// Process-wide HTTP body budget (global + per-host).
    pub conn_budget: Arc<ConnectionBudget>,
    /// Attempt-local; updated only when redirect follow succeeds. Init = `job.url`.
    pub resolved_url: String,
    /// Set after a preflight attempt this run so `run_transfer` + single do not double-probe.
    pub preflight_done: bool,
    /// Flush + `sync_data` on pause (from `EngineRuntimeConfig`).
    pub fsync_on_pause: bool,
}

impl TransferContext {
    pub fn new(
        job: Job,
        control: Arc<AtomicU8>,
        on_progress: ProgressCallback,
        handoff_auth: Option<HandoffAuth>,
        limiter: Arc<GlobalBandwidthLimiter>,
    ) -> Self {
        let resolved_url = job.url.clone();
        Self {
            job,
            control,
            on_progress,
            handoff_auth,
            limiter,
            multi_connection_enabled: true,
            multi_min_bytes: DEFAULT_MULTI_MIN_BYTES,
            multi_max_segments: super::segment::DEFAULT_SEGMENT_COUNT,
            conn_budget: ConnectionBudget::new(32, 8),
            resolved_url,
            preflight_done: false,
            fsync_on_pause: false,
        }
    }
}

pub async fn run_http_download(
    job: &Job,
    limiter: Arc<GlobalBandwidthLimiter>,
    control: Arc<AtomicU8>,
    on_progress: ProgressCallback,
    handoff_auth: Option<&HandoffAuth>,
    fsync_on_pause: bool,
) -> Result<DownloadOutcome, DownloadError> {
    let mut ctx = TransferContext::new(
        job.clone(),
        control,
        on_progress,
        handoff_auth.cloned(),
        limiter,
    );
    ctx.fsync_on_pause = fsync_on_pause;
    run_http_download_with_ctx(&mut ctx).await
}

/// Single-stream transfer using an attempt-local [`TransferContext`] (URL pin + handoff).
pub async fn run_http_download_with_ctx(
    ctx: &mut TransferContext,
) -> Result<DownloadOutcome, DownloadError> {
    ensure_parent_directory(&ctx.job.target_path)
        .await
        .map_err(|message| download_error(FailureCategory::Disk, message, false))?;

    let client = download_client()?;

    // Best-effort preflight: skipped when `run_transfer` already probed this attempt.
    apply_preflight(ctx).await;

    if let Some(outcome) = control_outcome(&ctx.control) {
        return Ok(outcome);
    }

    // Skip metadata_len when map/v1 — preallocate would lie; never invent a Range.
    let mut existing_bytes = match resume_decision(&ctx.job) {
        ResumeDecision::NeedDiskLen => metadata_len(&ctx.job.temp_path).await.unwrap_or(0),
        ResumeDecision::Contiguous { offset } => offset,
        ResumeDecision::MultiMap => {
            return Err(download_error(
                FailureCategory::Resume,
                "Multi-connection partial; single-stream resume is not supported. Use Restart."
                    .into(),
                false,
            ));
        }
        ResumeDecision::RestartRequired => {
            return Err(download_error(
                FailureCategory::Resume,
                "Multi-part incomplete; Restart required.".into(),
                false,
            ));
        }
    };

    let job_url = ctx.job.url.clone();
    // Pinned resolved URL (updated after redirects); handoff still keyed off job_url.
    let mut current_url = ctx.resolved_url.clone();
    let mut validators = ctx.job.validators.clone();
    let mut target_path = ctx.job.target_path.clone();
    let mut temp_path = ctx.job.temp_path.clone();
    let mut filename = ctx.job.filename.clone();
    // Mutated on full-replace so reconnect oracle matches the progress patch (v1+ → 0).
    let mut transfer_format_version = ctx.job.transfer_format_version;
    let mut total_bytes: u64;
    let mut resume_supported = ctx.job.resume_supported;

    // Short reconnect budget resets each `run_http_download_with_ctx` call (worker
    // long-retry does not share the budget). Cumulative job counter is preserved.
    let mut short_reconnects: u32 = 0;
    let mut cumulative_reconnects = ctx.job.reconnect_count;
    let reconnect_baseline = ctx.job.reconnect_count;

    let control = ctx.control.clone();
    let on_progress = ctx.on_progress.clone();
    let handoff_auth = ctx.handoff_auth.clone();
    let limiter = ctx.limiter.clone();

    loop {
        if let Some(outcome) = control_outcome(&control) {
            return Ok(outcome);
        }

        let fetch_result = fetch_with_redirects(
            &client,
            &job_url,
            &current_url,
            existing_bytes,
            &validators,
            &control,
            handoff_auth.as_ref(),
        )
        .await;

        let (response, final_url) = match fetch_result {
            Ok(pair) => pair,
            Err(error) => {
                // Pause/cancel during reconnect GET (fetch_with_redirects wraps as Internal).
                if let Some(outcome) = control_outcome(&control) {
                    return Ok(outcome);
                }
                // Connect errors on reconnect GET only (not the first attempt).
                match prepare_reconnect(
                    &error,
                    /*is_fetch_phase=*/ true,
                    short_reconnects,
                    existing_bytes,
                    resume_supported,
                    transfer_format_version,
                    &temp_path,
                    &control,
                    &on_progress,
                    &mut cumulative_reconnects,
                )
                .await
                {
                    ReconnectAction::Retry { offset } => {
                        short_reconnects += 1;
                        existing_bytes = offset;
                        continue;
                    }
                    ReconnectAction::Control(outcome) => return Ok(outcome),
                    ReconnectAction::GiveUp => return Err(error),
                }
            }
        };
        current_url = final_url;
        ctx.resolved_url = current_url.clone();

        if let Some(outcome) = control_outcome(&control) {
            return Ok(outcome);
        }

        let status = response.status();
        // 416 Range Not Satisfiable — non-retryable without Restart.
        if status == StatusCode::RANGE_NOT_SATISFIABLE && existing_bytes > 0 {
            return Err(download_error(
                FailureCategory::Resume,
                format!(
                    "Server rejected resume at {existing_bytes} bytes. Use Restart to download from zero."
                ),
                false,
            ));
        }

        if !(status.is_success() || status == StatusCode::PARTIAL_CONTENT) {
            let retryable = should_retry_status(status);
            let mut message = format!("Download failed with HTTP {status}.");
            if status == StatusCode::BAD_GATEWAY || status == StatusCode::SERVICE_UNAVAILABLE {
                message.push_str(
                    " The CDN/origin rejected this request — often a bad or glued download token/URL, \
or a temporary gateway issue. Confirm the full URL is a single link (not two pasted together).",
                );
            } else if status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED {
                message.push_str(
                    " Access denied — the link may require a browser session, cookies, or a fresh token.",
                );
            } else if status == StatusCode::NOT_FOUND {
                message.push_str(" File not found — the link may have expired.");
            }
            // Retryable HTTP on reconnect GET can use short budget; first attempt bubbles.
            let error = download_error(FailureCategory::Http, message, retryable);
            if let Some(outcome) = control_outcome(&control) {
                return Ok(outcome);
            }
            if retryable {
                match prepare_reconnect(
                    &error,
                    /*is_fetch_phase=*/ true,
                    short_reconnects,
                    existing_bytes,
                    resume_supported,
                    transfer_format_version,
                    &temp_path,
                    &control,
                    &on_progress,
                    &mut cumulative_reconnects,
                )
                .await
                {
                    ReconnectAction::Retry { offset } => {
                        short_reconnects += 1;
                        existing_bytes = offset;
                        continue;
                    }
                    ReconnectAction::Control(outcome) => return Ok(outcome),
                    ReconnectAction::GiveUp => return Err(error),
                }
            }
            return Err(error);
        }

        resume_supported = status == StatusCode::PARTIAL_CONTENT
            || response
                .headers()
                .get(ACCEPT_RANGES)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.to_ascii_lowercase().contains("bytes"))
            || resume_supported;

        // 200 with partial on disk: full replace (Range ignored or If-Range entity changed).
        let mut full_replace = false;
        if existing_bytes > 0 && status != StatusCode::PARTIAL_CONTENT {
            existing_bytes = 0;
            full_replace = true;
            let _ = tokio::fs::remove_file(&temp_path).await;
        }

        // Parse Content-Range once (206 resume + optional * total).
        let parsed_range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range);

        // 206 resume: require parseable Content-Range whose start matches expected offset
        // (fail-closed — RFC requires Content-Range on 206; misaligned append is worse).
        if status == StatusCode::PARTIAL_CONTENT && existing_bytes > 0 {
            match parsed_range {
                Some((start, _end, _total)) if start == existing_bytes => {
                    let incoming = content_validators_from_headers(response.headers(), 0);
                    if resume_identity_mismatch(&validators, &incoming) {
                        return Err(download_error(
                            FailureCategory::Resume,
                            "Remote content identity changed (ETag or Last-Modified). Use Restart."
                                .into(),
                            false,
                        ));
                    }
                }
                Some((start, _end, _total)) => {
                    return Err(download_error(
                        FailureCategory::Resume,
                        format!(
                            "Unexpected resume range (got start {start}, expected {existing_bytes}). Use Restart."
                        ),
                        false,
                    ));
                }
                None => {
                    return Err(download_error(
                        FailureCategory::Resume,
                        "Missing or invalid Content-Range on partial response. Use Restart.".into(),
                        false,
                    ));
                }
            }
        }

        // Numeric Content-Range total vs stored expected_size (ignore * totals).
        // Skip expected_size check after full replace — identity is being rebuilt.
        let range_total = parsed_range.and_then(|(_s, _e, total)| total);
        if !full_replace {
            if let Some((total, expected)) =
                content_range_size_mismatch(range_total, validators.expected_size)
            {
                return Err(download_error(
                    FailureCategory::Resume,
                    format!(
                        "Remote size changed ({total} bytes vs expected {expected}). Use Restart."
                    ),
                    false,
                ));
            }
        }

        total_bytes = response
            .content_length()
            .map(|len| {
                if status == StatusCode::PARTIAL_CONTENT {
                    existing_bytes.saturating_add(len)
                } else {
                    len
                }
            })
            .unwrap_or(0);

        // Prefer numeric Content-Range total when present; `*` leaves content-length math.
        if let Some((_start, _end, Some(total))) = parsed_range {
            total_bytes = total;
        }

        // Capture validators for this response; keep local copy for reconnect If-Range.
        // Full replace: snapshot replaces identity; else field-wise merge.
        let validators_patch = if full_replace {
            let snap = content_validators_from_headers(response.headers(), total_bytes);
            validators = snap.clone();
            Some(snap)
        } else {
            let patch = content_validators_patch(response.headers(), total_bytes);
            if let Some(ref p) = patch {
                validators.merge_present(p.clone());
            }
            patch
        };

        // Filename / path discovery (first response or when still generic).
        if let Some(header_name) = response
            .headers()
            .get(CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_disposition_filename)
        {
            if filename == "download.bin"
                || filename_from_url_fallback(&job_url).as_deref() == Some(filename.as_str())
            {
                filename = header_name;
                if let Some(parent) = target_path.parent() {
                    let new_target = parent.join(&filename);
                    let new_temp = super::filesystem::temp_path_for(&new_target);
                    if temp_path != new_temp && temp_path.exists() {
                        let _ = tokio::fs::rename(&temp_path, &new_temp).await;
                    }
                    target_path = new_target;
                    temp_path = new_temp;
                }
            }
        } else if let Some(from_final) = filename_from_response_url(&current_url) {
            if filename == "download.bin" {
                filename = from_final;
                if let Some(parent) = target_path.parent() {
                    let new_target = parent.join(&filename);
                    let new_temp = super::filesystem::temp_path_for(&new_target);
                    if temp_path != new_temp && temp_path.exists() {
                        let _ = tokio::fs::rename(&temp_path, &new_temp).await;
                    }
                    target_path = new_target;
                    temp_path = new_temp;
                }
            }
        }

        let mut starting = ProgressUpdate::starting_tick(
            existing_bytes,
            total_bytes,
            Some(filename.clone()),
            Some(target_path.clone()),
            Some(temp_path.clone()),
            Some(resume_supported),
            validators_patch,
        );
        if full_replace {
            // Replace identity + clear multi version; surface a non-fatal user notice.
            // Keep local oracle in sync with the Job progress patch (v1+ → contiguous v0).
            transfer_format_version = 0;
            starting.replace_validators = Some(true);
            starting.transfer_format_version = Some(0);
            starting.toast = Some(FULL_REPLACE_NOTICE.into());
        }
        if cumulative_reconnects > reconnect_baseline {
            starting.reconnect_count = Some(cumulative_reconnects);
        }
        on_progress(starting);

        ensure_parent_directory(&temp_path)
            .await
            .map_err(|message| download_error(FailureCategory::Disk, message, false))?;

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(existing_bytes == 0)
            .open(&temp_path)
            .await
            .map_err(|error| {
                download_error(
                    FailureCategory::Disk,
                    format!("Could not open partial download file: {error}"),
                    false,
                )
            })?;

        let mut writer = BufWriter::with_capacity(1024 * 1024, file);
        if existing_bytes > 0 {
            writer
                .seek(std::io::SeekFrom::Start(existing_bytes))
                .await
                .map_err(|error| {
                    download_error(
                        FailureCategory::Disk,
                        format!("Could not seek partial download file: {error}"),
                        false,
                    )
                })?;
        }

        let mut downloaded = existing_bytes;
        let mut stream = response.bytes_stream();
        let mut last_progress = Instant::now();
        let mut window_start = Instant::now();
        let mut window_bytes: u64 = 0;

        on_progress(ProgressUpdate::downloading_tick(
            downloaded,
            total_bytes,
            0,
            0,
            progress_percent(downloaded, total_bytes),
        ));

        // Body read loop — body/network errors trigger mid-transfer reconnect.
        let body_result: Result<DownloadOutcome, DownloadError> = async {
            loop {
                if let Some(outcome) = control_outcome(&control) {
                    flush_partial_writer(&mut writer, fsync_on_pause, outcome).await?;
                    emit_control_exit_progress(&on_progress, downloaded, total_bytes);
                    return Ok(outcome);
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
                    Ok(c) => c,
                    Err(error) => {
                        // Durable flush + close before reconnect: oracle must not run
                        // ahead of disk, and metadata_len needs a closed handle.
                        writer.flush().await.map_err(|e| disk_write_error(e))?;
                        drop(writer);
                        let retryable = error.is_timeout()
                            || error.is_connect()
                            || error.is_request()
                            || error.is_body()
                            || error.is_decode();
                        return Err(download_error(
                            FailureCategory::Network,
                            format!("Download stream failed: {}", format_reqwest_error(&error)),
                            retryable,
                        ));
                    }
                };

                if chunk.is_empty() {
                    continue;
                }

                // Pre-write: charge the shared limiter (may burst up to bucket capacity).
                // Interruptible for pause/cancel, but once the stream has delivered a chunk
                // it must be written — dropping it leaves a Range-resume hole.
                // On abort mid-throttle some quanta may already be charged; do not re-acquire
                // the full length (would double-bill). Slight under-charge on the pause edge
                // is acceptable.
                let acquired = limiter.acquire(chunk.len(), Some(control.as_ref())).await;

                writer.write_all(&chunk).await.map_err(disk_write_error)?;

                let n = chunk.len() as u64;
                downloaded = downloaded.saturating_add(n);
                window_bytes = window_bytes.saturating_add(n);

                if !acquired {
                    let outcome = control_outcome(&control).unwrap_or(DownloadOutcome::Paused);
                    flush_partial_writer(&mut writer, fsync_on_pause, outcome).await?;
                    emit_control_exit_progress(&on_progress, downloaded, total_bytes);
                    return Ok(outcome);
                }

                if last_progress.elapsed() >= PROGRESS_INTERVAL {
                    let elapsed = window_start.elapsed().as_secs_f64().max(0.001);
                    let speed = (window_bytes as f64 / elapsed) as u64;
                    window_start = Instant::now();
                    window_bytes = 0;

                    let eta_secs = if speed > 0 && total_bytes > downloaded {
                        (total_bytes - downloaded) / speed
                    } else {
                        0
                    };

                    on_progress(ProgressUpdate::downloading_tick(
                        downloaded,
                        total_bytes,
                        speed,
                        eta_secs,
                        progress_percent(downloaded, total_bytes),
                    ));
                    last_progress = Instant::now();
                }
            }

            if let Some(outcome) = control_outcome(&control) {
                flush_partial_writer(&mut writer, fsync_on_pause, outcome).await?;
                emit_control_exit_progress(&on_progress, downloaded, total_bytes);
                return Ok(outcome);
            }

            writer
                .flush()
                .await
                .map_err(|error| disk_write_error(error))?;
            drop(writer);

            if total_bytes > 0 && downloaded < total_bytes {
                return Err(download_error(
                    FailureCategory::Network,
                    format!("Download incomplete ({downloaded} of {total_bytes} bytes)."),
                    true,
                ));
            }

            verify_sha256_if_expected(&temp_path, ctx.job.expected_sha256.as_deref()).await?;

            let final_path = move_to_final_path(&temp_path, &target_path)
                .await
                .map_err(|message| download_error(FailureCategory::Disk, message, false))?;

            on_progress(ProgressUpdate::completed_tick(
                downloaded,
                if total_bytes == 0 {
                    downloaded
                } else {
                    total_bytes
                },
                final_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string()),
                Some(final_path),
                Some(temp_path.clone()),
                Some(resume_supported),
            ));

            Ok(DownloadOutcome::Completed)
        }
        .await;

        match body_result {
            Ok(outcome) => return Ok(outcome),
            Err(error) => {
                // Pause/cancel observed after stream teardown (or remapped control path).
                if let Some(outcome) = control_outcome(&control) {
                    return Ok(outcome);
                }
                // Keep local downloaded for oracle when version-gated (v1+).
                // Disk errors from flush are non-retryable → prepare_reconnect GiveUps.
                existing_bytes = downloaded;
                match prepare_reconnect(
                    &error,
                    /*is_fetch_phase=*/ false,
                    short_reconnects,
                    existing_bytes,
                    resume_supported,
                    transfer_format_version,
                    &temp_path,
                    &control,
                    &on_progress,
                    &mut cumulative_reconnects,
                )
                .await
                {
                    ReconnectAction::Retry { offset } => {
                        short_reconnects += 1;
                        existing_bytes = offset;
                        continue;
                    }
                    ReconnectAction::Control(outcome) => return Ok(outcome),
                    ReconnectAction::GiveUp => return Err(error),
                }
            }
        }
    }
}

/// Outcome of a mid-transfer reconnect decision.
enum ReconnectAction {
    Retry { offset: u64 },
    Control(DownloadOutcome),
    GiveUp,
}

/// Decide whether to short-reconnect, sleep (control-polled), and refresh offset.
async fn prepare_reconnect(
    error: &DownloadError,
    is_fetch_phase: bool,
    short_reconnects: u32,
    existing_bytes: u64,
    resume_supported: bool,
    transfer_format_version: u32,
    temp_path: &std::path::Path,
    control: &AtomicU8,
    on_progress: &ProgressCallback,
    cumulative_reconnects: &mut u32,
) -> ReconnectAction {
    if !can_mid_transfer_reconnect(
        error,
        is_fetch_phase,
        short_reconnects,
        existing_bytes,
        resume_supported,
    ) {
        return ReconnectAction::GiveUp;
    }

    *cumulative_reconnects = cumulative_reconnects.saturating_add(1);
    let next_short = short_reconnects + 1;

    on_progress(ProgressUpdate {
        downloaded_bytes: Some(existing_bytes),
        speed: Some(0),
        eta_secs: Some(0),
        reconnect_count: Some(*cumulative_reconnects),
        state_hint: Some(ProgressHint::Starting),
        ..Default::default()
    });

    let delay = reconnect_backoff(next_short);
    if let Some(outcome) = sleep_interruptible(control, delay).await {
        return ReconnectAction::Control(outcome);
    }

    // Refresh offset from progress oracle after durable flush.
    let offset = refresh_reconnect_offset(transfer_format_version, existing_bytes, temp_path).await;

    ReconnectAction::Retry { offset }
}

/// Nested reconnect eligibility (before worker `RETRY_DELAYS`).
fn can_mid_transfer_reconnect(
    error: &DownloadError,
    is_fetch_phase: bool,
    short_reconnects: u32,
    existing_bytes: u64,
    resume_supported: bool,
) -> bool {
    if short_reconnects >= RECONNECT_MAX {
        return false;
    }
    if !is_reconnectable_error(error) {
        return false;
    }
    // Initial connect/GET failures bubble to worker retry; only reconnect-GET connect
    // errors use the short budget (`short_reconnects > 0`).
    if is_fetch_phase && short_reconnects == 0 {
        return false;
    }
    // Ranges usable: from zero always; partial needs known resume support.
    ranges_usable_for_reconnect(existing_bytes, resume_supported)
}

fn is_reconnectable_error(error: &DownloadError) -> bool {
    error.retryable
        && matches!(
            error.category,
            FailureCategory::Network | FailureCategory::Http
        )
}

fn ranges_usable_for_reconnect(existing_bytes: u64, resume_supported: bool) -> bool {
    existing_bytes == 0 || resume_supported
}

/// Short backoff: base 200ms, doubles per attempt, cap 2s.
pub(crate) fn reconnect_backoff(attempt_1_based: u32) -> Duration {
    let shift = attempt_1_based.saturating_sub(1).min(16);
    let ms = (RECONNECT_BASE.as_millis() as u64).saturating_mul(1u64 << shift);
    Duration::from_millis(ms.min(RECONNECT_CAP.as_millis() as u64))
}

/// Sleep `total` while polling control every `CONTROL_POLL` (pause/cancel aborts).
pub(crate) async fn sleep_interruptible(
    control: &AtomicU8,
    total: Duration,
) -> Option<DownloadOutcome> {
    let deadline = tokio::time::Instant::now() + total;
    loop {
        if let Some(outcome) = control_outcome(control) {
            return Some(outcome);
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return None;
        }
        let slice = (deadline - now).min(CONTROL_POLL);
        sleep(slice).await;
    }
}

/// Progress oracle after reconnect: v1+ keeps tracked bytes; v0 uses `.part` length.
async fn refresh_reconnect_offset(
    transfer_format_version: u32,
    tracked_downloaded: u64,
    temp_path: &std::path::Path,
) -> u64 {
    if transfer_format_version >= 1 {
        tracked_downloaded
    } else {
        metadata_len(temp_path).await.unwrap_or(tracked_downloaded)
    }
}

async fn fetch_with_redirects(
    client: &Client,
    job_url: &str,
    url: &str,
    existing_bytes: u64,
    validators: &ContentValidators,
    control: &AtomicU8,
    handoff_auth: Option<&HandoffAuth>,
) -> Result<(reqwest::Response, String), DownloadError> {
    let mut current = url.to_string();
    let mut redirects = 0u32;

    loop {
        if let Some(outcome) = control_outcome(control) {
            return Err(download_error(
                FailureCategory::Internal,
                match outcome {
                    DownloadOutcome::Paused => "Download paused.".into(),
                    DownloadOutcome::Canceled => "Download canceled.".into(),
                    DownloadOutcome::Completed => "Interrupted.".into(),
                },
                false,
            ));
        }

        let response = send_download_request(
            client,
            job_url,
            &current,
            existing_bytes,
            validators,
            handoff_auth,
        )
        .await?;

        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    download_error(
                        FailureCategory::Http,
                        "Redirect missing Location header.".into(),
                        false,
                    )
                })?;

            let next = resolve_redirect_location(&current, location)?;

            redirects += 1;
            if redirects > MAX_REDIRECTS {
                return Err(download_error(
                    FailureCategory::Http,
                    "Too many redirects.".into(),
                    false,
                ));
            }
            current = next;
            continue;
        }

        return Ok((response, current));
    }
}

/// Kind of transfer request sharing handoff / referer / identity headers.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TransferRequestKind {
    /// GET with optional open-ended Range resume (`bytes=N-`).
    Get { existing_bytes: u64 },
    /// GET closed Range (`bytes={start}-{end}`, inclusive) for multi-segment.
    GetClosed { start: u64, end: u64 },
    /// HEAD preflight probe.
    Head,
    /// GET `Range: bytes=0-0` Accept-Ranges / size probe.
    RangeProbe,
}

/// Build a transfer request (preflight HEAD/Range probe or download GET).
/// Applies same-origin handoff, identity encoding, and browser-like Referer.
pub(crate) fn build_transfer_request(
    client: &Client,
    kind: TransferRequestKind,
    job_url: &str,
    url: &str,
    handoff_auth: Option<&HandoffAuth>,
) -> reqwest::RequestBuilder {
    let mut request = match kind {
        TransferRequestKind::Get { .. }
        | TransferRequestKind::GetClosed { .. }
        | TransferRequestKind::RangeProbe => client.get(url),
        TransferRequestKind::Head => client.head(url),
    };
    request = request.header(ACCEPT_ENCODING, "identity");

    let mut has_referer = false;
    if let Some(auth) = handoff_auth_for_request_url(job_url, url, handoff_auth) {
        for header in &auth.headers {
            if !is_allowed_handoff_header(&header.name) {
                continue;
            }
            if header.name.eq_ignore_ascii_case("referer") {
                has_referer = true;
            }
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(header.name.as_bytes()),
                reqwest::header::HeaderValue::from_str(&header.value),
            ) {
                request = request.header(name, value);
            }
        }
    }

    if !has_referer {
        if let Some(referer) = referer_for_url(url) {
            request = request.header(REFERER, referer);
        }
    }

    match kind {
        TransferRequestKind::Get { existing_bytes } if existing_bytes > 0 => {
            request = request.header(RANGE, format!("bytes={existing_bytes}-"));
        }
        TransferRequestKind::GetClosed { start, end } => {
            request = request.header(RANGE, format!("bytes={start}-{end}"));
        }
        TransferRequestKind::RangeProbe => {
            request = request.header(RANGE, "bytes=0-0");
        }
        _ => {}
    }

    request
}

/// Build a GET for the download transfer (identity encoding, optional Range/If-Range, Referer).
fn build_download_request(
    client: &Client,
    job_url: &str,
    url: &str,
    existing_bytes: u64,
    validators: &ContentValidators,
    handoff_auth: Option<&HandoffAuth>,
) -> reqwest::RequestBuilder {
    let mut request = build_transfer_request(
        client,
        TransferRequestKind::Get { existing_bytes },
        job_url,
        url,
        handoff_auth,
    );
    if existing_bytes > 0 {
        if let Some(if_range) = if_range_header_value(validators) {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(if_range) {
                request = request.header(IF_RANGE, value);
            }
        }
    }
    request
}

/// Resolve a redirect Location against the current URL (absolute or relative).
pub(crate) fn resolve_redirect_location(
    current: &str,
    location: &str,
) -> Result<String, DownloadError> {
    match url::Url::parse(location) {
        Ok(absolute) => Ok(absolute.to_string()),
        Err(_) => {
            let base = url::Url::parse(current).map_err(|error| {
                download_error(
                    FailureCategory::Http,
                    format!("Invalid URL during redirect: {error}"),
                    false,
                )
            })?;
            base.join(location).map(|u| u.to_string()).map_err(|error| {
                download_error(
                    FailureCategory::Http,
                    format!("Invalid redirect target: {error}"),
                    false,
                )
            })
        }
    }
}

/// Best-effort preflight: pin `resolved_url` and publish an early progress patch.
/// No-op (returns `None`) when this attempt already probed.
pub(crate) async fn apply_preflight(
    ctx: &mut TransferContext,
) -> Option<super::preflight::PreflightInfo> {
    if ctx.preflight_done {
        return None;
    }
    ctx.preflight_done = true;
    let client = download_client().ok()?;
    let info = super::preflight::run_preflight(
        &client,
        &ctx.job.url,
        &ctx.resolved_url,
        ctx.handoff_auth.as_ref(),
        &ctx.control,
    )
    .await?;
    ctx.resolved_url = info.final_url.clone();
    let patch = preflight_progress_patch(&ctx.job, &info);
    if let Some(total) = patch.total_bytes {
        ctx.job.total_bytes = total;
    }
    if let Some(resume) = patch.resume_supported {
        ctx.job.resume_supported = resume;
    }
    if let Some(validators) = patch.validators.clone() {
        ctx.job.validators.merge_present(validators);
    }
    if let Some(filename) = patch.filename.clone() {
        ctx.job.filename = filename;
    }
    (ctx.on_progress)(patch);
    Some(info)
}

/// Early ProgressUpdate from preflight (size / validators / resume hint).
///
/// Sparse only: never forces `progress` (would zero a resume job) or overwrites a
/// user/uniquified `filename` — GET `starting_tick` owns rename + path updates.
pub(crate) fn preflight_progress_patch(
    job: &Job,
    info: &super::preflight::PreflightInfo,
) -> ProgressUpdate {
    let total = info.total_bytes.filter(|&n| n > 0);
    let validators = {
        let captured = ContentValidators {
            etag: info.etag.clone(),
            last_modified: info.last_modified.clone(),
            expected_size: total,
        };
        if captured.is_empty() {
            None
        } else {
            Some(captured)
        }
    };
    // Filename only from Content-Disposition, and only when the job still has a
    // generic fallback name (matches GET CD rename caution). Never URL-derive here.
    let filename = info.filename.as_ref().and_then(|cd_name| {
        let generic = job.filename.is_empty()
            || job.filename == "download.bin"
            || filename_from_url_fallback(&job.url).as_deref() == Some(job.filename.as_str());
        if generic {
            Some(cd_name.clone())
        } else {
            None
        }
    });
    ProgressUpdate {
        total_bytes: total,
        // Leave progress / downloaded_bytes / speed / eta None so resume partials
        // and coalesce do not flash 0%.
        filename,
        resume_supported: info.accept_ranges,
        state_hint: Some(ProgressHint::Starting),
        validators,
        ..Default::default()
    }
}

/// Prefer TCP HTTP/1.1–2, then fall back to HTTP/3 (QUIC) on connect/TLS failures.
/// QUIC often bypasses SNI-based router filters that break plain HTTPS.
async fn send_download_request(
    client: &Client,
    job_url: &str,
    url: &str,
    existing_bytes: u64,
    validators: &ContentValidators,
    handoff_auth: Option<&HandoffAuth>,
) -> Result<reqwest::Response, DownloadError> {
    let primary = build_download_request(
        client,
        job_url,
        url,
        existing_bytes,
        validators,
        handoff_auth,
    )
    .send()
    .await;

    match primary {
        Ok(response) => Ok(response),
        Err(error) if should_try_http3(&error) && url.starts_with("https://") => {
            match build_download_request(
                client,
                job_url,
                url,
                existing_bytes,
                validators,
                handoff_auth,
            )
            .version(Version::HTTP_3)
            .send()
            .await
            {
                Ok(response) => Ok(response),
                Err(http3_error) => {
                    let tcp_detail = format_reqwest_error(&error);
                    let h3_detail = format_reqwest_error(&http3_error);
                    let retryable = error.is_timeout()
                        || error.is_connect()
                        || error.is_request()
                        || http3_error.is_timeout()
                        || http3_error.is_connect()
                        || http3_error.is_request();
                    let message = if tcp_detail == h3_detail {
                        format!("Could not connect (TCP + HTTP/3): {tcp_detail}")
                    } else {
                        format!(
                            "Could not connect. TCP/HTTPS: {tcp_detail} | HTTP/3 (QUIC): {h3_detail}"
                        )
                    };
                    Err(download_error(FailureCategory::Network, message, retryable))
                }
            }
        }
        Err(error) => {
            let retryable = error.is_timeout() || error.is_connect() || error.is_request();
            Err(download_error(
                FailureCategory::Network,
                format!("Could not connect: {}", format_reqwest_error(&error)),
                retryable,
            ))
        }
    }
}

/// GET a closed Range against a **pinned** URL. Does not follow redirects
/// (unexpected hops reconnect to the pinned URL).
pub(crate) async fn send_segment_get(
    client: &Client,
    job_url: &str,
    pinned_url: &str,
    range_start: u64,
    range_end: u64,
    validators: &ContentValidators,
    handoff_auth: Option<&HandoffAuth>,
) -> Result<reqwest::Response, DownloadError> {
    let mut request = build_transfer_request(
        client,
        TransferRequestKind::GetClosed {
            start: range_start,
            end: range_end,
        },
        job_url,
        pinned_url,
        handoff_auth,
    );
    if range_start > 0 {
        if let Some(if_range) = if_range_header_value(validators) {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(if_range) {
                request = request.header(IF_RANGE, value);
            }
        }
    }

    let primary = request.send().await;
    match primary {
        Ok(response) => Ok(response),
        Err(error) if should_try_http3(&error) && pinned_url.starts_with("https://") => {
            let mut retry = build_transfer_request(
                client,
                TransferRequestKind::GetClosed {
                    start: range_start,
                    end: range_end,
                },
                job_url,
                pinned_url,
                handoff_auth,
            );
            if range_start > 0 {
                if let Some(if_range) = if_range_header_value(validators) {
                    if let Ok(value) = reqwest::header::HeaderValue::from_str(if_range) {
                        retry = retry.header(IF_RANGE, value);
                    }
                }
            }
            match retry.version(Version::HTTP_3).send().await {
                Ok(response) => Ok(response),
                Err(http3_error) => {
                    let tcp_detail = format_reqwest_error(&error);
                    let h3_detail = format_reqwest_error(&http3_error);
                    let retryable = error.is_timeout()
                        || error.is_connect()
                        || error.is_request()
                        || http3_error.is_timeout()
                        || http3_error.is_connect()
                        || http3_error.is_request();
                    let message = if tcp_detail == h3_detail {
                        format!("Could not connect (TCP + HTTP/3): {tcp_detail}")
                    } else {
                        format!(
                            "Could not connect. TCP/HTTPS: {tcp_detail} | HTTP/3 (QUIC): {h3_detail}"
                        )
                    };
                    Err(download_error(FailureCategory::Network, message, retryable))
                }
            }
        }
        Err(error) => {
            let retryable = error.is_timeout() || error.is_connect() || error.is_request();
            Err(download_error(
                FailureCategory::Network,
                format!("Could not connect: {}", format_reqwest_error(&error)),
                retryable,
            ))
        }
    }
}

fn should_try_http3(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout() || error.is_request() || looks_like_tls_failure(error)
}

fn looks_like_tls_failure(error: &reqwest::Error) -> bool {
    let text = format_error_chain(error).to_ascii_lowercase();
    text.contains("tls")
        || text.contains("ssl")
        || text.contains("certificate")
        || text.contains("handshake")
        || text.contains("corrupt message")
        || text.contains("invalidcontenttype")
        || text.contains("invalid content type")
        || text.contains("sec_e_invalid_token")
        || text.contains("frame size")
        || text.contains("corrupted frame")
}

/// Full error chain for UI: top-level message + nested causes + optional filter hint.
pub fn format_reqwest_error(error: &reqwest::Error) -> String {
    let chain = format_error_chain(error);
    if looks_like_tls_interference(&chain) {
        format!(
            "{chain}. Hint: TLS handshake failed — possible router web filter (e.g. ASUS AiProtection), \
antivirus HTTPS scan, or blocked domain. Browsers may still work via HTTP/3/QUIC; try allowlisting \
the host or copy the browser's final download URL."
        )
    } else {
        chain
    }
}

fn format_error_chain(error: &reqwest::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    let top = error.to_string();
    parts.push(top);

    let mut source = error.source();
    while let Some(err) = source {
        let text = err.to_string();
        // Skip exact duplicates / pure wrappers that add no detail.
        if parts.iter().all(|p| p != &text) {
            parts.push(text);
        }
        source = err.source();
    }

    if parts.len() == 1 {
        return parts.remove(0);
    }

    // "top (cause1 → cause2 → root)"
    let root_path = parts[1..].join(" → ");
    format!("{} ({})", parts[0], root_path)
}

fn looks_like_tls_interference(chain: &str) -> bool {
    let lower = chain.to_ascii_lowercase();
    lower.contains("corrupt message")
        || lower.contains("invalidcontenttype")
        || lower.contains("invalid content type")
        || lower.contains("sec_e_invalid_token")
        || lower.contains("token supplied to the function is invalid")
        || lower.contains("frame size")
        || lower.contains("corrupted frame")
        || lower.contains("certificate")
        || (lower.contains("tls")
            && (lower.contains("fail") || lower.contains("error") || lower.contains("handshake")))
        || (lower.contains("ssl")
            && (lower.contains("fail") || lower.contains("error") || lower.contains("handshake")))
}

pub(crate) fn control_outcome(control: &AtomicU8) -> Option<DownloadOutcome> {
    match control.load(Ordering::Relaxed) {
        CONTROL_PAUSED => Some(DownloadOutcome::Paused),
        CONTROL_CANCELED => Some(DownloadOutcome::Canceled),
        _ => None,
    }
}

pub fn store_control(control: &AtomicU8, value: WorkerControl) {
    let raw = match value {
        WorkerControl::Continue => CONTROL_CONTINUE,
        WorkerControl::Paused => CONTROL_PAUSED,
        WorkerControl::Canceled => CONTROL_CANCELED,
    };
    control.store(raw, Ordering::Relaxed);
}

pub(crate) fn progress_percent(downloaded: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    ((downloaded as f64 / total as f64) * 100.0).clamp(0.0, 100.0)
}

/// How single-stream should pick a Range start. Never treats `written_sum` as a prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeDecision {
    /// v0, no map — caller should `metadata_len` (contiguous `.part`).
    NeedDiskLen,
    /// One consistent segment: Range starts at `start + written`.
    Contiguous { offset: u64 },
    /// Consistent multi map — per-segment remaining; single-stream must not seek.
    MultiMap,
    /// v1 + missing map, or present map that is inconsistent — do not invent Range.
    RestartRequired,
}

fn resume_decision(job: &Job) -> ResumeDecision {
    match job.segment_map.as_ref() {
        Some(map) if !map.is_consistent() => ResumeDecision::RestartRequired,
        Some(map) if map.segments.len() <= 1 => {
            let offset = map
                .segments
                .first()
                .map(super::segment::Segment::remaining_start)
                .unwrap_or(0);
            ResumeDecision::Contiguous { offset }
        }
        Some(_) => ResumeDecision::MultiMap,
        None if job.transfer_format_version >= 1 => ResumeDecision::RestartRequired,
        None => ResumeDecision::NeedDiskLen,
    }
}

/// Strong ETags are usable for If-Range. Weak form is `W/"…"` (RFC 7232).
fn is_strong_etag(etag: &str) -> bool {
    let t = etag.trim();
    !t.is_empty() && !t.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("W/"))
}

/// If-Range value selection (normative):
/// - strong ETag → prefer it
/// - weak ETag only → prefer Last-Modified if present, else bare Range (`None`)
/// - Last-Modified (no strong ETag) → use it
/// - none → bare Range
pub(crate) fn if_range_header_value(validators: &ContentValidators) -> Option<&str> {
    if let Some(etag) = validators.etag.as_deref() {
        let etag = etag.trim();
        if is_strong_etag(etag) {
            return Some(etag);
        }
    }
    validators
        .last_modified
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Both sides present and differ. Missing 206 headers are not a mismatch
/// (CDN 206 without identity still continues).
fn resume_identity_mismatch(stored: &ContentValidators, incoming: &ContentValidators) -> bool {
    if let (Some(a), Some(b)) = (stored.etag.as_deref(), incoming.etag.as_deref()) {
        if a.trim() != b.trim() {
            return true;
        }
    }
    if let (Some(a), Some(b)) = (
        stored.last_modified.as_deref(),
        incoming.last_modified.as_deref(),
    ) {
        if a.trim() != b.trim() {
            return true;
        }
    }
    false
}

/// Size mismatch when both stored expected_size and a numeric Content-Range total are known.
pub(crate) fn content_range_size_mismatch(
    content_range_total: Option<u64>,
    expected_size: Option<u64>,
) -> Option<(u64, u64)> {
    match (content_range_total, expected_size) {
        (Some(total), Some(expected)) if total != expected => Some((total, expected)),
        _ => None,
    }
}

/// Capture ETag / Last-Modified / expected size from a successful download response.
fn content_validators_from_headers(
    headers: &reqwest::header::HeaderMap,
    total_bytes: u64,
) -> ContentValidators {
    ContentValidators {
        etag: headers
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        last_modified: headers
            .get(LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        expected_size: if total_bytes > 0 {
            Some(total_bytes)
        } else {
            None
        },
    }
}

/// Progress patch value: `None` when capture is empty so apply does not replace stored identity.
fn content_validators_patch(
    headers: &reqwest::header::HeaderMap,
    total_bytes: u64,
) -> Option<ContentValidators> {
    let captured = content_validators_from_headers(headers, total_bytes);
    if captured.is_empty() {
        None
    } else {
        Some(captured)
    }
}

fn disk_write_error(error: std::io::Error) -> DownloadError {
    download_error(
        FailureCategory::Disk,
        format!("Could not write download data: {error}"),
        false,
    )
}

/// Flush buffered writes; optionally `sync_data` on pause (prefer over `sync_all` on Windows).
async fn flush_partial_writer(
    writer: &mut BufWriter<tokio::fs::File>,
    fsync_on_pause: bool,
    outcome: DownloadOutcome,
) -> Result<(), DownloadError> {
    writer.flush().await.map_err(disk_write_error)?;
    if should_sync_data_on_exit(fsync_on_pause, outcome) {
        // Already flushed to the OS; a failed durability sync must not fail Pause.
        let _ = writer.get_ref().sync_data().await;
    }
    Ok(())
}

/// `sync_data` only on pause when enabled — cancel/complete skip fsync.
fn should_sync_data_on_exit(fsync_on_pause: bool, outcome: DownloadOutcome) -> bool {
    fsync_on_pause && matches!(outcome, DownloadOutcome::Paused)
}

fn emit_control_exit_progress(on_progress: &ProgressCallback, downloaded: u64, total_bytes: u64) {
    on_progress(ProgressUpdate::downloading_tick(
        downloaded,
        total_bytes,
        0,
        0,
        progress_percent(downloaded, total_bytes),
    ));
}

pub(crate) fn should_retry_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    ) || status.is_server_error()
}

fn filename_from_url_fallback(url: &str) -> Option<String> {
    super::filesystem::derive_filename_from_url(url)
}

fn filename_from_response_url(url: &str) -> Option<String> {
    super::filesystem::derive_filename_from_url(url).map(|s| sanitize_filename(&s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn progress_clamps() {
        assert_eq!(progress_percent(50, 100), 50.0);
        assert_eq!(progress_percent(0, 0), 0.0);
        assert_eq!(progress_percent(150, 100), 100.0);
    }

    #[test]
    fn tls_interference_hint_matches_known_patterns() {
        assert!(looks_like_tls_interference(
            "error sending request (received corrupt message of type InvalidContentType)"
        ));
        assert!(looks_like_tls_interference(
            "The token supplied to the function is invalid (os error -2146893048)"
        ));
        assert!(!looks_like_tls_interference("connection refused"));
    }

    /// Coalesce merge order: later patch wins on Some; earlier Some preserved when later is None.
    #[test]
    fn progress_update_merge_later_wins() {
        let earlier = ProgressUpdate {
            downloaded_bytes: Some(10),
            total_bytes: Some(100),
            speed: Some(1),
            eta_secs: Some(90),
            progress: Some(10.0),
            filename: Some("a.bin".into()),
            target_path: Some(PathBuf::from("/tmp/a")),
            temp_path: Some(PathBuf::from("/tmp/a.part")),
            resume_supported: Some(true),
            state_hint: Some(ProgressHint::Starting),
            validators: Some(ContentValidators {
                etag: Some("\"v1\"".into()),
                last_modified: None,
                expected_size: Some(100),
            }),
            ..Default::default()
        };
        let later = ProgressUpdate {
            downloaded_bytes: Some(50),
            total_bytes: None,
            speed: Some(5),
            eta_secs: None,
            progress: Some(50.0),
            filename: None,
            target_path: None,
            temp_path: None,
            resume_supported: None,
            state_hint: Some(ProgressHint::Downloading),
            // Speed tick leaves validators None → earlier preserved.
            validators: None,
            ..Default::default()
        };

        let merged = earlier.merge(later);
        assert_eq!(merged.downloaded_bytes, Some(50));
        assert_eq!(merged.total_bytes, Some(100)); // preserved from earlier
        assert_eq!(merged.speed, Some(5));
        assert_eq!(merged.eta_secs, Some(90)); // preserved from earlier
        assert_eq!(merged.progress, Some(50.0));
        assert_eq!(merged.filename.as_deref(), Some("a.bin"));
        assert_eq!(merged.target_path, Some(PathBuf::from("/tmp/a")));
        assert_eq!(merged.temp_path, Some(PathBuf::from("/tmp/a.part")));
        assert_eq!(merged.resume_supported, Some(true));
        assert_eq!(merged.state_hint, Some(ProgressHint::Downloading));
        assert_eq!(
            merged.validators.as_ref().and_then(|v| v.etag.as_deref()),
            Some("\"v1\"")
        );
    }

    #[test]
    fn content_validators_from_headers_captures_etag_lm_size() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(ETAG, "\"abc123\"".parse().unwrap());
        headers.insert(
            LAST_MODIFIED,
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        let v = content_validators_from_headers(&headers, 4096);
        assert_eq!(v.etag.as_deref(), Some("\"abc123\""));
        assert_eq!(
            v.last_modified.as_deref(),
            Some("Wed, 21 Oct 2015 07:28:00 GMT")
        );
        assert_eq!(v.expected_size, Some(4096));
    }

    #[test]
    fn content_validators_zero_total_omits_expected_size() {
        let headers = reqwest::header::HeaderMap::new();
        let v = content_validators_from_headers(&headers, 0);
        assert!(v.is_empty());
    }

    #[test]
    fn content_validators_patch_none_when_empty() {
        let headers = reqwest::header::HeaderMap::new();
        assert!(content_validators_patch(&headers, 0).is_none());
    }

    #[test]
    fn content_validators_patch_some_when_present() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(ETAG, "\"x\"".parse().unwrap());
        let patch = content_validators_patch(&headers, 0).expect("non-empty");
        assert_eq!(patch.etag.as_deref(), Some("\"x\""));
    }

    #[test]
    fn starting_tick_empty_validators_leave_none() {
        let tick = ProgressUpdate::starting_tick(0, 0, None, None, None, Some(true), None);
        assert!(tick.validators.is_none());
    }

    #[test]
    fn resume_decision_v0_uses_disk_len() {
        let mut job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        job.downloaded_bytes = 42;
        job.transfer_format_version = 0;
        assert_eq!(resume_decision(&job), ResumeDecision::NeedDiskLen);
    }

    #[test]
    fn resume_decision_v1_no_map_does_not_invent() {
        let mut job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        job.downloaded_bytes = 42;
        job.transfer_format_version = 1;
        assert_eq!(resume_decision(&job), ResumeDecision::RestartRequired);
    }

    #[test]
    fn resume_decision_single_segment_uses_remaining_start() {
        let mut job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        job.downloaded_bytes = 42;
        job.segment_map = Some(crate::download::segment::SegmentMap {
            total_bytes: 1000,
            segment_count: 1,
            segments: vec![crate::download::segment::Segment {
                index: 0,
                start: 0,
                end: 999,
                written: 250,
                state: crate::download::segment::SegmentState::Active,
            }],
            preallocated: true,
        });
        assert_eq!(
            resume_decision(&job),
            ResumeDecision::Contiguous { offset: 250 }
        );
    }

    #[test]
    fn resume_decision_multi_map_does_not_invent_prefix() {
        let mut job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        job.downloaded_bytes = 42;
        job.transfer_format_version = 0;
        job.segment_map = Some(crate::download::segment::SegmentMap {
            total_bytes: 1000,
            segment_count: 2,
            segments: vec![
                crate::download::segment::Segment {
                    index: 0,
                    start: 0,
                    end: 499,
                    written: 100,
                    state: crate::download::segment::SegmentState::Active,
                },
                crate::download::segment::Segment {
                    index: 1,
                    start: 500,
                    end: 999,
                    written: 25,
                    state: crate::download::segment::SegmentState::Pending,
                },
            ],
            preallocated: true,
        });
        assert_eq!(resume_decision(&job), ResumeDecision::MultiMap);
    }

    #[test]
    fn resume_decision_inconsistent_map_restart_required() {
        let mut job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        job.transfer_format_version = 1;
        job.segment_map = Some(crate::download::segment::SegmentMap {
            total_bytes: 1000,
            segment_count: 2,
            segments: vec![
                crate::download::segment::Segment {
                    index: 0,
                    start: 0,
                    end: 499,
                    written: 100,
                    state: crate::download::segment::SegmentState::Active,
                },
                crate::download::segment::Segment {
                    index: 1,
                    start: 0,
                    end: 999,
                    written: 25,
                    state: crate::download::segment::SegmentState::Pending,
                },
            ],
            preallocated: true,
        });
        assert_eq!(resume_decision(&job), ResumeDecision::RestartRequired);
    }

    #[test]
    fn downloading_tick_sets_scalars_only() {
        let tick = ProgressUpdate::downloading_tick(25, 100, 10, 7, 25.0);
        assert_eq!(tick.downloaded_bytes, Some(25));
        assert_eq!(tick.total_bytes, Some(100));
        assert_eq!(tick.speed, Some(10));
        assert!(tick.validators.is_none());
        assert!(tick.transfer_format_version.is_none());
        assert!(tick.segment_map.is_none());
        assert_eq!(tick.eta_secs, Some(7));
        assert_eq!(tick.progress, Some(25.0));
        assert!(tick.filename.is_none());
        assert!(tick.target_path.is_none());
        assert!(tick.temp_path.is_none());
        assert!(tick.resume_supported.is_none());
        assert_eq!(tick.state_hint, Some(ProgressHint::Downloading));
    }

    #[test]
    fn sync_data_only_when_pause_and_enabled() {
        assert!(should_sync_data_on_exit(true, DownloadOutcome::Paused));
        assert!(!should_sync_data_on_exit(false, DownloadOutcome::Paused));
        assert!(!should_sync_data_on_exit(true, DownloadOutcome::Canceled));
        assert!(!should_sync_data_on_exit(true, DownloadOutcome::Completed));
        assert!(!should_sync_data_on_exit(false, DownloadOutcome::Canceled));
    }

    #[test]
    fn is_strong_etag_rejects_weak() {
        assert!(is_strong_etag("\"abc123\""));
        assert!(is_strong_etag("\"cdn-opaque-v2\""));
        assert!(!is_strong_etag("W/\"abc123\""));
        assert!(!is_strong_etag("w/\"weak\""));
        assert!(!is_strong_etag(""));
        assert!(!is_strong_etag("   "));
    }

    #[test]
    fn if_range_prefers_strong_etag_over_last_modified() {
        let v = ContentValidators {
            etag: Some("\"strong-1\"".into()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".into()),
            expected_size: Some(1_048_576),
        };
        assert_eq!(if_range_header_value(&v), Some("\"strong-1\""));
    }

    #[test]
    fn if_range_weak_etag_falls_back_to_last_modified() {
        // CDN-like: weak ETag + Last-Modified → If-Range uses LM only.
        let v = ContentValidators {
            etag: Some("W/\"5f4dcc3b\"".into()),
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            expected_size: Some(999),
        };
        assert_eq!(
            if_range_header_value(&v),
            Some("Mon, 01 Jan 2024 00:00:00 GMT")
        );
    }

    #[test]
    fn preflight_patch_leaves_progress_none_on_known_total() {
        let job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        let info = super::super::preflight::PreflightInfo {
            total_bytes: Some(1_000_000),
            filename: None,
            accept_ranges: Some(true),
            etag: Some("\"e\"".into()),
            last_modified: None,
            final_url: job.url.clone(),
        };
        let patch = preflight_progress_patch(&job, &info);
        assert_eq!(patch.total_bytes, Some(1_000_000));
        assert!(patch.progress.is_none(), "must not force 0% on resume");
        assert!(patch.downloaded_bytes.is_none());
        assert!(patch.speed.is_none());
        assert!(patch.eta_secs.is_none());
        assert_eq!(patch.resume_supported, Some(true));
        assert_eq!(
            patch.validators.as_ref().and_then(|v| v.etag.as_deref()),
            Some("\"e\"")
        );
    }

    #[test]
    fn if_range_weak_etag_only_uses_bare_range() {
        let v = ContentValidators {
            etag: Some("W/\"only-weak\"".into()),
            last_modified: None,
            expected_size: Some(100),
        };
        assert_eq!(if_range_header_value(&v), None);
    }

    #[test]
    fn if_range_last_modified_only() {
        let v = ContentValidators {
            etag: None,
            last_modified: Some("Tue, 15 Nov 1994 12:45:26 GMT".into()),
            expected_size: None,
        };
        assert_eq!(
            if_range_header_value(&v),
            Some("Tue, 15 Nov 1994 12:45:26 GMT")
        );
    }

    #[test]
    fn if_range_none_when_empty_validators() {
        assert_eq!(if_range_header_value(&ContentValidators::default()), None);
    }

    #[test]
    fn content_range_size_mismatch_numeric_only() {
        assert_eq!(
            content_range_size_mismatch(Some(1000), Some(2000)),
            Some((1000, 2000))
        );
        assert_eq!(content_range_size_mismatch(Some(1000), Some(1000)), None);
        // `*` total → None total → no mismatch.
        assert_eq!(content_range_size_mismatch(None, Some(1000)), None);
        assert_eq!(content_range_size_mismatch(Some(1000), None), None);
    }

    #[test]
    fn resume_identity_mismatch_compares_present_fields_only() {
        let stored = ContentValidators {
            etag: Some("\"a\"".into()),
            last_modified: Some("Tue, 15 Nov 1994 12:45:26 GMT".into()),
            expected_size: Some(1000),
        };
        let same = ContentValidators {
            etag: Some("\"a\"".into()),
            last_modified: Some("Tue, 15 Nov 1994 12:45:26 GMT".into()),
            expected_size: None,
        };
        assert!(!resume_identity_mismatch(&stored, &same));

        let missing = ContentValidators::default();
        assert!(
            !resume_identity_mismatch(&stored, &missing),
            "absent 206 headers are not a mismatch"
        );

        let weak_changed = ContentValidators {
            etag: Some("W/\"b\"".into()),
            last_modified: None,
            expected_size: None,
        };
        assert!(resume_identity_mismatch(&stored, &weak_changed));

        let lm_changed = ContentValidators {
            etag: None,
            last_modified: Some("Wed, 16 Nov 1994 12:45:26 GMT".into()),
            expected_size: None,
        };
        assert!(resume_identity_mismatch(&stored, &lm_changed));
    }

    #[test]
    fn cdn_like_206_star_total_and_strong_etag_selection() {
        // CloudFront-style probe: Content-Range bytes 0-0/* + strong ETag.
        let (start, end, total) = parse_content_range("bytes 0-0/*").unwrap();
        assert_eq!((start, end, total), (0, 0, None));
        assert!(content_range_size_mismatch(total, Some(5_000_000)).is_none());

        let v = ContentValidators {
            etag: Some("\"cf-etag-abc\"".into()),
            last_modified: Some("Wed, 12 Aug 2026 08:00:00 GMT".into()),
            expected_size: Some(5_000_000),
        };
        assert_eq!(if_range_header_value(&v), Some("\"cf-etag-abc\""));

        // Resume mid-file with numeric total matching expected_size.
        let (start, end, total) = parse_content_range("bytes 1000-4999/5000").unwrap();
        assert_eq!(start, 1000);
        assert_eq!(end, 4999);
        assert_eq!(total, Some(5000));
        assert!(content_range_size_mismatch(total, Some(5000)).is_none());
        assert_eq!(
            content_range_size_mismatch(total, Some(4096)),
            Some((5000, 4096))
        );
    }

    #[test]
    fn cdn_like_weak_etag_with_accept_ranges() {
        // Fastly/Akamai often emit weak ETags; prefer bare Range over weak If-Range.
        let v = ContentValidators {
            etag: Some("W/\"1a2b3c\"".into()),
            last_modified: None,
            expected_size: Some(2_048_576),
        };
        assert!(!is_strong_etag(v.etag.as_deref().unwrap()));
        assert_eq!(if_range_header_value(&v), None);

        // Same object with LM present — use LM for If-Range.
        let mut with_lm = v.clone();
        with_lm.last_modified = Some("Sun, 06 Nov 1994 08:49:37 GMT".into());
        assert_eq!(
            if_range_header_value(&with_lm),
            Some("Sun, 06 Nov 1994 08:49:37 GMT")
        );
    }

    #[test]
    fn build_download_request_attaches_range_and_strong_if_range() {
        let client = download_client().expect("client");
        let url = "https://cdn.example.com/file.bin";
        let strong = ContentValidators {
            etag: Some("\"strong-etag\"".into()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".into()),
            expected_size: Some(4096),
        };
        let req = build_download_request(&client, url, url, 512, &strong, None)
            .build()
            .expect("build");
        assert_eq!(
            req.headers().get(RANGE).and_then(|v| v.to_str().ok()),
            Some("bytes=512-")
        );
        assert_eq!(
            req.headers().get(IF_RANGE).and_then(|v| v.to_str().ok()),
            Some("\"strong-etag\"")
        );
    }

    #[test]
    fn build_download_request_weak_only_omits_if_range() {
        let client = download_client().expect("client");
        let url = "https://cdn.example.com/file.bin";
        let weak_only = ContentValidators {
            etag: Some("W/\"weak\"".into()),
            last_modified: None,
            expected_size: Some(100),
        };
        let req = build_download_request(&client, url, url, 100, &weak_only, None)
            .build()
            .expect("build");
        assert_eq!(
            req.headers().get(RANGE).and_then(|v| v.to_str().ok()),
            Some("bytes=100-")
        );
        assert!(req.headers().get(IF_RANGE).is_none());
    }

    #[test]
    fn build_download_request_no_range_when_offset_zero() {
        let client = download_client().expect("client");
        let url = "https://cdn.example.com/file.bin";
        let strong = ContentValidators {
            etag: Some("\"x\"".into()),
            last_modified: None,
            expected_size: None,
        };
        let req = build_download_request(&client, url, url, 0, &strong, None)
            .build()
            .expect("build");
        assert!(req.headers().get(RANGE).is_none());
        assert!(req.headers().get(IF_RANGE).is_none());
    }

    #[test]
    fn preflight_patch_preserves_user_and_uniquified_filename() {
        let mut job = Job::new(
            "https://example.com/file.zip".into(),
            "file (1).zip".into(),
            PathBuf::from("C:\\dl\\file (1).zip"),
            PathBuf::from("C:\\dl\\file (1).zip.part"),
        );
        let info = super::super::preflight::PreflightInfo {
            total_bytes: Some(10),
            filename: Some("server-name.zip".into()),
            accept_ranges: Some(true),
            etag: None,
            last_modified: None,
            final_url: job.url.clone(),
        };
        let patch = preflight_progress_patch(&job, &info);
        assert!(
            patch.filename.is_none(),
            "uniquified name must not be overwritten"
        );

        // Generic fallback still allows CD rename hint.
        job.filename = "download.bin".into();
        let patch = preflight_progress_patch(&job, &info);
        assert_eq!(patch.filename.as_deref(), Some("server-name.zip"));
    }

    #[test]
    fn reconnect_backoff_200ms_to_2s() {
        assert_eq!(reconnect_backoff(1), Duration::from_millis(200));
        assert_eq!(reconnect_backoff(2), Duration::from_millis(400));
        assert_eq!(reconnect_backoff(3), Duration::from_millis(800));
        assert_eq!(reconnect_backoff(4), Duration::from_millis(1600));
        assert_eq!(reconnect_backoff(5), Duration::from_secs(2)); // 3200 capped
        assert_eq!(reconnect_backoff(6), Duration::from_secs(2));
    }

    #[test]
    fn can_reconnect_body_network_incomplete() {
        let body_err = download_error(
            FailureCategory::Network,
            "Download stream failed: reset".into(),
            true,
        );
        let incomplete = download_error(
            FailureCategory::Network,
            "Download incomplete (100 of 500 bytes).".into(),
            true,
        );
        // Mid-body with resume support.
        assert!(can_mid_transfer_reconnect(&body_err, false, 0, 100, true));
        assert!(can_mid_transfer_reconnect(&incomplete, false, 0, 100, true));
        // Exhausted short budget.
        assert!(!can_mid_transfer_reconnect(
            &body_err,
            false,
            RECONNECT_MAX,
            100,
            true
        ));
        // Partial without range support — not seamless reconnect.
        assert!(!can_mid_transfer_reconnect(&body_err, false, 0, 100, false));
        // From zero always ranges-usable.
        assert!(can_mid_transfer_reconnect(&body_err, false, 0, 0, false));
    }

    #[test]
    fn can_reconnect_fetch_only_after_prior_short_reconnect() {
        let connect = download_error(
            FailureCategory::Network,
            "Could not connect: timed out".into(),
            true,
        );
        // First attempt connect → worker RETRY_DELAYS.
        assert!(!can_mid_transfer_reconnect(&connect, true, 0, 50, true));
        // Reconnect GET connect error → short budget.
        assert!(can_mid_transfer_reconnect(&connect, true, 1, 50, true));
    }

    #[test]
    fn non_retryable_and_disk_errors_not_reconnectable() {
        let resume = download_error(
            FailureCategory::Resume,
            "Server rejected resume".into(),
            false,
        );
        let disk = download_error(FailureCategory::Disk, "Could not write".into(), false);
        assert!(!can_mid_transfer_reconnect(&resume, false, 0, 10, true));
        assert!(!can_mid_transfer_reconnect(&disk, false, 0, 10, true));
        assert!(!is_reconnectable_error(&resume));
        assert!(!is_reconnectable_error(&disk));
    }

    #[test]
    fn ranges_usable_for_reconnect_rules() {
        assert!(ranges_usable_for_reconnect(0, false));
        assert!(ranges_usable_for_reconnect(0, true));
        assert!(ranges_usable_for_reconnect(10, true));
        assert!(!ranges_usable_for_reconnect(10, false));
    }

    #[tokio::test]
    async fn sleep_interruptible_respects_cancel() {
        let control = Arc::new(AtomicU8::new(CONTROL_CONTINUE));
        let control_sleep = control.clone();
        let sleeper = tokio::spawn(async move {
            sleep_interruptible(control_sleep.as_ref(), Duration::from_secs(30)).await
        });
        // Flip cancel shortly after start.
        sleep(Duration::from_millis(20)).await;
        control.store(CONTROL_CANCELED, Ordering::Relaxed);
        let outcome = sleeper.await.expect("join");
        assert_eq!(outcome, Some(DownloadOutcome::Canceled));
    }

    #[tokio::test]
    async fn sleep_interruptible_respects_pause() {
        let control = Arc::new(AtomicU8::new(CONTROL_CONTINUE));
        let control_sleep = control.clone();
        let sleeper = tokio::spawn(async move {
            sleep_interruptible(control_sleep.as_ref(), Duration::from_secs(30)).await
        });
        sleep(Duration::from_millis(20)).await;
        control.store(CONTROL_PAUSED, Ordering::Relaxed);
        let outcome = sleeper.await.expect("join");
        assert_eq!(outcome, Some(DownloadOutcome::Paused));
    }

    #[tokio::test]
    async fn sleep_interruptible_completes_without_control() {
        let control = AtomicU8::new(CONTROL_CONTINUE);
        let outcome = sleep_interruptible(&control, Duration::from_millis(30)).await;
        assert!(outcome.is_none());
    }

    /// Reconnect re-GET must re-apply Range + strong If-Range on the pinned URL
    /// (same builder path as mid-transfer continue).
    #[test]
    fn reconnect_reget_attaches_range_if_range_on_pinned_url() {
        let client = download_client().expect("client");
        let job_url = "https://origin.example.com/dl/token";
        let pinned = "https://cdn.example.com/file.bin?sig=1";
        let validators = ContentValidators {
            etag: Some("\"resume-etag\"".into()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".into()),
            expected_size: Some(10_000),
        };
        let offset = 4096u64;
        let req = build_download_request(&client, job_url, pinned, offset, &validators, None)
            .build()
            .expect("build");
        assert_eq!(req.url().as_str(), pinned);
        assert_eq!(
            req.headers().get(RANGE).and_then(|v| v.to_str().ok()),
            Some("bytes=4096-")
        );
        assert_eq!(
            req.headers().get(IF_RANGE).and_then(|v| v.to_str().ok()),
            Some("\"resume-etag\"")
        );
    }

    #[test]
    fn disk_flush_failure_is_not_reconnectable() {
        let disk = disk_write_error(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "disk full",
        ));
        assert!(!is_reconnectable_error(&disk));
        assert!(!can_mid_transfer_reconnect(&disk, false, 0, 100, true));
        assert_eq!(disk.category, FailureCategory::Disk);
        assert!(!disk.retryable);
    }

    #[tokio::test]
    async fn refresh_reconnect_offset_version_gate() {
        let dir = std::env::temp_dir().join(format!(
            "rusticdl-reconnect-offset-{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let part = dir.join("f.bin.part");
        tokio::fs::write(&part, vec![0u8; 77]).await.unwrap();

        // v0: disk length wins.
        assert_eq!(refresh_reconnect_offset(0, 10, &part).await, 77);
        // v1+: tracked downloaded (map-authoritative), ignore sparse length.
        assert_eq!(refresh_reconnect_offset(1, 42, &part).await, 42);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn prepare_reconnect_bumps_count_and_refreshes_offset() {
        let dir =
            std::env::temp_dir().join(format!("rusticdl-reconnect-prep-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let part = dir.join("f.bin.part");
        tokio::fs::write(&part, vec![1u8; 64]).await.unwrap();

        let control = AtomicU8::new(CONTROL_CONTINUE);
        let patches = Arc::new(std::sync::Mutex::new(Vec::new()));
        let patches_cb = patches.clone();
        let on_progress: ProgressCallback = Arc::new(move |u: ProgressUpdate| {
            patches_cb.lock().unwrap().push(u);
        });

        let mut cumulative = 3u32;
        let err = download_error(
            FailureCategory::Network,
            "Download incomplete (64 of 200 bytes).".into(),
            true,
        );
        let action = prepare_reconnect(
            &err,
            false,
            0,
            64,
            true,
            0,
            &part,
            &control,
            &on_progress,
            &mut cumulative,
        )
        .await;

        match action {
            ReconnectAction::Retry { offset } => {
                assert_eq!(offset, 64); // disk oracle
            }
            ReconnectAction::Control(_) => panic!("expected Retry, got Control"),
            ReconnectAction::GiveUp => panic!("expected Retry, got GiveUp"),
        }
        assert_eq!(cumulative, 4);
        let held = patches.lock().unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].reconnect_count, Some(4));
        assert_eq!(held[0].downloaded_bytes, Some(64));
        assert_eq!(held[0].state_hint, Some(ProgressHint::Starting));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
