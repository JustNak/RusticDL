//! Auto-updater backed by GitHub Releases.
//!
//! Always targets the public latest release of this repository:
//! `https://github.com/JustNak/RusticDL/releases/latest`
//!
//! Flow (one click in the app):
//! 1. Query the GitHub Releases API for the latest tag + assets.
//! 2. Compare against the built-in app version.
//! 3. If newer, flush app state, spawn **RusticDL Updater** with the setup
//!    download URL, then quit. The updater shows progress, runs NSIS `/S`,
//!    and relaunches this app.

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

/// Result of comparing the running build to GitHub's latest release.
#[derive(Debug, Clone)]
pub enum UpdateCheck {
    /// Installed version is already the latest (or newer, e.g. dev builds).
    UpToDate { current: String, latest: String },
    /// A newer release is available.
    Available(UpdateInfo),
}

/// Metadata for an installable update.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub release_name: String,
    pub html_url: String,
    /// Truncated release body from GitHub (optional UI copy).
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

    let notes = release
        .body
        .as_ref()
        .map(|b| b.trim())
        .filter(|b| !b.is_empty())
        .map(|b| truncate_notes(b, 600));

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

/// Spawn the updater, which downloads/installs the update after this process exits.
///
/// Callers must flush app state, then quit promptly so the updater can replace files.
pub fn launch_updater(opts: &LaunchUpdaterOpts) -> Result<(), String> {
    let updater = updater_exe_path()?;
    let app_exe =
        std::env::current_exe().map_err(|e| format!("Could not resolve app path: {e}"))?;
    let pid = std::process::id();

    let mut cmd = std::process::Command::new(&updater);
    cmd.arg("--app-exe")
        .arg(&app_exe)
        .arg("--download-url")
        .arg(&opts.download_url)
        .arg("--wait-pid")
        .arg(pid.to_string())
        .arg("--from-version")
        .arg(&opts.from_version)
        .arg("--to-version")
        .arg(&opts.to_version)
        .arg("--release-page")
        .arg(&opts.release_page);
    if let Some(size) = opts.setup_size {
        cmd.arg("--expected-size").arg(size.to_string());
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS so the updater outlives us when we quit for the update.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(|e| format!("Could not start {UPDATER_NAME}: {e}"))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        cmd.spawn()
            .map_err(|e| format!("Could not start {UPDATER_NAME}: {e}"))?;
        Ok(())
    }
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
