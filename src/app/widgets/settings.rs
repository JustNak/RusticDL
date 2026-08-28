use gpui::{
    div, hsla, prelude::FluentBuilder, px, App, Context, Entity, Hsla, InteractiveElement,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    tooltip::Tooltip,
    v_flex, ActiveTheme, Icon, Sizable, StyledExt, Theme,
};

use super::super::DownloadApp;
use crate::settings::AccentPreset;

pub(crate) fn field_label(text: &'static str, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_xs()
        .font_semibold()
        .text_color(theme.foreground)
        .child(text)
}

pub(crate) fn field_hint(text: impl Into<SharedString>, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_xs()
        .font_normal()
        .text_color(theme.muted_foreground.opacity(0.78))
        .child(text.into())
}

pub(crate) fn settings_field_label(text: &'static str, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_sm()
        .font_semibold()
        .text_color(theme.foreground)
        .child(text)
}

pub(crate) fn settings_input_with_reset(
    id: impl Into<SharedString>,
    input: &Entity<InputState>,
    current: &str,
    default_value: &str,
    default_label: impl Into<SharedString>,
    app: Entity<DownloadApp>,
    disabled: bool,
) -> Input {
    let dirty = current.trim() != default_value.trim();
    let default_owned = default_value.to_string();
    let tip: SharedString = format!("Reset to default ({})", default_label.into()).into();
    let reset_id = id.into();
    let input_entity = input.clone();

    Input::new(input)
        .w_full()
        .disabled(disabled)
        .when(dirty && !disabled, |inp| {
            inp.suffix(
                Button::new(reset_id)
                    .ghost()
                    .compact()
                    .icon(Icon::empty().path("icons/rotate-cw.svg"))
                    .tooltip(tip)
                    .on_click({
                        let input_entity = input_entity.clone();
                        let default_owned = default_owned.clone();
                        let app = app.clone();
                        move |_, window, cx| {
                            input_entity.update(cx, |state, cx| {
                                state.set_value(default_owned.clone(), window, cx);
                            });
                            let _ = app.update(cx, |_, cx| cx.notify());
                        }
                    }),
            )
        })
}

pub(crate) fn settings_subgroup(
    title: &'static str,
    with_divider: bool,
    cx: &mut App,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let eyebrow: SharedString = title.to_ascii_uppercase().into();
    v_flex()
        .w_full()
        .gap_2()
        .when(with_divider, |el| {
            el.child(div().w_full().h(px(1.)).bg(theme.border.opacity(0.55)))
        })
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(theme.muted_foreground)
                .child(eyebrow),
        )
}

pub(crate) fn settings_choice_row(
    label: &'static str,
    hint: Option<&'static str>,
    control: impl IntoElement,
    cx: &mut App,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .gap_4()
        .items_center()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .min_w(px(140.))
                .max_w(px(320.))
                .gap_0p5()
                .child(settings_field_label(label, cx))
                .when_some(hint, |el, text| el.child(field_hint(text, cx))),
        )
        .child(div().flex_shrink_0().child(control))
}

pub(crate) fn accent_preset_swatch(
    preset: AccentPreset,
    selected: bool,
    swatch: Hsla,
    theme: &Theme,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let label = preset.label();
    let tip: SharedString = if preset == AccentPreset::Default {
        "Default — stock theme color".into()
    } else {
        label.to_string().into()
    };
    let light_fill = swatch.l > 0.72;
    let fill_border = if selected {
        if light_fill {
            theme.foreground.opacity(0.35)
        } else {
            theme.background.opacity(0.35)
        }
    } else if light_fill {
        theme.border.opacity(0.85)
    } else {
        theme.border.opacity(0.45)
    };
    div()
        .id(SharedString::from(format!("accent-{label}")))
        .size(px(32.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .border_2()
        .border_color(if selected {
            if light_fill {
                theme.muted_foreground.opacity(0.95)
            } else {
                theme.foreground.opacity(0.92)
            }
        } else {
            theme.border.opacity(0.0)
        })
        .when(!selected, |el| {
            el.hover(|s| {
                s.border_color(theme.muted_foreground.opacity(0.55))
                    .bg(theme.secondary.opacity(0.4))
            })
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.set_accent_preset(preset, window, cx);
        }))
        .child(
            div()
                .size(px(20.))
                .rounded_full()
                .bg(swatch)
                .border_1()
                .border_color(fill_border),
        )
}

pub(crate) fn accent_custom_swatch(
    selected: bool,
    _custom_color: Hsla,
    theme: &Theme,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let tip: SharedString = "Custom — mix your own accent".into();
    let plate = hsla(0.0, 0.0, 0.98, 1.0);
    let brush = hsla(0.0, 0.0, 0.22, 1.0);

    div()
        .id("accent-Custom")
        .size(px(32.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .border_2()
        .border_color(if selected {
            theme.foreground.opacity(0.92)
        } else {
            theme.border.opacity(0.0)
        })
        .when(!selected, |el| {
            el.hover(|s| {
                s.border_color(theme.muted_foreground.opacity(0.55))
                    .bg(theme.secondary.opacity(0.4))
            })
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(|this, _, window, cx| {
            this.set_accent_preset(AccentPreset::Custom, window, cx);
        }))
        .child(
            div()
                .size(px(20.))
                .rounded_full()
                .bg(plate)
                .border_1()
                .border_color(theme.border.opacity(0.5))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::empty()
                        .path("icons/paintbrush.svg")
                        .with_size(px(12.))
                        .text_color(brush),
                ),
        )
}

pub(crate) fn accent_hsl_slider_row(
    label: &'static str,
    value: String,
    slider: impl IntoElement,
    theme: &Theme,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .w_full()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child(label),
                )
                .child(
                    div()
                        .text_xs()
                        .font_medium()
                        .text_color(theme.muted_foreground.opacity(0.85))
                        .child(value),
                ),
        )
        .child(slider)
}
