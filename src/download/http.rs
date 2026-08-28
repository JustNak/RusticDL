use reqwest::header::{ACCEPT_RANGES, CONTENT_DISPOSITION, ETAG, LAST_MODIFIED};
use reqwest::StatusCode;
use std::sync::atomic::AtomicU8;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use super::bandwidth::GlobalBandwidthLimiter;
use super::body::{stream_body, AppendSink, StreamEnd, CONTROL_POLL};
use super::client::download_client;
use super::conn_budget::{host_key_for_budget, ConnectionBudget};
use super::engine::EngineRuntimeConfig;
use super::eta::EtaSmoother;
use super::fetch::{
    content_range_size_mismatch, fetch_range, FetchRequest, RangeSpec, RangeStatus,
};
use super::filesystem::{
    ensure_parent_directory, metadata_len, move_to_final_path, parse_content_disposition_filename,
    sanitize_filename,
};
use super::handoff::{session_url_after_auth_denied, HandoffAuth};
use super::job::{
    download_error, ContentValidators, DownloadError, DownloadOutcome, FailureCategory, Job,
};
use super::progress::{
    CommitIdentity, MapUpdate, NoopIdentity, ProgressHint, ProgressTick, TransferEvent,
    TransferEventCallback,
};
use super::resume::{resume_oracle, ResumeOracle};
use super::verify::verify_sha256_if_expected;

const PROGRESS_INTERVAL: Duration = Duration::from_millis(400);

pub(crate) const RECONNECT_MAX: u32 = 5;
pub(crate) const RECONNECT_BASE: Duration = Duration::from_millis(200);
pub(crate) const RECONNECT_CAP: Duration = Duration::from_secs(2);

const FULL_REPLACE_NOTICE: &str =
    "Remote file changed or server ignored resume; restarting download from the beginning.";

pub use super::context::TransferContext;
pub(crate) use super::fetch::control_outcome;
pub use super::fetch::store_control;

pub async fn run_http_download(
    job: &Job,
    limiter: Arc<GlobalBandwidthLimiter>,
    control: Arc<AtomicU8>,
    on_progress: TransferEventCallback,
    handoff_auth: Option<&HandoffAuth>,
) -> Result<DownloadOutcome, DownloadError> {
    let config = EngineRuntimeConfig::default();
    let mut ctx = TransferContext::from_runtime(
        job.clone(),
        control,
        on_progress,
        handoff_auth.cloned(),
        limiter,
        ConnectionBudget::new(
            config.max_total_connections,
            config.max_connections_per_host,
        ),
        Arc::new(NoopIdentity),
        &config,
    );
    run_http_download_with_ctx(&mut ctx).await
}

