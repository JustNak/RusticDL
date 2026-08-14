use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::branding::APP_VERSION;
use crate::download::Job;
use crate::settings::Settings;
use crate::updater::normalize_version;

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

/// Release notes snapshot for the post-update “What’s new” dialog.
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

/// Load a pending What’s new snapshot if present and valid for this build.
///
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

/// Remove the pending snapshot (ack after showing, or discard stale).
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
    let Ok(bytes) = fs::read(&paths.state) else {
        return Vec::new();
    };
    serde_json::from_slice::<PersistedState>(&bytes)
        .map(|s| s.jobs)
        .unwrap_or_default()
}

pub fn save_jobs(paths: &AppPaths, jobs: &[Job]) -> Result<(), String> {
    let _guard = write_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    ensure_app_dirs(paths)?;
    let json = serde_json::to_vec(&PersistedStateRef { jobs })
        .map_err(|e| format!("Could not serialize state: {e}"))?;
    atomic_write(&paths.state, &json)
}

/// Write via temp file + rename so readers never see a partial JSON document.
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
}
