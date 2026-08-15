//! Queue confirm dialogs extracted from `DownloadApp`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use gpui::{div, Context, ParentElement, Styled, Window};
use gpui_component::{
    button::ButtonVariant, dialog::DialogButtonProps, v_flex, ActiveTheme, WindowExt,
};

use super::add_dialog::enqueue_urls;
use super::DownloadApp;
use crate::download::EngineCommand;

/// Stable key for a clipboard URL set so focus flapping does not re-prompt.
pub(crate) fn clipboard_urls_key(urls: &[String]) -> u64 {
    let mut sorted: Vec<&str> = urls.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut hasher = DefaultHasher::new();
    for url in sorted {
        url.hash(&mut hasher);
    }
    hasher.finish()
}

impl DownloadApp {
    pub(crate) fn confirm_remove(
        &mut self,
        id: String,
        filename: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let engine = self.engine.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let engine = engine.clone();
            let id = id.clone();
            let muted = cx.theme().muted_foreground;
            dialog
                .title("Remove download?")
                .confirm()
                .overlay_closable(true)
                .keyboard(true)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Remove")
                        .ok_variant(ButtonVariant::Danger),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .child(format!("Remove “{filename}” from the queue?")),
                        )
                        .child(div().text_xs().text_color(muted).child(
                            "The downloaded file is kept. Any leftover .part file is deleted.",
                        )),
                )
                .on_ok(move |_, _, _| {
                    engine.send(EngineCommand::Remove {
                        id: id.clone(),
                        delete_partial: true,
                        delete_file: false,
                    });
                    true
                })
        });
    }

    pub(crate) fn confirm_delete(
        &mut self,
        id: String,
        filename: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let engine = self.engine.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let engine = engine.clone();
            let id = id.clone();
            let muted = cx.theme().muted_foreground;
            dialog
                .title("Delete downloaded file?")
                .confirm()
                .overlay_closable(true)
                .keyboard(true)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Delete file")
                        .ok_variant(ButtonVariant::Danger),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(format!(
                            "Delete “{filename}” from disk and remove it from the queue?"
                        )))
                        .child(div().text_xs().text_color(muted).child(
                            "This cannot be undone. Leftover .part files are also removed.",
                        )),
                )
                .on_ok(move |_, _, _| {
                    engine.send(EngineCommand::Remove {
                        id: id.clone(),
                        delete_partial: true,
                        delete_file: true,
                    });
                    true
                })
        });
    }

    /// Multi-select remove: one confirm listing count; only removable jobs
    /// (terminal or paused) leave the queue. Active jobs are left alone.
    pub(crate) fn confirm_remove_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .jobs
            .iter()
            .filter(|j| self.selected_ids.iter().any(|id| id == &j.id) && j.is_removable())
            .map(|j| j.id.clone())
            .collect();

        if ids.is_empty() {
            self.show_toast("No removable items in the selection.", cx);
            return;
        }

        let engine = self.engine.clone();
        let count = ids.len();
        let muted = cx.theme().muted_foreground;
        let body = format!("Remove {count} selected item(s) from the queue?");
        let note = "Downloaded files are kept. Leftover .part files are deleted. Active downloads are left alone.";

        window.open_dialog(cx, move |dialog, _, _| {
            let engine = engine.clone();
            let ids = ids.clone();
            dialog
                .title("Remove selected downloads?")
                .confirm()
                .overlay_closable(true)
                .keyboard(true)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Remove")
                        .ok_variant(ButtonVariant::Danger),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(body.clone()))
                        .child(div().text_xs().text_color(muted).child(note)),
                )
                .on_ok(move |_, _, _| {
                    for id in &ids {
                        engine.send(EngineCommand::Remove {
                            id: id.clone(),
                            delete_partial: true,
                            delete_file: false,
                        });
                    }
                    true
                })
        });
    }

    /// Multi-select delete: remove selected jobs whose files exist on disk.
    /// Active downloads are left alone.
    pub(crate) fn confirm_delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .jobs
            .iter()
            .filter(|j| self.selected_ids.iter().any(|id| id == &j.id) && j.has_deletable_file())
            .map(|j| j.id.clone())
            .collect();

        if ids.is_empty() {
            self.show_toast("No deletable files in the selection.", cx);
            return;
        }

        let engine = self.engine.clone();
        let count = ids.len();
        let muted = cx.theme().muted_foreground;
        let body =
            format!("Delete {count} selected file(s) from disk and remove them from the queue?");
        let note = "This cannot be undone. Leftover .part files are also removed. Active downloads are left alone.";

        window.open_dialog(cx, move |dialog, _, _| {
            let engine = engine.clone();
            let ids = ids.clone();
            dialog
                .title("Delete selected files?")
                .confirm()
                .overlay_closable(true)
                .keyboard(true)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Delete files")
                        .ok_variant(ButtonVariant::Danger),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(body.clone()))
                        .child(div().text_xs().text_color(muted).child(note)),
                )
                .on_ok(move |_, _, _| {
                    for id in &ids {
                        engine.send(EngineCommand::Remove {
                            id: id.clone(),
                            delete_partial: true,
                            delete_file: true,
                        });
                    }
                    true
                })
        });
    }

    /// Remove finished jobs (completed, failed, canceled) from the queue.
    pub(crate) fn confirm_clear_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .jobs
            .iter()
            .filter(|j| j.state.is_terminal())
            .map(|j| j.id.clone())
            .collect();

        if ids.is_empty() {
            self.show_toast("Nothing to clear.", cx);
            return;
        }

        let engine = self.engine.clone();
        let count = ids.len();
        let body = format!(
            "Remove {count} finished item(s) from the queue? Active downloads stay. Files on disk are kept."
        );

        window.open_dialog(cx, move |dialog, _, _| {
            let engine = engine.clone();
            let ids = ids.clone();
            let body = body.clone();
            dialog
                .title("Clear all finished downloads?")
                .confirm()
                .overlay_closable(true)
                .keyboard(true)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Clear all")
                        .ok_variant(ButtonVariant::Danger),
                )
                .child(div().text_sm().child(body))
                .on_ok(move |_, _, _| {
                    for id in &ids {
                        // Keep on-disk files (including leftover .part for failed jobs).
                        engine.send(EngineCommand::Remove {
                            id: id.clone(),
                            delete_partial: false,
                            delete_file: false,
                        });
                    }
                    true
                })
        });
    }

    /// Confirm before draft-resetting Settings prefs to defaults (Save still required).
    pub(crate) fn confirm_reset_settings_defaults(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app_view = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let app_view = app_view.clone();
            let muted = cx.theme().muted_foreground;
            dialog
                .title("Reset settings to defaults?")
                .confirm()
                .overlay_closable(true)
                .keyboard(true)
                .button_props(DialogButtonProps::default().ok_text("Reset defaults"))
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div().text_sm().child(
                                "Theme, download limits, notifications, system options, and browser capture will return to recommended defaults.",
                            ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("Window size and download folder are kept."),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("You still need to press Save settings to persist."),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    app_view.update(cx, |app, cx| {
                        app.reset_settings_draft(window, cx);
                    });
                    true
                })
        });
    }

    /// Confirm before enqueueing URLs found on the clipboard (never auto-downloads).
    pub(crate) fn confirm_add_clipboard_urls(
        &mut self,
        urls: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if urls.is_empty() {
            return;
        }

        let count = urls.len();
        let title = if count == 1 {
            "Add download from clipboard?".to_string()
        } else {
            format!("Add {count} downloads from clipboard?")
        };

        let preview_lines: Vec<String> = urls
            .iter()
            .take(5)
            .map(|u| {
                // Char-boundary safe: URLs can contain non-ASCII after extraction.
                let mut iter = u.chars();
                let prefix: String = iter.by_ref().take(69).collect();
                if iter.next().is_some() {
                    format!("{prefix}…")
                } else {
                    u.clone()
                }
            })
            .collect();
        let mut body = preview_lines.join("\n");
        if count > 5 {
            body.push_str(&format!("\n…and {} more", count - 5));
        }

        let engine = self.engine.clone();
        let settings = self.settings.clone();
        let app_view = cx.entity().clone();

        window.open_dialog(cx, move |dialog, _, cx| {
            let engine = engine.clone();
            let urls = urls.clone();
            let settings = settings.clone();
            let app_view = app_view.clone();
            let title = title.clone();
            let body = body.clone();
            let muted = cx.theme().muted_foreground;
            dialog
                .title(title)
                .confirm()
                .overlay_closable(true)
                .keyboard(true)
                .button_props(DialogButtonProps::default().ok_text(if count == 1 {
                    "Add download"
                } else {
                    "Add downloads"
                }))
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(format!(
                            "Clipboard has {count} HTTP(S) URL(s). Add to the queue?"
                        )))
                        .child(div().text_xs().text_color(muted).child(body))
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("Nothing downloads until you confirm."),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let n = enqueue_urls(urls.clone(), &settings, None, None, &engine);
                    app_view.update(cx, |app, cx| {
                        app.show_toast(
                            if n == 1 {
                                "Added 1 download from clipboard.".to_string()
                            } else {
                                format!("Added {n} downloads from clipboard.")
                            },
                            cx,
                        );
                    });
                    true
                })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::clipboard_urls_key;

    #[test]
    fn clipboard_urls_key_order_independent() {
        let a = vec![
            "https://b.example/x".to_string(),
            "https://a.example/y".to_string(),
        ];
        let b = vec![
            "https://a.example/y".to_string(),
            "https://b.example/x".to_string(),
        ];
        assert_eq!(clipboard_urls_key(&a), clipboard_urls_key(&b));
    }

    #[test]
    fn clipboard_urls_key_differs_when_set_changes() {
        let a = vec!["https://a.example/1".to_string()];
        let b = vec!["https://a.example/2".to_string()];
        assert_ne!(clipboard_urls_key(&a), clipboard_urls_key(&b));
    }
}
