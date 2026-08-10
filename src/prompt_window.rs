//! Dedicated floating window for browser ask-mode download confirmation.
//!
//! Lives independent of the main queue window so handoff still works when the
//! main UI is minimized or hidden.

use std::path::PathBuf;

use gpui::{
    div, px, size, App, AppContext, Bounds, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, PathPromptOptions, Render, SharedString, Styled, Window, WindowBounds,
    WindowDecorations, WindowHandle, WindowKind, WindowOptions,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    v_flex, ActiveTheme, IconName, Root, StyledExt, TitleBar,
};

use crate::appearance::apply_appearance;
use crate::branding::APP_NAME;
use crate::format::format_bytes;
use crate::ipc::{BrowserPromptView, IpcBridge, PromptDecision};
use crate::settings::Settings;
use crate::window_icon::apply_app_icon;

const PROMPT_WINDOW_W: f32 = 480.0;
const PROMPT_WINDOW_H: f32 = 360.0;

/// Root content of the browser download prompt window.
pub struct BrowserPromptWindow {
    prompt: BrowserPromptView,
    ipc: IpcBridge,
    name_input: Entity<InputState>,
    dir_input: Entity<InputState>,
    /// Prevent double resolve if both button and window-close fire.
    resolved: bool,
}

impl BrowserPromptWindow {
    pub fn new(
        prompt: BrowserPromptView,
        ipc: IpcBridge,
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

        Self {
            prompt,
            ipc,
            name_input,
            dir_input,
            resolved: false,
        }
    }

    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.resolved {
            return;
        }
        self.resolved = true;
        let _ = self
            .ipc
            .resolve_prompt(&self.prompt.id, PromptDecision::Dismiss);
        // If called from the close button path (not should_close), remove the window.
        window.remove_window();
        cx.notify();
    }

    /// Dismiss without calling `remove_window` — used from `on_window_should_close`.
    pub fn dismiss_on_close(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.resolved {
            return;
        }
        self.resolved = true;
        let _ = self
            .ipc
            .resolve_prompt(&self.prompt.id, PromptDecision::Dismiss);
        cx.notify();
    }

    pub fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.resolved {
            return;
        }
        self.resolved = true;

        let filename = {
            let raw = self.name_input.read(cx).value().to_string();
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        let directory = PathBuf::from(self.dir_input.read(cx).value().to_string());

        let _ = self.ipc.resolve_prompt(
            &self.prompt.id,
            PromptDecision::Accept {
                filename,
                directory: Some(directory),
            },
        );
        window.remove_window();
        cx.notify();
    }

    fn browse_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.dir_input.clone();
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
}

impl Render for BrowserPromptWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let size_label = self
            .prompt
            .total_bytes
            .filter(|n| *n > 0)
            .map(format_bytes)
            .unwrap_or_else(|| "Unknown size".into());
        let source_label = format!(
            "{} · {}",
            self.prompt.browser,
            self.prompt.entry_point.replace('_', " ")
        );
        let title_line = self
            .prompt
            .page_title
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or("Browser download");
        let url_display = truncate_middle(&self.prompt.url, 64);
        let save_preview = shorten_path(&self.dir_input.read(cx).value());

        // Path picker / other overlays hosted by Root.
        let dialog_layer = Root::render_dialog_layer(window, cx);

        v_flex()
            .id("browser-prompt-window")
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                TitleBar::new().child(
                    h_flex().gap_2().items_center().px_3().child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .child("Confirm browser download"),
                    ),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .w_full()
                    .px_4()
                    .py_3()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .child(title_line.to_string()),
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
                            .child(Input::new(&self.name_input).w_full()),
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
                                    .child(Input::new(&self.dir_input).w_full().flex_1())
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
                        div().text_xs().text_color(muted).child(
                            "Dismiss cancels the browser capture. The file will not download in the browser.",
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
                                        this.dismiss(window, cx);
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
                    ),
            )
            .children(dialog_layer)
    }
}

/// Open a dedicated browser prompt window (floating popup).
///
/// Returns a handle to the Root of that window when successful.
pub fn open_browser_prompt_window(
    prompt: BrowserPromptView,
    ipc: IpcBridge,
    settings: &Settings,
    cx: &mut App,
) -> Option<WindowHandle<Root>> {
    let prompt_id = prompt.id.clone();
    let prompt_size = size(px(PROMPT_WINDOW_W), px(PROMPT_WINDOW_H));
    let bounds = Bounds::centered(None, prompt_size, cx);
    let settings = settings.clone();
    let ipc_fallback = ipc.clone();

    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some({
                let mut opts = TitleBar::title_bar_options();
                opts.title = Some(SharedString::from(format!("{APP_NAME} — Confirm download")));
                opts
            }),
            window_decorations: Some(WindowDecorations::Client),
            window_min_size: Some(size(px(400.0), px(300.0))),
            kind: WindowKind::PopUp,
            focus: true,
            show: true,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        },
        move |window, cx| {
            let view = cx.new(|cx| BrowserPromptWindow::new(prompt, ipc, &settings, window, cx));

            // Titlebar close → dismiss handoff without double-removing the window.
            let view_for_close = view.clone();
            window.on_window_should_close(cx, move |window, cx| {
                let _ = view_for_close.update(cx, |this, cx| {
                    this.dismiss_on_close(window, cx);
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
                window.activate_window();
            });
            Some(handle)
        }
        Err(error) => {
            eprintln!("[prompt] could not open browser prompt window: {error:#}");
            let _ = ipc_fallback.resolve_prompt(&prompt_id, PromptDecision::Dismiss);
            None
        }
    }
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
