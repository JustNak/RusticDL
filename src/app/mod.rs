mod about_dialog;
mod add_dialog;
mod confirm_dialogs;
mod detail;
mod filter;
mod job_row;
mod layout;
mod queue_view;
mod selection;
mod settings_category;
mod settings_panel;
mod shortcuts;
mod sidebar;
mod status_bar;
mod title_bar;
mod toast;
mod update_flow;
mod widgets;

pub use filter::FilterKind;
pub(crate) use settings_category::SettingsCategory;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    canvas, div, point, prelude::FluentBuilder, px, size, App, AppContext, Bounds, Context,
    Corners, ElementId, Entity, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    KeystrokeEvent, MouseButton, MouseDownEvent, NavigationDirection, ParentElement, Render,
    SharedString, Styled, Window, WindowBounds,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{InputEvent, InputState},
    slider::{SliderEvent, SliderState},
    v_flex, ActiveTheme, Icon, IconName, Root, Sizable, WindowExt,
};

use crate::appearance::{
    apply_appearance, apply_window_opacity, film_grain_image, noise_enabled, vignette_edge_alpha,
    vignette_enabled,
};
use crate::download::{open_path, EngineCommand, EngineEvent, EngineHandle, Job, JobState};
use crate::extension_settings::{DownloadHandoffMode, ExtensionIntegrationSettings};
use crate::format::{filter_jobs, job_matches_search, sort_jobs};
use crate::ipc::IpcBridge;
use crate::notifications::{
    compose_balloon, filter_notify_edges, filter_pending_by_toggles, hard_os_eligible,
    in_app_summary_messages, soft_os_eligible, terminal_edges, BalloonContextMap, BalloonOutcome,
    InAppToastKind, OsNotifyBuffer, PendingOsTerminal, TerminalKind,
};
use crate::persistence::{save_jobs, save_settings, AppPaths};
use crate::prompt_window::open_browser_prompt_window;
use crate::settings::{
    AccentPreset, AppTheme, CornerRadiusScale, OsNotifyMode, ProgressStyle, Settings, SortColumn,
    SortDirection, UiDensity, WindowLayout, MAX_NOISE_INTENSITY, MAX_VIGNETTE_INTENSITY,
    MAX_WINDOW_TRANSPARENCY,
};
use crate::startup::{apply_launch_at_startup, launched_minimized};
use crate::tray::{
    hide_main_window, main_window_hwnd, show_main_window, show_main_window_hwnd, SystemTray,
    TrayEvent,
};
use crate::updater::UpdateInfo;
use toast::{Toast, ToastKind, TOAST_AUTO_HIDE, TOAST_MAX_STACK};
use widgets::render_vignette_overlay;

/// Debounce progress-driven `state.json` writes; terminal transitions flush immediately.
const JOBS_SAVE_DEBOUNCE: Duration = Duration::from_secs(1);

pub struct DownloadApp {
    jobs: Vec<Job>,
    settings: Settings,
    paths: AppPaths,
    engine: EngineHandle,
    ipc: IpcBridge,
    filter: FilterKind,
    /// Selected job ids; primary is `last()` (detail only when len == 1).
    selected_ids: Vec<String>,
    /// Anchor for future Shift+range multi-select (PR-07).
    selection_anchor_id: Option<String>,
    last_ui_update: Instant,
    pending_jobs: Option<Vec<Job>>,
    pending_toast: Option<String>,
    toasts: Vec<Toast>,
    next_toast_id: u64,
    search_input: Entity<InputState>,
    dir_input: Entity<InputState>,
    concurrent_input: Entity<InputState>,
    retry_input: Entity<InputState>,
    speed_input: Entity<InputState>,
    /// Draft textarea for `settings.extension.excluded_hosts` (one host per line).
    excluded_hosts_input: Entity<InputState>,
    /// Draft field for `settings.extension.captured_file_extensions` (comma-separated).
    captured_extensions_input: Entity<InputState>,
    /// Unsaved Browser capture toggles/enums (preview) not yet flushed via Save settings.
    extension_settings_dirty: bool,
    /// Last extension snapshot written to disk / IPC (or accepted from the bridge).
    extension_committed: ExtensionIntegrationSettings,
    /// When true, rewrite host/extension text drafts on the next frame that has a Window.
    extension_text_inputs_stale: bool,
    noise_slider: Entity<SliderState>,
    opacity_slider: Entity<SliderState>,
    hue_slider: Entity<SliderState>,
    sat_slider: Entity<SliderState>,
    light_slider: Entity<SliderState>,
    vignette_slider: Entity<SliderState>,
    /// Last transparency value pushed to the platform (avoids re-setting layered style every frame).
    applied_window_transparency: Option<u8>,
    /// In-memory window geometry changed since last disk write.
    window_layout_dirty: bool,
    /// Throttle how often we rewrite settings.json while the user drags a resize edge.
    last_window_layout_save: Instant,
    /// Browser ask-mode prompt currently shown in its dedicated window.
    browser_prompt_open_id: Option<String>,
    /// Queue snapshot differs from last successful disk write.
    jobs_dirty: bool,
    /// Throttle progress-only state.json writes.
    last_jobs_save: Instant,
    /// True while a GitHub update check or updater handoff is running.
    update_busy: bool,
    /// Cached latest release when an update is available (Install update menu label).
    available_update: Option<UpdateInfo>,
    /// Interactive check found an update; open the dialog on the next frame with a Window.
    pending_show_update_dialog: bool,
    /// System tray icon (Windows). Present when close-to-tray, hidden-to-tray,
    /// or OS notify mode is enabled (`sync_tray_lifetime`).
    system_tray: Option<SystemTray>,
    /// When true, the next close request actually quits (tray Exit / updates).
    force_quit: bool,
    /// Main window is currently hidden to the tray.
    window_hidden_to_tray: bool,
    /// Win32 HWND of the main window (0 if unknown). Used to restore without render.
    main_hwnd: isize,
    /// Tray "Show" was requested; applied on the next render (has Window).
    pending_tray_show: bool,
    /// Tray "Exit" was requested; applied on the next render.
    pending_tray_exit: bool,
    /// Balloon click context id to resolve after showing the main window.
    pending_balloon_click: Option<u64>,
    /// OS balloon burst-coalesce buffer (Pipeline B).
    os_notify_buffer: OsNotifyBuffer,
    /// Last-N balloon click contexts for open-file / show policy.
    balloon_contexts: BalloonContextMap,
    /// Debounce key for the last clipboard URL set offered on focus (PR-10).
    last_clipboard_urls_key: Option<u64>,
    /// Active Settings mini-nav category (does not discard draft when switched).
    settings_category: SettingsCategory,
    /// Queue filter to restore when leaving Settings (Back / Esc / mouse-back).
    /// Never `FilterKind::Settings`.
    settings_return_filter: FilterKind,
}

