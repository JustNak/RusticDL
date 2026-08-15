use gpui::{
    div, prelude::FluentBuilder, px, Context, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled,
};
use gpui_component::{h_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt};

use super::super::filter::FilterKind;
use super::super::settings_category::SettingsCategory;
use super::super::DownloadApp;
use crate::download::FileTypeKind;

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

/// Parent "All downloads" row with a chevron that expands the type tree.
pub(crate) fn library_parent_nav(
    count: i32,
    active: bool,
    expanded: bool,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
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
    let count_color = if active {
        theme.muted_foreground
    } else {
        theme.sidebar_foreground.opacity(0.9)
    };

    h_flex()
        .id("nav-All downloads")
        .h(px(36.))
        .px_2()
        .gap_1()
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
        .child(
            h_flex()
                .id("nav-library-toggle")
                .size(px(18.))
                .items_center()
                .justify_center()
                .rounded(theme.radius)
                .cursor_pointer()
                .hover(|s| s.bg(theme.secondary.opacity(0.55)))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_sidebar_library(cx);
                }))
                .child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .with_size(px(12.))
                    .text_color(theme.muted_foreground),
                ),
        )
        .child(
            h_flex()
                .id("nav-All downloads-select")
                .flex_1()
                .h_full()
                .gap_2()
                .items_center()
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.select_filter(FilterKind::All, window, cx);
                }))
                .child(
                    Icon::new(FilterKind::All.nav_icon())
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
                        .child("All downloads"),
                )
                .when(count >= 0, |el| {
                    el.child(
                        div()
                            .text_xs()
                            .font_medium()
                            .text_color(count_color)
                            .child(format_nav_count(count)),
                    )
                }),
        )
}

/// Indented type row under All downloads.
pub(crate) fn type_nav_item(
    kind: FileTypeKind,
    count: i32,
    active: bool,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
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
    let count_color = if active {
        theme.muted_foreground
    } else {
        theme.sidebar_foreground.opacity(0.9)
    };
    let label = kind.label();

    h_flex()
        .id(SharedString::from(format!("nav-type-{label}")))
        .h(px(32.))
        .pl(px(22.))
        .pr_2()
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
            this.select_filter(FilterKind::FileType(kind), window, cx);
        }))
        .child(
            Icon::empty()
                .path(kind.icon_path())
                .with_size(px(14.))
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
        .child(
            div()
                .text_xs()
                .font_medium()
                .text_color(count_color)
                .child(format_nav_count(count)),
        )
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
