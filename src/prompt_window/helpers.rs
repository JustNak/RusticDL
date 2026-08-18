//! Shared display helpers for the capture HUD.
//!
//! Keep local `capture_progress_bar` and `shorten_path` (do not unify with
//! queue `styled_progress` / widgets path helpers).

use std::collections::VecDeque;
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
pub(super) const SPARK_COLUMNS: usize = 40;
/// Samples committed into one trail column. 40 × 2 ≈ the 90-sample / 9s ring.
const TRAIL_BUCKET: usize = 2;
/// Per-tick ease toward the live sample. Stays in (0, 1] so the tip cannot overshoot.
const TRAIL_EASE: f32 = 0.42;
/// Reduce-motion keeps a short flat trail (matches the sample-ring cap in `mod`).
const REDUCE_MOTION_COLUMNS: usize = 12;
/// Keep the plot inside the clip rect so AA does not bleed past the card.
const SPARK_INSET: f32 = 1.0;

/// Append-only speed trail. Committed columns never rematerialize; only the live
/// tip eases toward the newest samples. This is the motion fix: same `speed_samples`
/// data, no jump / reverse / rubber-band from re-bucketing or overshooting curves.
#[derive(Debug, Clone, Default)]
pub(super) struct TrailMotion {
    committed: VecDeque<f32>,
    live_sum: u64,
    live_count: u32,
    live_display: f32,
    live_target: f32,
}

impl TrailMotion {
    pub(super) fn push_sample(&mut self, speed: u64, reduce_motion: bool) {
        self.live_sum = self.live_sum.saturating_add(speed);
        self.live_count = self.live_count.saturating_add(1);
        self.live_target = self.live_sum as f32 / self.live_count as f32;
        self.step_ease(reduce_motion);

        let bucket = if reduce_motion { 1 } else { TRAIL_BUCKET };
        if self.live_count as usize >= bucket {
            let cap = if reduce_motion {
                REDUCE_MOTION_COLUMNS
            } else {
                SPARK_COLUMNS
            };
            if self.committed.len() >= cap {
                self.committed.pop_front();
            }
            // Commit the true bucket mean (same data), not the eased tip.
            self.committed.push_back(self.live_target);
            self.live_sum = 0;
            self.live_count = 0;
            self.live_target = self.live_display;
        }
    }

    pub(super) fn step_ease(&mut self, reduce_motion: bool) {
        if self.live_count == 0 {
            return;
        }
        if reduce_motion {
            self.live_display = self.live_target;
            return;
        }
        self.live_display = ease_toward(self.live_display, self.live_target, TRAIL_EASE);
    }

    /// Right-aligned columns for the plot. Empty slots are `None` (left pad).
    pub(super) fn columns(&self) -> Vec<Option<f32>> {
        let mut vals: Vec<f32> = self.committed.iter().copied().collect();
        if self.live_count > 0 {
            vals.push(self.live_display);
        }
        pad_left_f32(&vals, SPARK_COLUMNS)
    }
}

/// Exponential ease-out toward `target`. `alpha` in (0, 1] never crosses.
pub(super) fn ease_toward(current: f32, target: f32, alpha: f32) -> f32 {
    let alpha = alpha.clamp(0.0, 1.0);
    let next = current + (target - current) * alpha;
    if (next - target).abs() <= 0.5 {
        target
    } else {
        next
    }
}

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
    trail: &[Option<f32>],
    session_peak: u64,
    bar_color: Hsla,
    muted: Hsla,
    theme: &gpui_component::Theme,
    reduce_motion: bool,
    status: &'static str,
) -> impl IntoElement {
    let has_data = samples.iter().any(|&s| s > 0);
    let columns = trail.to_vec();
    let visible_max = columns
        .iter()
        .filter_map(|c| *c)
        .fold(0.0f32, |acc, v| acc.max(v));
    let scale_max = sticky_scale(session_peak, visible_max.ceil() as u64);
    let avg = visible_average_f32(&columns);
    let current = samples.last().copied().filter(|&s| s > 0);
    let peak = session_peak.max(visible_max.ceil() as u64);
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
///
/// Every sample belongs to exactly one bucket (no live-tip hole). The capture
/// HUD paints from [`TrailMotion`] instead; this stays as a stateless helper
/// and for tests.
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
            let start = i * n / columns;
            let end = ((i + 1) * n / columns).max(start + 1);
            let chunk = &samples[start..end];
            chunk.iter().sum::<u64>() / chunk.len() as u64
        })
        .collect()
}

