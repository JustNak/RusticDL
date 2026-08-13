//! General settings category panel.

use gpui::{
    div, prelude::FluentBuilder, Context, IntoElement, ParentElement, SharedString, Styled,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    clipboard::Clipboard,
    group_box::{GroupBox, GroupBoxVariants},
    h_flex,
    input::Input,
    v_flex, ActiveTheme, Disableable, IconName, Sizable,
};

use super::super::widgets::{
    browse_directory, field_hint, settings_choice_row, settings_field_label,
    settings_input_with_reset, settings_subgroup,
};
use super::super::DownloadApp;
use crate::download::reveal_in_folder;
use crate::settings::{Settings, UpdateChannel};

impl DownloadApp {
    pub(super) fn render_settings_general(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let data_dir = self.paths.root.display().to_string();
        let update_channel = self.settings.update_channel;
        let update_busy = self.update_busy;
        let update_label = self.update_action_label();
        let multi_enabled = self.settings.multi_connection_enabled;
        let fsync_on_pause = self.settings.fsync_on_pause;
        // Derive from live drafts (same parse fallbacks as Save), not last-committed only.
        let budget_hint = {
            let concurrent = self
                .concurrent_input
                .read(cx)
                .value()
                .parse::<u32>()
                .unwrap_or(self.settings.max_concurrent_downloads)
                .clamp(1, 64);
            let segs = self
                .multi_max_segments_input
                .read(cx)
                .value()
                .parse::<u32>()
                .unwrap_or(self.settings.multi_max_segments)
                .clamp(1, 16);
            let total = self
                .max_total_connections_input
                .read(cx)
                .value()
                .parse::<u32>()
                .unwrap_or(self.settings.max_total_connections)
                .clamp(1, 256);
            if multi_enabled && concurrent.saturating_mul(segs) > total {
                Some(
                    "Max concurrent × multi segments exceeds total connections — segments will queue on budget.",
                )
            } else {
                None
            }
        };

        GroupBox::new().outline().child(
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
                .child({
                    let defaults = Settings::default();
                    let app = cx.entity();
                    let concurrent_val = self.concurrent_input.read(cx).value().to_string();
                    let retry_val = self.retry_input.read(cx).value().to_string();
                    let speed_val = self.speed_input.read(cx).value().to_string();
                    let concurrent_def = defaults.max_concurrent_downloads.to_string();
                    let retry_def = defaults.auto_retry_attempts.to_string();
                    let speed_def = defaults.speed_limit_kib_per_second.to_string();
                    h_flex()
                        .gap_4()
                        .items_start()
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1p5()
                                .child(settings_field_label("Max concurrent", cx))
                                .child(settings_input_with_reset(
                                    "reset-max-concurrent",
                                    &self.concurrent_input,
                                    &concurrent_val,
                                    &concurrent_def,
                                    concurrent_def.clone(),
                                    app.clone(),
                                    false,
                                )),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1p5()
                                .child(settings_field_label("Auto-retry attempts", cx))
                                .child(settings_input_with_reset(
                                    "reset-auto-retry",
                                    &self.retry_input,
                                    &retry_val,
                                    &retry_def,
                                    retry_def.clone(),
                                    app.clone(),
                                    false,
                                )),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1p5()
                                .child(settings_field_label("Speed limit (KiB/s)", cx))
                                .child(settings_input_with_reset(
                                    "reset-speed-limit",
                                    &self.speed_input,
                                    &speed_val,
                                    &speed_def,
                                    "0 = unlimited",
                                    app,
                                    false,
                                )),
                        )
                })
                .child(field_hint(
                    "Speed limit: 0 = unlimited, shared across all downloads. Brief bursts up to ~2× the rate (min 64 KiB) are normal after idle.",
                    cx,
                ))
                .child(settings_choice_row(
                    "Fsync on pause",
                    Some("Flush partial data to disk when pausing (safer on power loss)."),
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("fsync-pause-off")
                                .label("Off")
                                .when(!fsync_on_pause, |b| b.primary())
                                .when(fsync_on_pause, |b| b.outline())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_fsync_on_pause(false, window, cx);
                                })),
                        )
                        .child(
                            Button::new("fsync-pause-on")
                                .label("On")
                                .when(fsync_on_pause, |b| b.primary())
                                .when(!fsync_on_pause, |b| b.outline())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_fsync_on_pause(true, window, cx);
                                })),
                        ),
                    cx,
                ))
                .child(settings_choice_row(
                    "Multi-connection",
                    Some("Split large files across parallel Range connections. Off forces new jobs to single-stream."),
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("multi-conn-off")
                                .label("Off")
                                .when(!multi_enabled, |b| b.primary())
                                .when(multi_enabled, |b| b.outline())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_multi_connection_enabled(false, window, cx);
                                })),
                        )
                        .child(
                            Button::new("multi-conn-on")
                                .label("On")
                                .when(multi_enabled, |b| b.primary())
                                .when(!multi_enabled, |b| b.outline())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_multi_connection_enabled(true, window, cx);
                                })),
                        ),
                    cx,
                ))
                .child({
                    let defaults = Settings::default();
                    let app = cx.entity();
                    let segs_val = self.multi_max_segments_input.read(cx).value().to_string();
                    let mib_val = self.multi_min_mib_input.read(cx).value().to_string();
                    let total_val = self
                        .max_total_connections_input
                        .read(cx)
                        .value()
                        .to_string();
                    let host_val = self
                        .max_connections_per_host_input
                        .read(cx)
                        .value()
                        .to_string();
                    let segs_def = defaults.multi_max_segments.to_string();
                    let mib_def = (defaults.multi_min_bytes / (1024 * 1024))
                        .max(1)
                        .to_string();
                    let total_def = defaults.max_total_connections.to_string();
                    let host_def = defaults.max_connections_per_host.to_string();
                    h_flex()
                        .gap_4()
                        .items_start()
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1p5()
                                .child(settings_field_label("Max segments", cx))
                                .child(settings_input_with_reset(
                                    "reset-multi-max-segments",
                                    &self.multi_max_segments_input,
                                    &segs_val,
                                    &segs_def,
                                    segs_def.clone(),
                                    app.clone(),
                                    !multi_enabled,
                                )),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1p5()
                                .child(settings_field_label("Min size (MiB)", cx))
                                .child(settings_input_with_reset(
                                    "reset-multi-min-mib",
                                    &self.multi_min_mib_input,
                                    &mib_val,
                                    &mib_def,
                                    mib_def.clone(),
                                    app.clone(),
                                    !multi_enabled,
                                )),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1p5()
                                .child(settings_field_label("Total connections", cx))
                                .child(settings_input_with_reset(
                                    "reset-max-total-connections",
                                    &self.max_total_connections_input,
                                    &total_val,
                                    &total_def,
                                    total_def.clone(),
                                    app.clone(),
                                    !multi_enabled,
                                )),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1p5()
                                .child(settings_field_label("Per-host connections", cx))
                                .child(settings_input_with_reset(
                                    "reset-max-conn-per-host",
                                    &self.max_connections_per_host_input,
                                    &host_val,
                                    &host_def,
                                    host_def.clone(),
                                    app,
                                    !multi_enabled,
                                )),
                        )
                })
                .when_some(budget_hint, |el, text| el.child(field_hint(text, cx)))
                .child(settings_subgroup("Updates", true, cx))
                .child(settings_choice_row(
                    "Check for updates",
                    Some("Same check as the brand menu and About dialog."),
                    Button::new("settings-check-updates")
                        .outline()
                        .label(update_label)
                        .disabled(update_busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.begin_update_action(window, cx);
                        })),
                    cx,
                ))
                .child(settings_choice_row(
                    "Update channel",
                    Some("Stable = latest release. Nightly = newest pre-release with a setup."),
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("update-channel-stable")
                                .label(UpdateChannel::Stable.label())
                                .when(update_channel == UpdateChannel::Stable, |b| b.primary())
                                .when(update_channel != UpdateChannel::Stable, |b| b.outline())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_update_channel(UpdateChannel::Stable, window, cx);
                                })),
                        )
                        .child(
                            Button::new("update-channel-nightly")
                                .label(UpdateChannel::Nightly.label())
                                .when(update_channel == UpdateChannel::Nightly, |b| b.primary())
                                .when(update_channel != UpdateChannel::Nightly, |b| b.outline())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_update_channel(UpdateChannel::Nightly, window, cx);
                                })),
                        ),
                    cx,
                ))
                .child(settings_subgroup("App data", true, cx))
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
                                            if let Err(msg) = reveal_in_folder(&this.paths.root) {
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
