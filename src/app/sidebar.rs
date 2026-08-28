
use gpui::{
    div, prelude::FluentBuilder, px, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled,
};
use gpui_component::{
    divider::Divider, h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt,
};

use super::filter::FilterKind;
use super::settings_category::SettingsCategory;
use super::widgets::{library_parent_nav, nav_item, settings_nav_item, type_nav_item};
use super::DownloadApp;
use crate::download::FileTypeKind;
use crate::format::{count_jobs, count_jobs_by_type};

impl DownloadApp {
    pub(crate) fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (all, active, completed, failed) = count_jobs(&self.jobs);
        let type_counts = count_jobs_by_type(&self.jobs);
        let theme = cx.theme().clone();
        let filter = self.filter;
        let sidebar_w = self.settings.ui_density.sidebar_w();
        let library_expanded = self.settings.sidebar_library_expanded;
        let all_active = filter == FilterKind::All;

        v_flex()
            .w(px(sidebar_w))
            .flex_shrink_0()
            .h_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .p_3()
            .gap_0p5()
            .child(library_parent_nav(all, all_active, library_expanded, cx))
            .when(library_expanded, |el| {
                el.children(FileTypeKind::ALL.into_iter().map(|kind| {
                    type_nav_item(
                        kind,
                        type_counts[kind.index()],
                        filter == FilterKind::FileType(kind),
                        cx,
                    )
                }))
            })
            .child(nav_item(
                "Active",
                FilterKind::Active,
                active,
                filter == FilterKind::Active,
                cx,
            ))
            .child(nav_item(
                "Completed",
                FilterKind::Completed,
                completed,
                filter == FilterKind::Completed,
                cx,
            ))
            .child(nav_item(
                "Failed",
                FilterKind::Failed,
                failed,
                filter == FilterKind::Failed,
                cx,
            ))
            .child(div().flex_1())
            .child(Divider::horizontal().my_2())
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child("APP"),
            )
            .child(nav_item(
                "Settings",
                FilterKind::Settings,
                -1,
                filter == FilterKind::Settings,
                cx,
            ))
            .child(
                h_flex()
                    .id("nav-about")
                    .h(px(36.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded(theme.radius)
                    .hover(|s| s.bg(theme.secondary.opacity(0.55)))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_about_dialog(window, cx);
                    }))
                    .child(
                        Icon::empty()
                            .path("icons/info.svg")
                            .with_size(px(15.))
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(theme.sidebar_foreground)
                            .child("About"),
                    ),
            )
    }

    pub(crate) fn render_settings_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let sidebar_w = self.settings.ui_density.sidebar_w();
        let category = self.settings_category;

        v_flex()
            .id("settings-sidebar")
            .w(px(sidebar_w))
            .flex_shrink_0()
            .h_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .p_3()
            .gap_0p5()
            .child(
                h_flex()
                    .id("settings-nav-back")
                    .h(px(36.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded(theme.radius)
                    .hover(|s| s.bg(theme.secondary.opacity(0.55)))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.leave_settings(window, cx);
                    }))
                    .child(
                        Icon::new(IconName::ChevronLeft)
                            .with_size(px(15.))
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(theme.sidebar_foreground)
                            .child("Back"),
                    ),
            )
            .child(Divider::horizontal().my_2())
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child("SETTINGS"),
            )
            .children(
                SettingsCategory::ALL
                    .into_iter()
                    .map(|cat| settings_nav_item(cat, category == cat, cx)),
            )
    }
}
