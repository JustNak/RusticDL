//! Shared display helpers for the capture HUD.
//!
//! Keep local `capture_progress_bar` and `shorten_path` (do not unify with
//! queue `styled_progress` / widgets path helpers).

use std::path::PathBuf;

use gpui::{
    div, prelude::FluentBuilder, px, Hsla, InteractiveElement, IntoElement, ParentElement, Styled,
};
use gpui_component::{h_flex, progress::Progress, v_flex, StyledExt};

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

/// Compact bar sparkline of recent throughput (bytes/sec samples).
pub(super) fn speed_sparkline(
    samples: &[u64],
    bar_color: Hsla,
    muted: Hsla,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    let max = samples.iter().copied().max().unwrap_or(0).max(1);
    let has_data = samples.iter().any(|&s| s > 0);

    v_flex()
        .id("capture-speed-sparkline")
        .flex_1()
        .min_h(px(56.))
        .w_full()
        .gap_1()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border.opacity(0.55))
        .bg(theme.secondary.opacity(0.35))
        .px_2()
        .py_1p5()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .font_medium()
                        .text_color(muted)
                        .child("Speed"),
                )
                .child(div().text_xs().text_color(muted).child(if has_data {
                    "live"
                } else {
                    "waiting…"
                })),
        )
        .child(
            h_flex()
                .id("sparkline-bars")
                .flex_1()
                .w_full()
                .min_h(px(36.))
                .items_end()
                .gap_px()
                .when(!has_data, |el| {
                    el.child(
                        div()
                            .flex_1()
                            .h(px(2.))
                            .rounded_full()
                            .bg(muted.opacity(0.35)),
                    )
                })
                .when(has_data, |el| {
                    el.children(samples.iter().enumerate().map(|(i, &speed)| {
                        let t = (speed as f32 / max as f32).clamp(0.08, 1.0);
                        let h = (36.0 * t).max(2.0);
                        let latest = i + 1 == samples.len();
                        div()
                            .flex_1()
                            .min_w(px(1.))
                            .h(px(h))
                            .rounded(px(1.))
                            .bg(if latest {
                                bar_color
                            } else {
                                bar_color.opacity(0.45 + 0.4 * t)
                            })
                    }))
                }),
        )
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
