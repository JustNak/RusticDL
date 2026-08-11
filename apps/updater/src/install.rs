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
        let status = Command::new(path)
            .args(["/S"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|e| format!("Could not start installer: {e}"))?;

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
