//! Floating browser capture HUD: Confirm → Progress → Complete.
//!
//! Lives independent of the main queue window so handoff still works when the
//! main UI is minimized or hidden.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    div, prelude::FluentBuilder, px, size, App, AppContext, Bounds, Context, Entity, Hsla,
    InteractiveElement, IntoElement, ParentElement, PathPromptOptions, Render, SharedString,
    Styled, Window, WindowBounds, WindowDecorations, WindowHandle, WindowKind, WindowOptions,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    progress::Progress,
    v_flex, ActiveTheme, Disableable, Icon, IconName, Root, Sizable, StyledExt,
};

use crate::appearance::apply_appearance;
use crate::branding::APP_NAME;
use crate::browser_popup_chrome::themed_popup_title_bar;
use crate::download::{open_path, reveal_in_folder, EngineCommand, EngineHandle, Job, JobState};
use crate::format::{format_bytes, format_eta, format_size, format_speed};
use crate::ipc::{BrowserPromptView, IpcBridge, PromptDecision};
use crate::settings::{ProgressStyle, Settings};
use crate::window_icon::apply_app_icon;
use crate::window_placement::center_window;

const CAPTURE_WINDOW_W: f32 = 480.0;
const CAPTURE_WINDOW_H: f32 = 320.0;

/// Phase of the floating capture HUD.
#[derive(Debug, Clone)]
enum CapturePhase {
    /// Ask-mode confirmation form.
    Confirm,
    /// Waiting for engine job id after Accept (or bound and downloading).
    Progress {
        /// Bound job id once known.
        job_id: Option<String>,
        /// URL used to match the new job after Accept.
        url: String,
    },
    /// Successful terminal surface.
    Complete {
        job_id: String,
        filename: String,
        target_path: PathBuf,
        total_bytes: u64,
    },
}

/// Root content of the browser capture window.
pub struct BrowserPromptWindow {
    phase: CapturePhase,
    /// Present only in Confirm (and briefly during morph setup).
    prompt: Option<BrowserPromptView>,
    ipc: IpcBridge,
    engine: EngineHandle,
    progress_style: ProgressStyle,
    name_input: Option<Entity<InputState>>,
    dir_input: Option<Entity<InputState>>,
    /// Live job snapshot for Progress (refreshed on timer).
    job: Option<Job>,
    /// Last error toast surface for open/reveal failures.
    action_error: Option<String>,
    /// Prevent double resolve if both button and window-close fire.
    resolved: bool,
    /// True once we registered a waiting URL for morph ownership.
    waiting_url_noted: bool,
    /// Progress HUD cancel in-flight (disable double-click).
    canceling: bool,
}

impl BrowserPromptWindow {
    fn new_confirm(
        prompt: BrowserPromptView,
        ipc: IpcBridge,
        engine: EngineHandle,
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        apply_appearance(settings, Some(window), cx);
        apply_app_icon(window);

        let default_name = prompt
            .suggested_filename
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| {
                crate::download::filesystem::derive_filename_from_url(&prompt.url)
                    .unwrap_or_else(|| "download.bin".into())
            });

