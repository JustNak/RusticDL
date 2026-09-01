use std::path::Path;
use std::sync::atomic::AtomicU8;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::{Stream, StreamExt};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio::time::{sleep, timeout};

use super::bandwidth::GlobalBandwidthLimiter;
use super::fetch::{control_outcome, format_reqwest_error, CONTROL_POLL};
use super::job::{download_error, DownloadError, DownloadOutcome, FailureCategory};
use super::segment_io::SegmentFileWriter;

pub(crate) use super::fetch::stall_error;
pub use super::fetch::STALL_TIMEOUT;

const WRITE_BUF: usize = 256 * 1024;

#[async_trait]
pub trait BodySink: Send {
    async fn write_chunk(&mut self, data: &[u8]) -> Result<usize, DownloadError>;
    async fn flush(&mut self) -> Result<(), DownloadError>;
    fn offset(&self) -> u64;
    fn target_offset(&self) -> Option<u64> {
        None
    }
}

pub struct AppendSink {
    writer: BufWriter<tokio::fs::File>,
    offset: u64,
    target: Option<u64>,
}

impl AppendSink {
    pub async fn open(path: &Path, offset: u64) -> Result<Self, DownloadError> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(offset == 0)
            .open(path)
            .await
            .map_err(|error| {
                download_error(
                    FailureCategory::Disk,
                    format!("Could not open partial download file: {error}"),
                    false,
                )
            })?;

        let mut writer = BufWriter::with_capacity(WRITE_BUF, file);
        if offset > 0 {
            writer
                .seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|error| {
                    download_error(
                        FailureCategory::Disk,
                        format!("Could not seek partial download file: {error}"),
                        false,
                    )
                })?;
        }

        Ok(Self {
            writer,
            offset,
            target: None,
        })
    }

    pub fn with_target(mut self, total: u64) -> Self {
        self.target = if total > 0 { Some(total) } else { None };
        self
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub async fn sync_data(&self) -> Result<(), DownloadError> {
        self.writer
            .get_ref()
            .sync_data()
            .await
            .map_err(disk_write_error)
    }
}

#[async_trait]
impl BodySink for AppendSink {
    async fn write_chunk(&mut self, data: &[u8]) -> Result<usize, DownloadError> {
        if data.is_empty() {
            return Ok(0);
        }
        self.writer
            .write_all(data)
            .await
            .map_err(disk_write_error)?;
        self.offset = self.offset.saturating_add(data.len() as u64);
        Ok(data.len())
    }

    async fn flush(&mut self) -> Result<(), DownloadError> {
        self.writer.flush().await.map_err(disk_write_error)
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn target_offset(&self) -> Option<u64> {
        self.target
    }
}

pub struct PositionedSink {
    writer: Arc<SegmentFileWriter>,
    offset: u64,
    end_inclusive: u64,
    #[cfg(test)]
    last_write_off_worker: bool,
}

impl PositionedSink {
    pub fn new(writer: Arc<SegmentFileWriter>, offset: u64, end_inclusive: u64) -> Self {
        Self {
            writer,
            offset,
            end_inclusive,
            #[cfg(test)]
            last_write_off_worker: false,
        }
    }

    #[cfg(test)]
    pub fn last_write_used_blocking_pool(&self) -> bool {
        self.last_write_off_worker
    }
}

#[async_trait]
impl BodySink for PositionedSink {
    async fn write_chunk(&mut self, data: &[u8]) -> Result<usize, DownloadError> {
        if data.is_empty() {
            return Ok(0);
        }
        let writer = self.writer.clone();
        let offset = self.offset;
        let end_inclusive = self.end_inclusive;
        let owned = data.to_vec();
        let worker_thread = std::thread::current().id();
        let (n, write_thread) = tokio::task::spawn_blocking(move || {
            writer
                .write_at(offset, &owned, end_inclusive)
                .map(|n| (n, std::thread::current().id()))
        })
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
        #[cfg(test)]
        {
            self.last_write_off_worker = write_thread != worker_thread;
        }
        let _ = (write_thread, worker_thread);
        self.offset = self.offset.saturating_add(n as u64);
        Ok(n)
    }

