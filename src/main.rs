#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod appearance;
mod assets;
mod branding;
mod download;
mod extension_settings;
mod format;
mod ipc;
mod persistence;
mod prompt_window;
mod settings;
mod startup;
mod tray;
mod updater;
mod window_icon;

use app::DownloadApp;
use assets::Assets;
use branding::{APP_NAME, APP_USER_MODEL_ID};
use download::spawn_engine;
use gpui::{
    point, px, size, App, AppContext, Application, Bounds, SharedString, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::{Root, TitleBar};
use ipc::{start_ipc_server, IpcBridge};
use persistence::{app_paths, ensure_app_dirs, load_jobs, load_settings};
use settings::{WindowLayout, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH};
use startup::{apply_launch_at_startup, launched_minimized};
use window_icon::apply_app_icon;

fn main() {
    set_app_user_model_id();

    let paths = app_paths();
    let _ = ensure_app_dirs(&paths);
    let settings = load_settings(&paths);
    // Keep the OS autostart entry aligned with saved prefs (self-heal after moves/updates).
    let _ = apply_launch_at_startup(settings.launch_at_startup, settings.startup_minimized);
    let start_hidden = launched_minimized();
    let jobs = load_jobs(&paths);

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _guard = runtime.enter();

    let (engine, mut event_rx) = spawn_engine(
        jobs.clone(),
        settings.max_concurrent_downloads,
        settings.auto_retry_attempts,
        settings.speed_limit_kib_per_second,
    );

    let ipc_bridge = IpcBridge::new(engine.clone(), &settings, paths.clone());
    ipc_bridge.update_jobs(&jobs);
    start_ipc_server(ipc_bridge.clone());

    let (ui_tx, ui_rx) = async_channel::unbounded();
    std::thread::spawn(move || {
        while let Some(event) = event_rx.blocking_recv() {
            if ui_tx.send_blocking(event).is_err() {
                break;
            }
        }
    });

    let initial_settings = settings;
    let initial_jobs = jobs;
    let initial_paths = paths;

    Application::new()
        .with_assets(Assets::new())
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            // Theme / accent / opacity are applied when the window opens
            // (DownloadApp::new → appearance::apply_appearance).

            let window_bounds = window_bounds_from_layout(&initial_settings.window_layout, cx);
            let settings = initial_settings;
            let jobs = initial_jobs;
            let paths = initial_paths;
            let engine = engine.clone();
            let ui_rx = ui_rx.clone();
            let ipc_bridge = ipc_bridge.clone();

            cx.spawn(async move |cx| {
                cx.open_window(
                    WindowOptions {
                        window_bounds: Some(window_bounds),
                        // Client-drawn chrome: gpui-component TitleBar supplies
                        // drag region + window controls. Keep a title string for
                        // taskbar / Alt-Tab while drawing our own bar.
                        titlebar: Some({
                            let mut opts = TitleBar::title_bar_options();
                            opts.title = Some(SharedString::from(APP_NAME));
                            opts
                        }),
                        window_decorations: Some(WindowDecorations::Client),
                        window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
                        // `--minimized` (startup-minimized) keeps the shell hidden until tray show.
                        show: !start_hidden,
                        ..Default::default()
                    },
                    move |window, cx| {
                        apply_app_icon(window);
                        let view = cx.new(|cx| {
                            DownloadApp::new(
                                jobs, settings, paths, engine, ui_rx, ipc_bridge, window, cx,
                            )
                        });
                        cx.new(|cx| Root::new(view, window, cx))
                    },
                )
                .expect("open window");
            })
            .detach();
        });
}

/// Build GPUI window bounds from the persisted layout.
///
/// Fresh install / missing position → centered default size.
/// Saved position is used only when it still intersects a display.
fn window_bounds_from_layout(layout: &WindowLayout, cx: &App) -> WindowBounds {
    let mut layout = layout.clone();
    layout.sanitize();

    let size = size(px(layout.width), px(layout.height));
    let bounds = match (layout.x, layout.y) {
        (Some(x), Some(y)) => {
            let candidate = Bounds {
                origin: point(px(x), px(y)),
                size,
            };
            if bounds_visible_on_any_display(&candidate, cx) {
                candidate
            } else {
                Bounds::centered(None, size, cx)
            }
        }
        _ => Bounds::centered(None, size, cx),
    };

    if layout.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

fn bounds_visible_on_any_display(bounds: &Bounds<gpui::Pixels>, cx: &App) -> bool {
    cx.displays()
        .iter()
        .any(|display| display.bounds().intersects(bounds))
}

/// Pin the process to a stable AppUserModelID so Start Menu / taskbar / jump
/// lists group under **RusticDL** (matches installer shortcut ApplicationID).
fn set_app_user_model_id() {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

        let wide: Vec<u16> = std::ffi::OsStr::new(APP_USER_MODEL_ID)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let _ = unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(wide.as_ptr())) };
    }
}
