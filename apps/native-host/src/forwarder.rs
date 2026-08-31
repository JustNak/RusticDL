use crate::protocol::{AppRequestEnvelope, AppResponseEnvelope};
use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
pub const DEFAULT_PIPE_PATH: &str = r"\\.\pipe\rusticdl.v1";
#[cfg(not(windows))]
pub const DEFAULT_PIPE_PATH: &str = "rusticdl.v1.sock";

#[cfg(windows)]
const DEFAULT_APP_EXECUTABLE: &str = "rusticdl.exe";
#[cfg(not(windows))]
const DEFAULT_APP_EXECUTABLE: &str = "rusticdl";
const CONNECT_ATTEMPTS: usize = 10;
const CONNECT_DELAY: Duration = Duration::from_millis(300);
const APP_FORWARD_TIMEOUT: Duration = Duration::from_secs(15);
const APP_PROMPT_FORWARD_TIMEOUT: Duration = Duration::from_secs(5 * 60 + 30);
const MAX_APP_RESPONSE_BYTES: usize = 256 * 1024;
const PIPE_OPEN_RETRIES: usize = 20;
const PIPE_OPEN_RETRY_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub enum ForwarderError {
    AppNotInstalled,
    AppUnreachable,
    Serialization(String),
    Transport(String),
}

pub struct AppForwarder {
    pipe_path: String,
    desktop_path: PathBuf,
}

impl AppForwarder {
    pub fn from_environment() -> Self {
        let pipe_path = default_transport_path();
        let desktop_path = resolve_desktop_path();

        Self {
            pipe_path,
            desktop_path,
        }
    }

    pub fn launch_app(&self) -> Result<(), ForwarderError> {
        if !self.desktop_path.exists() {
            return Err(ForwarderError::AppNotInstalled);
        }

        Command::new(&self.desktop_path)
            .spawn()
            .map(|_| ())
            .map_err(|error| {
                ForwarderError::Transport(format!("Could not launch RusticDL: {error}"))
            })
    }

    pub fn send<T>(
        &self,
        request: &AppRequestEnvelope<T>,
    ) -> Result<AppResponseEnvelope, ForwarderError>
    where
        T: Serialize,
    {
        let timeout = if request.message_type == "prompt_download" {
            APP_PROMPT_FORWARD_TIMEOUT
        } else {
            APP_FORWARD_TIMEOUT
        };
        let deadline = Instant::now() + timeout;
        match self.send_once_before_deadline(request, deadline) {
            Ok(response) => Ok(response),
            Err(ForwarderError::AppUnreachable) => {
                self.launch_app()?;

                for _ in 0..CONNECT_ATTEMPTS {
                    let Some(remaining) = remaining_timeout(deadline) else {
                        return Err(ForwarderError::AppUnreachable);
                    };
                    thread::sleep(remaining.min(CONNECT_DELAY));
                    if let Ok(response) = self.send_once_before_deadline(request, deadline) {
                        return Ok(response);
                    }
                }

                Err(ForwarderError::AppUnreachable)
            }
            Err(error) => Err(error),
        }
    }

    fn send_once_before_deadline<T>(
        &self,
        request: &AppRequestEnvelope<T>,
        deadline: Instant,
    ) -> Result<AppResponseEnvelope, ForwarderError>
    where
        T: Serialize,
    {
        let request_json = serde_json::to_string(request).map_err(|error| {
            ForwarderError::Serialization(format!("Could not serialize app request: {error}"))
        })?;

        let timeout = remaining_timeout(deadline).ok_or(ForwarderError::AppUnreachable)?;
        self.send_serialized_with_timeout(request_json, timeout)
    }

    fn send_serialized_with_timeout(
        &self,
        request_json: String,
        timeout: Duration,
    ) -> Result<AppResponseEnvelope, ForwarderError> {
        let pipe_path = self.pipe_path.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(send_serialized_once(&pipe_path, &request_json));
        });

        receiver
            .recv_timeout(timeout)
            .map_err(|_| ForwarderError::AppUnreachable)?
    }
}

