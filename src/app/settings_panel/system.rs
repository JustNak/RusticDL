use gpui::{Context, IntoElement, ParentElement};

use super::super::widgets::{
    settings_bays, ExclusiveOpt, SettingsBay, SettingsExclusiveRow, SettingsToggleRow,
};
use super::super::DownloadApp;
use crate::settings::OsNotifyMode;

impl DownloadApp {
    pub(super) fn render_settings_system(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let close_to_tray = self.settings.close_to_tray;
        let launch_at_startup = self.settings.launch_at_startup;
        let startup_minimized = self.settings.startup_minimized;
        let os_notify_mode = self.settings.os_notify_mode;
        let notify_on_complete = self.settings.notify_on_complete;
        let notify_on_fail = self.settings.notify_on_fail;
        let clipboard_watch_enabled = self.settings.clipboard_watch_enabled;
        let app = cx.entity();

        settings_bays()
            .child(
                SettingsBay::new("Window & startup")
                    .child(
                        SettingsToggleRow::new("close-tray", "Close to tray", close_to_tray, {
                            let app = app.clone();
                            move |on, window, cx| {
                                app.update(cx, |this, cx| {
                                    this.set_close_to_tray(on, window, cx);
                                });
                            }
                        })
                        .hint("Hides to the tray instead of quitting."),
                    )
                    .child(SettingsToggleRow::new(
                        "startup",
                        "Launch at startup",
                        launch_at_startup,
                        {
                            let app = app.clone();
                            move |on, window, cx| {
                                app.update(cx, |this, cx| {
                                    this.set_launch_at_startup(on, window, cx);
                                });
                            }
                        },
                    ))
                    .child(
                        SettingsToggleRow::new(
                            "startup-min",
                            "Start minimized",
                            startup_minimized && launch_at_startup,
                            {
                                let app = app.clone();
                                move |on, window, cx| {
                                    app.update(cx, |this, cx| {
                                        this.set_startup_minimized(on, window, cx);
                                    });
                                }
                            },
                        )
                        .hint("Opens hidden in the tray when launch at startup is On.")
                        .disabled(!launch_at_startup),
                    ),
            )
            .child(
                SettingsBay::new("Notifications")
                    .child(
                        SettingsExclusiveRow::new(
                            "os-notify",
                            "OS notifications",
                            os_notify_mode,
                            [
                                ExclusiveOpt::new(
                                    OsNotifyMode::Off,
                                    "os-notify-off",
                                    OsNotifyMode::Off.label(),
                                ),
                                ExclusiveOpt::new(
                                    OsNotifyMode::WhenHiddenToTray,
                                    "os-notify-when-hidden",
                                    OsNotifyMode::WhenHiddenToTray.label(),
                                ),
                                ExclusiveOpt::new(
                                    OsNotifyMode::Always,
                                    "os-notify-always",
                                    OsNotifyMode::Always.label(),
                                ),
                            ],
                            {
                                let app = app.clone();
                                move |mode, window, cx| {
                                    app.update(cx, |this, cx| {
                                        this.set_os_notify_mode(mode, window, cx);
                                    });
                                }
                            },
                        )
                        .hint("Uses the tray icon even if Close to tray is Off."),
                    )
                    .child(SettingsToggleRow::new(
                        "notify-complete",
                        "Notify on complete",
                        notify_on_complete,
                        {
                            let app = app.clone();
                            move |on, window, cx| {
                                app.update(cx, |this, cx| {
                                    this.set_notify_on_complete(on, window, cx);
                                });
                            }
                        },
                    ))
                    .child(SettingsToggleRow::new(
                        "notify-fail",
                        "Notify on fail",
                        notify_on_fail,
                        {
                            let app = app.clone();
                            move |on, window, cx| {
                                app.update(cx, |this, cx| {
                                    this.set_notify_on_fail(on, window, cx);
                                });
                            }
                        },
                    )),
            )
            .child(
                SettingsBay::new("Clipboard").child(
                    SettingsToggleRow::new(
                        "clipboard-watch",
                        "Clipboard URL watch",
                        clipboard_watch_enabled,
                        {
                            let app = app.clone();
                            move |on, window, cx| {
                                app.update(cx, |this, cx| {
                                    this.set_clipboard_watch_enabled(on, window, cx);
                                });
                            }
                        },
                    )
                    .hint("Offers clipboard HTTP(S) URLs on focus; never auto-downloads."),
                ),
            )
    }
}
