//! Shared transfer GET: Range / If-Range, H3 fallback, redirect follow, status classify.

use std::error::Error as StdError;
use std::future::Future;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use reqwest::header::{ACCEPT_ENCODING, IF_RANGE, RANGE, REFERER};
use reqwest::{Client, StatusCode, Version};

use super::client::referer_for_url;
use super::filesystem::parse_content_range;
use super::handoff::{handoff_auth_for_request_url, is_allowed_handoff_header, HandoffAuth};
use super::job::{
    download_error, ContentValidators, DownloadError, DownloadOutcome, FailureCategory,
    WorkerControl,
};

/// Timeout for HEAD / Range 0-0 preflight probes.
pub(crate) const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(8);
pub(crate) const MAX_REDIRECTS: u32 = 10;

pub(crate) const CONTROL_CONTINUE: u8 = 0;
pub(crate) const CONTROL_PAUSED: u8 = 1;
pub(crate) const CONTROL_CANCELED: u8 = 2;

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

/// Open-ended (`bytes=N-`) or closed inclusive (`bytes=N-M`) transfer Range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    Open { start: u64 },
    Closed { start: u64, end: u64 },
}

impl RangeSpec {
    pub fn start(self) -> u64 {
        match self {
            Self::Open { start } | Self::Closed { start, .. } => start,
        }
    }
}

pub struct FetchRequest<'a> {
    pub client: &'a Client,
    pub job_url: &'a str,
    pub url: &'a str,
    pub range: RangeSpec,
    pub validators: &'a ContentValidators,
    pub handoff: Option<&'a HandoffAuth>,
    pub follow_redirects: bool,
    pub control: &'a AtomicU8,
}

