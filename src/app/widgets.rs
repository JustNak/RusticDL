use gpui::{
    div, hsla, linear_color_stop, linear_gradient, prelude::FluentBuilder, px, App, Context,
    Entity, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement, PathPromptOptions,
    SharedString, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{
    h_flex, input::InputState, progress::Progress, tag::Tag, tooltip::Tooltip, v_flex, ActiveTheme,
    Icon, IconName, Sizable, StyledExt, Theme,
};
use std::path::PathBuf;

use super::filter::FilterKind;
use super::layout::{
    QueueColumns, COL_ACTIONS_W, COL_DATE_W, COL_ETA_W, COL_SIZE_W, COL_SPEED_W, STATUS_DOT,
};
use super::settings_category::SettingsCategory;
use super::DownloadApp;
use crate::settings::{AccentPreset, ProgressStyle, SortColumn, SortDirection};

/// Soft edge vignette using four linear-gradient strips.
pub(crate) fn render_vignette_overlay(edge_alpha: f32, is_dark: bool) -> impl IntoElement {
    let a = edge_alpha.clamp(0.0, 0.5);
    let edge = if is_dark {
        hsla(0.0, 0.0, 0.0, a)
    } else {
        hsla(0.0, 0.0, 0.08, a * 0.85)
    };
    let clear = hsla(0.0, 0.0, 0.0, 0.0);
    let band = px(96.);

    div()
        .absolute()
        .inset_0()
        .size_full()
        // Top
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(band)
                .bg(linear_gradient(
                    180.0,
                    linear_color_stop(edge, 0.0),
                    linear_color_stop(clear, 1.0),
                )),
        )
        // Bottom
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(band)
                .bg(linear_gradient(
                    0.0,
                    linear_color_stop(edge, 0.0),
                    linear_color_stop(clear, 1.0),
                )),
        )
        // Left
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(band)
                .bg(linear_gradient(
                    90.0,
                    linear_color_stop(edge, 0.0),
                    linear_color_stop(clear, 1.0),
                )),
        )
        // Right
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(band)
                .bg(linear_gradient(
                    270.0,
                    linear_color_stop(edge, 0.0),
                    linear_color_stop(clear, 1.0),
                )),
        )
}

/// Progress bar variants for queue rows and settings preview.
/// `value` is 0..100.
pub(crate) fn styled_progress(value: f32, color: Hsla, style: ProgressStyle) -> impl IntoElement {
    let value = value.clamp(0.0, 100.0);
    match style {
        ProgressStyle::Solid => Progress::new()
            .value(value)
            .bg(color)
            .h(px(6.))
            .w_full()
            .rounded_full()
            .into_any_element(),
        ProgressStyle::Soft => Progress::new()
            .value(value)
            .bg(color.opacity(0.85))
            .h(px(4.))
            .w_full()
            .rounded_full()
            .into_any_element(),
        ProgressStyle::Glow => Progress::new()
            .value(value)
            .bg(color)
            .h(px(9.))
            .w_full()
            .rounded_full()
            .into_any_element(),
        ProgressStyle::Segmented => {
            const SEGMENTS: u32 = 12;
            let filled = ((value / 100.0) * SEGMENTS as f32).round() as u32;
            h_flex()
                .w_full()
                .gap_0p5()
                .h(px(8.))
                .items_center()
                .children((0..SEGMENTS).map(move |i| {
                    let on = i < filled;
                    div().flex_1().h_full().rounded(px(2.)).bg(if on {
                        color
                    } else {
                        color.opacity(0.16)
                    })
                }))
                .into_any_element()
        }
    }
}

/// Decorative icon badge for empty / search-empty states.
pub(crate) fn empty_state_badge(
    icon: IconName,
    icon_color: Hsla,
    fill: Hsla,
    ring: Hsla,
    reduce_motion: bool,
) -> impl IntoElement {
    let outer = if reduce_motion { 56.0 } else { 64.0 };
    let inner = if reduce_motion { 44.0 } else { 48.0 };
    div()
        .w(px(outer))
        .h(px(outer))
        .rounded_full()
        .border_1()
        .border_color(ring)
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(inner))
                .h(px(inner))
                .rounded_full()
                .bg(fill)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).with_size(px(22.)).text_color(icon_color)),
        )
}

