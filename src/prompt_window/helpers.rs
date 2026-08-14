//! Shared display helpers for the capture HUD.
//!
//! Keep local `capture_progress_bar` and `shorten_path` (do not unify with
//! queue `styled_progress` / widgets path helpers).

use std::path::{Path, PathBuf};

use gpui::{
    canvas, div, fill, point, prelude::FluentBuilder, px, size, Bounds, Corners, Hsla,
    InteractiveElement, IntoElement, ParentElement, PathBuilder, Styled, Window,
};
use gpui_component::{h_flex, progress::Progress, v_flex, StyledExt};

use super::SPEED_SAMPLE_CAP;
use crate::format::format_speed;
use crate::settings::ProgressStyle;

/// Visible columns in the capture speed graph (history is averaged down to this).
const SPARK_COLUMNS: usize = 40;
/// Keep the plot inside the clip rect so AA does not bleed past the card.
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
    session_peak: u64,
    bar_color: Hsla,
    muted: Hsla,
    theme: &gpui_component::Theme,
    reduce_motion: bool,
    status: &'static str,
) -> impl IntoElement {
    let has_data = samples.iter().any(|&s| s > 0);
    let columns = spark_columns(samples, SPARK_COLUMNS);
    let visible_max = columns.iter().filter_map(|c| *c).max().unwrap_or(0);
    let scale_max = sticky_scale(session_peak, visible_max);
    let avg = visible_average(&columns);
    let current = samples.last().copied().filter(|&s| s > 0);
    let peak = session_peak.max(visible_max);
    let smooth = !reduce_motion;

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
                    h_flex()
                        .gap_2()
                        .items_baseline()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .font_medium()
                                .text_color(muted)
                                .child("Speed"),
                        )
                        .when_some(current, |el, speed| {
                            el.child(div().text_xs().font_medium().child(format_speed(speed)))
                        }),
                )
                .child(div().text_xs().text_color(muted).child(status)),
        )
        .when(has_data && peak > 0, |el| {
            let mut bits = vec![format!("peak {}", format_speed(peak))];
            if let Some(avg) = avg.filter(|&v| v > 0) {
                bits.push(format!("avg {}", format_speed(avg)));
            }
            el.child(
                div()
                    .text_xs()
                    .text_color(muted.opacity(0.85))
                    .child(bits.join(" · ")),
            )
        })
        .child(
            div()
                .id("sparkline-plot")
                .w_full()
                .flex_1()
                .min_h(px(24.))
                .overflow_hidden()
                .child(
                    canvas(
                        move |_, _, _| (),
                        move |bounds, _, window, _| {
                            paint_speed_graph(
                                bounds, &columns, scale_max, avg, bar_color, muted, smooth, window,
                            );
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

/// Never shrink the Y-scale below the session peak so dips stay visible.
fn sticky_scale(session_peak: u64, visible_max: u64) -> u64 {
    session_peak.max(visible_max)
}

/// Mean of populated columns, including zeros so a stall pulls the average down.
fn visible_average(columns: &[Option<u64>]) -> Option<u64> {
    let mut sum = 0u64;
    let mut n = 0u64;
    for slot in columns {
        if let Some(speed) = *slot {
            sum = sum.saturating_add(speed);
            n += 1;
        }
    }
    sum.checked_div(n)
}

/// Populated columns as `(index, speed)`, skipping leading/gap `None`.
fn plot_samples(columns: &[Option<u64>]) -> Vec<(usize, u64)> {
    columns
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| slot.map(|speed| (i, speed)))
        .collect()
}

/// Pixel height for one sample. Zero speed stays on the baseline (no floor wall).
fn bar_height_px(speed: u64, max: u64, usable: f32) -> f32 {
    if speed == 0 || max == 0 || usable <= 0.0 {
        return 0.0;
    }
    let h = (speed as f32 / max as f32) * usable;
    h.clamp(1.0, usable)
}

fn append_series(builder: &mut PathBuilder, points: &[(f32, f32)], smooth: bool) {
    if points.is_empty() {
        return;
    }
    if !smooth || points.len() < 3 {
        for &(x, y) in points.iter().skip(1) {
            builder.line_to(point(px(x), px(y)));
        }
        return;
    }
    for i in 0..points.len() - 1 {
        let (x0, y0) = points[i];
        let (x1, y1) = points[i + 1];
        let mx = (x0 + x1) * 0.5;
        let my = (y0 + y1) * 0.5;
        if i == 0 {
            builder.line_to(point(px(mx), px(my)));
        } else {
            builder.curve_to(point(px(mx), px(my)), point(px(x0), px(y0)));
        }
    }
    let (lx, ly) = points[points.len() - 1];
    builder.line_to(point(px(lx), px(ly)));
}

fn paint_h_line(window: &mut Window, x: f32, y: f32, w: f32, color: Hsla) {
    window.paint_quad(fill(
        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(w), px(1.0)),
        },
        color,
    ));
}

fn paint_speed_graph(
    bounds: Bounds<gpui::Pixels>,
    columns: &[Option<u64>],
    scale_max: u64,
    avg: Option<u64>,
    line_color: Hsla,
    muted: Hsla,
    smooth: bool,
    window: &mut Window,
) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    if width < 4.0 || height < 4.0 {
        return;
    }

    let n = columns.len().max(1) as f32;
    let inset = SPARK_INSET;
    let inner_w = (width - inset * 2.0).max(1.0);
    let usable_h = (height - inset * 2.0).max(1.0);
    let origin_x = f32::from(bounds.origin.x);
    let origin_y = f32::from(bounds.origin.y);
    let x0 = origin_x + inset;
    let base_y = origin_y + height - inset;
    let plot_h = (usable_h - 1.0).max(1.0);
    let step = inner_w / n;
    let peak = scale_max.max(1);

    // Guides first so the series sits on top.
    paint_h_line(window, x0, base_y - 1.0, inner_w, muted.opacity(0.42));
    paint_h_line(
        window,
        x0,
        base_y - 1.0 - plot_h * 0.5,
        inner_w,
        muted.opacity(0.18),
    );
    paint_h_line(
        window,
        x0,
        base_y - 1.0 - plot_h,
        inner_w,
        muted.opacity(0.22),
    );

    if let Some(avg) = avg.filter(|&v| v > 0) {
        let avg_h = bar_height_px(avg, peak, plot_h);
        if avg_h > 0.0 {
            let mut dash = PathBuilder::stroke(px(1.0)).dash_array(&[px(3.0), px(3.0)]);
            let y = base_y - 1.0 - avg_h;
            dash.move_to(point(px(x0), px(y)));
            dash.line_to(point(px(x0 + inner_w), px(y)));
            if let Ok(path) = dash.build() {
                window.paint_path(path, muted.opacity(0.55));
            }
        }
    }

    let plotted = plot_samples(columns);
    if plotted.is_empty() {
        return;
    }

    let points: Vec<(f32, f32)> = plotted
        .iter()
        .map(|&(i, speed)| {
            let x = x0 + (i as f32 + 0.5) * step;
            let y = base_y - 1.0 - bar_height_px(speed, peak, plot_h);
            (x, y)
        })
        .collect();

    let (first_x, first_y) = points[0];
    let (last_x, last_y) = points[points.len() - 1];

    let mut area = PathBuilder::fill();
    area.move_to(point(px(first_x), px(base_y - 1.0)));
    area.line_to(point(px(first_x), px(first_y)));
    append_series(&mut area, &points, smooth);
    area.line_to(point(px(last_x), px(base_y - 1.0)));
    area.close();
    if let Ok(path) = area.build() {
        window.paint_path(path, line_color.opacity(0.16));
    }

    let mut stroke = PathBuilder::stroke(px(1.5));
    stroke.move_to(point(px(first_x), px(first_y)));
    append_series(&mut stroke, &points, smooth);
    if let Ok(path) = stroke.build() {
        window.paint_path(path, line_color);
    }

    // Live tip — last sample.
    window.paint_quad(
        fill(
            Bounds {
                origin: point(px(last_x - 2.5), px(last_y - 2.5)),
                size: size(px(5.0), px(5.0)),
            },
            line_color,
        )
        .corner_radii(Corners {
            top_left: px(2.5),
            top_right: px(2.5),
            bottom_right: px(2.5),
            bottom_left: px(2.5),
        }),
    );
}