        let name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Filename")
                .default_value(default_name)
        });
        let dir_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Download directory")
                .default_value(prompt.default_directory.to_string_lossy().to_string())
        });

        window.activate_window();
        start_sync_timer(cx);

        Self {
            phase: CapturePhase::Confirm,
            prompt: Some(prompt),
            ipc,
            engine,
            progress_style: settings.progress_style,
            name_input: Some(name_input),
            dir_input: Some(dir_input),
            job: None,
            action_error: None,
            resolved: false,
            waiting_url_noted: false,
            canceling: false,
        }
    }

    fn new_progress(
        job_id: String,
        ipc: IpcBridge,
        engine: EngineHandle,
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        apply_appearance(settings, Some(window), cx);
        apply_app_icon(window);

        let job = ipc
            .jobs_snapshot()
            .into_iter()
            .find(|j| j.id == job_id);
        let url = job
            .as_ref()
            .map(|j| j.url.clone())
            .unwrap_or_default();

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

        window.activate_window();
        start_sync_timer(cx);

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
        }
    }

    fn new_complete(
        job: Job,
        ipc: IpcBridge,
        engine: EngineHandle,
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        apply_appearance(settings, Some(window), cx);
        apply_app_icon(window);
        window.activate_window();
        start_sync_timer(cx);

        Self {
            phase: CapturePhase::Complete {
                job_id: job.id.clone(),
                filename: job.filename.clone(),
                target_path: job.target_path.clone(),
                total_bytes: job.total_bytes,
            },
            prompt: None,
            ipc,
            engine,
            progress_style: settings.progress_style,
            name_input: None,
            dir_input: None,
            job: Some(job),
            action_error: None,
            resolved: true,
            waiting_url_noted: false,
            canceling: false,
        }
    }

    fn dismiss_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.resolved {
            return;
        }
        self.resolved = true;
        if let Some(prompt) = &self.prompt {
            let _ = self
                .ipc
                .resolve_prompt(&prompt.id, PromptDecision::Dismiss);
        }
        window.remove_window();
        cx.notify();
    }

    /// Dismiss without `remove_window` — used from `on_window_should_close`.
    fn dismiss_confirm_on_close(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.phase, CapturePhase::Confirm) && !self.resolved {
            self.resolved = true;
            if let Some(prompt) = &self.prompt {
                let _ = self
                    .ipc
                    .resolve_prompt(&prompt.id, PromptDecision::Dismiss);
            }
        }
        self.release_ownership();
        cx.notify();
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.resolved {
            return;
        }
        self.resolved = true;

        let prompt = match &self.prompt {
            Some(p) => p.clone(),
            None => {
                window.remove_window();
                return;
            }
        };

        let filename = self.name_input.as_ref().and_then(|input| {
            let raw = input.read(cx).value().to_string();
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        let directory = self
            .dir_input
            .as_ref()
            .map(|input| PathBuf::from(input.read(cx).value().to_string()));

        let _ = self.ipc.resolve_prompt(
            &prompt.id,
            PromptDecision::Accept {
                filename,
                directory,
            },
        );

        if self.ipc.show_progress_after_handoff() {
            // Morph Confirm → Progress; bind job by URL when enqueue completes.
            self.ipc.note_progress_waiting_url(&prompt.url);
            self.waiting_url_noted = true;
            self.phase = CapturePhase::Progress {
                job_id: None,
                url: prompt.url.clone(),
            };
            self.name_input = None;
            self.dir_input = None;
            self.prompt = None;
            cx.notify();
        } else {
            window.remove_window();
            cx.notify();
        }
    }

    fn browse_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.dir_input.clone() else {
            return;
        };
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(SharedString::from("Select Folder")),
        });

        window
            .spawn(cx, async move |cx| {
                if let Ok(Ok(Some(paths))) = rx.await {
                    if let Some(path) = paths.into_iter().next() {
                        let _ = cx.update(|window, cx| {
                            input.update(cx, |state, cx| {
                                state.set_value(path.to_string_lossy().to_string(), window, cx);
                            });
                        });
                    }
                }
            })
            .detach();
    }

    fn release_ownership(&mut self) {
        if let CapturePhase::Progress { job_id, url } = &self.phase {
            if let Some(id) = job_id {
                self.ipc.release_progress_job(id);
            }
            if self.waiting_url_noted {
                self.ipc.clear_progress_waiting_url(url);
                self.waiting_url_noted = false;
            }
        } else if let CapturePhase::Complete { job_id, .. } = &self.phase {
            self.ipc.release_progress_job(job_id);
        }
    }

    fn close_hud(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.phase {
            CapturePhase::Confirm => {
                self.dismiss_confirm(window, cx);
            }
            CapturePhase::Progress { .. } | CapturePhase::Complete { .. } => {
                // Progress close ≠ cancel; Complete close just dismisses.
                self.release_ownership();
                window.remove_window();
                cx.notify();
            }
        }
    }

    fn close_hud_on_native_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.phase {
            CapturePhase::Confirm => self.dismiss_confirm_on_close(window, cx),
            CapturePhase::Progress { .. } | CapturePhase::Complete { .. } => {
                self.release_ownership();
                cx.notify();
            }
        }
    }

    fn pause(&mut self, cx: &mut Context<Self>) {
        if let CapturePhase::Progress {
            job_id: Some(id), ..
        } = &self.phase
        {
            self.engine.send(EngineCommand::Pause(id.clone()));
            cx.notify();
        }
    }

    fn resume(&mut self, cx: &mut Context<Self>) {
        if let CapturePhase::Progress {
            job_id: Some(id), ..
        } = &self.phase
        {
            self.engine.send(EngineCommand::Resume(id.clone()));
            cx.notify();
        }
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.canceling {
            return;
        }
        if let CapturePhase::Progress {
            job_id: Some(id), ..
        } = &self.phase
        {
            self.canceling = true;
            self.engine.send(EngineCommand::Cancel {
                id: id.clone(),
                delete_partial: true,
            });
            self.release_ownership();
            window.remove_window();
            cx.notify();
        }
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        if let CapturePhase::Progress {
            job_id: Some(id), ..
        } = &self.phase
        {
            self.engine.send(EngineCommand::Retry(id.clone()));
            self.canceling = false;
            cx.notify();
        }
    }

    fn open_file(&mut self, cx: &mut Context<Self>) {
        let path = match &self.phase {
            CapturePhase::Complete { target_path, .. } => target_path.clone(),
            _ => return,
        };
        if let Err(msg) = open_path(&path) {
            self.action_error = Some(msg);
            cx.notify();
        }
    }

    fn show_in_folder(&mut self, cx: &mut Context<Self>) {
        let path = match &self.phase {
            CapturePhase::Complete { target_path, .. } => target_path.clone(),
            _ => return,
        };
        if let Err(msg) = reveal_in_folder(&path) {
            self.action_error = Some(msg);
            cx.notify();
        }
    }

    /// Refresh job snapshot; bind id; morph to Complete when done.
    fn sync_from_bridge(&mut self, cx: &mut Context<Self>) {
        let jobs = self.ipc.jobs_snapshot();

        match &self.phase {
            CapturePhase::Progress { job_id, url } => {
                let mut bound_id = job_id.clone();
                if bound_id.is_none() {
                    // Match newly enqueued active job for this URL.
                    if let Some(j) = jobs.iter().find(|j| {
                        j.url == *url
                            && matches!(
                                j.state,
                                JobState::Queued
                                    | JobState::Starting
                                    | JobState::Downloading
                                    | JobState::Paused
                                    | JobState::Completed
                                    | JobState::Failed
                            )
                    }) {
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
                    if let Some(j) = jobs.iter().find(|j| j.id == id) {
                        self.job = Some(j.clone());
                        if j.state == JobState::Completed {
                            let _ = self.ipc.try_claim_complete_hud(&id);
                            self.phase = CapturePhase::Complete {
                                job_id: id,
                                filename: j.filename.clone(),
                                target_path: j.target_path.clone(),
                                total_bytes: j.total_bytes,
                            };
                            cx.notify();
                            return;
                        }
                        if j.state == JobState::Canceled {
                            // Cancelled elsewhere — close HUD.
                            // Caller may remove window; signal via phase clear on next close.
                        }
                    }
                }

                if let CapturePhase::Progress { job_id: ref mut slot, .. } = self.phase {
                    *slot = bound_id;
                }
                cx.notify();
            }
            CapturePhase::Complete { .. } => {
                // Keep snapshot fresh for path existence checks.
                if let CapturePhase::Complete { job_id, .. } = &self.phase {
                    if let Some(j) = jobs.into_iter().find(|j| j.id == *job_id) {
                        self.job = Some(j);
                    }
                }
            }
            CapturePhase::Confirm => {}
        }
    }

    fn title_for_phase(&self) -> &'static str {
        match &self.phase {
            CapturePhase::Confirm => "Confirm browser download",
            CapturePhase::Progress { .. } => {
                if let Some(job) = &self.job {
                    match job.state {
                        JobState::Paused => "Download paused",
                        JobState::Failed => "Download failed",
                        JobState::Canceled => "Download canceled",
                        _ => "Downloading",
                    }
                } else {
                    "Downloading"
                }
            }
            CapturePhase::Complete { .. } => "Download complete",
        }
    }
}

