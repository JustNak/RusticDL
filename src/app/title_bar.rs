//! Title bar chrome extracted from `DownloadApp`.

use gpui::{
    div, img, prelude::FluentBuilder, px, Context, Corner, InteractiveElement, IntoElement,
    ObjectFit, ParentElement, Styled, StyledImage,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    menu::{DropdownMenu, PopupMenuItem},
    ActiveTheme, Icon, IconName, Sizable, StyledExt, TitleBar,
};

use super::filter::FilterKind;
use super::DownloadApp;
use crate::branding::{APP_LOGO_DARK, APP_LOGO_LIGHT, APP_NAME, APP_VERSION};
use crate::download::{EngineCommand, JobState};
use crate::format::format_speed;
use crate::format::total_download_speed;
use crate::updater::{open_release_page, open_url};

impl DownloadApp {
    pub(crate) fn render_title_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let show_queue_chrome = self.filter != FilterKind::Settings;
        let update_busy = self.update_busy;
        let update_action_label = self.update_action_label();
        let release_page_url = self
            .available_update
            .as_ref()
            .map(|info| info.html_url.clone());
        let view = cx.entity();
        let total_speed = total_download_speed(&self.jobs);
        // Only show live throughput — filter count lives in the sidebar badge.
        let speed_label = if total_speed > 0 {
            Some(format!("↓ {}", format_speed(total_speed)))
        } else {
            None
        };

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
                                        // Full quit (close-to-tray must not intercept).
                                        app.force_quit_app(cx);
                                    });
                                }
                            }),
                    )
            }
        };

        TitleBar::new().h(px(48.)).child(
            h_flex()
                .id("title-bar-content")
                .w_full()
                .h_full()
                .items_center()
                .gap_2()
                .pr_1()
                // Brand mark + name share one overflow menu (logo is clickable too).
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
                                        .text_color(theme.foreground)
                                        .child(APP_NAME),
                                ),
                        )
                        .dropdown_menu_with_anchor(Corner::BottomLeft, brand_menu),
                )
                .when_some(speed_label, |el, label| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .flex_shrink_0()
                            .child(label),
                    )
                })
                .when(show_queue_chrome, |el| {
                    el.child(
                        div().flex_1().min_w(px(180.)).max_w(px(420.)).child(
                            Input::new(&self.search_input).w_full().prefix(
                                Icon::new(IconName::Search)
                                    .with_size(px(14.))
                                    .text_color(theme.muted_foreground),
                            ),
                        ),
                    )
                    .child(
                        Button::new("queue-overflow")
                            .ghost()
                            .icon(IconName::EllipsisVertical)
                            .tooltip("More actions")
                            .dropdown_menu_with_anchor(
                                Corner::BottomRight,
                                Self::queue_overflow_menu_builder(view.clone()),
                            ),
                    )
                })
                .when(!show_queue_chrome, |el| el.child(div().flex_1()))
                .when(show_queue_chrome, |el| {
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

    /// Shared builder for the queue ⋮ overflow (title bar).
    fn queue_overflow_menu_builder(
        view: gpui::Entity<Self>,
    ) -> impl Fn(
        gpui_component::menu::PopupMenu,
        &mut gpui::Window,
        &mut gpui::Context<gpui_component::menu::PopupMenu>,
    ) -> gpui_component::menu::PopupMenu
           + 'static {
        move |menu, _window, menu_cx| {
            let app = view.read(menu_cx);
            let can_pause = app.jobs.iter().any(|j| {
                matches!(
                    j.state,
                    JobState::Queued | JobState::Starting | JobState::Downloading
                )
            });
            let can_resume = app.jobs.iter().any(|j| j.state == JobState::Paused);
            let can_retry = app
                .jobs
                .iter()
                .any(|j| matches!(j.state, JobState::Failed | JobState::Canceled));
            let can_clear = app.jobs.iter().any(|j| j.state.is_terminal());
            let engine = app.engine.clone();

            menu.min_w(px(196.))
                .item(
                    PopupMenuItem::new("Pause all")
                        .icon(IconName::Minus)
                        .disabled(!can_pause)
                        .on_click({
                            let engine = engine.clone();
                            move |_, _, _| {
                                engine.send(EngineCommand::PauseAll);
                            }
                        }),
                )
                .item(
                    PopupMenuItem::new("Resume all")
                        .icon(IconName::Redo2)
                        .disabled(!can_resume)
                        .on_click({
                            let engine = engine.clone();
                            move |_, _, _| {
                                engine.send(EngineCommand::ResumeAll);
                            }
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Retry all")
                        .icon(IconName::Redo)
                        .disabled(!can_retry)
                        .on_click({
                            let engine = engine.clone();
                            move |_, _, _| {
                                engine.send(EngineCommand::RetryAll);
                            }
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Clear all")
                        .icon(IconName::Delete)
                        .disabled(!can_clear)
                        .on_click({
                            let view = view.clone();
                            move |_, window, cx| {
                                let _ = view.update(cx, |app, cx| {
                                    app.confirm_clear_all(window, cx);
                                });
                            }
                        }),
                )
        }
    }
}
