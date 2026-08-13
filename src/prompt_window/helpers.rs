//! Shared display helpers for the capture HUD.
//!
//! Keep local `capture_progress_bar` and `shorten_path` (do not unify with
//! queue `styled_progress` / widgets path helpers).

use std::path::PathBuf;

use gpui::{
    canvas, div, fill, point, px, size, Bounds, Corners, Hsla, InteractiveElement, IntoElement,
    ParentElement, Styled, Window,
};
use gpui_component::{h_flex, progress::Progress, v_flex, StyledExt};

use super::SPEED_SAMPLE_CAP;
use crate::settings::ProgressStyle;

/// Visible columns in the capture speed graph (history is averaged down to this).
const SPARK_COLUMNS: usize = 40;
/// Air between columns. Wider than 1px so bars do not fuse into a block.
const SPARK_GAP: f32 = 2.0;
/// Keep quads inside the clip rect so AA does not bleed past the card.
const SPARK_INSET: f32 = 1.0;

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

/// Compact throughput graph of recent bytes/sec samples.
pub(super) fn speed_sparkline(
    samples: &[u64],
    bar_color: Hsla,
    muted: Hsla,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    let has_data = samples.iter().any(|&s| s > 0);
    let columns = spark_columns(samples, SPARK_COLUMNS);
    let max = samples.iter().copied().max().unwrap_or(0);

    v_flex()
        .id("capture-speed-sparkline")
        .flex_1()
        .min_h(px(56.))
        .w_full()
        .gap_1()
        .overflow_hidden()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border.opacity(0.55))
        .bg(theme.secondary.opacity(0.35))
        .px_2()
        .py_1p5()
        .child(
            h_flex()
                .w_full()
                .flex_shrink_0()
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
            div()
                .id("sparkline-bars")
                .w_full()
                .flex_1()
                .min_h(px(24.))
                .overflow_hidden()
                .child(
                    canvas(
                        move |_, _, _| (),
                        move |bounds, _, window, _| {
                            paint_speed_graph(bounds, &columns, max, bar_color, muted, window);
                        },
                    )
                    .size_full(),
                ),
        )
}

/// Average `samples` into at most `columns` buckets, preserving order.
fn downsample_avg(samples: &[u64], columns: usize) -> Vec<u64> {
    if columns == 0 || samples.is_empty() {
        return Vec::new();
    }
    if samples.len() <= columns {
        return samples.to_vec();
    }
    let n = samples.len();
    (0..columns)
        .map(|i| {
            // Keep the live tip exact; average the rest so 90 ticks do not become a barcode.
            if i + 1 == columns {
                return samples[n - 1];
            }
            let start = i * n / columns;
            let end = ((i + 1) * n / columns).max(start + 1);
            let chunk = &samples[start..end];
            chunk.iter().sum::<u64>() / chunk.len() as u64
        })
        .collect()
}

/// Right-align history so column width stays stable as samples accrue.
fn pad_left(samples: &[u64], columns: usize) -> Vec<Option<u64>> {
    if columns == 0 {
        return Vec::new();
    }
    let take = samples.len().min(columns);
    let pad = columns - take;
    let mut out = vec![None; columns];
    if take > 0 {
        let src = &samples[samples.len() - take..];
        for (i, &speed) in src.iter().enumerate() {
            out[pad + i] = Some(speed);
        }
    }
    out
}

fn spark_columns(samples: &[u64], columns: usize) -> Vec<Option<u64>> {
    let columns = columns.min(SPEED_SAMPLE_CAP).max(1);
    pad_left(&downsample_avg(samples, columns), columns)
}

/// Pixel height for one column. Zero speed stays on the baseline (no floor wall).
fn bar_height_px(speed: u64, max: u64, usable: f32) -> f32 {
    if speed == 0 || max == 0 || usable <= 0.0 {
        return 0.0;
    }
    let h = (speed as f32 / max as f32) * usable;
    h.clamp(2.0, usable)
}