impl DownloadApp {
    pub fn new(
        jobs: Vec<Job>,
        settings: Settings,
        paths: AppPaths,
        engine: EngineHandle,
        event_rx: async_channel::Receiver<EngineEvent>,
        ipc: IpcBridge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search name, URL, or path…")
                .clean_on_escape()
        });
        let dir_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Download directory")
                .default_value(settings.download_directory.to_string_lossy().to_string())
        });
        let concurrent_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.max_concurrent_downloads.to_string())
        });
        let retry_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.auto_retry_attempts.to_string())
        });
        let speed_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("0 = unlimited")
                .default_value(settings.speed_limit_kib_per_second.to_string())
        });
        let excluded_hosts_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(3)
                .placeholder("One host per line…")
                .default_value(settings.extension.excluded_hosts.join("\n"))
        });
        let captured_extensions_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(3)
                .placeholder("zip, pdf, exe… (comma-separated)")
                .default_value(settings.extension.captured_file_extensions.join(", "))
        });

        let noise_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(MAX_NOISE_INTENSITY as f32)
                .step(1.0)
                .default_value(settings.noise_intensity as f32)
        });
        let opacity_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(MAX_WINDOW_TRANSPARENCY as f32)
                .step(1.0)
                .default_value(settings.window_transparency as f32)
        });
        let hue_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(360.0)
                .step(1.0)
                .default_value(settings.accent_hue)
        });
        let sat_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(100.0)
                .step(1.0)
                .default_value(settings.accent_saturation)
        });
        let light_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(100.0)
                .step(1.0)
                .default_value(settings.accent_lightness)
        });
        let vignette_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(MAX_VIGNETTE_INTENSITY as f32)
                .step(1.0)
                .default_value(settings.vignette_intensity as f32)
        });

        cx.subscribe(&search_input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change | InputEvent::PressEnter { .. }) {
                cx.notify();
            }
        })
        .detach();

        cx.subscribe(&noise_slider, |this, _, event: &SliderEvent, cx| {
            let SliderEvent::Change(v) = event;
            this.settings.noise_intensity = v.start().round().clamp(0.0, 100.0) as u8;
            cx.notify();
        })
        .detach();

        cx.subscribe(&opacity_slider, |this, _, event: &SliderEvent, cx| {
            let SliderEvent::Change(v) = event;
            this.settings.window_transparency = v.start().round().clamp(0.0, 100.0) as u8;
            // Window chrome (layered alpha) is applied on the next frame via
            // sync_window_chrome; theme accents stay as-is.
            cx.notify();
        })
        .detach();

        cx.subscribe(&hue_slider, |this, _, event: &SliderEvent, cx| {
            let SliderEvent::Change(v) = event;
            this.settings.accent_hue = v.start().rem_euclid(360.0);
            if this.settings.accent_preset == AccentPreset::Custom {
                apply_appearance(&this.settings, None, cx);
            }
            cx.notify();
        })
        .detach();

        cx.subscribe(&sat_slider, |this, _, event: &SliderEvent, cx| {
            let SliderEvent::Change(v) = event;
            this.settings.accent_saturation = v.start().clamp(0.0, 100.0);
            if this.settings.accent_preset == AccentPreset::Custom {
                apply_appearance(&this.settings, None, cx);
            }
            cx.notify();
        })
        .detach();

        cx.subscribe(&light_slider, |this, _, event: &SliderEvent, cx| {
            let SliderEvent::Change(v) = event;
            this.settings.accent_lightness = v.start().clamp(0.0, 100.0);
            if this.settings.accent_preset == AccentPreset::Custom {
                apply_appearance(&this.settings, None, cx);
            }
            cx.notify();
        })
        .detach();

        cx.subscribe(&vignette_slider, |this, _, event: &SliderEvent, cx| {
            let SliderEvent::Change(v) = event;
            this.settings.vignette_intensity = v.start().round().clamp(0.0, 100.0) as u8;
            cx.notify();
        })
        .detach();

        apply_appearance(&settings, Some(window), cx);

        // Escape must run *before* widget keybindings (Input binds Escape).
        // Root `capture_key_down` only runs after actions, so Esc was swallowed
        // while mouse-back (separate path) still left Settings correctly.
        //
        // `intercept_keystrokes` is app-global (every window). Scope to the main
        // DownloadApp window so Esc in BrowserPromptWindow does not leave
        // Settings / clear selection or stop propagation for the prompt.
        let escape_view = cx.weak_entity();
        cx.intercept_keystrokes(move |event: &KeystrokeEvent, window, cx| {
            if event.keystroke.key.as_str() != "escape" || event.keystroke.modifiers.modified() {
                return;
            }
            let Some(entity) = escape_view.upgrade() else {
                return;
            };
            let handled = entity.update(cx, |app, cx| {
                let event_hwnd = main_window_hwnd(window);
                if app.main_hwnd != 0 && event_hwnd != 0 && event_hwnd != app.main_hwnd {
                    return false;
                }
                app.handle_escape_keystroke(window, cx)
            });
            if handled {
                cx.stop_propagation();
            }
        })
        .detach();

        // Persist size / position / maximized across launches.
        cx.observe_window_bounds(window, |this, window, _cx| {
            this.capture_window_layout(window);
        })
        .detach();

        // Opt-in clipboard URL watch on main-window focus gain (never auto-downloads).
        cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                return;
            }
            this.on_window_activated(window, cx);
        })
        .detach();

        cx.spawn(async move |this, cx| {
            while let Ok(event) = event_rx.recv().await {
                let result = this.update(cx, |app, cx| match event {
                    EngineEvent::JobsChanged(jobs) => app.on_jobs_changed(jobs, cx),
                    EngineEvent::Toast(message) => {
                        app.pending_toast = Some(message);
                        cx.notify();
                    }
                });
                if result.is_err() {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                if this
                    .update(cx, |app, cx| {
                        if let Some(jobs) = app.pending_jobs.take() {
                            app.apply_jobs(jobs, cx);
                        }
                        // OS balloon burst deadline (Pipeline B).
                        app.flush_os_notify_if_due(cx);
                        // Flush a debounced window-layout write after the user stops resizing.
                        app.flush_window_layout_if_due();
                        app.flush_jobs_save_if_due();
                        // Dedicated prompt windows open even if the main UI is idle.
                        app.poll_browser_prompt(cx);
                        // Second-instance / extension show_window while hidden to tray.
                        app.poll_hidden_window_actions(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        ipc.update_settings(&settings);
        ipc.update_jobs(&jobs);

        let started_minimized = launched_minimized();
        // Tray is needed for close-to-tray, startup-minimized, and OS balloons.
        let need_tray = settings.close_to_tray
            || started_minimized
            || settings.os_notify_mode != OsNotifyMode::Off;
        let (tray_tx, tray_rx) = async_channel::unbounded::<TrayEvent>();
        let system_tray = if need_tray {
            SystemTray::start(tray_tx)
        } else {
            drop(tray_tx);
            None
        };

        if system_tray.is_some() {
            cx.spawn(async move |this, cx| {
                while let Ok(event) = tray_rx.recv().await {
                    let result = this.update(cx, |app, cx| app.handle_tray_event(event, cx));
                    if result.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }

        let extension_committed = settings.extension.clone();
        let mut app = Self {
            jobs,
            settings,
            paths,
            engine,
            ipc,
            filter: FilterKind::All,
            selected_ids: Vec::new(),
            selection_anchor_id: None,
            last_ui_update: Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
            pending_jobs: None,
            pending_toast: None,
            toasts: Vec::new(),
            next_toast_id: 1,
            search_input,
            dir_input,
            concurrent_input,
            retry_input,
            speed_input,
            excluded_hosts_input,
            captured_extensions_input,
            extension_settings_dirty: false,
            extension_committed,
            extension_text_inputs_stale: false,
            noise_slider,
            opacity_slider,
            hue_slider,
            sat_slider,
            light_slider,
            vignette_slider,
            applied_window_transparency: None,
            window_layout_dirty: false,
            // Allow an immediate first save after the first real bounds change.
            last_window_layout_save: Instant::now()
                .checked_sub(Duration::from_secs(2))
                .unwrap_or_else(Instant::now),
            browser_prompt_open_id: None,
            jobs_dirty: false,
            last_jobs_save: Instant::now()
                .checked_sub(Duration::from_secs(2))
                .unwrap_or_else(Instant::now),
            update_busy: false,
            available_update: None,
            pending_show_update_dialog: false,
            system_tray,
            force_quit: false,
            window_hidden_to_tray: started_minimized,
            main_hwnd: main_window_hwnd(window),
            pending_tray_show: false,
            pending_tray_exit: false,
            pending_balloon_click: None,
            os_notify_buffer: OsNotifyBuffer::default(),
            balloon_contexts: BalloonContextMap::default(),
            last_clipboard_urls_key: None,
            settings_category: SettingsCategory::General,
            settings_return_filter: FilterKind::All,
        };

        // Quiet startup check against GitHub Releases (toast only if an update exists).
        // Route through begin_update_check so update_busy serializes with interactive checks.
        app.begin_update_check(false, cx);

        // Close (X) → tray when enabled; tray Exit / force_quit still destroy the window.
        let entity = cx.entity();
        window.on_window_should_close(cx, move |window, cx| {
            entity.update(cx, |app, cx| app.handle_window_should_close(window, cx))
        });

        app
    }

    /// Intercept main-window close: hide to tray when the preference is on.
    fn handle_window_should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.force_quit || !self.settings.close_to_tray {
            self.flush_window_layout_now();
            self.flush_jobs_save_now();
            return true;
        }

        // Need a tray icon to restore; without one, quit instead of orphaning.
        self.ensure_tray(cx);
        if self.system_tray.is_none() {
            self.flush_window_layout_now();
            self.flush_jobs_save_now();
            return true;
        }

        self.flush_window_layout_now();
        self.flush_jobs_save_if_due();
        let hwnd = main_window_hwnd(window);
        if hwnd != 0 {
            self.main_hwnd = hwnd;
        }
        hide_main_window(window);
        self.window_hidden_to_tray = true;
        cx.notify();
        false
    }

    fn ensure_tray(&mut self, cx: &mut Context<Self>) {
        if self.system_tray.is_some() {
            return;
        }
        let (tray_tx, tray_rx) = async_channel::unbounded::<TrayEvent>();
        self.system_tray = SystemTray::start(tray_tx);
        if self.system_tray.is_none() {
            return;
        }
        cx.spawn(async move |this, cx| {
            while let Ok(event) = tray_rx.recv().await {
                let result = this.update(cx, |app, cx| app.handle_tray_event(event, cx));
                if result.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn stop_tray(&mut self) {
        self.system_tray = None;
    }

    /// Keep tray alive when close-to-tray, currently hidden, or OS notify is enabled.
    fn sync_tray_lifetime(&mut self, cx: &mut Context<Self>) {
        let needed = self.settings.close_to_tray
            || self.window_hidden_to_tray
            || self.settings.os_notify_mode != OsNotifyMode::Off;
        if needed {
            self.ensure_tray(cx);
        } else {
            self.stop_tray();
        }
    }

    fn handle_tray_event(&mut self, event: TrayEvent, cx: &mut Context<Self>) {
        match event {
            TrayEvent::ShowWindow => {
                // Show HWND immediately — hidden windows may not paint until visible again.
                self.restore_main_window_now();
                self.pending_tray_show = true;
                cx.notify();
            }
            TrayEvent::Exit => {
                self.force_quit = true;
                self.pending_tray_exit = true;
                cx.notify();
            }
            TrayEvent::BalloonUserClick { context_id } => {
                // Always restore/focus the main window; open-file resolved on render.
                self.restore_main_window_now();
                self.pending_tray_show = true;
                self.pending_balloon_click = Some(context_id);
                cx.notify();
            }
        }
    }

    /// Restore the main window using the cached HWND (no GPUI Window required).
    fn restore_main_window_now(&mut self) {
        self.window_hidden_to_tray = false;
        if self.main_hwnd != 0 {
            show_main_window_hwnd(self.main_hwnd);
        }
    }

    /// Poll IPC show_window while the main window may be hidden (no render).
    fn poll_hidden_window_actions(&mut self, cx: &mut Context<Self>) {
        if self.ipc.take_show_window_request() {
            self.restore_main_window_now();
            self.pending_tray_show = true;
            cx.notify();
        }
    }

    fn apply_pending_tray_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Keep HWND fresh whenever we have a Window (handles recreate edge cases).
        let hwnd = main_window_hwnd(window);
        if hwnd != 0 {
            self.main_hwnd = hwnd;
        }
        if self.pending_tray_show {
            self.pending_tray_show = false;
            self.window_hidden_to_tray = false;
            show_main_window(window);
        }
        if let Some(context_id) = self.pending_balloon_click.take() {
            self.handle_balloon_click(context_id, cx);
        }
        if self.pending_tray_exit {
            self.pending_tray_exit = false;
            self.force_quit = true;
            self.flush_window_layout_now();
            self.flush_jobs_save_now();
            self.stop_tray();
            cx.quit();
        }
    }

    /// Balloon click policy: show window already applied; open file for SingleComplete.
    fn handle_balloon_click(&mut self, context_id: u64, cx: &mut Context<Self>) {
        let Some(ctx) = self.balloon_contexts.lookup(context_id).cloned() else {
            return;
        };
        if ctx.kind != BalloonOutcome::SingleComplete {
            return;
        }
        let Some(path) = ctx.target_path else {
            return;
        };
        if let Err(msg) = open_path(&path) {
            self.show_error_toast(format!("Could not open file: {msg}"), cx);
        }
    }

    /// Snapshot restore-size + maximized from the platform window into settings.
    fn capture_window_layout(&mut self, window: &Window) {
        let layout = window_layout_from_window(window);
        if self.settings.window_layout == layout {
            return;
        }
        self.settings.window_layout = layout;
        self.window_layout_dirty = true;
        // Write promptly when quiet; during an active drag, throttle via the timer loop.
        self.flush_window_layout_if_due();
    }

    fn flush_window_layout_if_due(&mut self) {
        if !self.window_layout_dirty {
            return;
        }
        if self.last_window_layout_save.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.window_layout_dirty = false;
        self.last_window_layout_save = Instant::now();
        // Do not flush unsaved Browser capture previews via incidental layout writes.
        let _ = save_settings(&self.paths, &self.settings_for_disk());
    }

    fn flush_window_layout_now(&mut self) {
        if !self.window_layout_dirty {
            return;
        }
        self.window_layout_dirty = false;
        self.last_window_layout_save = Instant::now();
        let _ = save_settings(&self.paths, &self.settings_for_disk());
    }

    fn on_jobs_changed(&mut self, jobs: Vec<Job>, cx: &mut Context<Self>) {
        if self.last_ui_update.elapsed() < Duration::from_millis(80) {
            self.pending_jobs = Some(jobs);
            return;
        }
        self.apply_jobs(jobs, cx);
    }

    fn apply_jobs(&mut self, jobs: Vec<Job>, cx: &mut Context<Self>) {
        // Edge-detect BEFORE overwrite (Completed / Failed only — never Canceled).
        let edges = terminal_edges(&self.jobs, &jobs);
        let notify_edges = filter_notify_edges(
            &edges,
            self.settings.notify_on_complete,
            self.settings.notify_on_fail,
        );

        // Pipeline A — in-app toasts (immediate when window is visible).
        if !self.window_hidden_to_tray && !notify_edges.is_empty() {
            for (kind, message) in
                in_app_summary_messages(&notify_edges, self.settings.os_notify_mode)
            {
                match kind {
                    InAppToastKind::Info => self.show_toast(message, cx),
                    InAppToastKind::Error => self.show_error_toast(message, cx),
                }
            }
        }

        // Pipeline B — OS balloons (burst coalesce; hard eligibility at flush).
        if soft_os_eligible(self.settings.os_notify_mode) && !notify_edges.is_empty() {
            let pending: Vec<PendingOsTerminal> = notify_edges
                .iter()
                .map(PendingOsTerminal::from_edge)
                .collect();
            let now = Instant::now();
            if self.os_notify_buffer.enqueue(pending, now) {
                self.flush_os_notify(cx);
            }
        }

        let force_persist = jobs_need_immediate_persist(&self.jobs, &jobs);
        self.prune_selection(&jobs);
        self.jobs = jobs;
        self.last_ui_update = Instant::now();
        self.jobs_dirty = true;
        if force_persist {
            self.flush_jobs_save_now();
        } else {
            self.flush_jobs_save_if_due();
        }
        self.ipc.update_jobs(&self.jobs);
        // Adopt bridge extension settings only when the user has no local preview
        // (unsaved toggles). Never clobber while extension_settings_dirty.
        self.sync_extension_settings_from_bridge(false);
        cx.notify();
    }

    fn flush_os_notify_if_due(&mut self, cx: &mut Context<Self>) {
        if self.os_notify_buffer.deadline_elapsed(Instant::now()) {
            self.flush_os_notify(cx);
        }
    }

    /// Flush OS coalesce buffer: re-check hard eligibility, compose one balloon, show.
    ///
    /// Arms the 2s burst window only after a balloon is actually shown. Hard-drops
    /// (e.g. WhenHiddenToTray while visible) do not delay the next solitary balloon.
    fn flush_os_notify(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let pending = self.os_notify_buffer.take_pending();
        if pending.is_empty() {
            return;
        }

        // Hard eligibility with *current* mode / visibility / toggles.
        // Do not arm burst on drop — visible completes under WhenHiddenToTray must
        // not tax the first real OS balloon after the user hides to tray.
        if !hard_os_eligible(self.settings.os_notify_mode, self.window_hidden_to_tray) {
            return;
        }

        let pending = filter_pending_by_toggles(
            pending,
            self.settings.notify_on_complete,
            self.settings.notify_on_fail,
        );
        if pending.is_empty() {
            return;
        }

        let Some(payload) = compose_balloon(&pending) else {
            return;
        };

        // Tray required for balloons; ensure lifetime when notify is on.
        self.sync_tray_lifetime(cx);
        if let Some(tray) = self.system_tray.as_ref() {
            // Allocate context only when we can actually show a balloon.
            let context_id = self.balloon_contexts.allocate(&payload);
            tray.show_notification(&payload.title, &payload.body, payload.level, context_id);
            self.os_notify_buffer.after_flush(now);
        } else {
            eprintln!("rusticdl: OS notification skipped (tray unavailable)");
            // Always mode skips success in-app because "OS covers success". If the
            // tray is missing, fall back so visible completes are not silent.
            if self.settings.os_notify_mode == OsNotifyMode::Always && !self.window_hidden_to_tray {
                self.fallback_in_app_for_missed_os_complete(&pending, cx);
            }
        }
    }

    /// In-app Info for complete edges when Always OS path could not show a balloon.
    fn fallback_in_app_for_missed_os_complete(
        &mut self,
        pending: &[PendingOsTerminal],
        cx: &mut Context<Self>,
    ) {
        let completes: Vec<&PendingOsTerminal> = pending
            .iter()
            .filter(|p| p.kind == TerminalKind::Complete)
            .collect();
        if completes.is_empty() {
            return;
        }
        let message = if completes.len() == 1 {
            format!("Download complete: {}", completes[0].filename)
        } else {
            format!("{} downloads finished", completes.len())
        };
        self.show_toast(message, cx);
    }

    fn flush_jobs_save_if_due(&mut self) {
        if !self.jobs_dirty {
            return;
        }
        if self.last_jobs_save.elapsed() < JOBS_SAVE_DEBOUNCE {
            return;
        }
        self.flush_jobs_save_now();
    }

    fn flush_jobs_save_now(&mut self) {
        if !self.jobs_dirty {
            return;
        }
        self.jobs_dirty = false;
        self.last_jobs_save = Instant::now();
        let _ = save_jobs(&self.paths, &self.jobs);
    }

    fn flush_toast(&mut self, cx: &mut Context<Self>) {
        if let Some(message) = self.pending_toast.take() {
            self.push_toast(message, ToastKind::Info, cx);
        }
    }

    fn show_toast(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.push_toast(message, ToastKind::Info, cx);
    }

    fn show_error_toast(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.push_toast(message, ToastKind::Error, cx);
    }

    fn push_toast(&mut self, message: impl Into<String>, kind: ToastKind, cx: &mut Context<Self>) {
        let id = self.next_toast_id;
        self.next_toast_id = self.next_toast_id.wrapping_add(1);
        self.toasts.push(Toast {
            id,
            message: SharedString::from(message.into()),
            kind,
        });
        if self.toasts.len() > TOAST_MAX_STACK {
            let overflow = self.toasts.len() - TOAST_MAX_STACK;
            self.toasts.drain(0..overflow);
        }

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_AUTO_HIDE).await;
            let _ = this.update(cx, |app, cx| {
                app.dismiss_toast(id, cx);
            });
        })
        .detach();

        cx.notify();
    }

    fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.id != id);
        if self.toasts.len() != before {
            cx.notify();
        }
    }

    /// Bottom-right toast stack (clear of the ~30px status bar).
    fn render_toast_layer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let toasts = self.toasts.clone();

        div()
            .absolute()
            // status bar (~30px) + 16px margin
            .bottom(px(46.))
            .right_4()
            .child(
                v_flex()
                    .id("toast-list")
                    .gap_3()
                    .children(toasts.into_iter().map(|toast| {
                        let id = toast.id;
                        let (icon, icon_color) = match toast.kind {
                            ToastKind::Info => (IconName::Info, theme.info),
                            ToastKind::Error => (IconName::CircleX, theme.danger),
                        };
                        h_flex()
                            .id(ElementId::from(("toast", id)))
                            .occlude()
                            .items_start()
                            .gap_3()
                            .w_112()
                            .max_w(px(420.))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.popover)
                            .rounded(theme.radius_lg)
                            .shadow_md()
                            .py_3()
                            .px_4()
                            .child(div().pt_0p5().child(Icon::new(icon).text_color(icon_color)))
                            .child(div().flex_1().min_w_0().text_sm().child(toast.message))
                            .child(
                                Button::new(ElementId::from(("toast-close", id)))
                                    .icon(IconName::Close)
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.dismiss_toast(id, cx);
                                    })),
                            )
                    })),
            )
    }

    fn search_query(&self, cx: &App) -> String {
        self.search_input.read(cx).value().to_string()
    }

    fn visible_jobs(&self, cx: &App) -> Vec<Job> {
        let query = self.search_query(cx);
        let mut jobs: Vec<Job> = filter_jobs(&self.jobs, self.filter.as_index())
            .into_iter()
            .filter(|job| job_matches_search(job, &query))
            .cloned()
            .collect();
        sort_jobs(
            &mut jobs,
            self.settings.sort_column,
            self.settings.sort_direction,
        );
        jobs
    }

    /// Toggle or switch queue sort; persists the preference immediately.
    fn set_sort_column(&mut self, column: SortColumn, cx: &mut Context<Self>) {
        if self.settings.sort_column == column {
            self.settings.sort_direction = self.settings.sort_direction.toggle();
        } else {
            self.settings.sort_column = column;
            // Name reads naturally A→Z; metrics usually want largest/newest first.
            self.settings.sort_direction = match column {
                SortColumn::Name => SortDirection::Asc,
                _ => SortDirection::Desc,
            };
        }
        // Sort prefs only — do not flush unsaved Browser capture previews.
        let _ = save_settings(&self.paths, &self.settings_for_disk());
        cx.notify();
    }

    /// Settings snapshot safe for incidental disk writes (layout, sort).
    /// Keeps committed extension when the user has unsaved Browser capture previews.
    fn settings_for_disk(&self) -> Settings {
        let mut settings = self.settings.clone();
        if self.extension_settings_dirty {
            settings.extension = self.extension_committed.clone();
        }
        settings
    }

    /// Pull extension settings from the IPC bridge when safe.
    ///
    /// While dirty, keeps the live preview (`settings.extension`) but still
    /// advances `extension_committed` from the bridge so incidental disk
    /// flushes do not overwrite a newer extension-saved snapshot.
    fn sync_extension_settings_from_bridge(&mut self, force_text_refresh: bool) {
        let Some(extension) = self.ipc.extension_settings() else {
            return;
        };
        if self.extension_settings_dirty {
            // Keep preview; only track latest external/disk truth for incidental saves.
            if self.extension_committed != extension {
                self.extension_committed = extension;
            }
            return;
        }
        if self.settings.extension == extension {
            return;
        }
        self.settings.extension = extension.clone();
        self.extension_committed = extension;
        // When Settings is open, text drafts must follow the adopted snapshot
        // (Issue 5); refresh on the next render frame that has a Window.
        if force_text_refresh || self.filter == FilterKind::Settings {
            self.extension_text_inputs_stale = true;
        }
    }

    fn refresh_extension_text_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let excluded = self.settings.extension.excluded_hosts.join("\n");
        let captured = self.settings.extension.captured_file_extensions.join(", ");
        self.excluded_hosts_input
            .update(cx, |i, cx| i.set_value(excluded, window, cx));
        self.captured_extensions_input
            .update(cx, |i, cx| i.set_value(captured, window, cx));
        self.extension_text_inputs_stale = false;
    }

    fn save_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let download_directory = PathBuf::from(self.dir_input.read(cx).value().to_string());
        let max_concurrent = self
            .concurrent_input
            .read(cx)
            .value()
            .parse::<u32>()
            .unwrap_or(3)
            .max(1);
        let auto_retry = self
            .retry_input
            .read(cx)
            .value()
            .parse::<u32>()
            .unwrap_or(3);
        let speed_limit = self
            .speed_input
            .read(cx)
            .value()
            .parse::<u32>()
            .unwrap_or(0);

        self.settings.download_directory = download_directory;
        self.settings.max_concurrent_downloads = max_concurrent;
        self.settings.auto_retry_attempts = auto_retry;
        self.settings.speed_limit_kib_per_second = speed_limit;

        // Browser capture text lists — drafts until Save; sanitize via extension.sanitize().
        let excluded_hosts = self
            .excluded_hosts_input
            .read(cx)
            .value()
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        let captured_extensions = self
            .captured_extensions_input
            .read(cx)
            .value()
            .split(|c: char| c == ',' || c.is_whitespace())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        self.settings.extension.excluded_hosts = excluded_hosts;
        self.settings.extension.captured_file_extensions = captured_extensions;

        self.settings.sanitize_appearance();
        self.extension_settings_dirty = false;
        self.extension_committed = self.settings.extension.clone();
        // Show sanitized hosts/extensions in the drafts after Save.
        self.refresh_extension_text_inputs(window, cx);
        let _ = save_settings(&self.paths, &self.settings);
        self.ipc.update_settings(&self.settings);

        // Keep Windows Run-key entry in sync with launch preferences.
        if let Err(msg) = apply_launch_at_startup(
            self.settings.launch_at_startup,
            self.settings.startup_minimized,
        ) {
            self.show_toast(format!("Startup setting: {msg}"), cx);
        }

        // Tray lifetime: close-to-tray, hidden-to-tray, or OS notify != Off.
        self.sync_tray_lifetime(cx);

        apply_appearance(&self.settings, Some(window), cx);

        self.engine.send(EngineCommand::UpdateSettings {
            max_concurrent,
            auto_retry,
            speed_limit_kib: speed_limit,
        });

        self.show_toast("Settings saved.", cx);
        cx.notify();
    }

    fn set_close_to_tray(&mut self, on: bool, _window: &mut Window, cx: &mut Context<Self>) {
        self.settings.close_to_tray = on;
        self.sync_tray_lifetime(cx);
        cx.notify();
    }

    /// Browser capture toggles preview immediately; disk + IPC flush is "Save settings".
    fn set_extension_enabled(&mut self, on: bool, _window: &mut Window, cx: &mut Context<Self>) {
        self.settings.extension.enabled = on;
        self.extension_settings_dirty = true;
        cx.notify();
    }

    fn set_download_handoff_mode(
        &mut self,
        mode: DownloadHandoffMode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.extension.download_handoff_mode = mode;
        self.extension_settings_dirty = true;
        cx.notify();
    }

    fn set_context_menu_enabled(&mut self, on: bool, _window: &mut Window, cx: &mut Context<Self>) {
        self.settings.extension.context_menu_enabled = on;
        self.extension_settings_dirty = true;
        cx.notify();
    }

    fn set_show_badge_status(&mut self, on: bool, _window: &mut Window, cx: &mut Context<Self>) {
        self.settings.extension.show_badge_status = on;
        self.extension_settings_dirty = true;
        cx.notify();
    }

    fn set_show_progress_after_handoff(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.extension.show_progress_after_handoff = on;
        self.extension_settings_dirty = true;
        cx.notify();
    }

    fn set_download_capture_debug_logging(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.extension.download_capture_debug_logging = on;
        self.extension_settings_dirty = true;
        cx.notify();
    }

    fn set_launch_at_startup(&mut self, on: bool, _window: &mut Window, cx: &mut Context<Self>) {
        self.settings.launch_at_startup = on;
        if !on {
            self.settings.startup_minimized = false;
        }
        cx.notify();
    }

    fn set_startup_minimized(&mut self, on: bool, _window: &mut Window, cx: &mut Context<Self>) {
        self.settings.startup_minimized = on && self.settings.launch_at_startup;
        cx.notify();
    }

    fn set_os_notify_mode(
        &mut self,
        mode: OsNotifyMode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.os_notify_mode = mode;
        self.sync_tray_lifetime(cx);
        // Drop pending + burst window when turning Off so re-enable is clean.
        if mode == OsNotifyMode::Off {
            self.os_notify_buffer.clear();
        }
        cx.notify();
    }

    fn set_notify_on_complete(&mut self, on: bool, _window: &mut Window, cx: &mut Context<Self>) {
        self.settings.notify_on_complete = on;
        cx.notify();
    }

    fn set_notify_on_fail(&mut self, on: bool, _window: &mut Window, cx: &mut Context<Self>) {
        self.settings.notify_on_fail = on;
        cx.notify();
    }

    fn set_clipboard_watch_enabled(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.clipboard_watch_enabled = on;
        // Fresh enable should re-offer current clipboard on next focus.
        if !on {
            self.last_clipboard_urls_key = None;
        }
        cx.notify();
    }

    /// On main-window activation: optionally offer HTTP(S) clipboard URLs.
    ///
    /// Safety: never enqueues without a confirm dialog. Skips when disabled,
    /// tray-hidden, a dialog is already open, or the same URL set was just offered.
    fn on_window_activated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings.clipboard_watch_enabled {
            return;
        }
        if self.window_hidden_to_tray {
            return;
        }
        if window.has_active_dialog(cx) {
            return;
        }

        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let urls = crate::download::extract_http_urls(&text);
        if urls.is_empty() {
            return;
        }

        let key = confirm_dialogs::clipboard_urls_key(&urls);
        if self.last_clipboard_urls_key == Some(key) {
            return;
        }
        // Record before open so Cancel / focus flap does not re-prompt the same set.
        self.last_clipboard_urls_key = Some(key);
        self.confirm_add_clipboard_urls(urls, window, cx);
    }

    /// Poll for browser ask-mode handoffs and open a dedicated prompt window.
    ///
    /// Safe to call from the main window render loop or a background timer so
    /// prompts still appear when the main UI is idle or minimized.
    fn poll_browser_prompt(&mut self, cx: &mut Context<Self>) {
        // Clear tracking when the prompt was resolved, timed out, or the window closed.
        if let Some(id) = self.browser_prompt_open_id.clone() {
            if !self.ipc.is_prompt_pending(&id) {
                self.browser_prompt_open_id = None;
            }
            return;
        }

        let Some(prompt) = self.ipc.claim_next_prompt_for_ui() else {
            return;
        };
        let prompt_id = prompt.id.clone();
        let opened = open_browser_prompt_window(prompt, self.ipc.clone(), &self.settings, cx);
        if opened.is_some() {
            self.browser_prompt_open_id = Some(prompt_id);
        } else {
            self.browser_prompt_open_id = None;
        }
    }

    fn set_theme_draft(&mut self, theme: AppTheme, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.theme = theme;
        apply_appearance(&self.settings, Some(window), cx);
        cx.notify();
    }

    fn set_accent_preset(
        &mut self,
        preset: AccentPreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.accent_preset = preset;
        apply_appearance(&self.settings, Some(window), cx);
        cx.notify();
    }

    fn reset_appearance_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.reset_appearance();
        let noise = self.settings.noise_intensity as f32;
        let transparency = self.settings.window_transparency as f32;
        let hue = self.settings.accent_hue;
        let sat = self.settings.accent_saturation;
        let light = self.settings.accent_lightness;
        let vignette = self.settings.vignette_intensity as f32;
        self.noise_slider
            .update(cx, |s, cx| s.set_value(noise, window, cx));
        self.opacity_slider
            .update(cx, |s, cx| s.set_value(transparency, window, cx));
        self.hue_slider
            .update(cx, |s, cx| s.set_value(hue, window, cx));
        self.sat_slider
            .update(cx, |s, cx| s.set_value(sat, window, cx));
        self.light_slider
            .update(cx, |s, cx| s.set_value(light, window, cx));
        self.vignette_slider
            .update(cx, |s, cx| s.set_value(vignette, window, cx));
        apply_appearance(&self.settings, Some(window), cx);
        cx.notify();
    }

    /// Factory reset of settings prefs into the live draft (keeps window layout + download dir).
    ///
    /// Does **not** call `save_settings`, IPC, engine, or startup-registry updates. The
    /// Settings UI copy still asks the user to press **Save settings** to commit.
    ///
    /// Note (pre-existing draft architecture): `self.settings` is the single live model
    /// used by incidental flushes (`flush_window_layout_*`, sort persist, Drop). Those
    /// paths write `settings_for_disk()`, which only reverts **extension** when
    /// `extension_settings_dirty`; other draft fields (theme, limits, system, appearance,
    /// sort, …) can hit disk without an explicit Save. A full unsaved-settings snapshot
    /// is out of scope for the Reset-defaults PR — document only.
    pub(crate) fn reset_settings_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.reset_to_defaults_preserving_layout_and_dir();

        // Text inputs bound to General / Browser panels.
        let dir = self
            .settings
            .download_directory
            .to_string_lossy()
            .to_string();
        let concurrent = self.settings.max_concurrent_downloads.to_string();
        let retry = self.settings.auto_retry_attempts.to_string();
        let speed = self.settings.speed_limit_kib_per_second.to_string();
        self.dir_input
            .update(cx, |i, cx| i.set_value(dir, window, cx));
        self.concurrent_input
            .update(cx, |i, cx| i.set_value(concurrent, window, cx));
        self.retry_input
            .update(cx, |i, cx| i.set_value(retry, window, cx));
        self.speed_input
            .update(cx, |i, cx| i.set_value(speed, window, cx));
        self.refresh_extension_text_inputs(window, cx);

        // Appearance sliders (same rebind as reset_appearance_draft).
        let noise = self.settings.noise_intensity as f32;
        let transparency = self.settings.window_transparency as f32;
        let hue = self.settings.accent_hue;
        let sat = self.settings.accent_saturation;
        let light = self.settings.accent_lightness;
        let vignette = self.settings.vignette_intensity as f32;
        self.noise_slider
            .update(cx, |s, cx| s.set_value(noise, window, cx));
        self.opacity_slider
            .update(cx, |s, cx| s.set_value(transparency, window, cx));
        self.hue_slider
            .update(cx, |s, cx| s.set_value(hue, window, cx));
        self.sat_slider
            .update(cx, |s, cx| s.set_value(sat, window, cx));
        self.light_slider
            .update(cx, |s, cx| s.set_value(light, window, cx));
        self.vignette_slider
            .update(cx, |s, cx| s.set_value(vignette, window, cx));

        // Browser capture draft may now differ from last committed snapshot.
        self.extension_settings_dirty = self.settings.extension != self.extension_committed;
        if !self.settings.clipboard_watch_enabled {
            self.last_clipboard_urls_key = None;
        }
        // Match live System toggle side-effects for tray lifetime.
        self.sync_tray_lifetime(cx);
        apply_appearance(&self.settings, Some(window), cx);
        cx.notify();
    }

    fn sync_window_chrome(&mut self, window: &mut Window) {
        // Re-apply when either transparency or blur preference changes.
        // Encode as transparency in high bits-ish: just re-apply always when blur differs.
        let pct = self.settings.window_transparency;
        let key = pct.saturating_add(if self.settings.backdrop_blur { 128 } else { 0 });
        if self.applied_window_transparency == Some(key) {
            return;
        }
        apply_window_opacity(window, pct, self.settings.backdrop_blur);
        self.applied_window_transparency = Some(key);
    }

    fn set_ui_density(&mut self, density: UiDensity, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.ui_density = density;
        apply_appearance(&self.settings, Some(window), cx);
        cx.notify();
    }

    fn set_corner_radius(
        &mut self,
        scale: CornerRadiusScale,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.corner_radius = scale;
        apply_appearance(&self.settings, Some(window), cx);
        cx.notify();
    }

    fn set_backdrop_blur(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.backdrop_blur = on;
        apply_appearance(&self.settings, Some(window), cx);
        cx.notify();
    }

    fn set_reduce_motion(&mut self, on: bool, _window: &mut Window, cx: &mut Context<Self>) {
        self.settings.reduce_motion = on;
        cx.notify();
    }

    fn set_progress_style(
        &mut self,
        style: ProgressStyle,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.progress_style = style;
        cx.notify();
    }

    fn select_filter(&mut self, filter: FilterKind, window: &mut Window, cx: &mut Context<Self>) {
        // Remember the queue filter we came from so Back/Esc/mouse-back restore it.
        if filter == FilterKind::Settings && self.filter != FilterKind::Settings {
            self.settings_return_filter = self.filter;
        }
        self.filter = filter;
        if filter == FilterKind::Settings {
            // When the user has no local Browser capture preview, adopt any
            // bridge updates made while Settings was closed. Dirty previews
            // survive reopen (same idea as System toggles keeping in-memory
            // values until Save).
            self.sync_extension_settings_from_bridge(true);
            let dir = self
                .settings
                .download_directory
                .to_string_lossy()
                .to_string();
            let concurrent = self.settings.max_concurrent_downloads.to_string();
            let retry = self.settings.auto_retry_attempts.to_string();
            let speed = self.settings.speed_limit_kib_per_second.to_string();
            self.dir_input
                .update(cx, |i, cx| i.set_value(dir, window, cx));
            self.concurrent_input
                .update(cx, |i, cx| i.set_value(concurrent, window, cx));
            self.retry_input
                .update(cx, |i, cx| i.set_value(retry, window, cx));
            self.speed_input
                .update(cx, |i, cx| i.set_value(speed, window, cx));
            self.refresh_extension_text_inputs(window, cx);
        }
        cx.notify();
    }

    /// Leave Settings via Back / Esc / mouse-back / `/` search. Draft prefs are preserved.
    pub(crate) fn leave_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter != FilterKind::Settings {
            return;
        }
        let dest = match self.settings_return_filter {
            FilterKind::Settings => FilterKind::All,
            other => other,
        };
        self.select_filter(dest, window, cx);
    }

    /// Escape owner (keystroke interceptor): dialogs → leave Settings → clear selection.
    /// Returns true when the keystroke was handled (caller stops propagation).
    ///
    /// Runs before Input/dialog keybindings so Settings Esc matches mouse-back.
    /// Leaves search-field `clean_on_escape` alone when not in Settings.
    pub(crate) fn handle_escape_keystroke(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if window.has_active_dialog(cx) {
            window.close_dialog(cx);
            return true;
        }
        if self.filter == FilterKind::Settings {
            self.leave_settings(window, cx);
            return true;
        }
        // Queue search clears on Escape via InputState::clean_on_escape.
        if self.search_input.focus_handle(cx).is_focused(window) {
            return false;
        }
        if !self.selected_ids.is_empty() {
            self.clear_selection();
            cx.notify();
            return true;
        }
        false
    }

    /// Detail panel job: only when exactly one id is selected.
    fn selected_job(&self) -> Option<&Job> {
        if self.selected_ids.len() != 1 {
            return None;
        }
        let id = self.primary_selected_id()?;
        self.jobs.iter().find(|j| j.id == id)
    }

    fn filtered_count(&self) -> usize {
        filter_jobs(&self.jobs, self.filter.as_index()).len()
    }
}

