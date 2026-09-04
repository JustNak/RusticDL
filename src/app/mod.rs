mod about_dialog;
mod add_dialog;
mod browser_capture;
mod confirm_dialogs;
mod detail;
mod dialog_layout;
mod filter;
mod job_row;
mod jobs_ui;
mod layout;
mod queue_filter;
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
mod window_layout;

pub use filter::FilterKind;
pub(crate) use settings_category::SettingsCategory;

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    canvas, div, point, prelude::FluentBuilder, px, size, AppContext, Bounds, Context, Corners,
    Entity, Focusable, InteractiveElement, IntoElement, KeyDownEvent, KeystrokeEvent, MouseButton,
    MouseDownEvent, NavigationDirection, ParentElement, Render, Styled, Window, WindowHandle,
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
use crate::download::{EngineEvent, EngineHandle, FileTypeKind, Job};
use crate::extension_settings::ExtensionIntegrationSettings;
use crate::ipc::IpcBridge;
use crate::notifications::{BalloonContextMap, OsNotifyBuffer};
use crate::persistence::{load_pending_whats_new, AppPaths, PendingWhatsNew};
use crate::settings::{
    AccentPreset, OsNotifyMode, Settings, MAX_NOISE_INTENSITY, MAX_VIGNETTE_INTENSITY,
    MAX_WINDOW_TRANSPARENCY,
};
use crate::startup::launched_minimized;
use crate::tray::{main_window_hwnd, show_main_window, SystemTray, TrayEvent};
use crate::updater::UpdateInfo;
use toast::Toast;
use widgets::render_vignette_overlay;

