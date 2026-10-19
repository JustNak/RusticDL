//! Engine-path coverage for a non-identity `Content-Encoding`.
//!
//! Each case enqueues through `spawn_engine` against a `std::net` HTTP/1.1
//! server on `127.0.0.1`. The plaintext fixture and its gzip twin are the
//! bytes from the #188 triage.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use super::{spawn_engine, EngineCommand, EngineEvent, EngineRuntimeConfig, MemoryJobStore};
use crate::download::job::{FailureCategory, Job, JobState};
use crate::download::{EnqueueStatus, FilenameConflictPolicy};

const PLAIN: &[u8] = b"RusticDL plaintext payload 0123456789";
const PLAIN_SHA: &str = "c9c378aa019d42921e2fb10dce2c6a7818e206bdf2a091465b8015cc739bb9b4";
const GZIP_SHA: &str = "aed03d9dd4cdbc665fc3098ee5b5139a15d862adbaeea3aa62a16b813c3e01ac";
const GZIP_BODY: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0x0b, 0x2a, 0x2d, 0x2e, 0xc9, 0x4c,
    0x76, 0xf1, 0x51, 0x28, 0xc8, 0x49, 0xcc, 0xcc, 0x2b, 0x49, 0xad, 0x28, 0x51, 0x28, 0x48, 0xac,
    0xcc, 0xc9, 0x4f, 0x4c, 0x51, 0x30, 0x30, 0x34, 0x32, 0x36, 0x31, 0x35, 0x33, 0xb7, 0xb0, 0x04,
    0x00, 0x2b, 0xcb, 0x05, 0x3f, 0x25, 0x00, 0x00, 0x00,
];
const ENCODING_ERROR: &str = "Response is Content-Encoding: gzip; asked for identity. Use a URL that sends the decoded file.";

const RESUME_PREFIX: usize = 16;

enum Script {
    /// HEAD and GET share one body and one optional Content-Encoding.
    Uniform {
        encoding: Option<&'static str>,
        body: &'static [u8],
    },
    /// HEAD advertises the plaintext length. GET is the gzip body.
    HeadPlainGetEncoded {
        encoding: &'static str,
        head_len: u64,
        body: &'static [u8],
    },
    /// First GET writes an identity prefix and closes early. The Range GET is gzip.
    ResumeGzip,
    /// Two segments. Probes stay identity. Segment bodies are gzip-labeled.
    MultiGzip { total: u64 },
    /// HEAD and GET share a body labeled with Transfer-Encoding and no Content-Encoding.
    /// `chunk_wire` sends `body` as one HTTP chunk; otherwise the bytes are raw.
    /// HEAD's Content-Length is `body.len()` either way.
    TransferEncoded {
        values: &'static [&'static str],
        body: &'static [u8],
        chunk_wire: bool,
    },
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn file_sha(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => sha256_hex(&bytes),
        Err(_) => "-".into(),
    }
}

struct DirCleanup(std::path::PathBuf);

impl Drop for DirCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn read_headers(socket: &mut TcpStream) -> Option<String> {
    let mut collected = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if collected.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if collected.len() > 64 * 1024 {
            return None;
        }
        match socket.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => collected.extend_from_slice(&buf[..n]),
        }
    }
    if !collected.windows(4).any(|window| window == b"\r\n\r\n") {
        return None;
    }
    Some(String::from_utf8_lossy(&collected).into_owned())
}

fn header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    for line in request.split("\r\n") {
        let Some((raw_name, value)) = line.split_once(':') else {
            continue;
        };
        if raw_name.trim().eq_ignore_ascii_case(name) {
            return Some(value.trim());
        }
    }
    None
}

fn request_range(request: &str) -> Option<(u64, Option<u64>)> {
    let value = header_value(request, "range")?;
    let rest = value
        .strip_prefix("bytes=")
        .or_else(|| value.strip_prefix("BYTES="))?;
    let (start, end) = rest.split_once('-')?;
    let start = start.trim().parse().ok()?;
    let end = end.trim();
    let end = if end.is_empty() {
        None
    } else {
        Some(end.parse().ok()?)
    };
    Some((start, end))
}

fn summary(request: &str) -> String {
    let method = request.split_whitespace().next().unwrap_or("?");
    let range = header_value(request, "range").unwrap_or("-");
    let accept = header_value(request, "accept-encoding").unwrap_or("-");
    format!("{method} range={range} accept-encoding={accept}")
}

