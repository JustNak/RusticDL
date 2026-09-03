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

/// Apply a downloaded update package (NSIS on Windows, tarball on Linux).
pub fn apply_update_package(
    path: &Path,
    app_exe: &Path,
    progress: &dyn ProgressSink,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = app_exe;
        run_silent_installer(path, progress)
    }
    #[cfg(target_os = "linux")]
    {
        extract_linux_tarball(path, app_exe, progress)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (path, app_exe, progress);
        Err("In-app updates are not supported on this platform.".into())
    }
}

#[cfg(target_os = "linux")]
fn extract_linux_tarball(
    archive: &Path,
    app_exe: &Path,
    progress: &dyn ProgressSink,
) -> Result<(), String> {
    progress.set_status("Installing update…".into());
    progress.set_progress_unknown();

    if !archive.is_file() {
        return Err(format!("Update archive missing: {}", archive.display()));
    }

    let prefix = app_exe
        .parent()
        .ok_or_else(|| "Could not resolve install directory.".to_string())?;
    if !prefix.is_dir() {
        return Err(format!(
            "Install directory is missing:\n{}",
            prefix.display()
        ));
    }

    progress.set_status("Extracting update…".into());
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(prefix)
        .status()
        .map_err(|e| format!("Could not extract update (tar): {e}"))?;
    if !status.success() {
        return Err(format!(
            "tar exited with code {:?}. RusticDL may be partially updated.",
            status.code()
        ));
    }

    use std::os::unix::fs::PermissionsExt;
    for name in ["rusticdl", "rusticdl-native-host", "rusticdl-updater"] {
        let bin = prefix.join(name);
        if !bin.is_file() {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&bin) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o755);
            let _ = std::fs::set_permissions(&bin, perms);
        }
    }

    Ok(())
}

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
    fn unsigned_installer_reaches_command() {
        let dir = std::env::temp_dir().join(format!(
            "rusticdl-updater-unsigned-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("unsigned-setup.exe");
        std::fs::write(&path, b"not-a-signed-pe\n").expect("unsigned payload");

        let err = run_silent_installer(&path, &NoopProgress)
            .expect_err("dummy payload is not a real installer");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            !err.contains("Authenticode") && !err.contains("WinVerifyTrust"),
            "unsigned installer must not be rejected for missing Authenticode, got {err:?}"
        );
        assert!(
            err.contains("Could not start installer") || err.contains("Installer exited"),
            "unsigned dummy must reach Command and fail as a start/exit error, got {err:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn apply_update_package_extracts_tarball_into_app_dir() {
        let dir = std::env::temp_dir().join(format!(
            "rusticdl-linux-extract-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let prefix = dir.join("prefix");
        let src = dir.join("src");
        std::fs::create_dir_all(&prefix).expect("prefix");
        std::fs::create_dir_all(&src).expect("src");
        std::fs::write(src.join("rusticdl"), b"new-bin").expect("src rusticdl");
        std::fs::write(prefix.join("rusticdl"), b"old-bin").expect("old rusticdl");
        let archive = dir.join("RusticDL-linux-x64.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&src)
            .arg("rusticdl")
            .status()
            .expect("tar create");
        assert!(status.success(), "tar create failed");

        apply_update_package(&archive, &prefix.join("rusticdl"), &NoopProgress)
            .expect("extract tarball");
        assert_eq!(
            std::fs::read(prefix.join("rusticdl")).expect("read rusticdl"),
            b"new-bin"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
