//! Browser extension integration settings (persisted with desktop settings).

use serde::{Deserialize, Serialize};

pub const DEFAULT_CAPTURED_FILE_EXTENSIONS: &[&str] = &[
    "7z", "apk", "bz2", "cab", "deb", "dmg", "doc", "docx", "exe", "gz", "iso", "jar", "msi",
    "pdf", "ppt", "pptx", "rar", "rpm", "tar", "tgz", "txz", "xls", "xlsx", "xz", "zip", "zst",
];

pub const DEFAULT_EXCLUDED_HOSTS: &[&str] = &["web.telegram.org"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DownloadHandoffMode {
    Off,
    #[default]
    Ask,
    Auto,
}

impl DownloadHandoffMode {
    pub fn as_protocol(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Ask => "ask",
            Self::Auto => "auto",
        }
    }

    pub fn from_protocol(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "ask" => Some(Self::Ask),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionIntegrationSettings {
    pub enabled: bool,
    pub download_handoff_mode: DownloadHandoffMode,
    pub context_menu_enabled: bool,
    pub show_progress_after_handoff: bool,
    pub show_badge_status: bool,
    pub excluded_hosts: Vec<String>,
    pub captured_file_extensions: Vec<String>,
    pub download_capture_debug_logging: bool,
}

impl Default for ExtensionIntegrationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            download_handoff_mode: DownloadHandoffMode::Ask,
            context_menu_enabled: true,
            show_progress_after_handoff: true,
            show_badge_status: true,
            excluded_hosts: DEFAULT_EXCLUDED_HOSTS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            captured_file_extensions: DEFAULT_CAPTURED_FILE_EXTENSIONS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            download_capture_debug_logging: false,
        }
    }
}

impl ExtensionIntegrationSettings {
    pub fn sanitize(&mut self) {
        self.excluded_hosts = normalize_hosts(&self.excluded_hosts);
        self.captured_file_extensions = normalize_extensions(&self.captured_file_extensions);
    }

    /// Protocol-facing camelCase JSON used by the extension.
    pub fn to_protocol_json(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "downloadHandoffMode": self.download_handoff_mode.as_protocol(),
            "contextMenuEnabled": self.context_menu_enabled,
            "showProgressAfterHandoff": self.show_progress_after_handoff,
            "showBadgeStatus": self.show_badge_status,
            "excludedHosts": self.excluded_hosts,
            "ignoredFileExtensions": [],
            "capturedFileExtensions": self.captured_file_extensions,
            "downloadCaptureDebugLogging": self.download_capture_debug_logging,
        })
    }

    pub fn from_protocol_json(value: &serde_json::Value) -> Result<Self, String> {
        let mut settings = Self::default();
        if let Some(enabled) = value.get("enabled").and_then(|v| v.as_bool()) {
            settings.enabled = enabled;
        }
        if let Some(mode) = value
            .get("downloadHandoffMode")
            .and_then(|v| v.as_str())
            .and_then(DownloadHandoffMode::from_protocol)
        {
            settings.download_handoff_mode = mode;
        }
        if let Some(v) = value.get("contextMenuEnabled").and_then(|v| v.as_bool()) {
            settings.context_menu_enabled = v;
        }
        if let Some(v) = value
            .get("showProgressAfterHandoff")
            .and_then(|v| v.as_bool())
        {
            settings.show_progress_after_handoff = v;
        }
        if let Some(v) = value.get("showBadgeStatus").and_then(|v| v.as_bool()) {
            settings.show_badge_status = v;
        }
        if let Some(hosts) = value.get("excludedHosts").and_then(|v| v.as_array()) {
            settings.excluded_hosts = hosts
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
        }
        if let Some(exts) = value
            .get("capturedFileExtensions")
            .and_then(|v| v.as_array())
        {
            settings.captured_file_extensions = exts
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
        }
        if let Some(v) = value
            .get("downloadCaptureDebugLogging")
            .and_then(|v| v.as_bool())
        {
            settings.download_capture_debug_logging = v;
        }
        settings.sanitize();
        Ok(settings)
    }
}

fn normalize_hosts(hosts: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for host in hosts {
        let normalized = normalize_host_pattern(host);
        if !normalized.is_empty() && !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    out
}

fn normalize_host_pattern(value: &str) -> String {
    let mut pattern = value.trim().to_ascii_lowercase();
    if let Some(rest) = pattern.strip_prefix("https://") {
        pattern = rest.to_string();
    } else if let Some(rest) = pattern.strip_prefix("http://") {
        pattern = rest.to_string();
    }
    if let Some((host, _)) = pattern.split_once('/') {
        pattern = host.to_string();
    }
    if let Some((host, _)) = pattern.split_once('?') {
        pattern = host.to_string();
    }
    if let Some((host, _)) = pattern.split_once('#') {
        pattern = host.to_string();
    }
    if let Some((host, _)) = pattern.rsplit_once(':') {
        if host.chars().all(|c| c.is_ascii_digit() || c == '.') {
            // keep as-is for IPv4:port edge — strip only trailing :port when host has no colons of ipv6
        }
        if !host.contains(':')
            && pattern[host.len() + 1..]
                .chars()
                .all(|c| c.is_ascii_digit())
        {
            pattern = host.to_string();
        }
    }
    pattern = pattern.trim_matches('.').to_string();
    if pattern.is_empty()
        || pattern.contains('/')
        || pattern.contains('\\')
        || pattern.contains(char::is_whitespace)
        || !pattern.chars().any(|c| c.is_ascii_alphanumeric())
        || !pattern
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '*' | '-'))
    {
        return String::new();
    }
    pattern
}

fn normalize_extensions(values: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        for candidate in value.split(|c: char| c == ',' || c.is_whitespace()) {
            let mut ext = candidate
                .trim()
                .trim_start_matches('.')
                .to_ascii_lowercase();
            if ext == "7zip" {
                ext = "7z".into();
            }
            if ext.is_empty()
                || ext.contains('/')
                || ext.contains('\\')
                || ext.chars().all(|c| c == '.')
            {
                continue;
            }
            if !out.contains(&ext) {
                out.push(ext);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_roundtrip() {
        let settings = ExtensionIntegrationSettings::default();
        let json = settings.to_protocol_json();
        let parsed = ExtensionIntegrationSettings::from_protocol_json(&json).unwrap();
        assert_eq!(parsed.enabled, settings.enabled);
        assert_eq!(parsed.download_handoff_mode, DownloadHandoffMode::Ask);
        assert!(parsed.captured_file_extensions.contains(&"zip".into()));
    }
}
