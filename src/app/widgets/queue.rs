use gpui::{
    div, prelude::FluentBuilder, px, Context, Hsla, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled,
};
use gpui_component::{
    h_flex, tag::Tag, tooltip::Tooltip, Icon, IconName, Sizable, StyledExt, Theme,
};

use super::super::layout::{
    QueueColumns, COL_ACTIONS_W, COL_DATE_W, COL_ETA_W, COL_SIZE_W, COL_SPEED_W, FILE_ICON_W,
    STATUS_BADGE,
};
use super::super::DownloadApp;
use super::chrome::soft_tooltip;
use crate::download::FileTypeKind;
use crate::settings::{SortColumn, SortDirection};

pub(crate) fn status_chip(text: String, color: Hsla) -> impl IntoElement {
    div().text_xs().font_medium().text_color(color).child(text)
}

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
    match tone {
        1 => Tag::primary().small().child(status),
        2 => Tag::success().small().child(status),
        3 => Tag::warning().small().child(status),
        4 => Tag::danger().small().child(status),
        _ => Tag::secondary().small().child(status),
    }
}

pub(crate) fn file_extension_label(filename: &str) -> String {
    std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_else(|| "—".into())
}

pub(crate) fn file_type_status_tile(
    job_id: &str,
    filename: &str,
    status: &'static str,
    status_color: Hsla,
    theme: &Theme,
) -> impl IntoElement {
    let kind = FileTypeKind::from_filename(filename);
    let tip: SharedString = format!("{status} · {}", kind.label()).into();
    let tip_color = theme.muted_foreground;
    let tile = px(FILE_ICON_W);
    let badge = px(STATUS_BADGE);
    let icon_color = theme.muted_foreground;
    let fill = theme.secondary.opacity(0.55);
    let ring = theme.border.opacity(0.5);

    div()
        .id(SharedString::from(format!("file-type-{job_id}")))
        .relative()
        .flex_shrink_0()
        .w(tile)
        .h(tile)
        .rounded(theme.radius)
        .bg(fill)
        .border_1()
        .border_color(ring)
        .flex()
        .items_center()
        .justify_center()
        .tooltip(move |window, cx| soft_tooltip(tip.clone(), tip_color, window, cx))
        .child(
            Icon::empty()
                .path(kind.icon_path())
                .with_size(px(14.))
                .text_color(icon_color),
        )
        .child(
            div()
                .absolute()
                .right(px(-2.))
                .bottom(px(-2.))
                .w(badge)
                .h(badge)
                .rounded_full()
                .bg(status_color)
                .border_1()
                .border_color(theme.background.opacity(0.9)),
        )
}

pub(crate) fn name_char_budget(main_w: f32, cols: QueueColumns) -> usize {
    let mut used = 32.0 + FILE_ICON_W + COL_SIZE_W + COL_ACTIONS_W + 12.0 * 5.0;
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
    ((name_px / 8.0) as usize).clamp(16, 200)
}

pub(crate) fn detail_name_char_budget(main_w: f32) -> usize {
    let used = 40.0 + 16.0 + 96.0 + 28.0 + 24.0;
    let name_px = (main_w - used).max(200.0);
    ((name_px / 8.0) as usize).clamp(40, 280)
}

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
    use super::{
        detail_name_char_budget, ellipsize_name, file_extension_label, name_char_budget,
        QueueColumns,
    };
    use crate::download::FileTypeKind;

    #[test]
    fn ellipsize_keeps_short_names() {
        assert_eq!(ellipsize_name("readme.txt", 16).as_ref(), "readme.txt");
    }

    #[test]
    fn ellipsize_truncates_long_release_names() {
        let name = "HELLMODE.S02E02.A.NEW.FRIEND.1080p.HIDI.WEB-DL.AAC2.0.H.264-VARYG.mkv.zip";
        let out = ellipsize_name(name, 20);
        assert_eq!(out.as_ref(), "HELLMODE.S02E02.A...");
        assert!(out.as_ref().ends_with("..."));
        assert!(out.chars().count() <= 20);
    }

    #[test]
    fn name_budget_stays_usable_when_metrics_are_open() {
        let cols = QueueColumns {
            date: true,
            speed: true,
            eta: true,
        };
        assert!(name_char_budget(900.0, cols) >= 16);
        assert!(name_char_budget(0.0, cols) >= 16);
    }

    #[test]
    fn detail_name_budget_fits_long_release_names() {
        let name = "HELL.MODE.S02E02.A.NEW.FRIEND.1080p.HIDI.WEB-DL.AAC2.0.H.264-VARYG.mkv.zip";
        assert!(detail_name_char_budget(1100.0) >= name.chars().count());
        assert!(detail_name_char_budget(0.0) >= 40);
    }

    #[test]
    fn file_extension_label_uses_last_segment() {
        assert_eq!(file_extension_label("movie.mkv.zip"), "zip");
        assert_eq!(file_extension_label("track.FLAC"), "flac");
        assert_eq!(file_extension_label("song.mp3"), "mp3");
        assert_eq!(file_extension_label("README"), "—");
    }

    #[test]
    fn file_type_kind_from_common_extensions() {
        assert_eq!(FileTypeKind::from_filename("clip.mp4").label(), "Video");
        assert_eq!(FileTypeKind::from_filename("song.mp3").label(), "Audio");
        assert_eq!(
            FileTypeKind::from_filename("pack.zip").label(),
            "Compressed"
        );
        assert_eq!(FileTypeKind::from_filename("notes").label(), "Other");
    }
}