pub(super) fn default_prompt_filename(prompt: &crate::ipc::BrowserPromptView) -> String {
    prompt
        .suggested_filename
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| {
            crate::download::filesystem::derive_filename_from_url(&prompt.url)
                .unwrap_or_else(|| "download.bin".into())
        })
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

/// Last 1–2 folder segments of a file's parent directory (never the filename).
pub(super) fn shorten_folder(path: &Path) -> String {
    let Some(parent) = path.parent() else {
        return "default folder".into();
    };
    if parent.as_os_str().is_empty() {
        return "default folder".into();
    }
    let parts: Vec<_> = parent
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let sep = std::path::MAIN_SEPARATOR;
    match parts.as_slice() {
        [] => parent.to_string_lossy().into_owned(),
        [one] => one.clone(),
        [.., grand, leaf] => format!("{grand}{sep}{leaf}"),
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
        assert!(bar_height_px(1, 100, 40.0) >= 1.0);
        assert!(bar_height_px(1, 100, 40.0) <= 40.0);
    }

    #[test]
    fn sticky_scale_does_not_shrink_below_session_peak() {
        assert_eq!(sticky_scale(25_000_000, 5_000_000), 25_000_000);
        assert_eq!(sticky_scale(5_000_000, 8_000_000), 8_000_000);
        assert_eq!(sticky_scale(0, 0), 0);
    }

    #[test]
    fn visible_average_includes_zeros_and_skips_none() {
        let cols = vec![None, Some(10), Some(0), Some(20)];
        assert_eq!(visible_average(&cols), Some(10));
        assert_eq!(visible_average(&[None, None]), None);
    }

    #[test]
    fn plot_samples_skips_none_and_keeps_last() {
        let cols = vec![None, None, Some(10), Some(20), Some(5)];
        let plotted = plot_samples(&cols);
        assert_eq!(plotted, vec![(2, 10), (3, 20), (4, 5)]);
        assert_eq!(plotted.last().copied(), Some((4, 5)));
    }

    #[test]
    fn shorten_folder_drops_filename() {
        let path = PathBuf::from(r"C:\Users\Zeus\Downloads\show.s01e01.mkv");
        let folder = shorten_folder(&path);
        assert!(!folder.to_lowercase().contains("show"));
        assert!(folder.contains("Downloads") || folder.contains("Zeus"));
    }

    #[test]
    fn truncate_middle_keeps_extension() {
        let name = "[ToonsHub] Tomb Raider King S01E01 1080p BILI WEB-DL AAC2.0 H.265 (Dog).mkv";
        let out = truncate_middle(name, 40);
        assert!(out.contains('…'));
        assert!(out.ends_with(".mkv"));
        assert!(out.chars().count() <= 40);
    }
}
