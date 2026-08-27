use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use serde::Deserialize;

use super::version::{is_nightly_version, normalize_version, should_offer_on_channel};
use crate::branding::{APP_NAME, APP_VERSION, GITHUB_OWNER, GITHUB_REPO, SETUP_ASSET_NAME};
use crate::settings::UpdateChannel;

/// GitHub API: latest stable (non-prerelease) release for this project.
pub fn latest_release_api() -> String {
    format!("https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest")
}

/// GitHub API: recent releases list (used to find nightly builds).
pub fn releases_list_api() -> String {
    format!("https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPO}/releases?per_page=100")
}

/// Human-facing latest stable release page.
pub fn latest_release_page() -> String {
    format!("https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest")
}

/// Human-facing releases list (stable + nightly pre-releases).
pub fn releases_page() -> String {
    format!("https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}/releases")
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

/// Query GitHub for the latest release on `channel` and compare to this build.
pub async fn check_for_update(channel: UpdateChannel) -> Result<UpdateCheck, String> {
    let client = github_client()?;
    let release = match channel {
        UpdateChannel::Stable => fetch_stable_release(&client).await?,
        UpdateChannel::Nightly => fetch_nightly_release(&client).await?,
    };
    compare_release(release, channel)
}

async fn fetch_stable_release(client: &reqwest::Client) -> Result<GhRelease, String> {
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

    Ok(release)
}

async fn fetch_nightly_release(client: &reqwest::Client) -> Result<GhRelease, String> {
    let response = client
        .get(releases_list_api())
        .send()
        .await
        .map_err(|e| format!("Could not reach GitHub: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let snippet = body.chars().take(160).collect::<String>();
        return Err(format!(
            "GitHub returned {status} while checking for nightly updates. {snippet}"
        ));
    }

    let releases: Vec<GhRelease> = response
        .json()
        .await
        .map_err(|e| format!("Could not parse GitHub releases list: {e}"))?;

    // GitHub returns newest first; take the first published nightly with a setup asset.
    releases
        .into_iter()
        .find(is_published_nightly)
        .ok_or_else(|| "No Nightly build with a setup installer was found on GitHub.".into())
}

fn is_published_nightly(release: &GhRelease) -> bool {
    !release.draft
        && release.prerelease
        && is_nightly_version(&release.tag_name)
        && release
            .assets
            .iter()
            .any(|a| a.name.eq_ignore_ascii_case(SETUP_ASSET_NAME))
}

fn compare_release(release: GhRelease, channel: UpdateChannel) -> Result<UpdateCheck, String> {
    let latest_raw = release.tag_name.trim();
    let latest = normalize_version(latest_raw);
    let current = normalize_version(APP_VERSION);

    if !should_offer_on_channel(&latest, &current, channel) {
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
                "Release (v{latest}) has no “{SETUP_ASSET_NAME}” asset. Open the release page instead."
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

/// Open the releases list in the default browser (includes nightly pre-releases).
pub fn open_release_page() -> Result<(), String> {
    open_url(&releases_page())
}

/// Open a URL (release page or similar) in the default browser.
pub fn open_url(url: &str) -> Result<(), String> {
    open::that(url).map_err(|e| format!("Could not open browser: {e}"))
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
    fn endpoints_point_at_github() {
        let api = latest_release_api();
        let page = latest_release_page();
        let list = releases_page();
        assert!(api.contains(GITHUB_OWNER));
        assert!(api.contains(GITHUB_REPO));
        assert!(api.ends_with("/releases/latest"));
        assert!(page.contains("github.com"));
        assert!(page.contains(GITHUB_REPO));
        assert!(list.ends_with("/releases"));
        assert!(releases_list_api().contains("per_page=100"));
        assert!(SETUP_ASSET_NAME.ends_with(".exe"));
        assert!(!APP_VERSION.is_empty());
        assert_eq!(APP_VERSION, env!("CARGO_PKG_VERSION"));
    }
}
