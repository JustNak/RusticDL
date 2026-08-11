//! Settings panel UI extracted from `DownloadApp` for maintainability.
//! Category shell: vertical mini-nav + one panel at a time.

use gpui::{
    div, prelude::FluentBuilder, px, Context, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    clipboard::Clipboard,
    group_box::{GroupBox, GroupBoxVariants},
    h_flex,
    input::Input,
    slider::Slider,
    v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt,
};

use super::settings_category::SettingsCategory;
use super::widgets::{
    accent_custom_swatch, accent_hsl_slider_row, accent_preset_swatch, browse_directory,
    field_hint, settings_choice_row, settings_field_label, settings_nav_item, settings_subgroup,
    styled_progress,
};
use super::DownloadApp;
use crate::appearance::{accent_swatch_color, custom_accent_hsla, resolve_theme_mode};
use crate::download::reveal_in_folder;
use crate::extension_settings::DownloadHandoffMode;
use crate::settings::{
    AccentPreset, AppTheme, CornerRadiusScale, OsNotifyMode, ProgressStyle, UiDensity,
};

impl DownloadApp {
    pub(super) fn render_settings(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let settings_pad = self.settings.ui_density.settings_pad();
        let category = self.settings_category;
        // Mini-nav 148–160px; slightly tighter in Compact density.
        let nav_w = match self.settings.ui_density {
            UiDensity::Comfortable => 160.0,
            UiDensity::Compact => 148.0,
        };

        v_flex()
            .id("settings-view")
            .size_full()
            .bg(theme.background)
            // Body: mini-nav + scrolling category content.
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        v_flex()
                            .id("settings-mini-nav")
                            .w(px(nav_w))
                            .flex_shrink_0()
                            .h_full()
                            .bg(theme.sidebar)
                            .border_r_1()
                            .border_color(theme.sidebar_border)
                            .p_2()
                            .gap_0p5()
                            .children(
                                SettingsCategory::ALL
                                    .into_iter()
                                    .map(|cat| settings_nav_item(cat, category == cat, cx)),
                            ),
                    )
                    .child(
                        div()
                            // Key id by category so GPUI does not reuse scroll offset
                            // when switching from a tall panel (Appearance) to a short one.
                            .id(SharedString::from(format!(
                                "settings-content-scroll-{}",
                                category.label()
                            )))
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .p(px(settings_pad))
                            .child(
                                v_flex()
                                    .gap_5()
                                    .max_w(px(720.))
                                    .child(
                                        v_flex()
                                            .gap_0p5()
                                            .child(
                                                div()
                                                    .text_lg()
                                                    .font_bold()
                                                    .text_color(theme.foreground)
                                                    .child("Settings"),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child("Preferences and defaults"),
                                            ),
                                    )
                                    .child(match category {
                                        SettingsCategory::General => {
                                            self.render_settings_general(cx).into_any_element()
                                        }
                                        SettingsCategory::System => {
                                            self.render_settings_system(cx).into_any_element()
                                        }
                                        SettingsCategory::Browser => {
                                            self.render_settings_browser(cx).into_any_element()
                                        }
                                        SettingsCategory::Appearance => {
                                            self.render_settings_appearance(cx).into_any_element()
                                        }
                                        SettingsCategory::Data => {
                                            self.render_settings_data(cx).into_any_element()
                                        }
                                    }),
                            ),
                    ),
            )
            // Sticky footer (flex shell, not CSS position:sticky): always visible.
            .child(
                h_flex()
                    .id("settings-footer")
                    .flex_shrink_0()
                    .w_full()
                    .px(px(settings_pad))
                    .py_3()
                    .gap_3()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(
                        Button::new("reset-settings-defaults")
                            .outline()
                            .label("Reset defaults")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm_reset_settings_defaults(window, cx);
                            })),
                    )
                    .child(
                        Button::new("save-settings")
                            .primary()
                            .icon(IconName::Check)
                            .label("Save settings")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_settings(window, cx);
                            })),
                    ),
            )
    }

    fn settings_group_title(
        &self,
        category: SettingsCategory,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        h_flex()
            .gap_2()
            .items_center()
            .child(
                Icon::new(category.icon())
                    .with_size(px(14.))
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child(category.panel_title()),
            )
    }

    fn render_settings_general(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        GroupBox::new()
            .outline()
            .title(self.settings_group_title(SettingsCategory::General, cx))
            .child(
                v_flex()
                    .gap_4()
                    .child(settings_subgroup("Downloads", false, cx))
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(settings_field_label("Download directory", cx))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(Input::new(&self.dir_input).w_full().flex_1())
                                    .child(
                                        Button::new("browse-settings-dir")
                                            .label("Browse...")
                                            .icon(IconName::FolderOpen)
                                            .outline()
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                browse_directory(
                                                    this.dir_input.clone(),
                                                    cx.entity().clone(),
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    ),
                            ),
                    )
                    .child(settings_subgroup("Limits", true, cx))
                    .child(
                        h_flex()
                            .gap_4()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap_1p5()
                                    .child(settings_field_label("Max concurrent", cx))
                                    .child(Input::new(&self.concurrent_input).w_full()),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap_1p5()
                                    .child(settings_field_label("Auto-retry attempts", cx))
                                    .child(Input::new(&self.retry_input).w_full()),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap_1p5()
                                    .child(settings_field_label("Speed limit (KiB/s)", cx))
                                    .child(Input::new(&self.speed_input).w_full())
                                    .child(field_hint("0 means unlimited.", cx)),
                            ),
                    ),
            )
    }

    fn render_settings_system(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let close_to_tray = self.settings.close_to_tray;
        let launch_at_startup = self.settings.launch_at_startup;
        let startup_minimized = self.settings.startup_minimized;
        let os_notify_mode = self.settings.os_notify_mode;
        let notify_on_complete = self.settings.notify_on_complete;
        let notify_on_fail = self.settings.notify_on_fail;
        let clipboard_watch_enabled = self.settings.clipboard_watch_enabled;

        GroupBox::new()
            .outline()
            .title(self.settings_group_title(SettingsCategory::System, cx))
            .child(
                v_flex()
                    .gap_3()
                    .child(settings_subgroup("Window & startup", false, cx))
                    .child(settings_choice_row(
                        "Close to tray",
                        Some("Hides to the tray instead of quitting."),
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("close-tray-off")
                                    .label("Off")
                                    .when(!close_to_tray, |b| b.primary())
                                    .when(close_to_tray, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_close_to_tray(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("close-tray-on")
                                    .label("On")
                                    .when(close_to_tray, |b| b.primary())
                                    .when(!close_to_tray, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_close_to_tray(true, window, cx);
                                    })),
                            ),
                        cx,
                    ))
                    .child(settings_choice_row(
                        "Launch at startup",
                        None,
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("startup-off")
                                    .label("Off")
                                    .when(!launch_at_startup, |b| b.primary())
                                    .when(launch_at_startup, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_launch_at_startup(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("startup-on")
                                    .label("On")
                                    .when(launch_at_startup, |b| b.primary())
                                    .when(!launch_at_startup, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_launch_at_startup(true, window, cx);
                                    })),
                            ),
                        cx,
                    ))
                    .child(settings_choice_row(
                        "Start minimized",
                        Some("Opens hidden in the tray when launch at startup is On."),
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("startup-min-off")
                                    .label("Off")
                                    .disabled(!launch_at_startup)
                                    .when(!startup_minimized || !launch_at_startup, |b| b.primary())
                                    .when(startup_minimized && launch_at_startup, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_startup_minimized(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("startup-min-on")
                                    .label("On")
                                    .disabled(!launch_at_startup)
                                    .when(startup_minimized && launch_at_startup, |b| b.primary())
                                    .when(!startup_minimized || !launch_at_startup, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_startup_minimized(true, window, cx);
                                    })),
                            ),
                        cx,
                    ))
                    .child(settings_subgroup("Notifications", true, cx))
                    .child(settings_choice_row(
                        "OS notifications",
                        Some("Uses the tray icon even if Close to tray is Off."),
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new("os-notify-off")
                                    .label(OsNotifyMode::Off.label())
                                    .when(os_notify_mode == OsNotifyMode::Off, |b| b.primary())
                                    .when(os_notify_mode != OsNotifyMode::Off, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_os_notify_mode(OsNotifyMode::Off, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("os-notify-when-hidden")
                                    .label(OsNotifyMode::WhenHiddenToTray.label())
                                    .when(os_notify_mode == OsNotifyMode::WhenHiddenToTray, |b| {
                                        b.primary()
                                    })
                                    .when(os_notify_mode != OsNotifyMode::WhenHiddenToTray, |b| {
                                        b.outline()
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_os_notify_mode(
                                            OsNotifyMode::WhenHiddenToTray,
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("os-notify-always")
                                    .label(OsNotifyMode::Always.label())
                                    .when(os_notify_mode == OsNotifyMode::Always, |b| b.primary())
                                    .when(os_notify_mode != OsNotifyMode::Always, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_os_notify_mode(OsNotifyMode::Always, window, cx);
                                    })),
                            ),
                        cx,
                    ))
                    .child(settings_choice_row(
                        "Notify on complete",
                        None,
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("notify-complete-off")
                                    .label("Off")
                                    .when(!notify_on_complete, |b| b.primary())
                                    .when(notify_on_complete, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_notify_on_complete(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("notify-complete-on")
                                    .label("On")
                                    .when(notify_on_complete, |b| b.primary())
                                    .when(!notify_on_complete, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_notify_on_complete(true, window, cx);
                                    })),
                            ),
                        cx,
                    ))
                    .child(settings_choice_row(
                        "Notify on fail",
                        None,
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("notify-fail-off")
                                    .label("Off")
                                    .when(!notify_on_fail, |b| b.primary())
                                    .when(notify_on_fail, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_notify_on_fail(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("notify-fail-on")
                                    .label("On")
                                    .when(notify_on_fail, |b| b.primary())
                                    .when(!notify_on_fail, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_notify_on_fail(true, window, cx);
                                    })),
                            ),
                        cx,
                    ))
                    .child(settings_subgroup("Clipboard", true, cx))
                    .child(settings_choice_row(
                        "Clipboard URL watch",
                        Some("Offers clipboard HTTP(S) URLs on focus; never auto-downloads."),
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("clipboard-watch-off")
                                    .label("Off")
                                    .when(!clipboard_watch_enabled, |b| b.primary())
                                    .when(clipboard_watch_enabled, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_clipboard_watch_enabled(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("clipboard-watch-on")
                                    .label("On")
                                    .when(clipboard_watch_enabled, |b| b.primary())
                                    .when(!clipboard_watch_enabled, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_clipboard_watch_enabled(true, window, cx);
                                    })),
                            ),
                        cx,
                    )),
            )
    }

    fn render_settings_browser(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let ext_enabled = self.settings.extension.enabled;
        let handoff_mode = self.settings.extension.download_handoff_mode;
        let context_menu_enabled = self.settings.extension.context_menu_enabled;
        let show_badge_status = self.settings.extension.show_badge_status;
        let show_progress_after_handoff = self.settings.extension.show_progress_after_handoff;
        let capture_debug_logging = self.settings.extension.download_capture_debug_logging;

        GroupBox::new()
            .outline()
            .title(self.settings_group_title(SettingsCategory::Browser, cx))
            .child(
                v_flex()
                    .gap_3()
                    .child(field_hint(
                        "Saved with Save settings. Synced to the browser extension when connected.",
                        cx,
                    ))
                    .child(settings_subgroup("Capture", false, cx))
                    .child(settings_choice_row(
                        "Enable browser capture",
                        Some("Options below apply only while capture is On."),
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("ext-enabled-off")
                                    .label("Off")
                                    .when(!ext_enabled, |b| b.primary())
                                    .when(ext_enabled, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_extension_enabled(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("ext-enabled-on")
                                    .label("On")
                                    .when(ext_enabled, |b| b.primary())
                                    .when(!ext_enabled, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_extension_enabled(true, window, cx);
                                    })),
                            ),
                        cx,
                    ))
                    .child(settings_subgroup("Handoff", true, cx))
                    .child(settings_choice_row(
                        "Download handoff",
                        Some("Off skips; Ask prompts; Auto hands off silently."),
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new("handoff-off")
                                    .label("Off")
                                    .disabled(!ext_enabled)
                                    .when(handoff_mode == DownloadHandoffMode::Off, |b| b.primary())
                                    .when(handoff_mode != DownloadHandoffMode::Off, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_download_handoff_mode(
                                            DownloadHandoffMode::Off,
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("handoff-ask")
                                    .label("Ask")
                                    .disabled(!ext_enabled)
                                    .when(handoff_mode == DownloadHandoffMode::Ask, |b| b.primary())
                                    .when(handoff_mode != DownloadHandoffMode::Ask, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_download_handoff_mode(
                                            DownloadHandoffMode::Ask,
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("handoff-auto")
                                    .label("Auto")
                                    .disabled(!ext_enabled)
                                    .when(handoff_mode == DownloadHandoffMode::Auto, |b| {
                                        b.primary()
                                    })
                                    .when(handoff_mode != DownloadHandoffMode::Auto, |b| {
                                        b.outline()
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_download_handoff_mode(
                                            DownloadHandoffMode::Auto,
                                            window,
                                            cx,
                                        );
                                    })),
                            ),
                        cx,
                    ))
                    .child(settings_subgroup("UI", true, cx))
                    .child(settings_choice_row(
                        "Context menu",
                        None,
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("ext-ctx-off")
                                    .label("Off")
                                    .disabled(!ext_enabled)
                                    .when(!context_menu_enabled, |b| b.primary())
                                    .when(context_menu_enabled, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_context_menu_enabled(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("ext-ctx-on")
                                    .label("On")
                                    .disabled(!ext_enabled)
                                    .when(context_menu_enabled, |b| b.primary())
                                    .when(!context_menu_enabled, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_context_menu_enabled(true, window, cx);
                                    })),
                            ),
                        cx,
                    ))
                    .child(settings_choice_row(
                        "Toolbar badge status",
                        Some("Connection / activity on the extension toolbar icon."),
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("ext-badge-off")
                                    .label("Off")
                                    .disabled(!ext_enabled)
                                    .when(!show_badge_status, |b| b.primary())
                                    .when(show_badge_status, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_show_badge_status(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("ext-badge-on")
                                    .label("On")
                                    .disabled(!ext_enabled)
                                    .when(show_badge_status, |b| b.primary())
                                    .when(!show_badge_status, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_show_badge_status(true, window, cx);
                                    })),
                            ),
                        cx,
                    ))
                    .child(settings_choice_row(
                        "Show progress after handoff",
                        Some("Focus or surface RusticDL after a browser handoff."),
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("ext-progress-off")
                                    .label("Off")
                                    .disabled(!ext_enabled)
                                    .when(!show_progress_after_handoff, |b| b.primary())
                                    .when(show_progress_after_handoff, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_show_progress_after_handoff(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("ext-progress-on")
                                    .label("On")
                                    .disabled(!ext_enabled)
                                    .when(show_progress_after_handoff, |b| b.primary())
                                    .when(!show_progress_after_handoff, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_show_progress_after_handoff(true, window, cx);
                                    })),
                            ),
                        cx,
                    ))
                    .child(settings_subgroup("Filters", true, cx))
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(settings_field_label("Excluded hosts", cx))
                            .child(
                                Input::new(&self.excluded_hosts_input)
                                    .w_full()
                                    .disabled(!ext_enabled),
                            )
                            .child(field_hint(
                                "One host per line. Matching sites skip capture.",
                                cx,
                            )),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(settings_field_label("Captured file extensions", cx))
                            .child(
                                Input::new(&self.captured_extensions_input)
                                    .w_full()
                                    .disabled(!ext_enabled),
                            )
                            .child(field_hint(
                                "Comma-separated extensions to intercept (e.g. zip, pdf, exe).",
                                cx,
                            )),
                    )
                    .child(settings_subgroup("Diagnostics", true, cx))
                    .child(settings_choice_row(
                        "Capture debug logging",
                        None,
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("ext-debug-off")
                                    .label("Off")
                                    .disabled(!ext_enabled)
                                    .when(!capture_debug_logging, |b| b.primary())
                                    .when(capture_debug_logging, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_download_capture_debug_logging(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("ext-debug-on")
                                    .label("On")
                                    .disabled(!ext_enabled)
                                    .when(capture_debug_logging, |b| b.primary())
                                    .when(!capture_debug_logging, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_download_capture_debug_logging(true, window, cx);
                                    })),
                            ),
                        cx,
                    )),
            )
    }

    fn render_settings_appearance(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
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
            .title(self.settings_group_title(SettingsCategory::Appearance, cx))
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
                            .flex_wrap()
                            .child(
                                Button::new("theme-light")
                                    .icon(IconName::Sun)
                                    .label("Light")
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
                        h_flex().gap_2().flex_wrap().children(UiDensity::ALL.into_iter().map(
                            |d| {
                                let selected = ui_density == d;
                                Button::new(SharedString::from(format!("density-{}", d.label())))
                                    .label(d.label())
                                    .when(selected, |b| b.primary())
                                    .when(!selected, |b| b.outline())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.set_ui_density(d, window, cx);
                                    }))
                            },
                        )),
                        cx,
                    ))
                    .child(settings_choice_row(
                        "Corner radius",
                        None,
                        h_flex().gap_2().flex_wrap().children(
                            CornerRadiusScale::ALL.into_iter().map(|scale| {
                                let selected = corner_radius == scale;
                                Button::new(SharedString::from(format!(
                                    "radius-{}",
                                    scale.label()
                                )))
                                .label(scale.label())
                                .when(selected, |b| b.primary())
                                .when(!selected, |b| b.outline())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_corner_radius(scale, window, cx);
                                }))
                            }),
                        ),
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
                        h_flex().gap_2().flex_wrap().children(
                            ProgressStyle::ALL.into_iter().map(|style| {
                                let selected = progress_style == style;
                                Button::new(SharedString::from(format!(
                                    "progress-{}",
                                    style.label()
                                )))
                                .label(style.label())
                                .when(selected, |b| b.primary())
                                .when(!selected, |b| b.outline())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_progress_style(style, window, cx);
                                }))
                            }),
                        ),
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

    fn render_settings_data(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let data_dir = self.paths.root.display().to_string();

        GroupBox::new()
            .outline()
            .title(self.settings_group_title(SettingsCategory::Data, cx))
            .child(
                v_flex()
                    .gap_3()
                    .child(settings_subgroup("App data", false, cx))
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(settings_field_label("App data directory", cx))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_x_hidden()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(data_dir.clone()),
                                    )
                                    .child(
                                        Clipboard::new("copy-data-dir")
                                            .value(SharedString::from(data_dir)),
                                    )
                                    .child(
                                        Button::new("open-data-dir")
                                            .outline()
                                            .small()
                                            .icon(IconName::FolderOpen)
                                            .label("Open")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                if let Err(msg) = reveal_in_folder(&this.paths.root)
                                                {
                                                    this.show_toast(msg, cx);
                                                }
                                            })),
                                    ),
                            )
                            .child(field_hint("settings.json and state.json live here.", cx)),
                    ),
            )
    }
}
