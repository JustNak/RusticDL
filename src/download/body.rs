use std::path::Path;
use std::sync::atomic::AtomicU8;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::{Stream, StreamExt};
use reqwest::Version;
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio::time::{sleep, timeout};

use super::bandwidth::GlobalBandwidthLimiter;
use super::fetch::{
    control_outcome, format_reqwest_error, reqwest_failure_is_retryable, CONTROL_POLL,
};
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
        let path = path.to_path_buf();
        let truncate = offset == 0;
        let file = tokio::task::spawn_blocking(move || {
            super::filesystem::open_download_file(&path, false, true, true, truncate)
        })
        .await
        .map_err(|error| {
            download_error(
                FailureCategory::Disk,
                format!("Could not open partial download file: {error}"),
                false,
            )
        })?
        .map_err(|error| {
            download_error(
                FailureCategory::Disk,
                format!("Could not open partial download file: {error}"),
                false,
            )
        })?;
        let file = tokio::fs::File::from_std(file);

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
        let Some(target) = self.target else {
            self.writer
                .write_all(data)
                .await
                .map_err(disk_write_error)?;
            self.offset = self.offset.saturating_add(data.len() as u64);
            return Ok(data.len());
        };
        let room = target.saturating_sub(self.offset);
        if (data.len() as u64) <= room {
            self.writer
                .write_all(data)
                .await
                .map_err(disk_write_error)?;
            self.offset = self.offset.saturating_add(data.len() as u64);
            return Ok(data.len());
        }
        // A known size is a hard end. Keep the bytes that still fit, and do
        // not let the rest of a runaway body extend the file.
        let keep = room as usize;
        if keep > 0 {
            self.writer
                .write_all(&data[..keep])
                .await
                .map_err(disk_write_error)?;
            self.offset = self.offset.saturating_add(room);
        }
        self.writer.flush().await.map_err(disk_write_error)?;
        let observed = self.offset.saturating_add((data.len() - keep) as u64);
        Err(remote_size_changed_error(observed, target))
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

/// Counts a body without touching the `.part`. Used to compare a repeated
/// full response with the candidate already on disk.
pub(crate) struct CountingSink {
    offset: u64,
    /// Once the body passes this length, stop. The confirm path only needs
    /// to know the repeated body is longer than the candidate.
    limit: Option<u64>,
}

impl CountingSink {
    pub(crate) fn new() -> Self {
        Self {
            offset: 0,
            limit: None,
        }
    }

    pub(crate) fn stop_after(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }
}

#[async_trait]
impl BodySink for CountingSink {
    async fn write_chunk(&mut self, data: &[u8]) -> Result<usize, DownloadError> {
        if data.is_empty() {
            return Ok(0);
        }
        let Some(limit) = self.limit else {
            self.offset = self.offset.saturating_add(data.len() as u64);
            return Ok(data.len());
        };
        let room = limit.saturating_sub(self.offset);
        if (data.len() as u64) <= room {
            self.offset = self.offset.saturating_add(data.len() as u64);
            return Ok(data.len());
        }
        // One byte past the candidate is enough to reject the body. Do not
        // pull the rest of a runaway confirm response.
        let keep = (room as usize).saturating_add(1).min(data.len());
        self.offset = self.offset.saturating_add(keep as u64);
        Err(remote_size_changed_error(self.offset, limit))
    }

    async fn flush(&mut self) -> Result<(), DownloadError> {
        Ok(())
    }

    fn offset(&self) -> u64 {
        self.offset
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EosPolicy {
    /// Known length, chunked HTTP/1.1, or a framed HTTP/2 or HTTP/3 end-of-stream.
    Complete,
    /// HTTP/1 close-delimited body with no length. A clean FIN is incomplete.
    UnknownLength,
}

/// RFC 7230 `tchar`. A coding outside this set, or longer than 32 bytes, is
/// not copied into the error string.
fn is_rfc7230_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            matches!(
                byte,
                b'!'
                    | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
                    | b'0'..=b'9'
                    | b'A'..=b'Z'
                    | b'a'..=b'z'
            )
        })
}

const CODING_NAME_MAX_BYTES: usize = 32;

fn display_coding(coding: &str) -> String {
    if coding.len() <= CODING_NAME_MAX_BYTES && is_rfc7230_token(coding) {
        coding.to_ascii_lowercase()
    } else {
        "unrecognized".to_string()
    }
}

enum ListedCoding {
    Identity,
    Chunked,
    Other(String),
}

fn coding_label(coding: &ListedCoding) -> &str {
    match coding {
        ListedCoding::Identity => "identity",
        ListedCoding::Chunked => "chunked",
        ListedCoding::Other(name) => name,
    }
}

fn list_codings(
    headers: &reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
) -> Result<Option<Vec<ListedCoding>>, ()> {
    // Hyper treats a trailing empty piece as "not chunked" and reads the body
    // raw. Dropping that piece here would accept `chunked,` as one chunked.
    let keep_empty = name == reqwest::header::TRANSFER_ENCODING;
    let values = headers.get_all(name);
    if values.iter().next().is_none() {
        return Ok(None);
    }
    let mut listed = Vec::new();
    for value in values {
        let text = value.to_str().map_err(|_| ())?;
        for coding in text.split(',') {
            let coding = coding.trim();
            if coding.is_empty() {
                if keep_empty {
                    listed.push(ListedCoding::Other("unrecognized".to_string()));
                }
                continue;
            }
            listed.push(if coding.eq_ignore_ascii_case("identity") {
                ListedCoding::Identity
            } else if coding.eq_ignore_ascii_case("chunked")
                && coding.len() <= CODING_NAME_MAX_BYTES
                && is_rfc7230_token(coding)
            {
                ListedCoding::Chunked
            } else {
                ListedCoding::Other(display_coding(coding))
            });
        }
    }
    Ok(Some(listed))
}

const CODING_LIST_MAX_NAMES: usize = 4;
const CODING_LIST_MAX_BYTES: usize = 64;

