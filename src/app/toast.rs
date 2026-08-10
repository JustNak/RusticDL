use std::time::Duration;

use gpui::SharedString;

/// In-app toast (bottom-right). gpui-component's Notification layer is fixed top-right.
pub(crate) const TOAST_AUTO_HIDE: Duration = Duration::from_secs(5);
pub(crate) const TOAST_MAX_STACK: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastKind {
    Info,
    Error,
}

#[derive(Debug, Clone)]
pub(crate) struct Toast {
    pub id: u64,
    pub message: SharedString,
    pub kind: ToastKind,
}