/// Compact path for secondary UI hints (e.g. Advanced row preview).
pub(crate) fn shorten_path_display(path: &str) -> String {
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

/// Open the platform folder picker and write the chosen path into `input`.
///
/// Uses GPUI's native path prompt (with a proper parent HWND on Windows) instead
/// of `rfd`, which often fails silently or opens behind the app window.
pub(crate) fn browse_directory(
    input: Entity<InputState>,
    app_view: Entity<DownloadApp>,
    window: &mut Window,
    cx: &mut App,
) {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(SharedString::from("Select Folder")),
    });

    window
        .spawn(cx, async move |cx| match rx.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let _ = cx.update(|window, cx| {
                        input.update(cx, |state, cx| {
                            state.set_value(path.to_string_lossy().to_string(), window, cx);
                        });
                    });
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(err)) => {
                let _ = app_view.update(cx, |app, cx| {
                    app.show_error_toast(format!("Could not open folder picker: {err}"), cx);
                });
            }
            Err(_) => {}
        })
        .detach();
}

/// Field title — stronger than hints so forms scan as Label → control → help.
/// Used by add dialog and other compact forms (`text_xs`).
pub(crate) fn field_label(text: &'static str, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_xs()
        .font_semibold()
        .text_color(theme.foreground)
        .child(text)
}

/// Supporting description under a field. Kept smaller/softer than `field_label`.
pub(crate) fn field_hint(text: impl Into<SharedString>, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_xs()
        .font_normal()
        .text_color(theme.muted_foreground.opacity(0.78))
        .child(text.into())
}

// ── Settings layout helpers (settings panels only; leave add-dialog labels alone) ──

/// Settings field label: `text_sm` semibold so hierarchy beats muted hints.
pub(crate) fn settings_field_label(text: &'static str, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_sm()
        .font_semibold()
        .text_color(theme.foreground)
        .child(text)
}

/// Sub-group eyebrow (e.g. NOTIFICATIONS). Optional top hairline divider.
pub(crate) fn settings_subgroup(
    title: &'static str,
    with_divider: bool,
    cx: &mut App,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let eyebrow: SharedString = title.to_ascii_uppercase().into();
    v_flex()
        .w_full()
        .gap_2()
        .when(with_divider, |el| {
            el.child(div().w_full().h(px(1.)).bg(theme.border.opacity(0.55)))
        })
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(theme.muted_foreground)
                .child(eyebrow),
        )
}

/// Horizontal toggle/choice row: label (+ optional hint) left, control cluster right.
///
/// Control side is width-capped so multi-button clusters with `.flex_wrap()` can
/// wrap under Compact density / narrow content instead of overflowing the label.
pub(crate) fn settings_choice_row(
    label: &'static str,
    hint: Option<&'static str>,
    control: impl IntoElement,
    cx: &mut App,
) -> impl IntoElement {
    // Label left, control cluster right. Allow the control column to grow so
    // multi-button groups (density / radius / progress) do not crush and
    // stack on top of each other in a narrow pane.
    h_flex()
        .w_full()
        .gap_3()
        .items_start()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .min_w(px(120.))
                .gap_0p5()
                .pt_1()
                .child(settings_field_label(label, cx))
                .when_some(hint, |el, text| el.child(field_hint(text, cx))),
        )
        .child(div().flex_shrink_0().max_w(px(360.)).child(control))
}

