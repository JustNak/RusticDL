use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::download::Job;
use crate::settings::Settings;

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
}

pub fn app_paths() -> AppPaths {
    let root = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(crate::branding::APP_DATA_DIR_NAME);
    AppPaths {
        settings: root.join("settings.json"),
        state: root.join("state.json"),
        root,
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
    let state = PersistedState {
        jobs: jobs.to_vec(),
    };
    let json =
        serde_json::to_vec_pretty(&state).map_err(|e| format!("Could not serialize state: {e}"))?;
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
