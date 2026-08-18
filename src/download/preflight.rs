//! HEAD + Range probes (size, real Range support, validators, URL pin).
//!
//! Shares the transfer request builder so browser handoff headers apply the same way
//! as the download path. Best-effort: failures return `None` and never block enqueue.
//!
//! Multi-segment is allowed only after a GET `Range` returns 206 — HEAD
//! `Accept-Ranges: bytes` is not enough (many hosts lie). A second GET at a
//! non-zero offset confirms later segments can resume.

use reqwest::header::{
    ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, ETAG, LAST_MODIFIED,
    LOCATION,
};
use reqwest::{Client, StatusCode};
use std::sync::atomic::AtomicU8;

use super::fetch::{
    build_transfer_request, control_outcome, resolve_redirect_location, send_following_redirects,
    TransferRequestKind, PREFLIGHT_TIMEOUT,
};
use super::filesystem::{parse_content_disposition_filename, parse_content_range};
use super::handoff::HandoffAuth;

/// Planner / transfer input from a successful preflight probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightInfo {
    pub total_bytes: Option<u64>,
    pub filename: Option<String>,
    /// `None` = unknown (header absent and probe inconclusive).
    pub accept_ranges: Option<bool>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Final URL after following redirects (pin for multi / reconnect).
    pub final_url: String,
}

/// How aggressively preflight should prove Range support.
///
/// Transfer entry skips body probes when a v1 map is missing/inconsistent
/// (never invent Range / fetch a body) and skips confirmation when the
/// planner cannot choose multi (legacy v0 partial, or size already below min).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreflightPlan {
    /// HEAD only — no GET (resume-required jobs).
    pub skip_range_probes: bool,
    /// Confirm GET `0-0` / `1-1` when multi might still qualify.
    pub prove_ranges: bool,
    /// Used to skip confirmation after HEAD already shows `size < min`.
    pub multi_min_bytes: u64,
}

impl Default for PreflightPlan {
    fn default() -> Self {
        Self {
            skip_range_probes: false,
            prove_ranges: true,
            multi_min_bytes: 0,
        }
    }
}

/// Run HEAD (8s), then confirm Range with GET probes.
///
/// Uses the shared transfer request builder (handoff + referer + identity). Returns
/// `None` on network/HTTP failure so the transfer can still start without preflight.
///
/// HEAD transport failure still attempts a Range probe from `start_url` (some CDNs
/// mishandle HEAD while Range GET works). Filename is only from Content-Disposition
/// when present — never URL-derived (GET path owns rename + path update).
///
/// `Accept-Ranges: none` skips Range confirmation. Otherwise GET `bytes=0-0`
/// must be 206 to claim ranges; a 200 means the server ignored Range. When
/// size is known, at or above `multi_min_bytes`, and greater than 1 byte,
/// GET `bytes=1-1` must also be 206 — a 200 on either probe disables multi
/// (single-stream can still start from zero). Other probe statuses are
/// inconclusive and leave the HEAD / 0-0 result unchanged.
pub async fn run_preflight(
    client: &Client,
    job_url: &str,
    start_url: &str,
    handoff_auth: Option<&HandoffAuth>,
    control: &AtomicU8,
) -> Option<PreflightInfo> {
    run_preflight_planned(
        client,
        job_url,
        start_url,
        handoff_auth,
        control,
        PreflightPlan::default(),
    )
    .await
}

