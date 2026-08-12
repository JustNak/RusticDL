use gpui::{
    div, prelude::FluentBuilder, px, Context, Hsla, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled,
};
use gpui_component::{
    h_flex, tag::Tag, tooltip::Tooltip, Icon, IconName, Sizable, StyledExt, Theme,
};

use super::super::layout::{
    QueueColumns, COL_ACTIONS_W, COL_DATE_W, COL_ETA_W, COL_SIZE_W, COL_SPEED_W, FILE_ICON_W,
    STATUS_BADGE, STATUS_DOT,
};
use super::super::DownloadApp;
use super::chrome::soft_tooltip;
use crate::settings::{SortColumn, SortDirection};

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

/// File-type group used for queue row icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileTypeKind {
    Archive,
    Video,
    Audio,
    Image,
    Program,
    Document,
    Generic,
}

impl FileTypeKind {
    pub(crate) fn from_filename(filename: &str) -> Self {
        let ext = std::path::Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "lz4" | "zst" | "cab"
            | "iso" => Self::Archive,
            "mkv" | "mp4" | "avi" | "webm" | "mov" | "m4v" | "wmv" | "flv" | "mpeg" | "mpg"
            | "ts" | "m2ts" => Self::Video,
            "mp3" | "flac" | "wav" | "aac" | "m4a" | "ogg" | "opus" | "wma" | "aiff" => Self::Audio,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "tif" | "tiff"
            | "heic" | "avif" => Self::Image,
            "exe" | "msi" | "bat" | "cmd" | "com" | "appx" | "msix" | "dll" | "sys" | "scr"
            | "ps1" | "sh" | "bin" | "run" | "app" | "dmg" | "pkg" | "deb" | "rpm" => Self::Program,
            "pdf" | "doc" | "docx" | "txt" | "md" | "rtf" | "odt" | "xls" | "xlsx" | "ppt"
            | "pptx" | "csv" | "json" | "xml" | "html" | "htm" | "epub" | "mobi" => Self::Document,
            _ => Self::Generic,
        }
    }

    pub(crate) fn icon_path(self) -> &'static str {
        match self {
            Self::Archive => "icons/file-archive.svg",
            Self::Video => "icons/file-video.svg",
            Self::Audio => "icons/file-audio.svg",
            Self::Image => "icons/file-image.svg",
            Self::Program => "icons/file-code.svg",
            Self::Document => "icons/file-text.svg",
            Self::Generic => "icons/file.svg",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Archive => "Archive",
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Image => "Image",
            Self::Program => "Program",
            Self::Document => "Document",
            Self::Generic => "File",
        }
    }
}

/// File-type tile with a status badge overlaid at the bottom-right.
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

/// Approximate how many characters fit in the Name column (text-sm / semibold).
pub(crate) fn name_char_budget(main_w: f32, cols: QueueColumns) -> usize {
    // Row chrome always present: padding + file icon tile + size + actions + gaps.
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
