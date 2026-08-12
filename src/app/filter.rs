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
