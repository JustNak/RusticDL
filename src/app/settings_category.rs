use gpui_component::IconName;

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

    pub(crate) fn panel_title(self) -> &'static str {
        match self {
            Self::Browser => "Browser capture",
            other => other.label(),
        }
    }
}
