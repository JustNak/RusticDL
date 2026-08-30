use gpui::{Context, IntoElement, ParentElement, Styled};
use gpui_component::v_flex;

use super::super::widgets::{
    field_hint, settings_bays, settings_field_label, settings_input_with_reset, ExclusiveOpt,
    SettingsBay, SettingsExclusiveRow, SettingsToggleRow,
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
        let defaults = ExtensionIntegrationSettings::default();
        let hosts_def = defaults.excluded_hosts.join("\n");
        let hosts_val = self.excluded_hosts_input.read(cx).value().to_string();
        let ext_def = defaults.captured_file_extensions.join(", ");
        let ext_val = self.captured_extensions_input.read(cx).value().to_string();
        let app = cx.entity();

        v_flex()
            .w_full()
            .gap_4()
            .child(field_hint(
                "Saved with Save settings. Synced to the browser extension when connected.",
                cx,
            ))
            .child(
                settings_bays()
                    .child(
                        SettingsBay::new("Capture").child(
                            SettingsToggleRow::new(
                                "ext-enabled",
                                "Enable browser capture",
                                ext_enabled,
                                {
                                    let app = app.clone();
                                    move |on, window, cx| {
                                        app.update(cx, |this, cx| {
                                            this.set_extension_enabled(on, window, cx);
                                        });
                                    }
                                },
                            )
                            .hint("Options below apply only while capture is On."),
                        ),
                    )
                    .child(
                        SettingsBay::new("Handoff").child(
                            SettingsExclusiveRow::new(
                                "handoff",
                                "Download handoff",
                                handoff_mode,
                                [
                                    ExclusiveOpt::new(
                                        DownloadHandoffMode::Off,
                                        "handoff-off",
                                        "Off",
                                    ),
                                    ExclusiveOpt::new(
                                        DownloadHandoffMode::Ask,
                                        "handoff-ask",
                                        "Ask",
                                    ),
                                    ExclusiveOpt::new(
                                        DownloadHandoffMode::Auto,
                                        "handoff-auto",
                                        "Auto",
                                    ),
                                ],
                                {
                                    let app = app.clone();
                                    move |mode, window, cx| {
                                        app.update(cx, |this, cx| {
                                            this.set_download_handoff_mode(mode, window, cx);
                                        });
                                    }
                                },
                            )
                            .hint("Off skips; Ask prompts; Auto hands off silently.")
                            .disabled(!ext_enabled),
                        ),
                    )
                    .child(
                        SettingsBay::new("UI")
                            .child(SettingsToggleRow::new(
                                "ext-ctx",
                                "Context menu",
                                context_menu_enabled,
                                {
                                    let app = app.clone();
                                    move |on, window, cx| {
                                        app.update(cx, |this, cx| {
                                            this.set_context_menu_enabled(on, window, cx);
                                        });
                                    }
                                },
                            ).disabled(!ext_enabled))
                            .child(
                                SettingsToggleRow::new(
                                    "ext-badge",
                                    "Toolbar badge status",
                                    show_badge_status,
                                    {
                                        let app = app.clone();
                                        move |on, window, cx| {
                                            app.update(cx, |this, cx| {
                                                this.set_show_badge_status(on, window, cx);
                                            });
                                        }
                                    },
                                )
                                .hint("Connection / activity on the extension toolbar icon.")
                                .disabled(!ext_enabled),
                            )
                            .child(
                                SettingsToggleRow::new(
                                    "ext-progress",
                                    "Show progress after handoff",
                                    show_progress_after_handoff,
                                    {
                                        let app = app.clone();
                                        move |on, window, cx| {
                                            app.update(cx, |this, cx| {
                                                this.set_show_progress_after_handoff(on, window, cx);
                                            });
                                        }
                                    },
                                )
                                .hint("Show a floating progress window after browser capture.")
                                .disabled(!ext_enabled),
                            ),
                    )
                    .child(
                        SettingsBay::new("Filters")
                            .child(
                                v_flex()
                                    .gap_1p5()
                                    .child(settings_field_label("Excluded hosts", cx))
                                    .child(settings_input_with_reset(
                                        "reset-excluded-hosts",
                                        &self.excluded_hosts_input,
                                        &hosts_val,
                                        &hosts_def,
                                        "factory list",
                                        app.clone(),
                                        !ext_enabled,
                                    ))
                                    .child(field_hint(
                                        "One host per line. Matching sites skip capture.",
                                        cx,
                                    )),
                            )
                            .child(
                                v_flex()
                                    .gap_1p5()
                                    .child(settings_field_label("Captured file extensions", cx))
                                    .child(settings_input_with_reset(
                                        "reset-captured-extensions",
                                        &self.captured_extensions_input,
                                        &ext_val,
                                        &ext_def,
                                        "factory list",
                                        app.clone(),
                                        !ext_enabled,
                                    ))
                                    .child(field_hint(
                                        "Comma-separated extensions to intercept (e.g. zip, pdf, exe).",
                                        cx,
                                    )),
                            ),
                    )
                    .child(
                        SettingsBay::new("Diagnostics").child(
                            SettingsToggleRow::new(
                                "ext-debug",
                                "Capture debug logging",
                                capture_debug_logging,
                                {
                                    let app = app.clone();
                                    move |on, window, cx| {
                                        app.update(cx, |this, cx| {
                                            this.set_download_capture_debug_logging(on, window, cx);
                                        });
                                    }
                                },
                            )
                            .disabled(!ext_enabled),
                        ),
                    ),
            )
    }
}
