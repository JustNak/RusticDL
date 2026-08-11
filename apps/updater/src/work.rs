//! Update pipeline: wait → download/resolve → install → relaunch.

use crate::args::UpdaterArgs;
use crate::download::{download_installer, resolve_local_installer};
use crate::install::{relaunch_app, run_silent_installer};
use crate::process::{wait_for_process_exit, WaitError};
use crate::ui::ProgressSink;

use std::time::Duration;

#[derive(Debug)]
pub enum UpdateOutcome {
    Success,
    WaitTimeout,
    DownloadFailed(String),
    InstallFailed(String),
    RelaunchFailed(String),
}

impl UpdateOutcome {
    pub fn error_message(&self) -> Option<String> {
        match self {
            Self::Success => None,
            Self::WaitTimeout => Some(
                "RusticDL did not exit in time.\n\nClose RusticDL completely and try updating again."
                    .into(),
            ),
            Self::DownloadFailed(m)
            | Self::InstallFailed(m)
            | Self::RelaunchFailed(m) => Some(m.clone()),
        }
    }
}

/// Run the full update sequence, reporting progress to `progress`.
pub fn run_update(args: &UpdaterArgs, progress: &dyn ProgressSink) -> UpdateOutcome {
    if let Some(pid) = args.wait_pid {
        let timeout = Duration::from_secs(args.wait_timeout_secs.max(5));
        if let Err(WaitError::Timeout) = wait_for_process_exit(pid, timeout, progress) {
            return UpdateOutcome::WaitTimeout;
        }
    }

    let installer_path = if let Some(url) = &args.download_url {
        match download_installer(url, args.expected_size, progress) {
            Ok(path) => path,
            Err(e) => return UpdateOutcome::DownloadFailed(e),
        }
    } else if let Some(path) = &args.installer_path {
        progress.set_status("Preparing installer…".into());
        match resolve_local_installer(path) {
            Ok(path) => path,
            Err(e) => return UpdateOutcome::DownloadFailed(e),
        }
    } else {
        return UpdateOutcome::DownloadFailed(
            "No download URL or installer path was provided.".into(),
        );
    };

    if let Err(e) = run_silent_installer(&installer_path, progress) {
        return UpdateOutcome::InstallFailed(e);
    }

    progress.set_status("Starting RusticDL…".into());
    progress.set_progress_percent(100);

    // Small delay so the progress text is readable and files settle.
    std::thread::sleep(Duration::from_millis(350));

    if let Err(e) = relaunch_app(&args.app_exe) {
        return UpdateOutcome::RelaunchFailed(e);
    }

    UpdateOutcome::Success
}
