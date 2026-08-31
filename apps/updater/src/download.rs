//! Download the NSIS setup binary for apply.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};

use crate::ui::ProgressSink;

#[cfg(windows)]
const SETUP_ASSET_NAME: &str = "RusticDL-windows-x64-setup.exe";
#[cfg(target_os = "linux")]
const SETUP_ASSET_NAME: &str = "RusticDL-linux-x64.tar.gz";
#[cfg(not(any(windows, target_os = "linux")))]
const SETUP_ASSET_NAME: &str = "RusticDL-update.bin";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_TIMEOUT: Duration = Duration::from_secs(300);

pub fn download_installer(
    url: &str,
    expected_size: Option<u64>,
    expected_sha256: Option<&str>,
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
                progress.set_status(format!(
                    "Downloaded {} (expected {})",
                    format_bytes(meta.len()),
                    format_bytes(total)
                )); // status is String
            }
        }
    }

    if cfg!(target_os = "linux") {
        let expected = expected_sha256
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                "Linux update is missing SHA-256. Refuse to install an unverified archive."
                    .to_string()
            })?;
        verify_sha256(&installer_path, expected)?;
    } else if let Some(expected) = expected_sha256
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        verify_sha256(&installer_path, expected)?;
    }

    progress.set_progress_percent(100);
    Ok(installer_path)
}

fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};

    let mut file =
        File::open(path).map_err(|e| format!("Could not read downloaded archive: {e}"))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("Could not hash downloaded archive: {e}"))?;
    let digest = hasher.finalize();
    let got: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if !got.eq_ignore_ascii_case(expected) {
        return Err(format!(
            "SHA-256 mismatch for {}.\nExpected {expected}\nGot      {got}\n\
The download may be corrupt. Install manually from the release page.",
            path.display()
        ));
    }
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_sha256_accepts_matching_digest() {
        let dir = std::env::temp_dir().join(format!(
            "rusticdl-sha256-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("payload.bin");
        std::fs::write(&path, b"rusticdl-sha256-fixture\n").expect("payload");
        verify_sha256(
            &path,
            "2f3c6d1c0b6a0c1d8b7f4a3e5c9d2a1b0e8f7c6d5a4b3c2d1e0f9a8b7c6d5e4f",
        )
        .expect_err("wrong hash must fail");
        let expected = {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(b"rusticdl-sha256-fixture\n");
            digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        verify_sha256(&path, &expected).expect("matching hash");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