impl Drop for DownloadApp {
    fn drop(&mut self) {
        // Ensure the last resize/move before close is written even if the
        // debounced timer never got another tick.
        self.flush_window_layout_now();
        self.flush_jobs_save_now();
    }
}

/// Persist immediately on queue membership or terminal-state changes; debounce pure progress.
fn jobs_need_immediate_persist(previous: &[Job], next: &[Job]) -> bool {
    if previous.len() != next.len() {
        return true;
    }
    use std::collections::HashMap;
    let prev: HashMap<&str, JobState> = previous
        .iter()
        .map(|job| (job.id.as_str(), job.state))
        .collect();
    for job in next {
        match prev.get(job.id.as_str()) {
            None => return true,
            Some(state) if *state != job.state => return true,
            _ => {}
        }
    }
    previous
        .iter()
        .any(|job| !next.iter().any(|n| n.id == job.id))
}

/// Capture restore bounds from the platform window (not the maximized full-screen rect).
fn window_layout_from_window(window: &Window) -> WindowLayout {
    let wb = window.window_bounds();
    let bounds = wb.get_bounds();
    let mut layout = WindowLayout {
        width: bounds.size.width.to_f64() as f32,
        height: bounds.size.height.to_f64() as f32,
        x: Some(bounds.origin.x.to_f64() as f32),
        y: Some(bounds.origin.y.to_f64() as f32),
        maximized: matches!(wb, WindowBounds::Maximized(_)),
    };
    layout.sanitize();
    layout
}

