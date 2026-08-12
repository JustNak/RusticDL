//! Browser extension settings category panel.

use gpui::{prelude::FluentBuilder, px, Context, IntoElement, ParentElement, Styled};
use gpui_component::{
    button::{Button, ButtonVariants},
    group_box::{GroupBox, GroupBoxVariants},
    h_flex, v_flex, Disableable,
};

use super::super::widgets::{
    field_hint, settings_choice_row, settings_field_label, settings_input_with_reset,
    settings_subgroup,
};
use super::super::DownloadApp;
use crate::extension_settings::{DownloadHandoffMode, ExtensionIntegrationSettings};

impl DownloadApp {
    pub(super) fn render_settings_browser(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let ext_enabled = self.settings.extension.enabled;
        let handoff_mode = self.settings.extension.download_handoff_mode;
        let context_menu_enabled = self.settings.extension.context_menu_enabled;
        let show_badge_status = self.settings.extension.show_badge_status;
        let show_progress_after_handoff = self.settings.extension.show_progress_after_handoff;
        let capture_debug_logging = self.settings.extension.download_capture_debug_logging;

        GroupBox::new().outline().child(
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
                        .child(
                            Button::new("handoff-off")
                                .label("Off")
                                .min_w(px(72.))
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
                                .min_w(px(72.))
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
                                .min_w(px(72.))
                                .disabled(!ext_enabled)
                                .when(handoff_mode == DownloadHandoffMode::Auto, |b| b.primary())
                                .when(handoff_mode != DownloadHandoffMode::Auto, |b| b.outline())
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
                    Some("Show a floating progress window after browser capture."),
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
                .child({
                    let defaults = ExtensionIntegrationSettings::default();
                    let hosts_def = defaults.excluded_hosts.join("\n");
                    let hosts_val = self.excluded_hosts_input.read(cx).value().to_string();
                    let app = cx.entity();
                    v_flex()
                        .gap_1p5()
                        .child(settings_field_label("Excluded hosts", cx))
                        .child(settings_input_with_reset(
                            "reset-excluded-hosts",
                            &self.excluded_hosts_input,
                            &hosts_val,
                            &hosts_def,
                            "factory list",
                            app,
                            !ext_enabled,
                        ))
                        .child(field_hint(
                            "One host per line. Matching sites skip capture.",
                            cx,
                        ))
                })
                .child({
                    let defaults = ExtensionIntegrationSettings::default();
                    let ext_def = defaults.captured_file_extensions.join(", ");
                    let ext_val = self.captured_extensions_input.read(cx).value().to_string();
                    let app = cx.entity();
                    v_flex()
                        .gap_1p5()
                        .child(settings_field_label("Captured file extensions", cx))
                        .child(settings_input_with_reset(
                            "reset-captured-extensions",
                            &self.captured_extensions_input,
                            &ext_val,
                            &ext_def,
                            "factory list",
                            app,
                            !ext_enabled,
                        ))
                        .child(field_hint(
                            "Comma-separated extensions to intercept (e.g. zip, pdf, exe).",
                            cx,
                        ))
                })
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
}
