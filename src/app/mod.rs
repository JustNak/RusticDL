mod about_dialog;
mod add_dialog;
mod browser_capture;
mod confirm_dialogs;
mod detail;
mod filter;
mod job_row;
mod jobs_ui;
mod layout;
mod queue_view;
mod selection;
mod settings_actions;
mod settings_category;
mod settings_panel;
mod shortcuts;
mod sidebar;
mod status_bar;
mod title_bar;
mod toast;
mod tray_lifecycle;
mod update_flow;
mod widgets;

pub use filter::FilterKind;
pub(crate) use settings_category::SettingsCategory;

use std::time::{Duration, Instant};

use gpui::{
    canvas, div, point, prelude::FluentBuilder, px, size, App, AppContext, Bounds, Context,
    Corners, Entity, Focusable, InteractiveElement, IntoElement, KeyDownEvent, KeystrokeEvent,
    MouseButton, MouseDownEvent, NavigationDirection, ParentElement, Render, Styled, Window,
    WindowBounds,
};
use gpui_component::{
    h_flex,
    input::{InputEvent, InputState},
    slider::{SliderEvent, SliderState},
    v_flex, ActiveTheme, Root, WindowExt,
};

use crate::appearance::{
    apply_appearance, film_grain_image, noise_enabled, vignette_edge_alpha, vignette_enabled,
};
use crate::download::{EngineEvent, EngineHandle, Job};
use crate::extension_settings::ExtensionIntegrationSettings;
use crate::format::{filter_jobs, job_matches_search, sort_jobs};
use crate::ipc::IpcBridge;
use crate::notifications::{BalloonContextMap, OsNotifyBuffer};
use crate::persistence::{load_pending_whats_new, save_settings, AppPaths, PendingWhatsNew};
use crate::settings::{
    AccentPreset, OsNotifyMode, Settings, SortColumn, SortDirection, WindowLayout,
    MAX_NOISE_INTENSITY, MAX_VIGNETTE_INTENSITY, MAX_WINDOW_TRANSPARENCY,
};
use crate::startup::launched_minimized;
use crate::tray::{main_window_hwnd, show_main_window, SystemTray, TrayEvent};
use crate::updater::UpdateInfo;
use toast::Toast;
use widgets::render_vignette_overlay;

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
    /// Browser-handoff job ids that should re-open Complete if Progress was closed early.
    browser_watch_complete_ids: Vec<String>,
    /// Queue snapshot differs from last successful disk write.
    jobs_dirty: bool,
    /// Throttle progress-only state.json writes.
    last_jobs_save: Instant,
    /// True while a GitHub update check or updater handoff is running.
    update_busy: bool,
    /// Bumped when a check starts or is invalidated (e.g. channel switch).
    /// Completions whose generation no longer matches are dropped.
    update_check_gen: u64,
    /// Cached latest release when an update is available (menu label + toast actions).
    available_update: Option<UpdateInfo>,
    /// Id of the staged update-flow toast so check → result replaces instead of stacking.
    update_toast_id: Option<u64>,
    /// Post-update changelog snapshot (from `pending_whats_new.json` after relaunch).
    pending_whats_new: Option<PendingWhatsNew>,
    /// Open the What’s new dialog once a Window is free (no stacking over About/etc.).
    pending_show_whats_new: bool,
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
                        // Dedicated prompt / progress / complete windows even if main UI is idle.
                        app.poll_browser_prompt(cx);
                        app.poll_browser_progress(cx);
                        app.poll_browser_complete(cx);
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
            browser_watch_complete_ids: Vec::new(),
            jobs_dirty: false,
            last_jobs_save: Instant::now()
                .checked_sub(Duration::from_secs(2))
                .unwrap_or_else(Instant::now),
            update_busy: false,
            update_check_gen: 0,
            available_update: None,
            update_toast_id: None,
            pending_whats_new: None,
            pending_show_whats_new: false,
            system_tray,
            force_quit: false,
            window_hidden_to_tray: started_minimized,
            main_hwnd: main_window_hwnd(window),
            pending_tray_show: false,
            pending_balloon_click: None,
            os_notify_buffer: OsNotifyBuffer::default(),
            balloon_contexts: BalloonContextMap::default(),
            last_clipboard_urls_key: None,
            settings_category: SettingsCategory::General,
            settings_return_filter: FilterKind::All,
        };

        // Post-update What’s new: snapshot written before handoff, shown once after relaunch.
        if let Some(pending) = load_pending_whats_new(&app.paths) {
            app.pending_whats_new = Some(pending);
            app.pending_show_whats_new = true;
        }

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
        self.poll_browser_progress(cx);
        self.poll_browser_complete(cx);
        self.apply_pending_tray_actions(window, cx);
        // Post-update changelog after relaunch (once a Window is free).
        self.apply_pending_whats_new(window, cx);
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