fn names_for_error(codings: &[ListedCoding]) -> String {
    let foreign_only = codings
        .iter()
        .any(|coding| matches!(coding, ListedCoding::Other(_)));
    let mut shown = Vec::new();
    let mut total = 0usize;
    for coding in codings {
        let name = match coding {
            ListedCoding::Other(name) => name.as_str(),
            ListedCoding::Identity | ListedCoding::Chunked if foreign_only => continue,
            ListedCoding::Identity | ListedCoding::Chunked => coding_label(coding),
        };
        total += 1;
        if shown.len() < CODING_LIST_MAX_NAMES {
            shown.push(name);
        }
    }
    cap_coding_list(&shown, total)
}

fn cap_coding_list(names: &[&str], total: usize) -> String {
    if total == 0 {
        return "unrecognized".to_string();
    }
    let mut take = names.len().min(CODING_LIST_MAX_NAMES).min(total);
    loop {
        let omitted = total - take;
        let listed = names[..take].join(", ");
        let combined = if omitted == 0 {
            listed
        } else if listed.is_empty() {
            format!("+{omitted} more")
        } else {
            format!("{listed} +{omitted} more")
        };
        if combined.len() <= CODING_LIST_MAX_BYTES {
            return combined;
        }
        if take == 0 {
            return "unrecognized".to_string();
        }
        take -= 1;
    }
}

/// A `Content-Encoding` other than `identity` means the body is not the file.
/// The client already asked for `identity` and does not decode. The error is
/// not retried: another attempt would store the same encoded bytes.
pub(crate) fn reject_non_identity_content_encoding(
    headers: &reqwest::header::HeaderMap,
) -> Result<(), DownloadError> {
    let codings = match list_codings(headers, reqwest::header::CONTENT_ENCODING) {
        Ok(Some(codings)) => codings,
        Ok(None) => return Ok(()),
        Err(()) => return Err(encoded_body_error("unreadable")),
    };
    let offending: Vec<ListedCoding> = codings
        .into_iter()
        .filter(|coding| !matches!(coding, ListedCoding::Identity))
        .collect();
    if offending.is_empty() {
        return Ok(());
    }
    Err(encoded_body_error(&names_for_error(&offending)))
}

/// Allow no `Transfer-Encoding`, identity only, or exactly one `chunked` as
/// the final token (earlier tokens may only be `identity`). An empty token
/// anywhere in the combined header values is rejected. Not retried.
pub(crate) fn reject_unsupported_transfer_encoding(
    headers: &reqwest::header::HeaderMap,
) -> Result<(), DownloadError> {
    let codings = match list_codings(headers, reqwest::header::TRANSFER_ENCODING) {
        Ok(Some(codings)) => codings,
        Ok(None) => return Ok(()),
        Err(()) => return Err(transfer_encoded_body_error("unreadable")),
    };
    if transfer_codings_allowed(&codings) {
        return Ok(());
    }
    Err(transfer_encoded_body_error(&names_for_error(&codings)))
}

fn transfer_codings_allowed(codings: &[ListedCoding]) -> bool {
    if codings.is_empty()
        || codings
            .iter()
            .all(|coding| matches!(coding, ListedCoding::Identity))
    {
        return true;
    }
    let chunked = codings
        .iter()
        .filter(|coding| matches!(coding, ListedCoding::Chunked))
        .count();
    let ends_chunked = matches!(codings.last(), Some(ListedCoding::Chunked));
    let prefix_identity = codings
        .iter()
        .rev()
        .skip(1)
        .all(|coding| matches!(coding, ListedCoding::Identity));
    chunked == 1 && ends_chunked && prefix_identity
}

/// True only when this client treats `Transfer-Encoding` as a decoded body:
/// a single final `chunked`, with only `identity` before it.
pub(crate) fn transfer_encoding_is_accepted_chunked(headers: &reqwest::header::HeaderMap) -> bool {
    match list_codings(headers, reqwest::header::TRANSFER_ENCODING) {
        Ok(Some(codings)) => {
            transfer_codings_allowed(&codings)
                && codings
                    .iter()
                    .any(|coding| matches!(coding, ListedCoding::Chunked))
        }
        _ => false,
    }
}

/// Content-Encoding and Transfer-Encoding both name the bytes that would be stored.
pub(crate) fn reject_encoded_body(
    headers: &reqwest::header::HeaderMap,
) -> Result<(), DownloadError> {
    reject_non_identity_content_encoding(headers)?;
    reject_unsupported_transfer_encoding(headers)
}

fn encoded_body_error(coding: &str) -> DownloadError {
    download_error(
        FailureCategory::Resume,
        format!(
            "Response is Content-Encoding: {coding}; asked for identity. Use a URL that sends the decoded file."
        ),
        false,
    )
}

fn transfer_encoded_body_error(coding: &str) -> DownloadError {
    download_error(
        FailureCategory::Resume,
        format!(
            "Response is Transfer-Encoding: {coding}; asked for identity. Use a URL that sends the decoded file."
        ),
        false,
    )
}

pub async fn stream_body(
    response: reqwest::Response,
    sink: &mut impl BodySink,
    control: &AtomicU8,
    limiter: &GlobalBandwidthLimiter,
    on_chunk: impl FnMut(u64),
) -> Result<StreamEnd, DownloadError> {
    // Last gate for every writer (`http` single-stream, lengthless confirm,
    // and multi-segment). Headers are checked before the body is pulled.
    reject_encoded_body(response.headers())?;
    let policy = eos_policy_for_response(&response, sink.target_offset());
    stream_body_with_stall(
        response,
        sink,
        control,
        limiter,
        STALL_TIMEOUT,
        policy,
        on_chunk,
    )
    .await
}

fn eos_policy_for_response(response: &reqwest::Response, target: Option<u64>) -> EosPolicy {
    // A bare close is complete only for a single final `chunked`. `chunked`
    // earlier in the list is framing bytes, not a decoded body.
    let chunked = transfer_encoding_is_accepted_chunked(response.headers());
    eos_policy(
        response.version(),
        response.content_length(),
        chunked,
        target,
    )
}