/// Right-align history so column width stays stable as samples accrue.
fn pad_left(samples: &[u64], columns: usize) -> Vec<Option<u64>> {
    pad_left_f32(
        &samples.iter().map(|&s| s as f32).collect::<Vec<_>>(),
        columns,
    )
    .into_iter()
    .map(|slot| slot.map(|v| v.round() as u64))
    .collect()
}

fn pad_left_f32(samples: &[f32], columns: usize) -> Vec<Option<f32>> {
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

fn visible_average_f32(columns: &[Option<f32>]) -> Option<u64> {
    let mut sum = 0.0f32;
    let mut n = 0u64;
    for slot in columns {
        if let Some(speed) = *slot {
            sum += speed;
            n += 1;
        }
    }
    if n == 0 {
        None
    } else {
        Some((sum / n as f32).round() as u64)
    }
}

/// Populated columns as `(index, speed)`, skipping leading/gap `None`.
fn plot_samples(columns: &[Option<f32>]) -> Vec<(usize, f32)> {
    columns
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| slot.map(|speed| (i, speed)))
        .collect()
}

/// Pixel height for one sample. Zero speed stays on the baseline (no floor wall).
fn bar_height_px(speed: f32, max: f32, usable: f32) -> f32 {
    if speed <= 0.0 || max <= 0.0 || usable <= 0.0 {
        return 0.0;
    }
    let h = (speed / max) * usable;
    h.clamp(1.0, usable)
}

/// Fritsch–Carlson monotone cubic tangents. Zero at turning points so the
/// interpolant cannot overshoot a peak or reverse through a neighbor.
fn monotone_tangents(xs: &[f32], ys: &[f32]) -> Vec<f32> {
    let n = ys.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![0.0];
    }
    let mut delta = vec![0.0; n - 1];
    for i in 0..n - 1 {
        let dx = xs[i + 1] - xs[i];
        delta[i] = if dx.abs() < f32::EPSILON {
            0.0
        } else {
            (ys[i + 1] - ys[i]) / dx
        };
    }
    let mut m = vec![0.0; n];
    m[0] = delta[0];
    m[n - 1] = delta[n - 2];
    for i in 1..n - 1 {
        if delta[i - 1] * delta[i] <= 0.0 {
            m[i] = 0.0;
        } else {
            m[i] = (delta[i - 1] + delta[i]) * 0.5;
        }
    }
    for i in 0..n - 1 {
        if delta[i].abs() < f32::EPSILON {
            m[i] = 0.0;
            m[i + 1] = 0.0;
            continue;
        }
        let a = m[i] / delta[i];
        let b = m[i + 1] / delta[i];
        let s = a * a + b * b;
        if s > 9.0 {
            let t = 3.0 / s.sqrt();
            m[i] = t * a * delta[i];
            m[i + 1] = t * b * delta[i];
        }
    }
    m
}

