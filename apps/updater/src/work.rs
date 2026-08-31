//! Update pipeline: download/resolve → close rusticdl → install → relaunch.

use crate::args::UpdaterArgs;
use crate::download::{download_installer, resolve_local_installer};
use crate::install::{apply_update_package, relaunch_app};
use crate::process::{close_app_for_replace, WaitError};
use crate::ui::ProgressSink;

use std::path::{Path, PathBuf};
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

/// Ordered steps of a successful in-app update.
///
/// Close always happens after the installer is ready and before replace.
/// Relaunch is only after a successful install — never before overwrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePhase {
    Download,
    ResolveLocal,
    CloseApp,
    Install,
    Relaunch,
}

/// The one legal success path: prepare installer → close rusticdl → replace → open once.
pub fn update_success_phases(has_download_url: bool) -> &'static [UpdatePhase] {
    if has_download_url {
        &[
            UpdatePhase::Download,
            UpdatePhase::CloseApp,
            UpdatePhase::Install,
            UpdatePhase::Relaunch,
        ]
    } else {
        &[
            UpdatePhase::ResolveLocal,
            UpdatePhase::CloseApp,
            UpdatePhase::Install,
            UpdatePhase::Relaunch,
        ]
    }
}

trait UpdateDriver {
    fn download(
        &mut self,
        url: &str,
        expected_size: Option<u64>,
        expected_sha256: Option<&str>,
        progress: &dyn ProgressSink,
    ) -> Result<PathBuf, String>;
    fn resolve_local(
        &mut self,
        path: &Path,
        progress: &dyn ProgressSink,
    ) -> Result<PathBuf, String>;
    fn close_app(
        &mut self,
        wait_pid: Option<u32>,
        app_exe: &Path,
        timeout: Duration,
        progress: &dyn ProgressSink,
    ) -> Result<(), WaitError>;
    fn install(
        &mut self,
        path: &Path,
        app_exe: &Path,
        progress: &dyn ProgressSink,
    ) -> Result<(), String>;
    fn relaunch(&mut self, app_exe: &Path) -> Result<(), String>;
}

struct RealDriver;

impl UpdateDriver for RealDriver {
    fn download(
        &mut self,
        url: &str,
        expected_size: Option<u64>,
        expected_sha256: Option<&str>,
        progress: &dyn ProgressSink,
    ) -> Result<PathBuf, String> {
        download_installer(url, expected_size, expected_sha256, progress)
    }

    fn resolve_local(
        &mut self,
        path: &Path,
        progress: &dyn ProgressSink,
    ) -> Result<PathBuf, String> {
        progress.set_status("Preparing installer…".into());
        resolve_local_installer(path)
    }

    fn close_app(
        &mut self,
        wait_pid: Option<u32>,
        app_exe: &Path,
        timeout: Duration,
        progress: &dyn ProgressSink,
    ) -> Result<(), WaitError> {
        close_app_for_replace(wait_pid, app_exe, timeout, progress)
    }

    fn install(
        &mut self,
        path: &Path,
        app_exe: &Path,
        progress: &dyn ProgressSink,
    ) -> Result<(), String> {
        apply_update_package(path, app_exe, progress)
    }

    fn relaunch(&mut self, app_exe: &Path) -> Result<(), String> {
        std::thread::sleep(Duration::from_millis(350));
        relaunch_app(app_exe)
    }
}

pub fn run_update(args: &UpdaterArgs, progress: &dyn ProgressSink) -> UpdateOutcome {
    run_update_with(args, progress, &mut RealDriver)
}

