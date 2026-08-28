use gpui::{
    div, hsla, linear_color_stop, linear_gradient, px, App, Hsla, IntoElement, ParentElement,
    SharedString, Styled, Window,
};
use gpui_component::{tooltip::Tooltip, Icon, IconName, Sizable, StyledExt};

pub(crate) fn render_vignette_overlay(edge_alpha: f32, is_dark: bool) -> impl IntoElement {
    let a = edge_alpha.clamp(0.0, 0.5);
    let edge = if is_dark {
        hsla(0.0, 0.0, 0.0, a)
    } else {
        hsla(0.0, 0.0, 0.08, a * 0.85)
    };
    let clear = hsla(0.0, 0.0, 0.0, 0.0);
    let band = px(96.);

    div()
        .absolute()
        .inset_0()
        .size_full()
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(band)
                .bg(linear_gradient(
                    180.0,
                    linear_color_stop(edge, 0.0),
                    linear_color_stop(clear, 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(band)
                .bg(linear_gradient(
                    0.0,
                    linear_color_stop(edge, 0.0),
                    linear_color_stop(clear, 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(band)
                .bg(linear_gradient(
                    90.0,
                    linear_color_stop(edge, 0.0),
                    linear_color_stop(clear, 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(band)
                .bg(linear_gradient(
                    270.0,
                    linear_color_stop(edge, 0.0),
                    linear_color_stop(clear, 1.0),
                )),
        )
}

pub(crate) fn empty_state_badge(
    icon: IconName,
    icon_color: Hsla,
    fill: Hsla,
    ring: Hsla,
    reduce_motion: bool,
) -> impl IntoElement {
    let outer = if reduce_motion { 56.0 } else { 64.0 };
    let inner = if reduce_motion { 44.0 } else { 48.0 };
    div()
        .w(px(outer))
        .h(px(outer))
        .rounded_full()
        .border_1()
        .border_color(ring)
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(inner))
                .h(px(inner))
                .rounded_full()
                .bg(fill)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).with_size(px(22.)).text_color(icon_color)),
        )
}

pub(crate) fn soft_tooltip(
    text: SharedString,
    tip_color: Hsla,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyView {
    Tooltip::new(text)
        .text_xs()
        .font_normal()
        .text_color(tip_color)
        .py_0()
        .px_1p5()
        .build(window, cx)
}
