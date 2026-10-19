//! Engine-path coverage for a server that answers a resume Range with 200.
//!
//! These tests drive `spawn_engine` against a `std::net` HTTP/1.1 server.
//! `auto_retry` stays at the settings default (6).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;

use tokio::sync::mpsc::UnboundedReceiver;

use super::MemoryJobStore;
use super::{spawn_engine, EngineCommand, EngineEvent, EngineHandle, EngineRuntimeConfig};
use crate::download::job::{Job, JobState};
use crate::download::verify::{sha256_file_hex, sha256_hex};

const IGNORED_RANGE: &str =
    "Server ignored Range; partial kept. Use Restart to download from zero.";

struct Seen(Arc<Mutex<Vec<String>>>);

impl Seen {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn resume_gets(&self) -> usize {
        self.0
            .lock()
            .expect("request log")
            .iter()
            .filter(|request| is_resume_get(request))
            .count()
    }

    fn dump(&self) -> String {
        self.0
            .lock()
            .expect("request log")
            .iter()
            .map(|request| {
                let mut line = request.lines().next().unwrap_or("").to_string();
                line.push_str(&range_summary(request));
                line
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

fn range_summary(request: &str) -> String {
    match range_start(request) {
        Some(start) => format!(" range={start}"),
        None => String::new(),
    }
}

fn is_resume_get(request: &str) -> bool {
    request.starts_with("GET ") && range_start(request).is_some_and(|start| start > 0)
}

fn range_start(request: &str) -> Option<u64> {
    let line = request
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("range:"))?;
    let spec = line.split_once(':')?.1.trim();
    let bytes = spec
        .get("bytes=".len()..)
        .filter(|_| spec.to_ascii_lowercase().starts_with("bytes="))?;
    bytes.split(['-', ',']).next()?.parse().ok()
}

fn read_headers(stream: &mut TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|window| window == b"\r\n\r\n") || buf.len() > 65_536 {
                    break;
                }
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn write_response(stream: &mut TcpStream, bytes: &[u8]) {
    let _ = stream.write_all(bytes);
    let _ = stream.flush();
}

/// Abort the TCP connection so the client sees a reset, not a clean EOF.
/// A clean close of a lengthless body is a different error and would confirm
/// or adopt that body instead of keeping the partial.
fn reset_connection(stream: TcpStream) {
    linger_abort(&stream);
    drop(stream);
}

#[cfg(unix)]
fn linger_abort(stream: &TcpStream) {
    let linger = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            &linger as *const libc::linger as *const libc::c_void,
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        );
    }
}

#[cfg(windows)]
fn linger_abort(stream: &TcpStream) {
    #[repr(C)]
    struct Linger {
        onoff: u16,
        linger: u16,
    }

    #[link(name = "ws2_32")]
    extern "system" {
        fn setsockopt(s: usize, level: i32, optname: i32, optval: *const u8, optlen: i32) -> i32;
    }

    let linger = Linger {
        onoff: 1,
        linger: 0,
    };
    unsafe {
        setsockopt(
            stream.as_raw_socket() as usize,
            0xffff,
            0x0080,
            &linger as *const Linger as *const u8,
            std::mem::size_of::<Linger>() as i32,
        );
    }
}

fn bind_server() -> (String, TcpListener) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    (format!("http://{addr}"), listener)
}

fn spawn_accept<F>(listener: TcpListener, handler: F)
where
    F: Fn(TcpStream) + Send + Sync + 'static,
{
    let handler = Arc::new(handler);
    std::thread::spawn(move || loop {
        let Ok((stream, _)) = listener.accept() else {
            break;
        };
        let _ = stream.set_nodelay(true);
        let handler = handler.clone();
        std::thread::spawn(move || handler(stream));
    });
}

