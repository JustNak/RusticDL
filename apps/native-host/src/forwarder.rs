use crate::protocol::{AppRequestEnvelope, AppResponseEnvelope};
use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_PIPE_PATH: &str = r"\\.\pipe\rusticdl.v1";
const DEFAULT_APP_EXECUTABLE: &str = "rusticdl.exe";
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
        let pipe_path =
            std::env::var("RUSTICDL_PIPE_PATH").unwrap_or_else(|_| DEFAULT_PIPE_PATH.to_string());
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
    let mut stream = open_pipe_with_retry(pipe_path)?;

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

fn open_pipe_with_retry(pipe_path: &str) -> Result<std::fs::File, ForwarderError> {
    let mut last_error = None;
    for attempt in 0..PIPE_OPEN_RETRIES {
        match open_pipe(pipe_path) {
            Ok(file) => return Ok(file),
            Err(error) => {
                let kind = error.kind();
                // Not found / busy / temporary access races while the server re-arms instances.
                let retryable = kind == std::io::ErrorKind::NotFound
                    || kind == std::io::ErrorKind::WouldBlock
                    || kind == std::io::ErrorKind::TimedOut
                    || kind == std::io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(231) // ERROR_PIPE_BUSY
                    || error.raw_os_error() == Some(5); // ERROR_ACCESS_DENIED (re-arm race)
                last_error = Some(error);
                if !retryable || attempt + 1 == PIPE_OPEN_RETRIES {
                    break;
                }
                thread::sleep(PIPE_OPEN_RETRY_DELAY);
            }
        }
    }
    Err(map_open_error(last_error.unwrap_or_else(|| {
        std::io::Error::other("pipe open failed")
    })))
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

fn map_open_error(error: std::io::Error) -> ForwarderError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return ForwarderError::AppUnreachable;
    }

    if error.raw_os_error() == Some(5) || error.kind() == std::io::ErrorKind::PermissionDenied {
        return ForwarderError::Transport(
            "Could not connect to the RusticDL pipe (access denied). \
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

    #[test]
    fn app_response_reader_rejects_oversized_lines() {
        let raw = vec![b'x'; MAX_APP_RESPONSE_BYTES + 1];
        let mut reader = BufReader::new(Cursor::new(raw));

        let error = read_limited_response_line(&mut reader)
            .expect_err("oversized app response should reject");

        assert!(matches!(error, ForwarderError::Transport(_)));
    }
}
