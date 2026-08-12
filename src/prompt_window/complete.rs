//! Complete phase: construct, open/reveal actions, and render.

use std::collections::VecDeque;

use gpui::{div, prelude::FluentBuilder, Context, IntoElement, ParentElement, Styled, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt,
};

use super::helpers::shorten_path;
use super::start_sync_timer;
use super::{BrowserPromptWindow, CapturePhase};
use crate::appearance::apply_appearance;
use crate::download::{open_path, reveal_in_folder, EngineHandle, Job};
use crate::format::{format_bytes, format_size};
use crate::ipc::IpcBridge;
use crate::settings::Settings;
use crate::window_icon::apply_app_icon;

impl BrowserPromptWindow {
    pub(super) fn new_complete(
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
            speed_samples: VecDeque::new(),
            reduce_motion: settings.reduce_motion,
        }
    }

    pub(super) fn open_file(&mut self, cx: &mut Context<Self>) {
        let path = match &self.phase {
            CapturePhase::Complete { target_path, .. } => target_path.clone(),
            _ => return,
        };
        if let Err(msg) = open_path(&path) {
            self.action_error = Some(msg);
            cx.notify();
        }
    }

    pub(super) fn show_in_folder(&mut self, cx: &mut Context<Self>) {
        let path = match &self.phase {
            CapturePhase::Complete { target_path, .. } => target_path.clone(),
            _ => return,
        };
        if let Err(msg) = reveal_in_folder(&path) {
            self.action_error = Some(msg);
            cx.notify();
        }
    }

    pub(super) fn render_complete(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
                            .child(div().text_sm().font_medium().child(filename))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{size_label} · finished")),
                            )
                            .child(div().text_xs().text_color(muted).child(path_preview)),
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
