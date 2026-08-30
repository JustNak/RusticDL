use gpui::{
    div, prelude::FluentBuilder, px, Context, IntoElement, ParentElement, Styled,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    slider::Slider,
    v_flex, ActiveTheme, IconName, StyledExt,
};

use super::super::widgets::{
    accent_custom_swatch, accent_hsl_slider_row, accent_preset_swatch, field_hint, settings_bays,
    settings_field_label, styled_progress, ExclusiveOpt, SettingsBay, SettingsExclusiveRow,
    SettingsToggleRow,
};
use super::super::DownloadApp;
use crate::appearance::{accent_swatch_color, custom_accent_hsla, resolve_theme_mode};
use crate::settings::{AccentPreset, AppTheme, CornerRadiusScale, ProgressStyle, UiDensity};

impl DownloadApp {
    pub(super) fn render_settings_appearance(
        &mut self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
        let app = cx.entity();

        settings_bays()
            .child(
                SettingsBay::new("Theme & color")
                    .child(
                        SettingsExclusiveRow::new(
                            "theme",
                            "Theme",
                            theme_choice,
                            [
                                ExclusiveOpt::new(AppTheme::Light, "theme-light", "Light")
                                    .icon(IconName::Sun),
                                ExclusiveOpt::new(AppTheme::Dark, "theme-dark", "Dark")
                                    .icon(IconName::Moon),
                                ExclusiveOpt::new(AppTheme::System, "theme-system", "System")
                                    .icon(IconName::Settings),
                            ],
                            {
                                let app = app.clone();
                                move |next, window, cx| {
                                    app.update(cx, |this, cx| {
                                        this.set_theme_draft(next, window, cx);
                                    });
                                }
                            },
                        )
                        .hint_dynamic(mode_hint),
                    )
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
                    ),
            )
            .child(
                SettingsBay::new("Glass & texture")
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
                            .child(field_hint(
                                "0% solid. Higher values glass the window; blur softens the backdrop when transparent.",
                                cx,
                            )),
                    )
                    .child(SettingsToggleRow::new(
                        "blur",
                        "Backdrop blur",
                        backdrop_blur,
                        {
                            let app = app.clone();
                            move |on, window, cx| {
                                app.update(cx, |this, cx| {
                                    this.set_backdrop_blur(on, window, cx);
                                });
                            }
                        },
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
                    ),
            )
            .child(
                SettingsBay::new("Layout & motion")
                    .child(
                        SettingsExclusiveRow::new(
                            "density",
                            "UI density",
                            ui_density,
                            UiDensity::ALL.map(|d| {
                                ExclusiveOpt::new(
                                    d,
                                    format!("density-{}", d.label()),
                                    d.label(),
                                )
                            }),
                            {
                                let app = app.clone();
                                move |d, window, cx| {
                                    app.update(cx, |this, cx| {
                                        this.set_ui_density(d, window, cx);
                                    });
                                }
                            },
                        )
                        .hint("Compact tightens rows, sidebar, and settings padding."),
                    )
                    .child(SettingsExclusiveRow::new(
                        "radius",
                        "Corner radius",
                        corner_radius,
                        CornerRadiusScale::ALL.map(|scale| {
                            ExclusiveOpt::new(
                                scale,
                                format!("radius-{}", scale.label()),
                                scale.label(),
                            )
                        }),
                        {
                            let app = app.clone();
                            move |scale, window, cx| {
                                app.update(cx, |this, cx| {
                                    this.set_corner_radius(scale, window, cx);
                                });
                            }
                        },
                    ))
                    .child(
                        SettingsToggleRow::new(
                            "motion",
                            "Reduce motion",
                            reduce_motion,
                            {
                                let app = app.clone();
                                move |on, window, cx| {
                                    app.update(cx, |this, cx| {
                                        this.set_reduce_motion(on, window, cx);
                                    });
                                }
                            },
                        )
                        .hint("Calmer empty states and less decorative motion."),
                    ),
            )
            .child(
                SettingsBay::new("Progress")
                    .child(SettingsExclusiveRow::new(
                        "progress",
                        "Progress style",
                        progress_style,
                        ProgressStyle::ALL.map(|style| {
                            ExclusiveOpt::new(
                                style,
                                format!("progress-{}", style.label()),
                                style.label(),
                            )
                        }),
                        {
                            let app = app.clone();
                            move |style, window, cx| {
                                app.update(cx, |this, cx| {
                                    this.set_progress_style(style, window, cx);
                                });
                            }
                        },
                    ))
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
