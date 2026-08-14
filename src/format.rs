use std::time::{SystemTime, UNIX_EPOCH};

use crate::download::{Job, JobState};
use crate::settings::{SortColumn, SortDirection};

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

pub fn format_speed(bytes_per_sec: u64) -> String {
    if bytes_per_sec == 0 {
        return "—".into();
    }
    format!("{}/s", format_bytes(bytes_per_sec))
}

pub fn format_eta(secs: u64) -> String {
    if secs == 0 {
        return "—".into();
    }
    let secs = quantize_eta(secs);
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes >= 2 {
        // Drop seconds once the estimate is minutes-scale — they only jitter.
        format!("{minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// Exact elapsed time for a finished transfer (not quantized like ETA).
pub fn format_duration(secs: u64) -> String {
    if secs == 0 {
        return "—".into();
    }
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        if seconds == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m {seconds:02}s")
        }
    } else {
        format!("{seconds}s")
    }
}

/// Snap remaining-time to coarse buckets so a 1–2s engine wiggle does not
/// rewrite the label every tick.
fn quantize_eta(secs: u64) -> u64 {
    if secs < 10 {
        secs
    } else if secs < 60 {
        secs / 5 * 5
    } else if secs < 10 * 60 {
        secs / 15 * 15
    } else if secs < 60 * 60 {
        secs / 30 * 30
    } else {
        secs / 60 * 60
    }
}

pub fn format_size(job: &Job) -> String {
    if job.total_bytes > 0 {
        if job.downloaded_bytes > 0 && job.state != JobState::Completed {
            format!(
                "{} / {}",
                format_bytes(job.downloaded_bytes),
                format_bytes(job.total_bytes)
            )
        } else {
            format_bytes(job.total_bytes)
        }
    } else if job.downloaded_bytes > 0 {
        format_bytes(job.downloaded_bytes)
    } else {
        "—".into()
    }
}

/// Format a job's created-at timestamp for the queue Date column.
///
/// - Under 24 hours: relative (`Just now`, `12m ago`, `5h ago`)
/// - 24 hours or older: absolute calendar date using the OS short-date format
///   (falls back to `mm/dd/yyyy` when the system formatter is unavailable)
pub fn format_date(unix_secs: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let age = now.saturating_sub(unix_secs);
    if age < 60 {
        "Just now".into()
    } else if age < 3600 {
        format!("{}m ago", age / 60)
    } else if age < 86_400 {
        format!("{}h ago", age / 3600)
    } else {
        format_absolute_date(unix_secs)
    }
}

/// Absolute local calendar date for timestamps older than a day.
fn format_absolute_date(unix_secs: u64) -> String {
    #[cfg(windows)]
    {
        if let Some(s) = format_absolute_date_windows(unix_secs) {
            return s;
        }
    }
    format_absolute_date_fallback(unix_secs)
}

/// Best-effort UTC calendar date as `mm/dd/yyyy` when locale formatting fails.
fn format_absolute_date_fallback(unix_secs: u64) -> String {
    // Civil date from Unix days (UTC). Good enough as a portable last resort.
    // Algorithm from Howard Hinnant's civil_from_days (public domain).
    let z = (unix_secs / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{m:02}/{d:02}/{y:04}")
}

#[cfg(windows)]
fn format_absolute_date_windows(unix_secs: u64) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::Globalization::{GetDateFormatEx, DATE_SHORTDATE};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

    // FILETIME: 100ns ticks since 1601-01-01 UTC.
    // Unix epoch is 11644473600 seconds after that.
    const UNIX_TO_FILETIME_SECS: u64 = 11_644_473_600;
    let ticks = unix_secs
        .checked_add(UNIX_TO_FILETIME_SECS)?
        .checked_mul(10_000_000)?;
    let file_time = FILETIME {
        dwLowDateTime: (ticks & 0xFFFF_FFFF) as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };

    unsafe {
        let mut utc = SYSTEMTIME::default();
        FileTimeToSystemTime(&file_time, &mut utc).ok()?;

        let mut local = SYSTEMTIME::default();
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;

        // First call: required buffer length (chars, including null).
        let needed = GetDateFormatEx(
            PCWSTR::null(),
            DATE_SHORTDATE,
            Some(&local as *const SYSTEMTIME),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
        );
        if needed <= 0 {
            return None;
        }

        let mut buf = vec![0u16; needed as usize];
        let written = GetDateFormatEx(
            PCWSTR::null(),
            DATE_SHORTDATE,
            Some(&local as *const SYSTEMTIME),
            PCWSTR::null(),
            Some(buf.as_mut_slice()),
            PCWSTR::null(),
        );
        if written <= 0 {
            return None;
        }

        // written includes the terminating null.
        let len = (written as usize).saturating_sub(1).min(buf.len());
        String::from_utf16(&buf[..len]).ok()
    }
}

pub fn filter_jobs<'a>(jobs: &'a [Job], filter_index: i32) -> Vec<&'a Job> {
    jobs.iter()
        .filter(|job| match filter_index {
            1 => matches!(
                job.state,
                JobState::Queued | JobState::Starting | JobState::Downloading | JobState::Paused
            ),
            2 => job.state == JobState::Completed,
            3 => matches!(job.state, JobState::Failed | JobState::Canceled),
            _ => true,
        })
        .collect()
}

