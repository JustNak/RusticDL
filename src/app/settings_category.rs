//! Settings mini-nav category enum (shared by shell state and nav widgets).

use gpui_component::IconName;

/// Settings mini-nav categories. Switching does not discard the draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsCategory {
    #[default]
    General,
    DownloadEngine,
    System,
    Browser,
    Appearance,
}

impl SettingsCategory {
    pub(crate) const ALL: [Self; 5] = [
        Self::General,
        Self::DownloadEngine,
        Self::System,
        Self::Browser,
        Self::Appearance,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::DownloadEngine => "Download Engine",
            Self::System => "System",
            Self::Browser => "Browser",
            Self::Appearance => "Appearance",
        }
    }

    pub(crate) fn icon(self) -> IconName {
        match self {
            Self::General => IconName::Folder,
            Self::DownloadEngine => IconName::ArrowDown,
            Self::System => IconName::Settings,
            Self::Browser => IconName::ExternalLink,
            Self::Appearance => IconName::Palette,
        }
    }

    /// GroupBox title (Browser keeps the longer “Browser capture” name).
    pub(crate) fn panel_title(self) -> &'static str {
        match self {
            Self::Browser => "Browser capture",
            other => other.label(),
        }
    }
}
