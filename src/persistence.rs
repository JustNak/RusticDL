use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::branding::APP_VERSION;
use crate::download::{Job, JobState};
use crate::settings::Settings;
use crate::updater::normalize_version;

/// Max `JobState::Completed` rows kept in `state.json`.
///
/// Oldest completed jobs are dropped first. Active and paused jobs are never
/// dropped, nor are Failed / Canceled. Not a Settings field — bump this
/// constant if the product needs a different bound.
pub const MAX_COMPLETED_HISTORY: usize = 500;

/// Serializes settings + state writes so UI layout saves cannot race extension IPC.
fn write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub root: PathBuf,
    pub settings: PathBuf,
    pub state: PathBuf,
    /// Snapshot written before update handoff; shown once after relaunch.
    pub pending_whats_new: PathBuf,
}

pub fn app_paths() -> AppPaths {
    let root = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(crate::branding::APP_DATA_DIR_NAME);
    AppPaths {
        settings: root.join("settings.json"),
        state: root.join("state.json"),
        pending_whats_new: root.join("pending_whats_new.json"),
        root,
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PendingWhatsNew {
    pub from_version: String,
    pub to_version: String,
    #[serde(default)]
    pub release_name: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub notes: Option<String>,
}

impl PendingWhatsNew {
    /// True when this snapshot targets the binary that is now running.
    pub fn matches_running_app(&self) -> bool {
        normalize_version(&self.to_version) == normalize_version(APP_VERSION)
    }
}

/// Corrupt or stale files (wrong `toVersion`) are deleted so they cannot reappear later.
pub fn load_pending_whats_new(paths: &AppPaths) -> Option<PendingWhatsNew> {
    let bytes = fs::read(&paths.pending_whats_new).ok()?;
    let pending: PendingWhatsNew = match serde_json::from_slice(&bytes) {
        Ok(pending) => pending,
        Err(_) => {
            let _ = clear_pending_whats_new(paths);
            return None;
        }
    };
    if pending.matches_running_app() {
        Some(pending)
    } else {
        let _ = clear_pending_whats_new(paths);
        None
    }
}

/// Persist notes before quitting into the updater (best-effort).
pub fn save_pending_whats_new(paths: &AppPaths, pending: &PendingWhatsNew) -> Result<(), String> {
    let _guard = write_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    ensure_app_dirs(paths)?;
    let json = serde_json::to_vec_pretty(pending)
        .map_err(|e| format!("Could not serialize What’s new snapshot: {e}"))?;
    atomic_write(&paths.pending_whats_new, &json)
}

pub fn clear_pending_whats_new(paths: &AppPaths) -> Result<(), String> {
    let _guard = write_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match fs::remove_file(&paths.pending_whats_new) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Could not clear What’s new snapshot: {e}")),
    }
}

pub fn ensure_app_dirs(paths: &AppPaths) -> Result<(), String> {
    fs::create_dir_all(&paths.root).map_err(|e| format!("Could not create app data dir: {e}"))
}

pub fn load_settings(paths: &AppPaths) -> Settings {
    let Ok(bytes) = fs::read(&paths.settings) else {
        return Settings::default();
    };
    let mut settings: Settings = serde_json::from_slice(&bytes).unwrap_or_default();
    settings.sanitize_appearance();
    settings
}

pub fn save_settings(paths: &AppPaths, settings: &Settings) -> Result<(), String> {
    let _guard = write_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    ensure_app_dirs(paths)?;
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|e| format!("Could not serialize settings: {e}"))?;
    atomic_write(&paths.settings, &json)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    jobs: Vec<Job>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedStateRef<'a> {
    jobs: &'a [Job],
}

pub fn load_jobs(paths: &AppPaths) -> Vec<Job> {
    load_jobs_with_history_cap(paths, MAX_COMPLETED_HISTORY)
}

pub fn save_jobs(paths: &AppPaths, jobs: &[Job]) -> Result<(), String> {
    save_jobs_with_history_cap(paths, jobs, MAX_COMPLETED_HISTORY)
}

fn load_jobs_with_history_cap(paths: &AppPaths, max_completed: usize) -> Vec<Job> {
    let Ok(bytes) = fs::read(&paths.state) else {
        return Vec::new();
    };
    let jobs = serde_json::from_slice::<PersistedState>(&bytes)
        .map(|s| s.jobs)
        .unwrap_or_default();
    match cap_completed_history(&jobs, max_completed) {
        Cow::Borrowed(_) => jobs,
        Cow::Owned(trimmed) => {
            let _ = save_jobs_with_history_cap(paths, &trimmed, max_completed);
            trimmed
        }
    }
}

