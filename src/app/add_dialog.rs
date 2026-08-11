//! Add-download dialog extracted from `DownloadApp` for maintainability.
//! Behavior is unchanged — pure move of `open_add_dialog` plus small pure helpers.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    div, prelude::FluentBuilder, px, App, AppContext, Context, Entity, InteractiveElement,
    ParentElement, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{
    button::Button,
    dialog::DialogButtonProps,
    h_flex,
    input::{Input, InputState},
    v_flex, ActiveTheme, Icon, IconName, Root, Sizable, StyledExt, WindowExt,
};

use super::widgets::{browse_directory, field_hint, field_label, shorten_path_display};
use super::DownloadApp;
use crate::download::EngineCommand;

/// Estimated dialog content height so the footer stays on-screen.
fn estimated_dialog_height(is_advanced: bool, is_multi: bool) -> f32 {
    match (is_advanced, is_multi) {
        (true, true) => 560.0,
        (true, false) => 480.0,
        (false, true) => 400.0,
        (false, false) => 280.0,
    }
}

/// Vertical margin that centers compact dialogs and biases tall ones upward.
fn dialog_margin_top(view_h: f32, est_h: f32) -> f32 {
    let max_top = (view_h - est_h - 20.0).max(24.0);
    ((view_h - est_h) * 0.5).clamp(24.0, max_top)
}

/// Enqueue one or more URLs from the add dialog; returns whether the dialog should close.
fn submit_add_download(
    raw: &str,
    filename: String,
    directory: PathBuf,
    engine: &crate::download::EngineHandle,
    app_view: &Entity<DownloadApp>,
    cx: &mut App,
) -> bool {
    // Engine also splits glued schemes; do it here so filename applies to first only.
    let urls = crate::download::extract_http_urls(raw);
    if urls.is_empty() {
        app_view.update(cx, |app, cx| {
            app.show_error_toast("Enter at least one valid HTTP(S) URL.", cx);
        });
        return false;
    }

    let single_name = if urls.len() == 1 && !filename.trim().is_empty() {
        Some(filename)
    } else {
        None
    };

    // One Add per URL keeps jobs independent; engine still re-splits defensively.
    for (i, url) in urls.iter().enumerate() {
        engine.send(EngineCommand::Add {
            url: url.clone(),
            filename: if i == 0 { single_name.clone() } else { None },
            directory: directory.clone(),
            handoff_auth: None,
            reply: None,
        });
    }
    true
}

impl DownloadApp {
    pub(super) fn open_add_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let default_dir = self.settings.download_directory.clone();
        let engine = self.engine.clone();
        let app_view = cx.entity().clone();