fn send_serialized_once(
    pipe_path: &str,
    request_json: &str,
) -> Result<AppResponseEnvelope, ForwarderError> {
    #[cfg(windows)]
    {
        let stream = open_pipe_with_retry(pipe_path)?;
        write_request_read_response(stream, request_json)
    }
    #[cfg(unix)]
    {
        let stream = open_unix_socket_with_retry(pipe_path)?;
        write_request_read_response(stream, request_json)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (pipe_path, request_json);
        Err(ForwarderError::Transport(
            "App transport is not available on this platform.".into(),
        ))
    }
}

fn write_request_read_response<S: std::io::Read + Write>(
    mut stream: S,
    request_json: &str,
) -> Result<AppResponseEnvelope, ForwarderError> {
    stream
        .write_all(request_json.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .and_then(|_| stream.flush())
        .map_err(|error| {
            ForwarderError::Transport(format!("Could not send app request: {error}"))
        })?;

    let mut reader = BufReader::new(stream);
    let response_line = read_limited_response_line(&mut reader)?;

    if response_line.trim().is_empty() {
        return Err(ForwarderError::AppUnreachable);
    }

    serde_json::from_str::<AppResponseEnvelope>(&response_line).map_err(|error| {
        ForwarderError::Serialization(format!("Could not parse app response: {error}"))
    })
}

fn is_retryable_open_error(error: &std::io::Error) -> bool {
    let kind = error.kind();
    kind == std::io::ErrorKind::NotFound
        || kind == std::io::ErrorKind::WouldBlock
        || kind == std::io::ErrorKind::TimedOut
        || kind == std::io::ErrorKind::PermissionDenied
        || kind == std::io::ErrorKind::ConnectionRefused
        || error.raw_os_error() == Some(231) // ERROR_PIPE_BUSY
        || error.raw_os_error() == Some(5) // ERROR_ACCESS_DENIED (re-arm race)
}

fn retry_open<T, F>(mut open: F) -> Result<T, ForwarderError>
where
    F: FnMut() -> std::io::Result<T>,
{
    let mut last_error = None;
    for attempt in 0..PIPE_OPEN_RETRIES {
        match open() {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                let retryable = is_retryable_open_error(&error);
                last_error = Some(error);
                if !retryable || attempt + 1 == PIPE_OPEN_RETRIES {
                    break;
                }
                thread::sleep(PIPE_OPEN_RETRY_DELAY);
            }
        }
    }
    Err(map_open_error(last_error.unwrap_or_else(|| {
        std::io::Error::other("transport open failed")
    })))
}

#[cfg(windows)]
fn open_pipe_with_retry(pipe_path: &str) -> Result<std::fs::File, ForwarderError> {
    retry_open(|| open_pipe(pipe_path))
}

#[cfg(unix)]
fn open_unix_socket_with_retry(
    socket_path: &str,
) -> Result<std::os::unix::net::UnixStream, ForwarderError> {
    retry_open(|| std::os::unix::net::UnixStream::connect(socket_path))
}

fn open_pipe(pipe_path: &str) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OVERLAPPED matches the Tokio server pipe mode.
        const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;

        wait_named_pipe(pipe_path, 1_000);

        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .open(pipe_path)
    }
    #[cfg(not(windows))]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe_path)
    }
}

#[cfg(windows)]
fn wait_named_pipe(pipe_path: &str, timeout_ms: u32) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn WaitNamedPipeW(lpNamedPipeName: *const u16, nTimeOut: u32) -> i32;
    }

    let wide: Vec<u16> = OsStr::new(pipe_path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let _ = WaitNamedPipeW(wide.as_ptr(), timeout_ms);
    }
}