fn start_sync_timer(cx: &mut Context<BrowserPromptWindow>) {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            if this
                .update(cx, |this, cx| {
                    if !matches!(this.phase, CapturePhase::Confirm) {
                        this.sync_from_bridge(cx);
                    }
                })
                .is_err()
            {
                break;
            }
        }
    })
    .detach();
}

impl Render for BrowserPromptWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let title = self.title_for_phase().to_string();
        let dialog_layer = Root::render_dialog_layer(window, cx);

        let body = match &self.phase {
            CapturePhase::Confirm => self.render_confirm(cx),
            CapturePhase::Progress { .. } => self.render_progress(cx),
            CapturePhase::Complete { .. } => self.render_complete(cx),
        };

        v_flex()
            .id("browser-capture-window")
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(themed_popup_title_bar(
                title,
                cx.listener(|this, _, window, cx| {
                    this.close_hud(window, cx);
                }),
                cx,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .w_full()
                    .px_4()
                    .py_3()
                    .gap_3()
                    .child(body),
            )
            .children(dialog_layer)
    }
}

impl BrowserPromptWindow {
    fn render_confirm(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let prompt = self.prompt.as_ref();

        let size_label = prompt
            .and_then(|p| p.total_bytes)
            .filter(|n| *n > 0)
            .map(format_bytes)
            .unwrap_or_else(|| "Unknown size".into());
        let source_label = prompt
            .map(|p| {
                format!(
                    "{} · {}",
                    p.browser,
                    p.entry_point.replace('_', " ")
                )
            })
            .unwrap_or_default();
        let title_line = prompt
            .and_then(|p| p.page_title.as_deref())
            .filter(|t| !t.trim().is_empty())
            .unwrap_or("Browser download")
            .to_string();
        let url_display = prompt
            .map(|p| truncate_middle(&p.url, 64))
            .unwrap_or_default();
        let save_preview = self
            .dir_input
            .as_ref()
            .map(|d| shorten_path(&d.read(cx).value()))
            .unwrap_or_else(|| "default folder".into());

        v_flex()
            .gap_3()
            .size_full()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .child(title_line),
                    )
                    .child(div().text_xs().text_color(muted).child(source_label))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("{size_label} · {url_display}")),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xs().font_medium().child("Filename"))
                    .when_some(self.name_input.as_ref(), |el, input| {
                        el.child(Input::new(input).w_full())
                    }),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xs().font_medium().child("Save to"))
                    .child(
                        h_flex()
                            .gap_2()
                            .w_full()
                            .items_center()
                            .when_some(self.dir_input.as_ref(), |el, input| {
                                el.child(Input::new(input).w_full().flex_1())
                            })
                            .child(
                                Button::new("prompt-browse-dir")
                                    .label("Browse")
                                    .icon(IconName::FolderOpen)
                                    .outline()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.browse_directory(window, cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("Preview: {save_preview}")),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .pt_1()
                    .child(
                        Button::new("prompt-dismiss")
                            .label("Dismiss")
                            .outline()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dismiss_confirm(window, cx);
                            })),
                    )
                    .child(
                        Button::new("prompt-start")
                            .label("Start download")
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.accept(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_progress(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let progress_style = self.progress_style;

        let (filename, progress, size_line, speed_line, state_label, can_pause, can_resume, can_retry, error) =
            if let Some(job) = &self.job {
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

        let progress_color = if self.job.as_ref().is_some_and(|j| j.state == JobState::Paused) {
            theme.warning
        } else if self.job.as_ref().is_some_and(|j| j.state == JobState::Failed) {
            theme.danger
        } else {
            theme.progress_bar
        };

        v_flex()
            .gap_3()
            .size_full()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .child(filename),
                    )
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
                    .child(capture_progress_bar(progress, progress_color, progress_style))
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
                        .child(msg),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .pt_1()
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

    fn render_complete(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let success = theme.success;

        let (filename, total_bytes, target_path) = match &self.phase {
            CapturePhase::Complete {
                filename,
                total_bytes,
                target_path,
                ..
            } => (filename.clone(), *total_bytes, target_path.clone()),
            _ => return div().into_any_element(),
        };

        let size_label = if total_bytes > 0 {
            format_bytes(total_bytes)
        } else {
            self.job
                .as_ref()
                .map(format_size)
                .unwrap_or_else(|| "—".into())
        };
        let path_preview = shorten_path(&target_path.to_string_lossy());
        let file_exists = target_path.exists();
        let action_error = self.action_error.clone();

        v_flex()
            .gap_3()
            .size_full()
            .justify_center()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_color(success)
                            .child(Icon::new(IconName::CircleCheck).large()),
                    )
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .child(filename),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{size_label} · finished")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(path_preview),
                            ),
                    ),
            )
            .when_some(action_error, |el, msg| {
                el.child(div().text_xs().text_color(theme.danger).child(msg))
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .child(
                        Button::new("capture-show")
                            .label("Show")
                            .icon(IconName::FolderOpen)
                            .outline()
                            .disabled(!file_exists)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_in_folder(cx);
                            })),
                    )
                    .child(
                        Button::new("capture-open")
                            .label("Open file")
                            .primary()
                            .disabled(!file_exists)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_file(cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}

/// Open the ask-mode browser confirm window (may morph into progress/complete).
pub fn open_browser_prompt_window(
    prompt: BrowserPromptView,
    ipc: IpcBridge,
    engine: EngineHandle,
    settings: &Settings,
    cx: &mut App,
) -> Option<WindowHandle<Root>> {
    open_capture_window(
        format!("{APP_NAME} — Confirm download"),
        {
            let prompt = prompt.clone();
            let ipc = ipc.clone();
            let engine = engine.clone();
            let settings = settings.clone();
            move |window, cx| {
                BrowserPromptWindow::new_confirm(prompt, ipc, engine, &settings, window, cx)
            }
        },
        ipc,
        &prompt.id,
        cx,
    )
}

/// Open a progress (or complete) HUD for a browser-handoff job.
pub fn open_browser_progress_window(
    job_id: String,
    ipc: IpcBridge,
    engine: EngineHandle,
    settings: &Settings,
    cx: &mut App,
) -> Option<WindowHandle<Root>> {
    if !ipc.try_own_progress_job(&job_id) {
        return None;
    }
    let opened = open_capture_window(
        format!("{APP_NAME} — Downloading"),
        {
            let job_id = job_id.clone();
            let ipc = ipc.clone();
            let engine = engine.clone();
            let settings = settings.clone();
            move |window, cx| {
                BrowserPromptWindow::new_progress(job_id, ipc, engine, &settings, window, cx)
            }
        },
        ipc.clone(),
        &job_id,
        cx,
    );
    if opened.is_none() {
        ipc.release_progress_job(&job_id);
    }
    opened
}

/// Open the Complete HUD for a finished browser-handoff job (e.g. progress was closed early).
pub fn open_browser_complete_window(
    job: Job,
    ipc: IpcBridge,
    engine: EngineHandle,
    settings: &Settings,
    cx: &mut App,
) -> Option<WindowHandle<Root>> {
    if !ipc.try_claim_complete_hud(&job.id) {
        return None;
    }
    let _ = ipc.try_own_progress_job(&job.id);
    let job_id = job.id.clone();
    open_capture_window(
        format!("{APP_NAME} — Download complete"),
        {
            let ipc = ipc.clone();
            let engine = engine.clone();
            let settings = settings.clone();
            move |window, cx| {
                BrowserPromptWindow::new_complete(job, ipc, engine, &settings, window, cx)
            }
        },
        ipc,
        &job_id,
        cx,
    )
}

fn open_capture_window<F>(
    title: String,
    build: F,
    ipc_fallback: IpcBridge,
    fallback_prompt_id: &str,
    cx: &mut App,
) -> Option<WindowHandle<Root>>
where
    F: FnOnce(&mut Window, &mut Context<BrowserPromptWindow>) -> BrowserPromptWindow + 'static,
{
    let prompt_size = size(px(CAPTURE_WINDOW_W), px(CAPTURE_WINDOW_H));
    let bounds = Bounds::centered(None, prompt_size, cx);
    let fallback_id = fallback_prompt_id.to_string();

    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some(SharedString::from(title)),
                appears_transparent: true,
                traffic_light_position: Some(gpui::point(px(9.0), px(9.0))),
            }),
            window_decorations: Some(WindowDecorations::Client),
            window_min_size: Some(size(px(400.0), px(260.0))),
            kind: WindowKind::PopUp,
            focus: true,
            show: true,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        },
        move |window, cx| {
            let view = cx.new(|cx| build(window, cx));

            let view_for_close = view.clone();
            window.on_window_should_close(cx, move |window, cx| {
                let _ = view_for_close.update(cx, |this, cx| {
                    this.close_hud_on_native_close(window, cx);
                });
                true
            });

            cx.new(|cx| Root::new(view, window, cx))
        },
    );

    match result {
        Ok(handle) => {
            cx.activate(true);
            let _ = handle.update(cx, |_root, window, _cx| {
                center_window(window);
                window.activate_window();
            });
            Some(handle)
        }
        Err(error) => {
            eprintln!("[capture] could not open browser capture window: {error:#}");
            // Best-effort dismiss if this was still a confirm prompt id.
            let _ = ipc_fallback.resolve_prompt(&fallback_id, PromptDecision::Dismiss);
            ipc_fallback.release_progress_job(&fallback_id);
            None
        }
    }
}

fn capture_progress_bar(value: f32, color: Hsla, style: ProgressStyle) -> impl IntoElement {
    let value = value.clamp(0.0, 100.0);
    let height = match style {
        ProgressStyle::Soft => px(4.),
        ProgressStyle::Glow => px(9.),
        ProgressStyle::Solid | ProgressStyle::Segmented => px(6.),
    };
    Progress::new()
        .value(value)
        .bg(color)
        .h(height)
        .w_full()
        .rounded_full()
}

fn truncate_middle(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    if max_chars < 8 {
        return value.chars().take(max_chars).collect();
    }
    let keep = (max_chars - 1) / 2;
    let head: String = value.chars().take(keep).collect();
    let tail: String = value
        .chars()
        .rev()
        .take(max_chars - keep - 1)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

fn shorten_path(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() {
        return "default folder".into();
    }
    let buf = PathBuf::from(path);
    let parts: Vec<_> = buf
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let sep = std::path::MAIN_SEPARATOR;
    match parts.as_slice() {
        [] => path.to_string(),
        [one] => one.clone(),
        [.., parent, leaf] => format!("{parent}{sep}{leaf}"),
    }
}
