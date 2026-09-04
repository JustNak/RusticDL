use gpui::{
    div, img, prelude::FluentBuilder, px, Context, Corner, InteractiveElement, IntoElement,
    ObjectFit, ParentElement, StatefulInteractiveElement, Styled, StyledImage, WindowControlArea,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    divider::Divider,
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt,
};

use super::filter::FilterKind;
use super::layout::sidebar_on_right;
use super::settings_category::SettingsCategory;
use super::widgets::{library_parent_nav, nav_item, settings_nav_item, type_nav_item};
use super::DownloadApp;
use crate::branding::{APP_LOGO_DARK, APP_LOGO_LIGHT, APP_NAME, APP_VERSION};
use crate::download::FileTypeKind;
use crate::format::{count_jobs, count_jobs_by_type};
use crate::updater::{open_release_page, open_url};

/// Matches the main-pane title bar so brand and toolbar share one horizontal band.
const SIDEBAR_BRAND_H: f32 = 48.0;

impl DownloadApp {
    fn sidebar_frame(&self, cx: &mut Context<Self>) -> gpui::Div {
        let theme = cx.theme().clone();
        let sidebar_w = self.settings.ui_density.sidebar_w();
        let on_right = sidebar_on_right();
        v_flex()
            .w(px(sidebar_w))
            .flex_shrink_0()
            .h_full()
            .bg(theme.sidebar)
            .when(on_right, |el| el.border_l_1())
            .when(!on_right, |el| el.border_r_1())
            .border_color(theme.sidebar_border)
            .child(self.render_sidebar_brand(cx))
    }

    fn render_sidebar_brand(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let update_busy = self.update_busy;
        let update_action_label = self.update_action_label();
        let release_page_url = self
            .available_update
            .as_ref()
            .map(|info| info.html_url.clone());
        let view = cx.entity();
        let logo = if theme.is_dark() {
            APP_LOGO_DARK
        } else {
            APP_LOGO_LIGHT
        };
        let brand_menu = {
            let view = view.clone();
            move |menu: gpui_component::menu::PopupMenu,
                  _window: &mut gpui::Window,
                  _menu_cx: &mut gpui::Context<gpui_component::menu::PopupMenu>| {
                let view = view.clone();
                menu.min_w(px(200.))
                    .item(
                        PopupMenuItem::new(if update_busy {
                            "Updating…".to_string()
                        } else {
                            update_action_label.clone()
                        })
                        .icon(Icon::empty().path("icons/rotate-cw.svg"))
                        .disabled(update_busy)
                        .on_click({
                            let view = view.clone();
                            move |_, window, cx| {
                                view.update(cx, |app, cx| {
                                    app.begin_update_action(window, cx);
                                });
                            }
                        }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new("Open releases on GitHub")
                            .icon(IconName::ExternalLink)
                            .on_click({
                                let release_page_url = release_page_url.clone();
                                move |_, _, _| {
                                    if let Some(url) = &release_page_url {
                                        let _ = open_url(url);
                                    } else {
                                        let _ = open_release_page();
                                    }
                                }
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(format!("About {APP_NAME}…  v{APP_VERSION}"))
                            .icon(IconName::Info)
                            .on_click({
                                let view = view.clone();
                                move |_, window, cx| {
                                    view.update(cx, |app, cx| {
                                        app.open_about_dialog(window, cx);
                                    });
                                }
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new("Exit")
                            .icon(IconName::WindowClose)
                            .on_click({
                                let view = view.clone();
                                move |_, _, cx| {
                                    view.update(cx, |app, cx| {
                                        app.force_quit_app(cx);
                                    });
                                }
                            }),
                    )
            }
        };

        h_flex()
            .id("sidebar-brand")
            .h(px(SIDEBAR_BRAND_H))
            .px_3()
            .flex_shrink_0()
            .items_center()
            .w_full()
            .overflow_hidden()
            .window_control_area(WindowControlArea::Drag)
            .child(
                Button::new("app-brand-menu")
                    .ghost()
                    .compact()
                    .tooltip("App menu")
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .w(px(32.))
                                    .h(px(32.))
                                    .rounded(theme.radius)
                                    .overflow_hidden()
                                    .flex_shrink_0()
                                    .child(
                                        img(logo)
                                            .w(px(32.))
                                            .h(px(32.))
                                            .object_fit(ObjectFit::Cover),
                                    ),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(theme.sidebar_foreground)
                                    .overflow_hidden()
                                    .child(APP_NAME),
                            ),
                    )
                    .dropdown_menu_with_anchor(
                        if sidebar_on_right() {
                            Corner::BottomRight
                        } else {
                            Corner::BottomLeft
                        },
                        brand_menu,
                    ),
            )
    }

    pub(crate) fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (all, active, completed, failed) = count_jobs(&self.jobs);
        let type_counts = count_jobs_by_type(&self.jobs);
        let theme = cx.theme().clone();
        let filter = self.filter;
        let library_expanded = self.settings.sidebar_library_expanded;
        let all_active = filter == FilterKind::All;

        self.sidebar_frame(cx).child(
            v_flex()
                .flex_1()
                .min_h_0()
                .p_3()
                .pt_1()
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
                ),
        )
    }

    pub(crate) fn render_settings_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let category = self.settings_category;

        self.sidebar_frame(cx).id("settings-sidebar").child(
            v_flex()
                .flex_1()
                .min_h_0()
                .p_3()
                .pt_1()
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
                ),
        )
    }
}
