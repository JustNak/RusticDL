//! Download the NSIS setup binary for apply.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};

use crate::ui::ProgressSink;

const SETUP_ASSET_NAME: &str = "RusticDL-windows-x64-setup.exe";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_TIMEOUT: Duration = Duration::from_secs(300);

/// Download `url` to a temp path, reporting progress when size is known.
pub fn download_installer(
    url: &str,
    expected_size: Option<u64>,
    progress: &dyn ProgressSink,
) -> Result<PathBuf, String> {
    progress.set_status("Downloading update…".into());
    progress.set_progress_unknown();

    let client = http_client()?;
    let mut response = client
        .get(url)
        .header(ACCEPT, "application/octet-stream")
        .send()
        .map_err(|e| format!("Download failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("Download failed with HTTP {}.", response.status()));
    }

    let total = response
        .content_length()
        .or(expected_size)
        .filter(|&n| n > 0);

    let temp_dir = std::env::temp_dir().join("rusticdl-update");
    std::fs::create_dir_all(&temp_dir).map_err(|e| format!("Could not create temp folder: {e}"))?;

    let installer_path = temp_dir.join(SETUP_ASSET_NAME);
    let _ = std::fs::remove_file(&installer_path);

    let mut file = File::create(&installer_path)
        .map_err(|e| format!("Could not create installer file: {e}"))?;

    let mut buf = [0_u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    loop {
        let n = response
            .read(&mut buf)
            .map_err(|e| format!("Download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("Could not write installer: {e}"))?;
        downloaded = downloaded.saturating_add(n as u64);
        if let Some(total) = total {
            let pct = ((downloaded.min(total) as f64 / total as f64) * 100.0).round() as u32;
            progress.set_progress_percent(pct.min(100));
            progress.set_status(format!(
                "Downloading update… {} / {}",
                format_bytes(downloaded),
                format_bytes(total)
            ));
        } else {
            progress.set_status(format!("Downloading update… {}", format_bytes(downloaded)));
        }
    }
    file.flush()
        .map_err(|e| format!("Could not finalize installer: {e}"))?;
    drop(file);

    if !installer_path.is_file() {
        return Err("Download finished but installer file is missing.".into());
    }
    if let Some(total) = expected_size {
        if let Ok(meta) = std::fs::metadata(&installer_path) {
            if meta.len() != total {
                // Soft warning only: GitHub size can differ if CDN recompresses (rare).
                progress.set_status(format!(
                    "Downloaded {} (expected {})",
                    format_bytes(meta.len()),
                    format_bytes(total)
                )); // status is String
            }
        }
    }

    progress.set_progress_percent(100);
    Ok(installer_path)
}

/// Ensure a local installer path exists.
pub fn resolve_local_installer(path: &Path) -> Result<PathBuf, String> {
    if !path.is_file() {
        return Err(format!("Installer not found:\n{}", path.display()));
    }
    Ok(path.to_path_buf())
}

fn http_client() -> Result<reqwest::blocking::Client, String> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("RusticDL-Updater/0.2"));
    headers.insert(ACCEPT, HeaderValue::from_static("application/octet-stream"));

    reqwest::blocking::Client::builder()
        .default_headers(headers)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(READ_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| format!("Could not create HTTP client: {e}"))
}

fn format_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let n = n as f64;
    if n >= GB {
        format!("{:.2} GB", n / GB)
    } else if n >= MB {
        format!("{:.1} MB", n / MB)
    } else if n >= KB {
        format!("{:.0} KB", n / KB)
    } else {
        format!("{n:.0} B")
    }
}
