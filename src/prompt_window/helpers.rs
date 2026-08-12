//! Shared display helpers for the capture HUD.
//!
//! Keep local `capture_progress_bar` and `shorten_path` (do not unify with
//! queue `styled_progress` / widgets path helpers).

use std::path::PathBuf;

use gpui::{px, Hsla, IntoElement, Styled};
use gpui_component::progress::Progress;

use crate::settings::ProgressStyle;

pub(super) fn capture_progress_bar(
    value: f32,
    color: Hsla,
    style: ProgressStyle,
) -> impl IntoElement {
    let value = value.clamp(0.0, 100.0);
    let height = match style {
        ProgressStyle::Soft => px(4.),
        ProgressStyle::Glow => px(9.),
        ProgressStyle::Solid | ProgressStyle::Segmented => px(6.),
    };
    Progress::new()
        .value(value)
        .bg(color)
        .h(height)
        .w_full()
        .rounded_full()
}

pub(super) fn truncate_middle(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    if max_chars < 8 {
        return value.chars().take(max_chars).collect();
    }
    let keep = (max_chars - 1) / 2;
    let head: String = value.chars().take(keep).collect();
    let tail: String = value
        .chars()
        .rev()
        .take(max_chars - keep - 1)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

pub(super) fn shorten_path(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() {
        return "default folder".into();
    }
    let buf = PathBuf::from(path);
    let parts: Vec<_> = buf
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let sep = std::path::MAIN_SEPARATOR;
    match parts.as_slice() {
        [] => path.to_string(),
        [one] => one.clone(),
        [.., parent, leaf] => format!("{parent}{sep}{leaf}"),
    }
}
