//! Title bar chrome extracted from `DownloadApp`.

use gpui::{
    div, img, prelude::FluentBuilder, px, Context, Corner, InteractiveElement, IntoElement,
    ObjectFit, ParentElement, Styled, StyledImage,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    ActiveTheme, Icon, IconName, TitleBar,
};

use super::filter::FilterKind;
use super::DownloadApp;
use crate::branding::{APP_LOGO_DARK, APP_LOGO_LIGHT, APP_NAME, APP_VERSION};
use crate::format::format_speed;
use crate::format::total_download_speed;
use crate::updater::{open_release_page, open_url};

impl DownloadApp {
    pub(crate) fn render_title_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let show_actions = self.filter != FilterKind::Settings;
        let update_busy = self.update_busy;
        let update_action_label = self.update_action_label();
        let release_page_url = self
            .available_update
            .as_ref()
            .map(|info| info.html_url.clone());
        let view = cx.entity();
        let filtered_count = self.filtered_count();
        let total_speed = total_download_speed(&self.jobs);
        let context_label = if self.filter == FilterKind::Settings {
            format!("Settings · {}", self.settings_category.label())
        } else if total_speed > 0 {
            format!("↓ {}", format_speed(total_speed))
        } else if filtered_count > 0 {
            format!(
                "{} · {}",
                self.filter.title(),
                self.filter.subtitle(filtered_count)
            )
        } else {
            String::new()
        };

        TitleBar::new().h(px(48.)).child(
            h_flex()
                .id("title-bar-content")
                .w_full()
                .h_full()
                .items_center()
                .gap_3()
                .pr_1()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .flex_shrink_0()
                        .child({
                            // Brand mark switches with theme so the glyph stays readable.
                            let logo = if theme.is_dark() {
                                APP_LOGO_DARK
                            } else {
                                APP_LOGO_LIGHT
                            };
                            div()
                                .w(px(26.))
                                .h(px(26.))
                                .rounded(theme.radius)
                                .overflow_hidden()
                                .flex_shrink_0()
                                .child(img(logo).w(px(26.)).h(px(26.)).object_fit(ObjectFit::Cover))
                        })
                        .child(
                            // Clickable product name → overflow menu (updates).
                            Button::new("app-brand-menu")
                                .ghost()
                                .label(APP_NAME)
                                .tooltip("App menu")
                                .dropdown_menu_with_anchor(
                                    Corner::BottomLeft,
                                    move |menu, _window, _menu_cx| {
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
                                                    move |_, _window, cx| {
                                                        view.update(cx, |app, cx| {
                                                            app.begin_one_click_update(cx);
                                                        });
                                                    }
                                                }),
                                            )
                                            .separator()
                                            .item(
                                                PopupMenuItem::new("Open releases on GitHub")
                                                    .icon(IconName::ExternalLink)
                                                    .on_click({
                                                        let release_page_url =
                                                            release_page_url.clone();
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
                                                PopupMenuItem::new(format!(
                                                    "About {APP_NAME}…  v{APP_VERSION}"
                                                ))
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
                                    },
                                ),
                        )
                        .when(!context_label.is_empty(), |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(context_label),
                            )
                        }),
                )
                .child(div().flex_1())
                .when(show_actions, |el| {
                    el.child(
                        Button::new("add-download")
                            .primary()
                            .icon(IconName::Plus)
                            .label("Add download")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_add_dialog(window, cx);
                            })),
                    )
                }),
        )
    }
}
