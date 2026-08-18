//! Complete phase: construct, open/reveal actions, and render.

use std::collections::VecDeque;

use gpui::{
    div, prelude::FluentBuilder, px, Context, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    tooltip::Tooltip,
    v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt,
};

use super::helpers::{shorten_folder, truncate_middle};
use super::{BrowserPromptWindow, CapturePhase, CAPTURE_COMPLETE_H, CAPTURE_WINDOW_W};
use crate::appearance::apply_window_opacity;
use crate::download::{open_path, reveal_in_folder, EngineHandle, Job};
use crate::format::{format_bytes, format_duration, format_size, format_speed};
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
        apply_window_opacity(window, settings.window_transparency, settings.backdrop_blur);
        apply_app_icon(window);
        window.activate_window();
        let cascade_index = ipc.capture_window_count().saturating_sub(1);

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
            trail: Default::default(),
            peak_speed: 0,
            reduce_motion: settings.reduce_motion,
            fitted_size: Some((CAPTURE_WINDOW_W, CAPTURE_COMPLETE_H)),
            cascade_index,
        }
    }

    pub(super) fn open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = match &self.phase {
            CapturePhase::Complete { target_path, .. } => target_path.clone(),
            _ => return,
        };
        if let Err(msg) = open_path(&path) {
            self.action_error = Some(msg);
            cx.notify();
            return;
        }
        self.close_hud(window, cx);
    }

    pub(super) fn show_in_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = match &self.phase {
            CapturePhase::Complete { target_path, .. } => target_path.clone(),
            _ => return,
        };
        if let Err(msg) = reveal_in_folder(&path) {
            self.action_error = Some(msg);
            cx.notify();
            return;
        }
        self.close_hud(window, cx);
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

        let elapsed_secs = self.job.as_ref().map(|j| j.elapsed_secs());
        let downloaded = self
            .job
            .as_ref()
            .map(|j| {
                if j.downloaded_bytes > 0 {
                    j.downloaded_bytes
                } else {
                    j.total_bytes
                }
            })
            .unwrap_or(total_bytes);
        let avg_speed = elapsed_secs
            .filter(|&s| s > 0)
            .map(|s| downloaded / s)
            .filter(|&s| s > 0);

        let mut meta = vec![size_label];
        if let Some(secs) = elapsed_secs.filter(|&s| s > 0) {
            meta.push(format_duration(secs));
        }
        if self.peak_speed > 0 {
            meta.push(format!("peak {}", format_speed(self.peak_speed)));
        }
        if let Some(avg) = avg_speed {
            meta.push(format!("avg {}", format_speed(avg)));
        }

        let folder_label = shorten_folder(&target_path);
        let filename_display = truncate_middle(&filename, 52);
        let filename_tip: SharedString = filename.clone().into();
        let path_tip: SharedString = target_path.to_string_lossy().into_owned().into();
        let file_exists = target_path.exists();
        let action_error = self.action_error.clone();
        let tip_color = muted;

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .w_full()
                    .flex_1()
                    .gap_3()
                    .items_center()
                    .px_1()
                    .child(
                        h_flex()
                            .size(px(40.))
                            .rounded_full()
                            .bg(success.opacity(0.16))
                            .items_center()
                            .justify_center()
                            .flex_shrink_0()
                            .child(Icon::new(IconName::CircleCheck).large().text_color(success)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .id("capture-complete-name")
                                    .text_sm()
                                    .font_medium()
                                    .whitespace_nowrap()
                                    .child(filename_display)
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(filename_tip.clone())
                                            .text_xs()
                                            .font_normal()
                                            .text_color(tip_color)
                                            .py_0()
                                            .px_1p5()
                                            .build(window, cx)
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(meta.join(" · ")),
                            )
                            .child(
                                div()
                                    .id("capture-complete-folder")
                                    .text_xs()
                                    .text_color(muted)
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(folder_label)
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(path_tip.clone())
                                            .text_xs()
                                            .font_normal()
                                            .text_color(tip_color)
                                            .py_0()
                                            .px_1p5()
                                            .build(window, cx)
                                    }),
                            ),
                    ),
            )
            .when(!file_exists, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(theme.warning)
                        .child("File is missing from disk"),
                )
            })
            .when_some(action_error, |el, msg| {
                el.child(div().text_xs().text_color(theme.danger).child(msg))
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .flex_shrink_0()
                    .child(
                        Button::new("capture-show")
                            .label("Show")
                            .icon(IconName::FolderOpen)
                            .outline()
                            .disabled(!file_exists)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_in_folder(window, cx);
                            })),
                    )
                    .child(
                        Button::new("capture-open")
                            .label("Open file")
                            .primary()
                            .disabled(!file_exists)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_file(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}
