use gpui::{App, Entity, PathPromptOptions, SharedString, Window};
use gpui_component::input::InputState;
use std::path::PathBuf;

use super::super::DownloadApp;

pub(crate) fn shorten_path_display(path: &str) -> String {
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

/// Uses GPUI's native path prompt (with a proper parent HWND on Windows) instead
/// of `rfd`, which often fails silently or opens behind the app window.
pub(crate) fn browse_directory(
    input: Entity<InputState>,
    app_view: Entity<DownloadApp>,
    window: &mut Window,
    cx: &mut App,
) {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(SharedString::from("Select Folder")),
    });

    window
        .spawn(cx, async move |cx| match rx.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let _ = cx.update(|window, cx| {
                        input.update(cx, |state, cx| {
                            state.set_value(path.to_string_lossy().to_string(), window, cx);
                        });
                    });
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(err)) => {
                let _ = app_view.update(cx, |app, cx| {
                    app.show_error_toast(format!("Could not open folder picker: {err}"), cx);
                });
            }
            Err(_) => {}
        })
        .detach();
}
