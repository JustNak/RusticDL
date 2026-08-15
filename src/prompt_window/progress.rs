//! Progress phase: construct, engine controls, sync, and render.

use std::collections::VecDeque;

use gpui::{div, prelude::FluentBuilder, Context, IntoElement, ParentElement, Styled, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, StyledExt,
};

use super::helpers::{capture_progress_bar, speed_sparkline};
use super::start_sync_timer;
use super::{
    BrowserPromptWindow, CapturePhase, CAPTURE_COMPLETE_H, CAPTURE_WINDOW_H, SPEED_SAMPLE_CAP,
};
use crate::appearance::apply_appearance;
use crate::download::{EngineCommand, EngineHandle, JobState};
use crate::format::{format_eta, format_size, format_speed};
use crate::ipc::IpcBridge;
use crate::settings::Settings;
use crate::window_icon::apply_app_icon;

impl BrowserPromptWindow {
    pub(super) fn new_progress(
        job_id: String,
        ipc: IpcBridge,
        engine: EngineHandle,
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        apply_appearance(settings, Some(window), cx);
        apply_app_icon(window);

        let job = ipc.job_by_id(&job_id);
        let url = job.as_ref().map(|j| j.url.clone()).unwrap_or_default();

        // Promote to Complete immediately if already finished.
        let phase = if let Some(j) = job.as_ref() {
            if j.state == JobState::Completed {
                let _ = ipc.try_claim_complete_hud(&job_id);
                CapturePhase::Complete {
                    job_id: job_id.clone(),
                    filename: j.filename.clone(),
                    target_path: j.target_path.clone(),
                    total_bytes: j.total_bytes,
                }
            } else {
                CapturePhase::Progress {
                    job_id: Some(job_id.clone()),
                    url,
                }
            }
        } else {
            CapturePhase::Progress {
                job_id: Some(job_id),
                url,
            }
        };

        let mut speed_samples = VecDeque::with_capacity(SPEED_SAMPLE_CAP);
        let mut peak_speed = 0u64;
        if let Some(j) = job.as_ref() {
            if matches!(j.state, JobState::Downloading | JobState::Starting) && j.speed > 0 {
                speed_samples.push_back(j.speed);
                peak_speed = j.speed;
            }
        }

        window.activate_window();
        if matches!(phase, CapturePhase::Progress { .. }) {
            start_sync_timer(cx);
        }

        let fitted_height = Some(if matches!(phase, CapturePhase::Complete { .. }) {
            CAPTURE_COMPLETE_H
        } else {
            CAPTURE_WINDOW_H
        });

        Self {
            phase,
            prompt: None,
            ipc,
            engine,
            progress_style: settings.progress_style,
            name_input: None,
            dir_input: None,
            job,
            action_error: None,
            resolved: true,
            waiting_url_noted: false,
            canceling: false,
            speed_samples,
            peak_speed,
            reduce_motion: settings.reduce_motion,
            fitted_height,
        }
    }

    pub(super) fn pause(&mut self, cx: &mut Context<Self>) {
        if let CapturePhase::Progress {
            job_id: Some(id), ..
        } = &self.phase
        {
            self.engine.send(EngineCommand::Pause(id.clone()));
            cx.notify();
        }
    }

    pub(super) fn resume(&mut self, cx: &mut Context<Self>) {
        if let CapturePhase::Progress {
            job_id: Some(id), ..
        } = &self.phase
        {
            self.engine.send(EngineCommand::Resume(id.clone()));
            cx.notify();
        }
    }