/// HTTP/2 and HTTP/3 have no `Transfer-Encoding`. A body with no
/// `Content-Length` still ends on a framed end-of-stream, which is complete.
/// A bare HTTP/1 close with no length stays incomplete. A known length stays
/// `Complete` so a short body still fails the sink target check.
fn eos_policy(
    version: Version,
    content_length: Option<u64>,
    chunked: bool,
    target: Option<u64>,
) -> EosPolicy {
    if target.is_some() || content_length.is_some() || chunked {
        return EosPolicy::Complete;
    }
    match version {
        Version::HTTP_2 | Version::HTTP_3 => EosPolicy::Complete,
        _ => EosPolicy::UnknownLength,
    }
}

pub(crate) async fn stream_body_with_stall(
    response: reqwest::Response,
    sink: &mut impl BodySink,
    control: &AtomicU8,
    limiter: &GlobalBandwidthLimiter,
    stall_timeout: Duration,
    policy: EosPolicy,
    on_chunk: impl FnMut(u64),
) -> Result<StreamEnd, DownloadError> {
    let stream = response.bytes_stream().map(|item| match item {
        Ok(chunk) => Ok(chunk),
        Err(error) => Err(download_error(
            FailureCategory::Network,
            format!("Download stream failed: {}", format_reqwest_error(&error)),
            reqwest_failure_is_retryable(&error),
        )),
    });
    futures_util::pin_mut!(stream);
    stream_body_loop(
        stream,
        sink,
        control,
        limiter,
        stall_timeout,
        policy,
        on_chunk,
    )
    .await
}

pub(crate) async fn stream_body_loop<S, B>(
    mut stream: S,
    sink: &mut impl BodySink,
    control: &AtomicU8,
    limiter: &GlobalBandwidthLimiter,
    stall_timeout: Duration,
    policy: EosPolicy,
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
                // A reset after the last byte is a finished body, not a failed one.
                if target_reached(sink) {
                    return Ok(StreamEnd::Exhausted {
                        downloaded: sink.offset(),
                    });
                }
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
    if policy == EosPolicy::UnknownLength {
        return Err(lengthless_eof_error(downloaded));
    }

    Ok(StreamEnd::Exhausted { downloaded })
}

pub(crate) fn lengthless_eof_error(downloaded: u64) -> DownloadError {
    download_error(
        FailureCategory::Network,
        format!("Download ended without a length or chunked terminator ({downloaded} bytes)."),
        true,
    )
}
pub(crate) fn is_lengthless_eof(error: &DownloadError) -> bool {
    error.message.contains("without a length")
}

pub(crate) fn is_incomplete_download(error: &DownloadError) -> bool {
    error.message.starts_with("Download incomplete (")
}

pub(crate) fn remote_size_changed_error(actual: u64, expected: u64) -> DownloadError {
    download_error(
        FailureCategory::Resume,
        format!("Remote size changed ({actual} bytes vs expected {expected}). Use Restart."),
        false,
    )
}

pub(crate) fn is_remote_size_mismatch(error: &DownloadError) -> bool {
    error.category == FailureCategory::Resume && error.message.starts_with("Remote size changed (")
}