/// Equal-size circular preset swatch (solid fill + selection ring).
pub(crate) fn accent_preset_swatch(
    preset: AccentPreset,
    selected: bool,
    swatch: Hsla,
    theme: &Theme,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let label = preset.label();
    let tip: SharedString = if preset == AccentPreset::Default {
        "Default — stock theme color".into()
    } else {
        label.to_string().into()
    };
    // Light fills (stock dark primary is often near-white) need a stronger edge
    // so they don't dissolve into the selection ring or the panel.
    let light_fill = swatch.l > 0.72;
    let fill_border = if selected {
        if light_fill {
            theme.foreground.opacity(0.35)
        } else {
            theme.background.opacity(0.35)
        }
    } else if light_fill {
        theme.border.opacity(0.85)
    } else {
        theme.border.opacity(0.45)
    };
    div()
        .id(SharedString::from(format!("accent-{label}")))
        .size(px(32.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .border_2()
        .border_color(if selected {
            // Darker ring when the fill itself is light so selection stays obvious.
            if light_fill {
                theme.muted_foreground.opacity(0.95)
            } else {
                theme.foreground.opacity(0.92)
            }
        } else {
            theme.border.opacity(0.0)
        })
        .when(!selected, |el| {
            el.hover(|s| {
                s.border_color(theme.muted_foreground.opacity(0.55))
                    .bg(theme.secondary.opacity(0.4))
            })
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.set_accent_preset(preset, window, cx);
        }))
        .child(
            div()
                .size(px(20.))
                .rounded_full()
                .bg(swatch)
                .border_1()
                .border_color(fill_border),
        )
}

/// Custom mixer entry: white disc + paintbrush — clearly not a solid preset.
pub(crate) fn accent_custom_swatch(
    selected: bool,
    _custom_color: Hsla,
    theme: &Theme,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let tip: SharedString = "Custom — mix your own accent".into();
    // White plate always; brush in dark ink so it stays readable on light/dark UI.
    let plate = hsla(0.0, 0.0, 0.98, 1.0);
    let brush = hsla(0.0, 0.0, 0.22, 1.0);

    div()
        .id("accent-Custom")
        .size(px(32.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .border_2()
        .border_color(if selected {
            theme.foreground.opacity(0.92)
        } else {
            theme.border.opacity(0.0)
        })
        .when(!selected, |el| {
            el.hover(|s| {
                s.border_color(theme.muted_foreground.opacity(0.55))
                    .bg(theme.secondary.opacity(0.4))
            })
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(|this, _, window, cx| {
            this.set_accent_preset(AccentPreset::Custom, window, cx);
        }))
        .child(
            div()
                .size(px(20.))
                .rounded_full()
                .bg(plate)
                .border_1()
                .border_color(theme.border.opacity(0.5))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::empty()
                        .path("icons/paintbrush.svg")
                        .with_size(px(12.))
                        .text_color(brush),
                ),
        )
}

pub(crate) fn accent_hsl_slider_row(
    label: &'static str,
    value: String,
    slider: impl IntoElement,
    theme: &Theme,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .w_full()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child(label),
                )
                .child(
                    div()
                        .text_xs()
                        .font_medium()
                        .text_color(theme.muted_foreground.opacity(0.85))
                        .child(value),
                ),
        )
        .child(slider)
}

pub(crate) fn status_chip(text: String, color: Hsla) -> impl IntoElement {
    div().text_xs().font_medium().text_color(color).child(text)
}

/// Clickable queue column header with asc/desc indicator for the active sort.
/// `center` centers the label (and sort chevron) in fixed-width metric columns.
pub(crate) fn sortable_header(
    label: &'static str,
    column: SortColumn,
    flex: bool,
    width: Option<gpui::Pixels>,
    center: bool,
    active_column: SortColumn,
    direction: SortDirection,
    theme: &gpui_component::Theme,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let active = active_column == column;
    let color = if active {
        theme.foreground
    } else {
        theme.muted_foreground
    };
    let tip: SharedString = if active {
        match direction {
            SortDirection::Asc => {
                format!("Sorted by {label} · ascending (click to reverse)").into()
            }
            SortDirection::Desc => {
                format!("Sorted by {label} · descending (click to reverse)").into()
            }
        }
    } else {
        format!("Sort by {label}").into()
    };

    h_flex()
        .id(SharedString::from(format!("sort-header-{label}")))
        .when(flex, |d| d.flex_1().min_w_0())
        .when_some(width, |d, w| d.w(w).flex_shrink_0())
        .gap_0p5()
        .items_center()
        .when(center, |d| d.justify_center())
        .cursor_pointer()
        .rounded(theme.radius)
        .hover(|s| s.bg(theme.secondary.opacity(0.45)))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_sort_column(column, cx);
        }))
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(color)
                .child(label),
        )
        .when(active, |el| {
            el.child(
                Icon::new(match direction {
                    SortDirection::Asc => IconName::ChevronUp,
                    SortDirection::Desc => IconName::ChevronDown,
                })
                .with_size(px(12.))
                .text_color(theme.primary),
            )
        })
}