fn read_limited_response_line<R: BufRead>(reader: &mut R) -> Result<String, ForwarderError> {
    let mut response = Vec::new();

    loop {
        let available = reader.fill_buf().map_err(|error| {
            ForwarderError::Transport(format!("Could not read app response: {error}"))
        })?;

        if available.is_empty() {
            break;
        }

        let newline_index = available.iter().position(|byte| *byte == b'\n');
        let read_len = newline_index
            .map(|index| index.saturating_add(1))
            .unwrap_or(available.len());

        if response.len().saturating_add(read_len) > MAX_APP_RESPONSE_BYTES {
            return Err(ForwarderError::Transport(format!(
                "App response exceeds {MAX_APP_RESPONSE_BYTES} bytes."
            )));
        }

        response.extend_from_slice(&available[..read_len]);
        reader.consume(read_len);

        if newline_index.is_some() {
            break;
        }
    }

    String::from_utf8(response).map_err(|error| {
        ForwarderError::Serialization(format!("Could not decode app response: {error}"))
    })
}

fn default_transport_path() -> String {
    if let Ok(path) = std::env::var("RUSTICDL_PIPE_PATH") {
        if !path.trim().is_empty() {
            return path;
        }
    }
    #[cfg(windows)]
    {
        DEFAULT_PIPE_PATH.to_string()
    }
    #[cfg(unix)]
    {
        default_unix_socket_path()
    }
    #[cfg(not(any(windows, unix)))]
    {
        DEFAULT_PIPE_PATH.to_string()
    }
}

#[cfg(unix)]
fn default_unix_socket_path() -> String {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return format!("{trimmed}/{DEFAULT_PIPE_PATH}");
        }
    }
    format!("/tmp/rusticdl-{}/{}", unix_uid(), DEFAULT_PIPE_PATH)
}

#[cfg(unix)]
fn unix_uid() -> u32 {
    unsafe { libc::getuid() }
}

fn map_open_error(error: std::io::Error) -> ForwarderError {
    if error.kind() == std::io::ErrorKind::NotFound
        || error.kind() == std::io::ErrorKind::ConnectionRefused
    {
        return ForwarderError::AppUnreachable;
    }

    if error.raw_os_error() == Some(5) || error.kind() == std::io::ErrorKind::PermissionDenied {
        return ForwarderError::Transport(
            "Could not connect to the RusticDL app transport (access denied). \
Start RusticDL without elevation (not as Administrator), \
then reload the extension and try again."
                .into(),
        );
    }

    ForwarderError::Transport(format!("Could not connect to app transport: {error}"))
}

fn resolve_desktop_path() -> PathBuf {
    if let Ok(path) = std::env::var("RUSTICDL_DESKTOP_PATH") {
        return PathBuf::from(path);
    }

    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .map(|parent| parent.join(DEFAULT_APP_EXECUTABLE))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_APP_EXECUTABLE))
}

fn remaining_timeout(deadline: Instant) -> Option<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        None
    } else {
        Some(remaining)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[cfg(unix)]
    #[test]
    fn default_unix_socket_uses_xdg_runtime_dir() {
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        let pipe_prev = std::env::var_os("RUSTICDL_PIPE_PATH");
        std::env::remove_var("RUSTICDL_PIPE_PATH");
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        let path = default_transport_path();
        match prev {
            Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        match pipe_prev {
            Some(value) => std::env::set_var("RUSTICDL_PIPE_PATH", value),
            None => std::env::remove_var("RUSTICDL_PIPE_PATH"),
        }
        assert_eq!(path, "/run/user/1000/rusticdl.v1.sock");
    }

    #[test]
    fn app_response_reader_rejects_oversized_lines() {
        let raw = vec![b'x'; MAX_APP_RESPONSE_BYTES + 1];
        let mut reader = BufReader::new(Cursor::new(raw));

        let error = read_limited_response_line(&mut reader)
            .expect_err("oversized app response should reject");

        assert!(matches!(error, ForwarderError::Transport(_)));
    }
}
