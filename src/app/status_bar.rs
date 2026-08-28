
use gpui::{div, prelude::FluentBuilder, px, Context, IntoElement, ParentElement, Styled};
use gpui_component::{h_flex, ActiveTheme};

use super::widgets::status_chip;
use super::DownloadApp;
use crate::download::JobState;
use crate::format::{
    count_jobs, format_bytes, format_speed, total_completed_bytes, total_download_speed,
};

impl DownloadApp {
    pub(crate) fn render_status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (all, active, completed, failed) = count_jobs(&self.jobs);
        let speed = total_download_speed(&self.jobs);
        let completed_bytes = total_completed_bytes(&self.jobs);
        let downloading = self
            .jobs
            .iter()
            .filter(|j| matches!(j.state, JobState::Downloading | JobState::Starting))
            .count();
        let limit = self.settings.speed_limit_kib_per_second;

        h_flex()
            .h(px(30.))
            .flex_shrink_0()
            .px_4()
            .gap_3()
            .items_center()
            .overflow_x_hidden()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.secondary.opacity(0.4))
            .child(status_chip(format!("{all} total"), theme.muted_foreground))
            .child(status_chip(
                format!("{active} active"),
                if active > 0 {
                    theme.primary
                } else {
                    theme.muted_foreground
                },
            ))
            .child(status_chip(
                format!("{completed} done"),
                if completed > 0 {
                    theme.success
                } else {
                    theme.muted_foreground
                },
            ))
            .when(failed > 0, |el| {
                el.child(status_chip(format!("{failed} failed"), theme.danger))
            })
            .child(div().flex_1())
            .when(completed_bytes > 0, |el| {
                el.child(status_chip(
                    format!("{} saved", format_bytes(completed_bytes)),
                    theme.muted_foreground,
                ))
            })
            .when(limit > 0, |el| {
                el.child(status_chip(format!("Limit {} KiB/s", limit), theme.warning))
            })
            .child(status_chip(
                if downloading > 0 {
                    format!("↓ {}", format_speed(speed))
                } else {
                    "Idle".into()
                },
                if downloading > 0 {
                    theme.foreground
                } else {
                    theme.muted_foreground
                },
            ))
    }
}