/// Fixed-width metric cell; content is centered under the column header.
pub(crate) fn metric_cell(
    width: f32,
    text: impl Into<SharedString>,
    color: Hsla,
    medium: bool,
) -> impl IntoElement {
    h_flex()
        .w(px(width))
        .flex_shrink_0()
        .justify_center()
        .items_center()
        .overflow_hidden()
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_xs()
                .when(medium, |d| d.font_medium())
                .text_color(color)
                .child(text.into()),
        )
}

/// Format a sidebar nav count for display. Caps at 999 (`999+` beyond that).
pub(crate) fn format_nav_count(count: i32) -> SharedString {
    if count > 999 {
        "999+".into()
    } else {
        count.to_string().into()
    }
}

pub(crate) fn nav_item(
    label: &'static str,
    filter: FilterKind,
    count: i32,
    active: bool,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    // Selected filter: subtle grey highlight. Unselected: plain (white/default surface).
    let bg = if active {
        theme
            .secondary
            .opacity(if theme.is_dark() { 0.55 } else { 0.85 })
    } else {
        theme.transparent
    };
    let fg = if active {
        theme.sidebar_accent_foreground
    } else {
        theme.sidebar_foreground
    };
    let icon_color = if active {
        theme.sidebar_primary
    } else {
        theme.muted_foreground
    };
    // Count text: muted grey when selected, brighter when not.
    let count_color = if active {
        theme.muted_foreground
    } else {
        theme.sidebar_foreground.opacity(0.9)
    };

    h_flex()
        .id(SharedString::from(format!("nav-{label}")))
        .h(px(36.))
        .px_2()
        .gap_2()
        .items_center()
        .rounded(theme.radius)
        .bg(bg)
        .hover(|s| {
            s.bg(if active {
                theme
                    .secondary
                    .opacity(if theme.is_dark() { 0.65 } else { 0.95 })
            } else {
                theme.secondary.opacity(0.45)
            })
        })
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, window, cx| {
            this.select_filter(filter, window, cx);
        }))
        .child(
            Icon::new(filter.nav_icon())
                .with_size(px(15.))
                .text_color(icon_color),
        )
        .child(
            div()
                .flex_1()
                .text_sm()
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .text_color(fg)
                .child(label),
        )
        .when(count >= 0, |el| {
            el.child(
                div()
                    .text_xs()
                    .font_medium()
                    .text_color(count_color)
                    .child(format_nav_count(count)),
            )
        })
}

/// Settings mini-nav row: icon + label, active styling aligned with [`nav_item`].
/// No badge counts (categories are not queue filters).
pub(crate) fn settings_nav_item(
    category: SettingsCategory,
    active: bool,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let label = category.label();
    let bg = if active {
        theme
            .secondary
            .opacity(if theme.is_dark() { 0.55 } else { 0.85 })
    } else {
        theme.transparent
    };
    let fg = if active {
        theme.sidebar_accent_foreground
    } else {
        theme.sidebar_foreground
    };
    let icon_color = if active {
        theme.sidebar_primary
    } else {
        theme.muted_foreground
    };

    h_flex()
        .id(SharedString::from(format!("settings-nav-{label}")))
        .h(px(36.))
        .px_2()
        .gap_2()
        .items_center()
        .rounded(theme.radius)
        .bg(bg)
        .hover(|s| {
            s.bg(if active {
                theme
                    .secondary
                    .opacity(if theme.is_dark() { 0.65 } else { 0.95 })
            } else {
                theme.secondary.opacity(0.45)
            })
        })
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            if this.settings_category != category {
                this.settings_category = category;
                cx.notify();
            }
        }))
        .child(
            Icon::new(category.icon())
                .with_size(px(15.))
                .text_color(icon_color),
        )
        .child(
            div()
                .flex_1()
                .text_sm()
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .text_color(fg)
                .child(label),
        )
}