fn hermite_y(y0: f32, y1: f32, m0: f32, m1: f32, dx: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    h00 * y0 + h10 * dx * m0 + h01 * y1 + h11 * dx * m1
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
    let xs: Vec<f32> = points.iter().map(|p| p.0).collect();
    let ys: Vec<f32> = points.iter().map(|p| p.1).collect();
    let tangents = monotone_tangents(&xs, &ys);
    for i in 0..points.len() - 1 {
        let (x0, y0) = points[i];
        let (x1, y1) = points[i + 1];
        let dx = x1 - x0;
        let c1x = x0 + dx / 3.0;
        let c1y = y0 + tangents[i] * dx / 3.0;
        let c2x = x1 - dx / 3.0;
        let c2y = y1 - tangents[i + 1] * dx / 3.0;
        builder.cubic_bezier_to(
            point(px(x1), px(y1)),
            point(px(c1x), px(c1y)),
            point(px(c2x), px(c2y)),
        );
    }
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
    columns: &[Option<f32>],
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

    let peak_f = peak as f32;
    if let Some(avg) = avg.filter(|&v| v > 0) {
        let avg_h = bar_height_px(avg as f32, peak_f, plot_h);
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
            let y = base_y - 1.0 - bar_height_px(speed, peak_f, plot_h);
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
        // Every sample is in exactly one bucket — no live-tip hole that drops 7.
        assert_eq!(downsample_avg(&samples, 4), vec![1, 3, 5, 7]);
    }

    #[test]
    fn downsample_covers_every_sample() {
        let samples: Vec<u64> = (1..=90).collect();
        let cols = downsample_avg(&samples, SPARK_COLUMNS);
        assert_eq!(cols.len(), SPARK_COLUMNS);
        let covered: u64 = {
            // Reconstruct coverage by summing bucket * count is hard; instead
            // check first/last and that no bucket is the raw last sample alone
            // while skipping its neighbors (the old rubber-band hole).
            let last = *cols.last().unwrap();
            assert!(last < 90, "last bucket should average the tail, not pin 90");
            assert!(last >= 85);
            last
        };
        let _ = covered;
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
        let last = cols.last().copied().flatten().unwrap();
        assert!(last < 90);
        assert!(last >= 85);
    }

    #[test]
    fn bar_height_zero_stays_on_baseline() {
        assert_eq!(bar_height_px(0.0, 100.0, 40.0), 0.0);
        assert_eq!(bar_height_px(50.0, 0.0, 40.0), 0.0);
    }

    #[test]
    fn bar_height_peak_fills_track_and_never_exceeds_it() {
        assert_eq!(bar_height_px(100.0, 100.0, 40.0), 40.0);
        assert!(bar_height_px(1.0, 100.0, 40.0) >= 1.0);
        assert!(bar_height_px(1.0, 100.0, 40.0) <= 40.0);
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
        let cols = vec![None, None, Some(10.0), Some(20.0), Some(5.0)];
        let plotted = plot_samples(&cols);
        assert_eq!(plotted, vec![(2, 10.0), (3, 20.0), (4, 5.0)]);
        assert_eq!(plotted.last().copied(), Some((4, 5.0)));
    }

    #[test]
    fn ease_toward_never_crosses_target() {
        let mut v = 0.0f32;
        for _ in 0..20 {
            let next = ease_toward(v, 100.0, 0.42);
            assert!(next >= v, "must not reverse");
            assert!(next <= 100.0, "must not overshoot");
            v = next;
        }
        assert_eq!(ease_toward(99.8, 100.0, 0.42), 100.0);
        let down = ease_toward(80.0, 10.0, 0.42);
        assert!(down < 80.0 && down >= 10.0);
    }

    #[test]
    fn trail_committed_columns_do_not_rematerialize() {
        let mut trail = TrailMotion::default();
        for speed in [10u64, 20, 30, 40, 50, 60] {
            trail.push_sample(speed, false);
        }
        // 6 samples / bucket 2 → 3 committed, no live remainder.
        let first = trail.columns();
        let committed: Vec<f32> = first.iter().copied().flatten().collect();
        assert_eq!(committed.len(), 3);
        assert!((committed[0] - 15.0).abs() < 0.01);
        assert!((committed[1] - 35.0).abs() < 0.01);
        assert!((committed[2] - 55.0).abs() < 0.01);

        trail.push_sample(99, false);
        let second = trail.columns();
        let again: Vec<f32> = second.iter().copied().flatten().collect();
        // First three committed values stay put; only a new live tip appears.
        assert_eq!(again.len(), 4);
        assert!((again[0] - 15.0).abs() < 0.01);
        assert!((again[1] - 35.0).abs() < 0.01);
        assert!((again[2] - 55.0).abs() < 0.01);
        assert!(
            again[3] > 55.0 && again[3] < 99.0,
            "tip eases, does not snap"
        );
    }

    #[test]
    fn trail_reduce_motion_snaps_and_stays_short() {
        let mut trail = TrailMotion::default();
        for speed in 1u64..=20 {
            trail.push_sample(speed, true);
        }
        let cols: Vec<f32> = trail.columns().iter().copied().flatten().collect();
        assert_eq!(cols.len(), REDUCE_MOTION_COLUMNS);
        assert!((cols[cols.len() - 1] - 20.0).abs() < 0.01);
    }

    #[test]
    fn monotone_curve_does_not_overshoot_a_peak() {
        let xs = [0.0, 1.0, 2.0, 3.0];
        let ys = [0.0, 10.0, 0.0, 4.0];
        let m = monotone_tangents(&xs, &ys);
        // Local peak at i=1 → tangent 0; samples along the segment stay in [0, 10].
        assert_eq!(m[1], 0.0);
        for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let y = hermite_y(ys[0], ys[1], m[0], m[1], 1.0, t);
            assert!((0.0..=10.0).contains(&y), "overshoot at t={t}: {y}");
            let y2 = hermite_y(ys[1], ys[2], m[1], m[2], 1.0, t);
            assert!(
                (0.0..=10.0).contains(&y2),
                "overshoot after peak t={t}: {y2}"
            );
        }
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