fn paint_speed_graph(
    bounds: Bounds<gpui::Pixels>,
    columns: &[Option<u64>],
    max: u64,
    bar_color: Hsla,
    muted: Hsla,
    window: &mut Window,
) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    if width < 4.0 || height < 4.0 {
        return;
    }

    let n = columns.len().max(1) as f32;
    let inset = SPARK_INSET;
    let gap = SPARK_GAP;
    let inner_w = (width - inset * 2.0).max(1.0);
    let usable_h = (height - inset * 2.0).max(1.0);
    let bar_w = ((inner_w - gap * (n - 1.0)) / n).floor().max(1.0);
    let used = bar_w * n + gap * (n - 1.0);
    let origin_x = f32::from(bounds.origin.x);
    let origin_y = f32::from(bounds.origin.y);
    let x0 = origin_x + inset + ((inner_w - used) * 0.5).floor().max(0.0);
    let base_y = origin_y + height - inset;

    // Hairline floor — bars sit on this, never on the card border.
    window.paint_quad(fill(
        Bounds {
            origin: point(px(origin_x + inset), px(base_y - 1.0)),
            size: size(px(inner_w), px(1.0)),
        },
        muted.opacity(0.28),
    ));

    let last_i = columns.iter().rposition(|slot| slot.is_some_and(|s| s > 0));
    let peak = max.max(1);
    let plot_h = (usable_h - 1.0).max(1.0);
    let radius = (bar_w * 0.4).min(2.0);

    for (i, slot) in columns.iter().enumerate() {
        let Some(speed) = *slot else {
            continue;
        };
        let bar_h = bar_height_px(speed, peak, plot_h);
        if bar_h <= 0.0 {
            continue;
        }
        let x = x0 + i as f32 * (bar_w + gap);
        let y = base_y - 1.0 - bar_h;
        let t = speed as f32 / peak as f32;
        let color = if last_i == Some(i) {
            bar_color
        } else {
            bar_color.opacity(0.40 + 0.40 * t)
        };
        window.paint_quad(
            fill(
                Bounds {
                    origin: point(px(x), px(y)),
                    size: size(px(bar_w), px(bar_h)),
                },
                color,
            )
            .corner_radii(Corners {
                top_left: px(radius),
                top_right: px(radius),
                bottom_right: px(0.),
                bottom_left: px(0.),
            }),
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_keeps_short_series() {
        let samples = [1, 2, 3];
        assert_eq!(downsample_avg(&samples, 8), vec![1, 2, 3]);
    }

    #[test]
    fn downsample_averages_even_buckets() {
        let samples: Vec<u64> = (1..=8).collect();
        assert_eq!(downsample_avg(&samples, 4), vec![1, 3, 5, 8]);
    }

    #[test]
    fn pad_left_right_aligns_history() {
        let cols = pad_left(&[10, 20], 4);
        assert_eq!(cols, vec![None, None, Some(10), Some(20)]);
    }

    #[test]
    fn spark_columns_caps_and_pads() {
        let samples: Vec<u64> = (1..=90).collect();
        let cols = spark_columns(&samples, SPARK_COLUMNS);
        assert_eq!(cols.len(), SPARK_COLUMNS);
        assert!(cols.iter().all(|c| c.is_some()));
        assert_eq!(cols.last().copied().flatten(), Some(90));
    }

    #[test]
    fn bar_height_zero_stays_on_baseline() {
        assert_eq!(bar_height_px(0, 100, 40.0), 0.0);
        assert_eq!(bar_height_px(50, 0, 40.0), 0.0);
    }

    #[test]
    fn bar_height_peak_fills_track_and_never_exceeds_it() {
        assert_eq!(bar_height_px(100, 100, 40.0), 40.0);
        assert!(bar_height_px(1, 100, 40.0) >= 2.0);
        assert!(bar_height_px(1, 100, 40.0) <= 40.0);
    }
}
