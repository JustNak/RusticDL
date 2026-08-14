//! Conflict phase: same-name file already exists — rename, overwrite, or cancel.

use std::path::PathBuf;

use gpui::{div, prelude::FluentBuilder, Context, IntoElement, ParentElement, Styled, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    v_flex, ActiveTheme, Disableable, IconName, StyledExt,
};

use super::helpers::{shorten_path, truncate_middle};
use super::BrowserPromptWindow;
use crate::download::{find_filename_collision, FilenameCollision};
use crate::format::format_bytes;

impl BrowserPromptWindow {
    fn current_conflict_name(&self, cx: &Context<Self>) -> String {
        self.name_input
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "download.bin".into())
    }

    fn current_conflict_dir(&self, cx: &Context<Self>) -> PathBuf {
        self.dir_input
            .as_ref()
            .map(|input| PathBuf::from(input.read(cx).value().to_string()))
            .unwrap_or_default()
    }

    pub(super) fn current_collision(&self, cx: &Context<Self>) -> Option<FilenameCollision> {
        find_filename_collision(
            &self.current_conflict_dir(cx),
            &self.current_conflict_name(cx),
            &self.ipc.jobs_snapshot(),
        )
    }

    /// Put a free `file (n).ext` into the filename field. Does not start the job.
    pub(super) fn apply_suggested_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(suggested) = self.current_collision(cx).map(|c| c.suggested_unique_name) else {
            return;
        };
        let Some(input) = self.name_input.clone() else {
            return;
        };
        input.update(cx, |state, cx| {
            state.set_value(suggested, window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn resolve_start_download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.current_collision(cx).is_some() {
            return;
        }
        let name = self.current_conflict_name(cx);
        if name.is_empty() {
            return;
        }
        self.resolve_accept(Some(name), false, window, cx);
    }

    pub(super) fn resolve_overwrite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .current_collision(cx)
            .is_some_and(|c| c.blocks_overwrite)
        {
            return;
        }
        let filename = Some(self.current_conflict_name(cx));
        self.resolve_accept(filename, true, window, cx);
    }

    pub(super) fn render_conflict(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let prompt = self.prompt.as_ref();
        let collision = self.current_collision(cx);
        let name_taken = collision.is_some();
        let blocks_overwrite = collision.as_ref().is_some_and(|c| c.blocks_overwrite);
        let unique_preview = collision
            .as_ref()
            .map(|c| c.suggested_unique_name.clone())
            .unwrap_or_default();
        let display_name = collision
            .as_ref()
            .map(|c| c.filename.clone())
            .unwrap_or_else(|| self.current_conflict_name(cx));

        let size_label = prompt
            .and_then(|p| p.total_bytes)
            .filter(|n| *n > 0)
            .map(format_bytes)
            .unwrap_or_else(|| "Unknown size".into());
        let source_label = prompt
            .map(|p| format!("{} · {}", p.browser, p.entry_point.replace('_', " ")))
            .unwrap_or_default();
        let url_display = prompt
            .map(|p| truncate_middle(&p.url, 64))
            .unwrap_or_default();
        let save_preview = self
            .dir_input
            .as_ref()
            .map(|d| shorten_path(&d.read(cx).value()))
            .unwrap_or_else(|| "default folder".into());

        let banner = if name_taken && blocks_overwrite {
            format!("“{display_name}” is used by another download.")
        } else if name_taken {
            format!("“{display_name}” already exists in this folder.")
        } else {
            format!("“{display_name}” is available.")
        };
        let hint = if name_taken {
            format!(
                "Change the filename below, then click Start download. Suggested: {unique_preview}."
            )
        } else {
            "This name is available. Click Start download to keep it.".into()
        };

        v_flex()
            .gap_3()
            .size_full()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_medium().child(banner))
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
                                Button::new("conflict-browse-dir")
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
                    )
                    .child(div().text_xs().text_color(muted).child(hint)),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .pt_1()
                    .child(
                        Button::new("conflict-cancel")
                            .label("Cancel")
                            .outline()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dismiss_confirm(window, cx);
                            })),
                    )
                    .when(name_taken, |el| {
                        el.child(
                            Button::new("conflict-overwrite")
                                .label("Overwrite")
                                .danger()
                                .disabled(blocks_overwrite)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.resolve_overwrite(window, cx);
                                })),
                        )
                        .child(
                            Button::new("conflict-rename")
                                .label("Rename")
                                .outline()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.apply_suggested_rename(window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("conflict-start")
                            .label("Start download")
                            .primary()
                            .disabled(name_taken)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.resolve_start_download(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}
