use gpui::{
    div, prelude::FluentBuilder, px, Context, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    clipboard::Clipboard,
    h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt, Theme,
};

use super::job_row::restart_icon;
use super::widgets::{ellipsize_name, soft_tooltip, status_color, status_tag};
use super::DownloadApp;
use crate::download::{
    fallback_reason_label, open_path, reveal_in_folder, EngineCommand, Job, JobState,
};
use crate::format::{format_eta, format_size, format_speed};

/// Inline “Label value” pair used in the detail meta row (no card chrome).
pub(crate) fn detail_pair(
    label: &'static str,
    value: impl Into<SharedString>,
    theme: &Theme,
) -> impl IntoElement {
    let value = value.into();
    let is_placeholder = value.as_ref() == "—" || value.as_ref().is_empty();
    let value_color = if is_placeholder {
        theme.muted_foreground.opacity(0.7)
    } else {
        theme.foreground
    };
    h_flex()
        .gap_2()
        .items_baseline()
        .flex_shrink_0()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .text_xs()
                .font_medium()
                .text_color(value_color)
                .whitespace_nowrap()
                .child(value),
        )
}

/// Thin vertical rule between meta pairs — same language as the status bar separators.
pub(crate) fn detail_meta_sep(theme: &Theme) -> impl IntoElement {
    div()
        .w(px(1.))
        .h(px(14.))
        .flex_shrink_0()
        .mx_0p5()
        .bg(theme.border.opacity(0.85))
}

