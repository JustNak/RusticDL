use std::path::PathBuf;

use gpui::{
    div, prelude::FluentBuilder, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    tooltip::Tooltip,
    v_flex, ActiveTheme, Disableable, IconName, StyledExt,
};

use super::helpers::{shorten_path, truncate_middle};
use super::BrowserPromptWindow;
use crate::download::{find_filename_collision, FilenameCollision};
use crate::format::format_bytes;

impl BrowserPromptWindow {
    fn typed_conflict_name(&self, cx: &Context<Self>) -> String {
        self.name_input
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default()
    }

    fn current_conflict_name(&self, cx: &Context<Self>) -> String {
        let name = self.typed_conflict_name(cx);
        if name.is_empty() {
            "download.bin".into()
        } else {
            name
        }
    }

    fn current_conflict_dir(&self, cx: &Context<Self>) -> PathBuf {
        self.dir_input
            .as_ref()
            .map(|input| PathBuf::from(input.read(cx).value().to_string()))
            .unwrap_or_default()
    }

    pub(super) fn current_collision(&self, cx: &Context<Self>) -> Option<FilenameCollision> {
        let name = self.typed_conflict_name(cx);
        if name.is_empty() {
            return None;
        }
        find_filename_collision(
            &self.current_conflict_dir(cx),
            &name,
            &self.ipc.jobs_snapshot(),
        )
    }

    pub(super) fn apply_suggested_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(suggested) = self.current_collision(cx).map(|c| c.suggested_unique_name) else {
            return;
        };
        let Some(input) = self.name_input.as_ref() else {
            return;
        };
        input.update(cx, |state, cx| {
            state.set_value(suggested, window, cx);
            state.focus(window, cx);
        });
        window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        cx.notify();
    }

    pub(super) fn resolve_start_download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.typed_conflict_name(cx);
        if name.is_empty() || self.current_collision(cx).is_some() {
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
        let name_empty = self.typed_conflict_name(cx).is_empty();
        let start_blocked = name_taken || name_empty;
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

        let short_name = truncate_middle(&display_name, 40);
        let short_unique = truncate_middle(&unique_preview, 32);
        let banner = if name_taken && blocks_overwrite {
            format!("“{short_name}” is used by another download.")
        } else if name_taken {
            format!("“{short_name}” already exists in this folder.")
        } else {
            format!("“{short_name}” is available.")
        };
        let hint = if name_taken && blocks_overwrite {
            format!("Another download is using this name. Click Rename for {short_unique}.")
        } else if name_taken {
            format!("Pick a different name, or click Rename for {short_unique}.")
        } else if name_empty {
            "Enter a filename to start the download.".into()
        } else {
            "This name is available. Click Start download to keep it.".into()
        };
        let hint_color = if start_blocked { theme.danger } else { muted };
        let start_button = Button::new("conflict-start")
            .label("Start download")
            .disabled(start_blocked)
            .when(!start_blocked, |btn| btn.primary())
            .on_click(cx.listener(|this, _, window, cx| {
                this.resolve_start_download(window, cx);
            }));

        v_flex()
            .gap_3()
            .size_full()
            .child(
                v_flex()
                    .gap_1()
                    .flex_shrink_0()
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
                    .flex_shrink_0()
                    .child(div().text_xs().font_medium().child("Filename"))
                    .when_some(self.name_input.as_ref(), |el, input| {
                        el.child(Input::new(input).w_full())
                    })
                    .when(name_taken, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .font_medium()
                                .text_color(theme.danger)
                                .child("Duplicate Name"),
                        )
                    }),
            )
            .child(
                v_flex()
                    .gap_1()
                    .flex_1()
                    .min_h_0()
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
                    .child(div().text_xs().text_color(hint_color).child(hint)),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .items_center()
                    .gap_2()
                    .pt_2()
                    .flex_shrink_0()
                    .flex_wrap()
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
                        div()
                            .id("conflict-start-wrap")
                            .flex_shrink_0()
                            .when(name_taken, |el| {
                                let danger = theme.danger;
                                el.tooltip(move |window, cx| {
                                    Tooltip::new("Duplicate Name")
                                        .text_xs()
                                        .font_medium()
                                        .text_color(danger)
                                        .build(window, cx)
                                })
                            })
                            .when(name_empty && !name_taken, |el| {
                                let danger = theme.danger;
                                el.tooltip(move |window, cx| {
                                    Tooltip::new("Enter a filename")
                                        .text_xs()
                                        .font_medium()
                                        .text_color(danger)
                                        .build(window, cx)
                                })
                            })
                            .child(start_button),
                    ),
            )
            .into_any_element()
    }
}
