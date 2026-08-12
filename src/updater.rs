//! Auto-updater backed by GitHub Releases.
//!
//! Always targets the public latest release of this repository:
//! `https://github.com/JustNak/RusticDL/releases/latest`
//!
//! Staged flow (main app UI in `update_flow`):
//! 1. Query the GitHub Releases API for the latest tag + assets.
//! 2. Compare against the built-in app version and surface toast stages:
//!    Checking → You're up to date | Update available [Update] → Restart [Restart].
//! 3. On Restart, flush app state, spawn **RusticDL Updater** with the setup
//!    download URL, then quit. The updater shows progress, runs NSIS `/S`, and
//!    relaunches this app.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::branding::{
    APP_NAME, APP_VERSION, GITHUB_OWNER, GITHUB_REPO, SETUP_ASSET_NAME, UPDATER_EXE_NAME,
    UPDATER_NAME,
};

/// GitHub API: latest release for this project.
pub fn latest_release_api() -> String {
    format!("https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest")
}

/// Human-facing releases page (opens in browser).
pub fn latest_release_page() -> String {
    format!("https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest")
}
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_TIMEOUT: Duration = Duration::from_secs(120);
/// Max release-body characters retained for update UI / post-update What’s new.
const NOTES_MAX_CHARS: usize = 4_000;

/// Result of comparing the running build to GitHub's latest release.
#[derive(Debug, Clone)]
pub enum UpdateCheck {
    /// Installed version is already the latest (or newer, e.g. dev builds).
    UpToDate {
        #[allow(dead_code)]
        current: String,
        #[allow(dead_code)]
        latest: String,
    },
    /// A newer release is available.
    Available(UpdateInfo),
}

/// Metadata for an installable update.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    /// GitHub release title (reserved for richer update UI).
    #[allow(dead_code)]
    pub release_name: String,
    pub html_url: String,
    /// Truncated release body from GitHub (reserved for richer update UI).
    #[allow(dead_code)]
    pub notes: Option<String>,
    pub setup_download_url: String,
    pub setup_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    name: Option<String>,
    html_url: String,
    body: Option<String>,
    assets: Vec<GhAsset>,
    draft: bool,
    prerelease: bool,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

/// Query GitHub for the latest release and compare to this build.
pub async fn check_for_update() -> Result<UpdateCheck, String> {
    let client = github_client()?;
    let response = client
        .get(latest_release_api())
        .send()
        .await
        .map_err(|e| format!("Could not reach GitHub: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let snippet = body.chars().take(160).collect::<String>();
        return Err(format!(
            "GitHub returned {status} while checking for updates. {snippet}"
        ));
    }

    let release: GhRelease = response
        .json()
        .await
        .map_err(|e| format!("Could not parse GitHub release response: {e}"))?;

    if release.draft {
        return Err("Latest GitHub release is still a draft.".into());
    }

    let latest_raw = release.tag_name.trim();
    let latest = normalize_version(latest_raw);
    let current = normalize_version(APP_VERSION);

    if !is_newer(&latest, &current) {
        // Still surface prerelease tags that aren't "newer" numerically as up-to-date.
        let _ = release.prerelease;
        return Ok(UpdateCheck::UpToDate {
            current: current.clone(),
            latest,
        });
    }

    let asset = release
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(SETUP_ASSET_NAME))
        .ok_or_else(|| {
            format!(
                "Latest release (v{latest}) has no “{SETUP_ASSET_NAME}” asset. Open the release page instead."
            )
        })?;

    // Keep enough body for the post-update What’s new dialog; the pre-install
    // consent dialog applies its own shorter truncation when rendering.
    let notes = release
        .body
        .as_ref()
        .map(|b| b.trim())
        .filter(|b| !b.is_empty())
        .map(|b| truncate_notes(b, NOTES_MAX_CHARS));

    Ok(UpdateCheck::Available(UpdateInfo {
        current_version: current,
        latest_version: latest,
        release_name: release
            .name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("{APP_NAME} {latest_raw}")),
        html_url: release.html_url,
        notes,
        setup_download_url: asset.browser_download_url.clone(),
        setup_size: Some(asset.size),
    }))
}