fn run_update_with(
    args: &UpdaterArgs,
    progress: &dyn ProgressSink,
    driver: &mut dyn UpdateDriver,
) -> UpdateOutcome {
    let installer_path = if let Some(url) = &args.download_url {
        match driver.download(
            url,
            args.expected_size,
            args.expected_sha256.as_deref(),
            progress,
        ) {
            Ok(path) => path,
            Err(e) => return UpdateOutcome::DownloadFailed(e),
        }
    } else if let Some(path) = &args.installer_path {
        match driver.resolve_local(path, progress) {
            Ok(path) => path,
            Err(e) => return UpdateOutcome::DownloadFailed(e),
        }
    } else {
        return UpdateOutcome::DownloadFailed(
            "No download URL or installer path was provided.".into(),
        );
    };

    let timeout = Duration::from_secs(args.wait_timeout_secs.max(5));
    if let Err(WaitError::Timeout) =
        driver.close_app(args.wait_pid, &args.app_exe, timeout, progress)
    {
        return UpdateOutcome::WaitTimeout;
    }

    if let Err(e) = driver.install(&installer_path, &args.app_exe, progress) {
        return UpdateOutcome::InstallFailed(e);
    }

    progress.set_status("Starting RusticDL…".into());
    progress.set_progress_percent(100);

    if let Err(e) = driver.relaunch(&args.app_exe) {
        return UpdateOutcome::RelaunchFailed(e);
    }

    UpdateOutcome::Success
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::UpdaterArgs;
    use crate::ui::ProgressSink;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    struct NoopProgress;
    impl ProgressSink for NoopProgress {
        fn set_status(&self, _text: String) {}
        fn set_progress_percent(&self, _percent: u32) {}
        fn set_progress_unknown(&self) {}
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Download,
        ResolveLocal,
        CloseApp { wait_pid: Option<u32> },
        Install,
        Relaunch,
    }

    struct RecordingDriver {
        calls: Arc<Mutex<Vec<Call>>>,
        fail_close: bool,
        fail_install: bool,
    }

    impl RecordingDriver {
        fn new() -> (Self, Arc<Mutex<Vec<Call>>>) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    calls: Arc::clone(&calls),
                    fail_close: false,
                    fail_install: false,
                },
                calls,
            )
        }

        fn push(&self, call: Call) {
            self.calls.lock().unwrap().push(call);
        }
    }

    impl UpdateDriver for RecordingDriver {
        fn download(
            &mut self,
            _url: &str,
            _expected_size: Option<u64>,
            _expected_sha256: Option<&str>,
            _progress: &dyn ProgressSink,
        ) -> Result<PathBuf, String> {
            self.push(Call::Download);
            Ok(PathBuf::from("setup.exe"))
        }

        fn resolve_local(
            &mut self,
            path: &Path,
            _progress: &dyn ProgressSink,
        ) -> Result<PathBuf, String> {
            self.push(Call::ResolveLocal);
            Ok(path.to_path_buf())
        }

        fn close_app(
            &mut self,
            wait_pid: Option<u32>,
            _app_exe: &Path,
            _timeout: Duration,
            _progress: &dyn ProgressSink,
        ) -> Result<(), WaitError> {
            self.push(Call::CloseApp { wait_pid });
            if self.fail_close {
                Err(WaitError::Timeout)
            } else {
                Ok(())
            }
        }

        fn install(
            &mut self,
            _path: &Path,
            _app_exe: &Path,
            _progress: &dyn ProgressSink,
        ) -> Result<(), String> {
            self.push(Call::Install);
            if self.fail_install {
                Err("install failed".into())
            } else {
                Ok(())
            }
        }

        fn relaunch(&mut self, _app_exe: &Path) -> Result<(), String> {
            self.push(Call::Relaunch);
            Ok(())
        }
    }

    fn download_args() -> UpdaterArgs {
        UpdaterArgs::parse([
            "--app-exe",
            r"C:\Apps\RusticDL\rusticdl.exe",
            "--download-url",
            "https://example.com/setup.exe",
            "--wait-pid",
            "4242",
            "--wait-timeout-secs",
            "5",
        ])
        .unwrap()
    }

    fn local_args() -> UpdaterArgs {
        UpdaterArgs::parse([
            "--app-exe",
            r"C:\Apps\RusticDL\rusticdl.exe",
            "--installer-path",
            r"C:\Temp\setup.exe",
        ])
        .unwrap()
    }

    #[test]
    fn success_phases_close_before_replace_relaunch_once_after() {
        for phases in [update_success_phases(true), update_success_phases(false)] {
            let close = phases.iter().position(|p| *p == UpdatePhase::CloseApp);
            let install = phases.iter().position(|p| *p == UpdatePhase::Install);
            let relaunch = phases.iter().position(|p| *p == UpdatePhase::Relaunch);
            assert!(close.is_some() && install.is_some() && relaunch.is_some());
            assert!(close < install, "rusticdl must be closed before overwrite");
            assert!(
                install < relaunch,
                "must not start rusticdl until replace is done"
            );
            assert_eq!(
                phases
                    .iter()
                    .filter(|p| **p == UpdatePhase::Relaunch)
                    .count(),
                1
            );
            assert_eq!(*phases.last().unwrap(), UpdatePhase::Relaunch);
        }
    }

    #[test]
    fn run_update_closes_then_replaces_then_relaunches_once() {
        let args = download_args();
        let (mut driver, calls) = RecordingDriver::new();
        let outcome = run_update_with(&args, &NoopProgress, &mut driver);
        assert!(matches!(outcome, UpdateOutcome::Success));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                Call::Download,
                Call::CloseApp {
                    wait_pid: Some(4242)
                },
                Call::Install,
                Call::Relaunch,
            ]
        );
    }

    #[test]
    fn run_update_local_path_uses_same_close_replace_relaunch_order() {
        let args = local_args();
        let (mut driver, calls) = RecordingDriver::new();
        let outcome = run_update_with(&args, &NoopProgress, &mut driver);
        assert!(matches!(outcome, UpdateOutcome::Success));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                Call::ResolveLocal,
                Call::CloseApp { wait_pid: None },
                Call::Install,
                Call::Relaunch,
            ]
        );
    }

    #[test]
    fn run_update_does_not_install_or_relaunch_if_app_stays_open() {
        let args = download_args();
        let (mut driver, calls) = RecordingDriver::new();
        driver.fail_close = true;
        let outcome = run_update_with(&args, &NoopProgress, &mut driver);
        assert!(matches!(outcome, UpdateOutcome::WaitTimeout));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                Call::Download,
                Call::CloseApp {
                    wait_pid: Some(4242)
                },
            ]
        );
    }

    #[test]
    fn run_update_does_not_relaunch_if_install_fails() {
        let args = download_args();
        let (mut driver, calls) = RecordingDriver::new();
        driver.fail_install = true;
        let outcome = run_update_with(&args, &NoopProgress, &mut driver);
        assert!(matches!(outcome, UpdateOutcome::InstallFailed(_)));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                Call::Download,
                Call::CloseApp {
                    wait_pid: Some(4242)
                },
                Call::Install,
            ]
        );
    }
}
