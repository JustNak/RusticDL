use gpui::{
    div, hsla, prelude::FluentBuilder, px, App, Context, Hsla, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled,
};
use gpui_component::{
    h_flex, tooltip::Tooltip, v_flex, ActiveTheme, Icon, Sizable, StyledExt, Theme,
};

use super::super::DownloadApp;
use crate::settings::AccentPreset;

/// Field title — stronger than hints so forms scan as Label → control → help.
/// Used by add dialog and other compact forms (`text_xs`).
pub(crate) fn field_label(text: &'static str, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_xs()
        .font_semibold()
        .text_color(theme.foreground)
        .child(text)
}

/// Supporting description under a field. Kept smaller/softer than `field_label`.
pub(crate) fn field_hint(text: impl Into<SharedString>, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_xs()
        .font_normal()
        .text_color(theme.muted_foreground.opacity(0.78))
        .child(text.into())
}

// ── Settings layout helpers (settings panels only; leave add-dialog labels alone) ──

/// Settings field label: `text_sm` semibold so hierarchy beats muted hints.
pub(crate) fn settings_field_label(text: &'static str, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_sm()
        .font_semibold()
        .text_color(theme.foreground)
        .child(text)
}

/// Sub-group eyebrow (e.g. NOTIFICATIONS). Optional top hairline divider.
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

/// Horizontal toggle/choice row: label (+ optional hint) left, control cluster right.
///
/// Label is width-capped so multi-button control clusters (theme, density, handoff)
/// keep a single horizontal row instead of wrapping and bleeding into the next field.
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
        // Size to the control cluster; do not cap width so Off/On/Auto groups
        // stay on one line inside the expanded settings content column.
        .child(div().flex_shrink_0().child(control))
}

/// Equal-size circular preset swatch (solid fill + selection ring).
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
    // Light fills (stock dark primary is often near-white) need a stronger edge
    // so they don't dissolve into the selection ring or the panel.
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
            // Darker ring when the fill itself is light so selection stays obvious.
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

/// Custom mixer entry: white disc + paintbrush — clearly not a solid preset.
pub(crate) fn accent_custom_swatch(
    selected: bool,
    _custom_color: Hsla,
    theme: &Theme,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let tip: SharedString = "Custom — mix your own accent".into();
    // White plate always; brush in dark ink so it stays readable on light/dark UI.
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
