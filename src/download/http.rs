use futures_util::StreamExt;
use reqwest::header::{
    ACCEPT_ENCODING, ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_RANGE, ETAG, LAST_MODIFIED, RANGE,
    REFERER,
};
use reqwest::{Client, StatusCode, Version};
use std::error::Error as StdError;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio::time::sleep;

use super::bandwidth::GlobalBandwidthLimiter;
use super::client::{download_client, referer_for_url};
use super::filesystem::{
    ensure_parent_directory, metadata_len, move_to_final_path, parse_content_disposition_filename,
    parse_content_range, sanitize_filename,
};
use super::handoff::{handoff_auth_for_request_url, is_allowed_handoff_header, HandoffAuth};
use super::job::{
    download_error, ContentValidators, DownloadError, DownloadOutcome, FailureCategory, Job,
    TransferMode, WorkerControl,
};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(400);
const CONTROL_POLL: Duration = Duration::from_millis(200);
#[allow(dead_code)]
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_REDIRECTS: u32 = 10;

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
    pub transfer_format_version: Option<u32>,
    pub active_connections: Option<u32>,
    pub reconnect_count: Option<u32>,
    pub transfer_mode: Option<TransferMode>,
    pub fallback_reason: Option<String>,
}

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
            transfer_format_version: later
                .transfer_format_version
                .or(self.transfer_format_version),
            active_connections: later.active_connections.or(self.active_connections),
            reconnect_count: later.reconnect_count.or(self.reconnect_count),
            transfer_mode: later.transfer_mode.or(self.transfer_mode),
            fallback_reason: later.fallback_reason.or(self.fallback_reason),
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
            transfer_format_version: None,
            active_connections: None,
            reconnect_count: None,
            transfer_mode: None,
            fallback_reason: None,
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
            transfer_format_version: None,
            active_connections: None,
            reconnect_count: None,
            transfer_mode: None,
            fallback_reason: None,
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
            transfer_format_version: None,
            active_connections: None,
            reconnect_count: None,
            transfer_mode: None,
            fallback_reason: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressHint {
    Starting,
    Downloading,
}

pub type ProgressCallback = Arc<dyn Fn(ProgressUpdate) + Send + Sync>;

