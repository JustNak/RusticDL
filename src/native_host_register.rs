//! Register the browser native messaging host on Linux.
//!
//! Chrome/Firefox require an **absolute** `path` in a JSON manifest under
//! each browser's config directory. Rewriting on launch keeps that path
//! correct after a portable move or in-place update.

use std::fs;
use std::path::{Path, PathBuf};

use crate::branding::{NATIVE_HOST_BIN_NAME, NATIVE_HOST_NAME};

const CHROMIUM_TEMPLATE: &str =
    include_str!("../apps/native-host/manifests/chromium.template.json");
const EDGE_TEMPLATE: &str = include_str!("../apps/native-host/manifests/edge.template.json");
const FIREFOX_TEMPLATE: &str = include_str!("../apps/native-host/manifests/firefox.template.json");
const PINNED_CHROMIUM_ID: &str = "looccikmfpkiagfaeiocohmcneoacmom";
const DEFAULT_FIREFOX_ID: &str = "rusticdl@local";

const CHROMIUM_CONFIG_DIRS: &[&str] = &[
    "google-chrome",
    "google-chrome-beta",
    "google-chrome-unstable",
    "google-chrome-for-testing",
    "chromium",
    "BraveSoftware/Brave-Browser",
    "vivaldi",
];

const EDGE_CONFIG_DIRS: &[&str] = &[
    "microsoft-edge",
    "microsoft-edge-beta",
    "microsoft-edge-dev",
];

/// Result of writing native-messaging manifests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHostRegisterReport {
    pub host_path: PathBuf,
    pub written: Vec<PathBuf>,
}

/// Register when a sibling native-host binary exists. `Ok(None)` if this
/// install is desktop-only (no host next to the app).
pub fn sync_native_host_registration() -> Result<Option<NativeHostRegisterReport>, String> {
    match sibling_native_host_path() {
        Some(host) => register_native_host_at(&host).map(Some),
        None => Ok(None),
    }
}

/// Register using the sibling native-host binary, or error if it is missing.
pub fn register_native_host() -> Result<NativeHostRegisterReport, String> {
    let host = sibling_native_host_path().ok_or_else(|| {
        format!(
            "RusticDL Backend ({NATIVE_HOST_BIN_NAME}) was not found next to the app. \
Install the full Linux package or run scripts/register-native-host.sh."
        )
    })?;
    register_native_host_at(&host)
}

/// Canonical sibling `rusticdl-native-host` next to the running desktop app.
pub fn sibling_native_host_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let host = exe.parent()?.join(NATIVE_HOST_BIN_NAME);
    host.is_file().then_some(host)
}

pub fn register_native_host_at(host_path: &Path) -> Result<NativeHostRegisterReport, String> {
    if !host_path.is_file() {
        return Err(format!(
            "Native host binary not found:\n{}",
            host_path.display()
        ));
    }
    let host_path = host_path
        .canonicalize()
        .map_err(|error| format!("Could not resolve native host path: {error}"))?;
    if !host_path.is_absolute() {
        return Err("Native messaging host path must be absolute on Linux.".into());
    }

    let host_path_str = host_path
        .to_str()
        .ok_or_else(|| "Native host path is not valid UTF-8.".to_string())?;

    let chromium_id = chromium_extension_id();
    let edge_id = chromium_id.clone();
    let firefox_id = DEFAULT_FIREFOX_ID.to_string();

    let chromium_json = render_manifest(
        CHROMIUM_TEMPLATE,
        host_path_str,
        &chromium_id,
        "__CHROMIUM_EXTENSION_ID__",
    );
    let edge_json = render_manifest(
        EDGE_TEMPLATE,
        host_path_str,
        &edge_id,
        "__EDGE_EXTENSION_ID__",
    );
    let firefox_json = render_manifest(
        FIREFOX_TEMPLATE,
        host_path_str,
        &firefox_id,
        "__FIREFOX_EXTENSION_ID__",
    );

    let mut written = Vec::new();

    if let Some(install_root) = host_path.parent() {
        let copies = install_root.join("native-messaging");
        fs::create_dir_all(&copies)
            .map_err(|error| format!("Could not create {}: {error}", copies.display()))?;
        written.push(write_utf8(
            &copies.join(format!("{NATIVE_HOST_NAME}.chrome.json")),
            &chromium_json,
        )?);
        written.push(write_utf8(
            &copies.join(format!("{NATIVE_HOST_NAME}.edge.json")),
            &edge_json,
        )?);
        written.push(write_utf8(
            &copies.join(format!("{NATIVE_HOST_NAME}.firefox.json")),
            &firefox_json,
        )?);
    }

    let manifest_name = format!("{NATIVE_HOST_NAME}.json");
    for dir in chromium_native_messaging_dirs() {
        written.push(write_manifest_file(&dir, &manifest_name, &chromium_json)?);
    }
    for dir in edge_native_messaging_dirs() {
        written.push(write_manifest_file(&dir, &manifest_name, &edge_json)?);
    }
    for dir in firefox_native_messaging_dirs() {
        written.push(write_manifest_file(&dir, &manifest_name, &firefox_json)?);
    }

    Ok(NativeHostRegisterReport { host_path, written })
}

