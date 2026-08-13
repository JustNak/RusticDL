use percent_encoding::percent_decode_str;
use std::path::{Path, PathBuf};
use tokio::fs;

use super::job::Job;

pub async fn ensure_parent_directory(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Download path has no parent directory.".to_string())?;

    fs::create_dir_all(parent)
        .await
        .map_err(|error| format!("Could not create download directory: {error}"))
}

pub async fn metadata_len(path: &Path) -> Option<u64> {
    fs::metadata(path).await.ok().map(|metadata| metadata.len())
}

/// Result of aligning job progress with on-disk partial state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileResult {
    /// Authoritative byte offset for single-stream Range resume.
    pub on_disk: u64,
    /// True when `job.downloaded_bytes` / progress were updated.
    pub changed: bool,
}

/// Align `job.downloaded_bytes` (and progress) with a known on-disk length.
///
/// Split from the async I/O so callers can `metadata_len` without holding locks.
pub fn apply_partial_progress_from_disk(job: &mut Job, on_disk: u64) -> ReconcileResult {
    let mut changed = job.downloaded_bytes != on_disk;
    if changed {
        job.downloaded_bytes = on_disk;
    }
    // Repair stale progress even when bytes already match (corrupt/partial state.json).
    if job.total_bytes > 0 {
        let expected = ((on_disk as f64 / job.total_bytes as f64) * 100.0).clamp(0.0, 100.0);
        if (job.progress - expected).abs() > 1e-9 {
            job.progress = expected;
            changed = true;
        }
    }
    ReconcileResult { on_disk, changed }
}

/// Align `job.downloaded_bytes` (and progress) with the contiguous `.part` length.
///
/// **PR 2 scope:** single-stream / legacy only (`metadata_len`).
/// Engine uses `metadata_len` + `apply_partial_progress_from_disk` so the mutex
/// is not held across filesystem I/O; this convenience wrapper remains for tests
/// and call sites that already own the job exclusively.
///
/// Future branches (not yet — Job lacks these fields in PR 2):
/// - **PR 4:** if `transfer_format_version >= 1`, skip `metadata_len` (version gate).
/// - **PR 8:** if `segment_map.is_some()`, `downloaded_bytes = sum(written)` only.
#[allow(dead_code)] // public API; engine prefers lock-split path
pub async fn reconcile_partial_progress(job: &mut Job) -> ReconcileResult {
    // PR4: if job.transfer_format_version >= 1 { /* no metadata_len */ }
    // PR8: if job.segment_map.is_some() { /* sum(segment.written); return */ }

    let on_disk = metadata_len(&job.temp_path).await.unwrap_or(0);
    apply_partial_progress_from_disk(job, on_disk)
}

pub async fn move_to_final_path(temp_path: &Path, target_path: &Path) -> Result<PathBuf, String> {
    let final_path = allocate_final_path(target_path).await?;

    fs::rename(temp_path, &final_path)
        .await
        .map_err(|error| format!("Could not finalize downloaded file: {error}"))?;

    Ok(final_path)
}