fn save_jobs_with_history_cap(
    paths: &AppPaths,
    jobs: &[Job],
    max_completed: usize,
) -> Result<(), String> {
    let _guard = write_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    ensure_app_dirs(paths)?;
    let jobs = cap_completed_history(jobs, max_completed);
    let json = serde_json::to_vec(&PersistedStateRef {
        jobs: jobs.as_ref(),
    })
    .map_err(|e| format!("Could not serialize state: {e}"))?;
    atomic_write(&paths.state, &json)
}

/// Keep at most `max_completed` `JobState::Completed` rows, dropping the oldest
/// by `completed_at` (fallback: `created_at`). Other states are untouched.
fn cap_completed_history(jobs: &[Job], max_completed: usize) -> Cow<'_, [Job]> {
    let completed_idxs: Vec<usize> = jobs
        .iter()
        .enumerate()
        .filter(|(_, job)| job.state == JobState::Completed)
        .map(|(idx, _)| idx)
        .collect();
    if completed_idxs.len() <= max_completed {
        return Cow::Borrowed(jobs);
    }

    let drop_n = completed_idxs.len() - max_completed;
    let mut oldest = completed_idxs;
    oldest.sort_by_key(|&idx| jobs[idx].completed_at.unwrap_or(jobs[idx].created_at));
    let drop_idxs: HashSet<usize> = oldest.into_iter().take(drop_n).collect();

    Cow::Owned(
        jobs.iter()
            .enumerate()
            .filter(|(idx, _)| !drop_idxs.contains(idx))
            .map(|(_, job)| job.clone())
            .collect(),
    )
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Persistence path has no parent directory.".to_string())?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data.json");
    let temp_path = parent.join(format!(".{file_name}.tmp"));
    fs::write(&temp_path, bytes).map_err(|e| format!("Could not write temp file: {e}"))?;
    fs::rename(&temp_path, path).map_err(|e| {
        let _ = fs::remove_file(&temp_path);
        format!("Could not finalize write: {e}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_paths(tag: &str) -> AppPaths {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("rusticdl-whats-new-{tag}-{nanos}"));
        let _ = fs::create_dir_all(&root);
        AppPaths {
            settings: root.join("settings.json"),
            state: root.join("state.json"),
            pending_whats_new: root.join("pending_whats_new.json"),
            root,
        }
    }

    #[test]
    fn pending_whats_new_round_trip() {
        let paths = temp_paths("round");
        let pending = PendingWhatsNew {
            from_version: "0.1.0".into(),
            to_version: APP_VERSION.into(),
            release_name: "Test".into(),
            html_url: "https://example.com".into(),
            notes: Some("- fix one\n- fix two".into()),
        };
        save_pending_whats_new(&paths, &pending).unwrap();
        let loaded = load_pending_whats_new(&paths).expect("matching pending");
        assert_eq!(loaded, pending);
        clear_pending_whats_new(&paths).unwrap();
        assert!(load_pending_whats_new(&paths).is_none());
        let _ = fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn stale_pending_whats_new_is_discarded() {
        let paths = temp_paths("stale");
        let pending = PendingWhatsNew {
            from_version: "0.0.1".into(),
            to_version: "9.9.9".into(),
            release_name: "Future".into(),
            html_url: String::new(),
            notes: None,
        };
        save_pending_whats_new(&paths, &pending).unwrap();
        assert!(load_pending_whats_new(&paths).is_none());
        assert!(!paths.pending_whats_new.is_file());
        let _ = fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn corrupt_pending_whats_new_is_discarded() {
        let paths = temp_paths("corrupt");
        ensure_app_dirs(&paths).unwrap();
        fs::write(&paths.pending_whats_new, b"{not valid json").unwrap();
        assert!(load_pending_whats_new(&paths).is_none());
        assert!(!paths.pending_whats_new.is_file());
        let _ = fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn save_jobs_writes_compact_camel_case_json() {
        let paths = temp_paths("jobs");
        let job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        let job_id = job.id.clone();
        save_jobs(&paths, std::slice::from_ref(&job)).unwrap();

        let bytes = fs::read(&paths.state).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            !text.contains('\n'),
            "state.json must be compact, not pretty"
        );
        assert!(
            text.contains("\"jobs\""),
            "camelCase jobs key must be present"
        );
        assert!(
            !text.contains("\"Jobs\""),
            "PascalCase jobs key must not be written"
        );

        let loaded = load_jobs(&paths);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, job_id);
        assert_eq!(loaded[0].filename, "f.bin");
        assert_eq!(loaded[0].url, "https://example.com/f.bin");
        let _ = fs::remove_dir_all(&paths.root);
    }

    fn sample_job(id: &str, state: JobState, created_at: u64, completed_at: Option<u64>) -> Job {
        let mut job = Job::new(
            format!("https://example.com/{id}.bin"),
            format!("{id}.bin"),
            PathBuf::from(format!("C:\\dl\\{id}.bin")),
            PathBuf::from(format!("C:\\dl\\{id}.bin.part")),
        );
        job.id = id.into();
        job.state = state;
        job.created_at = created_at;
        job.completed_at = completed_at;
        job
    }

    fn write_state_untrimmed(paths: &AppPaths, jobs: &[Job]) {
        ensure_app_dirs(paths).unwrap();
        let json = serde_json::to_vec(&PersistedStateRef { jobs }).unwrap();
        atomic_write(&paths.state, &json).unwrap();
    }

    fn ids(jobs: &[Job]) -> Vec<&str> {
        jobs.iter().map(|job| job.id.as_str()).collect()
    }

    #[test]
    fn cap_drops_oldest_completed_only() {
        let jobs = vec![
            sample_job("active", JobState::Downloading, 1, None),
            sample_job("old-done", JobState::Completed, 2, Some(10)),
            sample_job("paused", JobState::Paused, 3, None),
            sample_job("mid-done", JobState::Completed, 4, Some(20)),
            sample_job("failed", JobState::Failed, 5, Some(25)),
            sample_job("new-done", JobState::Completed, 6, Some(30)),
            sample_job("canceled", JobState::Canceled, 7, Some(35)),
            sample_job("queued", JobState::Queued, 8, None),
        ];

        let trimmed = cap_completed_history(&jobs, 2);
        assert_eq!(
            ids(&trimmed),
            ["active", "paused", "mid-done", "failed", "new-done", "canceled", "queued"]
        );
        assert!(trimmed.iter().all(|job| job.id != "old-done"));
        assert_eq!(
            trimmed
                .iter()
                .filter(|job| job.state == JobState::Completed)
                .count(),
            2
        );
    }

    #[test]
    fn cap_uses_created_at_when_completed_at_missing() {
        let jobs = vec![
            sample_job("older", JobState::Completed, 10, None),
            sample_job("newer", JobState::Completed, 20, None),
            sample_job("newest", JobState::Completed, 30, Some(5)),
        ];
        let trimmed = cap_completed_history(&jobs, 2);
        assert_eq!(ids(&trimmed), ["older", "newer"]);
    }

    #[test]
    fn save_jobs_trims_oldest_completed() {
        let paths = temp_paths("save-cap");
        let jobs = vec![
            sample_job("keep-active", JobState::Downloading, 1, None),
            sample_job("drop", JobState::Completed, 2, Some(10)),
            sample_job("keep-paused", JobState::Paused, 3, None),
            sample_job("keep-old", JobState::Completed, 4, Some(20)),
            sample_job("keep-new", JobState::Completed, 5, Some(30)),
        ];
        save_jobs_with_history_cap(&paths, &jobs, 2).unwrap();

        let stored: PersistedState =
            serde_json::from_slice(&fs::read(&paths.state).unwrap()).unwrap();
        assert_eq!(
            ids(&stored.jobs),
            ["keep-active", "keep-paused", "keep-old", "keep-new"]
        );
        let _ = fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn load_jobs_trims_and_rewrites_oversized_file() {
        let paths = temp_paths("load-cap");
        let jobs = vec![
            sample_job("keep-active", JobState::Starting, 1, None),
            sample_job("drop-a", JobState::Completed, 2, Some(10)),
            sample_job("drop-b", JobState::Completed, 3, Some(11)),
            sample_job("keep-paused", JobState::Paused, 4, None),
            sample_job("keep-done", JobState::Completed, 5, Some(40)),
        ];
        write_state_untrimmed(&paths, &jobs);
        let before = fs::read(&paths.state).unwrap();
        assert!(before.len() > 10, "fixture must write a real state file");

        let loaded = load_jobs_with_history_cap(&paths, 1);
        assert_eq!(ids(&loaded), ["keep-active", "keep-paused", "keep-done"]);

        let stored: PersistedState =
            serde_json::from_slice(&fs::read(&paths.state).unwrap()).unwrap();
        assert_eq!(
            ids(&stored.jobs),
            ["keep-active", "keep-paused", "keep-done"]
        );
        assert!(
            fs::read(&paths.state).unwrap().len() < before.len(),
            "oversized state.json must shrink on load"
        );
        let _ = fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn load_jobs_under_cap_does_not_rewrite() {
        let paths = temp_paths("load-nocap");
        let jobs = vec![
            sample_job("active", JobState::Queued, 1, None),
            sample_job("done", JobState::Completed, 2, Some(10)),
        ];
        write_state_untrimmed(&paths, &jobs);
        let before = fs::read(&paths.state).unwrap();

        let loaded = load_jobs_with_history_cap(&paths, 500);
        assert_eq!(ids(&loaded), ["active", "done"]);
        assert_eq!(fs::read(&paths.state).unwrap(), before);
        let _ = fs::remove_dir_all(&paths.root);
    }
}
