use gpui_component::IconName;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    All,
    Active,
    Completed,
    Failed,
    Settings,
}

impl FilterKind {
    pub(crate) fn as_index(self) -> i32 {
        match self {
            Self::All => 0,
            Self::Active => 1,
            Self::Completed => 2,
            Self::Failed => 3,
            Self::Settings => 4,
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::All => "All downloads",
            Self::Active => "Active",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Settings => "Settings",
        }
    }

    pub(crate) fn subtitle(self, count: usize) -> String {
        match self {
            Self::Settings => "Preferences and defaults".into(),
            Self::All if count == 0 => "Your download queue is empty".into(),
            Self::All => format!("{count} item{}", if count == 1 { "" } else { "s" }),
            Self::Active if count == 0 => "Nothing in progress".into(),
            Self::Active => format!("{count} active"),
            Self::Completed if count == 0 => "No finished downloads yet".into(),
            Self::Completed => format!("{count} completed"),
            Self::Failed if count == 0 => "No failures".into(),
            Self::Failed => format!("{count} failed or canceled"),
        }
    }

    pub(crate) fn empty_title(self) -> &'static str {
        match self {
            Self::All => "No downloads yet",
            Self::Active => "No active downloads",
            Self::Completed => "No completed downloads",
            Self::Failed => "No failed downloads",
            Self::Settings => "Settings",
        }
    }

    pub(crate) fn empty_body(self) -> &'static str {
        match self {
            Self::All => "Paste an HTTP or HTTPS link to start a transfer.",
            Self::Active => "Queued and in-progress jobs will show up here.",
            Self::Completed => "Finished files will appear in this list.",
            Self::Failed => "Failed or canceled jobs will appear here for retry.",
            Self::Settings => "",
        }
    }

    pub(crate) fn empty_icon(self) -> IconName {
        match self {
            Self::All => IconName::Inbox,
            Self::Active => IconName::LoaderCircle,
            Self::Completed => IconName::CircleCheck,
            Self::Failed => IconName::TriangleAlert,
            Self::Settings => IconName::Settings,
        }
    }

    pub(crate) fn nav_icon(self) -> IconName {
        match self {
            Self::All => IconName::Inbox,
            Self::Active => IconName::ArrowDown,
            Self::Completed => IconName::CircleCheck,
            Self::Failed => IconName::CircleX,
            Self::Settings => IconName::Settings,
        }
    }
}