pub async fn run_preflight_planned(
    client: &Client,
    job_url: &str,
    start_url: &str,
    handoff_auth: Option<&HandoffAuth>,
    control: &AtomicU8,
    plan: PreflightPlan,
) -> Option<PreflightInfo> {
    let head_result = send_with_redirects(
        client,
        TransferRequestKind::Head,
        job_url,
        start_url,
        handoff_auth,
        control,
    )
    .await;

    if control_outcome(control).is_some() {
        return None;
    }

    // Prefer Content-Length header: `Response::content_length()` is body size_hint
    // (empty/unknown for HEAD), not the advertised entity length.
    let mut total_bytes = None;
    let mut accept_ranges = None;
    let mut etag = None;
    let mut last_modified = None;
    // CD only — never URL-derive (would clobber uniquified / user-chosen names).
    let mut filename = None;
    let mut resolved = start_url.to_string();
    let mut head_ok = false;

    if let Some((head_response, final_url)) = head_result {
        resolved = final_url;
        let status = head_response.status();
        // Some CDNs reject HEAD; fall through to Range probe from the last URL.
        head_ok = status.is_success();
        if head_ok {
            total_bytes = content_length_header(head_response.headers());
            accept_ranges = parse_accept_ranges(head_response.headers());
            etag = header_string(head_response.headers(), ETAG);
            last_modified = header_string(head_response.headers(), LAST_MODIFIED);
            filename = head_response
                .headers()
                .get(CONTENT_DISPOSITION)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_disposition_filename);
        }
        drop(head_response);
    }
    // HEAD transport failure: still try Range 0-0 from start_url (resolved unchanged).

    if plan.skip_range_probes {
        if !head_ok {
            return None;
        }
    } else {
        // Confirm Range unless HEAD already said `none`, the file is already
        // below min, or the planner cannot choose multi. Size-only hosts still
        // probe so we can read Content-Range / Content-Length.
        let below_min = total_bytes.is_some_and(|n| n < plan.multi_min_bytes);
        let need_zero_probe = total_bytes.is_none()
            || (plan.prove_ranges && !below_min && accept_ranges != Some(false));

        if need_zero_probe {
            if let Some((probe_response, probe_url)) = send_with_redirects(
                client,
                TransferRequestKind::RangeProbe,
                job_url,
                &resolved,
                handoff_auth,
                control,
            )
            .await
            {
                resolved = probe_url;
                apply_zero_range_probe(
                    &probe_response,
                    &mut total_bytes,
                    &mut accept_ranges,
                    &mut etag,
                    &mut last_modified,
                    &mut filename,
                );
                drop(probe_response);
            } else if !head_ok {
                // Neither HEAD (success or transport) nor probe succeeded.
                return None;
            }
        } else if !head_ok {
            return None;
        }

        // Multi needs a non-zero Range (later segments never start at 0).
        // Only a 200 (ignored Range) disables multi; 403/5xx is inconclusive.
        let below_min = total_bytes.is_some_and(|n| n < plan.multi_min_bytes);
        if plan.prove_ranges
            && !below_min
            && accept_ranges == Some(true)
            && total_bytes.is_some_and(|n| n > 1)
            && control_outcome(control).is_none()
        {
            if let Some((mid_response, mid_url)) = send_with_redirects(
                client,
                TransferRequestKind::GetClosed { start: 1, end: 1 },
                job_url,
                &resolved,
                handoff_auth,
                control,
            )
            .await
            {
                resolved = mid_url;
                if mid_response.status() == StatusCode::OK {
                    accept_ranges = Some(false);
                }
                drop(mid_response);
            }
        }
    }

    Some(PreflightInfo {
        total_bytes,
        filename,
        accept_ranges,
        etag,
        last_modified,
        final_url: resolved,
    })
}

fn apply_zero_range_probe(
    probe_response: &reqwest::Response,
    total_bytes: &mut Option<u64>,
    accept_ranges: &mut Option<bool>,
    etag: &mut Option<String>,
    last_modified: &mut Option<String>,
    filename: &mut Option<String>,
) {
    let probe_status = probe_response.status();
    if probe_status == StatusCode::PARTIAL_CONTENT {
        *accept_ranges = Some(true);
    } else if probe_status.is_success() {
        // 200 = full entity; server ignored Range.
        *accept_ranges = Some(false);
    } else if accept_ranges.is_none() {
        if let Some(ar) = parse_accept_ranges(probe_response.headers()) {
            *accept_ranges = Some(ar);
        }
    }

    if total_bytes.is_none() {
        if let Some(content_range) = probe_response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
        {
            if let Some((_start, _end, Some(total))) = parse_content_range(content_range) {
                if total > 0 {
                    *total_bytes = Some(total);
                }
            }
        }
        if total_bytes.is_none() && probe_status.is_success() {
            *total_bytes = content_length_header(probe_response.headers());
        }
    }

    if etag.is_none() {
        *etag = header_string(probe_response.headers(), ETAG);
    }
    if last_modified.is_none() {
        *last_modified = header_string(probe_response.headers(), LAST_MODIFIED);
    }
    if filename.is_none() {
        *filename = probe_response
            .headers()
            .get(CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_disposition_filename);
    }
}