pub(crate) fn render_detail(
    job: &Job,
    max_h: f32,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let tone = job.state.tone();
    let accent = status_color(tone, &theme);
    let size = format_size(job);
    let speed = if matches!(job.state, JobState::Downloading | JobState::Starting) {
        format_speed(job.speed)
    } else {
        "—".into()
    };
    let eta = if job.state == JobState::Downloading {
        format_eta(job.eta_secs)
    } else {
        "—".into()
    };
    let resume = if job.resume_supported {
        "Supported"
    } else {
        "Unavailable"
    };
    let progress = format!("{:.1}%", job.progress);
    let retries = job.retry_attempts.to_string();
    let mode = job
        .transfer_mode
        .map(|m| m.label().to_string())
        .unwrap_or_else(|| "—".into());
    let connections = job.active_connections.to_string();
    let reconnects = job.reconnect_count.to_string();
    let fallback = job.fallback_reason.clone();
    let path = job.target_path.to_string_lossy().to_string();
    let path_tip: SharedString = path.clone().into();
    let tip_color = theme.muted_foreground;
    let url = job.url.clone();
    let error = job.error.clone();
    let id = job.id.clone();
    let filename = job.filename.clone();
    let filename_tip: SharedString = job.filename.clone().into();

    let can_pause = matches!(
        job.state,
        JobState::Queued | JobState::Starting | JobState::Downloading
    );
    let can_resume = job.state == JobState::Paused;
    let can_retry = matches!(job.state, JobState::Failed | JobState::Canceled);
    // Restart wipes partial progress and starts from zero — only useful after a
    // failed or canceled transfer, not on completed jobs.
    let can_restart = matches!(job.state, JobState::Failed | JobState::Canceled);
    let can_open = job.state == JobState::Completed && job.target_path.exists();
    let can_remove = job.is_removable();
    let can_delete = job.has_deletable_file();
    let can_cancel = !job.state.is_terminal() && job.state != JobState::Paused;

    // Height-capped inspector: scrolls internally so the job list keeps space.
    // Flat surfaces only — hierarchy comes from type and a single top border, not nested cards.
    v_flex()
        .id("job-detail")
        .flex_shrink_0()
        .max_h(px(max_h))
        .min_h_0()
        .border_t_1()
        .border_color(theme.border)
        .bg(theme.secondary.opacity(0.28))
        .child(
            div()
                .id("job-detail-scroll")
                .max_h(px(max_h))
                .min_h_0()
                .overflow_y_scroll()
                .px_5()
                .pt_3()
                .pb_3()
                .child(
                    v_flex()
                        .gap_3()
                        // ── Header ──
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_2p5()
                                .items_center()
                                .child(
                                    Icon::new(match job.state {
                                        JobState::Completed => IconName::CircleCheck,
                                        JobState::Failed | JobState::Canceled => {
                                            IconName::TriangleAlert
                                        }
                                        JobState::Paused => IconName::Minus,
                                        _ => IconName::File,
                                    })
                                    .with_size(px(16.))
                                    .text_color(accent)
                                    .flex_shrink_0(),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .gap_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .child(
                                            // Soft character clamp — GPUI text-overflow is unreliable
                                            // in nested flex, same approach as the queue Name column.
                                            div()
                                                .id(SharedString::from(format!(
                                                    "detail-name-{}",
                                                    job.id
                                                )))
                                                .min_w_0()
                                                .text_sm()
                                                .font_semibold()
                                                .text_color(theme.foreground)
                                                .child(ellipsize_name(&job.filename, 72))
                                                .tooltip(move |window, cx| {
                                                    soft_tooltip(
                                                        filename_tip.clone(),
                                                        tip_color,
                                                        window,
                                                        cx,
                                                    )
                                                }),
                                        )
                                        .child(
                                            h_flex()
                                                .gap_1p5()
                                                .items_center()
                                                .min_w_0()
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .overflow_hidden()
                                                        .whitespace_nowrap()
                                                        .text_ellipsis()
                                                        .text_xs()
                                                        .text_color(theme.muted_foreground)
                                                        .child(url.clone()),
                                                )
                                                .child(
                                                    Clipboard::new(SharedString::from(format!(
                                                        "copy-url-{}",
                                                        job.id
                                                    )))
                                                    .value(SharedString::from(url.clone())),
                                                ),
                                        ),
                                )
                                .child(status_tag(job.state.label(), tone))
                                .child(
                                    Button::new("detail-close")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Close)
                                        .tooltip("Hide details")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.clear_selection();
                                            cx.notify();
                                        })),
                                ),
                        )
                        // ── Meta row: inline label/value pairs ──
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_3()
                                .items_center()
                                .flex_wrap()
                                .child(detail_pair("Size", size, &theme))
                                .child(detail_meta_sep(&theme))
                                .child(detail_pair("Speed", speed, &theme))
                                .child(detail_meta_sep(&theme))
                                .child(detail_pair("ETA", eta, &theme))
                                .child(detail_meta_sep(&theme))
                                .child(detail_pair("Progress", progress, &theme))
                                .child(detail_meta_sep(&theme))
                                .child(detail_pair("Resume", resume, &theme))
                                .child(detail_meta_sep(&theme))
                                .child(detail_pair("Retries", retries, &theme))
                                .child(detail_meta_sep(&theme))
                                .child(detail_pair("Mode", mode, &theme))
                                .child(detail_meta_sep(&theme))
                                .child(detail_pair("Connections", connections, &theme))
                                .child(detail_meta_sep(&theme))
                                .child(detail_pair("Reconnects", reconnects, &theme)),
                        )
                        .when_some(fallback, |el, reason| {
                            el.child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap_2()
                                    .items_start()
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("Fallback"),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_xs()
                                            .text_color(theme.foreground)
                                            .child(fallback_reason_label(&reason).to_string()),
                                    ),
                            )
                        })
                        // ── Path ──
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_3()
                                .items_center()
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("Path"),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("detail-path-{}", job.id)))
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_xs()
                                        .text_color(theme.foreground)
                                        .child(path.clone())
                                        .tooltip(move |window, cx| {
                                            soft_tooltip(path_tip.clone(), tip_color, window, cx)
                                        }),
                                )
                                .child(
                                    Clipboard::new(SharedString::from(format!(
                                        "detail-copy-path-{}",
                                        id
                                    )))
                                    .value(SharedString::from(path.clone())),
                                ),
                        )
                        .when_some(error, |el, err| {
                            // Error keeps a light tint — semantic, not decorative card chrome.
                            el.child(
                                h_flex()
                                    .gap_2()
                                    .items_start()
                                    .child(
                                        Icon::new(IconName::TriangleAlert)
                                            .with_size(px(14.))
                                            .text_color(theme.danger),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_xs()
                                            .text_color(theme.danger)
                                            .child(err),
                                    ),
                            )
                        })
                        // ── Actions ──
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .flex_wrap()
                                .pt_1()
                                .when(can_pause, |el| {
                                    let id = id.clone();
                                    el.child(
                                        Button::new("detail-pause")
                                            .outline()
                                            .small()
                                            .icon(IconName::Minus)
                                            .label("Pause")
                                            .on_click(cx.listener(move |this, _, _, _| {
                                                this.engine.send(EngineCommand::Pause(id.clone()));
                                            })),
                                    )
                                })
                                .when(can_resume, |el| {
                                    let id = id.clone();
                                    el.child(
                                        Button::new("detail-resume")
                                            .outline()
                                            .small()
                                            .icon(IconName::Redo2)
                                            .label("Resume")
                                            .on_click(cx.listener(move |this, _, _, _| {
                                                this.engine.send(EngineCommand::Resume(id.clone()));
                                            })),
                                    )
                                })
                                .when(can_retry, |el| {
                                    let id = id.clone();
                                    el.child(
                                        Button::new("detail-retry")
                                            .outline()
                                            .small()
                                            .icon(IconName::Redo)
                                            .label("Retry")
                                            .on_click(cx.listener(move |this, _, _, _| {
                                                this.engine.send(EngineCommand::Retry(id.clone()));
                                            })),
                                    )
                                })
                                .when(can_restart, |el| {
                                    let id = id.clone();
                                    el.child(
                                        Button::new("detail-restart")
                                            .outline()
                                            .small()
                                            .icon(restart_icon())
                                            .label("Restart")
                                            .tooltip("Discard progress and download from the start")
                                            .on_click(cx.listener(move |this, _, _, _| {
                                                this.engine
                                                    .send(EngineCommand::Restart(id.clone()));
                                            })),
                                    )
                                })
                                .when(can_cancel, |el| {
                                    let id = id.clone();
                                    el.child(
                                        Button::new("detail-cancel")
                                            .outline()
                                            .small()
                                            .danger()
                                            .icon(IconName::Close)
                                            .label("Cancel")
                                            .on_click(cx.listener(move |this, _, _, _| {
                                                this.engine.send(EngineCommand::Cancel {
                                                    id: id.clone(),
                                                    delete_partial: false,
                                                });
                                            })),
                                    )
                                })
                                .when(can_open, |el| {
                                    let id = id.clone();
                                    el.child(
                                        Button::new("detail-open")
                                            .outline()
                                            .small()
                                            .icon(IconName::ExternalLink)
                                            .label("Open")
                                            .on_click(cx.listener(move |this, _, _window, cx| {
                                                if let Some(job) =
                                                    this.jobs.iter().find(|j| j.id == id)
                                                {
                                                    if let Err(msg) = open_path(&job.target_path) {
                                                        this.show_toast(msg, cx);
                                                    }
                                                }
                                            })),
                                    )
                                })
                                .child({
                                    let id = id.clone();
                                    Button::new("detail-reveal")
                                        .outline()
                                        .small()
                                        .icon(IconName::FolderOpen)
                                        .label("Open")
                                        .tooltip("Open containing folder")
                                        .on_click(cx.listener(move |this, _, _window, cx| {
                                            if let Some(job) = this.jobs.iter().find(|j| j.id == id)
                                            {
                                                let path = if job.target_path.exists() {
                                                    job.target_path.clone()
                                                } else {
                                                    job.temp_path.clone()
                                                };
                                                if let Err(msg) = reveal_in_folder(&path) {
                                                    this.show_toast(msg, cx);
                                                }
                                            }
                                        }))
                                })
                                .when(can_remove, |el| {
                                    let id = id.clone();
                                    let filename = filename.clone();
                                    el.child(
                                        Button::new("detail-remove")
                                            .outline()
                                            .small()
                                            .icon(IconName::Close)
                                            .label("Remove")
                                            .tooltip("Remove from queue, keep the file")
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.confirm_remove(
                                                    id.clone(),
                                                    filename.clone(),
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    )
                                })
                                .when(can_delete, |el| {
                                    let id = id.clone();
                                    let filename = filename.clone();
                                    el.child(
                                        Button::new("detail-delete")
                                            .danger()
                                            .small()
                                            .icon(IconName::Delete)
                                            .label("Delete")
                                            .tooltip("Delete the file from disk")
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.confirm_delete(
                                                    id.clone(),
                                                    filename.clone(),
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    )
                                }),
                        ),
                ),
        )
}
