use std::fs;
use std::path::PathBuf;

use crate::download::Job;
use crate::settings::Settings;

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
    ensure_app_dirs(paths)?;
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|e| format!("Could not serialize settings: {e}"))?;
    fs::write(&paths.settings, json).map_err(|e| format!("Could not write settings: {e}"))
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
    ensure_app_dirs(paths)?;
    let state = PersistedState {
        jobs: jobs.to_vec(),
    };
    let json =
        serde_json::to_vec_pretty(&state).map_err(|e| format!("Could not serialize state: {e}"))?;
    fs::write(&paths.state, json).map_err(|e| format!("Could not write state: {e}"))
}