    async fn flush(&mut self) -> Result<(), DownloadError> {
        Ok(())
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn target_offset(&self) -> Option<u64> {
        Some(self.end_inclusive.saturating_add(1))
    }
}

#[derive(Debug)]
pub enum StreamEnd {
    Exhausted { downloaded: u64 },
    Control(DownloadOutcome),
}

pub async fn stream_body(
    response: reqwest::Response,
    sink: &mut impl BodySink,
    control: &AtomicU8,
    limiter: &GlobalBandwidthLimiter,
    on_chunk: impl FnMut(u64),
) -> Result<StreamEnd, DownloadError> {
    stream_body_with_stall(response, sink, control, limiter, STALL_TIMEOUT, on_chunk).await
}

pub(crate) async fn stream_body_with_stall(
    response: reqwest::Response,
    sink: &mut impl BodySink,
    control: &AtomicU8,
    limiter: &GlobalBandwidthLimiter,
    stall_timeout: Duration,
    on_chunk: impl FnMut(u64),
) -> Result<StreamEnd, DownloadError> {
    let stream = response.bytes_stream().map(|item| match item {
        Ok(chunk) => Ok(chunk),
        Err(error) => {
            let retryable = error.is_timeout()
                || error.is_connect()
                || error.is_request()
                || error.is_body()
                || error.is_decode();
            Err(download_error(
                FailureCategory::Network,
                format!("Download stream failed: {}", format_reqwest_error(&error)),
                retryable,
            ))
        }
    });
    futures_util::pin_mut!(stream);
    stream_body_loop(stream, sink, control, limiter, stall_timeout, on_chunk).await
}

pub(crate) async fn stream_body_loop<S, B>(
    mut stream: S,
    sink: &mut impl BodySink,
    control: &AtomicU8,
    limiter: &GlobalBandwidthLimiter,
    stall_timeout: Duration,
    mut on_chunk: impl FnMut(u64),
) -> Result<StreamEnd, DownloadError>
where
    S: Stream<Item = Result<B, DownloadError>> + Unpin,
    B: AsRef<[u8]>,
{
    let mut last_byte = Instant::now();

    loop {
        if let Some(outcome) = control_outcome(control) {
            sink.flush().await?;
            return Ok(StreamEnd::Control(outcome));
        }

        let idle = last_byte.elapsed();
        if idle >= stall_timeout {
            sink.flush().await?;
            return Err(stall_error(stall_timeout));
        }
        let wait = stall_timeout.saturating_sub(idle).min(CONTROL_POLL);

        match timeout(wait, stream.next()).await {
            Err(_elapsed) => {
                on_chunk(0);
            }
            Ok(None) => break,
            Ok(Some(Err(error))) => {
                sink.flush().await?;
                return Err(error);
            }
            Ok(Some(Ok(chunk))) => {
                let data = chunk.as_ref();
                if data.is_empty() {
                    on_chunk(0);
                    // Empty frames are idle. Sleep the remaining poll slice so a
                    // ready empty stream cannot starve the stall clock or the runtime.
                    sleep(wait).await;
                    continue;
                }

                let acquired = limiter.acquire(data.len(), Some(control)).await;
                let n = sink.write_chunk(data).await?;
                last_byte = Instant::now();
                if n > 0 {
                    on_chunk(n as u64);
                }

                if !acquired {
                    sink.flush().await?;
                    let outcome = control_outcome(control).unwrap_or(DownloadOutcome::Paused);
                    return Ok(StreamEnd::Control(outcome));
                }

                if n == 0 || n < data.len() {
                    break;
                }
            }
        }
    }

    if let Some(outcome) = control_outcome(control) {
        sink.flush().await?;
        return Ok(StreamEnd::Control(outcome));
    }

    sink.flush().await?;

    let downloaded = sink.offset();
    if let Some(target) = sink.target_offset() {
        if downloaded < target {
            return Err(download_error(
                FailureCategory::Network,
                format!("Download incomplete ({downloaded} of {target} bytes)."),
                true,
            ));
        }
    }

    Ok(StreamEnd::Exhausted { downloaded })
}

fn disk_write_error(error: std::io::Error) -> DownloadError {
    download_error(
        FailureCategory::Disk,
        format!("Could not write download data: {error}"),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::client::download_client;
    use std::sync::atomic::AtomicU8;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    async fn serve_body(body: &[u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = body.to_vec();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
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
            let reply = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                payload.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
            let _ = socket.write_all(&payload).await;
            let _ = socket.shutdown().await;
        });
        format!("http://{addr}/file.bin")
    }

    async fn get_response(url: &str) -> reqwest::Response {
        download_client()
            .unwrap()
            .get(url)
            .send()
            .await
            .expect("GET")
    }

    #[tokio::test]
    async fn stream_body_append_sink_writes_full_payload() {
        let payload = b"append-sink-payload-0123456789";
        let url = serve_body(payload).await;
        let response = get_response(&url).await;

        let dir =
            std::env::temp_dir().join(format!("rusticdl-append-sink-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("out.bin.part");
        let mut sink = AppendSink::open(&path, 0)
            .await
            .unwrap()
            .with_target(payload.len() as u64);
        let control = AtomicU8::new(0);
        let limiter = GlobalBandwidthLimiter::new(None);
        let mut credited = 0u64;
        let end = stream_body(response, &mut sink, &control, &limiter, |n| {
            credited += n;
        })
        .await
        .expect("stream");
        match end {
            StreamEnd::Exhausted { downloaded } => {
                assert_eq!(downloaded, payload.len() as u64);
            }
            StreamEnd::Control(outcome) => panic!("unexpected control {outcome:?}"),
        }
        assert_eq!(credited, payload.len() as u64);
        drop(sink);
        assert_eq!(tokio::fs::read(&path).await.unwrap(), payload);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn stream_body_positioned_sink_goes_through_spawn_blocking() {
        let payload = b"positioned-sink-via-spawn-blocking";
        let url = serve_body(payload).await;
        let response = get_response(&url).await;

        let dir =
            std::env::temp_dir().join(format!("rusticdl-positioned-sink-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("out.bin.part");
        tokio::fs::write(&path, vec![0u8; payload.len()])
            .await
            .unwrap();
        let writer = Arc::new(SegmentFileWriter::open(&path).unwrap());
        let end_inclusive = (payload.len() as u64).saturating_sub(1);
        let mut sink = PositionedSink::new(writer, 0, end_inclusive);
        let control = AtomicU8::new(0);
        let limiter = GlobalBandwidthLimiter::new(None);
        let mut credited = 0u64;
        let end = stream_body(response, &mut sink, &control, &limiter, |n| {
            credited += n;
        })
        .await
        .expect("stream");
        match end {
            StreamEnd::Exhausted { downloaded } => {
                assert_eq!(downloaded, payload.len() as u64);
            }
            StreamEnd::Control(outcome) => panic!("unexpected control {outcome:?}"),
        }
        assert_eq!(credited, payload.len() as u64);
        assert!(
            sink.last_write_used_blocking_pool(),
            "PositionedSink::write_chunk must spawn_blocking (no inline File lock)"
        );
        drop(sink);
        assert_eq!(tokio::fs::read(&path).await.unwrap(), payload);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn stream_body_incomplete_at_end_is_retryable_network() {
        let payload = b"short";
        let url = serve_body(payload).await;
        let response = get_response(&url).await;

        let dir =
            std::env::temp_dir().join(format!("rusticdl-incomplete-sink-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("out.bin.part");
        let mut sink = AppendSink::open(&path, 0).await.unwrap().with_target(100);
        let control = AtomicU8::new(0);
        let limiter = GlobalBandwidthLimiter::new(None);
        let err = stream_body(response, &mut sink, &control, &limiter, |_| {})
            .await
            .expect_err("short body vs target");
        assert_eq!(err.category, FailureCategory::Network);
        assert!(err.retryable);
        assert!(err.message.contains("Download incomplete"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn stream_body_writes_delivered_chunk_when_acquire_aborts() {
        use crate::download::fetch::CONTROL_PAUSED;
        use std::sync::atomic::Ordering;
        use tokio::sync::oneshot;

        let payload = b"must-write-even-when-throttle-aborts";
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sent = payload.to_vec();
        let (body_sent_tx, body_sent_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
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
            let reply = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                sent.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
            let _ = socket.write_all(&sent).await;
            let _ = socket.shutdown().await;
            let _ = body_sent_tx.send(());
        });
        let url = format!("http://{addr}/file.bin");
        let response = get_response(&url).await;

        let limiter = GlobalBandwidthLimiter::new(Some(1));
        assert!(
            limiter
                .acquire(GlobalBandwidthLimiter::MAX_ACQUIRE_QUANTUM, None)
                .await
        );

        let dir =
            std::env::temp_dir().join(format!("rusticdl-limiter-abort-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("out.bin.part");
        let mut sink = AppendSink::open(&path, 0).await.unwrap();
        let control = std::sync::Arc::new(AtomicU8::new(0));
        let control_flip = control.clone();
        let mut credited = 0u64;

        let stream = stream_body(
            response,
            &mut sink,
            control.as_ref(),
            limiter.as_ref(),
            |n| {
                credited += n;
            },
        );
        let flipper = async {
            body_sent_rx.await.expect("body sent");
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            control_flip.store(CONTROL_PAUSED, Ordering::Relaxed);
        };
        let (end, _) = tokio::join!(stream, flipper);
        let end = end.expect("stream");
        match end {
            StreamEnd::Control(outcome) => {
                assert_eq!(outcome, DownloadOutcome::Paused);
            }
            StreamEnd::Exhausted { downloaded } => {
                panic!("expected Control after acquire abort, got Exhausted {downloaded}")
            }
        }
        assert_eq!(
            credited,
            payload.len() as u64,
            "delivered chunk must be credited after acquire abort"
        );
        assert_eq!(sink.offset(), payload.len() as u64);
        drop(sink);
        assert_eq!(
            tokio::fs::read(&path).await.unwrap(),
            payload,
            "delivered chunk must be written after acquire abort"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    async fn serve_partial_then_hang(
        body: &[u8],
        advertised_len: usize,
    ) -> (String, oneshot::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = body.to_vec();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
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
            let reply = format!(
                "HTTP/1.1 200 OK\r\nConnection: keep-alive\r\nContent-Length: {advertised_len}\r\n\r\n"
            );
            let _ = socket.write_all(reply.as_bytes()).await;
            let _ = socket.write_all(&payload).await;
            let _ = socket.flush().await;
            let _ = release_rx.await;
            drop(socket);
        });
        (format!("http://{addr}/file.bin"), release_tx)
    }

    #[tokio::test]
    async fn stream_body_silent_server_is_retryable_stall() {
        let payload = b"partial";
        let (url, hold) = serve_partial_then_hang(payload, 1000).await;
        let response = get_response(&url).await;

        let dir =
            std::env::temp_dir().join(format!("rusticdl-stall-sink-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("out.bin.part");
        let mut sink = AppendSink::open(&path, 0).await.unwrap().with_target(1000);
        let control = AtomicU8::new(0);
        let limiter = GlobalBandwidthLimiter::new(None);
        let err = tokio::time::timeout(
            Duration::from_secs(2),
            stream_body_with_stall(
                response,
                &mut sink,
                &control,
                limiter.as_ref(),
                Duration::from_millis(80),
                |_| {},
            ),
        )
        .await
        .expect("stall should fire well before 2s")
        .expect_err("silent body must stall, not hang");
        assert_eq!(err.category, FailureCategory::Network);
        assert!(err.retryable);
        assert!(err.message.contains("Download stalled"));
        assert_eq!(sink.offset(), payload.len() as u64);
        drop(hold);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn stream_body_emits_idle_zero_ticks_before_stall() {
        let payload = b"head";
        let (url, hold) = serve_partial_then_hang(payload, 1000).await;
        let response = get_response(&url).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-idle-tick-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("out.bin.part");
        let mut sink = AppendSink::open(&path, 0).await.unwrap();
        let control = AtomicU8::new(0);
        let limiter = GlobalBandwidthLimiter::new(None);
        let ticks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let ticks_cb = ticks.clone();
        let err = tokio::time::timeout(
            Duration::from_secs(3),
            stream_body_with_stall(
                response,
                &mut sink,
                &control,
                limiter.as_ref(),
                Duration::from_millis(700),
                move |n| ticks_cb.lock().unwrap().push(n),
            ),
        )
        .await
        .expect("stall should fire")
        .expect_err("silent body must stall");
        assert!(err.message.contains("Download stalled"));
        let ticks = ticks.lock().unwrap().clone();
        assert!(
            ticks.iter().any(|&n| n > 0),
            "should credit the delivered prefix, got {ticks:?}"
        );
        assert!(
            ticks.iter().any(|&n| n == 0),
            "idle polls must emit zero-byte ticks so UI speed can drop, got {ticks:?}"
        );
        drop(hold);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn empty_chunk_flood_still_stalls() {
        let stream = futures_util::stream::iter(
            std::iter::once(Ok::<Vec<u8>, DownloadError>(b"head".to_vec()))
                .chain(std::iter::repeat(Ok(Vec::new()))),
        );
        let dir =
            std::env::temp_dir().join(format!("rusticdl-empty-flood-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("out.bin.part");
        let mut sink = AppendSink::open(&path, 0).await.unwrap().with_target(1000);
        let control = AtomicU8::new(0);
        let limiter = GlobalBandwidthLimiter::new(None);
        let err = tokio::time::timeout(
            Duration::from_secs(2),
            stream_body_loop(
                stream,
                &mut sink,
                &control,
                limiter.as_ref(),
                Duration::from_millis(80),
                |_| {},
            ),
        )
        .await
        .expect("empty-chunk flood must not hang")
        .expect_err("empty chunks must not reset the stall clock");
        assert!(err.message.contains("Download stalled"));
        assert_eq!(sink.offset(), b"head".len() as u64);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn limiter_wait_does_not_count_as_stall() {
        let limiter = GlobalBandwidthLimiter::new(Some(200));
        assert!(
            limiter
                .acquire(GlobalBandwidthLimiter::MAX_ACQUIRE_QUANTUM, None)
                .await
        );
        let stream = futures_util::stream::iter([
            Ok::<Vec<u8>, DownloadError>(vec![1u8; 64]),
            Ok(vec![2u8; 64]),
        ]);
        let dir =
            std::env::temp_dir().join(format!("rusticdl-throttle-stall-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("out.bin.part");
        let mut sink = AppendSink::open(&path, 0).await.unwrap().with_target(128);
        let control = AtomicU8::new(0);
        let end = tokio::time::timeout(
            Duration::from_secs(3),
            stream_body_loop(
                stream,
                &mut sink,
                &control,
                limiter.as_ref(),
                Duration::from_millis(80),
                |_| {},
            ),
        )
        .await
        .expect("throttled write must finish")
        .expect("must not stall while waiting on the limiter");
        match end {
            StreamEnd::Exhausted { downloaded } => assert_eq!(downloaded, 128),
            StreamEnd::Control(outcome) => panic!("unexpected control {outcome:?}"),
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
