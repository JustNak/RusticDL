use serde::{Deserialize, Serialize};

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
