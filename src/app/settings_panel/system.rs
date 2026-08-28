use gpui::{prelude::FluentBuilder, px, Context, IntoElement, ParentElement, Styled};
use gpui_component::{
    button::{Button, ButtonVariants},
    group_box::{GroupBox, GroupBoxVariants},
    h_flex, v_flex, Disableable,
};

use super::super::widgets::{settings_choice_row, settings_subgroup};
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

        GroupBox::new().outline().child(
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
                        .child(
                            Button::new("os-notify-off")
                                .label(OsNotifyMode::Off.label())
                                .min_w(px(108.))
                                .when(os_notify_mode == OsNotifyMode::Off, |b| b.primary())
                                .when(os_notify_mode != OsNotifyMode::Off, |b| b.outline())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_os_notify_mode(OsNotifyMode::Off, window, cx);
                                })),
                        )
                        .child(
                            Button::new("os-notify-when-hidden")
                                .label(OsNotifyMode::WhenHiddenToTray.label())
                                .min_w(px(108.))
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
                                .min_w(px(108.))
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
}