/// Parse `Accept-Ranges`: `bytes` → true, `none` → false, absent/other → unknown.
pub fn parse_accept_ranges(headers: &reqwest::header::HeaderMap) -> Option<bool> {
    let value = headers.get(ACCEPT_RANGES)?.to_str().ok()?;
    let lower = value.to_ascii_lowercase();
    let tokens: Vec<&str> = lower
        .split([',', ' '])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.iter().any(|t| *t == "bytes") {
        Some(true)
    } else if tokens.iter().any(|t| *t == "none") {
        Some(false)
    } else {
        None
    }
}

fn header_string(
    headers: &reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

fn content_length_header(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
}

/// Find the session gateway's Location without requesting the signed hop.
///
/// Canvas `/files/:id/download` 302s to a one-time Inst-FS / Drive URL. Following
/// that hop here (HEAD or Range 0-0) consumes the token; the transfer GET then
/// 401s. Callers pin the Location and let the first transfer request use it.
pub async fn discover_handoff_location(
    client: &Client,
    job_url: &str,
    start_url: &str,
    handoff_auth: Option<&HandoffAuth>,
    control: &AtomicU8,
) -> Option<String> {
    for kind in [TransferRequestKind::Head, TransferRequestKind::RangeProbe] {
        if control_outcome(control).is_some() {
            return None;
        }
        let Ok(response) = build_transfer_request(client, kind, job_url, start_url, handoff_auth)
            .timeout(PREFLIGHT_TIMEOUT)
            .send()
            .await
        else {
            continue;
        };
        let status = response.status();
        if status.is_redirection() {
            let Some(location) = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
            else {
                continue;
            };
            let Ok(next) = resolve_redirect_location(start_url, location) else {
                continue;
            };
            if next != start_url {
                return Some(next);
            }
            return None;
        }
        if status.is_success() {
            // File is at the session URL itself — no one-time hop to protect.
            return None;
        }
    }
    None
}

async fn send_with_redirects(
    client: &Client,
    kind: TransferRequestKind,
    job_url: &str,
    url: &str,
    handoff_auth: Option<&HandoffAuth>,
    control: &AtomicU8,
) -> Option<(reqwest::Response, String)> {
    send_following_redirects(url, control, |current| {
        let request = build_transfer_request(client, kind, job_url, current, handoff_auth)
            .timeout(PREFLIGHT_TIMEOUT);
        async move {
            request.send().await.map_err(|error| {
                super::job::download_error(
                    super::job::FailureCategory::Network,
                    error.to_string(),
                    true,
                )
            })
        }
    })
    .await
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::client::download_client;
    use crate::download::handoff::{HandoffAuth, HandoffAuthHeader};
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    #[test]
    fn accept_ranges_bytes_true() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(ACCEPT_RANGES, "bytes".parse().unwrap());
        assert_eq!(parse_accept_ranges(&headers), Some(true));
    }

    #[test]
    fn accept_ranges_none_false() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(ACCEPT_RANGES, "none".parse().unwrap());
        assert_eq!(parse_accept_ranges(&headers), Some(false));
    }

    #[test]
    fn accept_ranges_absent_unknown() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(parse_accept_ranges(&headers), None);
    }

    /// Minimal HTTP/1.1 mock: records request lines + headers, serves scripted replies.
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
                // Read until end of headers.
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

    fn range_206(total: u64, start: u64) -> String {
        format!(
            "HTTP/1.1 206 Partial Content\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Range: bytes {start}-{start}/{total}\r\n\
Content-Length: 1\r\n\
\r\nx"
        )
    }

    #[tokio::test]
    async fn discover_handoff_location_does_not_fetch_signed_hop() {
        let redirect = "HTTP/1.1 302 Found\r\n\
Connection: close\r\n\
Location: /signed.bin?token=once\r\n\
Content-Length: 0\r\n\
\r\n"
            .to_string();
        let must_not_hit = "HTTP/1.1 401 Unauthorized\r\n\
Connection: close\r\n\
Content-Length: 0\r\n\
\r\n"
            .to_string();
        let (base, mut reqs, _handle) = spawn_scripted_server(vec![redirect, must_not_hit]).await;
        let start = format!("{base}/files/1/download");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let auth = HandoffAuth {
            headers: vec![HandoffAuthHeader {
                name: "Cookie".into(),
                value: "canvas=1".into(),
            }],
            ..Default::default()
        };

        let location = discover_handoff_location(&client, &start, &start, Some(&auth), &control)
            .await
            .expect("Location");
        assert_eq!(location, format!("{base}/signed.bin?token=once"));

        let first = reqs.recv().await.expect("session request");
        assert!(
            first.starts_with("HEAD ") || first.starts_with("GET "),
            "session hop: {first}"
        );
        assert!(
            first.to_ascii_lowercase().contains("cookie: canvas=1"),
            "session hop must send cookies:\n{first}"
        );
        tokio::time::timeout(std::time::Duration::from_millis(80), reqs.recv())
            .await
            .expect_err("signed hop must not be fetched during discover");
    }

    #[tokio::test]
    async fn apply_preflight_handoff_pins_unfetched_location() {
        use crate::download::bandwidth::GlobalBandwidthLimiter;
        use crate::download::conn_budget::ConnectionBudget;
        use crate::download::context::TransferContext;
        use crate::download::engine::EngineRuntimeConfig;
        use crate::download::http::apply_preflight;
        use crate::download::job::Job;
        use crate::download::progress::{NoopIdentity, TransferEvent};
        use std::path::PathBuf;

        let redirect = "HTTP/1.1 302 Found\r\n\
Connection: close\r\n\
Location: /signed.bin?token=once\r\n\
Content-Length: 0\r\n\
\r\n"
            .to_string();
        let must_not_hit = "HTTP/1.1 401 Unauthorized\r\n\
Connection: close\r\n\
Content-Length: 0\r\n\
\r\n"
            .to_string();
        let (base, mut reqs, _handle) = spawn_scripted_server(vec![redirect, must_not_hit]).await;
        let start = format!("{base}/files/1/download");
        let control = Arc::new(AtomicU8::new(0));
        let on_progress = Arc::new(|_: TransferEvent| {});
        let auth = HandoffAuth {
            headers: vec![HandoffAuthHeader {
                name: "Cookie".into(),
                value: "canvas=1".into(),
            }],
            ..Default::default()
        };
        let job = Job::new(
            start.clone(),
            "file.bin".into(),
            PathBuf::from("C:\\dl\\file.bin"),
            PathBuf::from("C:\\dl\\file.bin.part"),
        );
        let mut ctx = TransferContext::from_runtime(
            job,
            control.clone(),
            on_progress,
            Some(auth),
            GlobalBandwidthLimiter::new(None),
            ConnectionBudget::new(32, 8),
            Arc::new(NoopIdentity),
            &EngineRuntimeConfig::default(),
        );

        let info = apply_preflight(&mut ctx).await;
        assert!(
            info.is_none(),
            "handoff preflight must not consume the signed hop"
        );
        assert_eq!(ctx.resolved_url, format!("{base}/signed.bin?token=once"));
        let first = reqs.recv().await.expect("session request");
        assert!(
            first.starts_with("HEAD ") || first.starts_with("GET "),
            "{first}"
        );
        tokio::time::timeout(std::time::Duration::from_millis(80), reqs.recv())
            .await
            .expect_err("signed hop must stay unused so the transfer GET can use the token");
    }

    #[tokio::test]
    async fn preflight_sends_cookie_handoff_same_origin() {
        let head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: 1024\r\n\
ETag: \"v1\"\r\n\
\r\n"
            .to_string();

        let (base, mut reqs, _handle) =
            spawn_scripted_server(vec![head, range_206(1024, 0), range_206(1024, 1)]).await;
        let url = format!("{base}/file.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let auth = HandoffAuth {
            headers: vec![HandoffAuthHeader {
                name: "Cookie".into(),
                value: "session=abc123".into(),
            }],
            ..Default::default()
        };

        let info = run_preflight(&client, &url, &url, Some(&auth), &control)
            .await
            .expect("preflight ok");

        assert_eq!(info.total_bytes, Some(1024));
        assert_eq!(info.accept_ranges, Some(true));
        assert_eq!(info.etag.as_deref(), Some("\"v1\""));
        assert_eq!(info.final_url, url);

        let recorded = reqs.recv().await.expect("request recorded");
        assert!(
            recorded
                .to_ascii_lowercase()
                .contains("cookie: session=abc123"),
            "expected Cookie handoff on preflight HEAD, got:\n{recorded}"
        );
        assert!(
            recorded.starts_with("HEAD "),
            "expected HEAD method: {recorded}"
        );
    }

    #[tokio::test]
    async fn preflight_strips_cookie_cross_origin() {
        // Job URL is a different origin; request hits mock without Cookie.
        let head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: 50\r\n\
\r\n"
            .to_string();
        let (base, mut reqs, _handle) =
            spawn_scripted_server(vec![head, range_206(50, 0), range_206(50, 1)]).await;
        let request_url = format!("{base}/file.bin");
        // Different host → cross-origin vs request_url.
        let job_url = "https://cdn.example.com/file.bin";
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let auth = HandoffAuth {
            headers: vec![HandoffAuthHeader {
                name: "Cookie".into(),
                value: "session=secret".into(),
            }],
            ..Default::default()
        };

        let info = run_preflight(&client, job_url, &request_url, Some(&auth), &control)
            .await
            .expect("preflight ok");
        assert_eq!(info.total_bytes, Some(50));

        let recorded = reqs.recv().await.expect("request");
        assert!(
            !recorded.to_ascii_lowercase().contains("cookie:"),
            "cross-origin must strip Cookie:\n{recorded}"
        );
    }

    #[tokio::test]
    async fn preflight_pins_final_url_after_redirect() {
        let (base, _reqs, _handle) = {
            // Build replies after we know base — two-phase: bind first.
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let base = format!("http://{addr}");
            let redirect = format!(
                "HTTP/1.1 302 Found\r\n\
Connection: close\r\n\
Location: {base}/final.bin\r\n\
Content-Length: 0\r\n\
\r\n"
            );
            let final_head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: 4096\r\n\
ETag: \"final\"\r\n\
\r\n"
                .to_string();

            let (tx, rx) = mpsc::unbounded_channel::<String>();
            let handle = tokio::spawn(async move {
                let mut replies =
                    vec![redirect, final_head, range_206(4096, 0), range_206(4096, 1)].into_iter();
                for _ in 0..4 {
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
            (base, rx, handle)
        };

        let start = format!("{base}/start.bin");
        let expected_final = format!("{base}/final.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);

        let info = run_preflight(&client, &start, &start, None, &control)
            .await
            .expect("preflight after redirect");

        assert_eq!(info.final_url, expected_final);
        assert_eq!(info.total_bytes, Some(4096));
        assert_eq!(info.accept_ranges, Some(true));
        assert_eq!(info.etag.as_deref(), Some("\"final\""));
    }

    #[tokio::test]
    async fn range_probe_sets_accept_ranges_when_head_unknown() {
        // HEAD without Accept-Ranges / length → Range 0-0 206.
        let head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
\r\n"
            .to_string();
        let probe = "HTTP/1.1 206 Partial Content\r\n\
Connection: close\r\n\
Content-Range: bytes 0-0/8192\r\n\
Content-Length: 1\r\n\
ETag: \"probe\"\r\n\
\r\nx"
            .to_string();

        let (base, mut reqs, _handle) =
            spawn_scripted_server(vec![head, probe, range_206(8192, 1)]).await;
        let url = format!("{base}/blob.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);

        let info = run_preflight(&client, &url, &url, None, &control)
            .await
            .expect("preflight with range probe");

        assert_eq!(info.accept_ranges, Some(true));
        assert_eq!(info.total_bytes, Some(8192));
        assert_eq!(info.etag.as_deref(), Some("\"probe\""));

        let head_req = reqs.recv().await.expect("HEAD");
        assert!(
            head_req.starts_with("HEAD "),
            "first request should be HEAD:\n{head_req}"
        );
        let get_req = reqs.recv().await.expect("GET probe");
        assert!(
            get_req.starts_with("GET "),
            "second request should be GET:\n{get_req}"
        );
        assert!(
            get_req.to_ascii_lowercase().contains("range: bytes=0-0"),
            "expected Range 0-0 probe:\n{get_req}"
        );
    }

    #[tokio::test]
    async fn preflight_final_url_equals_start_when_no_redirect() {
        let head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: 10\r\n\
\r\n"
            .to_string();
        let (base, _reqs, _handle) =
            spawn_scripted_server(vec![head, range_206(10, 0), range_206(10, 1)]).await;
        let url = format!("{base}/x.bin");
        let client = download_client().unwrap();
        let control = Arc::new(AtomicU8::new(0));

        let info = run_preflight(&client, &url, &url, None, &control)
            .await
            .expect("preflight");

        assert_eq!(info.final_url, url);
        assert_eq!(info.total_bytes, Some(10));
        // Pause control is respected as abort → None on next call after set.
        control.store(1, Ordering::Relaxed);
        let aborted = run_preflight(&client, &url, &url, None, &control).await;
        assert!(aborted.is_none());
    }

    /// HEAD transport failure (peer closes without response) still tries Range 0-0.
    #[tokio::test]
    async fn head_transport_fail_still_tries_range_probe() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        let _handle = tokio::spawn(async move {
            // First connection: HEAD — close without writing (transport failure).
            if let Ok((mut socket, _)) = listener.accept().await {
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
                // Drop without response.
                drop(socket);
            }
            // Second connection: Range probe GET → 206.
            if let Ok((mut socket, _)) = listener.accept().await {
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
                let probe = "HTTP/1.1 206 Partial Content\r\n\
Connection: close\r\n\
Content-Range: bytes 0-0/2048\r\n\
Content-Length: 1\r\n\
Accept-Ranges: bytes\r\n\
\r\nx";
                let _ = socket.write_all(probe.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
            // Third connection: non-zero Range confirm.
            if let Ok((mut socket, _)) = listener.accept().await {
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
                let mid = range_206(2048, 1);
                let _ = socket.write_all(mid.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });

        let url = format!("{base}/cdn.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);

        let info = run_preflight(&client, &url, &url, None, &control)
            .await
            .expect("Range probe after HEAD transport fail");

        assert_eq!(info.total_bytes, Some(2048));
        assert_eq!(info.accept_ranges, Some(true));
        assert_eq!(info.final_url, url);

        let head_req = rx.recv().await.expect("HEAD");
        assert!(head_req.starts_with("HEAD "), "got: {head_req}");
        let get_req = rx.recv().await.expect("Range GET");
        assert!(get_req.starts_with("GET "), "got: {get_req}");
        assert!(get_req.to_ascii_lowercase().contains("range: bytes=0-0"));
    }

    /// Full preflight soft-fail (nothing answers) returns None — caller must not hard-error.
    #[tokio::test]
    async fn preflight_transport_fail_returns_none() {
        // Nothing listening — connection refused.
        let url = "http://127.0.0.1:1/nope.bin";
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let info = run_preflight(&client, url, url, None, &control).await;
        assert!(info.is_none(), "preflight must soft-fail, not panic");
    }

    /// Soft-fail path: preflight None, then transfer GET still completes.
    #[tokio::test]
    async fn preflight_soft_fail_does_not_abort_transfer() {
        use crate::download::bandwidth::GlobalBandwidthLimiter;
        use crate::download::conn_budget::ConnectionBudget;
        use crate::download::context::TransferContext;
        use crate::download::engine::EngineRuntimeConfig;
        use crate::download::job::Job;
        use crate::download::progress::{NoopIdentity, TransferEvent};
        use crate::download::transfer::run_transfer;
        use std::path::PathBuf;
        use std::sync::Mutex;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let body = b"hello-soft-fail";

        let _handle = tokio::spawn(async move {
            // Serve a few requests: preflight HEAD (drop), Range probe (drop), then GET body.
            for _ in 0..4 {
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
                let req = String::from_utf8_lossy(&collected).to_string();
                if req.starts_with("HEAD ") {
                    // Transport fail for HEAD.
                    drop(socket);
                    continue;
                }
                if req.to_ascii_lowercase().contains("range: bytes=0-0") {
                    // Fail Range probe too → full preflight None.
                    drop(socket);
                    continue;
                }
                // Full download GET.
                let reply = format!(
                    "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Content-Length: {}\r\n\
Accept-Ranges: bytes\r\n\
\r\n",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(body).await;
                let _ = socket.shutdown().await;
            }
        });

        let dir = std::env::temp_dir().join(format!("rusticdl-pf-soft-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/file.bin");
        let job = Job::new(url, "out.bin".into(), target.clone(), temp.clone());

        let control = Arc::new(AtomicU8::new(0));
        let patches: Arc<Mutex<Vec<TransferEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let patches_cb = patches.clone();
        let on_progress = Arc::new(move |u: TransferEvent| {
            patches_cb.lock().unwrap().push(u);
        });

        let ctx = TransferContext::from_runtime(
            job,
            control,
            on_progress,
            None,
            GlobalBandwidthLimiter::new(None),
            ConnectionBudget::new(32, 8),
            Arc::new(NoopIdentity),
            &EngineRuntimeConfig::default(),
        );
        let outcome = run_transfer(ctx)
            .await
            .expect("transfer should succeed after preflight soft-fail");
        assert!(matches!(
            outcome,
            crate::download::job::DownloadOutcome::Completed
        ));

        let data = std::fs::read(&target).expect("final file");
        assert_eq!(data, body);

        // Preflight produced no patch (None); transfer still emitted progress.
        let seen = patches.lock().unwrap();
        assert!(
            !seen.is_empty(),
            "download path should still publish progress"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn transfer_context_pins_resolved_url_from_preflight_info() {
        use crate::download::bandwidth::GlobalBandwidthLimiter;
        use crate::download::conn_budget::ConnectionBudget;
        use crate::download::context::TransferContext;
        use crate::download::engine::EngineRuntimeConfig;
        use crate::download::job::Job;
        use crate::download::progress::{NoopIdentity, TransferEvent};
        use std::path::PathBuf;

        let head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: 99\r\n\
\r\n"
            .to_string();
        let (base, _reqs, _handle) =
            spawn_scripted_server(vec![head, range_206(99, 0), range_206(99, 1)]).await;
        let url = format!("{base}/pin.bin");
        let client = download_client().unwrap();
        let control = Arc::new(AtomicU8::new(0));
        let on_progress = Arc::new(|_: TransferEvent| {});

        let job = Job::new(
            url.clone(),
            "pin.bin".into(),
            PathBuf::from("C:\\dl\\pin.bin"),
            PathBuf::from("C:\\dl\\pin.bin.part"),
        );
        let mut ctx = TransferContext::from_runtime(
            job,
            control.clone(),
            on_progress,
            None,
            GlobalBandwidthLimiter::new(None),
            ConnectionBudget::new(32, 8),
            Arc::new(NoopIdentity),
            &EngineRuntimeConfig::default(),
        );
        assert_eq!(ctx.resolved_url, url);

        let info = run_preflight(&client, &ctx.job.url, &ctx.resolved_url, None, &ctx.control)
            .await
            .expect("preflight");
        // Same assignment path as run_http_download_with_ctx.
        ctx.resolved_url = info.final_url.clone();
        assert_eq!(ctx.resolved_url, url);
        assert_eq!(info.total_bytes, Some(99));
    }

    #[tokio::test]
    async fn claimed_ranges_but_zero_probe_200_disables_multi() {
        let head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: 10485760\r\n\
\r\n"
            .to_string();
        let ignored = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Content-Length: 10485760\r\n\
\r\n"
            .to_string();
        let (base, mut reqs, _handle) = spawn_scripted_server(vec![head, ignored]).await;
        let url = format!("{base}/lie.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let info = run_preflight(&client, &url, &url, None, &control)
            .await
            .expect("preflight");
        assert_eq!(info.total_bytes, Some(10485760));
        assert_eq!(
            info.accept_ranges,
            Some(false),
            "200 on Range 0-0 must not qualify multi"
        );
        let _ = reqs.recv().await;
        let probe = reqs.recv().await.expect("zero probe");
        assert!(probe.to_ascii_lowercase().contains("range: bytes=0-0"));
    }

    #[tokio::test]
    async fn zero_probe_206_but_mid_probe_200_disables_multi() {
        let head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: 10485760\r\n\
\r\n"
            .to_string();
        let mid_ignored = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Content-Length: 10485760\r\n\
\r\n"
            .to_string();
        let (base, mut reqs, _handle) =
            spawn_scripted_server(vec![head, range_206(10485760, 0), mid_ignored]).await;
        let url = format!("{base}/half-lie.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let info = run_preflight(&client, &url, &url, None, &control)
            .await
            .expect("preflight");
        assert_eq!(info.accept_ranges, Some(false));
        let _ = reqs.recv().await;
        let zero = reqs.recv().await.expect("zero");
        assert!(zero.to_ascii_lowercase().contains("range: bytes=0-0"));
        let mid = reqs.recv().await.expect("mid");
        assert!(mid.to_ascii_lowercase().contains("range: bytes=1-1"));
    }

    #[tokio::test]
    async fn confirmed_zero_and_mid_range_keeps_multi() {
        let head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: 10485760\r\n\
\r\n"
            .to_string();
        let (base, mut reqs, _handle) =
            spawn_scripted_server(vec![head, range_206(10485760, 0), range_206(10485760, 1)]).await;
        let url = format!("{base}/real.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let info = run_preflight(&client, &url, &url, None, &control)
            .await
            .expect("preflight");
        assert_eq!(info.accept_ranges, Some(true));
        let _ = reqs.recv().await;
        let zero = reqs.recv().await.expect("zero");
        let mid = reqs.recv().await.expect("mid");
        assert!(zero.to_ascii_lowercase().contains("range: bytes=0-0"));
        assert!(mid.to_ascii_lowercase().contains("range: bytes=1-1"));
    }

    #[tokio::test]
    async fn skip_range_probes_sends_head_only() {
        let head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: 10485760\r\n\
\r\n"
            .to_string();
        let (base, mut reqs, _handle) = spawn_scripted_server(vec![head]).await;
        let url = format!("{base}/resume.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let info = run_preflight_planned(
            &client,
            &url,
            &url,
            None,
            &control,
            PreflightPlan {
                skip_range_probes: true,
                prove_ranges: false,
                multi_min_bytes: 1,
            },
        )
        .await
        .expect("preflight");
        assert_eq!(info.total_bytes, Some(10485760));
        assert_eq!(info.accept_ranges, Some(true));
        let recorded = reqs.recv().await.expect("HEAD");
        assert!(recorded.starts_with("HEAD "), "got {recorded}");
        let extra = tokio::time::timeout(std::time::Duration::from_millis(150), reqs.recv()).await;
        assert!(
            extra.is_err(),
            "resume-required must not GET a body, got {extra:?}"
        );
    }

    #[tokio::test]
    async fn below_min_after_head_skips_range_proof() {
        let head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: 64\r\n\
\r\n"
            .to_string();
        let (base, mut reqs, _handle) = spawn_scripted_server(vec![head]).await;
        let url = format!("{base}/tiny.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let info = run_preflight_planned(
            &client,
            &url,
            &url,
            None,
            &control,
            PreflightPlan {
                skip_range_probes: false,
                prove_ranges: true,
                multi_min_bytes: 5 * 1024 * 1024,
            },
        )
        .await
        .expect("preflight");
        assert_eq!(info.total_bytes, Some(64));
        assert_eq!(info.accept_ranges, Some(true));
        let recorded = reqs.recv().await.expect("HEAD");
        assert!(recorded.starts_with("HEAD "));
        let extra = tokio::time::timeout(std::time::Duration::from_millis(150), reqs.recv()).await;
        assert!(
            extra.is_err(),
            "size below min must not prove Range, got {extra:?}"
        );
    }

    #[tokio::test]
    async fn mid_probe_403_keeps_head_ranges() {
        let head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: 10485760\r\n\
\r\n"
            .to_string();
        let forbidden = "HTTP/1.1 403 Forbidden\r\n\
Connection: close\r\n\
Content-Length: 0\r\n\
\r\n"
            .to_string();
        let (base, mut reqs, _handle) =
            spawn_scripted_server(vec![head, range_206(10485760, 0), forbidden]).await;
        let url = format!("{base}/403.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let info = run_preflight(&client, &url, &url, None, &control)
            .await
            .expect("preflight");
        assert_eq!(
            info.accept_ranges,
            Some(true),
            "403 on 1-1 is inconclusive; keep 0-0 / HEAD ranges"
        );
        let _ = reqs.recv().await;
        let _ = reqs.recv().await;
        let mid = reqs.recv().await.expect("mid");
        assert!(mid.to_ascii_lowercase().contains("range: bytes=1-1"));
    }
}
