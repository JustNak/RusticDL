//! Run the NSIS installer and relaunch the main app.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::ui::ProgressSink;

/// NSIS flags for an in-app update. `/S` only — never `/R`.
///
/// The updater owns the single post-success relaunch. Passing `/R` would start
/// rusticdl at the end of setup, then this helper would start it again.
pub fn installer_silent_args() -> &'static [&'static str] {
    &["/S"]
}

/// Run a downloaded NSIS setup silently. Updater owns relaunch, so no `/R`.
pub fn run_silent_installer(path: &Path, progress: &dyn ProgressSink) -> Result<(), String> {
    progress.set_status("Installing update…".into());
    progress.set_progress_unknown();

    if !path.is_file() {
        return Err(format!("Installer missing: {}", path.display()));
    }

    // Fail closed: Authenticode (WinVerifyTrust) before Command / ShellExecute.
    verify_installer_authenticode(path)?;

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const ERROR_ELEVATION_REQUIRED: i32 = 740;

        let status = match Command::new(path)
            .args(installer_silent_args())
            .creation_flags(CREATE_NO_WINDOW)
            .status()
        {
            Ok(status) => status,
            Err(e) if e.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) => {
                // Per-machine setups (or Installer Detection) need elevation.
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

/// Reject the installer unless Authenticode trust succeeds.
///
/// Windows: `WinVerifyTrust` with `WINTRUST_ACTION_GENERIC_VERIFY_V2`
/// (embedded signature present, PE digest matches, cert chains to a trusted
/// root, code-signing EKU). Not a content-length check and not a SHA-256
/// from GitHub. Non-Windows: fail closed; this helper never executes there.
fn verify_installer_authenticode(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        win_verify_trust_authenticode(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(
            "Installer rejected: Authenticode verification failed (WinVerifyTrust). \
The installer cannot be verified on this platform."
                .into(),
        )
    }
}

#[cfg(windows)]
fn win_verify_trust_authenticode(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::GUID;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Security::WinTrust::{
        WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
        WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE, WTD_REVOKE_NONE,
        WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    };

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: windows::core::PCWSTR(wide.as_ptr()),
        ..Default::default()
    };

    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: std::ptr::from_mut(&mut file_info),
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        ..Default::default()
    };

    let mut action: GUID = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe {
        WinVerifyTrust(
            HWND::default(),
            std::ptr::from_mut(&mut action),
            std::ptr::from_mut(&mut data).cast(),
        )
    };

    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        let _ = WinVerifyTrust(
            HWND::default(),
            std::ptr::from_mut(&mut action),
            std::ptr::from_mut(&mut data).cast(),
        );
    }

    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "Installer rejected: Authenticode verification failed (WinVerifyTrust status 0x{:08X}). \
The update is unsigned, tampered, or not trusted. Install manually from the release page.",
            status as u32
        ))
    }
}

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
    let params = wide(std::ffi::OsStr::new(&installer_silent_args().join(" ")));
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

pub fn relaunch_app(app_exe: &Path) -> Result<(), String> {
    if !app_exe.is_file() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::ProgressSink;

    struct NoopProgress;
    impl ProgressSink for NoopProgress {
        fn set_status(&self, _text: String) {}
        fn set_progress_percent(&self, _percent: u32) {}
        fn set_progress_unknown(&self) {}
    }

    #[test]
    fn silent_install_does_not_relaunch() {
        let args = installer_silent_args();
        assert_eq!(args, &["/S"]);
        assert!(
            args.iter().all(|a| *a != "/R" && !a.contains("/R")),
            "updater owns relaunch; NSIS must not start rusticdl"
        );
    }

    #[test]
    fn refuses_execute_without_authenticode() {
        let dir = std::env::temp_dir().join(format!(
            "rusticdl-updater-authenticode-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("unsigned-setup.exe");
        std::fs::write(&path, b"not-a-signed-pe\n").expect("unsigned payload");

        let err = run_silent_installer(&path, &NoopProgress).expect_err(
            "execute-without-Authenticode: unsigned installer must not reach Command / ShellExecute",
        );
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            err.contains("Authenticode") && err.contains("WinVerifyTrust"),
            "execute-without-Authenticode: fail-closed gate must reject via Authenticode/WinVerifyTrust before Command / ShellExecute, got {err:?}"
        );
        assert!(
            !err.contains("Could not start installer"),
            "execute-without-Authenticode: must fail closed before Command / ShellExecute, got {err:?}"
        );
    }
}