async fn write_all_or_stop(socket: &mut TcpStream, bytes: &[u8]) {
    let _ = socket.write_all(bytes).await;
}

async fn write_repeat(socket: &mut TcpStream, byte: u8, len: u64) {
    let chunk = vec![byte; 64 * 1024];
    let mut left = len;
    while left > 0 {
        let n = (chunk.len() as u64).min(left) as usize;
        if socket.write_all(&chunk[..n]).await.is_err() {
            return;
        }
        left -= n as u64;
    }
}

fn chunk_encode(body: &[u8]) -> Vec<u8> {
    let mut wire = format!("{:x}\r\n", body.len()).into_bytes();
    wire.extend_from_slice(body);
    wire.extend_from_slice(b"\r\n0\r\n\r\n");
    wire
}

fn response_head(status: &str, extra: &str, content_length: u64) -> String {
    let mut head = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
    if !extra.is_empty() {
        head.push_str(extra);
        if !extra.ends_with("\r\n") {
            head.push_str("\r\n");
        }
    }
    head.push_str(&format!("Content-Length: {content_length}\r\n\r\n"));
    head
}

async fn serve_one(
    mut socket: TcpStream,
    script: Arc<Script>,
    resume_sent: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<String>>>,
) {
    let Some(request) = read_headers(&mut socket).await else {
        return;
    };
    seen.lock().await.push(summary(&request));
    let method = request.split_whitespace().next().unwrap_or("");
    match script.as_ref() {
        Script::Uniform { encoding, body } => {
            let mut extra = String::new();
            if let Some(encoding) = encoding {
                extra.push_str(&format!("Content-Encoding: {encoding}\r\n"));
            }
            let head = response_head("200 OK", &extra, body.len() as u64);
            write_all_or_stop(&mut socket, head.as_bytes()).await;
            if method == "GET" {
                write_all_or_stop(&mut socket, body).await;
            }
        }
        Script::HeadPlainGetEncoded {
            encoding,
            head_len,
            body,
        } => {
            if method == "HEAD" {
                let head = response_head("200 OK", "", *head_len);
                write_all_or_stop(&mut socket, head.as_bytes()).await;
            } else if method == "GET" {
                let extra = format!("Content-Encoding: {encoding}\r\n");
                let head = response_head("200 OK", &extra, body.len() as u64);
                write_all_or_stop(&mut socket, head.as_bytes()).await;
                write_all_or_stop(&mut socket, body).await;
            }
        }
        Script::ResumeGzip => {
            if method == "HEAD" {
                let extra = "Accept-Ranges: bytes\r\n";
                let head = response_head("200 OK", extra, PLAIN.len() as u64);
                write_all_or_stop(&mut socket, head.as_bytes()).await;
            } else if method == "GET" {
                let start = request_range(&request).map(|(start, _)| start).unwrap_or(0);
                let first = start == 0 && !resume_sent.swap(true, Ordering::SeqCst);
                if first {
                    let extra = "Accept-Ranges: bytes\r\n";
                    let head = response_head("200 OK", extra, PLAIN.len() as u64);
                    write_all_or_stop(&mut socket, head.as_bytes()).await;
                    write_all_or_stop(&mut socket, &PLAIN[..RESUME_PREFIX]).await;
                } else {
                    let end = PLAIN.len().saturating_sub(1) as u64;
                    let len = end.saturating_sub(start).saturating_add(1);
                    let extra = format!(
                        "Accept-Ranges: bytes\r\nContent-Encoding: gzip\r\nContent-Range: bytes {start}-{end}/{}\r\n",
                        PLAIN.len()
                    );
                    let head = response_head("206 Partial Content", &extra, len);
                    write_all_or_stop(&mut socket, head.as_bytes()).await;
                    write_repeat(&mut socket, 0x1f, len).await;
                }
            }
        }
        Script::MultiGzip { total } => {
            if method == "HEAD" {
                let extra = "Accept-Ranges: bytes\r\n";
                let head = response_head("200 OK", extra, *total);
                write_all_or_stop(&mut socket, head.as_bytes()).await;
            } else if method == "GET" {
                match request_range(&request) {
                    Some((0, Some(0))) | Some((1, Some(1))) => {
                        let (start, end) = request_range(&request).unwrap();
                        let end = end.unwrap();
                        let extra = format!(
                            "Accept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{total}\r\n"
                        );
                        let head = response_head("206 Partial Content", &extra, 1);
                        write_all_or_stop(&mut socket, head.as_bytes()).await;
                        write_all_or_stop(&mut socket, b"x").await;
                    }
                    Some((start, end)) => {
                        let end = end.unwrap_or(total.saturating_sub(1));
                        let len = end.saturating_sub(start).saturating_add(1);
                        let extra = format!(
                            "Accept-Ranges: bytes\r\nContent-Encoding: gzip\r\nContent-Range: bytes {start}-{end}/{total}\r\n"
                        );
                        let head = response_head("206 Partial Content", &extra, len);
                        write_all_or_stop(&mut socket, head.as_bytes()).await;
                        write_repeat(&mut socket, b'Z', len).await;
                    }
                    None => {
                        let extra = "Content-Encoding: gzip\r\n";
                        let head = response_head("200 OK", extra, *total);
                        write_all_or_stop(&mut socket, head.as_bytes()).await;
                        write_repeat(&mut socket, b'Z', *total).await;
                    }
                }
            }
        }
        Script::TransferEncoded {
            values,
            body,
            chunk_wire,
        } => {
            let mut extra = String::new();
            for value in *values {
                extra.push_str("Transfer-Encoding: ");
                extra.push_str(value);
                extra.push_str("\r\n");
            }
            let wire = if *chunk_wire {
                chunk_encode(body)
            } else {
                body.to_vec()
            };
            let head = response_head("200 OK", &extra, body.len() as u64);
            write_all_or_stop(&mut socket, head.as_bytes()).await;
            if method == "GET" {
                write_all_or_stop(&mut socket, &wire).await;
            }
        }
    }
    let _ = socket.shutdown().await;
}