    pub(super) fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.canceling {
            return;
        }
        if let CapturePhase::Progress {
            job_id: Some(id), ..
        } = &self.phase
        {
            self.canceling = true;
            // Match main-queue cancel: keep .part so resume/retry remains possible.
            self.engine.send(EngineCommand::Cancel {
                id: id.clone(),
                delete_partial: false,
            });
            self.release_ownership();
            window.remove_window();
            cx.notify();
        }
    }

    pub(super) fn retry(&mut self, cx: &mut Context<Self>) {
        if let CapturePhase::Progress {
            job_id: Some(id), ..
        } = &self.phase
        {
            self.engine.send(EngineCommand::Retry(id.clone()));
            self.canceling = false;
            cx.notify();
        }
    }

    /// Refresh job snapshot; bind id; morph to Complete when done.
    pub(super) fn sync_from_bridge(&mut self, cx: &mut Context<Self>) {
        match &self.phase {
            CapturePhase::Progress { job_id, url } => {
                let mut bound_id = job_id.clone();
                if bound_id.is_none() {
                    // Bind only active jobs; prefer newest so same-URL re-downloads
                    // do not attach to an older Completed/Failed row.
                    let snapshot = self.ipc.jobs_snapshot();
                    if let Some(j) = snapshot
                        .iter()
                        .filter(|j| j.url == *url && j.state.is_active())
                        .max_by_key(|j| j.created_at)
                    {
                        if self.ipc.try_own_progress_job(&j.id) {
                            bound_id = Some(j.id.clone());
                            if self.waiting_url_noted {
                                self.ipc.clear_progress_waiting_url(url);
                                self.waiting_url_noted = false;
                            }
                        }
                    }
                }

                if let Some(id) = bound_id.clone() {
                    if let Some(j) = self.ipc.job_by_id(&id) {
                        if matches!(j.state, JobState::Downloading | JobState::Starting) {
                            self.push_speed_sample(j.speed);
                        } else if j.state == JobState::Paused {
                            // Freeze chart — do not push new samples while paused.
                        }
                        if j.state == JobState::Completed {
                            let _ = self.ipc.try_claim_complete_hud(&id);
                            self.phase = CapturePhase::Complete {
                                job_id: id,
                                filename: j.filename.clone(),
                                target_path: j.target_path.clone(),
                                total_bytes: j.total_bytes,
                            };
                            self.job = Some(j);
                            cx.notify();
                            return;
                        }
                        self.job = Some(j);
                        // Canceled/Failed: keep HUD open so Retry stays available.
                    }
                }

                if let CapturePhase::Progress {
                    job_id: ref mut slot,
                    ..
                } = self.phase
                {
                    *slot = bound_id;
                }
                cx.notify();
            }
            CapturePhase::Complete { job_id, .. } => {
                // Keep snapshot fresh for path existence checks.
                if let Some(j) = self.ipc.job_by_id(job_id) {
                    self.job = Some(j);
                }
            }
            CapturePhase::Confirm | CapturePhase::Conflict => {}
        }
    }

    pub(super) fn render_progress(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let progress_style = self.progress_style;

        let (
            filename,
            progress,
            size_line,
            speed_line,
            state_label,
            can_pause,
            can_resume,
            can_retry,
            error,
        ) = if let Some(job) = &self.job {
            let progress = job.progress as f32;
            let size_line = format_size(job);
            let speed_line = format!(
                "{} · ETA {}",
                format_speed(job.speed),
                format_eta(job.eta_secs)
            );
            let can_pause = matches!(
                job.state,
                JobState::Queued | JobState::Starting | JobState::Downloading
            );
            let can_resume = job.state == JobState::Paused;
            let can_retry = matches!(job.state, JobState::Failed | JobState::Canceled);
            (
                job.filename.clone(),
                progress,
                size_line,
                speed_line,
                job.state.label().to_string(),
                can_pause,
                can_resume,
                can_retry,
                job.error.clone(),
            )
        } else {
            (
                "Starting…".into(),
                0.0_f32,
                "—".into(),
                "—".into(),
                "Starting".into(),
                false,
                false,
                false,
                None,
            )
        };

        let progress_color = if self
            .job
            .as_ref()
            .is_some_and(|j| j.state == JobState::Paused)
        {
            theme.warning
        } else if self
            .job
            .as_ref()
            .is_some_and(|j| j.state == JobState::Failed)
        {
            theme.danger
        } else {
            theme.progress_bar
        };

        let samples: Vec<u64> = self.speed_samples.iter().copied().collect();
        let spark_status = if self
            .job
            .as_ref()
            .is_some_and(|j| j.state == JobState::Paused)
        {
            "paused"
        } else if samples.iter().any(|&s| s > 0) {
            "live"
        } else {
            "waiting…"
        };

        v_flex()
            .gap_2()
            .size_full()
            .child(
                v_flex()
                    .gap_1()
                    .flex_shrink_0()
                    .child(div().text_sm().font_medium().child(filename))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("{state_label} · {size_line}")),
                    ),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .flex_shrink_0()
                    .child(capture_progress_bar(
                        progress,
                        progress_color,
                        progress_style,
                    ))
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{progress:.0}%")),
                            )
                            .child(div().text_xs().text_color(muted).child(speed_line)),
                    ),
            )
            .when_some(error, |el, msg| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(theme.danger)
                        .flex_shrink_0()
                        .child(msg),
                )
            })
            // Live speed sparkline fills the empty band under the progress row.
            .child(speed_sparkline(
                &samples,
                self.peak_speed,
                progress_color,
                muted,
                &theme,
                self.reduce_motion,
                spark_status,
            ))
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .pt_1()
                    .flex_shrink_0()
                    .when(can_pause, |el| {
                        el.child(
                            Button::new("capture-pause")
                                .label("Pause")
                                .outline()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.pause(cx);
                                })),
                        )
                    })
                    .when(can_resume, |el| {
                        el.child(
                            Button::new("capture-resume")
                                .label("Resume")
                                .outline()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.resume(cx);
                                })),
                        )
                    })
                    .when(can_retry, |el| {
                        el.child(
                            Button::new("capture-retry")
                                .label("Retry")
                                .outline()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.retry(cx);
                                })),
                        )
                    })
                    .when(
                        self.job
                            .as_ref()
                            .is_some_and(|j| !j.state.is_terminal() || j.state == JobState::Paused)
                            && !self.canceling,
                        |el| {
                            el.child(
                                Button::new("capture-cancel")
                                    .label("Cancel")
                                    .danger()
                                    .outline()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel(window, cx);
                                    })),
                            )
                        },
                    ),
            )
            .into_any_element()
    }
}
