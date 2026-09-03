use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use serde::Deserialize;

use super::version::{is_nightly_version, normalize_version, should_offer_on_channel};
use crate::branding::{
    update_asset_name, APP_NAME, APP_VERSION, CHECKSUMS_ASSET_NAME, GITHUB_OWNER, GITHUB_REPO,
    LINUX_TARBALL_ASSET_NAME, SETUP_ASSET_NAME,
};
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
    /// SHA-256 of the Linux tarball from `SHA256SUMS` (Windows has no checksum gate).
    pub setup_sha256: Option<String>,
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
    compare_release(&client, release, channel).await
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

    releases
        .into_iter()
        .find(is_published_nightly)
        .ok_or_else(|| {
            format!(
                "No Nightly build with “{}” was found on GitHub.",
                update_asset_name()
            )
        })
}

fn is_published_nightly(release: &GhRelease) -> bool {
    !release.draft
        && release.prerelease
        && is_nightly_version(&release.tag_name)
        && release
            .assets
            .iter()
            .any(|a| a.name.eq_ignore_ascii_case(update_asset_name()))
}

async fn compare_release(
    client: &reqwest::Client,
    release: GhRelease,
    channel: UpdateChannel,
) -> Result<UpdateCheck, String> {
    let latest_raw = release.tag_name.trim();
    let latest = normalize_version(latest_raw);
    let current = normalize_version(APP_VERSION);

    if !should_offer_on_channel(&latest, &current, channel) {
        return Ok(UpdateCheck::UpToDate {
            current: current.clone(),
            latest,
        });
    }

    let asset_name = update_asset_name();
    let asset = release
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(asset_name))
        .ok_or_else(|| {
            format!(
                "Release (v{latest}) has no “{asset_name}” asset. Open the release page instead."
            )
        })?;

    let setup_sha256 = if cfg!(target_os = "linux") {
        Some(fetch_release_sha256(client, &release, asset_name).await?)
    } else {
        None
    };

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
        setup_sha256,
    }))
}

async fn fetch_release_sha256(
    client: &reqwest::Client,
    release: &GhRelease,
    asset_name: &str,
) -> Result<String, String> {
    let sums = release
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(CHECKSUMS_ASSET_NAME))
        .ok_or_else(|| {
            "Release has no SHA256SUMS asset. Cannot verify the Linux tarball.".to_string()
        })?;

    let response = client
        .get(&sums.browser_download_url)
        .send()
        .await
        .map_err(|e| format!("Could not download SHA256SUMS: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "GitHub returned {} while downloading SHA256SUMS.",
            response.status()
        ));
    }
    let text = response
        .text()
        .await
        .map_err(|e| format!("Could not read SHA256SUMS: {e}"))?;
    parse_sha256sums(&text, asset_name).ok_or_else(|| {
        format!(
            "SHA256SUMS has no entry for {asset_name}. Refuse to install an unverified archive."
        )
    })
}

/// Parse a GNU `sha256sum` listing and return the hash for `file_name`.
pub fn parse_sha256sums(text: &str, file_name: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
        if base.eq_ignore_ascii_case(file_name) {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// Open the releases list in the default browser (includes nightly pre-releases).
pub fn open_release_page() -> Result<(), String> {
    open_url(&releases_page())
}

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
        assert!(LINUX_TARBALL_ASSET_NAME.ends_with(".tar.gz"));
        assert_eq!(CHECKSUMS_ASSET_NAME, "SHA256SUMS");
        assert!(!APP_VERSION.is_empty());
        assert_eq!(APP_VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn parse_sha256sums_matches_basename() {
        let text = "\
# comment
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  RusticDL-linux-x64.tar.gz
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb *other.bin
";
        assert_eq!(
            parse_sha256sums(text, LINUX_TARBALL_ASSET_NAME).as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert!(parse_sha256sums(text, SETUP_ASSET_NAME).is_none());
    }
}