impl Render for DownloadApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.flush_toast(cx);
        self.poll_browser_prompt(cx);
        self.apply_pending_tray_actions(window, cx);
        self.apply_pending_update_dialog(window, cx);
        if self.ipc.take_show_window_request() {
            self.window_hidden_to_tray = false;
            show_main_window(window);
        }
        // Bridge-adopted extension settings (while Settings is open) need a
        // Window to rewrite text drafts; apply_jobs only sets the stale flag.
        if self.extension_text_inputs_stale && self.filter == FilterKind::Settings {
            self.refresh_extension_text_inputs(window, cx);
        }
        self.sync_window_chrome(window);
        let theme = cx.theme().clone();
        let noise_on = noise_enabled(self.settings.noise_intensity);
        // Intensity is baked into texture alpha (canvas element opacity is ignored by GPUI).
        let grain = noise_on.then(|| film_grain_image(self.settings.noise_intensity));
        let vignette_on = vignette_enabled(self.settings.vignette_intensity);
        let vignette_a = vignette_edge_alpha(self.settings.vignette_intensity);
        let is_dark = theme.is_dark();

        // Dialog / sheet overlays live on Root state, but must be painted by the
        // app view (see gpui-component Root docs). Toasts are app-owned so we can
        // place them bottom-right (library notifications are fixed top-right).
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let toast_layer = self.render_toast_layer(cx);

        div()
            .id("download-app-root")
            .size_full()
            .relative()
            .bg(theme.background)
            // Global shortcuts (capture phase): non-Escape bindings when no text
            // focus / dialog. Escape is owned by `intercept_keystrokes` →
            // `handle_escape_keystroke` (must run before Input keybindings).
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_key_down(event, window, cx);
            }))
            // Mouse back (XButton1): close dialog first, else leave Settings
            // (same destination as the sidebar Back row / Esc).
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                if !matches!(
                    event.button,
                    MouseButton::Navigate(NavigationDirection::Back)
                ) {
                    return;
                }
                if window.has_active_dialog(cx) {
                    window.close_dialog(cx);
                    cx.stop_propagation();
                    return;
                }
                if this.filter == FilterKind::Settings {
                    this.leave_settings(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                v_flex().size_full().child(self.render_title_bar(cx)).child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .child(if self.filter == FilterKind::Settings {
                            self.render_settings_sidebar(cx).into_any_element()
                        } else {
                            self.render_sidebar(cx).into_any_element()
                        })
                        .child(
                            v_flex()
                                .flex_1()
                                .h_full()
                                .min_w_0()
                                .child(if self.filter == FilterKind::Settings {
                                    self.render_settings(cx).into_any_element()
                                } else {
                                    self.render_queue(window, cx)
                                })
                                .when(self.filter != FilterKind::Settings, |col| {
                                    col.child(self.render_status_bar(cx))
                                }),
                        ),
                ),
            )
            // Film grain: 1:1 tiled paint. Strength is in the texture alpha
            // (GPUI canvas `.opacity()` does not affect paint_image).
            .when(noise_on, |this| {
                let grain = grain.expect("noise_on implies grain texture");
                this.child(
                    canvas(
                        |_bounds, _window, _cx| (),
                        move |bounds, (), window, _cx| {
                            let tile_px = grain.size(0);
                            let scale = window.scale_factor().max(0.5);
                            let tile_w = px((tile_px.width.0 as f32) / scale);
                            let tile_h = px((tile_px.height.0 as f32) / scale);
                            if tile_w <= px(1.) || tile_h <= px(1.) {
                                return;
                            }
                            let origin = bounds.origin;
                            let end_x = bounds.origin.x + bounds.size.width;
                            let end_y = bounds.origin.y + bounds.size.height;
                            let mut y = origin.y;
                            while y < end_y {
                                let mut x = origin.x;
                                while x < end_x {
                                    let w = tile_w.min(end_x - x);
                                    let h = tile_h.min(end_y - y);
                                    let cell = Bounds {
                                        origin: point(x, y),
                                        size: size(w, h),
                                    };
                                    let _ = window.paint_image(
                                        cell,
                                        Corners::default(),
                                        grain.clone(),
                                        0,
                                        false,
                                    );
                                    x += tile_w;
                                }
                                y += tile_h;
                            }
                        },
                    )
                    .absolute()
                    .inset_0()
                    .size_full(),
                )
            })
            // Soft edge vignette (above grain, below modals).
            .when(vignette_on, |this| {
                this.child(render_vignette_overlay(vignette_a, is_dark))
            })
            .children(sheet_layer)
            .children(dialog_layer)
            .child(toast_layer)
    }
}
