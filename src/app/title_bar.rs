use gpui::{
    div, prelude::FluentBuilder, px, Context, Corner, InteractiveElement, IntoElement,
    ParentElement, Styled, Window,
};
#[cfg(target_os = "linux")]
use gpui::{App, MouseButton, StatefulInteractiveElement, WindowControlArea};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    menu::{DropdownMenu, PopupMenuItem},
    ActiveTheme, Icon, IconName, Sizable, TitleBar,
};

use super::filter::FilterKind;
#[cfg(target_os = "linux")]
use super::layout::sidebar_on_right;
use super::DownloadApp;
use crate::download::{EngineCommand, JobState};
#[cfg(target_os = "linux")]
use crate::hyprland;

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

#[cfg(target_os = "linux")]
fn linux_caption_btn(
    id: &'static str,
    icon: IconName,
    is_close: bool,
    cx: &App,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let theme = cx.theme();
    let hover_bg = if is_close {
        theme.danger
    } else {
        theme.secondary_hover
    };
    let hover_fg = if is_close {
        theme.danger_foreground
    } else {
        theme.secondary_foreground
    };
    let active_bg = if is_close {
        theme.danger_active
    } else {
        theme.secondary_active
    };
    div()
        .id(id)
        .flex()
        .w(px(46.))
        .h_full()
        .flex_shrink_0()
        .justify_center()
        .items_center()
        .text_color(theme.foreground)
        .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
        .active(move |s| s.bg(active_bg).text_color(hover_fg))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
        })
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_click(window, cx);
        })
        .child(Icon::new(icon).small())
}

#[cfg(target_os = "linux")]
fn linux_left_window_controls(window: &Window, cx: &App) -> impl IntoElement {
    let is_max = window.is_maximized();
    h_flex()
        .id("window-controls")
        .flex_shrink_0()
        .h_full()
        .items_center()
        .child(linux_caption_btn(
            "close",
            IconName::WindowClose,
            true,
            cx,
            |window, _| {
                window.remove_window();
            },
        ))
        .child(linux_caption_btn(
            "minimize",
            IconName::WindowMinimize,
            false,
            cx,
            |window, _| {
                window.minimize_window();
            },
        ))
        .child(linux_caption_btn(
            if is_max { "restore" } else { "maximize" },
            if is_max {
                IconName::WindowRestore
            } else {
                IconName::WindowMaximize
            },
            false,
            cx,
            |window, _| {
                window.zoom_window();
            },
        ))
}

