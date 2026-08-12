use futures_util::StreamExt;
use reqwest::header::{
    ACCEPT_ENCODING, ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_RANGE, RANGE, REFERER,
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
    download_error, DownloadError, DownloadOutcome, FailureCategory, Job, WorkerControl,
};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(400);
const CONTROL_POLL: Duration = Duration::from_millis(200);
#[allow(dead_code)]
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_REDIRECTS: u32 = 10;

const CONTROL_CONTINUE: u8 = 0;
const CONTROL_PAUSED: u8 = 1;
const CONTROL_CANCELED: u8 = 2;

#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub speed: u64,
    pub eta_secs: u64,
    pub progress: f64,
    pub filename: Option<String>,
    pub target_path: Option<std::path::PathBuf>,
    pub temp_path: Option<std::path::PathBuf>,
    pub resume_supported: Option<bool>,
    pub state_hint: ProgressHint,
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
) -> Result<DownloadOutcome, DownloadError> {
    ensure_parent_directory(&job.target_path)
        .await
        .map_err(|message| download_error(FailureCategory::Disk, message, false))?;

    let client = download_client()?;
    let mut current_url = job.url.clone();
    let mut existing_bytes = metadata_len(&job.temp_path).await.unwrap_or(0);

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

    on_progress(ProgressUpdate {
        downloaded_bytes: existing_bytes,
        total_bytes,
        speed: 0,
        eta_secs: 0,
        progress: progress_percent(existing_bytes, total_bytes),
        filename: Some(filename.clone()),
        target_path: Some(target_path.clone()),
        temp_path: Some(temp_path.clone()),
        resume_supported: Some(resume_supported),
        state_hint: ProgressHint::Starting,
    });

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

    on_progress(ProgressUpdate {
        downloaded_bytes: downloaded,
        total_bytes,
        speed: 0,
        eta_secs: 0,
        progress: progress_percent(downloaded, total_bytes),
        filename: None,
        target_path: None,
        temp_path: None,
        resume_supported: None,
        state_hint: ProgressHint::Downloading,
    });

    loop {
        if let Some(outcome) = control_outcome(&control) {
            writer
                .flush()
                .await
                .map_err(|error| disk_write_error(error))?;
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
        // Abort promptly on pause/cancel instead of waiting out a long throttle.
        if !limiter.acquire(chunk.len(), Some(&control)).await {
            writer.flush().await.map_err(disk_write_error)?;
            return Ok(control_outcome(&control).unwrap_or(DownloadOutcome::Paused));
        }

        writer.write_all(&chunk).await.map_err(disk_write_error)?;

        let n = chunk.len() as u64;
        downloaded = downloaded.saturating_add(n);
        window_bytes = window_bytes.saturating_add(n);

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

            on_progress(ProgressUpdate {
                downloaded_bytes: downloaded,
                total_bytes,
                speed,
                eta_secs,
                progress: progress_percent(downloaded, total_bytes),
                filename: None,
                target_path: None,
                temp_path: None,
                resume_supported: None,
                state_hint: ProgressHint::Downloading,
            });
            last_progress = Instant::now();
        }
    }

    writer
        .flush()
        .await
        .map_err(|error| disk_write_error(error))?;
    drop(writer);

    if let Some(outcome) = control_outcome(&control) {
        return Ok(outcome);
    }

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

    on_progress(ProgressUpdate {
        downloaded_bytes: downloaded,
        total_bytes: if total_bytes == 0 {
            downloaded
        } else {
            total_bytes
        },
        speed: 0,
        eta_secs: 0,
        progress: 100.0,
        filename: final_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string()),
        target_path: Some(final_path),
        temp_path: Some(temp_path),
        resume_supported: Some(resume_supported),
        state_hint: ProgressHint::Downloading,
    });

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

fn disk_write_error(error: std::io::Error) -> DownloadError {
    download_error(
        FailureCategory::Disk,
        format!("Could not write download data: {error}"),
        false,
    )
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
}
