//! HEAD + optional Range 0-0 preflight (size, Accept-Ranges, validators, URL pin).
//!
//! Shares the transfer request builder so browser handoff headers apply the same way
//! as the download path. Best-effort: failures return `None` and never block enqueue.

use reqwest::header::{
    ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, ETAG, LAST_MODIFIED,
};
use reqwest::{Client, StatusCode};
use std::sync::atomic::AtomicU8;

use super::filesystem::{
    derive_filename_from_url, parse_content_disposition_filename, parse_content_range,
};
use super::handoff::HandoffAuth;
use super::http::{
    build_transfer_request, control_outcome, resolve_redirect_location, TransferRequestKind,
    MAX_REDIRECTS, PREFLIGHT_TIMEOUT,
};

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

/// Run HEAD (8s), then GET `Range: bytes=0-0` when length or Accept-Ranges is unknown.
///
/// Uses the shared transfer request builder (handoff + referer + identity). Returns
/// `None` on network/HTTP failure so the transfer can still start without preflight.
pub async fn run_preflight(
    client: &Client,
    job_url: &str,
    start_url: &str,
    handoff_auth: Option<&HandoffAuth>,
    control: &AtomicU8,
) -> Option<PreflightInfo> {
    let (head_response, final_url) = send_with_redirects(
        client,
        TransferRequestKind::Head,
        job_url,
        start_url,
        handoff_auth,
        control,
    )
    .await?;

    if control_outcome(control).is_some() {
        return None;
    }

    let status = head_response.status();
    // Some CDNs reject HEAD; fall through to Range probe from the last URL.
    let head_ok = status.is_success();

    // Prefer Content-Length header: `Response::content_length()` is body size_hint
    // (empty/unknown for HEAD), not the advertised entity length.
    let mut total_bytes = if head_ok {
        content_length_header(head_response.headers())
    } else {
        None
    };
    let mut accept_ranges = if head_ok {
        parse_accept_ranges(head_response.headers())
    } else {
        None
    };
    let mut etag = if head_ok {
        header_string(head_response.headers(), ETAG)
    } else {
        None
    };
    let mut last_modified = if head_ok {
        header_string(head_response.headers(), LAST_MODIFIED)
    } else {
        None
    };
    let mut filename = if head_ok {
        head_response
            .headers()
            .get(CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_disposition_filename)
    } else {
        None
    };

    let need_probe = total_bytes.is_none() || accept_ranges.is_none();
    let mut resolved = final_url;

    if need_probe {
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
            let probe_status = probe_response.status();

            if probe_status == StatusCode::PARTIAL_CONTENT {
                // 206 on Range 0-0 proves byte ranges work.
                accept_ranges = Some(true);
            } else if accept_ranges.is_none() {
                // Explicit Accept-Ranges on probe response, if any.
                if let Some(ar) = parse_accept_ranges(probe_response.headers()) {
                    accept_ranges = Some(ar);
                }
            }

            if total_bytes.is_none() {
                if let Some(content_range) = probe_response
                    .headers()
                    .get(CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                {
                    if let Some((_start, _end, total)) = parse_content_range(content_range) {
                        if total > 0 {
                            total_bytes = Some(total);
                        }
                    }
                }
                // Full 200 with Content-Length (server ignored Range).
                if total_bytes.is_none() && probe_status.is_success() {
                    total_bytes = content_length_header(probe_response.headers());
                }
            }

            if etag.is_none() {
                etag = header_string(probe_response.headers(), ETAG);
            }
            if last_modified.is_none() {
                last_modified = header_string(probe_response.headers(), LAST_MODIFIED);
            }
            if filename.is_none() {
                filename = probe_response
                    .headers()
                    .get(CONTENT_DISPOSITION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_content_disposition_filename);
            }

            // Drop body (1 byte or more); we only needed headers.
            drop(probe_response);
        } else if !head_ok {
            // Neither HEAD nor probe succeeded.
            return None;
        }
    } else {
        drop(head_response);
    }

    if filename.is_none() {
        filename = derive_filename_from_url(&resolved);
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

async fn send_with_redirects(
    client: &Client,
    kind: TransferRequestKind,
    job_url: &str,
    url: &str,
    handoff_auth: Option<&HandoffAuth>,
    control: &AtomicU8,
) -> Option<(reqwest::Response, String)> {
    let mut current = url.to_string();
    let mut redirects = 0u32;

    loop {
        if control_outcome(control).is_some() {
            return None;
        }

        let response = build_transfer_request(client, kind, job_url, &current, handoff_auth)
            .timeout(PREFLIGHT_TIMEOUT)
            .send()
            .await
            .ok()?;

        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())?;
            let next = resolve_redirect_location(&current, location).ok()?;
            redirects += 1;
            if redirects > MAX_REDIRECTS {
                return None;
            }
            current = next;
            continue;
        }

        return Some((response, current));
    }
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

    #[tokio::test]
    async fn preflight_sends_cookie_handoff_same_origin() {
        let head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: 1024\r\n\
ETag: \"v1\"\r\n\
\r\n"
            .to_string();

        let (base, mut reqs, _handle) = spawn_scripted_server(vec![head]).await;
        let url = format!("{base}/file.bin");
        let client = download_client().unwrap();
        let control = AtomicU8::new(0);
        let auth = HandoffAuth {
            headers: vec![HandoffAuthHeader {
                name: "Cookie".into(),
                value: "session=abc123".into(),
            }],
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
        let (base, mut reqs, _handle) = spawn_scripted_server(vec![head]).await;
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
                let mut replies = vec![redirect, final_head].into_iter();
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

        let (base, mut reqs, _handle) = spawn_scripted_server(vec![head, probe]).await;
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
    async fn transfer_context_pins_resolved_url_from_preflight() {
        let head = "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: 10\r\n\
\r\n"
            .to_string();
        let (base, _reqs, _handle) = spawn_scripted_server(vec![head]).await;
        // Redirect-less; pin still equals final.
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
}