fn size_sort_key(job: &Job) -> u64 {
    if job.total_bytes > 0 {
        job.total_bytes
    } else {
        job.downloaded_bytes
    }
}

/// ASCII case-insensitive order (no allocation). Matches `to_lowercase` for
/// typical filenames; non-ASCII letters are compared as-is.
fn cmp_filename_ci(a: &str, b: &str) -> std::cmp::Ordering {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

/// Stable sort of the visible queue by the user's preferred column/direction.
pub fn sort_jobs(jobs: &mut [&Job], column: SortColumn, direction: SortDirection) {
    jobs.sort_by(|a, b| {
        let ord = match column {
            SortColumn::Name => {
                cmp_filename_ci(&a.filename, &b.filename).then_with(|| a.id.cmp(&b.id))
            }
            SortColumn::Date => a
                .created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id)),
            SortColumn::Speed => a.speed.cmp(&b.speed).then_with(|| a.id.cmp(&b.id)),
            SortColumn::Eta => a.eta_secs.cmp(&b.eta_secs).then_with(|| a.id.cmp(&b.id)),
            SortColumn::Size => size_sort_key(a)
                .cmp(&size_sort_key(b))
                .then_with(|| a.id.cmp(&b.id)),
        };
        match direction {
            SortDirection::Asc => ord,
            SortDirection::Desc => ord.reverse(),
        }
    });
}

pub fn count_jobs(jobs: &[Job]) -> (i32, i32, i32, i32) {
    let all = jobs.len() as i32;
    let active = jobs
        .iter()
        .filter(|j| {
            matches!(
                j.state,
                JobState::Queued | JobState::Starting | JobState::Downloading | JobState::Paused
            )
        })
        .count() as i32;
    let completed = jobs
        .iter()
        .filter(|j| j.state == JobState::Completed)
        .count() as i32;
    let failed = jobs
        .iter()
        .filter(|j| matches!(j.state, JobState::Failed | JobState::Canceled))
        .count() as i32;
    (all, active, completed, failed)
}

/// Aggregate download speed across currently transferring jobs.
pub fn total_download_speed(jobs: &[Job]) -> u64 {
    jobs.iter()
        .filter(|j| matches!(j.state, JobState::Downloading | JobState::Starting))
        .map(|j| j.speed)
        .sum()
}

/// Sum of completed file sizes (or downloaded bytes when total is unknown).
pub fn total_completed_bytes(jobs: &[Job]) -> u64 {
    jobs.iter()
        .filter(|j| j.state == JobState::Completed)
        .map(|j| {
            if j.total_bytes > 0 {
                j.total_bytes
            } else {
                j.downloaded_bytes
            }
        })
        .sum()
}

