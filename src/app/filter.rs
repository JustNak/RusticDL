use gpui_component::IconName;

use crate::download::FileTypeKind;
use crate::format::QueueFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    All,
    Active,
    Completed,
    Failed,
    Settings,
    FileType(FileTypeKind),
}

impl FilterKind {
    pub(crate) fn queue_filter(self) -> QueueFilter {
        match self {
            Self::All | Self::Settings => QueueFilter::All,
            Self::Active => QueueFilter::Active,
            Self::Completed => QueueFilter::Completed,
            Self::Failed => QueueFilter::Failed,
            Self::FileType(kind) => QueueFilter::FileType(kind),
        }
    }

    pub(crate) fn nav_icon(self) -> IconName {
        match self {
            Self::All | Self::FileType(_) => IconName::Inbox,
            Self::Active => IconName::ArrowDown,
            Self::Completed => IconName::CircleCheck,
            Self::Failed => IconName::CircleX,
            Self::Settings => IconName::Settings,
        }
    }
}
