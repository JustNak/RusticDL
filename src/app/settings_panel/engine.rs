use gpui::{prelude::FluentBuilder, Context, IntoElement, ParentElement, Styled};
use gpui_component::{
    button::{Button, ButtonVariants},
    group_box::{GroupBox, GroupBoxVariants},
    h_flex, v_flex,
};

use super::super::widgets::{
    field_hint, settings_choice_row, settings_field_label, settings_input_with_reset,
    settings_subgroup,
};
use super::super::DownloadApp;
use crate::settings::Settings;

impl DownloadApp {
    pub(super) fn render_settings_engine(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
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
            if concurrent.saturating_mul(segs) > total {
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
                .child(settings_subgroup("Limits", false, cx))
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
                .child(settings_subgroup("Connections", true, cx))
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
                                    false,
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
                                    false,
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
                                    false,
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
                                    false,
                                )),
                        )
                })
                .child({
                    let enabled = self.draft_multi_connection_enabled;
                    settings_choice_row(
                        "Multi-connection",
                        Some("Use parallel Range requests for large files."),
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("multi-conn-off")
                                    .label("Off")
                                    .when(!enabled, |b| b.primary())
                                    .when(enabled, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_draft_multi_connection_enabled(
                                            false, window, cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("multi-conn-on")
                                    .label("On")
                                    .when(enabled, |b| b.primary())
                                    .when(!enabled, |b| b.outline())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_draft_multi_connection_enabled(true, window, cx);
                                    })),
                            ),
                        cx,
                    )
                })
                .when_some(budget_hint, |el, text| el.child(field_hint(text, cx))),
        )
    }
}