struct Run {
    job: Job,
    requests: Vec<String>,
    /// Keeps the download directory until the test has read the files.
    _cleanup: DirCleanup,
}

async fn run_case(script: Script, config: EngineRuntimeConfig, timeout: Duration) -> Run {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_task = seen.clone();
    let script = Arc::new(script);
    let resume_sent = Arc::new(AtomicBool::new(false));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let script = script.clone();
            let resume_sent = resume_sent.clone();
            let seen_task = seen_task.clone();
            tokio::spawn(async move {
                serve_one(socket, script, resume_sent, seen_task).await;
            });
        }
    });

    let dir = std::env::temp_dir().join(format!("rusticdl-ce-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let cleanup = DirCleanup(dir.clone());
    let url = format!("http://{addr}/payload.bin");
    let (handle, mut events) =
        spawn_engine(Vec::new(), config, Arc::new(MemoryJobStore::default()));
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    handle.send(EngineCommand::Add {
        url,
        filename: Some("payload.bin".into()),
        directory: dir,
        handoff_auth: None,
        conflict: FilenameConflictPolicy::Uniquify,
        total_bytes: None,
        reply: Some(reply_tx),
    });
    let outcome = tokio::time::timeout(Duration::from_secs(2), reply_rx)
        .await
        .expect("enqueue reply")
        .expect("enqueue dropped");
    assert_eq!(outcome.status, EnqueueStatus::Queued, "{outcome:?}");
    let job_id = outcome.job_id.clone();
    let job = tokio::time::timeout(timeout, async {
        loop {
            match events.recv().await {
                Some(EngineEvent::JobsChanged(jobs)) => {
                    if let Some(job) = jobs.iter().find(|job| job.id == job_id) {
                        if job.state.is_terminal() {
                            break job.clone();
                        }
                    }
                }
                Some(EngineEvent::Toast(_)) => {}
                None => panic!("engine event channel closed"),
            }
        }
    })
    .await
    .expect("download did not reach a terminal state");
    handle.send(EngineCommand::Shutdown);
    server.abort();
    let requests = seen.lock().await.clone();
    assert!(
        requests
            .iter()
            .all(|line| line.contains("accept-encoding=identity")),
        "client must keep asking for identity: {requests:?}"
    );
    Run {
        job,
        requests,
        _cleanup: cleanup,
    }
}

fn default_engine() -> EngineRuntimeConfig {
    // Default auto_retry is 6. Leave it there so a retryable error cannot
    // pass a retry_attempts == 0 assertion.
    EngineRuntimeConfig::default()
}

fn fail_context(job: &Job) -> String {
    format!(
        "state={:?} category={:?} retry={} error={:?} target_exists={} target_sha={} temp_exists={} temp_len={}",
        job.state,
        job.failure_category,
        job.retry_attempts,
        job.error,
        job.target_path.is_file(),
        file_sha(&job.target_path),
        job.temp_path.is_file(),
        std::fs::metadata(&job.temp_path).map(|meta| meta.len()).unwrap_or(0),
    )
}

fn assert_named_coding_failure(job: &Job, message: &str) {
    let context = fail_context(job);
    assert_eq!(job.state, JobState::Failed, "{context}");
    assert_eq!(job.error.as_deref(), Some(message), "{context}");
    assert_eq!(
        job.failure_category,
        Some(FailureCategory::Resume),
        "{context}"
    );
    assert_eq!(job.retry_attempts, 0, "{context}");
    assert!(
        !job.target_path.exists(),
        "finished path must stay absent: {context}"
    );
}

fn assert_encoding_failure(job: &Job) {
    assert_named_coding_failure(job, ENCODING_ERROR);
}

async fn encoded_head_and_get(encoding: &'static str) -> Run {
    assert_eq!(GZIP_BODY.len(), 57);
    assert_eq!(sha256_hex(PLAIN), PLAIN_SHA);
    assert_eq!(sha256_hex(GZIP_BODY), GZIP_SHA);
    run_case(
        Script::Uniform {
            encoding: Some(encoding),
            body: GZIP_BODY,
        },
        default_engine(),
        Duration::from_secs(20),
    )
    .await
}

#[tokio::test]
async fn issue188_gzip_on_head_and_get_fails_closed() {
    let run = encoded_head_and_get("gzip").await;
    assert_encoding_failure(&run.job);
    assert!(
        !run.job.temp_path.exists(),
        "fresh failure must not leave a .part: {}",
        fail_context(&run.job)
    );
}

#[tokio::test]
async fn issue188_content_encoding_gzip_uppercase_fails_closed() {
    let run = encoded_head_and_get("GZIP").await;
    assert_encoding_failure(&run.job);
    assert!(!run.job.temp_path.exists(), "{}", fail_context(&run.job));
}

#[tokio::test]
async fn issue188_content_encoding_gzip_identity_list_fails_closed() {
    let run = encoded_head_and_get("gzip, identity").await;
    assert_encoding_failure(&run.job);
    assert!(!run.job.temp_path.exists(), "{}", fail_context(&run.job));
}

#[tokio::test]
async fn issue188_gzip_get_with_identity_sized_head_names_encoding() {
    let run = run_case(
        Script::HeadPlainGetEncoded {
            encoding: "gzip",
            head_len: PLAIN.len() as u64,
            body: GZIP_BODY,
        },
        default_engine(),
        Duration::from_secs(20),
    )
    .await;
    assert_encoding_failure(&run.job);
    assert!(
        !run.job.temp_path.exists(),
        "size-mismatch convention leaves no .part: {}",
        fail_context(&run.job)
    );
    assert!(
        !run.job
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Remote size changed"),
        "{}",
        fail_context(&run.job)
    );
}

async fn completes_plaintext(encoding: Option<&'static str>) {
    let run = run_case(
        Script::Uniform {
            encoding,
            body: PLAIN,
        },
        default_engine(),
        Duration::from_secs(20),
    )
    .await;
    let context = fail_context(&run.job);
    assert_eq!(run.job.state, JobState::Completed, "{context}");
    assert!(run.job.error.is_none(), "{context}");
    assert_eq!(run.job.retry_attempts, 0, "{context}");
    let bytes = std::fs::read(&run.job.target_path).expect("finished file");
    assert_eq!(bytes, PLAIN);
    assert_eq!(sha256_hex(&bytes), PLAIN_SHA);
    assert!(!run.job.temp_path.exists(), "{context}");
}

#[tokio::test]
async fn issue188_content_encoding_identity_completes_with_plaintext_sha256() {
    completes_plaintext(Some("identity")).await;
}

#[tokio::test]
async fn issue188_absent_content_encoding_completes_with_plaintext_sha256() {
    completes_plaintext(None).await;
}

#[tokio::test]
async fn issue188_range_206_gzip_resume_fails_closed() {
    let run = run_case(
        Script::ResumeGzip,
        default_engine(),
        Duration::from_secs(20),
    )
    .await;
    assert_encoding_failure(&run.job);
    let context = fail_context(&run.job);
    let partial = std::fs::read(&run.job.temp_path).unwrap_or_default();
    assert_eq!(
        partial,
        PLAIN[..RESUME_PREFIX],
        "identity prefix stays; gzip tail must not be appended: {context}"
    );
    assert!(
        run.requests
            .iter()
            .any(|line| line.contains("range=bytes=")),
        "resume must send Range: {requests:?}",
        requests = run.requests
    );
}

#[tokio::test]
async fn issue188_multi_segment_gzip_fails_closed() {
    // sanitize() clamps multi_min_bytes to 1 MiB. Two segments need 2 MiB
    // because each segment is at least 1 MiB.
    let total = 2 * 1024 * 1024;
    let mut config = default_engine();
    config.multi_min_bytes = 1024 * 1024;
    let run = run_case(Script::MultiGzip { total }, config, Duration::from_secs(40)).await;
    assert_encoding_failure(&run.job);
    let context = fail_context(&run.job);
    assert!(
        !run.job.temp_path.exists(),
        "unwritten multi partial is removed before the single-stream failure: {context}"
    );
    assert_eq!(
        run.job.fallback_reason.as_deref(),
        Some("multi_resume_fallback"),
        "segment path must run before the single-stream failure: {context} requests={:?}",
        run.requests
    );
    assert!(
        run.requests.iter().any(|line| {
            line.contains("range=bytes=")
                && !line.contains("range=bytes=0-0")
                && !line.contains("range=bytes=1-1")
        }),
        "a segment Range must be issued: {:?}",
        run.requests
    );
}

#[tokio::test]
async fn issue188_transfer_encoding_gzip_fails_closed() {
    let run = run_case(
        Script::TransferEncoded {
            values: &["gzip"],
            body: GZIP_BODY,
            chunk_wire: false,
        },
        default_engine(),
        Duration::from_secs(20),
    )
    .await;
    assert_named_coding_failure(
        &run.job,
        "Response is Transfer-Encoding: gzip; asked for identity. Use a URL that sends the decoded file.",
    );
    assert!(
        !run.job.temp_path.exists(),
        "fresh failure must not leave a .part: {}",
        fail_context(&run.job)
    );
    assert_ne!(
        file_sha(&run.job.target_path),
        GZIP_SHA,
        "finished file must not be the gzip bytes: {}",
        fail_context(&run.job)
    );
}

async fn assert_transfer_framing_rejected(
    values: &'static [&'static str],
    body: &'static [u8],
    chunk_wire: bool,
) {
    let run = run_case(
        Script::TransferEncoded {
            values,
            body,
            chunk_wire,
        },
        default_engine(),
        Duration::from_secs(20),
    )
    .await;
    let context = fail_context(&run.job);
    assert_eq!(run.job.state, JobState::Failed, "{context}");
    assert_eq!(run.job.retry_attempts, 0, "{context}");
    assert!(
        !run.job.target_path.exists(),
        "finished path must stay absent: {context}"
    );
    assert!(
        !run.job.temp_path.exists(),
        "no committed temp file: {context}"
    );
}