fn target_reached(sink: &impl BodySink) -> bool {
    match sink.target_offset() {
        Some(target) if target > 0 => sink.offset() >= target,
        _ => false,
    }
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

    fn encoding_headers(value: Option<&[u8]>) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(value) = value {
            headers.insert(
                reqwest::header::CONTENT_ENCODING,
                reqwest::header::HeaderValue::from_bytes(value).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn non_identity_content_encoding_names_the_coding_and_allows_identity() {
        let allow = [
            None,
            Some(&b"identity"[..]),
            Some(&b"Identity"[..]),
            Some(&b" identity "[..]),
            Some(&b"identity, identity"[..]),
            Some(&b"identity, Identity"[..]),
            Some(&b","[..]),
        ];
        for value in allow {
            assert!(
                reject_non_identity_content_encoding(&encoding_headers(value)).is_ok(),
                "{value:?}"
            );
        }

        let exact = "Response is Content-Encoding: gzip; asked for identity. Use a URL that sends the decoded file.";
        for value in ["gzip", "GZIP", "gzip, identity", "identity, gzip", " gzip "] {
            let err =
                reject_non_identity_content_encoding(&encoding_headers(Some(value.as_bytes())))
                    .expect_err(value);
            assert_eq!(err.message, exact, "{value}");
            assert_eq!(err.category, FailureCategory::Resume);
            assert!(!err.retryable, "{value}");
        }

        let deflate =
            reject_non_identity_content_encoding(&encoding_headers(Some(b"gzip, deflate")))
                .expect_err("list");
        assert_eq!(
            deflate.message,
            "Response is Content-Encoding: gzip, deflate; asked for identity. Use a URL that sends the decoded file."
        );
        assert!(!deflate.retryable);

        for coding in ["deflate", "br", "zstd", "x-gzip"] {
            let err =
                reject_non_identity_content_encoding(&encoding_headers(Some(coding.as_bytes())))
                    .expect_err(coding);
            assert!(err.message.contains(coding), "{err:?}");
            assert!(!err.retryable);
        }

        let opaque = reject_non_identity_content_encoding(&encoding_headers(Some(&[0xff])))
            .expect_err("opaque");
        assert!(opaque.message.contains("unreadable"));
        assert!(!opaque.retryable);
        assert_eq!(opaque.category, FailureCategory::Resume);

        let mut split = reqwest::header::HeaderMap::new();
        split.append(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("identity"),
        );
        split.append(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("gzip"),
        );
        let split_err = reject_non_identity_content_encoding(&split).expect_err("second header");
        assert_eq!(split_err.message, exact);
        assert!(!split_err.retryable);

        let mut identities = reqwest::header::HeaderMap::new();
        identities.append(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("identity"),
        );
        identities.append(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("Identity"),
        );
        assert!(reject_non_identity_content_encoding(&identities).is_ok());
    }

    fn transfer_headers(value: Option<&[u8]>) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(value) = value {
            headers.insert(
                reqwest::header::TRANSFER_ENCODING,
                reqwest::header::HeaderValue::from_bytes(value).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn transfer_encoding_rejects_codings_other_than_chunked_and_identity() {
        let allow = [
            None,
            Some(&b"chunked"[..]),
            Some(&b"Chunked"[..]),
            Some(&b"identity"[..]),
            Some(&b"Identity"[..]),
            Some(&b"identity, chunked"[..]),
        ];
        for value in allow {
            assert!(
                reject_unsupported_transfer_encoding(&transfer_headers(value)).is_ok(),
                "{value:?}"
            );
        }

        let exact = "Response is Transfer-Encoding: gzip; asked for identity. Use a URL that sends the decoded file.";
        for value in ["gzip", "GZIP", "gzip, chunked", "chunked, gzip", " gzip "] {
            let err =
                reject_unsupported_transfer_encoding(&transfer_headers(Some(value.as_bytes())))
                    .expect_err(value);
            assert_eq!(err.message, exact, "{value}");
            assert_eq!(err.category, FailureCategory::Resume);
            assert!(!err.retryable, "{value}");
        }

        for coding in ["deflate", "br", "zstd", "x-gzip"] {
            let err =
                reject_unsupported_transfer_encoding(&transfer_headers(Some(coding.as_bytes())))
                    .expect_err(coding);
            assert!(err.message.contains(coding), "{err:?}");
            assert!(err.message.contains("Transfer-Encoding"), "{err:?}");
            assert!(!err.retryable);
        }

        // chunked is framing only when it is the single final token.
        for (value, token) in [
            ("chunked, identity", "chunked, identity"),
            ("chunked, chunked", "chunked, chunked"),
        ] {
            let err =
                reject_unsupported_transfer_encoding(&transfer_headers(Some(value.as_bytes())))
                    .expect_err(value);
            assert_eq!(err.message, transfer_error(token), "{value}");
            assert!(!err.retryable, "{value}: {err:?}");
            assert_eq!(err.category, FailureCategory::Resume, "{value}");
        }

        let mut duplicate_chunked = reqwest::header::HeaderMap::new();
        duplicate_chunked.append(
            reqwest::header::TRANSFER_ENCODING,
            reqwest::header::HeaderValue::from_static("chunked"),
        );
        duplicate_chunked.append(
            reqwest::header::TRANSFER_ENCODING,
            reqwest::header::HeaderValue::from_static("chunked"),
        );
        let duplicate = reject_unsupported_transfer_encoding(&duplicate_chunked)
            .expect_err("two chunked headers");
        assert_eq!(duplicate.message, transfer_error("chunked, chunked"));
        assert!(!duplicate.retryable);
        assert_eq!(duplicate.category, FailureCategory::Resume);

        let mut identity_then_chunked = reqwest::header::HeaderMap::new();
        identity_then_chunked.append(
            reqwest::header::TRANSFER_ENCODING,
            reqwest::header::HeaderValue::from_static("identity"),
        );
        identity_then_chunked.append(
            reqwest::header::TRANSFER_ENCODING,
            reqwest::header::HeaderValue::from_static("chunked"),
        );
        assert!(reject_unsupported_transfer_encoding(&identity_then_chunked).is_ok());
        assert!(transfer_encoding_is_accepted_chunked(
            &identity_then_chunked
        ));
        assert!(transfer_encoding_is_accepted_chunked(&transfer_headers(
            Some(b"chunked")
        )));
        assert!(transfer_encoding_is_accepted_chunked(&transfer_headers(
            Some(b"identity, chunked")
        )));
        assert!(!transfer_encoding_is_accepted_chunked(&transfer_headers(
            Some(b"chunked, identity")
        )));
        assert!(!transfer_encoding_is_accepted_chunked(&transfer_headers(
            Some(b"chunked, chunked")
        )));
        assert!(!transfer_encoding_is_accepted_chunked(&duplicate_chunked));
    }

    fn transfer_error(token: &str) -> String {
        format!(
            "Response is Transfer-Encoding: {token}; asked for identity. Use a URL that sends the decoded file."
        )
    }

    fn assert_empty_transfer_token(headers: reqwest::header::HeaderMap, token: &str) {
        let err = reject_unsupported_transfer_encoding(&headers).expect_err(token);
        assert_eq!(err.message, transfer_error(token), "{token}");
        assert!(!err.retryable, "{token}");
        assert_eq!(err.category, FailureCategory::Resume, "{token}");
    }

    #[test]
    fn transfer_encoding_rejects_empty_tokens() {
        for (value, token) in [
            (&b"chunked,"[..], "unrecognized"),
            (&b"chunked, "[..], "unrecognized"),
            (&b"chunked,,"[..], "unrecognized, unrecognized"),
            (&b"chunked\t,"[..], "unrecognized"),
        ] {
            assert_empty_transfer_token(transfer_headers(Some(value)), token);
        }

        let mut empty_second = reqwest::header::HeaderMap::new();
        empty_second.append(
            reqwest::header::TRANSFER_ENCODING,
            reqwest::header::HeaderValue::from_static("chunked"),
        );
        empty_second.append(
            reqwest::header::TRANSFER_ENCODING,
            reqwest::header::HeaderValue::from_static(""),
        );
        assert_empty_transfer_token(empty_second, "unrecognized");

        let mut comma_second = reqwest::header::HeaderMap::new();
        comma_second.append(
            reqwest::header::TRANSFER_ENCODING,
            reqwest::header::HeaderValue::from_static("chunked"),
        );
        comma_second.append(
            reqwest::header::TRANSFER_ENCODING,
            reqwest::header::HeaderValue::from_static(","),
        );
        assert_empty_transfer_token(comma_second, "unrecognized, unrecognized");
    }

    #[test]
    fn content_encoding_name_is_a_short_token_or_unrecognized() {
        let huge = "A".repeat(300 * 1024);
        let huge_err =
            reject_non_identity_content_encoding(&encoding_headers(Some(huge.as_bytes())))
                .expect_err("huge");
        assert!(
            huge_err.message.len() < 200,
            "300 KB coding must not be stored in the error: {}",
            huge_err.message.len()
        );
        assert!(
            huge_err.message.contains("unrecognized"),
            "{}",
            huge_err.message
        );
        assert!(!huge_err.message.contains(&huge[..32]));
        assert!(!huge_err.retryable);

        let markup = "<a href=\"http://evil.example/x\">click</a>";
        let markup_err =
            reject_non_identity_content_encoding(&encoding_headers(Some(markup.as_bytes())))
                .expect_err("markup");
        assert!(
            !markup_err.message.contains("<a href"),
            "markup must not be echoed: {}",
            markup_err.message
        );
        assert!(
            !markup_err.message.contains("evil.example"),
            "{}",
            markup_err.message
        );
        assert!(
            markup_err.message.contains("unrecognized"),
            "{}",
            markup_err.message
        );

        let token_32 = "a".repeat(32);
        let echoed =
            reject_non_identity_content_encoding(&encoding_headers(Some(token_32.as_bytes())))
                .expect_err("32");
        assert!(echoed.message.contains(&token_32), "{}", echoed.message);

        let token_33 = "b".repeat(33);
        let capped =
            reject_non_identity_content_encoding(&encoding_headers(Some(token_33.as_bytes())))
                .expect_err("33");
        assert!(
            capped.message.contains("unrecognized"),
            "{}",
            capped.message
        );
        assert!(!capped.message.contains(&token_33));
    }

    fn content_encoding_error_max() -> usize {
        "Response is Content-Encoding: ".len()
            + CODING_LIST_MAX_BYTES
            + "; asked for identity. Use a URL that sends the decoded file.".len()
    }

    fn assert_capped_coding_list(err: &DownloadError, omitted: usize) {
        let cap = content_encoding_error_max();
        let expected = format!(
            "Response is Content-Encoding: a, a, a, a +{omitted} more; asked for identity. Use a URL that sends the decoded file."
        );
        assert_eq!(err.message, expected);
        assert!(
            err.message.len() <= cap,
            "coding list must stay within {cap} bytes, got {}",
            err.message.len()
        );
        assert!(err.message.contains(&format!("+{omitted} more")));
        assert!(!err.retryable);
        assert_eq!(err.category, FailureCategory::Resume);
    }

    #[test]
    fn coding_list_is_capped_for_two_hundred_thousand_names() {
        let header = "a,".repeat(200_000);
        let err = reject_non_identity_content_encoding(&encoding_headers(Some(header.as_bytes())))
            .expect_err("repeated");
        // Trailing comma is an empty piece. Content-Encoding still skips it.
        assert_capped_coding_list(&err, 200_000 - CODING_LIST_MAX_NAMES);
        assert!(!err.message.contains(&"a, ".repeat(5)));
        let notify = crate::notifications::NotifySendInvocation::from_payload(
            "Download failed",
            &format!("payload.bin — {}", err.message),
        );
        let body = notify.args.last().expect("notify body");
        assert!(
            body.len() <= 1024,
            "notification body must be 1 KiB or less, got {}",
            body.len()
        );
    }

    #[test]
    fn coding_list_caps_one_thousand_tokens_in_one_header() {
        let header = "a,".repeat(1000);
        let err = reject_non_identity_content_encoding(&encoding_headers(Some(header.as_bytes())))
            .expect_err("thousand");
        assert_capped_coding_list(&err, 1000 - CODING_LIST_MAX_NAMES);
    }

    #[test]
    fn coding_list_caps_forty_headers() {
        // 40 headers of 1830 `a` tokens. Joined without a list cap this is the
        // 219_688-character Content-Encoding error.
        let value = "a,".repeat(1830);
        let mut headers = reqwest::header::HeaderMap::new();
        for _ in 0..40 {
            headers.append(
                reqwest::header::CONTENT_ENCODING,
                reqwest::header::HeaderValue::from_str(&value).unwrap(),
            );
        }
        let err = reject_non_identity_content_encoding(&headers).expect_err("forty headers");
        assert_capped_coding_list(&err, 40 * 1830 - CODING_LIST_MAX_NAMES);
    }

    fn content_encoding_message(list: &str) -> String {
        format!(
            "Response is Content-Encoding: {list}; asked for identity. Use a URL that sends the decoded file."
        )
    }

    fn reject_content_codings(tokens: &[&str]) -> DownloadError {
        let header = tokens.join(",");
        reject_non_identity_content_encoding(&encoding_headers(Some(header.as_bytes())))
            .expect_err("coding list")
    }

    /// Three 32-byte tokens. The 4-name cap does not bind (only 3 names).
    /// The joined list is 32 + 2 + 32 + 2 + 32 = 100 bytes.
    /// Two names are 66, plus ` +1 more` (7) is 73, still over 64.
    /// One name plus ` +2 more` is 39, which fits.
    #[test]
    fn coding_list_byte_cap_three_32_byte_tokens() {
        let token = "a".repeat(32);
        let err = reject_content_codings(&[&token, &token, &token]);
        assert_eq!(
            err.message,
            content_encoding_message(&format!("{token} +2 more"))
        );
        assert!(!err.retryable);
        assert_eq!(err.category, FailureCategory::Resume);
    }

    /// 32 + `, ` + 30 = 64. Both names fit, so there is no `+K more`.
    #[test]
    fn coding_list_byte_cap_keeps_exactly_64_bytes() {
        let first = "a".repeat(32);
        let second = "b".repeat(30);
        let list = format!("{first}, {second}");
        assert_eq!(list.len(), 64);
        let err = reject_content_codings(&[&first, &second]);
        assert_eq!(err.message, content_encoding_message(&list));
        assert!(!err.retryable);
        assert_eq!(err.category, FailureCategory::Resume);
    }

    /// 32 + `, ` + 31 = 65. That pair does not fit, so the list is the first
    /// name plus ` +1 more` (39 bytes).
    #[test]
    fn coding_list_byte_cap_shrinks_at_65_bytes() {
        let first = "a".repeat(32);
        let second = "b".repeat(31);
        let uncapped = format!("{first}, {second}");
        assert_eq!(uncapped.len(), 65);
        let err = reject_content_codings(&[&first, &second]);
        assert_eq!(
            err.message,
            content_encoding_message(&format!("{first} +1 more"))
        );
        assert!(!err.retryable);
        assert_eq!(err.category, FailureCategory::Resume);
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
    async fn stream_error_after_full_target_completes() {
        let payload = b"full-body".to_vec();
        let stream = futures_util::stream::iter([
            Ok::<Vec<u8>, DownloadError>(payload.clone()),
            Err(download_error(
                FailureCategory::Network,
                "connection reset by peer".into(),
                true,
            )),
        ]);
        let dir =
            std::env::temp_dir().join(format!("rusticdl-reset-after-end-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("out.bin.part");
        let mut sink = AppendSink::open(&path, 0)
            .await
            .unwrap()
            .with_target(payload.len() as u64);
        let control = AtomicU8::new(0);
        let limiter = GlobalBandwidthLimiter::new(None);
        let end = stream_body_loop(
            stream,
            &mut sink,
            &control,
            limiter.as_ref(),
            Duration::from_secs(5),
            EosPolicy::Complete,
            |_| {},
        )
        .await
        .expect("reset after the last byte must not fail the transfer");
        let StreamEnd::Exhausted { downloaded } = end else {
            panic!("unexpected {end:?}");
        };
        assert_eq!(downloaded, payload.len() as u64);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn stream_body_overrun_stops_at_known_size() {
        let stream = futures_util::stream::iter([Ok::<Vec<u8>, DownloadError>(
            b"0123456789abcdef".to_vec(),
        )]);
        let dir =
            std::env::temp_dir().join(format!("rusticdl-overrun-cap-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("out.bin.part");
        let mut sink = AppendSink::open(&path, 0).await.unwrap().with_target(8);
        let control = AtomicU8::new(0);
        let limiter = GlobalBandwidthLimiter::new(None);
        let err = stream_body_loop(
            stream,
            &mut sink,
            &control,
            limiter.as_ref(),
            Duration::from_secs(5),
            EosPolicy::Complete,
            |_| {},
        )
        .await
        .expect_err("bytes past the known size must fail");
        assert_eq!(err.category, FailureCategory::Resume);
        assert!(!err.retryable);
        assert!(err.message.contains('8'), "{err:?}");
        assert!(err.message.contains("16"), "{err:?}");
        assert_eq!(sink.offset(), 8);
        drop(sink);
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"01234567");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn stream_error_before_target_stays_retryable() {
        let stream = futures_util::stream::iter([
            Ok::<Vec<u8>, DownloadError>(b"partial".to_vec()),
            Err(download_error(
                FailureCategory::Network,
                "connection reset by peer".into(),
                true,
            )),
        ]);
        let dir = std::env::temp_dir().join(format!("rusticdl-reset-mid-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("out.bin.part");
        let mut sink = AppendSink::open(&path, 0).await.unwrap().with_target(100);
        let control = AtomicU8::new(0);
        let limiter = GlobalBandwidthLimiter::new(None);
        let err = stream_body_loop(
            stream,
            &mut sink,
            &control,
            limiter.as_ref(),
            Duration::from_secs(5),
            EosPolicy::Complete,
            |_| {},
        )
        .await
        .expect_err("reset before the target must fail");
        assert!(err.retryable);
        assert_eq!(sink.offset(), b"partial".len() as u64);
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
                EosPolicy::Complete,
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
                EosPolicy::Complete,
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
                EosPolicy::Complete,
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
                EosPolicy::Complete,
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

    #[test]
    fn framed_http2_and_http3_end_of_stream_without_length_completes() {
        assert_eq!(
            eos_policy(Version::HTTP_2, None, false, None),
            EosPolicy::Complete
        );
        assert_eq!(
            eos_policy(Version::HTTP_3, None, false, None),
            EosPolicy::Complete
        );
    }

    #[test]
    fn http1_close_delimited_without_length_stays_incomplete() {
        assert_eq!(
            eos_policy(Version::HTTP_11, None, false, None),
            EosPolicy::UnknownLength
        );
        assert_eq!(
            eos_policy(Version::HTTP_10, None, false, None),
            EosPolicy::UnknownLength
        );
    }

    #[test]
    fn known_length_or_chunked_terminator_stays_complete() {
        assert_eq!(
            eos_policy(Version::HTTP_11, Some(10), false, None),
            EosPolicy::Complete
        );
        assert_eq!(
            eos_policy(Version::HTTP_11, None, true, None),
            EosPolicy::Complete
        );
        assert_eq!(
            eos_policy(Version::HTTP_2, None, false, Some(10)),
            EosPolicy::Complete
        );
    }

    #[tokio::test]
    async fn unknown_length_clean_eof_is_incomplete() {
        let stream =
            futures_util::stream::iter([Ok::<Vec<u8>, DownloadError>(b"partial".to_vec())]);
        let dir =
            std::env::temp_dir().join(format!("rusticdl-unknown-eof-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("out.bin.part");
        let mut sink = AppendSink::open(&path, 0).await.unwrap();
        let control = AtomicU8::new(0);
        let limiter = GlobalBandwidthLimiter::new(None);
        let err = stream_body_loop(
            stream,
            &mut sink,
            &control,
            limiter.as_ref(),
            Duration::from_secs(5),
            EosPolicy::UnknownLength,
            |_| {},
        )
        .await
        .expect_err("clean EOF without a length must not complete");
        assert!(err.retryable);
        assert!(is_lengthless_eof(&err));
        assert_eq!(sink.offset(), b"partial".len() as u64);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    async fn serve_encoded(encoding: &str, body: &[u8]) -> String {
        serve_with_extra_headers(&format!("Content-Encoding: {encoding}\r\n"), body).await
    }

    async fn serve_with_extra_headers(extra: &str, body: &[u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = body.to_vec();
        let extra = extra.to_string();
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
                "HTTP/1.1 200 OK\r\nConnection: close\r\n{extra}Content-Length: {}\r\n\r\n",
                payload.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
            let _ = socket.write_all(&payload).await;
            let _ = socket.shutdown().await;
        });
        format!("http://{addr}/file.bin")
    }

    struct TestDir(std::path::PathBuf);

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn stream_encoded(
        encoding: &str,
        payload: &[u8],
    ) -> (
        std::path::PathBuf,
        Result<StreamEnd, DownloadError>,
        TestDir,
    ) {
        stream_from_url(&serve_encoded(encoding, payload).await, payload).await
    }

    async fn stream_from_url(
        url: &str,
        payload: &[u8],
    ) -> (
        std::path::PathBuf,
        Result<StreamEnd, DownloadError>,
        TestDir,
    ) {
        let response = get_response(url).await;
        let dir = std::env::temp_dir().join(format!("rusticdl-ce-body-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let guard = TestDir(dir.clone());
        let path = dir.join("out.bin.part");
        let mut sink = AppendSink::open(&path, 0)
            .await
            .unwrap()
            .with_target(payload.len() as u64);
        let control = AtomicU8::new(0);
        let limiter = GlobalBandwidthLimiter::new(None);
        let result = stream_body(response, &mut sink, &control, &limiter, |_| {}).await;
        drop(sink);
        (path, result, guard)
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_gzip_content_encoding_before_write() {
        let payload = b"compressed-looking-bytes";
        let (path, result, _dir) = stream_encoded("gzip", payload).await;
        let len = tokio::fs::metadata(&path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        match result {
            Err(err) => {
                assert!(!err.retryable, "{err:?}");
                assert_eq!(err.category, FailureCategory::Resume);
                assert_eq!(
                    err.message,
                    "Response is Content-Encoding: gzip; asked for identity. Use a URL that sends the decoded file."
                );
                assert_eq!(len, 0, "encoded bytes must not land in the .part");
            }
            Ok(end) => panic!("gzip body was written: {end:?} part_len={len}"),
        }
        let _ = tokio::fs::remove_dir_all(path.parent().unwrap()).await;
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_gzip_uppercase_and_list_before_write() {
        for encoding in ["GZIP", "gzip, identity"] {
            let payload = b"compressed-looking-bytes";
            let (path, result, _dir) = stream_encoded(encoding, payload).await;
            let len = tokio::fs::metadata(&path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            match result {
                Err(err) => {
                    assert_eq!(
                        err.message,
                        "Response is Content-Encoding: gzip; asked for identity. Use a URL that sends the decoded file.",
                        "{encoding}"
                    );
                    assert!(!err.retryable);
                    assert_eq!(err.category, FailureCategory::Resume);
                    assert_eq!(len, 0, "{encoding}");
                }
                Ok(end) => panic!("{encoding} body was written: {end:?} part_len={len}"),
            }
            let _ = tokio::fs::remove_dir_all(path.parent().unwrap()).await;
        }
    }

    #[tokio::test]
    async fn issue188_stream_body_identity_content_encoding_still_writes() {
        let payload = b"identity-body-0123456789";
        let (path, result, _dir) = stream_encoded("identity", payload).await;
        let end = result.expect("identity coding is the decoded file");
        match end {
            StreamEnd::Exhausted { downloaded } => assert_eq!(downloaded, payload.len() as u64),
            StreamEnd::Control(outcome) => panic!("unexpected control {outcome:?}"),
        }
        assert_eq!(tokio::fs::read(&path).await.unwrap(), payload);
        let _ = tokio::fs::remove_dir_all(path.parent().unwrap()).await;
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_chunked_identity_before_write() {
        let payload = b"5\r\nhello\r\n0\r\n\r\n";
        let url =
            serve_with_extra_headers("Transfer-Encoding: chunked, identity\r\n", payload).await;
        let (path, result, _dir) = stream_from_url(&url, payload).await;
        let len = tokio::fs::metadata(&path)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0);
        match result {
            Err(err) => {
                assert_eq!(err.message, transfer_error("chunked, identity"));
                assert!(!err.retryable, "{err:?}");
                assert_eq!(err.category, FailureCategory::Resume);
                assert_eq!(
                    len, 0,
                    "an empty file can exist after AppendSink::open; the claim is no payload bytes"
                );
            }
            Ok(end) => panic!("chunk framing was written: {end:?} part_len={len}"),
        }
        let _ = tokio::fs::remove_dir_all(path.parent().unwrap()).await;
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_gzip_transfer_encoding_before_write() {
        let payload = b"compressed-looking-bytes";
        let url = serve_with_extra_headers("Transfer-Encoding: gzip\r\n", payload).await;
        let (path, result, _dir) = stream_from_url(&url, payload).await;
        let len = tokio::fs::metadata(&path)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0);
        match result {
            Err(err) => {
                assert!(!err.retryable, "{err:?}");
                assert_eq!(err.category, FailureCategory::Resume);
                assert_eq!(
                    err.message,
                    "Response is Transfer-Encoding: gzip; asked for identity. Use a URL that sends the decoded file."
                );
                assert_eq!(
                    len, 0,
                    "an empty file can exist after AppendSink::open; the claim is no payload bytes"
                );
            }
            Ok(end) => panic!("gzip transfer-encoding body was written: {end:?} part_len={len}"),
        }
        let _ = tokio::fs::remove_dir_all(path.parent().unwrap()).await;
    }

    #[tokio::test]
    async fn issue188_stream_body_chunked_and_identity_transfer_encoding_still_write() {
        let payload = b"hello";
        let chunked = b"HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let chunked_url = serve_raw_response(chunked).await;
        let (path, result, _dir) = stream_from_url(&chunked_url, payload).await;
        let end = result.expect("chunked framing is not an encoded file");
        match end {
            StreamEnd::Exhausted { downloaded } => assert_eq!(downloaded, payload.len() as u64),
            StreamEnd::Control(outcome) => panic!("unexpected control {outcome:?}"),
        }
        assert_eq!(tokio::fs::read(&path).await.unwrap(), payload);
        let _ = tokio::fs::remove_dir_all(path.parent().unwrap()).await;

        let identity_url =
            serve_with_extra_headers("Transfer-Encoding: identity\r\n", payload).await;
        let (path, result, _dir) = stream_from_url(&identity_url, payload).await;
        let end = result.expect("identity transfer coding is the raw file");
        match end {
            StreamEnd::Exhausted { downloaded } => assert_eq!(downloaded, payload.len() as u64),
            StreamEnd::Control(outcome) => panic!("unexpected control {outcome:?}"),
        }
        assert_eq!(tokio::fs::read(&path).await.unwrap(), payload);
        let _ = tokio::fs::remove_dir_all(path.parent().unwrap()).await;
    }

    async fn serve_raw_response(raw: &[u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let raw = raw.to_vec();
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
                if collected.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = socket.write_all(&raw).await;
            let _ = socket.shutdown().await;
        });
        format!("http://{addr}/file.bin")
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_gzip_on_a_second_content_encoding_header() {
        let payload = b"compressed-looking-bytes";
        let url = serve_with_extra_headers(
            "Content-Encoding: identity\r\nContent-Encoding: gzip\r\n",
            payload,
        )
        .await;
        let (path, result, _dir) = stream_from_url(&url, payload).await;
        let len = tokio::fs::metadata(&path)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0);
        match result {
            Err(err) => {
                assert!(!err.retryable, "{err:?}");
                assert_eq!(err.category, FailureCategory::Resume);
                assert_eq!(
                    err.message,
                    "Response is Content-Encoding: gzip; asked for identity. Use a URL that sends the decoded file."
                );
                assert_eq!(len, 0, "encoded bytes must not land in the .part");
            }
            Ok(end) => panic!("split gzip headers were written: {end:?} part_len={len}"),
        }
        let _ = tokio::fs::remove_dir_all(path.parent().unwrap()).await;
    }

    async fn assert_stream_transfer_rejected(extra: &str, payload: &[u8]) {
        let url = serve_with_extra_headers(extra, payload).await;
        let (path, result, _dir) = stream_from_url(&url, payload).await;
        let len = tokio::fs::metadata(&path)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0);
        match result {
            Err(err) => {
                assert!(!err.retryable, "{err:?}");
                assert_eq!(err.category, FailureCategory::Resume);
                assert_eq!(
                    err.message,
                    "Response is Transfer-Encoding: gzip; asked for identity. Use a URL that sends the decoded file."
                );
                assert_eq!(
                    len, 0,
                    "an empty file can exist after AppendSink::open; the claim is no payload bytes"
                );
            }
            Ok(end) => panic!("transfer-encoded body was written: {end:?} part_len={len}"),
        }
        let _ = tokio::fs::remove_dir_all(path.parent().unwrap()).await;
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_gzip_chunked_transfer_encoding() {
        assert_stream_transfer_rejected(
            "Transfer-Encoding: gzip, chunked\r\n",
            b"compressed-looking-bytes",
        )
        .await;
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_chunked_gzip_transfer_encoding() {
        assert_stream_transfer_rejected(
            "Transfer-Encoding: chunked, gzip\r\n",
            b"compressed-looking-bytes",
        )
        .await;
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_split_transfer_encoding_headers() {
        assert_stream_transfer_rejected(
            "Transfer-Encoding: gzip\r\nTransfer-Encoding: chunked\r\n",
            b"compressed-looking-bytes",
        )
        .await;
    }

    async fn assert_stream_empty_transfer_rejected(extra: &str, token: &str) {
        let payload = b"5\r\nhello\r\n0\r\n\r\n";
        let url = serve_with_extra_headers(extra, payload).await;
        let (path, result, _dir) = stream_from_url(&url, payload).await;
        let len = tokio::fs::metadata(&path)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0);
        match result {
            Err(err) => {
                assert_eq!(err.message, transfer_error(token), "{extra}");
                assert!(!err.retryable, "{extra}: {err:?}");
                assert_eq!(err.category, FailureCategory::Resume, "{extra}");
                assert_eq!(
                    len, 0,
                    "an empty file can exist after AppendSink::open; the claim is no payload bytes"
                );
            }
            Ok(end) => panic!("chunk framing was written: {end:?} part_len={len} extra={extra:?}"),
        }
        let _ = tokio::fs::remove_dir_all(path.parent().unwrap()).await;
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_chunked_trailing_comma() {
        assert_stream_empty_transfer_rejected("Transfer-Encoding: chunked,\r\n", "unrecognized")
            .await;
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_chunked_double_comma() {
        assert_stream_empty_transfer_rejected(
            "Transfer-Encoding: chunked,,\r\n",
            "unrecognized, unrecognized",
        )
        .await;
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_chunked_trailing_space() {
        assert_stream_empty_transfer_rejected("Transfer-Encoding: chunked, \r\n", "unrecognized")
            .await;
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_chunked_tab_comma() {
        assert_stream_empty_transfer_rejected("Transfer-Encoding: chunked\t,\r\n", "unrecognized")
            .await;
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_chunked_then_empty_header() {
        assert_stream_empty_transfer_rejected(
            "Transfer-Encoding: chunked\r\nTransfer-Encoding: \r\n",
            "unrecognized",
        )
        .await;
    }

    #[tokio::test]
    async fn issue188_stream_body_rejects_chunked_then_comma_header() {
        assert_stream_empty_transfer_rejected(
            "Transfer-Encoding: chunked\r\nTransfer-Encoding: ,\r\n",
            "unrecognized, unrecognized",
        )
        .await;
    }
}
