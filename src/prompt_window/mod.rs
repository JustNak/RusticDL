//! Floating browser capture HUD: Confirm → Progress → Complete.
//!
//! Lives independent of the main queue window so handoff still works when the
//! main UI is minimized or hidden.

mod complete;
mod confirm;
mod conflict;
mod helpers;
mod open;
mod progress;

pub use open::{
    open_browser_complete_window, open_browser_progress_window, open_browser_prompt_window,
};

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    Context, Entity, InteractiveElement, IntoElement, ParentElement, Render, Styled, Window,
};
use gpui_component::{input::InputState, v_flex, ActiveTheme, Root};

use crate::browser_popup_chrome::themed_popup_title_bar;
use crate::download::{EngineHandle, Job};
use crate::ipc::{BrowserPromptView, IpcBridge};
use crate::settings::ProgressStyle;

const CAPTURE_WINDOW_W: f32 = 480.0;
const CAPTURE_WINDOW_H: f32 = 320.0;
/// Conflict adds a rejection row + four actions; keep the button row on-screen.
const CAPTURE_CONFLICT_H: f32 = 352.0;
/// Complete phase has no sparkline or form — hug the file row + actions.
const CAPTURE_COMPLETE_H: f32 = 196.0;
/// Rolling speed samples for the Progress sparkline (~9s at 100ms tick).
const SPEED_SAMPLE_CAP: usize = 90;

/// Phase of the floating capture HUD.
#[derive(Debug, Clone)]
enum CapturePhase {
    /// Ask-mode confirmation form.
    Confirm,
    /// Same-name file already exists — rename, overwrite, or cancel.
    Conflict,
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
    /// Rolling bytes/sec samples for the Progress speed sparkline.
    speed_samples: VecDeque<u64>,
    /// Highest speed seen this HUD session (sticky Y-scale / peak label).
    peak_speed: u64,
    /// When true, skip animating sample growth (settings reduce_motion).
    reduce_motion: bool,
    /// Last height we applied via `resize` so Complete/Progress don't fight.
    fitted_height: Option<f32>,
}

impl BrowserPromptWindow {
    fn push_speed_sample(&mut self, speed: u64) {
        if speed > self.peak_speed {
            self.peak_speed = speed;
        }
        if self.reduce_motion {
            // Keep a short flat trail so the chart still has shape without churn.
            if self.speed_samples.len() >= 12 {
                self.speed_samples.pop_front();
            }
        } else if self.speed_samples.len() >= SPEED_SAMPLE_CAP {
            self.speed_samples.pop_front();
        }
        self.speed_samples.push_back(speed);
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
            CapturePhase::Confirm | CapturePhase::Conflict => {
                self.dismiss_confirm(window, cx);
            }
            CapturePhase::Progress { .. } | CapturePhase::Complete { .. } => {
                // Release so Complete can re-open if Progress closed mid-download.
                self.release_ownership();
                window.remove_window();
                cx.notify();
            }
        }
    }

    fn close_hud_on_native_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.phase {
            CapturePhase::Confirm | CapturePhase::Conflict => {
                self.dismiss_confirm_on_close(window, cx)
            }
            CapturePhase::Progress { .. } | CapturePhase::Complete { .. } => {
                self.release_ownership();
                cx.notify();
            }
        }
    }

    fn target_window_height(&self) -> f32 {
        match self.phase {
            CapturePhase::Complete { .. } => CAPTURE_COMPLETE_H,
            CapturePhase::Conflict => CAPTURE_CONFLICT_H,
            _ => CAPTURE_WINDOW_H,
        }
    }

    fn fit_window_to_phase(&mut self, window: &mut Window) {
        let target = self.target_window_height();
        if self.fitted_height.is_some_and(|h| (h - target).abs() < 0.5) {
            return;
        }
        // Record first so a synchronous resize→re-render does not loop.
        self.fitted_height = Some(target);
        window.resize(gpui::size(gpui::px(CAPTURE_WINDOW_W), gpui::px(target)));
        crate::window_placement::center_window(window);
    }

    fn title_for_phase(&self) -> &'static str {
        match &self.phase {
            CapturePhase::Confirm => "Confirm browser download",
            CapturePhase::Conflict => "File already exists",
            CapturePhase::Progress { .. } => {
                if let Some(job) = &self.job {
                    match job.state {
                        crate::download::JobState::Paused => "Download paused",
                        crate::download::JobState::Failed => "Download failed",
                        crate::download::JobState::Canceled => "Download canceled",
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
    cx.spawn(async move |this, cx| loop {
        cx.background_executor()
            .timer(Duration::from_millis(100))
            .await;
        let keep_going = this.update(cx, |this, cx| {
            if matches!(this.phase, CapturePhase::Progress { .. }) {
                this.sync_from_bridge(cx);
                true
            } else {
                false
            }
        });
        match keep_going {
            Ok(true) => {}
            _ => break,
        }
    })
    .detach();
}

impl Render for BrowserPromptWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Live name/folder edits can collide after Confirm opened on a free name.
        if matches!(self.phase, CapturePhase::Confirm)
            && !self.resolved
            && self.current_collision(cx).is_some()
        {
            self.phase = CapturePhase::Conflict;
        }
        self.fit_window_to_phase(window);
        let theme = cx.theme().clone();
        let title = self.title_for_phase().to_string();
        let dialog_layer = Root::render_dialog_layer(window, cx);

        let body = match &self.phase {
            CapturePhase::Confirm => self.render_confirm(cx),
            CapturePhase::Conflict => self.render_conflict(cx),
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
            .child(v_flex().flex_1().w_full().px_4().py_3().gap_3().child(body))
            .children(dialog_layer)
    }
}