pub struct DownloadApp {
    jobs: Arc<Vec<Job>>,
    latest_jobs: Arc<Vec<Job>>,
    settings: Settings,
    paths: AppPaths,
    engine: EngineHandle,
    ipc: IpcBridge,
    filter: FilterKind,
    selected_ids: Vec<String>,
    selection_anchor_id: Option<String>,
    last_ui_update: Instant,
    pending_jobs: Option<Arc<Vec<Job>>>,
    pending_toast: Option<String>,
    toasts: Vec<Toast>,
    next_toast_id: u64,
    search_input: Entity<InputState>,
    dir_input: Entity<InputState>,
    concurrent_input: Entity<InputState>,
    retry_input: Entity<InputState>,
    speed_input: Entity<InputState>,
    multi_max_segments_input: Entity<InputState>,
    multi_min_mib_input: Entity<InputState>,
    max_total_connections_input: Entity<InputState>,
    max_connections_per_host_input: Entity<InputState>,
    draft_multi_connection_enabled: bool,
    excluded_hosts_input: Entity<InputState>,
    captured_extensions_input: Entity<InputState>,
    category_folder_inputs: [Entity<InputState>; FileTypeKind::COUNT],
    extension_settings_dirty: bool,
    extension_committed: ExtensionIntegrationSettings,
    extension_text_inputs_stale: bool,
    noise_slider: Entity<SliderState>,
    opacity_slider: Entity<SliderState>,
    hue_slider: Entity<SliderState>,
    sat_slider: Entity<SliderState>,
    light_slider: Entity<SliderState>,
    vignette_slider: Entity<SliderState>,
    applied_window_transparency: Option<u8>,
    window_layout_dirty: bool,
    last_window_layout_save: Instant,
    browser_watch_complete_ids: Vec<String>,
    jobs_dirty: bool,
    last_jobs_save: Instant,
    update_busy: bool,
    update_check_gen: u64,
    available_update: Option<UpdateInfo>,
    update_toast_id: Option<u64>,
    pending_whats_new: Option<PendingWhatsNew>,
    pending_show_whats_new: bool,
    system_tray: Option<SystemTray>,
    force_quit: bool,
    window_hidden_to_tray: bool,
    main_hwnd: isize,
    pending_tray_show: bool,
    pending_balloon_click: Option<u64>,
    os_notify_buffer: OsNotifyBuffer,
    balloon_contexts: BalloonContextMap,
    last_clipboard_urls_key: Option<u64>,
    settings_category: SettingsCategory,
    settings_return_filter: FilterKind,
    /// Confirm / Progress / Complete HUD windows. Tray hide closes leftovers;
    /// new Confirm / Progress may still open while the main window is SW_HIDE.
    capture_windows: Vec<WindowHandle<Root>>,
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
        let multi_max_segments_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.multi_max_segments.to_string())
        });
        let multi_min_mib_input = cx.new(|cx| {
            let mib = (settings.multi_min_bytes / (1024 * 1024)).max(1);
            InputState::new(window, cx).default_value(mib.to_string())
        });
        let max_total_connections_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.max_total_connections.to_string())
        });
        let max_connections_per_host_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.max_connections_per_host.to_string())
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
        let category_folder_inputs = FileTypeKind::ALL.map(|kind| {
            let name = settings.category_folders.name(kind).to_string();
            cx.new(|cx| InputState::new(window, cx).default_value(name))
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

        for input in [
            &concurrent_input,
            &retry_input,
            &speed_input,
            &multi_max_segments_input,
            &multi_min_mib_input,
            &max_total_connections_input,
            &max_connections_per_host_input,
            &excluded_hosts_input,
            &captured_extensions_input,
            &dir_input,
        ] {
            cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            })
            .detach();
        }
        for input in &category_folder_inputs {
            cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            })
            .detach();
        }

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

        cx.observe_window_bounds(window, |this, window, _cx| {
            this.capture_window_layout(window);
        })
        .detach();

        cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                return;
            }
            this.on_window_activated(window, cx);
        })
        .detach();

        cx.spawn(async move |this, cx| {
            while let Ok(event) = event_rx.recv().await {
                let result = this.update(cx, |app, cx| {
                    match event {
                        EngineEvent::JobsChanged(jobs) => app.on_jobs_changed(jobs, cx),
                        EngineEvent::Toast(message) => {
                            app.pending_toast = Some(message);
                            cx.notify();
                        }
                    }
                    app.ipc.wake_ui();
                });
                if result.is_err() {
                    break;
                }
            }
        })
        .detach();

        let shell_wake = ipc.ui_wake();
        cx.spawn(async move |this, cx| loop {
            let park = match this.update(cx, |app, cx| app.run_shell_tick(cx)) {
                Ok(park) => park,
                Err(_) => break,
            };
            if park {
                shell_wake.notified().await;
            } else {
                cx.background_executor()
                    .timer(jobs_ui::SHELL_TICK_INTERVAL)
                    .await;
            }
        })
        .detach();

        ipc.update_settings(&settings);
        let jobs = Arc::new(jobs);
        ipc.update_jobs(Arc::clone(&jobs));

        let started_minimized = launched_minimized();
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
        let draft_multi_connection_enabled = settings.multi_connection_enabled;
        let mut app = Self {
            latest_jobs: Arc::clone(&jobs),
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
            multi_max_segments_input,
            multi_min_mib_input,
            max_total_connections_input,
            max_connections_per_host_input,
            draft_multi_connection_enabled,
            excluded_hosts_input,
            captured_extensions_input,
            category_folder_inputs,
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
            last_window_layout_save: Instant::now()
                .checked_sub(Duration::from_secs(2))
                .unwrap_or_else(Instant::now),
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
            capture_windows: Vec::new(),
        };

        if let Some(pending) = load_pending_whats_new(&app.paths) {
            app.pending_whats_new = Some(pending);
            app.pending_show_whats_new = true;
        }

        app.begin_update_check(false, cx);

        let entity = cx.entity();
        window.on_window_should_close(cx, move |window, cx| {
            entity.update(cx, |app, cx| app.handle_window_should_close(window, cx))
        });

        app
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

        let dir = self
            .settings
            .download_directory
            .to_string_lossy()
            .to_string();
        let concurrent = self.settings.max_concurrent_downloads.to_string();
        let retry = self.settings.auto_retry_attempts.to_string();
        let speed = self.settings.speed_limit_kib_per_second.to_string();
        let multi_segs = self.settings.multi_max_segments.to_string();
        let multi_mib = (self.settings.multi_min_bytes / (1024 * 1024))
            .max(1)
            .to_string();
        let max_total = self.settings.max_total_connections.to_string();
        let max_host = self.settings.max_connections_per_host.to_string();
        self.dir_input
            .update(cx, |i, cx| i.set_value(dir, window, cx));
        self.concurrent_input
            .update(cx, |i, cx| i.set_value(concurrent, window, cx));
        self.retry_input
            .update(cx, |i, cx| i.set_value(retry, window, cx));
        self.speed_input
            .update(cx, |i, cx| i.set_value(speed, window, cx));
        self.multi_max_segments_input
            .update(cx, |i, cx| i.set_value(multi_segs, window, cx));
        self.multi_min_mib_input
            .update(cx, |i, cx| i.set_value(multi_mib, window, cx));
        self.max_total_connections_input
            .update(cx, |i, cx| i.set_value(max_total, window, cx));
        self.max_connections_per_host_input
            .update(cx, |i, cx| i.set_value(max_host, window, cx));
        self.draft_multi_connection_enabled = self.settings.multi_connection_enabled;
        self.refresh_extension_text_inputs(window, cx);
        self.refresh_category_folder_inputs(window, cx);

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

        self.extension_settings_dirty = self.settings.extension != self.extension_committed;
        if !self.settings.clipboard_watch_enabled {
            self.last_clipboard_urls_key = None;
        }
        self.sync_tray_lifetime(cx);
        apply_appearance(&self.settings, Some(window), cx);
        cx.notify();
    }

    fn select_filter(&mut self, filter: FilterKind, window: &mut Window, cx: &mut Context<Self>) {
        if filter == FilterKind::Settings && self.filter != FilterKind::Settings {
            self.settings_return_filter = self.filter;
        }
        self.filter = filter;
        if filter == FilterKind::Settings {
            self.sync_extension_settings_from_bridge(true);
            let dir = self
                .settings
                .download_directory
                .to_string_lossy()
                .to_string();
            let concurrent = self.settings.max_concurrent_downloads.to_string();
            let retry = self.settings.auto_retry_attempts.to_string();
            let speed = self.settings.speed_limit_kib_per_second.to_string();
            let multi_segs = self.settings.multi_max_segments.to_string();
            let multi_mib = (self.settings.multi_min_bytes / (1024 * 1024))
                .max(1)
                .to_string();
            let max_total = self.settings.max_total_connections.to_string();
            let max_host = self.settings.max_connections_per_host.to_string();
            self.dir_input
                .update(cx, |i, cx| i.set_value(dir, window, cx));
            self.concurrent_input
                .update(cx, |i, cx| i.set_value(concurrent, window, cx));
            self.retry_input
                .update(cx, |i, cx| i.set_value(retry, window, cx));
            self.speed_input
                .update(cx, |i, cx| i.set_value(speed, window, cx));
            self.multi_max_segments_input
                .update(cx, |i, cx| i.set_value(multi_segs, window, cx));
            self.multi_min_mib_input
                .update(cx, |i, cx| i.set_value(multi_mib, window, cx));
            self.max_total_connections_input
                .update(cx, |i, cx| i.set_value(max_total, window, cx));
            self.max_connections_per_host_input
                .update(cx, |i, cx| i.set_value(max_host, window, cx));
            self.draft_multi_connection_enabled = self.settings.multi_connection_enabled;
            self.refresh_extension_text_inputs(window, cx);
            self.refresh_category_folder_inputs(window, cx);
        }
        if matches!(filter, FilterKind::FileType(_)) {
            self.settings.sidebar_library_expanded = true;
        }
        cx.notify();
    }

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
}

impl Drop for DownloadApp {
    fn drop(&mut self) {
        self.flush_window_layout_now();
        self.flush_jobs_save_now();
    }
}

impl Render for DownloadApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.flush_toast(cx);
        self.apply_pending_tray_actions(window, cx);
        self.apply_pending_whats_new(window, cx);
        if self.ipc.take_show_window_request() {
            self.window_hidden_to_tray = false;
            show_main_window(window);
        }
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
                h_flex()
                    .size_full()
                    .when(layout::sidebar_on_right(), |el| el.flex_row_reverse())
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
                            .child(self.render_title_bar(window, cx))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_h_0()
                                    .w_full()
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
            .when(vignette_on, |this| {
                this.child(render_vignette_overlay(vignette_a, is_dark))
            })
            .children(sheet_layer)
            .children(dialog_layer)
            .child(toast_layer)
    }
}
