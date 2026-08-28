use serde::{Deserialize, Serialize};

use crate::branding::APP_VERSION;

/// When to show OS (tray balloon) notifications for terminal downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OsNotifyMode {
    /// Only when the main window is hidden to the tray (recommended).
    #[default]
    WhenHiddenToTray,
    /// Always fire OS notification (subject to tray availability).
    Always,
    /// Never use OS notifications.
    Off,
}

impl OsNotifyMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::WhenHiddenToTray => "When hidden",
            Self::Always => "Always",
        }
    }
}

/// Which GitHub Releases stream the auto-updater follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    /// Latest non-prerelease (`/releases/latest`).
    #[default]
    Stable,
    /// Newest published `vX.Y.Z-nightly.*` GitHub pre-release with a setup asset.
    Nightly,
}

impl UpdateChannel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stable => "Stable",
            Self::Nightly => "Nightly",
        }
    }
}

/// First-run / missing-key default: Nightly when [`APP_VERSION`] contains `-nightly.`.
pub fn update_channel_for_app_version(version: &str) -> UpdateChannel {
    if version.contains("-nightly.") {
        UpdateChannel::Nightly
    } else {
        UpdateChannel::Stable
    }
}

/// Serde + [`Settings::default`] default for [`Settings::update_channel`].
pub fn default_update_channel() -> UpdateChannel {
    update_channel_for_app_version(APP_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_update_channel_nightly_stamp_returns_nightly() {
        let version = "0.3.5-nightly.20260828120000";
        assert!(version.contains("-nightly."));
        assert_eq!(
            update_channel_for_app_version(version),
            UpdateChannel::Nightly
        );
    }

    #[test]
    fn default_update_channel_stable_stamp_returns_stable() {
        let version = "0.3.5";
        assert!(!version.contains("-nightly."));
        assert_eq!(
            update_channel_for_app_version(version),
            UpdateChannel::Stable
        );
    }

    #[test]
    fn existing_settings_json_with_stable_channel_stays_stable() {
        let json = r#"{
            "downloadDirectory": "C:/dl",
            "maxConcurrentDownloads": 2,
            "autoRetryAttempts": 3,
            "speedLimitKibPerSecond": 0,
            "theme": "dark",
            "updateChannel": "stable"
        }"#;
        let settings: crate::settings::Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.update_channel, UpdateChannel::Stable);
        assert!(json.contains(r#""updateChannel": "stable""#));
    }
}
