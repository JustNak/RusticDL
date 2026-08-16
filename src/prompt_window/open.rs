//! Public window open APIs and shared capture window scaffolding.

use gpui::{
    px, size, App, AppContext, Bounds, Context, Pixels, SharedString, Size, Window, WindowBounds,
    WindowDecorations, WindowHandle, WindowKind, WindowOptions,
};
use gpui_component::Root;

use super::{
    BrowserPromptWindow, CAPTURE_COMPLETE_H, CAPTURE_CONFLICT_H, CAPTURE_CONFLICT_W,
    CAPTURE_WINDOW_H, CAPTURE_WINDOW_W,
};
use crate::branding::APP_NAME;
use crate::download::{EngineHandle, Job, JobState};
use crate::ipc::{BrowserPromptView, IpcBridge, PromptDecision};
use crate::settings::Settings;
use crate::window_placement::cascade_window;

/// Open the ask-mode browser confirm window (may morph into progress/complete).
pub fn open_browser_prompt_window(
    prompt: BrowserPromptView,
    ipc: IpcBridge,
    engine: EngineHandle,
    settings: &Settings,
    cx: &mut App,
) -> Option<WindowHandle<Root>> {
    let default_name = super::helpers::default_prompt_filename(&prompt);
    let opens_conflict = crate::download::find_filename_collision(
        &prompt.default_directory,
        &default_name,
        &ipc.jobs_snapshot(),
    )
    .is_some();
    let title = if opens_conflict {
        format!("{APP_NAME} — File already exists")
    } else {
        format!("{APP_NAME} — Confirm download")
    };
    open_capture_window(
        title,
        if opens_conflict {
            size(px(CAPTURE_CONFLICT_W), px(CAPTURE_CONFLICT_H))
        } else {
            size(px(CAPTURE_WINDOW_W), px(CAPTURE_WINDOW_H))
        },
        {
            let prompt = prompt.clone();
            let ipc = ipc.clone();
            let engine = engine.clone();
            let settings = settings.clone();
            move |window, cx| {
                BrowserPromptWindow::new_confirm(prompt, ipc, engine, &settings, window, cx)
            }
        },
        ipc,
        &prompt.id,
        cx,
    )
}

/// Open a progress (or complete) HUD for a browser-handoff job.
pub fn open_browser_progress_window(
    job_id: String,
    ipc: IpcBridge,
    engine: EngineHandle,
    settings: &Settings,
    cx: &mut App,
) -> Option<WindowHandle<Root>> {
    if !ipc.try_own_progress_job(&job_id) {
        return None;
    }
    let already_complete = ipc
        .job_by_id(&job_id)
        .is_some_and(|j| j.state == JobState::Completed);
    let opened = open_capture_window(
        format!("{APP_NAME} — Downloading"),
        size(
            px(CAPTURE_WINDOW_W),
            px(if already_complete {
                CAPTURE_COMPLETE_H
            } else {
                CAPTURE_WINDOW_H
            }),
        ),
        {
            let job_id = job_id.clone();
            let ipc = ipc.clone();
            let engine = engine.clone();
            let settings = settings.clone();
            move |window, cx| {
                BrowserPromptWindow::new_progress(job_id, ipc, engine, &settings, window, cx)
            }
        },
        ipc.clone(),
        &job_id,
        cx,
    );
    if opened.is_none() {
        ipc.release_progress_job(&job_id);
    }
    opened
}

/// Open the Complete HUD for a finished browser-handoff job (e.g. progress was closed early).
pub fn open_browser_complete_window(
    job: Job,
    ipc: IpcBridge,
    engine: EngineHandle,
    settings: &Settings,
    cx: &mut App,
) -> Option<WindowHandle<Root>> {
    if !ipc.try_claim_complete_hud(&job.id) {
        return None;
    }
    let _ = ipc.try_own_progress_job(&job.id);
    let job_id = job.id.clone();
    let opened = open_capture_window(
        format!("{APP_NAME} — Download complete"),
        size(px(CAPTURE_WINDOW_W), px(CAPTURE_COMPLETE_H)),
        {
            let ipc = ipc.clone();
            let engine = engine.clone();
            let settings = settings.clone();
            move |window, cx| {
                BrowserPromptWindow::new_complete(job, ipc, engine, &settings, window, cx)
            }
        },
        ipc.clone(),
        &job_id,
        cx,
    );
    if opened.is_none() {
        ipc.release_progress_job(&job_id);
        ipc.release_complete_hud(&job_id);
    }
    opened
}

fn open_capture_window<F>(
    title: String,
    prompt_size: Size<Pixels>,
    build: F,
    ipc_fallback: IpcBridge,
    fallback_prompt_id: &str,
    cx: &mut App,
) -> Option<WindowHandle<Root>>
where
    F: FnOnce(&mut Window, &mut Context<BrowserPromptWindow>) -> BrowserPromptWindow + 'static,
{
    let bounds = Bounds::centered(None, prompt_size, cx);
    let fallback_id = fallback_prompt_id.to_string();

    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some(SharedString::from(title)),
                appears_transparent: true,
                traffic_light_position: Some(gpui::point(px(9.0), px(9.0))),
            }),
            window_decorations: Some(WindowDecorations::Client),
            window_min_size: Some(size(px(360.0), px(160.0))),
            // Normal (not PopUp/tool-window): survives focus switches; only X closes.
            // Closing Progress/Complete releases HUD ownership — download keeps running.
            kind: WindowKind::Normal,
            focus: true,
            show: true,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        },
        move |window, cx| {
            let view = cx.new(|cx| build(window, cx));

            let view_for_close = view.clone();
            window.on_window_should_close(cx, move |window, cx| {
                let _ = view_for_close.update(cx, |this, cx| {
                    this.close_hud_on_native_close(window, cx);
                });
                true
            });

            cx.new(|cx| Root::new(view, window, cx))
        },
    );

    match result {
        Ok(handle) => {
            // Count includes this HUD (already claimed/owned). Index 0 stays centered.
            let cascade_index = ipc_fallback.capture_window_count().saturating_sub(1);
            cx.activate(true);
            let _ = handle.update(cx, |_root, window, _cx| {
                cascade_window(window, cascade_index);
                window.activate_window();
            });
            Some(handle)
        }
        Err(error) => {
            eprintln!("[capture] could not open browser capture window: {error:#}");
            // Best-effort dismiss if this was still a confirm prompt id.
            let _ = ipc_fallback.resolve_prompt(&fallback_id, PromptDecision::Dismiss);
            ipc_fallback.release_progress_job(&fallback_id);
            None
        }
    }
}