pub async fn run_http_download_with_ctx(
    ctx: &mut TransferContext,
) -> Result<DownloadOutcome, DownloadError> {
    ensure_parent_directory(&ctx.job.target_path)
        .await
        .map_err(|message| download_error(FailureCategory::Disk, message, false))?;

    let client = download_client()?;

    apply_preflight(ctx).await;

    if let Some(outcome) = control_outcome(&ctx.control) {
        return Ok(outcome);
    }

    let mut existing_bytes = match resume_oracle(&ctx.job) {
        ResumeOracle::FreshSingle | ResumeOracle::LegacySingle => {
            metadata_len(&ctx.job.temp_path).await.unwrap_or(0)
        }
        ResumeOracle::Multi { .. } => {
            return Err(download_error(
                FailureCategory::Resume,
                "Multi-connection partial; single-stream resume is not supported. Use Restart."
                    .into(),
                false,
            ));
        }
        ResumeOracle::RestartRequired => {
            return Err(download_error(
                FailureCategory::Resume,
                "Multi-part incomplete; Restart required.".into(),
                false,
            ));
        }
    };

    let host = host_key_for_budget(&ctx.resolved_url);
    let _permit = match ctx
        .conn_budget
        .acquire_interruptible(&host, &ctx.control)
        .await
    {
        Ok(permit) => permit,
        Err(outcome) => return Ok(outcome),
    };

    let job_url = ctx.job.url.clone();
    let mut current_url = ctx.resolved_url.clone();
    let mut validators = ctx.job.validators.clone();
    let mut target_path = ctx.job.target_path.clone();
    let mut temp_path = ctx.job.temp_path.clone();
    let mut filename = ctx.job.filename.clone();
    let mut transfer_format_version = ctx.job.transfer_format_version;
    let mut total_bytes: u64;
    let mut resume_supported = ctx.job.resume_supported;

    let mut short_reconnects: u32 = 0;
    let mut cumulative_reconnects = ctx.job.reconnect_count;
    let reconnect_baseline = ctx.job.reconnect_count;
    // One remint from job.url after a 401 on a burned Inst-FS / Drive hop.
    let mut replayed_session = false;

    let control = ctx.control.clone();
    let on_progress = ctx.on_progress.clone();
    let committer = ctx.committer.clone();
    let handoff_auth = ctx.handoff_auth.clone();
    let limiter = ctx.limiter.clone();

    loop {
        if let Some(outcome) = control_outcome(&control) {
            return Ok(outcome);
        }

        let fetch_result = fetch_range(FetchRequest {
            client: &client,
            job_url: &job_url,
            url: &current_url,
            range: RangeSpec::Open {
                start: existing_bytes,
            },
            validators: &validators,
            handoff: handoff_auth.as_ref(),
            follow_redirects: true,
            control: &control,
        })
        .await;

        let (response, final_url, range_status) = match fetch_result {
            Ok(outcome) => (outcome.response, outcome.final_url, outcome.status),
            Err(error) => {
                if let Some(outcome) = control_outcome(&control) {
                    return Ok(outcome);
                }
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
        if let RangeStatus::AuthDenied { status } = range_status {
            if let Some(replay) = session_url_after_auth_denied(
                &job_url,
                &final_url,
                handoff_auth.as_ref(),
                replayed_session,
            ) {
                replayed_session = true;
                current_url = replay.to_string();
                ctx.resolved_url = current_url.clone();
                drop(response);
                continue;
            }
            return Err(http_status_error(status, false));
        }

        current_url = final_url;
        ctx.resolved_url = current_url.clone();

        if let Some(outcome) = control_outcome(&control) {
            return Ok(outcome);
        }

        let mut full_replace = false;
        let range_total = match &range_status {
            RangeStatus::RangeNotSatisfiable { at } => {
                return Err(download_error(
                    FailureCategory::Resume,
                    format!(
                        "Server rejected resume at {at} bytes. Use Restart to download from zero."
                    ),
                    false,
                ));
            }
            RangeStatus::AuthDenied { status } => {
                return Err(http_status_error(*status, false));
            }
            RangeStatus::Other { status, retryable } => {
                if *status == StatusCode::PARTIAL_CONTENT && existing_bytes > 0 {
                    return Err(download_error(
                        FailureCategory::Resume,
                        "Missing or invalid Content-Range on partial response. Use Restart.".into(),
                        false,
                    ));
                }
                let error = http_status_error(*status, *retryable);
                if let Some(outcome) = control_outcome(&control) {
                    return Ok(outcome);
                }
                if *retryable {
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
            RangeStatus::RedirectWhenPinned => {
                return Err(download_error(
                    FailureCategory::Network,
                    "Unexpected redirect on segment request.".into(),
                    true,
                ));
            }
            RangeStatus::FullEntityWhenRangeRequested => {
                existing_bytes = 0;
                full_replace = true;
                let _ = tokio::fs::remove_file(&temp_path).await;
                None
            }
            RangeStatus::Partial { start, total, .. } => {
                if *start != existing_bytes {
                    return Err(download_error(
                        FailureCategory::Resume,
                        format!(
                            "Unexpected resume range (got start {start}, expected {existing_bytes}). Use Restart."
                        ),
                        false,
                    ));
                }
                if existing_bytes > 0 {
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
                *total
            }
            RangeStatus::OkFromZero => None,
        };

        let http_status = response.status();
        resume_supported = http_status == StatusCode::PARTIAL_CONTENT
            || response
                .headers()
                .get(ACCEPT_RANGES)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.to_ascii_lowercase().contains("bytes"))
            || resume_supported;

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
                if http_status == StatusCode::PARTIAL_CONTENT {
                    existing_bytes.saturating_add(len)
                } else {
                    len
                }
            })
            .unwrap_or(0);

        if let Some(total) = range_total {
            total_bytes = total;
        }

        // Capture validators for this response; keep local copy for reconnect If-Range.
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

        if !ctx.job.replace_existing {
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
        }

        if full_replace {
            transfer_format_version = 0;
        }
        committer
            .commit(
                &mut ctx.job,
                CommitIdentity {
                    downloaded_bytes: Some(existing_bytes),
                    total_bytes: Some(total_bytes),
                    progress: Some(progress_percent(existing_bytes, total_bytes)),
                    filename: Some(filename.clone()),
                    target_path: Some(target_path.clone()),
                    temp_path: Some(temp_path.clone()),
                    resume_supported: Some(resume_supported),
                    validators: validators_patch,
                    replace_validators: full_replace,
                    transfer_format_version: full_replace.then_some(0),
                    map: if full_replace {
                        MapUpdate::Clear
                    } else {
                        MapUpdate::Unchanged
                    },
                    ..Default::default()
                },
            )
            .await
            .map_err(|message| download_error(FailureCategory::Internal, message, false))?;
        let mut starting = ProgressTick {
            downloaded_bytes: Some(existing_bytes),
            total_bytes: Some(total_bytes),
            speed: Some(0),
            eta_secs: Some(0),
            progress: Some(progress_percent(existing_bytes, total_bytes)),
            state_hint: Some(ProgressHint::Starting),
            active_connections: Some(1),
            ..Default::default()
        };
        if cumulative_reconnects > reconnect_baseline {
            starting.reconnect_count = Some(cumulative_reconnects);
        }
        on_progress(TransferEvent::Tick(starting));
        if full_replace {
            on_progress(TransferEvent::Toast(FULL_REPLACE_NOTICE.into()));
        }

        ensure_parent_directory(&temp_path)
            .await
            .map_err(|message| download_error(FailureCategory::Disk, message, false))?;

        let mut sink = AppendSink::open(&temp_path, existing_bytes)
            .await?
            .with_target(total_bytes);
        let mut downloaded = existing_bytes;
        let mut last_progress = Instant::now();
        let mut window_start = Instant::now();
        let mut window_bytes: u64 = 0;
        let mut eta_smoother = EtaSmoother::new();

        on_progress(TransferEvent::Tick(ProgressTick::downloading(
            downloaded,
            total_bytes,
            0,
            0,
            progress_percent(downloaded, total_bytes),
        )));

        let body_result = stream_body(response, &mut sink, &control, limiter.as_ref(), |n| {
            downloaded = downloaded.saturating_add(n);
            window_bytes = window_bytes.saturating_add(n);
            if last_progress.elapsed() >= PROGRESS_INTERVAL {
                let elapsed = window_start.elapsed().as_secs_f64().max(0.001);
                let speed = (window_bytes as f64 / elapsed) as u64;
                window_start = Instant::now();
                window_bytes = 0;
                let remaining = total_bytes.saturating_sub(downloaded);
                let eta_secs = if speed == 0 {
                    0
                } else {
                    eta_smoother.observe(speed, remaining).1
                };
                on_progress(TransferEvent::Tick(ProgressTick::downloading(
                    downloaded,
                    total_bytes,
                    speed,
                    eta_secs,
                    progress_percent(downloaded, total_bytes),
                )));
                last_progress = Instant::now();
            }
        })
        .await;

        downloaded = sink.offset();
        match body_result {
            Ok(StreamEnd::Control(outcome)) => {
                if matches!(outcome, DownloadOutcome::Paused) {
                    let _ = sink.sync_data().await;
                }
                drop(sink);
                emit_control_exit_progress(&on_progress, downloaded, total_bytes);
                return Ok(outcome);
            }
            Ok(StreamEnd::Exhausted { downloaded }) => {
                drop(sink);
                verify_sha256_if_expected(&temp_path, ctx.job.expected_sha256.as_deref()).await?;
                let final_path =
                    move_to_final_path(&temp_path, &target_path, ctx.job.replace_existing)
                        .await
                        .map_err(|message| download_error(FailureCategory::Disk, message, false))?;
                committer
                    .commit(
                        &mut ctx.job,
                        CommitIdentity {
                            downloaded_bytes: Some(downloaded),
                            total_bytes: Some(if total_bytes == 0 {
                                downloaded
                            } else {
                                total_bytes
                            }),
                            progress: Some(100.0),
                            filename: final_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .map(|s| s.to_string()),
                            target_path: Some(final_path),
                            temp_path: Some(temp_path.clone()),
                            resume_supported: Some(resume_supported),
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|message| download_error(FailureCategory::Internal, message, false))?;
                return Ok(DownloadOutcome::Completed);
            }
            Err(error) => {
                drop(sink);
                if let Some(outcome) = control_outcome(&control) {
                    if error.retryable
                        || matches!(
                            error.category,
                            FailureCategory::Network | FailureCategory::Http
                        )
                    {
                        emit_control_exit_progress(&on_progress, existing_bytes, total_bytes);
                        return Ok(outcome);
                    }
                }
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

enum ReconnectAction {
    Retry { offset: u64 },
    Control(DownloadOutcome),
    GiveUp,
}

async fn prepare_reconnect(
    error: &DownloadError,
    is_fetch_phase: bool,
    short_reconnects: u32,
    existing_bytes: u64,
    resume_supported: bool,
    transfer_format_version: u32,
    temp_path: &std::path::Path,
    control: &AtomicU8,
    on_progress: &TransferEventCallback,
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

    on_progress(TransferEvent::Tick(ProgressTick {
        downloaded_bytes: Some(existing_bytes),
        speed: Some(0),
        eta_secs: Some(0),
        reconnect_count: Some(*cumulative_reconnects),
        state_hint: Some(ProgressHint::Starting),
        ..Default::default()
    }));

    let delay = reconnect_backoff(next_short);
    if let Some(outcome) = sleep_interruptible(control, delay).await {
        return ReconnectAction::Control(outcome);
    }

    let offset = refresh_reconnect_offset(transfer_format_version, existing_bytes, temp_path).await;

    ReconnectAction::Retry { offset }
}

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
    if is_fetch_phase && short_reconnects == 0 {
        return false;
    }
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

pub(crate) fn reconnect_backoff(attempt_1_based: u32) -> Duration {
    let shift = attempt_1_based.saturating_sub(1).min(16);
    let ms = (RECONNECT_BASE.as_millis() as u64).saturating_mul(1u64 << shift);
    Duration::from_millis(ms.min(RECONNECT_CAP.as_millis() as u64))
}

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

fn http_status_error(status: StatusCode, retryable: bool) -> DownloadError {
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
    download_error(FailureCategory::Http, message, retryable)
}

pub(crate) async fn apply_preflight(
    ctx: &mut TransferContext,
) -> Option<super::preflight::PreflightInfo> {
    if ctx.preflight_done {
        return None;
    }
    ctx.preflight_done = true;
    let client = download_client().ok()?;
    // Browser captures mint a one-time Inst-FS / Drive Location; discover it without consuming the hop.
    if ctx.handoff_auth.is_some() {
        if let Some(location) = super::preflight::discover_handoff_location(
            &client,
            &ctx.job.url,
            &ctx.resolved_url,
            ctx.handoff_auth.as_ref(),
            &ctx.control,
        )
        .await
        {
            ctx.resolved_url = location;
        }
        return None;
    }

    let oracle = resume_oracle(&ctx.job);
    let plan = super::preflight::PreflightPlan {
        skip_range_probes: oracle.is_resume_error(),
        prove_ranges: matches!(oracle, ResumeOracle::FreshSingle),
        multi_min_bytes: ctx.multi_min_bytes,
    };
    let info = super::preflight::run_preflight_planned(
        &client,
        &ctx.job.url,
        &ctx.resolved_url,
        ctx.handoff_auth.as_ref(),
        &ctx.control,
        plan,
    )
    .await?;
    ctx.resolved_url = info.final_url.clone();
    let identity = preflight_commit_identity(&ctx.job, &info);
    // Do not merge preflight ETag/LM onto the transfer-local job: If-Range on the GET owns identity.
    let _ = ctx.committer.commit(&mut ctx.job, identity).await;
    (ctx.on_progress)(TransferEvent::Tick(ProgressTick {
        total_bytes: info.total_bytes.filter(|&n| n > 0),
        state_hint: Some(ProgressHint::Starting),
        ..Default::default()
    }));
    Some(info)
}

pub(crate) fn preflight_commit_identity(
    job: &Job,
    info: &super::preflight::PreflightInfo,
) -> CommitIdentity {
    let total = info.total_bytes.filter(|&n| n > 0);
    let validators = if job.validators.etag.is_none() && job.validators.last_modified.is_none() {
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
    } else {
        None
    };
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
    CommitIdentity {
        total_bytes: total,
        filename,
        resume_supported: info.accept_ranges,
        validators,
        ..Default::default()
    }
}
pub(crate) fn progress_percent(downloaded: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    ((downloaded as f64 / total as f64) * 100.0).clamp(0.0, 100.0)
}
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

fn should_sync_data_on_exit(outcome: DownloadOutcome) -> bool {
    matches!(outcome, DownloadOutcome::Paused)
}

fn emit_control_exit_progress(
    on_progress: &TransferEventCallback,
    downloaded: u64,
    total_bytes: u64,
) {
    on_progress(TransferEvent::Tick(ProgressTick::downloading(
        downloaded,
        total_bytes,
        0,
        0,
        progress_percent(downloaded, total_bytes),
    )));
}

fn filename_from_url_fallback(url: &str) -> Option<String> {
    super::filesystem::derive_filename_from_url(url)
}

fn filename_from_response_url(url: &str) -> Option<String> {
    super::filesystem::derive_filename_from_url(url).map(|s| sanitize_filename(&s))
}

#[cfg(test)]
mod tests {
    use super::super::fetch::{CONTROL_CANCELED, CONTROL_CONTINUE, CONTROL_PAUSED};
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    #[test]
    fn progress_clamps() {
        assert_eq!(progress_percent(50, 100), 50.0);
        assert_eq!(progress_percent(0, 0), 0.0);
        assert_eq!(progress_percent(150, 100), 100.0);
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
    fn starting_commit_empty_validators_leave_none() {
        let identity = CommitIdentity {
            resume_supported: Some(true),
            ..Default::default()
        };
        assert!(identity.validators.is_none());
        assert!(!identity.replace_validators);
    }

    #[tokio::test]
    async fn single_stream_one_segment_map_rejects_without_seek() {
        let dir = std::env::temp_dir().join(format!("rusticdl-http-1seg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("f.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let original = vec![0xABu8; 1000];
        std::fs::write(&temp, &original).unwrap();

        let mut job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            target,
            temp.clone(),
        );
        job.downloaded_bytes = 250;
        job.total_bytes = 1000;
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
        assert!(matches!(
            resume_oracle(&job),
            ResumeOracle::Multi { ref map } if map.segments.len() == 1
        ));

        let mut ctx = TransferContext::from_runtime(
            job,
            Arc::new(AtomicU8::new(0)),
            Arc::new(|_: TransferEvent| {}),
            None,
            GlobalBandwidthLimiter::new(None),
            ConnectionBudget::new(32, 8),
            Arc::new(NoopIdentity),
            &EngineRuntimeConfig::default(),
        );
        ctx.preflight_done = true;

        let err = run_http_download_with_ctx(&mut ctx)
            .await
            .expect_err("1-segment map must not enter single-stream");
        assert_eq!(err.category, FailureCategory::Resume);
        assert!(
            err.message
                .contains("single-stream resume is not supported"),
            "got {}",
            err.message
        );
        assert_eq!(
            std::fs::read(&temp).unwrap(),
            original,
            "must not open/seek a preallocated 1-segment .part"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn downloading_tick_sets_scalars_only() {
        let tick = ProgressTick::downloading(25, 100, 10, 7, 25.0);
        assert_eq!(tick.downloaded_bytes, Some(25));
        assert_eq!(tick.total_bytes, Some(100));
        assert_eq!(tick.speed, Some(10));
        assert!(tick.segment_written.is_none());
        assert_eq!(tick.eta_secs, Some(7));
        assert_eq!(tick.progress, Some(25.0));
        assert!(tick.active_connections.is_none());
        assert!(tick.reconnect_count.is_none());
        assert_eq!(tick.state_hint, Some(ProgressHint::Downloading));
    }

    #[test]
    fn sync_data_only_on_pause() {
        assert!(should_sync_data_on_exit(DownloadOutcome::Paused));
        assert!(!should_sync_data_on_exit(DownloadOutcome::Canceled));
        assert!(!should_sync_data_on_exit(DownloadOutcome::Completed));
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
        let patch = preflight_commit_identity(&job, &info);
        assert_eq!(patch.total_bytes, Some(1_000_000));
        assert!(patch.progress.is_none(), "must not force 0% on resume");
        assert!(patch.downloaded_bytes.is_none());
        assert_eq!(patch.resume_supported, Some(true));
        assert_eq!(
            patch.validators.as_ref().and_then(|v| v.etag.as_deref()),
            Some("\"e\"")
        );
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
        let patch = preflight_commit_identity(&job, &info);
        assert!(
            patch.filename.is_none(),
            "uniquified name must not be overwritten"
        );

        job.filename = "download.bin".into();
        let patch = preflight_commit_identity(&job, &info);
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
        let stalled = crate::download::body::stall_error(std::time::Duration::from_secs(30));
        assert!(can_mid_transfer_reconnect(&body_err, false, 0, 100, true));
        assert!(can_mid_transfer_reconnect(&incomplete, false, 0, 100, true));
        assert!(can_mid_transfer_reconnect(&stalled, false, 0, 100, true));
        assert!(!can_mid_transfer_reconnect(&stalled, false, 0, 100, false));
        assert!(!can_mid_transfer_reconnect(
            &body_err,
            false,
            RECONNECT_MAX,
            100,
            true
        ));
        assert!(!can_mid_transfer_reconnect(&body_err, false, 0, 100, false));
        assert!(can_mid_transfer_reconnect(&body_err, false, 0, 0, false));
    }

    #[test]
    fn can_reconnect_fetch_only_after_prior_short_reconnect() {
        let connect = download_error(
            FailureCategory::Network,
            "Could not connect: timed out".into(),
            true,
        );
        assert!(!can_mid_transfer_reconnect(&connect, true, 0, 50, true));
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

    #[test]
    fn disk_flush_failure_is_not_reconnectable() {
        let disk = download_error(
            FailureCategory::Disk,
            "Could not write download data: disk full".into(),
            false,
        );
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

        assert_eq!(refresh_reconnect_offset(0, 10, &part).await, 77);
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
        let on_progress: TransferEventCallback = Arc::new(move |u: TransferEvent| {
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
        let TransferEvent::Tick(tick) = &held[0] else {
            panic!("expected Tick, got {:?}", held[0]);
        };
        assert_eq!(tick.reconnect_count, Some(4));
        assert_eq!(tick.downloaded_bytes, Some(64));
        assert_eq!(tick.state_hint, Some(ProgressHint::Starting));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
