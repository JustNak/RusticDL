//! Appearance settings category panel.

use gpui::{
    div, prelude::FluentBuilder, px, Context, IntoElement, ParentElement, SharedString, Styled,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    group_box::{GroupBox, GroupBoxVariants},
    h_flex,
    slider::Slider,
    v_flex, ActiveTheme, IconName, StyledExt,
};

use super::super::widgets::{
    accent_custom_swatch, accent_hsl_slider_row, accent_preset_swatch, field_hint,
    settings_choice_row, settings_field_label, settings_subgroup, styled_progress,
};
use super::super::DownloadApp;
use crate::appearance::{accent_swatch_color, custom_accent_hsla, resolve_theme_mode};
use crate::settings::{AccentPreset, AppTheme, CornerRadiusScale, ProgressStyle, UiDensity};

impl DownloadApp {
    pub(super) fn render_settings_appearance(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let theme_choice = self.settings.theme;
        let accent_preset = self.settings.accent_preset;
        let noise_pct = self.settings.noise_intensity;
        let transparency_pct = self.settings.window_transparency;
        let backdrop_blur = self.settings.backdrop_blur;
        let ui_density = self.settings.ui_density;
        let corner_radius = self.settings.corner_radius;
        let reduce_motion = self.settings.reduce_motion;
        let vignette_pct = self.settings.vignette_intensity;
        let progress_style = self.settings.progress_style;
        let accent_hue = self.settings.accent_hue;
        let accent_sat = self.settings.accent_saturation;
        let accent_light = self.settings.accent_lightness;
        let custom_color = custom_accent_hsla(accent_hue, accent_sat, accent_light);
        let resolved_mode = resolve_theme_mode(theme_choice, None, cx);
        // Sparse hints: only System needs a live mode note; Light/Dark covered by footer line.
        let mode_hint = match theme_choice {
            AppTheme::System => {
                if resolved_mode.is_dark() {
                    Some("Following system (currently dark).")
                } else {
                    Some("Following system (currently light).")
                }
            }
            AppTheme::Light | AppTheme::Dark => None,
        };

        GroupBox::new()
            .outline()
            .child(
                v_flex()
                    .gap_3()
                    // ── Theme & color ──
                    .child(settings_subgroup("Theme & color", false, cx))
                    .child(settings_choice_row(
                        "Theme",
                        mode_hint,
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("theme-light")
                                    .icon(IconName::Sun)
                                    .label("Light")
                                    .min_w(px(100.))
                                    .when(theme_choice == AppTheme::Light, |b| b.primary())
                                    .when(theme_choice != AppTheme::Light, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_theme_draft(AppTheme::Light, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("theme-dark")
                                    .icon(IconName::Moon)
                                    .label("Dark")
                                    .min_w(px(100.))
                                    .when(theme_choice == AppTheme::Dark, |b| b.primary())
                                    .when(theme_choice != AppTheme::Dark, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_theme_draft(AppTheme::Dark, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("theme-system")
                                    .icon(IconName::Settings)
                                    .label("System")
                                    .min_w(px(100.))
                                    .when(theme_choice == AppTheme::System, |b| b.primary())
                                    .when(theme_choice != AppTheme::System, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_theme_draft(AppTheme::System, window, cx);
                                    })),
                            ),
                        cx,
                    ))
                    // Accent — preset dots + distinct Custom (rainbow ring)
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(settings_field_label("Color accent", cx))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_medium()
                                            .text_color(theme.muted_foreground)
                                            .child(accent_preset.label()),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .flex_wrap()
                                    .items_center()
                                    .children(AccentPreset::ALL.into_iter().filter(|p| {
                                        *p != AccentPreset::Custom
                                    }).map(|preset| {
                                        accent_preset_swatch(
                                            preset,
                                            accent_preset == preset,
                                            accent_swatch_color(
                                                preset,
                                                accent_hue,
                                                accent_sat,
                                                accent_light,
                                                theme.primary,
                                            ),
                                            &theme,
                                            cx,
                                        )
                                    }))
                                    // Divider: presets | custom mixer
                                    .child(
                                        div()
                                            .mx_0p5()
                                            .w(px(1.))
                                            .h(px(18.))
                                            .rounded_full()
                                            .bg(theme.border.opacity(0.7)),
                                    )
                                    .child(accent_custom_swatch(
                                        accent_preset == AccentPreset::Custom,
                                        custom_color,
                                        &theme,
                                        cx,
                                    )),
                            )
                            .when(accent_preset == AccentPreset::Custom, |this| {
                                this.child(
                                    v_flex()
                                        .w_full()
                                        .gap_2p5()
                                        .p_3()
                                        .rounded(theme.radius_lg)
                                        .border_1()
                                        .border_color(theme.border.opacity(0.45))
                                        .bg(theme.secondary.opacity(0.28))
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .size(px(28.))
                                                        .rounded_full()
                                                        .bg(custom_color)
                                                        .border_2()
                                                        .border_color(
                                                            theme.foreground.opacity(0.22),
                                                        )
                                                        .flex_shrink_0(),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .font_semibold()
                                                        .text_color(theme.muted_foreground)
                                                        .child("Mix custom accent"),
                                                )
                                                .child(div().flex_1())
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .font_medium()
                                                        .text_color(theme.muted_foreground)
                                                        .child(format!(
                                                            "H {:.0}  S {:.0}%  L {:.0}%",
                                                            accent_hue, accent_sat, accent_light
                                                        )),
                                                ),
                                        )
                                        .child(accent_hsl_slider_row(
                                            "Hue",
                                            format!("{:.0}°", accent_hue),
                                            Slider::new(&self.hue_slider).horizontal().w_full(),
                                            &theme,
                                        ))
                                        .child(accent_hsl_slider_row(
                                            "Saturation",
                                            format!("{:.0}%", accent_sat),
                                            Slider::new(&self.sat_slider).horizontal().w_full(),
                                            &theme,
                                        ))
                                        .child(accent_hsl_slider_row(
                                            "Lightness",
                                            format!("{:.0}%", accent_light),
                                            Slider::new(&self.light_slider)
                                                .horizontal()
                                                .w_full(),
                                            &theme,
                                        )),
                                )
                            }),
                    )
                    // Live preview strip — near theme/accent so color changes are obvious
                    .child(
                        v_flex()
                            .gap_2()
                            .child(settings_field_label("Preview", cx))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .items_center()
                                    .p_3()
                                    .rounded(theme.radius_lg)
                                    .border_1()
                                    .border_color(theme.border.opacity(0.4))
                                    .bg(theme.secondary.opacity(0.35))
                                    .child(
                                        Button::new("preview-primary")
                                            .primary()
                                            .label("Primary"),
                                    )
                                    .child(
                                        Button::new("preview-outline")
                                            .outline()
                                            .label("Secondary"),
                                    )
                                    .child(div().w(px(140.)).child(styled_progress(
                                        64.0,
                                        theme.progress_bar,
                                        progress_style,
                                    )))
                                    .child(
                                        div()
                                            .px_2()
                                            .py_1()
                                            .rounded(theme.radius)
                                            .bg(theme.list_active)
                                            .border_1()
                                            .border_color(theme.list_active_border)
                                            .text_xs()
                                            .text_color(theme.foreground)
                                            .child("Selected row"),
                                    ),
                            ),
                    )
                    // ── Glass & texture ──
                    .child(settings_subgroup("Glass & texture", true, cx))
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .child(settings_field_label("Transparency", cx))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("{transparency_pct}%")),
                                    ),
                            )
                            .child(Slider::new(&self.opacity_slider).horizontal().w_full())
                            // One glass-interaction note for the whole sub-group
                            // (blur constraint kept here so Backdrop blur can stay hint-free).
                            .child(field_hint(
                                "0% solid. Higher values glass the window; blur softens the backdrop when transparent.",
                                cx,
                            )),
                    )
                    .child(settings_choice_row(
                        "Backdrop blur",
                        None,
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("blur-off")
                                    .label("Off")
                                    .when(!backdrop_blur, |b| b.primary())
                                    .when(backdrop_blur, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_backdrop_blur(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("blur-on")
                                    .label("On")
                                    .when(backdrop_blur, |b| b.primary())
                                    .when(!backdrop_blur, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_backdrop_blur(true, window, cx);
                                    })),
                            ),
                        cx,
                    ))
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .child(settings_field_label("Noise (film grain)", cx))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("{noise_pct}%")),
                                    ),
                            )
                            .child(Slider::new(&self.noise_slider).horizontal().w_full()),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .child(settings_field_label("Vignette", cx))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("{vignette_pct}%")),
                                    ),
                            )
                            .child(Slider::new(&self.vignette_slider).horizontal().w_full()),
                    )
                    // ── Layout & motion ──
                    .child(settings_subgroup("Layout & motion", true, cx))
                    .child(settings_choice_row(
                        "UI density",
                        Some("Compact tightens rows, sidebar, and settings padding."),
                        h_flex().gap_2().children(UiDensity::ALL.into_iter().map(|d| {
                            let selected = ui_density == d;
                            Button::new(SharedString::from(format!("density-{}", d.label())))
                                .label(d.label())
                                .min_w(px(108.))
                                .when(selected, |b| b.primary())
                                .when(!selected, |b| b.outline())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_ui_density(d, window, cx);
                                }))
                        })),
                        cx,
                    ))
                    .child(settings_choice_row(
                        "Corner radius",
                        None,
                        h_flex().gap_2().children(CornerRadiusScale::ALL.into_iter().map(
                            |scale| {
                                let selected = corner_radius == scale;
                                Button::new(SharedString::from(format!(
                                    "radius-{}",
                                    scale.label()
                                )))
                                .label(scale.label())
                                .min_w(px(88.))
                                .when(selected, |b| b.primary())
                                .when(!selected, |b| b.outline())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_corner_radius(scale, window, cx);
                                }))
                            },
                        )),
                        cx,
                    ))
                    .child(settings_choice_row(
                        "Reduce motion",
                        Some("Calmer empty states and less decorative motion."),
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("motion-off")
                                    .label("Off")
                                    .when(!reduce_motion, |b| b.primary())
                                    .when(reduce_motion, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_reduce_motion(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("motion-on")
                                    .label("On")
                                    .when(reduce_motion, |b| b.primary())
                                    .when(!reduce_motion, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_reduce_motion(true, window, cx);
                                    })),
                            ),
                        cx,
                    ))
                    // ── Progress ──
                    .child(settings_subgroup("Progress", true, cx))
                    .child(settings_choice_row(
                        "Progress style",
                        None,
                        h_flex().gap_2().children(ProgressStyle::ALL.into_iter().map(|style| {
                            let selected = progress_style == style;
                            Button::new(SharedString::from(format!(
                                "progress-{}",
                                style.label()
                            )))
                            .label(style.label())
                            .min_w(px(80.))
                            .when(selected, |b| b.primary())
                            .when(!selected, |b| b.outline())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_progress_style(style, window, cx);
                            }))
                        })),
                        cx,
                    ))
                    // Section-local reset (kept one-click; draft-only until Save)
                    .child(
                        h_flex().child(
                            Button::new("reset-appearance")
                                .outline()
                                .label("Reset appearance")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.reset_appearance_draft(window, cx);
                                })),
                        ),
                    )
                    .child(field_hint(
                        "Preview applies immediately; save settings to persist.",
                        cx,
                    )),
            )
    }
}