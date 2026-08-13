use percent_encoding::percent_decode_str;
use std::path::{Path, PathBuf};
use tokio::fs;

use super::job::Job;

/// Safety margin beyond remaining download bytes before preallocate is allowed.
/// `max(64 MiB, 1% of total)`.
pub fn preallocate_margin(total_bytes: u64) -> u64 {
    (total_bytes / 100).max(64 * 1024 * 1024)
}

/// True when free space is enough to finish writing `remaining` bytes.
pub fn free_space_allows_write(free: u64, remaining: u64) -> bool {
    free > remaining
}

/// True when free space covers remaining bytes **plus** preallocate margin.
///
/// Two-tier free-space policy (see `segment_io::preallocate_decision`):
/// - `free <= remaining` → Disk error (cannot finish)
/// - `remaining < free <= remaining + margin` → multi without `set_len`
/// - `free > remaining + margin` → preallocate allowed
pub fn free_space_allows_preallocate(free: u64, remaining: u64, total_bytes: u64) -> bool {
    free > remaining.saturating_add(preallocate_margin(total_bytes))
}

/// Free bytes available on the volume containing `path` (caller-available).
///
/// Returns `None` when the platform API is unavailable or the call fails
/// (fail-open for preallocate: multi may extend-on-write).
pub async fn free_space_bytes(path: &Path) -> Option<u64> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || free_space_bytes_sync(&path))
        .await
        .ok()
        .flatten()
}

/// True when `path` is a UNC share root (`\\server\share` or `\\server\share\`).
#[cfg(windows)]
fn is_unc_share_root(path: &Path) -> bool {
    let raw = path.as_os_str().to_string_lossy().replace('/', "\\");
    let trimmed = raw.trim_end_matches('\\');
    let Some(rest) = trimmed.strip_prefix("\\\\") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('\\').filter(|p| !p.is_empty()).collect();
    parts.len() == 2
}

/// `GetDiskFreeSpaceExW` needs a directory; UNC share roots need a trailing `\`.
#[cfg(windows)]
fn disk_free_query_path(path: &Path) -> PathBuf {
    let mut query = path.to_path_buf();
    while !query.exists() {
        if is_unc_share_root(&query) {
            break;
        }
        match query.parent() {
            Some(parent) if !parent.as_os_str().is_empty() && parent != query.as_path() => {
                query = parent.to_path_buf();
            }
            _ => break,
        }
    }
    if query.is_file() {
        if let Some(parent) = query.parent().filter(|p| !p.as_os_str().is_empty()) {
            query = parent.to_path_buf();
        }
    }
    let raw = query.as_os_str().to_string_lossy();
    if (raw.starts_with("\\\\") || raw.starts_with("//"))
        && !raw.ends_with('\\')
        && !raw.ends_with('/')
    {
        let mut owned = query.into_os_string();
        owned.push("\\");
        PathBuf::from(owned)
    } else {
        query
    }
}

#[cfg(windows)]
fn free_space_bytes_sync(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let query = disk_free_query_path(path);

    let wide: Vec<u16> = query
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut free_available: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut total_free: u64 = 0;

    let result = unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide.as_ptr()),
            Some(&mut free_available as *mut u64),
            Some(&mut total_bytes as *mut u64),
            Some(&mut total_free as *mut u64),
        )
    };

    match result {
        Ok(()) => Some(free_available),
        Err(_) => None,
    }
}

#[cfg(not(windows))]
fn free_space_bytes_sync(_path: &Path) -> Option<u64> {
    None
}

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
    /// Authoritative progress after reconcile (`job.downloaded_bytes`).
    pub downloaded_bytes: u64,
    /// Observed `.part` length (v0) or tracked bytes (v1+ version gate).
    pub on_disk: u64,
    /// True when `job.downloaded_bytes` / progress were updated.
    pub changed: bool,
    /// True when single-stream `metadata_len` was applied.
    pub used_metadata_len: bool,
    /// True when `transfer_format_version >= 1` skipped the metadata_len path.
    pub version_gated: bool,
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
    ReconcileResult {
        downloaded_bytes: job.downloaded_bytes,
        on_disk,
        changed,
        used_metadata_len: true,
        version_gated: false,
    }
}

