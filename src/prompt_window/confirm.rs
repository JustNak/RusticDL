//! Confirm phase: construct, accept/dismiss, browse, and render.

use std::collections::VecDeque;
use std::path::PathBuf;

use gpui::{
    div, prelude::FluentBuilder, AppContext, Context, IntoElement, ParentElement,
    PathPromptOptions, SharedString, Styled, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    v_flex, ActiveTheme, IconName, StyledExt,
};

use super::helpers::{default_prompt_filename, shorten_path, truncate_middle};
use super::start_sync_timer;
use super::{BrowserPromptWindow, CapturePhase, CAPTURE_WINDOW_H};
use crate::appearance::apply_appearance;
use crate::download::EngineHandle;
use crate::format::format_bytes;
use crate::ipc::{BrowserPromptView, IpcBridge, PromptDecision};
use crate::settings::Settings;
use crate::window_icon::apply_app_icon;

impl BrowserPromptWindow {
    pub(super) fn new_confirm(
        prompt: BrowserPromptView,
        ipc: IpcBridge,
        engine: EngineHandle,
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        apply_appearance(settings, Some(window), cx);
        apply_app_icon(window);

        let default_name = default_prompt_filename(&prompt);
        let opens_conflict = crate::download::find_filename_collision(
            &prompt.default_directory,
            &default_name,
            &ipc.jobs_snapshot(),
        )
        .is_some();

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

        if opens_conflict {
            name_input.update(cx, |state, cx| {
                state.focus(window, cx);
            });
        }

        window.activate_window();
        start_sync_timer(cx);

        Self {
            phase: if opens_conflict {
                CapturePhase::Conflict
            } else {
                CapturePhase::Confirm
            },
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
            speed_samples: VecDeque::new(),
            peak_speed: 0,
            reduce_motion: settings.reduce_motion,
            fitted_height: Some(CAPTURE_WINDOW_H),
        }
    }

    pub(super) fn dismiss_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.resolved {
            return;
        }
        self.resolved = true;
        if let Some(prompt) = &self.prompt {
            let _ = self.ipc.resolve_prompt(&prompt.id, PromptDecision::Dismiss);
        }
        window.remove_window();
        cx.notify();
    }

    /// Dismiss without `remove_window` — used from `on_window_should_close`.
    pub(super) fn dismiss_confirm_on_close(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.phase, CapturePhase::Confirm | CapturePhase::Conflict) && !self.resolved {
            self.resolved = true;
            if let Some(prompt) = &self.prompt {
                let _ = self.ipc.resolve_prompt(&prompt.id, PromptDecision::Dismiss);
            }
        }
        self.release_ownership();
        cx.notify();
    }

    pub(super) fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.resolve_accept(None, false, window, cx);
    }

    pub(super) fn resolve_accept(
        &mut self,
        filename_override: Option<String>,
        overwrite: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

        let filename = filename_override.or_else(|| {
            self.name_input.as_ref().and_then(|input| {
                let raw = input.read(cx).value().to_string();
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
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
                overwrite,
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

    pub(super) fn browse_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn render_confirm(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let prompt = self.prompt.as_ref();

        let size_label = prompt
            .and_then(|p| p.total_bytes)
            .filter(|n| *n > 0)
            .map(format_bytes)
            .unwrap_or_else(|| "Unknown size".into());
        let source_label = prompt
            .map(|p| format!("{} · {}", p.browser, p.entry_point.replace('_', " ")))
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
                    .child(div().text_sm().font_medium().child(title_line))
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
}