pub async fn allocate_final_path(target_path: &Path) -> Result<PathBuf, String> {
    if !target_path.exists() {
        return Ok(target_path.to_path_buf());
    }

    let stem = target_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = target_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let parent = target_path
        .parent()
        .ok_or_else(|| "Download path has no parent directory.".to_string())?;

    for index in 1..10_000 {
        let candidate = parent.join(format!("{stem} ({index}){extension}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err("Could not allocate a unique final download path.".into())
}

pub async fn remove_partial(path: &Path) {
    let _ = fs::remove_file(path).await;
}

pub fn parse_content_disposition_filename(header_value: &str) -> Option<String> {
    if let Some(encoded) = header_value
        .split(';')
        .find_map(|segment| segment.trim().strip_prefix("filename*="))
    {
        let sanitized = decode_content_disposition_filename(encoded);
        if !sanitized.is_empty() {
            return Some(sanitized);
        }
    }

    header_value
        .split(';')
        .find_map(|segment| segment.trim().strip_prefix("filename="))
        .map(decode_content_disposition_filename)
        .filter(|value| !value.is_empty())
}

pub fn decode_content_disposition_filename(value: &str) -> String {
    let value = value.trim().trim_matches('"').trim();
    let encoded = value.split("''").nth(1).unwrap_or(value);
    let decoded = percent_decode_str(encoded).decode_utf8_lossy();
    sanitize_filename(decoded.trim())
}

pub fn derive_filename_from_url(raw_url: &str) -> Option<String> {
    let parsed = url::Url::parse(raw_url).ok()?;
    let candidate = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())?;
    let decoded = percent_decode_str(candidate).decode_utf8_lossy();
    let sanitized = sanitize_filename(&decoded);
    if sanitized.is_empty() || sanitized == "download.bin" {
        None
    } else {
        Some(sanitized)
    }
}

pub fn sanitize_filename(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            character if character.is_control() => '_',
            _ => character,
        })
        .collect();

    let mut sanitized = sanitized.trim().trim_matches('.').trim().to_string();
    if is_windows_reserved_filename(&sanitized) {
        sanitized.push('_');
    }
    if sanitized.is_empty() {
        sanitized = "download.bin".into();
    }
    sanitized
}