        // Single-line by default; multi-line is opt-in via a toggle (InputState mode
        // is fixed at construction, so we keep two states and swap which is shown).
        let url_single =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://example.com/file.zip"));
        let url_multi = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(4)
                .placeholder("One URL per line…")
        });
        let name_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Leave blank to use the server name")
        });
        let dir_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(default_dir.to_string_lossy().to_string())
        });

        let url_single_ok = url_single.clone();
        let url_multi_ok = url_multi.clone();
        let name_for_ok = name_input.clone();
        let dir_for_ok = dir_input.clone();
        let dir_for_browse = dir_input.clone();
        // Dialog builder re-runs each paint; Cells keep toggle state across rebuilds.
        let advanced_open = Rc::new(Cell::new(false));
        let multi_urls = Rc::new(Cell::new(false));

        window.open_dialog(cx, {
            let url_single = url_single.clone();
            let url_multi = url_multi.clone();
            let name_input = name_input.clone();
            let dir_input = dir_input.clone();
            let engine = engine.clone();
            let app_view = app_view.clone();
            let advanced_open = advanced_open.clone();
            let multi_urls = multi_urls.clone();
            move |dialog, window, cx| {
                let url_single_ok = url_single_ok.clone();
                let url_multi_ok = url_multi_ok.clone();
                let multi_urls_ok = multi_urls.clone();
                let name_ok = name_for_ok.clone();
                let dir_ok = dir_for_ok.clone();
                let engine_ok = engine.clone();
                let app_view_ok = app_view.clone();
                let app_view_browse = app_view.clone();
                let dir_browse = dir_for_browse.clone();
                let theme = cx.theme().clone();
                let muted = theme.muted_foreground;
                let is_advanced = advanced_open.get();
                let is_multi = multi_urls.get();
                let save_preview = shorten_path_display(&dir_input.read(cx).value());

                // Center when compact; when Advanced / multi-URL is open, bias upward
                // so the footer (Cancel / Start download) never clips the window bottom.
                let est_h = estimated_dialog_height(is_advanced, is_multi);
                let view_h = window.viewport_size().height.to_f64() as f32;
                let margin_top = dialog_margin_top(view_h, est_h);

                dialog
                    .title("Add download")
                    .w(px(500.))
                    .margin_top(px(margin_top))
                    .border_color(theme.border.opacity(0.32))
                    .confirm()
                    // confirm() disables outside-click; re-enable for light dismiss UX.
                    .overlay_closable(true)
                    .keyboard(true)
                    .button_props(DialogButtonProps::default().ok_text("Start download"))
                    .child(
                        v_flex()
                            .gap_4()
                            .w_full()
                            // Keep last fields clear of the footer row.
                            .pb_2()
                            .child(
                                v_flex()
                                    .gap_1p5()
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_center()
                                            .justify_between()
                                            .gap_3()
                                            .child(field_label("URL", cx))
                                            .child(
                                                // Lightweight toggle (not Switch): Switch's
                                                // internal keyed state + dialog rebuild was
                                                // panicking on click inside open_dialog.
                                                h_flex()
                                                    .id("add-multi-urls-toggle")
                                                    .items_center()
                                                    .gap_1p5()
                                                    .px_1p5()
                                                    .py_0p5()
                                                    .rounded(theme.radius)
                                                    .cursor_pointer()
                                                    .hover(|this| {
                                                        this.bg(theme.accent.opacity(0.08))
                                                    })
                                                    .on_click({
                                                        let multi_urls = multi_urls.clone();
                                                        let url_single = url_single.clone();
                                                        let url_multi = url_multi.clone();
                                                        move |_, window, cx| {
                                                            let next = !multi_urls.get();
                                                            if next {
                                                                let text = url_single
                                                                    .read(cx)
                                                                    .value()
                                                                    .to_string();
                                                                url_multi.update(
                                                                    cx,
                                                                    |state, cx| {
                                                                        state.set_value(
                                                                            text, window, cx,
                                                                        );
                                                                    },
                                                                );
                                                            } else {
                                                                let text = url_multi
                                                                    .read(cx)
                                                                    .value()
                                                                    .to_string();
                                                                let first = text
                                                                    .lines()
                                                                    .map(str::trim)
                                                                    .find(|l| !l.is_empty())
                                                                    .unwrap_or("")
                                                                    .to_string();
                                                                url_single.update(
                                                                    cx,
                                                                    |state, cx| {
                                                                        state.set_value(
                                                                            first, window, cx,
                                                                        );
                                                                    },
                                                                );
                                                            }
                                                            multi_urls.set(next);
                                                            Root::update(window, cx, |_, _, cx| {
                                                                cx.notify();
                                                            });
                                                        }
                                                    })
                                                    .child(
                                                        div()
                                                            .w(px(28.))
                                                            .h(px(16.))
                                                            .rounded(px(16.))
                                                            .p(px(2.))
                                                            .bg(if is_multi {
                                                                theme.primary
                                                            } else {
                                                                theme.secondary
                                                            })
                                                            .child(
                                                                div()
                                                                    .w(px(12.))
                                                                    .h(px(12.))
                                                                    .rounded_full()
                                                                    .bg(theme.background)
                                                                    .when(is_multi, |el| {
                                                                        el.ml(px(12.))
                                                                    }),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(if is_multi {
                                                                theme.foreground
                                                            } else {
                                                                muted
                                                            })
                                                            .child("Multiple URLs"),
                                                    ),
                                            ),
                                    )
                                    .when(!is_multi, |el| {
                                        el.child(Input::new(&url_single).w_full())
                                    })
                                    .when(is_multi, |el| {
                                        // Explicit height: multi-line Input defaults to h_auto
                                        // and collapses when empty without a fixed size.
                                        el.child(Input::new(&url_multi).w_full().h(px(104.)))
                                    }),
                            )
                            .child(
                                v_flex()
                                    .gap_0()
                                    .w_full()
                                    .rounded(theme.radius_lg)
                                    .bg(theme.secondary.opacity(0.28))
                                    .child(
                                        h_flex()
                                            .id("add-download-advanced-toggle")
                                            .w_full()
                                            .items_center()
                                            .justify_between()
                                            .gap_2()
                                            .px_3()
                                            .py_2()
                                            .rounded(theme.radius_lg)
                                            .cursor_pointer()
                                            .hover(|this| this.bg(theme.accent.opacity(0.08)))
                                            .on_click({
                                                let advanced_open = advanced_open.clone();
                                                move |_, window, cx| {
                                                    advanced_open.set(!advanced_open.get());
                                                    Root::update(window, cx, |_, _, cx| {
                                                        cx.notify();
                                                    });
                                                }
                                            })
                                            .child(
                                                h_flex()
                                                    .items_center()
                                                    .gap_1p5()
                                                    .child(
                                                        Icon::new(if is_advanced {
                                                            IconName::ChevronDown
                                                        } else {
                                                            IconName::ChevronRight
                                                        })
                                                        .with_size(px(14.))
                                                        .text_color(muted),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .font_medium()
                                                            .text_color(theme.foreground)
                                                            .child("Advanced options"),
                                                    ),
                                            )
                                            .when(!is_advanced, |el| {
                                                el.child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(muted)
                                                        .px_2()
                                                        .py_0p5()
                                                        .rounded(theme.radius)
                                                        .bg(theme.background.opacity(0.55))
                                                        .child(format!("Save to {save_preview}")),
                                                )
                                            }),
                                    )
                                    .when(is_advanced, |this| {
                                        this.child(
                                            v_flex()
                                                .px_3()
                                                .pb_3()
                                                .gap_3()
                                                .w_full()
                                                .child(
                                                    div()
                                                        .h(px(1.))
                                                        .w_full()
                                                        .bg(theme.border.opacity(0.35)),
                                                )
                                                .child(
                                                    v_flex()
                                                        .gap_1p5()
                                                        .child(field_label("Filename", cx))
                                                        .child(Input::new(&name_input).w_full())
                                                        .child(field_hint(
                                                            "Optional. Applies to a single URL only.",
                                                            cx,
                                                        )),
                                                )
                                                .child(
                                                    v_flex()
                                                        .gap_1p5()
                                                        .child(field_label("Save to", cx))
                                                        .child(
                                                            h_flex()
                                                                .gap_2()
                                                                .w_full()
                                                                .items_center()
                                                                .child(
                                                                    Input::new(&dir_input)
                                                                        .w_full()
                                                                        .flex_1(),
                                                                )
                                                                .child(
                                                                    Button::new("browse-add-dir")
                                                                        .label("Browse")
                                                                        .icon(IconName::FolderOpen)
                                                                        .outline()
                                                                        .on_click(
                                                                            move |_, window, cx| {
                                                                                browse_directory(
                                                                                    dir_browse
                                                                                        .clone(),
                                                                                    app_view_browse
                                                                                        .clone(),
                                                                                    window,
                                                                                    cx,
                                                                                );
                                                                            },
                                                                        ),
                                                                ),
                                                        )
                                                        .child(field_hint(
                                                            "Folder for the finished file.",
                                                            cx,
                                                        )),
                                                ),
                                        )
                                    }),
                            ),
                    )
                    .on_ok(move |_, _window, cx| {
                        let raw = if multi_urls_ok.get() {
                            url_multi_ok.read(cx).value().to_string()
                        } else {
                            url_single_ok.read(cx).value().to_string()
                        };
                        let filename = name_ok.read(cx).value().to_string();
                        let directory = PathBuf::from(dir_ok.read(cx).value().to_string());
                        submit_add_download(
                            &raw,
                            filename,
                            directory,
                            &engine_ok,
                            &app_view_ok,
                            cx,
                        )
                    })
            }
        });
    }
}
