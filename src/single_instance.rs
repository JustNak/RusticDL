//! A second launch activates the existing window via the IPC
//! `show_window` request and exits instead of opening a duplicate UI.

use crate::branding::ipc_transport_path;
use crate::ipc::PROTOCOL_VERSION;

/// Named mutex held for the lifetime of the primary process.
#[cfg(windows)]
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

/// If another instance holds the lock, send `show_window` over IPC (best-effort)
/// and return [`InstanceRole::Secondary`].
pub fn claim_instance() -> InstanceRole {
    #[cfg(windows)]
    {
        if !try_acquire_mutex() {
            let _ = activate_existing_instance();
            return InstanceRole::Secondary;
        }
        InstanceRole::Primary
    }
    #[cfg(unix)]
    {
        if !try_acquire_flock() {
            let _ = activate_existing_instance();
            return InstanceRole::Secondary;
        }
        InstanceRole::Primary
    }
    #[cfg(not(any(windows, unix)))]
    {
        InstanceRole::Primary
    }
}

#[cfg(windows)]
fn try_acquire_mutex() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        GetLastError, SetLastError, ERROR_ALREADY_EXISTS, WIN32_ERROR,
    };
    use windows::Win32::System::Threading::CreateMutexW;

    let wide: Vec<u16> = MUTEX_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // Clear last error so a stale ERROR_ALREADY_EXISTS cannot misclassify primary.
    unsafe { SetLastError(WIN32_ERROR(0)) };
    // Keep the kernel mutex open for process lifetime. On windows 0.61, HANDLE is
    // Copy and is only closed via Owned/Free — do not wrap this handle in Owned.
    let result = unsafe { CreateMutexW(None, true, PCWSTR(wide.as_ptr())) };
    match result {
        Ok(_handle) => {
            let err = unsafe { GetLastError() };
            err != ERROR_ALREADY_EXISTS
        }
        Err(_) => true,
    }
}

#[cfg(unix)]
fn try_acquire_flock() -> bool {
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;
    use std::sync::OnceLock;

    static INSTANCE_LOCK: OnceLock<std::fs::File> = OnceLock::new();

    let path = crate::branding::instance_lock_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = match OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(_) => return true,
    };
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        let _ = INSTANCE_LOCK.set(file);
        true
    } else {
        false
    }
}

/// Ask the primary instance to restore/focus its main window.
#[cfg(any(windows, unix))]
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

    let mut stream = match open_pipe(&ipc_transport_path()) {
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
        Ok(n) if n > 0 => line.contains("\"ok\":true") || line.contains("\"ok\": true"),
        _ => false,
    }
}

#[cfg(unix)]
fn send_show_window_once(request_json: &str) -> bool {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = match UnixStream::connect(ipc_transport_path()) {
        Ok(stream) => stream,
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
        Ok(n) if n > 0 => line.contains("\"ok\":true") || line.contains("\"ok\": true"),
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
