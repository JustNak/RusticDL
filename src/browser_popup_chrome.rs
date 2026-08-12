//! Shared chrome for fixed-size browser capture popups.
//!
//! `gpui-component::TitleBar` always draws min/max/close on Windows. These
//! popups are non-resizable, so we use a close-only bar with a drag region.

use std::rc::Rc;

use gpui::{
    div, px, App, ClickEvent, InteractiveElement, IntoElement, MouseButton, ParentElement,
    StatefulInteractiveElement, Styled, Window, WindowControlArea,
};
use gpui_component::{h_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt};

pub const POPUP_TITLE_BAR_H: f32 = 34.0;

/// Close-only title bar with drag region (no minimize / maximize).
pub fn themed_popup_title_bar(
    title: impl Into<String>,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let title = title.into();
    let on_close: Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)> = Rc::new(on_close);
    let theme = cx.theme().clone();
    let danger = theme.danger;
    let danger_fg = theme.danger_foreground;
    let danger_active = theme.danger_active;

    h_flex()
        .id("popup-title-bar")
        .w_full()
        .h(px(POPUP_TITLE_BAR_H))
        .flex_shrink_0()
        .items_center()
        .justify_between()
        .pl_3()
        .border_b_1()
        .border_color(theme.title_bar_border)
        .bg(theme.title_bar)
        .child(
            div()
                .id("popup-title-drag")
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .window_control_area(WindowControlArea::Drag)
                .on_mouse_down(MouseButton::Left, |_, window, _| {
                    window.start_window_move();
                })
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child(title),
                ),
        )
        .child(
            div()
                .id("popup-close")
                .flex()
                .w(px(POPUP_TITLE_BAR_H))
                .h_full()
                .flex_shrink_0()
                .justify_center()
                .items_center()
                .text_color(theme.foreground)
                .window_control_area(WindowControlArea::Close)
                .cursor_pointer()
                .hover(move |s| s.bg(danger).text_color(danger_fg))
                .active(move |s| s.bg(danger_active).text_color(danger_fg))
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .on_click(move |ev, window, cx| {
                    cx.stop_propagation();
                    (on_close)(ev, window, cx);
                })
                .child(Icon::new(IconName::WindowClose).small()),
        )
}
