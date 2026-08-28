use gpui::{
    div, img, prelude::FluentBuilder, px, Context, Corner, InteractiveElement, IntoElement,
    MouseButton, ObjectFit, ParentElement, Styled, StyledImage, WindowControlArea,
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
#[cfg(target_os = "linux")]
use crate::hyprland;
use crate::updater::{open_release_page, open_url};

#[cfg(target_os = "linux")]
fn hyprland_title_bar_drag_region() -> gpui::Stateful<gpui::Div> {
    div()
        .id("title-bar-drag")
        .h_full()
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |_, window, _| {
            window.start_window_move();
        })
}

impl DownloadApp {
    pub(crate) fn render_title_bar(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
        // gpui-component TitleBar insets children (12px on Windows/Linux, 80px
        // on macOS for traffic lights). Size the brand column so the search
        // field starts at the sidebar's right edge.
        #[cfg(target_os = "macos")]
        const TITLE_BAR_LEFT_PAD: f32 = 80.0;
        #[cfg(not(target_os = "macos"))]
        const TITLE_BAR_LEFT_PAD: f32 = 12.0;
        let brand_col_w = (self.settings.ui_density.sidebar_w() - TITLE_BAR_LEFT_PAD).max(80.0);
        #[cfg(target_os = "linux")]
        let on_hyprland = hyprland::is_hyprland();
        #[cfg(not(target_os = "linux"))]
        let on_hyprland = false;
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

        let content = h_flex()
            .id("title-bar-content")
            .w_full()
            .h_full()
            .items_center()
            .gap_2()
            .pr_1()
            .child(
                h_flex()
                    .id("title-bar-brand")
                    .w(px(brand_col_w))
                    .h_full()
                    .flex_shrink_0()
                    .items_center()
                    .overflow_hidden()
                    .gap_2()
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
                    }),
            )
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
                .child(
                    Button::new("add-download")
                        .primary()
                        .icon(IconName::Plus)
                        .label("Add download")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_add_dialog(window, cx);
                        })),
                )
            })
            .when(!show_queue_chrome && !on_hyprland, |el| {
                el.child(div().flex_1())
            })
            .when(!show_queue_chrome && on_hyprland, |el| {
                el.child(hyprland_title_bar_drag_region().flex_1())
            });

        #[cfg(target_os = "linux")]
        if on_hyprland {
            return h_flex()
                .id("title-bar")
                .w_full()
                .h(px(48.))
                .flex_shrink_0()
                .items_center()
                .border_b_1()
                .border_color(theme.title_bar_border)
                .bg(theme.title_bar)
                .when(show_queue_chrome, |bar| {
                    bar.child(
                        hyprland_title_bar_drag_region()
                            .w(px(TITLE_BAR_LEFT_PAD))
                            .flex_shrink_0(),
                    )
                })
                .child(content)
                .into_any_element();
        }

        TitleBar::new().h(px(48.)).child(content).into_any_element()
    }

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

#[cfg(test)]
mod hyprland_title_bar_tests {
    const SOURCE: &str = include_str!("title_bar.rs");
    const PARENT_ID: &str = r#".id("title-bar")"#;
    const DRAG_ID: &str = r#".id("title-bar-drag")"#;
    const DRAG_TOKEN: &str = "WindowControlArea::Drag";
    const MOVE_TOKEN: &str = "start_window_move";

    fn hyprland_branch_source() -> &'static str {
        let start = SOURCE
            .find("if on_hyprland")
            .expect("Hyprland title-bar branch");
        let end = start
            + SOURCE[start..]
                .find(".into_any_element();")
                .expect("Hyprland title-bar return");
        &SOURCE[start..end]
    }

    #[test]
    fn hyprland_parent_title_bar_has_no_drag_or_move() {
        let branch = hyprland_branch_source();
        let parent_start = branch
            .find(PARENT_ID)
            .expect("parent title-bar id in Hyprland branch");
        let parent_scope = &branch[parent_start..];
        let child_offset = parent_scope
            .find(".child(")
            .expect("title-bar child in Hyprland branch");
        let parent_scope = &parent_scope[..child_offset];

        assert!(
            !parent_scope.contains(DRAG_TOKEN),
            "parent title-bar must not use {DRAG_TOKEN}"
        );
        assert!(
            !parent_scope.contains(MOVE_TOKEN),
            "parent title-bar must not call {MOVE_TOKEN}"
        );
    }

    #[test]
    fn hyprland_title_bar_drag_owns_drag_and_move() {
        let helper_start = SOURCE
            .find("fn hyprland_title_bar_drag_region")
            .expect("hyprland title-bar drag helper");
        let helper_scope = &SOURCE[helper_start..];
        let helper_end = helper_scope
            .find("\n}\n")
            .expect("hyprland title-bar drag helper end");
        let helper_scope = &helper_scope[..helper_end];

        assert!(
            helper_scope.contains(DRAG_ID),
            "title-bar-drag id must live on the drag helper"
        );
        assert!(
            helper_scope.contains(DRAG_TOKEN),
            "title-bar-drag must use {DRAG_TOKEN}"
        );
        assert!(
            helper_scope.contains(MOVE_TOKEN),
            "title-bar-drag must call {MOVE_TOKEN}"
        );

        let branch = hyprland_branch_source();
        assert!(
            branch.contains("hyprland_title_bar_drag_region()"),
            "Hyprland branch must mount title-bar-drag via hyprland_title_bar_drag_region"
        );
    }
}
