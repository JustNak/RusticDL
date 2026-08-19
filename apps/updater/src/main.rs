//! RusticDL Updater — dedicated process that applies an update and relaunches the app.
//!
//! Invoked by the main app after the user clicks Update:
//! 1. Show a small progress window.
//! 2. Download the NSIS setup (or use a local path).
//! 3. Ask rusticdl to quit (or kill it) before files are overwritten.
//! 4. Run the installer silently (`/S`, no `/R` — we own relaunch).
//! 5. Start RusticDL once the replace/install has finished, then exit.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod args;
mod download;
mod install;
mod process;
mod ui;
mod work;

use args::UpdaterArgs;
use work::{run_update, UpdateOutcome};

const EXIT_OK: i32 = 0;
const EXIT_BAD_ARGS: i32 = 1;
const EXIT_WAIT_TIMEOUT: i32 = 2;
const EXIT_DOWNLOAD: i32 = 3;
const EXIT_INSTALL: i32 = 4;
const EXIT_RELAUNCH: i32 = 5;
const EXIT_ALREADY_RUNNING: i32 = 6;

fn main() {
    let args = match UpdaterArgs::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("RusticDL Updater: {message}");
            #[cfg(windows)]
            ui::show_error_message("RusticDL Updater", &message, None);
            std::process::exit(EXIT_BAD_ARGS);
        }
    };

    if !process::try_acquire_single_instance() {
        let message = "An update is already in progress.";
        eprintln!("RusticDL Updater: {message}");
        #[cfg(windows)]
        ui::show_error_message("RusticDL Updater", message, None);
        std::process::exit(EXIT_ALREADY_RUNNING);
    }

    let title = match &args.to_version {
        Some(v) if !v.is_empty() => format!("Updating RusticDL to v{v}…"),
        _ => "Updating RusticDL…".to_string(),
    };

    let outcome = ui::run_with_progress_window(&title, {
        let args = args.clone();
        move |progress| run_update(&args, progress)
    });

    let code = match outcome {
        UpdateOutcome::Success => EXIT_OK,
        UpdateOutcome::WaitTimeout => EXIT_WAIT_TIMEOUT,
        UpdateOutcome::DownloadFailed(_) => EXIT_DOWNLOAD,
        UpdateOutcome::InstallFailed(_) => EXIT_INSTALL,
        UpdateOutcome::RelaunchFailed(_) => EXIT_RELAUNCH,
    };

    if let Some(message) = outcome.error_message() {
        eprintln!("RusticDL Updater: {message}");
        #[cfg(windows)]
        {
            let release = args.release_page.as_deref();
            ui::show_error_message("RusticDL Updater", &message, release);
            // Best-effort: leave the user with a running app when the main
            // process already exited for the update. Skip WaitTimeout — the
            // original app is still alive in that case.
            if matches!(
                outcome,
                UpdateOutcome::DownloadFailed(_)
                    | UpdateOutcome::InstallFailed(_)
                    | UpdateOutcome::RelaunchFailed(_)
            ) {
                let _ = install::relaunch_app(&args.app_exe);
            }
        }
        #[cfg(not(windows))]
        {
            let _ = message;
        }
    }

    std::process::exit(code);
}