pub(crate) fn status_color(tone: i32, theme: &gpui_component::Theme) -> Hsla {
    match tone {
        1 => theme.primary,
        2 => theme.success,
        3 => theme.warning,
        4 => theme.danger,
        _ => theme.muted_foreground,
    }
}

pub(crate) fn status_tag(status: &'static str, tone: i32) -> Tag {
    // Text badge kept for the detail panel only.
    match tone {
        1 => Tag::primary().small().child(status),
        2 => Tag::success().small().child(status),
        3 => Tag::warning().small().child(status),
        4 => Tag::danger().small().child(status),
        _ => Tag::secondary().small().child(status),
    }
}

/// Compact circular status indicator. Hover shows the full status label.
pub(crate) fn status_dot(
    job_id: &str,
    status: &'static str,
    color: Hsla,
    tip_color: Hsla,
) -> impl IntoElement {
    let label: SharedString = status.into();
    div()
        .id(SharedString::from(format!("status-dot-{job_id}")))
        .flex_shrink_0()
        .w(px(STATUS_DOT))
        .h(px(STATUS_DOT))
        .rounded_full()
        .bg(color)
        .border_1()
        .border_color(color.opacity(0.45))
        .tooltip(move |window, cx| soft_tooltip(label.clone(), tip_color, window, cx))
}

/// Smaller, muted tooltip used for status dots and full filenames.
pub(crate) fn soft_tooltip(
    text: SharedString,
    tip_color: Hsla,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyView {
    Tooltip::new(text)
        .text_xs()
        .font_normal()
        .text_color(tip_color)
        .py_0()
        .px_1p5()
        .build(window, cx)
}

/// Approximate how many characters fit in the Name column (text-sm / semibold).
pub(crate) fn name_char_budget(main_w: f32, cols: QueueColumns) -> usize {
    // Row chrome always present: padding + status dot + size + actions + gaps.
    let mut used = 32.0 + STATUS_DOT + COL_SIZE_W + COL_ACTIONS_W + 12.0 * 5.0;
    if cols.date {
        used += COL_DATE_W + 12.0;
    }
    if cols.speed {
        used += COL_SPEED_W + 12.0;
    }
    if cols.eta {
        used += COL_ETA_W + 12.0;
    }
    let name_px = (main_w - used).max(96.0);
    // ~8px average advance for semibold text-sm on Windows.
    ((name_px / 8.0) as usize).clamp(16, 200)
}

/// Force a visible "..." when the label is longer than the name column can show.
/// (GPUI's text-overflow ellipsis is unreliable for this flex layout.)
pub(crate) fn ellipsize_name(name: &str, max_chars: usize) -> SharedString {
    let count = name.chars().count();
    if count <= max_chars {
        return SharedString::from(name.to_string());
    }
    let keep = max_chars.saturating_sub(3).max(1);
    let head: String = name.chars().take(keep).collect();
    SharedString::from(format!("{head}..."))
}

#[cfg(test)]
mod tests {
    use super::format_nav_count;

    #[test]
    fn nav_count_shows_exact_value_up_to_999() {
        assert_eq!(format_nav_count(0).as_ref(), "0");
        assert_eq!(format_nav_count(42).as_ref(), "42");
        assert_eq!(format_nav_count(999).as_ref(), "999");
    }

    #[test]
    fn nav_count_caps_above_999() {
        assert_eq!(format_nav_count(1000).as_ref(), "999+");
        assert_eq!(format_nav_count(12_345).as_ref(), "999+");
    }
}