/// `query` is already trimmed and lowercased by the caller (empty matches all).
pub fn job_matches_search(job: &Job, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    job.filename.to_lowercase().contains(query)
        || job.url.to_lowercase().contains(query)
        || job
            .target_path
            .to_string_lossy()
            .to_lowercase()
            .contains(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_under_24h() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(format_date(now), "Just now");
        assert_eq!(format_date(now - 120), "2m ago");
        assert_eq!(format_date(now - 7200), "2h ago");
    }

    #[test]
    fn absolute_over_24h_is_calendar_date() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // ~3 days ago — must not be relative "Xd ago".
        let s = format_date(now - 3 * 86_400);
        assert!(!s.contains("ago"), "expected absolute date, got {s}");
        assert!(!s.is_empty());
        // System short date always includes the year for multi-day-old stamps.
        assert!(
            s.chars().any(|c| c.is_ascii_digit()),
            "expected digits in absolute date, got {s}"
        );
    }

    #[test]
    fn fallback_mm_dd_yyyy_shape() {
        // 2020-01-15 00:00:00 UTC
        let s = format_absolute_date_fallback(1_579_046_400);
        assert_eq!(s, "01/15/2020");
    }

    #[test]
    fn eta_unknown_is_em_dash() {
        assert_eq!(format_eta(0), "—");
    }

    #[test]
    fn eta_under_ten_seconds_is_exact() {
        assert_eq!(format_eta(7), "7s");
    }

    #[test]
    fn eta_seconds_snap_to_five() {
        assert_eq!(format_eta(54), "50s");
        assert_eq!(format_eta(53), "50s");
        assert_eq!(format_eta(56), "55s");
    }

    #[test]
    fn eta_minutes_drop_seconds_after_two() {
        assert_eq!(format_eta(125), "2m");
        assert_eq!(format_eta(190), "3m");
    }

    #[test]
    fn eta_one_minute_keeps_quantized_seconds() {
        assert_eq!(format_eta(70), "1m 00s");
        assert_eq!(format_eta(80), "1m 15s");
    }

    #[test]
    fn duration_unknown_is_em_dash() {
        assert_eq!(format_duration(0), "—");
    }

    #[test]
    fn duration_keeps_exact_seconds() {
        assert_eq!(format_duration(7), "7s");
        assert_eq!(format_duration(54), "54s");
        assert_eq!(format_duration(72), "1m 12s");
        assert_eq!(format_duration(120), "2m");
        assert_eq!(format_duration(3723), "1h 02m");
    }

    fn sample_job(id: &str, filename: &str, url: &str) -> Job {
        let mut job = Job::new(
            url.into(),
            filename.into(),
            std::path::PathBuf::from(format!("C:\\dl\\{filename}")),
            std::path::PathBuf::from(format!("C:\\dl\\{filename}.part")),
        );
        job.id = id.into();
        job
    }

    #[test]
    fn name_sort_is_case_insensitive_for_ascii() {
        let a = sample_job("2", "Zebra.bin", "https://example.com/z");
        let b = sample_job("1", "apple.bin", "https://example.com/a");
        let c = sample_job("3", "Banana.bin", "https://example.com/b");
        let mut jobs = vec![&a, &b, &c];
        sort_jobs(&mut jobs, SortColumn::Name, SortDirection::Asc);
        let names: Vec<&str> = jobs.iter().map(|j| j.filename.as_str()).collect();
        assert_eq!(names, ["apple.bin", "Banana.bin", "Zebra.bin"]);
    }

    #[test]
    fn search_matches_filename_url_and_path() {
        let job = sample_job("1", "Report.PDF", "https://cdn.example.com/files/x");
        assert!(job_matches_search(&job, "report"));
        assert!(job_matches_search(&job, "cdn.example"));
        assert!(job_matches_search(&job, "c:\\dl"));
        assert!(!job_matches_search(&job, "missing"));
        assert!(job_matches_search(&job, ""));
    }
}
