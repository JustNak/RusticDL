//! Single-instance guard for the main desktop app (Windows).
//!
//! A second launch activates the existing window via the named-pipe
//! `show_window` request and exits instead of opening a duplicate UI.

use crate::branding::PIPE_NAME;
use crate::ipc::PROTOCOL_VERSION;

/// Named mutex held for the lifetime of the primary process.
const MUTEX_NAME: &str = "Local\\RusticDL.App";

const ACTIVATE_ATTEMPTS: usize = 15;
const ACTIVATE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// Result of claiming the single-instance lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceRole {
    /// This process owns the lock and should run the full UI.
    Primary,
    /// Another instance is already running (and was asked to show its window).
    Secondary,
}

/// Try to become the sole running instance.
///
/// On Windows, if another instance holds the mutex, send `show_window` over the
/// IPC pipe (best-effort) and return [`InstanceRole::Secondary`].
pub fn claim_instance() -> InstanceRole {
    #[cfg(windows)]
    {
        if !try_acquire_mutex() {
            let _ = activate_existing_instance();
            return InstanceRole::Secondary;
        }
        InstanceRole::Primary
    }
    #[cfg(not(windows))]
    {
        InstanceRole::Primary
    }
}

#[cfg(windows)]
fn try_acquire_mutex() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    let wide: Vec<u16> = MUTEX_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // Intentionally leaked: held for process lifetime.
    let result = unsafe { CreateMutexW(None, true, PCWSTR(wide.as_ptr())) };
    match result {
        Ok(_handle) => {
            let err = unsafe { GetLastError() };
            err != ERROR_ALREADY_EXISTS
        }
        // If mutex APIs fail, allow startup rather than blocking the app.
        Err(_) => true,
    }
}

/// Ask the primary instance to restore/focus its main window.
#[cfg(windows)]
fn activate_existing_instance() -> bool {
    let request = serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "requestId": "single-instance-activate",
        "type": "show_window",
        "payload": {}
    });
    let Ok(body) = serde_json::to_string(&request) else {
        return false;
    };

    for attempt in 0..ACTIVATE_ATTEMPTS {
        if send_show_window_once(&body) {
            return true;
        }
        if attempt + 1 < ACTIVATE_ATTEMPTS {
            std::thread::sleep(ACTIVATE_RETRY_DELAY);
        }
    }
    false
}

#[cfg(windows)]
fn send_show_window_once(request_json: &str) -> bool {
    use std::io::{BufRead, BufReader, Write};

    let mut stream = match open_pipe(PIPE_NAME) {
        Ok(f) => f,
        Err(_) => return false,
    };

    if stream
        .write_all(request_json.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .and_then(|_| stream.flush())
        .is_err()
    {
        return false;
    }

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(n) if n > 0 => {
            // Any non-empty JSON line means the primary handled the request.
            line.contains("\"ok\"")
        }
        _ => false,
    }
}

#[cfg(windows)]
fn open_pipe(pipe_path: &str) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_FLAG_OVERLAPPED matches the Tokio server pipe mode.
    const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;

    wait_named_pipe(pipe_path, 500);

    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(pipe_path)
}

#[cfg(windows)]
fn wait_named_pipe(pipe_path: &str, timeout_ms: u32) {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Pipes::WaitNamedPipeW;

    let wide: Vec<u16> = std::ffi::OsStr::new(pipe_path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let _ = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), timeout_ms) };
}