fn write_manifest_file(dir: &Path, name: &str, contents: &str) -> Result<PathBuf, String> {
    fs::create_dir_all(dir)
        .map_err(|error| format!("Could not create {}: {error}", dir.display()))?;
    write_utf8(&dir.join(name), contents)
}

fn write_utf8(path: &Path, contents: &str) -> Result<PathBuf, String> {
    fs::write(path, contents)
        .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
    Ok(path.to_path_buf())
}

fn render_manifest(template: &str, host_path: &str, id: &str, id_placeholder: &str) -> String {
    template
        .replace("__HOST_PATH__", host_path)
        .replace(id_placeholder, id)
}

fn chromium_extension_id() -> String {
    const IDENTITY: &str = include_str!("../apps/extension/chromium-identity.json");
    serde_json::from_str::<serde_json::Value>(IDENTITY)
        .ok()
        .and_then(|value| {
            value
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        })
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| PINNED_CHROMIUM_ID.to_string())
}

fn xdg_config_home() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    home_dir()
        .map(|home| home.join(".config"))
        .unwrap_or_else(|| PathBuf::from(".config"))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

fn chromium_native_messaging_dirs() -> Vec<PathBuf> {
    let config = xdg_config_home();
    CHROMIUM_CONFIG_DIRS
        .iter()
        .map(|rel| config.join(rel).join("NativeMessagingHosts"))
        .collect()
}

fn edge_native_messaging_dirs() -> Vec<PathBuf> {
    let config = xdg_config_home();
    EDGE_CONFIG_DIRS
        .iter()
        .map(|rel| config.join(rel).join("NativeMessagingHosts"))
        .collect()
}

fn firefox_native_messaging_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home_dir() {
        dirs.push(home.join(".mozilla/native-messaging-hosts"));
        dirs.push(home.join(".librewolf/native-messaging-hosts"));
        dirs.push(home.join(".waterfox/native-messaging-hosts"));
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_manifest_uses_absolute_host_path() {
        let host = "/home/user/.local/lib/RusticDL/rusticdl-native-host";
        let json = render_manifest(
            CHROMIUM_TEMPLATE,
            host,
            PINNED_CHROMIUM_ID,
            "__CHROMIUM_EXTENSION_ID__",
        );
        assert!(json.contains(&format!("\"path\": \"{host}\"")));
        assert!(host.starts_with('/'));
        assert!(!json.contains("__HOST_PATH__"));
        assert!(json.contains(PINNED_CHROMIUM_ID));
        assert!(!json.contains('~'));
    }

    #[test]
    fn firefox_manifest_uses_allowed_extensions() {
        let json = render_manifest(
            FIREFOX_TEMPLATE,
            "/opt/RusticDL/rusticdl-native-host",
            DEFAULT_FIREFOX_ID,
            "__FIREFOX_EXTENSION_ID__",
        );
        assert!(json.contains("allowed_extensions"));
        assert!(json.contains(DEFAULT_FIREFOX_ID));
        assert!(json.contains("\"path\": \"/opt/RusticDL/rusticdl-native-host\""));
    }

    #[test]
    fn chromium_identity_matches_pinned_id() {
        assert_eq!(chromium_extension_id(), PINNED_CHROMIUM_ID);
    }

    #[test]
    fn xdg_config_dirs_are_absolute_when_home_is_set() {
        let prev_config = std::env::var_os("XDG_CONFIG_HOME");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/rusticdl-test-config");
        std::env::set_var("HOME", "/tmp/rusticdl-test-home");
        let chrome = chromium_native_messaging_dirs();
        let firefox = firefox_native_messaging_dirs();
        match prev_config {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match prev_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        assert!(chrome
            .iter()
            .all(|path| path.is_absolute() && path.ends_with("NativeMessagingHosts")));
        assert!(firefox
            .iter()
            .all(|path| path.is_absolute() && path.ends_with("native-messaging-hosts")));
        assert!(chrome
            .iter()
            .any(|path| path.to_string_lossy().contains("google-chrome")));
        assert!(firefox
            .iter()
            .any(|path| path.to_string_lossy().contains(".mozilla")));
    }
}