pub fn is_windows_reserved_filename(filename: &str) -> bool {
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(filename)
        .to_ascii_uppercase();

    matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

pub fn temp_path_for(target: &Path) -> PathBuf {
    let mut path = target.as_os_str().to_os_string();
    path.push(".part");
    PathBuf::from(path)
}

/// Pick a unique target filename within `directory`, avoiding collisions with
/// existing jobs and on-disk final/partial files.
pub fn allocate_unique_download_paths(
    directory: &Path,
    preferred_name: &str,
    occupied_targets: &[PathBuf],
    occupied_temps: &[PathBuf],
) -> (String, PathBuf, PathBuf) {
    let preferred = sanitize_filename(preferred_name);
    let preferred = if preferred.is_empty() {
        "download.bin".into()
    } else {
        preferred
    };

    let stem = Path::new(&preferred)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download")
        .to_string();
    let extension = Path::new(&preferred)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();

    for index in 0..10_000 {
        let name = if index == 0 {
            preferred.clone()
        } else {
            format!("{stem} ({index}){extension}")
        };
        let target = directory.join(&name);
        let temp = temp_path_for(&target);
        let taken = occupied_targets.iter().any(|path| path == &target)
            || occupied_temps.iter().any(|path| path == &temp)
            || target.exists()
            || temp.exists();
        if !taken {
            return (name, target, temp);
        }
    }

    // Extremely unlikely; fall back to a uuid-suffixed name.
    let name = format!("{stem}-{}.part-fallback{extension}", uuid::Uuid::new_v4());
    let target = directory.join(&name);
    let temp = temp_path_for(&target);
    (name, target, temp)
}

pub fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.trim();
    let range_and_total = value.strip_prefix("bytes ")?;
    let (range, total) = range_and_total.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_unsafe_names() {
        assert_eq!(sanitize_filename("a/b\\c:d?.zip"), "a_b_c_d_.zip");
    }

    #[test]
    fn parses_content_disposition() {
        let name =
            parse_content_disposition_filename("attachment; filename=\"report (final).pdf\"")
                .unwrap();
        assert_eq!(name, "report (final).pdf");
    }

    #[test]
    fn derives_filename_from_url() {
        let name = derive_filename_from_url("https://cdn.example.com/files/hello%20world.iso?x=1")
            .unwrap();
        assert_eq!(name, "hello world.iso");
    }

    #[test]
    fn parses_content_range_header() {
        let (start, end, total) = parse_content_range("bytes 100-199/1000").unwrap();
        assert_eq!((start, end, total), (100, 199, 1000));
    }

    #[test]
    fn allocates_unique_download_paths() {
        let dir = std::env::temp_dir().join(format!("rusticdl-path-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (n1, t1, p1) = allocate_unique_download_paths(&dir, "file.zip", &[], &[]);
        assert_eq!(n1, "file.zip");
        let (n2, t2, p2) =
            allocate_unique_download_paths(&dir, "file.zip", &[t1.clone()], &[p1.clone()]);
        assert_eq!(n2, "file (1).zip");
        assert_ne!(t1, t2);
        assert_ne!(p1, p2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reconciles_downloaded_bytes_from_temp_file_length() {
        let dir =
            std::env::temp_dir().join(format!("rusticdl-reconcile-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("payload.bin");
        let temp = temp_path_for(&target);
        let on_disk_len = 1234u64;
        std::fs::write(&temp, vec![0u8; on_disk_len as usize]).unwrap();

        let mut job = Job::new(
            "https://example.com/payload.bin".into(),
            "payload.bin".into(),
            target,
            temp.clone(),
        );
        // Stale UI/state counters (e.g. after crash while .part grew).
        job.downloaded_bytes = 100;
        job.total_bytes = 5000;
        job.progress = 2.0;

        let result = reconcile_partial_progress(&mut job).await;
        assert_eq!(result.on_disk, on_disk_len);
        assert!(result.changed);
        assert_eq!(job.downloaded_bytes, on_disk_len);
        let expected_progress = (on_disk_len as f64 / 5000.0) * 100.0;
        assert!((job.progress - expected_progress).abs() < 1e-9);

        // Idempotent when already aligned.
        let again = reconcile_partial_progress(&mut job).await;
        assert_eq!(again.on_disk, on_disk_len);
        assert!(!again.changed);

        // Missing .part → zero bytes.
        std::fs::remove_file(&temp).unwrap();
        let missing = reconcile_partial_progress(&mut job).await;
        assert_eq!(missing.on_disk, 0);
        assert!(missing.changed);
        assert_eq!(job.downloaded_bytes, 0);
        assert_eq!(job.progress, 0.0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reconcile_without_total_leaves_progress_unchanged() {
        let dir = std::env::temp_dir().join(format!(
            "rusticdl-reconcile-nototal-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("unknown.bin");
        let temp = temp_path_for(&target);
        std::fs::write(&temp, vec![1u8; 50]).unwrap();

        let mut job = Job::new(
            "https://example.com/unknown.bin".into(),
            "unknown.bin".into(),
            target,
            temp,
        );
        job.downloaded_bytes = 0;
        job.total_bytes = 0;
        job.progress = 12.5;

        let result = reconcile_partial_progress(&mut job).await;
        assert!(result.changed);
        assert_eq!(job.downloaded_bytes, 50);
        // No total → leave progress as-is (unknown size).
        assert_eq!(job.progress, 12.5);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reconciles_downward_when_counters_ahead_of_disk() {
        // Crash-relevant: progress tick / state.json ahead of durable .part length.
        let dir =
            std::env::temp_dir().join(format!("rusticdl-reconcile-down-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("payload.bin");
        let temp = temp_path_for(&target);
        std::fs::write(&temp, vec![0u8; 50]).unwrap();

        let mut job = Job::new(
            "https://example.com/payload.bin".into(),
            "payload.bin".into(),
            target,
            temp,
        );
        job.downloaded_bytes = 900;
        job.total_bytes = 1000;
        job.progress = 90.0;

        let result = reconcile_partial_progress(&mut job).await;
        assert_eq!(result.on_disk, 50);
        assert!(result.changed);
        assert_eq!(job.downloaded_bytes, 50);
        assert!((job.progress - 5.0).abs() < 1e-9);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repairs_progress_when_bytes_match_but_percent_stale() {
        let mut job = Job::new(
            "https://example.com/x.bin".into(),
            "x.bin".into(),
            PathBuf::from("x.bin"),
            PathBuf::from("x.bin.part"),
        );
        job.downloaded_bytes = 250;
        job.total_bytes = 1000;
        job.progress = 99.0; // corrupt relative to bytes

        let result = apply_partial_progress_from_disk(&mut job, 250);
        assert!(result.changed);
        assert_eq!(job.downloaded_bytes, 250);
        assert!((job.progress - 25.0).abs() < 1e-9);
    }
}