#[tokio::test]
async fn issue188_transfer_encoding_chunked_identity_fails_closed() {
    assert_transfer_framing_rejected(&["chunked, identity"], b"5\r\nhello\r\n0\r\n\r\n", false)
        .await;
}

#[tokio::test]
async fn issue188_transfer_encoding_chunked_chunked_fails_closed() {
    assert_transfer_framing_rejected(&["chunked, chunked"], b"hello", true).await;
}

#[tokio::test]
async fn issue188_transfer_encoding_duplicate_chunked_headers_fail_closed() {
    assert_transfer_framing_rejected(&["chunked", "chunked"], b"hello", true).await;
}

#[tokio::test]
async fn issue188_transfer_encoding_gzip_chunked_fails_closed() {
    assert_transfer_framing_rejected(&["gzip, chunked"], GZIP_BODY, true).await;
}

#[tokio::test]
async fn issue188_content_encoding_deflate_br_zstd_xgzip_fail_closed() {
    for coding in ["deflate", "br", "zstd", "x-gzip"] {
        let run = encoded_head_and_get(coding).await;
        let message = format!(
            "Response is Content-Encoding: {coding}; asked for identity. Use a URL that sends the decoded file."
        );
        assert_named_coding_failure(&run.job, &message);
        assert!(
            !run.job.temp_path.exists(),
            "{coding}: {}",
            fail_context(&run.job)
        );
    }
}