pub async fn run_http_download(
    job: &Job,
    limiter: Arc<GlobalBandwidthLimiter>,
    control: Arc<AtomicU8>,
    on_progress: ProgressCallback,
    handoff_auth: Option<&HandoffAuth>,
    fsync_on_pause: bool,
) -> Result<DownloadOutcome, DownloadError> {
    ensure_parent_directory(&job.target_path)
        .await
        .map_err(|message| download_error(FailureCategory::Disk, message, false))?;

    let client = download_client()?;
    let mut current_url = job.url.clone();
    // Version gate: v1+ is map-authoritative — never use sparse `.part` length for
    // Range/progress. v0 uses single-stream metadata_len.
    let disk_len = metadata_len(&job.temp_path).await.unwrap_or(0);
    let mut existing_bytes = resume_offset(job, disk_len);

    // Follow redirects manually (client has Policy::none).
    let (response, final_url) = fetch_with_redirects(
        &client,
        &job.url,
        &current_url,
        existing_bytes,
        &control,
        handoff_auth,
    )
    .await?;
    current_url = final_url;

    if let Some(outcome) = control_outcome(&control) {
        return Ok(outcome);
    }

    let status = response.status();
    if status == StatusCode::RANGE_NOT_SATISFIABLE && existing_bytes > 0 {
        // Partial already complete or invalid; try without range if file is full.
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
        return Err(download_error(FailureCategory::Http, message, retryable));
    }

    let resume_supported = status == StatusCode::PARTIAL_CONTENT
        || response
            .headers()
            .get(ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("bytes"));

    if existing_bytes > 0 && status != StatusCode::PARTIAL_CONTENT {
        // Server ignored Range and sent full body — restart from zero.
        existing_bytes = 0;
        let _ = tokio::fs::remove_file(&job.temp_path).await;
    }

    if existing_bytes > 0 {
        if let Some(content_range) = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
        {
            if let Some((start, _end, _total)) = parse_content_range(content_range) {
                if start != existing_bytes {
                    return Err(download_error(
                        FailureCategory::Resume,
                        format!(
                            "Unexpected resume range (got start {start}, expected {existing_bytes}). Use Restart."
                        ),
                        false,
                    ));
                }
            }
        }
    }

    let mut total_bytes = response
        .content_length()
        .map(|len| {
            if status == StatusCode::PARTIAL_CONTENT {
                existing_bytes.saturating_add(len)
            } else {
                len
            }
        })
        .unwrap_or(0);

    if let Some(content_range) = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
    {
        if let Some((_start, _end, total)) = parse_content_range(content_range) {
            total_bytes = total;
        }
    }

    // Empty capture → None so apply leaves stored validators alone (CDN 206 without
    // identity headers). Non-empty Some is field-wise merged on apply (never wipes).
    let validators = content_validators_patch(response.headers(), total_bytes);

    let mut target_path = job.target_path.clone();
    let mut temp_path = job.temp_path.clone();
    let mut filename = job.filename.clone();

    if let Some(header_name) = response
        .headers()
        .get(CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_disposition_filename)
    {
        if job.filename == "download.bin"
            || filename_from_url_fallback(&job.url).as_deref() == Some(job.filename.as_str())
        {
            filename = header_name;
            if let Some(parent) = target_path.parent() {
                target_path = parent.join(&filename);
                temp_path = super::filesystem::temp_path_for(&target_path);
                // If we already wrote to the old temp path, rename it.
                if job.temp_path != temp_path && job.temp_path.exists() {
                    let _ = tokio::fs::rename(&job.temp_path, &temp_path).await;
                }
            }
        }
    } else if let Some(from_final) = filename_from_response_url(&current_url) {
        if job.filename == "download.bin" {
            filename = from_final;
            if let Some(parent) = target_path.parent() {
                target_path = parent.join(&filename);
                temp_path = super::filesystem::temp_path_for(&target_path);
                if job.temp_path != temp_path && job.temp_path.exists() {
                    let _ = tokio::fs::rename(&job.temp_path, &temp_path).await;
                }
            }
        }
    }

    on_progress(ProgressUpdate::starting_tick(
        existing_bytes,
        total_bytes,
        Some(filename.clone()),
        Some(target_path.clone()),
        Some(temp_path.clone()),
        Some(resume_supported),
        validators,
    ));

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

    loop {
        if let Some(outcome) = control_outcome(&control) {
            flush_partial_writer(&mut writer, fsync_on_pause, outcome).await?;
            // Align UI with flushed bytes (may be ahead of last PROGRESS_INTERVAL tick).
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

        let chunk = chunk_result.map_err(|error| {
            let retryable = error.is_timeout()
                || error.is_connect()
                || error.is_request()
                || error.is_body()
                || error.is_decode();
            download_error(
                FailureCategory::Network,
                format!("Download stream failed: {}", format_reqwest_error(&error)),
                retryable,
            )
        })?;

        if chunk.is_empty() {
            continue;
        }

        // Pre-write: charge the shared limiter (may burst up to bucket capacity).
        // Interruptible for pause/cancel, but once the stream has delivered a chunk
        // it must be written — dropping it leaves a Range-resume hole.
        // On abort mid-throttle some quanta may already be charged; do not re-acquire
        // the full length (would double-bill). Slight under-charge on the pause edge
        // is acceptable.
        let acquired = limiter.acquire(chunk.len(), Some(&control)).await;

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
        Some(temp_path),
        Some(resume_supported),
    ));

    Ok(DownloadOutcome::Completed)
}

