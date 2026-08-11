//! Process wait + single-instance helpers.

use std::time::{Duration, Instant};

use crate::ui::ProgressSink;

const MUTEX_NAME: &str = "Local\\RusticDL.Updater";

/// Returns true if this process owns the single-instance mutex.
pub fn try_acquire_single_instance() -> bool {
    #[cfg(windows)]
    {
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
            Err(_) => true, // if mutex APIs fail, don't block updates
        }
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Wait until `pid` exits, or until `timeout`.
pub fn wait_for_process_exit(
    pid: u32,
    timeout: Duration,
    progress: &dyn ProgressSink,
) -> Result<(), WaitError> {
    progress.set_status("Waiting for RusticDL to close…".into());
    progress.set_progress_unknown();

    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };

        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) };
        let handle = match handle {
            Ok(h) if !h.is_invalid() => h,
            _ => {
                // Process already gone (or access denied treated as gone for update purposes).
                return Ok(());
            }
        };

        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return Err(WaitError::Timeout);
            }
            let ms = remaining.as_millis().min(u32::MAX as u128) as u32;
            // Poll in chunks so the UI can keep marquee animation via posted messages.
            let slice = ms.min(250);
            let wait = unsafe { WaitForSingleObject(handle, slice) };
            if wait == WAIT_OBJECT_0 {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                // Brief settle so file handles are fully released before NSIS KillProcess/overwrite.
                std::thread::sleep(Duration::from_millis(400));
                return Ok(());
            }
            if wait == WAIT_TIMEOUT {
                continue;
            }
            // Unexpected wait result — treat as exited to avoid stuck updater.
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Ok(());
        }
    }

    #[cfg(not(windows))]
    {
        let _ = (pid, timeout, progress);
        Err(WaitError::Timeout)
    }
}

#[derive(Debug)]
pub enum WaitError {
    Timeout,
}
