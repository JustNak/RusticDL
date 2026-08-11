//! Run the NSIS installer and relaunch the main app.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::ui::ProgressSink;

/// Run a downloaded NSIS setup silently. Updater owns relaunch, so no `/R`.
pub fn run_silent_installer(path: &Path, progress: &dyn ProgressSink) -> Result<(), String> {
    progress.set_status("Installing update…".into());
    progress.set_progress_unknown();

    if !path.is_file() {
        return Err(format!("Installer missing: {}", path.display()));
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Keep the console hidden; do not detach — we need to wait for completion.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const ERROR_ELEVATION_REQUIRED: i32 = 740;

        let status = match Command::new(path)
            .args(["/S"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
        {
            Ok(status) => status,
            Err(e) if e.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) => {
                // Per-machine setups (or Installer Detection) need elevation.
                // ShellExecuteEx with "runas" shows UAC and can wait for exit.
                return run_installer_elevated(path, progress);
            }
            Err(e) => return Err(format!("Could not start installer: {e}")),
        };

        if !status.success() {
            let code = status.code().unwrap_or(-1);
            return Err(format!(
                "Installer exited with code {code}. RusticDL may be partially updated."
            ));
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let status = Command::new(path)
            .status()
            .map_err(|e| format!("Could not start installer: {e}"))?;
        if !status.success() {
            return Err(format!("Installer exited with code {:?}.", status.code()));
        }
        Ok(())
    }
}

/// Run the NSIS setup elevated (UAC prompt) and wait for it to finish.
#[cfg(windows)]
fn run_installer_elevated(path: &Path, progress: &dyn ProgressSink) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, WAIT_FAILED, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    progress.set_status("Waiting for Administrator permission…".into());

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    let file = wide(path.as_os_str());
    let params = wide(std::ffi::OsStr::new("/S"));
    let verb = wide(std::ffi::OsStr::new("runas"));

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0 as i32,
        ..Default::default()
    };

    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok.is_err() {
        let err = std::io::Error::last_os_error();
        return Err(format!(
            "Could not start installer (elevation required): {err}\n\n\
Accept the Windows security prompt, or install the update manually from the release page."
        ));
    }

    if info.hProcess.is_invalid() {
        return Err(
            "Installer started but no process handle was returned; update status is unknown."
                .into(),
        );
    }

    progress.set_status("Installing update…".into());
    let wait = unsafe { WaitForSingleObject(info.hProcess, INFINITE) };
    unsafe {
        let _ = CloseHandle(info.hProcess);
    }

    if wait == WAIT_FAILED {
        return Err(format!(
            "Could not wait for installer: {}",
            std::io::Error::last_os_error()
        ));
    }
    if wait != WAIT_OBJECT_0 {
        return Err(format!(
            "Installer wait ended unexpectedly (status {}).",
            wait.0
        ));
    }
    Ok(())
}

/// Launch the main application after a successful update.
pub fn relaunch_app(app_exe: &Path) -> Result<(), String> {
    if !app_exe.is_file() {
        // Fresh install path might still be settling; brief retry.
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(200));
            if app_exe.is_file() {
                break;
            }
        }
    }
    if !app_exe.is_file() {
        return Err(format!(
            "Updated app not found at:\n{}\n\nLaunch RusticDL from the Start Menu.",
            app_exe.display()
        ));
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        Command::new(app_exe)
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(|e| format!("Could not start RusticDL: {e}"))?;
        Ok(())
    }

    #[cfg(not(windows))]
    {
        Command::new(app_exe)
            .spawn()
            .map_err(|e| format!("Could not start RusticDL: {e}"))?;
        Ok(())
    }
}