impl DownloadApp {
    pub(crate) fn render_title_bar(
        &mut self,
        #[cfg_attr(not(target_os = "linux"), allow(unused))] window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let show_queue_chrome = self.filter != FilterKind::Settings;
        let view = cx.entity();
        #[cfg(target_os = "linux")]
        let chrome_on_left = sidebar_on_right();

        // Main pane is on the left when the sidebar is on the right (macOS /
        // left-button Linux). TitleBar already insets 80px for traffic lights
        // on macOS; Windows / right-button Linux only need a 12px gutter.
        const TITLE_BAR_LEFT_PAD: f32 = 12.0;

        let content = h_flex()
            .id("title-bar-content")
            .w_full()
            .h_full()
            .items_center()
            .gap_2()
            .pr_1()
            .when(show_queue_chrome, |el| {
                el.child(
                    div().flex_1().min_w(px(180.)).child(
                        Input::new(&self.search_input).w_full().prefix(
                            Icon::new(IconName::Search)
                                .with_size(px(14.))
                                .text_color(theme.muted_foreground),
                        ),
                    ),
                )
            });

        #[cfg(target_os = "linux")]
        let on_hyprland = hyprland::is_hyprland();

        #[cfg(target_os = "linux")]
        let content = content
            .when(on_hyprland, |el| {
                el.child(if show_queue_chrome {
                    hyprland_title_bar_drag_region().w(px(12.)).flex_shrink_0()
                } else {
                    hyprland_title_bar_drag_region().flex_1()
                })
            })
            .when(!on_hyprland && !show_queue_chrome, |el| {
                el.child(div().flex_1())
            });

        #[cfg(not(target_os = "linux"))]
        let content = content.when(!show_queue_chrome, |el| el.child(div().flex_1()));

        let content = content.when(show_queue_chrome, |el| {
            el.child(
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
        });

        #[cfg(target_os = "linux")]
        if on_hyprland {
            return h_flex()
                .id("title-bar")
                .w_full()
                .h(px(48.))
                .flex_shrink_0()
                .items_center()
                .pl(px(TITLE_BAR_LEFT_PAD))
                .when(chrome_on_left, |el| el.pr(px(TITLE_BAR_LEFT_PAD)))
                .border_b_1()
                .border_color(theme.title_bar_border)
                .bg(theme.title_bar)
                .child(content)
                .into_any_element();
        }

        #[cfg(target_os = "linux")]
        if chrome_on_left {
            return h_flex()
                .id("title-bar")
                .w_full()
                .h(px(48.))
                .flex_shrink_0()
                .items_center()
                .pr(px(TITLE_BAR_LEFT_PAD))
                .border_b_1()
                .border_color(theme.title_bar_border)
                .bg(theme.title_bar)
                .on_double_click(|_, window, _| window.zoom_window())
                .child(linux_left_window_controls(window, cx))
                .child(content)
                .into_any_element();
        }

        let bar = TitleBar::new().h(px(48.));
        #[cfg(not(target_os = "macos"))]
        let bar = bar.pl(px(TITLE_BAR_LEFT_PAD));
        #[cfg(target_os = "macos")]
        let bar = bar.pr(px(TITLE_BAR_LEFT_PAD));
        bar.child(content).into_any_element()
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

    fn normalized_source(raw: &str) -> String {
        raw.replace("\r\n", "\n")
    }

    fn hyprland_branch_source(source: &str) -> &str {
        let start = source
            .find("if on_hyprland")
            .expect("Hyprland title-bar branch");
        let end = start
            + source[start..]
                .find(".into_any_element();")
                .expect("Hyprland title-bar return");
        &source[start..end]
    }

    fn hyprland_drag_helper_scope(source: &str) -> &str {
        let helper_start = source
            .find("fn hyprland_title_bar_drag_region")
            .expect("hyprland title-bar drag helper");
        let helper_scope = &source[helper_start..];
        let helper_end = helper_scope
            .find("\n}\n")
            .expect("hyprland title-bar drag helper end");
        &helper_scope[..helper_end]
    }

    #[test]
    fn hyprland_parent_title_bar_has_no_drag_or_move() {
        let source = normalized_source(SOURCE);
        let branch = hyprland_branch_source(&source);
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
        let source = normalized_source(SOURCE);
        let helper_scope = hyprland_drag_helper_scope(&source);

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

        assert!(
            source.contains("hyprland_title_bar_drag_region()"),
            "title bar must mount title-bar-drag via hyprland_title_bar_drag_region"
        );
    }

    #[test]
    fn hyprland_drag_helper_end_scan_handles_crlf_line_endings() {
        const CRLF_HELPER_END: &str = "\r\n}\r\n";
        const CRLF_FIXTURE: &str = concat!(
            "#[cfg(target_os = \"linux\")]\r\n",
            "fn hyprland_title_bar_drag_region() {\r\n",
            "    div()\r\n",
            "        .id(\"title-bar-drag\")\r\n",
            "        .window_control_area(WindowControlArea::Drag)\r\n",
            "        .on_mouse_down(MouseButton::Left, |_, window, _| {\r\n",
            "            window.start_window_move();\r\n",
            "        })\r\n",
            "}\r\n",
        );

        assert!(
            CRLF_FIXTURE.contains(CRLF_HELPER_END),
            "fixture must contain exact CRLF helper end token"
        );

        let source = normalized_source(CRLF_FIXTURE);
        let helper_scope = hyprland_drag_helper_scope(&source);
        assert!(helper_scope.contains(DRAG_ID));
        assert!(helper_scope.contains(DRAG_TOKEN));
        assert!(helper_scope.contains(MOVE_TOKEN));
    }

    #[test]
    fn hyprland_title_bar_drag_region_calls_are_linux_cfg_gated() {
        const CALL: &str = "hyprland_title_bar_drag_region()";
        const LINUX_CFG: &str = r#"#[cfg(target_os = "linux")]"#;

        let source = normalized_source(SOURCE);
        let mut search_from = 0;
        let mut call_sites = 0;
        while let Some(rel) = source[search_from..].find(CALL) {
            let at = search_from + rel;
            let is_definition = at >= 3 && source.get(at - 3..at) == Some("fn ");
            if !is_definition {
                call_sites += 1;
                let prefix = &source[..at];
                let cfg_at = prefix
                    .rfind(LINUX_CFG)
                    .expect("linux cfg must gate hyprland_title_bar_drag_region call");
                let cfg_gap = &source[cfg_at..at];
                assert!(
                    !cfg_gap.contains(r#"#[cfg(not(target_os = "linux"))]"#),
                    "hyprland_title_bar_drag_region call must not be under not(linux) cfg"
                );
            }
            search_from = at + CALL.len();
        }

        assert!(
            call_sites >= 1,
            "expected at least one linux-gated hyprland_title_bar_drag_region call"
        );
    }
}