pub struct FetchOutcome {
    pub response: reqwest::Response,
    pub final_url: String,
    pub status: RangeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeStatus {
    OkFromZero,
    Partial {
        start: u64,
        end: u64,
        total: Option<u64>,
    },
    FullEntityWhenRangeRequested,
    RangeNotSatisfiable {
        at: u64,
    },
    AuthDenied {
        status: StatusCode,
    },
    RedirectWhenPinned,
    Other {
        status: StatusCode,
        retryable: bool,
    },
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

pub async fn fetch_range(req: FetchRequest<'_>) -> Result<FetchOutcome, DownloadError> {
    let requested_start = req.range.start();
    let (response, final_url) = if req.follow_redirects {
        send_following_redirects(req.url, req.control, |current| {
            let current = current.to_string();
            let client = req.client;
            let job_url = req.job_url;
            let range = req.range;
            let validators = req.validators;
            let handoff = req.handoff;
            async move {
                send_transfer_get(client, job_url, &current, range, validators, handoff).await
            }
        })
        .await?
    } else {
        let response = send_transfer_get(
            req.client,
            req.job_url,
            req.url,
            req.range,
            req.validators,
            req.handoff,
        )
        .await?;
        if response.status().is_redirection() {
            return Ok(FetchOutcome {
                response,
                final_url: req.url.to_string(),
                status: RangeStatus::RedirectWhenPinned,
            });
        }
        (response, req.url.to_string())
    };

    let status = classify_range_status(&response, requested_start);
    Ok(FetchOutcome {
        response,
        final_url,
        status,
    })
}

/// Follow `Location` hops. `send_once` issues one request at `current` (caller sets timeout).
pub async fn send_following_redirects<F, Fut>(
    start_url: &str,
    control: &AtomicU8,
    mut send_once: F,
) -> Result<(reqwest::Response, String), DownloadError>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<reqwest::Response, DownloadError>>,
{
    let mut current = start_url.to_string();
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

        let response = send_once(&current).await?;

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
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn build_download_request(
    client: &Client,
    job_url: &str,
    url: &str,
    existing_bytes: u64,
    validators: &ContentValidators,
    handoff_auth: Option<&HandoffAuth>,
) -> reqwest::RequestBuilder {
    apply_if_range(
        build_transfer_request(
            client,
            TransferRequestKind::Get { existing_bytes },
            job_url,
            url,
            handoff_auth,
        ),
        existing_bytes,
        validators,
    )
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

fn apply_if_range(
    mut request: reqwest::RequestBuilder,
    start: u64,
    validators: &ContentValidators,
) -> reqwest::RequestBuilder {
    if start > 0 {
        if let Some(if_range) = if_range_header_value(validators) {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(if_range) {
                request = request.header(IF_RANGE, value);
            }
        }
    }
    request
}

fn range_kind(range: RangeSpec) -> TransferRequestKind {
    match range {
        RangeSpec::Open { start } => TransferRequestKind::Get {
            existing_bytes: start,
        },
        RangeSpec::Closed { start, end } => TransferRequestKind::GetClosed { start, end },
    }
}

async fn send_transfer_get(
    client: &Client,
    job_url: &str,
    url: &str,
    range: RangeSpec,
    validators: &ContentValidators,
    handoff: Option<&HandoffAuth>,
) -> Result<reqwest::Response, DownloadError> {
    let primary = apply_if_range(
        build_transfer_request(client, range_kind(range), job_url, url, handoff),
        range.start(),
        validators,
    )
    .send()
    .await;

    match primary {
        Ok(response) => Ok(response),
        Err(error) if should_try_http3(&error) && url.starts_with("https://") => {
            match apply_if_range(
                build_transfer_request(client, range_kind(range), job_url, url, handoff),
                range.start(),
                validators,
            )
            .version(Version::HTTP_3)
            .send()
            .await
            {
                Ok(response) => Ok(response),
                Err(http3_error) => Err(connect_error_tcp_and_h3(&error, &http3_error)),
            }
        }
        Err(error) => Err(connect_error_tcp(&error)),
    }
}

fn classify_range_status(response: &reqwest::Response, requested_start: u64) -> RangeStatus {
    let status = response.status();
    if status.is_redirection() {
        return RangeStatus::RedirectWhenPinned;
    }
    if status == StatusCode::RANGE_NOT_SATISFIABLE && requested_start > 0 {
        return RangeStatus::RangeNotSatisfiable {
            at: requested_start,
        };
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return RangeStatus::AuthDenied { status };
    }
    if status == StatusCode::OK && requested_start > 0 {
        return RangeStatus::FullEntityWhenRangeRequested;
    }
    if status == StatusCode::PARTIAL_CONTENT {
        if let Some((start, end, total)) = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range)
        {
            return RangeStatus::Partial { start, end, total };
        }
        if requested_start == 0 {
            return RangeStatus::OkFromZero;
        }
        // 206 without Content-Range on a non-zero Range — fail-closed in callers.
        return RangeStatus::Other {
            status,
            retryable: false,
        };
    }
    if status.is_success() && requested_start == 0 {
        return RangeStatus::OkFromZero;
    }
    RangeStatus::Other {
        status,
        retryable: should_retry_status(status),
    }
}

fn should_try_http3(error: &reqwest::Error) -> bool {
    should_try_http3_flags(
        error.is_connect(),
        error.is_timeout(),
        error.is_request(),
        &format_error_chain(error),
    )
}

fn should_try_http3_flags(
    is_connect: bool,
    is_timeout: bool,
    is_request: bool,
    chain: &str,
) -> bool {
    is_connect || is_timeout || is_request || looks_like_tls_failure_text(chain)
}

fn looks_like_tls_failure_text(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
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

fn format_error_chain(error: &reqwest::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    let top = error.to_string();
    parts.push(top);

    let mut source = error.source();
    while let Some(err) = source {
        let text = err.to_string();
        if parts.iter().all(|p| p != &text) {
            parts.push(text);
        }
        source = err.source();
    }

    if parts.len() == 1 {
        return parts.remove(0);
    }

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

fn connect_error_tcp(error: &reqwest::Error) -> DownloadError {
    let retryable = error.is_timeout() || error.is_connect() || error.is_request();
    download_error(
        FailureCategory::Network,
        format!("Could not connect: {}", format_reqwest_error(error)),
        retryable,
    )
}

/// User-visible Resume error when a segment GET gets a 200 for a non-zero Range.
pub(crate) const RANGE_IGNORED_MESSAGE: &str =
    "Server ignored Range on a multi-segment resume. Use Restart.";

pub(crate) fn classify_segment_status(
    status: &RangeStatus,
    range_start: u64,
    expected_size: Option<u64>,
) -> Result<(), DownloadError> {
    match status {
        RangeStatus::RedirectWhenPinned => Err(download_error(
            FailureCategory::Network,
            "Unexpected redirect on segment request.".into(),
            true,
        )),
        RangeStatus::AuthDenied { status } => Err(download_error(
            FailureCategory::Http,
            format!(
                "Download failed with HTTP {status}. Access denied — the link may require a browser session, cookies, or a fresh token."
            ),
            false,
        )),
        RangeStatus::RangeNotSatisfiable { at } => Err(download_error(
            FailureCategory::Resume,
            format!(
                "Server rejected resume at {at} bytes. Use Restart to download from zero."
            ),
            false,
        )),
        RangeStatus::FullEntityWhenRangeRequested => Err(download_error(
            FailureCategory::Resume,
            RANGE_IGNORED_MESSAGE.into(),
            false,
        )),
        RangeStatus::Other { status, retryable } => {
            if *status == StatusCode::PARTIAL_CONTENT && range_start > 0 {
                return Err(missing_content_range_error());
            }
            Err(download_error(
                FailureCategory::Http,
                format!("Download failed with HTTP {status}."),
                *retryable,
            ))
        }
        RangeStatus::Partial { start, total, .. } => {
            if *start != range_start {
                return Err(download_error(
                    FailureCategory::Resume,
                    format!(
                        "Unexpected resume range (got start {start}, expected {range_start}). Use Restart."
                    ),
                    false,
                ));
            }
            if let Some((remote, expected)) = content_range_size_mismatch(*total, expected_size) {
                return Err(download_error(
                    FailureCategory::Resume,
                    format!(
                        "Remote size changed ({remote} bytes vs expected {expected}). Use Restart."
                    ),
                    false,
                ));
            }
            Ok(())
        }
        RangeStatus::OkFromZero => {
            if range_start > 0 {
                return Err(missing_content_range_error());
            }
            Ok(())
        }
    }
}

fn missing_content_range_error() -> DownloadError {
    download_error(
        FailureCategory::Resume,
        "Missing or invalid Content-Range on partial response. Use Restart.".into(),
        false,
    )
}

fn connect_error_tcp_and_h3(tcp: &reqwest::Error, http3: &reqwest::Error) -> DownloadError {
    let tcp_detail = format_reqwest_error(tcp);
    let h3_detail = format_reqwest_error(http3);
    let retryable = tcp.is_timeout()
        || tcp.is_connect()
        || tcp.is_request()
        || http3.is_timeout()
        || http3.is_connect()
        || http3.is_request();
    let message = if tcp_detail == h3_detail {
        format!("Could not connect (TCP + HTTP/3): {tcp_detail}")
    } else {
        format!("Could not connect. TCP/HTTPS: {tcp_detail} | HTTP/3 (QUIC): {h3_detail}")
    };
    download_error(FailureCategory::Network, message, retryable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::client::download_client;
    use crate::download::handoff::{HandoffAuth, HandoffAuthHeader};
    use reqwest::header::{IF_RANGE, RANGE};
    use std::sync::atomic::AtomicU8;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    async fn spawn_scripted_server(
        replies: Vec<String>,
    ) -> (
        String,
        mpsc::UnboundedReceiver<String>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::unbounded_channel::<String>();

        let handle = tokio::spawn(async move {
            let mut replies = replies.into_iter();
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
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
                    if collected.len() > 16_384 {
                        break;
                    }
                }
                let request_text = String::from_utf8_lossy(&collected).to_string();
                let _ = tx.send(request_text);

                let Some(reply) = replies.next() else {
                    break;
                };
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });

        (format!("http://{addr}"), rx, handle)
    }

    fn fetch_req<'a>(
        client: &'a Client,
        url: &'a str,
        range: RangeSpec,
        validators: &'a ContentValidators,
        handoff: Option<&'a HandoffAuth>,
        follow_redirects: bool,
        control: &'a AtomicU8,
    ) -> FetchRequest<'a> {
        FetchRequest {
            client,
            job_url: url,
            url,
            range,
            validators,
            handoff,
            follow_redirects,
            control,
        }
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

    #[test]
    fn http3_fallback_triggers_on_connect_timeout_request_and_tls() {
        assert!(should_try_http3_flags(
            true,
            false,
            false,
            "connection refused"
        ));
        assert!(should_try_http3_flags(
            false,
            true,
            false,
            "connection refused"
        ));
        assert!(should_try_http3_flags(
            false,
            false,
            true,
            "connection refused"
        ));
        assert!(should_try_http3_flags(
            false,
            false,
            false,
            "error sending request (received corrupt message of type InvalidContentType)"
        ));
        assert!(should_try_http3_flags(
            false,
            false,
            false,
            "TLS handshake failure"
        ));
        assert!(!should_try_http3_flags(
            false,
            false,
            false,
            "connection refused"
        ));
        assert!(looks_like_tls_failure_text(
            "error sending request (received corrupt message of type InvalidContentType)"
        ));
        assert!(looks_like_tls_failure_text("TLS handshake failure"));
        assert!(!looks_like_tls_failure_text("connection refused"));
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
        assert_eq!(content_range_size_mismatch(None, Some(1000)), None);
        assert_eq!(content_range_size_mismatch(Some(1000), None), None);
    }

    #[test]
    fn cdn_like_206_star_total_and_strong_etag_selection() {
        let (start, end, total) = parse_content_range("bytes 0-0/*").unwrap();
        assert_eq!((start, end, total), (0, 0, None));
        assert!(content_range_size_mismatch(total, Some(5_000_000)).is_none());

        let v = ContentValidators {
            etag: Some("\"cf-etag-abc\"".into()),
            last_modified: Some("Wed, 12 Aug 2026 08:00:00 GMT".into()),
            expected_size: Some(5_000_000),
        };
        assert_eq!(if_range_header_value(&v), Some("\"cf-etag-abc\""));

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
        let v = ContentValidators {
            etag: Some("W/\"1a2b3c\"".into()),
            last_modified: None,
            expected_size: Some(2_048_576),
        };
        assert!(!is_strong_etag(v.etag.as_deref().unwrap()));
        assert_eq!(if_range_header_value(&v), None);

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

    #[tokio::test]
    async fn fetch_range_classifies_206_alignment() {
        let body = "HTTP/1.1 206 Partial Content\r\n\
Connection: close\r\n\
Content-Range: bytes 100-199/200\r\n\
Content-Length: 100\r\n\
\r\n"
            .to_string();
        let (base, _reqs, _handle) = spawn_scripted_server(vec![body]).await;
        let url = format!("{base}/file.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let validators = ContentValidators::default();
        let outcome = fetch_range(fetch_req(
            &client,
            &url,
            RangeSpec::Open { start: 100 },
            &validators,
            None,
            true,
            &control,
        ))
        .await
        .expect("fetch");
        assert_eq!(
            outcome.status,
            RangeStatus::Partial {
                start: 100,
                end: 199,
                total: Some(200),
            }
        );
    }

    #[tokio::test]
    async fn fetch_range_206_start_mismatch_is_still_partial() {
        let body = "HTTP/1.1 206 Partial Content\r\n\
Connection: close\r\n\
Content-Range: bytes 50-199/200\r\n\
Content-Length: 150\r\n\
\r\n"
            .to_string();
        let (base, _reqs, _handle) = spawn_scripted_server(vec![body]).await;
        let url = format!("{base}/file.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let validators = ContentValidators::default();
        let outcome = fetch_range(fetch_req(
            &client,
            &url,
            RangeSpec::Open { start: 100 },
            &validators,
            None,
            true,
            &control,
        ))
        .await
        .expect("fetch");
        match outcome.status {
            RangeStatus::Partial { start, .. } => assert_eq!(start, 50),
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_range_206_missing_content_range_at_offset() {
        let body = "HTTP/1.1 206 Partial Content\r\n\
Connection: close\r\n\
Content-Length: 10\r\n\
\r\nxxxxxxxxxx"
            .to_string();
        let (base, _reqs, _handle) = spawn_scripted_server(vec![body]).await;
        let url = format!("{base}/file.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let validators = ContentValidators::default();
        let outcome = fetch_range(fetch_req(
            &client,
            &url,
            RangeSpec::Open { start: 10 },
            &validators,
            None,
            true,
            &control,
        ))
        .await
        .expect("fetch");
        assert_eq!(
            outcome.status,
            RangeStatus::Other {
                status: StatusCode::PARTIAL_CONTENT,
                retryable: false,
            }
        );
    }

    #[tokio::test]
    async fn fetch_range_200_on_nonzero_range_is_full_entity() {
        let body = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Content-Length: 4\r\n\
\r\nabcd"
            .to_string();
        let (base, _reqs, _handle) = spawn_scripted_server(vec![body]).await;
        let url = format!("{base}/file.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let validators = ContentValidators::default();
        let outcome = fetch_range(fetch_req(
            &client,
            &url,
            RangeSpec::Open { start: 8 },
            &validators,
            None,
            true,
            &control,
        ))
        .await
        .expect("fetch");
        assert_eq!(outcome.status, RangeStatus::FullEntityWhenRangeRequested);
    }

    #[tokio::test]
    async fn fetch_range_pinned_redirect_does_not_follow() {
        let redirect = "HTTP/1.1 302 Found\r\n\
Connection: close\r\n\
Location: /other.bin\r\n\
Content-Length: 0\r\n\
\r\n"
            .to_string();
        let (base, _reqs, _handle) = spawn_scripted_server(vec![redirect]).await;
        let url = format!("{base}/file.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let validators = ContentValidators::default();
        let outcome = fetch_range(fetch_req(
            &client,
            &url,
            RangeSpec::Closed { start: 0, end: 9 },
            &validators,
            None,
            false,
            &control,
        ))
        .await
        .expect("fetch");
        assert_eq!(outcome.status, RangeStatus::RedirectWhenPinned);
        assert_eq!(outcome.final_url, url);
    }

    #[tokio::test]
    async fn handoff_cookie_follows_same_origin_redirect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let redirect = format!(
            "HTTP/1.1 302 Found\r\n\
Connection: close\r\n\
Location: {base}/final.bin\r\n\
Content-Length: 0\r\n\
\r\n"
        );
        let final_body = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Content-Length: 4\r\n\
\r\nabcd"
            .to_string();

        let _handle = tokio::spawn(async move {
            let mut replies = vec![redirect, final_body].into_iter();
            for _ in 0..2 {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
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
                let _ = tx.send(String::from_utf8_lossy(&collected).to_string());
                if let Some(reply) = replies.next() {
                    let _ = socket.write_all(reply.as_bytes()).await;
                }
                let _ = socket.shutdown().await;
            }
        });

        let start = format!("{base}/start.bin");
        let expected_final = format!("{base}/final.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let validators = ContentValidators::default();
        let auth = HandoffAuth {
            headers: vec![HandoffAuthHeader {
                name: "Cookie".into(),
                value: "sid=abc123".into(),
            }],
        };
        let outcome = fetch_range(FetchRequest {
            client: &client,
            job_url: &start,
            url: &start,
            range: RangeSpec::Open { start: 0 },
            validators: &validators,
            handoff: Some(&auth),
            follow_redirects: true,
            control: &control,
        })
        .await
        .expect("fetch");
        assert_eq!(outcome.final_url, expected_final);
        assert_eq!(outcome.status, RangeStatus::OkFromZero);

        let first = rx.recv().await.expect("first hop");
        let second = rx.recv().await.expect("second hop");
        assert!(
            first.to_ascii_lowercase().contains("cookie: sid=abc123"),
            "same-origin first hop must keep Cookie:\n{first}"
        );
        assert!(
            second.to_ascii_lowercase().contains("cookie: sid=abc123"),
            "same-origin redirect hop must keep Cookie:\n{second}"
        );
    }

    #[tokio::test]
    async fn handoff_cookie_stripped_on_cross_origin_request() {
        let body = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Content-Length: 1\r\n\
\r\nx"
            .to_string();
        let (base, mut reqs, _handle) = spawn_scripted_server(vec![body]).await;
        let url = format!("{base}/file.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let validators = ContentValidators::default();
        let auth = HandoffAuth {
            headers: vec![HandoffAuthHeader {
                name: "Cookie".into(),
                value: "sid=secret".into(),
            }],
        };
        let job_url = "https://cdn.example.com/file.bin";
        let _ = fetch_range(FetchRequest {
            client: &client,
            job_url,
            url: &url,
            range: RangeSpec::Open { start: 0 },
            validators: &validators,
            handoff: Some(&auth),
            follow_redirects: true,
            control: &control,
        })
        .await
        .expect("fetch");
        let recorded = reqs.recv().await.expect("request");
        assert!(
            !recorded.to_ascii_lowercase().contains("cookie:"),
            "cross-origin must strip Cookie:\n{recorded}"
        );
    }
}