async fn fetch_with_redirects(
    client: &Client,
    job_url: &str,
    url: &str,
    existing_bytes: u64,
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

        let response =
            send_download_request(client, job_url, &current, existing_bytes, handoff_auth).await?;

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

            let next = match url::Url::parse(location) {
                Ok(absolute) => absolute.to_string(),
                Err(_) => {
                    let base = url::Url::parse(&current).map_err(|error| {
                        download_error(
                            FailureCategory::Http,
                            format!("Invalid URL during redirect: {error}"),
                            false,
                        )
                    })?;
                    base.join(location)
                        .map_err(|error| {
                            download_error(
                                FailureCategory::Http,
                                format!("Invalid redirect target: {error}"),
                                false,
                            )
                        })?
                        .to_string()
                }
            };

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

/// Build a GET for the download transfer (identity encoding, optional Range, browser-like Referer).
fn build_download_request(
    client: &Client,
    job_url: &str,
    url: &str,
    existing_bytes: u64,
    handoff_auth: Option<&HandoffAuth>,
) -> reqwest::RequestBuilder {
    let mut request = client.get(url).header(ACCEPT_ENCODING, "identity");

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

    if existing_bytes > 0 {
        request = request.header(RANGE, format!("bytes={existing_bytes}-"));
    }

    request
}

/// Prefer TCP HTTP/1.1–2, then fall back to HTTP/3 (QUIC) on connect/TLS failures.
/// QUIC often bypasses SNI-based router filters that break plain HTTPS.
async fn send_download_request(
    client: &Client,
    job_url: &str,
    url: &str,
    existing_bytes: u64,
    handoff_auth: Option<&HandoffAuth>,
) -> Result<reqwest::Response, DownloadError> {
    let primary = build_download_request(client, job_url, url, existing_bytes, handoff_auth)
        .send()
        .await;

    match primary {
        Ok(response) => Ok(response),
        Err(error) if should_try_http3(&error) && url.starts_with("https://") => {
            match build_download_request(client, job_url, url, existing_bytes, handoff_auth)
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

fn control_outcome(control: &AtomicU8) -> Option<DownloadOutcome> {
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

fn progress_percent(downloaded: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    ((downloaded as f64 / total as f64) * 100.0).clamp(0.0, 100.0)
}

/// Resume Range start: v1+ is map-authoritative (`job.downloaded_bytes`); v0 uses `.part` length.
fn resume_offset(job: &Job, disk_len: u64) -> u64 {
    if job.transfer_format_version >= 1 {
        job.downloaded_bytes
    } else {
        disk_len
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

fn should_retry_status(status: StatusCode) -> bool {
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

/// HEAD preflight used when adding a job to guess size/filename (best effort).
#[allow(dead_code)]
pub async fn preflight(url: &str) -> Option<(Option<u64>, Option<String>)> {
    let client = download_client().ok()?;
    let mut current = url.to_string();
    let mut redirects = 0u32;

    let response = loop {
        let mut request = client
            .head(&current)
            .timeout(PREFLIGHT_TIMEOUT)
            .header(ACCEPT_ENCODING, "identity");
        if let Some(referer) = referer_for_url(&current) {
            request = request.header(REFERER, referer);
        }
        let response = request.send().await.ok()?;

        if !response.status().is_redirection() {
            break response;
        }

        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())?;
        let next = url::Url::parse(location)
            .ok()
            .map(|u| u.to_string())
            .or_else(|| {
                url::Url::parse(&current)
                    .ok()
                    .and_then(|base| base.join(location).ok())
                    .map(|u| u.to_string())
            })?;
        redirects += 1;
        if redirects > MAX_REDIRECTS {
            return None;
        }
        current = next;
    };

    if !response.status().is_success() {
        return None;
    }

    let total = response.content_length();
    let filename = response
        .headers()
        .get(CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_disposition_filename)
        .or_else(|| super::filesystem::derive_filename_from_url(&current));

    let _accept_ranges = response.headers().get(ACCEPT_RANGES);
    Some((total, filename))
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
    fn resume_offset_version_gate() {
        let mut job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        job.downloaded_bytes = 42;
        job.transfer_format_version = 0;
        assert_eq!(resume_offset(&job, 999), 999);

        job.transfer_format_version = 1;
        assert_eq!(resume_offset(&job, 999), 42);
    }

    #[test]
    fn downloading_tick_sets_scalars_only() {
        let tick = ProgressUpdate::downloading_tick(25, 100, 10, 7, 25.0);
        assert_eq!(tick.downloaded_bytes, Some(25));
        assert_eq!(tick.total_bytes, Some(100));
        assert_eq!(tick.speed, Some(10));
        assert!(tick.validators.is_none());
        assert!(tick.transfer_format_version.is_none());
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
}