/// Download the NSIS installer to a temp path (does not launch it).
///
/// Interactive updates now hand this off to **RusticDL Updater**. Kept for
/// tooling / fallback paths that want an in-process download.
#[allow(dead_code)]
pub async fn download_installer(download_url: &str) -> Result<PathBuf, String> {
    let client = github_client()?;
    let response = client
        .get(download_url)
        .header(ACCEPT, "application/octet-stream")
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("Download failed with HTTP {}.", response.status()));
    }

    let temp_dir = std::env::temp_dir().join("rusticdl-update");
    tokio::fs::create_dir_all(&temp_dir)
        .await
        .map_err(|e| format!("Could not create temp folder: {e}"))?;

    let installer_path = temp_dir.join(SETUP_ASSET_NAME);
    // Replace any previous partial download.
    let _ = tokio::fs::remove_file(&installer_path).await;

    let mut file = tokio::fs::File::create(&installer_path)
        .await
        .map_err(|e| format!("Could not create installer file: {e}"))?;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download interrupted: {e}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Could not write installer: {e}"))?;
    }
    file.flush()
        .await
        .map_err(|e| format!("Could not finalize installer: {e}"))?;
    drop(file);

    Ok(installer_path)
}

/// Open the latest release page in the default browser.
pub fn open_release_page() -> Result<(), String> {
    open_url(&latest_release_page())
}

/// Open a URL (release page or similar) in the default browser.
pub fn open_url(url: &str) -> Result<(), String> {
    open::that(url).map_err(|e| format!("Could not open browser: {e}"))
}

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

fn github_client() -> Result<reqwest::Client, String> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&format!("{APP_NAME}/{APP_VERSION}"))
            .unwrap_or_else(|_| HeaderValue::from_static("RusticDL")),
    );
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(
        "X-GitHub-Api-Version",
        HeaderValue::from_static("2022-11-28"),
    );

    reqwest::Client::builder()
        .default_headers(headers)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| format!("Could not create HTTP client: {e}"))
}

/// Strip a leading `v` / `V` and surrounding whitespace.
pub fn normalize_version(raw: &str) -> String {
    let s = raw.trim();
    let s = s
        .strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s);
    s.trim().to_string()
}

/// True when `latest` is a greater semver-like triple than `current`.
///
/// Accepts optional pre-release suffix (`1.2.3-beta`); pre-release of the same
/// core version is treated as older than the plain release.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_semverish(latest), parse_semverish(current)) {
        (Some(l), Some(c)) => l > c,
        _ => latest != current && !latest.is_empty(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Semverish {
    major: u64,
    minor: u64,
    patch: u64,
    /// 1 = plain release, 0 = pre-release (so 1.0.0 > 1.0.0-beta).
    release_rank: u8,
}

fn parse_semverish(s: &str) -> Option<Semverish> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (core, pre) = match s.split_once(['-', '+']) {
        Some((core, rest)) => (core, Some(rest)),
        None => (s, None),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    // Extra numeric segments ignored.
    let release_rank = if pre.is_some_and(|p| !p.is_empty()) {
        0
    } else {
        1
    };
    Some(Semverish {
        major,
        minor,
        patch,
        release_rank,
    })
}

fn truncate_notes(notes: &str, max_chars: usize) -> String {
    let trimmed = notes.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_v_prefix() {
        assert_eq!(normalize_version("v0.1.1"), "0.1.1");
        assert_eq!(normalize_version("V1.2.3"), "1.2.3");
        assert_eq!(normalize_version(" 0.2.0 "), "0.2.0");
    }

    #[test]
    fn is_newer_compares_triples() {
        assert!(is_newer("0.2.0", "0.1.1"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.1", "0.1.1"));
        assert!(!is_newer("0.1.0", "0.1.1"));
        assert!(is_newer("0.1.1", "0.1.1-beta"));
        assert!(!is_newer("0.1.1-beta", "0.1.1"));
    }

    #[test]
    fn endpoints_point_at_github() {
        let api = latest_release_api();
        let page = latest_release_page();
        assert!(api.contains(GITHUB_OWNER));
        assert!(api.contains(GITHUB_REPO));
        assert!(api.ends_with("/releases/latest"));
        assert!(page.contains("github.com"));
        assert!(page.contains(GITHUB_REPO));
        assert!(SETUP_ASSET_NAME.ends_with(".exe"));
        assert!(!APP_VERSION.is_empty());
    }
}
