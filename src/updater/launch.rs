use std::path::PathBuf;

use crate::branding::{UPDATER_EXE_NAME, UPDATER_NAME};

/// Launch a previously downloaded NSIS setup binary.
///
/// Prefer [`launch_updater`] for interactive updates so the user sees a progress
/// window. This remains available for repair/fallback tooling.
///
/// When `silent_relaunch` is true, starts with `/S /R` (no wizard; app relaunches
/// after success). Prefer flushing jobs/settings before calling this, then quit
/// promptly so the installer can replace in-use files.
#[allow(dead_code)]
pub fn launch_installer(path: &std::path::Path, silent_relaunch: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS so the installer outlives us when we quit for the update.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        let mut cmd = std::process::Command::new(path);
        // cargo-packager NSIS: /S = silent, /R = relaunch app after success.
        if silent_relaunch {
            cmd.args(["/S", "/R"]);
        }
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(|e| format!("Could not start installer: {e}"))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = silent_relaunch;
        std::process::Command::new(path)
            .spawn()
            .map_err(|e| format!("Could not start installer: {e}"))?;
        Ok(())
    }
}

/// Arguments for spawning the dedicated **RusticDL Updater** process.
#[derive(Debug, Clone)]
pub struct LaunchUpdaterOpts {
    pub download_url: String,
    pub from_version: String,
    pub to_version: String,
    pub release_page: String,
    pub setup_size: Option<u64>,
}

/// Resolve `rusticdl-updater.exe` next to the running main executable.
pub fn updater_exe_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Could not resolve app path: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "Could not resolve install directory.".to_string())?;
    let updater = dir.join(UPDATER_EXE_NAME);
    if !updater.is_file() {
        return Err(format!(
            "{UPDATER_NAME} was not found next to the app:\n{}\n\nReinstall RusticDL or rebuild with the updater package.",
            updater.display()
        ));
    }
    Ok(updater)
}

/// Copy the install-dir updater to a temp path before spawn.
///
/// NSIS overwrites `$INSTDIR\rusticdl-updater.exe` during silent update. If the
/// helper is still running from that path, Windows refuses the write and the
/// install fails (or leaves a stale helper). Running from `%TEMP%` avoids that.
fn stage_updater_exe(installed: &std::path::Path) -> Result<PathBuf, String> {
    let temp_dir = std::env::temp_dir().join("rusticdl-update");
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| format!("Could not create updater temp folder: {e}"))?;
    let staged = temp_dir.join(UPDATER_EXE_NAME);
    std::fs::copy(installed, &staged).map_err(|e| {
        format!(
            "Could not stage {UPDATER_NAME} for update:\n{}\n→ {}\n{e}",
            installed.display(),
            staged.display()
        )
    })?;
    Ok(staged)
}

/// Spawn the updater, which downloads/installs the update after this process exits.
///
/// Callers must flush app state, then quit promptly so the updater can replace files.
pub fn launch_updater(opts: &LaunchUpdaterOpts) -> Result<(), String> {
    let installed = updater_exe_path()?;
    let updater = stage_updater_exe(&installed)?;
    let app_exe =
        std::env::current_exe().map_err(|e| format!("Could not resolve app path: {e}"))?;
    let pid = std::process::id();

    let mut args: Vec<String> = vec![
        "--app-exe".into(),
        app_exe.to_string_lossy().into_owned(),
        "--download-url".into(),
        opts.download_url.clone(),
        "--wait-pid".into(),
        pid.to_string(),
        "--from-version".into(),
        opts.from_version.clone(),
        "--to-version".into(),
        opts.to_version.clone(),
        "--release-page".into(),
        opts.release_page.clone(),
    ];
    if let Some(size) = opts.setup_size {
        args.push("--expected-size".into());
        args.push(size.to_string());
    }

    #[cfg(windows)]
    {
        spawn_detached_windows(&updater, &args)
            .map_err(|e| format!("Could not start {UPDATER_NAME}: {e}"))
    }
    #[cfg(not(windows))]
    {
        let mut cmd = std::process::Command::new(&updater);
        cmd.args(&args);
        cmd.spawn()
            .map_err(|e| format!("Could not start {UPDATER_NAME}: {e}"))?;
        Ok(())
    }
}

/// Detached spawn that survives app quit, with UAC-safe fallback.
///
/// `CreateProcess` cannot elevate. If the target still needs elevation (missing
/// asInvoker manifest, AppCompat "Run as administrator", etc.), Windows returns
/// ERROR_ELEVATION_REQUIRED (740). Retry via `ShellExecuteEx`, which can show UAC.
#[cfg(windows)]
fn spawn_detached_windows(exe: &std::path::Path, args: &[String]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    // DETACHED_PROCESS: outlive the parent when it quits for the update.
    // CREATE_NEW_PROCESS_GROUP: independent console/signal group.
    // CREATE_BREAKAWAY_FROM_JOB: leave the parent's job so KILL_ON_JOB_CLOSE
    // does not tear the updater down when the main app exits (best-effort; the
    // job must allow breakaway).
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const ERROR_ELEVATION_REQUIRED: i32 = 740;

    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    match cmd
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
    {
        Ok(_) => Ok(()),
        Err(e) if e.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) => {
            // Fall back without breakaway first (some jobs disallow it), then ShellExecute.
            let mut retry = std::process::Command::new(exe);
            retry.args(args);
            match retry
                .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
                .spawn()
            {
                Ok(_) => Ok(()),
                Err(e2) if e2.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) => {
                    shell_execute_detached(exe, args)
                }
                Err(e2) => Err(e2.to_string()),
            }
        }
        Err(e) => {
            // Some restricted jobs reject CREATE_BREAKAWAY_FROM_JOB.
            let mut retry = std::process::Command::new(exe);
            retry.args(args);
            match retry
                .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
                .spawn()
            {
                Ok(_) => Ok(()),
                Err(e2) if e2.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) => {
                    shell_execute_detached(exe, args)
                }
                Err(e2) => Err(format!("{e}; retry: {e2}")),
            }
        }
    }
}

/// Launch via ShellExecuteEx so Windows can prompt for elevation when required.
#[cfg(windows)]
fn shell_execute_detached(exe: &std::path::Path, args: &[String]) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    let file = wide(exe.as_os_str());
    let params = {
        let mut joined = String::new();
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                joined.push(' ');
            }
            // Quote args with spaces; updater paths/URLs are already simple.
            if arg.chars().any(|c| c.is_whitespace()) {
                joined.push('"');
                joined.push_str(&arg.replace('"', "\\\""));
                joined.push('"');
            } else {
                joined.push_str(arg);
            }
        }
        wide(std::ffi::OsStr::new(&joined))
    };

    // Verb left null → "open". If the PE still requires elevation, Windows shows UAC.
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0 as i32,
        ..Default::default()
    };

    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok.is_err() {
        let err = std::io::Error::last_os_error();
        return Err(format!(
            "{err}\n\nIf Windows asks for Administrator permission, accept the prompt, or reinstall RusticDL with the latest setup."
        ));
    }

    // Detach immediately — do not wait for the update to finish.
    if !info.hProcess.is_invalid() {
        unsafe {
            let _ = CloseHandle(info.hProcess);
        }
    }
    Ok(())
}
