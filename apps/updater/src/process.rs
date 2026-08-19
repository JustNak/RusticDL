//! Process wait, quit/kill, and single-instance helpers.

use std::path::Path;
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
                // Brief settle so file handles are fully released before overwrite.
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

/// Close rusticdl before NSIS overwrites files.
///
/// Asks matching processes to quit, then kills leftovers. This must run
/// immediately before replace so a respawn during download cannot be
/// Restart-Manager-relaunched mid-install (the double-launch flash).
pub fn close_app_for_replace(
    wait_pid: Option<u32>,
    app_exe: &Path,
    timeout: Duration,
    progress: &dyn ProgressSink,
) -> Result<(), WaitError> {
    progress.set_status("Closing RusticDL…".into());
    progress.set_progress_unknown();

    let timeout = timeout.max(Duration::from_secs(5));
    let deadline = Instant::now() + timeout;

    if let Some(pid) = wait_pid {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            // Best-effort wait for the handoff pid; stragglers are quit/killed next.
            let _ = wait_for_process_exit(pid, remaining, progress);
        }
    }

    #[cfg(windows)]
    {
        progress.set_status("Closing RusticDL…".into());
        let remaining = deadline.saturating_duration_since(Instant::now());
        if win::close_matching_app_processes(app_exe, remaining) {
            std::thread::sleep(Duration::from_millis(400));
            return Ok(());
        }
        return Err(WaitError::Timeout);
    }

    #[cfg(not(windows))]
    {
        let _ = app_exe;
        Ok(())
    }
}

/// True when `process_name` is the main app image, never the updater helper.
pub fn is_target_app_exe_name(process_name: &str, app_exe_name: &str) -> bool {
    let process_name = file_name_only(process_name);
    let app_exe_name = file_name_only(app_exe_name);
    if process_name.is_empty() || app_exe_name.is_empty() {
        return false;
    }
    if process_name.eq_ignore_ascii_case("rusticdl-updater.exe")
        || process_name.eq_ignore_ascii_case("rusticdl-updater")
    {
        return false;
    }
    process_name.eq_ignore_ascii_case(app_exe_name)
}

fn file_name_only(name: &str) -> &str {
    name.rsplit(['\\', '/']).next().unwrap_or(name).trim()
}

#[derive(Debug)]
pub enum WaitError {
    Timeout,
}

#[cfg(windows)]
mod win {
    use super::is_target_app_exe_name;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, WAIT_TIMEOUT, WPARAM};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
        WaitForSingleObject, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
    };

    pub fn close_matching_app_processes(app_exe: &Path, timeout: Duration) -> bool {
        let mut pids = matching_app_pids(app_exe);
        if pids.is_empty() {
            return true;
        }

        request_quit(&pids);

        let deadline = Instant::now() + timeout.max(Duration::from_secs(2));
        loop {
            pids.retain(|pid| is_pid_running(*pid));
            if pids.is_empty() {
                return true;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        for pid in &pids {
            let _ = terminate_pid(*pid);
        }

        let kill_deadline = Instant::now() + Duration::from_secs(3);
        loop {
            pids.retain(|pid| is_pid_running(*pid));
            if pids.is_empty() {
                return true;
            }
            if Instant::now() >= kill_deadline {
                return matching_app_pids(app_exe).is_empty();
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    fn matching_app_pids(app_exe: &Path) -> Vec<u32> {
        let Some(app_name) = app_exe.file_name().and_then(|n| n.to_str()) else {
            return Vec::new();
        };
        let self_pid = unsafe { GetCurrentProcessId() };
        let snapshot = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
            Ok(h) if !h.is_invalid() => h,
            _ => return Vec::new(),
        };

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut pids = Vec::new();
        let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
        while ok {
            let name = wchar_file_name(&entry.szExeFile);
            if entry.th32ProcessID != self_pid && is_target_app_exe_name(&name, app_name) {
                if image_path_matches(entry.th32ProcessID, app_exe) {
                    pids.push(entry.th32ProcessID);
                }
            }
            ok = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
        }
        unsafe {
            let _ = CloseHandle(snapshot);
        }
        pids
    }

    fn image_path_matches(pid: u32, app_exe: &Path) -> bool {
        let Some(image) = process_image_path(pid) else {
            // Name already matched rusticdl.exe (not the updater). Include it.
            return true;
        };
        paths_equal_ignore_case(Path::new(&image), app_exe)
    }

    fn process_image_path(pid: u32) -> Option<String> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? };
        if handle.is_invalid() {
            return None;
        }
        let mut buf = [0u16; 512];
        let mut size = buf.len() as u32;
        let ok = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut size,
            )
        };
        unsafe {
            let _ = CloseHandle(handle);
        }
        if ok.is_err() || size == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }

    fn paths_equal_ignore_case(a: &Path, b: &Path) -> bool {
        if a == b {
            return true;
        }
        let a_s = a.to_string_lossy();
        let b_s = b.to_string_lossy();
        if a_s.eq_ignore_ascii_case(&b_s) {
            return true;
        }
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => {
                a == b
                    || a.to_string_lossy()
                        .eq_ignore_ascii_case(&b.to_string_lossy())
            }
            _ => false,
        }
    }

    fn wchar_file_name(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    fn request_quit(pids: &[u32]) {
        if pids.is_empty() {
            return;
        }
        let boxed = Box::new(pids.to_vec());
        let raw = Box::into_raw(boxed);
        unsafe {
            let _ = EnumWindows(Some(enum_close_proc), LPARAM(raw as isize));
            drop(Box::from_raw(raw));
        }
    }

    unsafe extern "system" fn enum_close_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        if lparam.0 == 0 {
            return BOOL(1);
        }
        let pids = &*(lparam.0 as *const Vec<u32>);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pids.contains(&pid) {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        BOOL(1)
    }

    fn terminate_pid(pid: u32) -> bool {
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) };
        let handle = match handle {
            Ok(h) if !h.is_invalid() => h,
            _ => return false,
        };
        let ok = unsafe { TerminateProcess(handle, 1) }.is_ok();
        if ok {
            let _ = unsafe { WaitForSingleObject(handle, 2_000) };
        }
        unsafe {
            let _ = CloseHandle(handle);
        }
        ok
    }

    fn is_pid_running(pid: u32) -> bool {
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) };
        let handle = match handle {
            Ok(h) if !h.is_invalid() => h,
            _ => return false,
        };
        let wait = unsafe { WaitForSingleObject(handle, 0) };
        unsafe {
            let _ = CloseHandle(handle);
        }
        wait == WAIT_TIMEOUT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_name_is_main_app_not_updater() {
        assert!(is_target_app_exe_name("rusticdl.exe", "rusticdl.exe"));
        assert!(is_target_app_exe_name("RusticDL.exe", "rusticdl.exe"));
        assert!(is_target_app_exe_name(
            r"C:\Apps\RusticDL\rusticdl.exe",
            "rusticdl.exe"
        ));
        assert!(!is_target_app_exe_name(
            "rusticdl-updater.exe",
            "rusticdl.exe"
        ));
        assert!(!is_target_app_exe_name(
            "rusticdl-updater.exe",
            "rusticdl-updater.exe"
        ));
        assert!(!is_target_app_exe_name("notepad.exe", "rusticdl.exe"));
        assert!(!is_target_app_exe_name("", "rusticdl.exe"));
    }
}