/// Align `job.downloaded_bytes` (and progress) with the contiguous `.part` length.
///
/// - `transfer_format_version >= 1`: **version gate** — do not use `metadata_len`
///   for progress/Range (map-authoritative). Leaves `downloaded_bytes` unchanged
///   (safe no-op when map is still absent).
/// - version 0: single-stream — set `downloaded_bytes` from `.part` length.
///
/// Engine uses `metadata_len` + `apply_partial_progress_from_disk` so the mutex
/// is not held across filesystem I/O; this convenience wrapper remains for tests
/// and call sites that already own the job exclusively.
#[allow(dead_code)] // public API; engine prefers lock-split path
pub async fn reconcile_partial_progress(job: &mut Job) -> ReconcileResult {
    if job.transfer_format_version >= 1 {
        return ReconcileResult {
            downloaded_bytes: job.downloaded_bytes,
            on_disk: job.downloaded_bytes,
            changed: false,
            used_metadata_len: false,
            version_gated: true,
        };
    }

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

/// Parse `Content-Range: bytes START-END/TOTAL`.
///
/// Unit is matched case-insensitively (`bytes` / `Bytes`).
/// `TOTAL` may be `*` (unknown length) → third field is `None`.
/// Unsatisfied forms (`bytes */1234`) and non-`bytes` units return `None`.
pub fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let value = value.trim();
    // RFC 9110: range unit is case-insensitive.
    let (unit, rest) = value.split_once(char::is_whitespace)?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let range_and_total = rest.trim();
    let (range, total) = range_and_total.split_once('/')?;
    // 416 unsatisfied-range form: `bytes */TOTAL` — no start/end to resume from.
    if range.trim() == "*" {
        return None;
    }
    let (start, end) = range.split_once('-')?;
    let start = start.trim().parse().ok()?;
    let end = end.trim().parse().ok()?;
    let total = total.trim();
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    Some((start, end, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_unsafe_names() {
        assert_eq!(sanitize_filename("a/b\\c:d?.zip"), "a_b_c_d_.zip");
    }

    #[cfg(windows)]
    #[test]
    fn disk_free_query_path_adds_unc_trailing_sep() {
        let q = disk_free_query_path(Path::new(r"\\server\share\file.part"));
        let s = q.to_string_lossy();
        assert!(s.ends_with('\\'), "UNC query must end with \\, got {s}");
        assert!(
            is_unc_share_root(Path::new(s.trim_end_matches('\\'))),
            "should stop at share root, got {s}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn disk_free_query_path_walks_to_existing_ancestor() {
        let dir = std::env::temp_dir();
        let missing = dir.join("no-such-dir-rusticdl-free-space").join("file.part");
        let q = disk_free_query_path(&missing);
        assert!(q.exists(), "query path should exist: {}", q.display());
    }

    #[tokio::test]
    async fn reconcile_version_gate_skips_metadata_len() {
        let dir =
            std::env::temp_dir().join(format!("rusticdl-reconcile-v1-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("file.bin");
        let temp = temp_path_for(&target);
        // Sparse preallocated-style file would mislead metadata_len.
        std::fs::write(&temp, vec![0u8; 10_000]).unwrap();

        let mut job = Job::new(
            "https://example.com/file.bin".into(),
            "file.bin".into(),
            target,
            temp,
        );
        job.transfer_format_version = 1;
        job.downloaded_bytes = 42; // map-authoritative placeholder
        job.total_bytes = 10_000;

        let result = reconcile_partial_progress(&mut job).await;
        assert!(result.version_gated);
        assert!(!result.used_metadata_len);
        assert_eq!(result.downloaded_bytes, 42);
        assert_eq!(job.downloaded_bytes, 42); // unchanged — no metadata_len

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reconcile_v0_uses_metadata_len() {
        let dir =
            std::env::temp_dir().join(format!("rusticdl-reconcile-v0-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("file.bin");
        let temp = temp_path_for(&target);
        std::fs::write(&temp, vec![1u8; 1234]).unwrap();

        let mut job = Job::new(
            "https://example.com/file.bin".into(),
            "file.bin".into(),
            target,
            temp,
        );
        job.transfer_format_version = 0;
        job.downloaded_bytes = 0;
        job.total_bytes = 5000;

        let result = reconcile_partial_progress(&mut job).await;
        assert!(!result.version_gated);
        assert!(result.used_metadata_len);
        assert_eq!(result.downloaded_bytes, 1234);
        assert_eq!(job.downloaded_bytes, 1234);
        assert!((job.progress - 24.68).abs() < 0.1);

        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!((start, end, total), (100, 199, Some(1000)));
    }

    #[test]
    fn parses_content_range_unit_case_insensitive() {
        let (start, end, total) = parse_content_range("Bytes 100-199/1000").unwrap();
        assert_eq!((start, end, total), (100, 199, Some(1000)));
        let (start, end, total) = parse_content_range("BYTES 0-0/*").unwrap();
        assert_eq!((start, end, total), (0, 0, None));
    }

    #[test]
    fn parses_content_range_star_total() {
        // CDN probe / open-ended: `bytes 0-0/*`
        let (start, end, total) = parse_content_range("bytes 0-0/*").unwrap();
        assert_eq!((start, end, total), (0, 0, None));

        let (start, end, total) = parse_content_range("bytes 500-999/*").unwrap();
        assert_eq!((start, end, total), (500, 999, None));
    }

    #[test]
    fn parse_content_range_rejects_unsatisfied_and_garbage() {
        assert!(parse_content_range("bytes */1000").is_none());
        assert!(parse_content_range("bytes */*").is_none());
        assert!(parse_content_range("items 0-1/2").is_none());
        assert!(parse_content_range("bytes abc-def/10").is_none());
        assert!(parse_content_range("bytes 0-1/nope").is_none());
        assert!(parse_content_range("").is_none());
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

    #[test]
    fn preallocate_margin_is_max_of_64mib_and_one_percent() {
        assert_eq!(preallocate_margin(0), 64 * 1024 * 1024);
        assert_eq!(preallocate_margin(100), 64 * 1024 * 1024);
        let big = 20 * 1024 * 1024 * 1024u64; // 20 GiB → 1% = 200 MiB
        assert_eq!(preallocate_margin(big), big / 100);
    }

    #[test]
    fn free_space_gate_helpers() {
        let total = 1000u64;
        let remaining = 100u64;
        let margin = preallocate_margin(total);
        assert!(!free_space_allows_write(remaining, remaining));
        assert!(free_space_allows_write(remaining + 1, remaining));
        assert!(!free_space_allows_preallocate(
            remaining + margin,
            remaining,
            total
        ));
        assert!(free_space_allows_preallocate(
            remaining + margin + 1,
            remaining,
            total
        ));
    }

    #[tokio::test]
    async fn free_space_bytes_on_temp() {
        let dir = std::env::temp_dir();
        let free = free_space_bytes(&dir).await;
        #[cfg(windows)]
        {
            let free = free.expect("GetDiskFreeSpaceExW should work on temp dir");
            assert!(free > 0, "expected positive free space on temp volume");
        }
        #[cfg(not(windows))]
        {
            // Non-Windows: API not implemented; fail-open returns None.
            let _ = free;
        }
    }
}