fn engine_for(
    url: &str,
    filename: &str,
) -> (
    PathBuf,
    PathBuf,
    String,
    EngineHandle,
    UnboundedReceiver<EngineEvent>,
) {
    let dir = std::env::temp_dir().join(format!("rusticdl-range-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let target = dir.join(filename);
    let temp = dir.join(format!("{filename}.part"));
    let mut job = Job::new(
        url.to_string(),
        filename.into(),
        target.clone(),
        temp.clone(),
    );
    job.state = JobState::Queued;
    let id = job.id.clone();
    let mut config = EngineRuntimeConfig::default();
    config.max_concurrent = 1;
    let (handle, events) = spawn_engine(vec![job], config, Arc::new(MemoryJobStore::default()));
    (target, temp, id, handle, events)
}

async fn wait_job(
    events: &mut UnboundedReceiver<EngineEvent>,
    id: &str,
    timeout: Duration,
    pred: impl Fn(&Job) -> bool,
) -> Job {
    let id = id.to_string();
    tokio::time::timeout(timeout, async move {
        loop {
            match events.recv().await {
                Some(EngineEvent::JobsChanged(jobs)) => {
                    if let Some(job) = jobs.iter().find(|job| job.id == id && pred(job)) {
                        return job.clone();
                    }
                }
                Some(EngineEvent::Toast(_)) => {}
                None => panic!("event channel closed"),
            }
        }
    })
    .await
    .expect("timed out waiting for the job")
}

fn part_len(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn head(extra: &str, len: usize) -> String {
    format!("HTTP/1.1 200 OK\r\nConnection: close\r\n{extra}Content-Length: {len}\r\n\r\n")
}

/// Pause during the body, then Resume. HEAD has a length and no Accept-Ranges,
/// so the resume Range is speculative. The server answers 200 with the entity.
#[tokio::test]
async fn no_range_pause_resume() {
    let body: Vec<u8> = (0..80_000u32).map(|n| (n % 251) as u8).collect();
    let (base, listener) = bind_server();
    let seen = Seen::new();
    let seen_srv = seen.0.clone();
    let payload = body.clone();
    let restart = Arc::new(AtomicBool::new(false));
    let restart_srv = restart.clone();
    spawn_accept(listener, move |mut stream| {
        let request = read_headers(&mut stream);
        if request.is_empty() {
            return;
        }
        seen_srv.lock().expect("request log").push(request.clone());
        if request.starts_with("HEAD ") {
            write_response(&mut stream, head("", payload.len()).as_bytes());
            return;
        }
        if !request.starts_with("GET ") {
            return;
        }
        if restart_srv.load(Ordering::SeqCst) || range_start(&request).is_some() {
            let mut response = head("", payload.len()).into_bytes();
            response.extend_from_slice(&payload);
            write_response(&mut stream, &response);
            return;
        }
        let prefix = &payload[..2048.min(payload.len())];
        let mut response = head("", payload.len()).into_bytes();
        response.extend_from_slice(&prefix[..1024.min(prefix.len())]);
        write_response(&mut stream, &response);
        std::thread::sleep(Duration::from_millis(500));
        let _ = stream.write_all(&prefix[1024.min(prefix.len())..]);
        let _ = stream.flush();
        let _ = stream.set_read_timeout(None);
        let mut discard = [0u8; 64];
        let _ = stream.read(&mut discard);
    });

    let (target, temp, id, engine, mut events) =
        engine_for(&format!("{base}/file.bin"), "file.bin");
    let started = wait_job(&mut events, &id, Duration::from_secs(5), |job| {
        job.downloaded_bytes > 0 || matches!(job.state, JobState::Downloading | JobState::Paused)
    })
    .await;
    if started.downloaded_bytes == 0 && started.state != JobState::Paused {
        let _ = wait_job(&mut events, &id, Duration::from_secs(4), |job| {
            job.downloaded_bytes > 0 || job.state == JobState::Paused
        })
        .await;
    }
    engine.send(EngineCommand::Pause(id.clone()));
    let paused = wait_job(&mut events, &id, Duration::from_secs(5), |job| {
        job.state == JobState::Paused
    })
    .await;
    let paused_len = part_len(&temp);
    assert!(
        paused_len > 0,
        "pause must keep a partial, downloaded_bytes={}",
        paused.downloaded_bytes
    );

    let started_at = Instant::now();
    engine.send(EngineCommand::Resume(id.clone()));
    let failed = wait_job(&mut events, &id, Duration::from_secs(45), |job| {
        job.state == JobState::Failed
    })
    .await;
    let elapsed = started_at.elapsed();
    let error = failed.error.clone().unwrap_or_default();
    let kept = part_len(&temp);
    let range_gets = seen.resume_gets();
    assert!(
        failed.state == JobState::Failed
            && failed.retry_attempts == 0
            && elapsed < Duration::from_secs(5)
            && error == IGNORED_RANGE
            && kept == paused_len
            && !target.exists()
            && range_gets == 1,
        "state={:?} retry_attempts={} elapsed_ms={} error={error:?} part={kept} paused={paused_len} target_exists={} range_gets={range_gets} requests={}",
        failed.state,
        failed.retry_attempts,
        elapsed.as_millis(),
        target.exists(),
        seen.dump()
    );

    restart.store(true, Ordering::SeqCst);
    engine.send(EngineCommand::Restart(id.clone()));
    let completed = wait_job(&mut events, &id, Duration::from_secs(10), |job| {
        matches!(job.state, JobState::Completed | JobState::Failed)
    })
    .await;
    assert_eq!(
        completed.state,
        JobState::Completed,
        "Restart must finish the file: {:?}",
        completed.error
    );
    let expected = sha256_hex(&body);
    let actual = sha256_file_hex(&target).expect("finished file");
    assert_eq!(actual, expected, "Restart bytes must match the entity");
    assert_eq!(std::fs::read(&target).expect("finished file"), body);
    engine.send(EngineCommand::Shutdown);
    let _ = std::fs::remove_dir_all(target.parent().expect("parent"));
}

/// The second builder is the lengthless fallthrough in `http.rs`: a Range
/// 200 with no Content-Length, after the known size was retired, when the
/// bytes on disk are not a lengthless candidate.
///
/// Each attempt truncates a sized body, then answers the resume with a new
/// ETag and one chunked chunk followed by a bare FIN. That keeps the chunk
/// and retires the old size without adopting a candidate. The next Range 200
/// repeats that ETag and has no length, which is the fallthrough.
#[tokio::test]
async fn ignored_range_lengthless_second_site_does_not_retry() {
    const ENTITY_LEN: usize = 20_000;
    const PREFIX_LEN: usize = 4096;
    let (base, listener) = bind_server();
    let seen = Seen::new();
    let seen_srv = seen.0.clone();
    let phase = Arc::new(AtomicU32::new(0));
    let phase_srv = phase.clone();
    let generation = Arc::new(AtomicU32::new(1));
    let generation_srv = generation.clone();
    spawn_accept(listener, move |mut stream| {
        let request = read_headers(&mut stream);
        if request.is_empty() {
            return;
        }
        seen_srv.lock().expect("request log").push(request.clone());
        if request.starts_with("HEAD ") {
            phase_srv.store(0, Ordering::SeqCst);
            write_response(
                &mut stream,
                head("Accept-Ranges: bytes\r\nETag: \"v1\"\r\n", ENTITY_LEN).as_bytes(),
            );
            return;
        }
        if !request.starts_with("GET ") {
            return;
        }
        let start = range_start(&request).unwrap_or(0);
        let state = phase_srv.load(Ordering::SeqCst);
        if state == 0 && start == 0 {
            phase_srv.store(1, Ordering::SeqCst);
            let mut response =
                head("Accept-Ranges: bytes\r\nETag: \"v1\"\r\n", ENTITY_LEN).into_bytes();
            response.extend(std::iter::repeat(b'o').take(PREFIX_LEN));
            write_response(&mut stream, &response);
            return;
        }
        if state == 0 || state == 1 {
            phase_srv.store(2, Ordering::SeqCst);
            let gen = generation_srv.fetch_add(1, Ordering::SeqCst) + 1;
            let etag = format!("\"v{gen}\"");
            let headers = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: {etag}\r\nTransfer-Encoding: chunked\r\n\r\n{PREFIX_LEN:x}\r\n"
            );
            let _ = stream.write_all(headers.as_bytes());
            let _ = stream.write_all(&[b'N'; PREFIX_LEN]);
            let _ = stream.write_all(b"\r\n");
            let _ = stream.flush();
            return;
        }
        let gen = generation_srv.load(Ordering::SeqCst);
        let etag = format!("\"v{gen}\"");
        let headers = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: {etag}\r\n\r\n"
        );
        write_response(&mut stream, headers.as_bytes());
    });

    let (target, temp, id, engine, mut events) =
        engine_for(&format!("{base}/file.bin"), "file.bin");
    let started_at = Instant::now();
    let failed = wait_job(&mut events, &id, Duration::from_secs(45), |job| {
        job.state == JobState::Failed
    })
    .await;
    let elapsed = started_at.elapsed();
    let error = failed.error.clone().unwrap_or_default();
    let kept = part_len(&temp);
    let range_gets = seen.resume_gets();
    assert!(
        failed.state == JobState::Failed
            && failed.retry_attempts == 0
            && elapsed < Duration::from_secs(5)
            && error == IGNORED_RANGE
            && kept > 0
            && !target.exists()
            && range_gets == 2,
        "state={:?} retry_attempts={} elapsed_ms={} error={error:?} part={kept} target_exists={} range_gets={range_gets} requests={}",
        failed.state,
        failed.retry_attempts,
        elapsed.as_millis(),
        target.exists(),
        seen.dump()
    );
    engine.send(EngineCommand::Shutdown);
    let _ = std::fs::remove_dir_all(target.parent().expect("parent"));
}

/// A server that answers the resume with 206 still finishes the file.
#[tokio::test]
async fn range_206_pause_resume_completes() {
    let body: Vec<u8> = (0..32_000u32).map(|n| (n % 251) as u8).collect();
    let (base, listener) = bind_server();
    let seen = Seen::new();
    let seen_srv = seen.0.clone();
    let payload = body.clone();
    spawn_accept(listener, move |mut stream| {
        let request = read_headers(&mut stream);
        if request.is_empty() {
            return;
        }
        seen_srv.lock().expect("request log").push(request.clone());
        if request.starts_with("HEAD ") {
            write_response(
                &mut stream,
                head("Accept-Ranges: bytes\r\n", payload.len()).as_bytes(),
            );
            return;
        }
        if !request.starts_with("GET ") {
            return;
        }
        if let Some(start) = range_start(&request) {
            let start = start as usize;
            let slice = if start < payload.len() {
                &payload[start..]
            } else {
                &[]
            };
            let end = payload.len().saturating_sub(1);
            let headers = format!(
                "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\n\r\n",
                payload.len(),
                slice.len()
            );
            let mut response = headers.into_bytes();
            response.extend_from_slice(slice);
            write_response(&mut stream, &response);
            return;
        }
        let prefix_end = 2048.min(payload.len());
        let mut response = head("Accept-Ranges: bytes\r\n", payload.len()).into_bytes();
        response.extend_from_slice(&payload[..1024.min(prefix_end)]);
        write_response(&mut stream, &response);
        std::thread::sleep(Duration::from_millis(500));
        let _ = stream.write_all(&payload[1024.min(prefix_end)..prefix_end]);
        let _ = stream.flush();
        let _ = stream.set_read_timeout(None);
        let mut discard = [0u8; 64];
        let _ = stream.read(&mut discard);
    });

    let (target, _temp, id, engine, mut events) =
        engine_for(&format!("{base}/file.bin"), "file.bin");
    let _ = wait_job(&mut events, &id, Duration::from_secs(5), |job| {
        job.downloaded_bytes > 0 || job.state == JobState::Paused
    })
    .await;
    engine.send(EngineCommand::Pause(id.clone()));
    let paused = wait_job(&mut events, &id, Duration::from_secs(5), |job| {
        job.state == JobState::Paused
    })
    .await;
    assert!(
        paused.downloaded_bytes > 0 || part_len(&paused.temp_path) > 0,
        "206 floor needs a partial before resume"
    );
    engine.send(EngineCommand::Resume(id.clone()));
    let done = wait_job(&mut events, &id, Duration::from_secs(10), |job| {
        matches!(job.state, JobState::Completed | JobState::Failed)
    })
    .await;
    assert_eq!(
        done.state,
        JobState::Completed,
        "206 resume must finish: {:?} requests={}",
        done.error,
        seen.dump()
    );
    assert_eq!(sha256_file_hex(&target).expect("file"), sha256_hex(&body));
    assert_eq!(std::fs::read(&target).expect("file"), body);
    assert!(
        seen.resume_gets() >= 1,
        "resume must send Range, requests={}",
        seen.dump()
    );
    engine.send(EngineCommand::Shutdown);
    let _ = std::fs::remove_dir_all(target.parent().expect("parent"));
}

/// A reset mid-body on a Range-capable server is still retried inside the
/// transfer. This guards against making every error non-retryable.
#[tokio::test]
async fn range_reset_mid_body_is_still_retried() {
    let body: Vec<u8> = (0..16_000u32).map(|n| (n % 251) as u8).collect();
    let (base, listener) = bind_server();
    let seen = Seen::new();
    let seen_srv = seen.0.clone();
    let payload = body.clone();
    let resets = Arc::new(AtomicU32::new(0));
    let resets_srv = resets.clone();
    spawn_accept(listener, move |mut stream| {
        let request = read_headers(&mut stream);
        if request.is_empty() {
            return;
        }
        seen_srv.lock().expect("request log").push(request.clone());
        if request.starts_with("HEAD ") {
            write_response(
                &mut stream,
                head("Accept-Ranges: bytes\r\n", payload.len()).as_bytes(),
            );
            return;
        }
        if !request.starts_with("GET ") {
            return;
        }
        if range_start(&request).is_none() && resets_srv.fetch_add(1, Ordering::SeqCst) == 0 {
            let mut response = head("Accept-Ranges: bytes\r\n", payload.len()).into_bytes();
            response.extend_from_slice(&payload[..4096]);
            write_response(&mut stream, &response);
            std::thread::sleep(Duration::from_millis(100));
            reset_connection(stream);
            return;
        }
        let start = range_start(&request).unwrap_or(0) as usize;
        let slice = if start < payload.len() {
            &payload[start..]
        } else {
            &[]
        };
        let end = payload.len().saturating_sub(1);
        let headers = format!(
            "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\n\r\n",
            payload.len(),
            slice.len()
        );
        let mut response = headers.into_bytes();
        response.extend_from_slice(slice);
        write_response(&mut stream, &response);
    });

    let (target, _temp, id, engine, mut events) =
        engine_for(&format!("{base}/file.bin"), "file.bin");
    let done = wait_job(&mut events, &id, Duration::from_secs(15), |job| {
        matches!(job.state, JobState::Completed | JobState::Failed)
    })
    .await;
    assert_eq!(
        done.state,
        JobState::Completed,
        "a reset on a Range-capable server must be retried: {:?} reconnects={} requests={}",
        done.error,
        done.reconnect_count,
        seen.dump()
    );
    assert!(
        done.reconnect_count >= 1,
        "the reset must be retried inside the transfer, reconnects={} requests={}",
        done.reconnect_count,
        seen.dump()
    );
    assert!(
        seen.resume_gets() >= 1,
        "the retry must Range-resume, requests={}",
        seen.dump()
    );
    assert_eq!(sha256_file_hex(&target).expect("file"), sha256_hex(&body));
    assert_eq!(std::fs::read(&target).expect("file"), body);
    engine.send(EngineCommand::Shutdown);
    let _ = std::fs::remove_dir_all(target.parent().expect("parent"));
}
