mod add_dialog;
mod detail;
mod filter;
mod job_row;
mod layout;
mod settings_panel;
mod toast;
mod widgets;

pub use filter::FilterKind;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    canvas, div, point, prelude::FluentBuilder, px, size, App, AppContext, Bounds, Context, Corner,
    Corners, ElementId, Entity, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, NavigationDirection, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, WindowBounds,
};
use gpui_component::{
    button::{Button, ButtonVariant, ButtonVariants},
    description_list::DescriptionList,
    dialog::DialogButtonProps,
    divider::Divider,
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    slider::{SliderEvent, SliderState},
    v_flex, ActiveTheme, Disableable, Icon, IconName, Root, Sizable, StyledExt, TitleBar,
    WindowExt,
};

use crate::appearance::{
    apply_appearance, apply_window_opacity, film_grain_image, noise_enabled, vignette_edge_alpha,
    vignette_enabled,
};
use crate::branding::{APP_NAME, APP_VERSION};
use crate::download::{EngineCommand, EngineEvent, EngineHandle, Job, JobState};
use crate::format::{
    count_jobs, filter_jobs, format_bytes, format_speed, job_matches_search, sort_jobs,
    total_completed_bytes, total_download_speed,
};
use crate::ipc::IpcBridge;
use crate::persistence::{save_jobs, save_settings, AppPaths};
use crate::prompt_window::open_browser_prompt_window;
use crate::settings::{
    AccentPreset, AppTheme, CornerRadiusScale, ProgressStyle, Settings, SortColumn, SortDirection,
    UiDensity, WindowLayout, MAX_NOISE_INTENSITY, MAX_VIGNETTE_INTENSITY, MAX_WINDOW_TRANSPARENCY,
};
use crate::startup::{apply_launch_at_startup, launched_minimized};
use crate::tray::{hide_main_window, show_main_window, SystemTray, TrayEvent};
use crate::updater::{
    check_for_update, download_installer, launch_installer, open_release_page, open_url,
    UpdateCheck, UpdateInfo,
};
use detail::render_detail;
use job_row::render_job_row;
use layout::{
    QueueColumns, COL_ACTIONS_W, COL_DATE_W, COL_ETA_W, COL_SIZE_W, COL_SPEED_W, DETAIL_MAX_H,
    DETAIL_MIN_CAP, LIST_MIN_H, STATUS_DOT,
};
use toast::{Toast, ToastKind, TOAST_AUTO_HIDE, TOAST_MAX_STACK};
use widgets::{empty_state_badge, nav_item, render_vignette_overlay, sortable_header, status_chip};

/// Debounce progress-driven `state.json` writes; terminal transitions flush immediately.
const JOBS_SAVE_DEBOUNCE: Duration = Duration::from_secs(1);

pub struct DownloadApp {
    jobs: Vec<Job>,
    settings: Settings,
    paths: AppPaths,
    engine: EngineHandle,
    ipc: IpcBridge,
    filter: FilterKind,
    selected_id: Option<String>,
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
    /// True while a GitHub update check or installer download is running.
    update_busy: bool,
    /// Cached latest release when an update is available (enables one-click install).
    available_update: Option<UpdateInfo>,
    /// System tray icon (Windows). Present when close-to-tray is enabled.
    system_tray: Option<SystemTray>,
    /// When true, the next close request actually quits (tray Exit / updates).
    force_quit: bool,
    /// Main window is currently hidden to the tray.
    window_hidden_to_tray: bool,
    /// Tray "Show" was requested; applied on the next render (has Window).
    pending_tray_show: bool,
    /// Tray "Exit" was requested; applied on the next render.
    pending_tray_exit: bool,
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

        // Persist size / position / maximized across launches.
        cx.observe_window_bounds(window, |this, window, _cx| {
            this.capture_window_layout(window);
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
                        // Flush a debounced window-layout write after the user stops resizing.
                        app.flush_window_layout_if_due();
                        app.flush_jobs_save_if_due();
                        // Dedicated prompt windows open even if the main UI is idle.
                        app.poll_browser_prompt(cx);
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

        // Quiet startup check against GitHub Releases (toast only if an update exists).
        spawn_update_check(false, cx);

        let started_minimized = launched_minimized();
        // Tray is needed for close-to-tray and for startup-minimized restores.
        let tray_needed = settings.close_to_tray || started_minimized;
        let (tray_tx, tray_rx) = async_channel::unbounded::<TrayEvent>();
        let system_tray = if tray_needed {
            SystemTray::start(tray_tx)
        } else {
            drop(tray_tx);
            None
        };

        if system_tray.is_some() {
            cx.spawn(async move |this, cx| {
                while let Ok(event) = tray_rx.recv().await {
                    let result = this.update(cx, |app, cx| match event {
                        TrayEvent::ShowWindow => {
                            app.pending_tray_show = true;
                            cx.notify();
                        }
                        TrayEvent::Exit => {
                            app.force_quit = true;
                            app.pending_tray_exit = true;
                            cx.notify();
                        }
                    });
                    if result.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }

        let app = Self {
            jobs,
            settings,
            paths,
            engine,
            ipc,
            filter: FilterKind::All,
            selected_id: None,
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
            system_tray,
            force_quit: false,
            window_hidden_to_tray: started_minimized,
            pending_tray_show: false,
            pending_tray_exit: false,
        };

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
                let result = this.update(cx, |app, cx| match event {
                    TrayEvent::ShowWindow => {
                        app.pending_tray_show = true;
                        cx.notify();
                    }
                    TrayEvent::Exit => {
                        app.force_quit = true;
                        app.pending_tray_exit = true;
                        cx.notify();
                    }
                });
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

    fn apply_pending_tray_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_tray_show {
            self.pending_tray_show = false;
            self.window_hidden_to_tray = false;
            show_main_window(window);
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
        let _ = save_settings(&self.paths, &self.settings);
    }

    fn flush_window_layout_now(&mut self) {
        if !self.window_layout_dirty {
            return;
        }
        self.window_layout_dirty = false;
        self.last_window_layout_save = Instant::now();
        let _ = save_settings(&self.paths, &self.settings);
    }

    fn on_jobs_changed(&mut self, jobs: Vec<Job>, cx: &mut Context<Self>) {
        if self.last_ui_update.elapsed() < Duration::from_millis(80) {
            self.pending_jobs = Some(jobs);
            return;
        }
        self.apply_jobs(jobs, cx);
    }

    fn apply_jobs(&mut self, jobs: Vec<Job>, cx: &mut Context<Self>) {
        let force_persist = jobs_need_immediate_persist(&self.jobs, &jobs);
        self.jobs = jobs;
        self.last_ui_update = Instant::now();
        self.jobs_dirty = true;
        if force_persist {
            self.flush_jobs_save_now();
        } else {
            self.flush_jobs_save_if_due();
        }
        self.ipc.update_jobs(&self.jobs);
        // Keep desktop extension settings in sync if the bridge wrote them.
        if let Some(extension) = self.ipc.extension_settings() {
            if self.settings.extension != extension {
                self.settings.extension = extension;
            }
        }
        if let Some(id) = &self.selected_id {
            if !self.jobs.iter().any(|j| &j.id == id) {
                self.selected_id = None;
            }
        }
        cx.notify();
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
        let _ = save_settings(&self.paths, &self.settings);
        cx.notify();
    }

    fn open_about_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app_view = cx.entity().clone();
        let update_busy = self.update_busy;
        let update_action_label = self.update_action_label();
        window.open_dialog(cx, move |dialog, window, cx| {
            let theme = cx.theme().clone();
            let muted = theme.muted_foreground;
            let app_view_check = app_view.clone();

            // Match Add download: viewport-center so the card sits mid-window, not top-biased.
            let est_h = 320.0;
            let view_h = window.viewport_size().height.to_f64() as f32;
            let max_top = (view_h - est_h - 20.0).max(24.0);
            let margin_top = ((view_h - est_h) * 0.5).clamp(24.0, max_top);

            dialog
                .title(format!("About {APP_NAME}"))
                .alert()
                .w(px(420.))
                .margin_top(px(margin_top))
                .border_color(theme.border.opacity(0.32))
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_sm().child(crate::branding::APP_TAGLINE))
                        .child(
                            DescriptionList::new()
                                .columns(1)
                                .bordered(false)
                                .label_width(px(96.))
                                .item("Version", APP_VERSION, 1)
                                .item("Engine", "Single-stream + Range resume", 1)
                                .item("License", "MIT", 1)
                                .item("Updates", "GitHub Releases", 1),
                        )
                        .child(div().text_xs().text_color(muted).child(format!(
                            "Data folder: %APPDATA%\\{}\\",
                            crate::branding::APP_DATA_DIR_NAME
                        )))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("about-check-update")
                                        .outline()
                                        .small()
                                        .label(if update_busy {
                                            "Updating…".to_string()
                                        } else {
                                            update_action_label.clone()
                                        })
                                        .disabled(update_busy)
                                        .on_click(move |_, _window, cx| {
                                            app_view_check.update(cx, |app, cx| {
                                                app.begin_one_click_update(cx);
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("about-open-releases")
                                        .ghost()
                                        .small()
                                        .label("Open releases")
                                        .on_click(|_, _, _| {
                                            let _ = open_release_page();
                                        }),
                                ),
                        ),
                )
        });
    }

    /// Label for the single update action (check or install cached release).
    fn update_action_label(&self) -> String {
        if let Some(info) = &self.available_update {
            format!("Install update v{}", info.latest_version)
        } else {
            "Check for updates".into()
        }
    }

    /// One click: install a known update, or check GitHub and install if newer.
    fn begin_one_click_update(&mut self, cx: &mut Context<Self>) {
        if self.update_busy {
            self.show_toast("An update is already in progress…", cx);
            return;
        }
        if let Some(info) = self.available_update.clone() {
            self.begin_download_update(info.setup_download_url, cx);
            return;
        }
        self.begin_update_check(true, cx);
    }

    /// Manual or silent GitHub Releases update check.
    fn begin_update_check(&mut self, interactive: bool, cx: &mut Context<Self>) {
        if self.update_busy {
            if interactive {
                self.show_toast("An update check is already running…", cx);
            }
            return;
        }
        self.update_busy = true;
        if interactive {
            self.show_toast("Checking GitHub for updates…", cx);
        }
        cx.notify();
        spawn_update_check(interactive, cx);
    }

    fn on_update_check_finished(
        &mut self,
        interactive: bool,
        result: Result<UpdateCheck, String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(UpdateCheck::UpToDate { current, latest }) => {
                self.available_update = None;
                if interactive {
                    self.update_busy = false;
                    self.show_toast(
                        format!("You're up to date (v{current}; latest is v{latest})."),
                        cx,
                    );
                }
            }
            Ok(UpdateCheck::Available(info)) => {
                self.available_update = Some(info.clone());
                if interactive {
                    // One click: go straight to silent download + install (no second prompt).
                    let size_hint = info
                        .setup_size
                        .map(|n| format!(" ({})", format_bytes(n)))
                        .unwrap_or_default();
                    self.show_toast(
                        format!(
                            "{} — downloading and installing{size_hint}…",
                            info.release_name
                        ),
                        cx,
                    );
                    // Keep `update_busy` true across check → download.
                    self.begin_download_update_inner(info.setup_download_url, cx);
                } else {
                    let size_hint = info
                        .setup_size
                        .map(|n| format!(" · {}", format_bytes(n)))
                        .unwrap_or_default();
                    self.show_toast(
                        format!(
                            "Update available: v{} (you have v{}){size_hint}. Click “Install update” in the {} menu once.",
                            info.latest_version, info.current_version, APP_NAME
                        ),
                        cx,
                    );
                }
            }
            Err(message) => {
                if interactive {
                    self.update_busy = false;
                    self.show_error_toast(message, cx);
                }
            }
        }
        cx.notify();
    }

    fn begin_download_update(&mut self, download_url: String, cx: &mut Context<Self>) {
        if self.update_busy {
            self.show_toast("An update is already in progress…", cx);
            return;
        }
        self.update_busy = true;
        self.begin_download_update_inner(download_url, cx);
    }

    /// Download the setup binary, persist state, launch silently (`/S /R`), then quit.
    ///
    /// Launch happens only after flush so the installer's silent kill cannot race a dirty save.
    fn begin_download_update_inner(&mut self, download_url: String, cx: &mut Context<Self>) {
        self.show_toast("Downloading update from GitHub…", cx);
        cx.notify();

        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("Could not start download runtime: {e}"))
                .and_then(|rt| rt.block_on(download_installer(&download_url)));
            let _ = tx.send_blocking(result);
        });

        cx.spawn(async move |this, cx| {
            let result = rx
                .recv()
                .await
                .unwrap_or_else(|_| Err("Update download was cancelled unexpectedly.".into()));
            let _ = this.update(cx, |app, cx| {
                match result {
                    Ok(installer_path) => {
                        // Persist before spawn: silent NSIS may KillProcess before Drop runs.
                        app.flush_jobs_save_now();
                        app.flush_window_layout_now();
                        if let Err(message) = launch_installer(&installer_path, true) {
                            app.update_busy = false;
                            app.show_error_toast(message, cx);
                            cx.notify();
                            return;
                        }
                        app.show_toast("Installing update — RusticDL will restart…", cx);
                        // Bypass close-to-tray so quit actually tears down the process.
                        app.force_quit = true;
                        app.stop_tray();
                        cx.notify();
                        cx.quit();
                    }
                    Err(message) => {
                        app.update_busy = false;
                        app.show_error_toast(message, cx);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn confirm_remove(
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
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("Any partial .part file will also be deleted."),
                        ),
                )
                .on_ok(move |_, _, _| {
                    engine.send(EngineCommand::Remove {
                        id: id.clone(),
                        delete_partial: true,
                    });
                    true
                })
        });
    }

    /// Remove finished jobs (completed, failed, canceled) from the queue.
    fn confirm_clear_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                        });
                    }
                    true
                })
        });
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

        self.settings.sanitize_appearance();
        let _ = save_settings(&self.paths, &self.settings);
        self.ipc.update_settings(&self.settings);

        // Keep Windows Run-key entry in sync with launch preferences.
        if let Err(msg) = apply_launch_at_startup(
            self.settings.launch_at_startup,
            self.settings.startup_minimized,
        ) {
            self.show_toast(format!("Startup setting: {msg}"), cx);
        }

        // Tray is required for close-to-tray; drop it when the preference is off
        // and the window is not currently hidden.
        if self.settings.close_to_tray {
            self.ensure_tray(cx);
        } else if !self.window_hidden_to_tray {
            self.stop_tray();
        }

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
        if on {
            self.ensure_tray(cx);
        } else if !self.window_hidden_to_tray {
            self.stop_tray();
        }
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
        self.filter = filter;
        if filter == FilterKind::Settings {
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
        }
        cx.notify();
    }

    fn selected_job(&self) -> Option<&Job> {
        let id = self.selected_id.as_ref()?;
        self.jobs.iter().find(|j| &j.id == id)
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

/// Run a GitHub Releases update check on a background thread and deliver the result to the UI.
fn spawn_update_check(interactive: bool, cx: &mut Context<DownloadApp>) {
    let delay = if interactive {
        Duration::from_millis(0)
    } else {
        Duration::from_secs(4)
    };

    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("Could not start update runtime: {e}"))
            .and_then(|rt| rt.block_on(check_for_update()));
        let _ = tx.send_blocking(result);
    });

    cx.spawn(async move |this, cx| {
        let result = rx
            .recv()
            .await
            .unwrap_or_else(|_| Err("Update check was cancelled unexpectedly.".into()));
        let _ = this.update(cx, |app, cx| {
            app.on_update_check_finished(interactive, result, cx);
        });
    })
    .detach();
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
        if self.ipc.take_show_window_request() {
            self.window_hidden_to_tray = false;
            show_main_window(window);
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
            // Capture Escape before focused inputs consume it, so dialogs always dismiss.
            .capture_key_down(cx.listener(|_, event: &KeyDownEvent, window, cx| {
                if !window.has_active_dialog(cx) {
                    return;
                }
                if event.keystroke.key.as_str() == "escape" && !event.keystroke.modifiers.modified()
                {
                    window.close_dialog(cx);
                    cx.stop_propagation();
                }
            }))
            // Mouse back (XButton1) dismisses the top dialog, like a browser modal.
            .capture_any_mouse_down(cx.listener(|_, event: &MouseDownEvent, window, cx| {
                if !window.has_active_dialog(cx) {
                    return;
                }
                if matches!(
                    event.button,
                    MouseButton::Navigate(NavigationDirection::Back)
                ) {
                    window.close_dialog(cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                v_flex().size_full().child(self.render_title_bar(cx)).child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .child(self.render_sidebar(cx))
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
                                .child(self.render_status_bar(cx)),
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

impl DownloadApp {
    fn render_title_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let show_actions = self.filter != FilterKind::Settings;
        let update_busy = self.update_busy;
        let update_action_label = self.update_action_label();
        let release_page_url = self
            .available_update
            .as_ref()
            .map(|info| info.html_url.clone());
        let view = cx.entity();
        let filtered_count = self.filtered_count();
        let total_speed = total_download_speed(&self.jobs);
        let context_label = if self.filter == FilterKind::Settings {
            "Settings".to_string()
        } else if total_speed > 0 {
            format!("↓ {}", format_speed(total_speed))
        } else if filtered_count > 0 {
            format!(
                "{} · {}",
                self.filter.title(),
                self.filter.subtitle(filtered_count)
            )
        } else {
            String::new()
        };

        TitleBar::new().h(px(48.)).child(
            h_flex()
                .id("title-bar-content")
                .w_full()
                .h_full()
                .items_center()
                .gap_3()
                .pr_1()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .flex_shrink_0()
                        .child(
                            div()
                                .w(px(26.))
                                .h(px(26.))
                                .rounded(theme.radius)
                                .bg(theme.primary)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    Icon::new(IconName::ArrowDown)
                                        .with_size(px(14.))
                                        .text_color(theme.primary_foreground),
                                ),
                        )
                        .child(
                            // Clickable product name → overflow menu (updates).
                            Button::new("app-brand-menu")
                                .ghost()
                                .label(APP_NAME)
                                .tooltip("App menu")
                                .dropdown_menu_with_anchor(
                                    Corner::BottomLeft,
                                    move |menu, _window, _menu_cx| {
                                        let view = view.clone();
                                        menu.min_w(px(200.))
                                            .item(
                                                PopupMenuItem::new(if update_busy {
                                                    "Updating…".to_string()
                                                } else {
                                                    update_action_label.clone()
                                                })
                                                .icon(Icon::empty().path("icons/rotate-cw.svg"))
                                                .disabled(update_busy)
                                                .on_click({
                                                    let view = view.clone();
                                                    move |_, _window, cx| {
                                                        view.update(cx, |app, cx| {
                                                            app.begin_one_click_update(cx);
                                                        });
                                                    }
                                                }),
                                            )
                                            .separator()
                                            .item(
                                                PopupMenuItem::new("Open releases on GitHub")
                                                    .icon(IconName::ExternalLink)
                                                    .on_click({
                                                        let release_page_url =
                                                            release_page_url.clone();
                                                        move |_, _, _| {
                                                            if let Some(url) = &release_page_url {
                                                                let _ = open_url(url);
                                                            } else {
                                                                let _ = open_release_page();
                                                            }
                                                        }
                                                    }),
                                            )
                                            .separator()
                                            .item(
                                                PopupMenuItem::new(format!(
                                                    "About {APP_NAME}…  v{APP_VERSION}"
                                                ))
                                                .icon(IconName::Info)
                                                .on_click({
                                                    let view = view.clone();
                                                    move |_, window, cx| {
                                                        view.update(cx, |app, cx| {
                                                            app.open_about_dialog(window, cx);
                                                        });
                                                    }
                                                }),
                                            )
                                    },
                                ),
                        )
                        .when(!context_label.is_empty(), |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(context_label),
                            )
                        }),
                )
                .child(div().flex_1())
                .when(show_actions, |el| {
                    el.child(
                        Button::new("add-download")
                            .primary()
                            .icon(IconName::Plus)
                            .label("Add download")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_add_dialog(window, cx);
                            })),
                    )
                }),
        )
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (all, active, completed, failed) = count_jobs(&self.jobs);
        let theme = cx.theme().clone();
        let filter = self.filter;
        let sidebar_w = self.settings.ui_density.sidebar_w();

        v_flex()
            .w(px(sidebar_w))
            .flex_shrink_0()
            .h_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .p_3()
            .gap_0p5()
            .child(nav_item(
                "All downloads",
                FilterKind::All,
                all,
                filter == FilterKind::All,
                cx,
            ))
            .child(nav_item(
                "Active",
                FilterKind::Active,
                active,
                filter == FilterKind::Active,
                cx,
            ))
            .child(nav_item(
                "Completed",
                FilterKind::Completed,
                completed,
                filter == FilterKind::Completed,
                cx,
            ))
            .child(nav_item(
                "Failed",
                FilterKind::Failed,
                failed,
                filter == FilterKind::Failed,
                cx,
            ))
            .child(div().flex_1())
            .child(Divider::horizontal().my_2())
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child("APP"),
            )
            .child(nav_item(
                "Settings",
                FilterKind::Settings,
                -1,
                filter == FilterKind::Settings,
                cx,
            ))
            .child(
                h_flex()
                    .id("nav-about")
                    .h(px(36.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded(theme.radius)
                    .hover(|s| s.bg(theme.secondary.opacity(0.55)))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_about_dialog(window, cx);
                    }))
                    .child(
                        Icon::empty()
                            .path("icons/info.svg")
                            .with_size(px(15.))
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(theme.sidebar_foreground)
                            .child("About"),
                    ),
            )
    }

    fn render_status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (all, active, completed, failed) = count_jobs(&self.jobs);
        let speed = total_download_speed(&self.jobs);
        let completed_bytes = total_completed_bytes(&self.jobs);
        let downloading = self
            .jobs
            .iter()
            .filter(|j| matches!(j.state, JobState::Downloading | JobState::Starting))
            .count();
        let limit = self.settings.speed_limit_kib_per_second;

        h_flex()
            .h(px(30.))
            .flex_shrink_0()
            .px_4()
            .gap_3()
            .items_center()
            .overflow_x_hidden()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.secondary.opacity(0.4))
            .child(status_chip(format!("{all} total"), theme.muted_foreground))
            .child(status_chip(
                format!("{active} active"),
                if active > 0 {
                    theme.primary
                } else {
                    theme.muted_foreground
                },
            ))
            .child(status_chip(
                format!("{completed} done"),
                if completed > 0 {
                    theme.success
                } else {
                    theme.muted_foreground
                },
            ))
            .when(failed > 0, |el| {
                el.child(status_chip(format!("{failed} failed"), theme.danger))
            })
            .child(div().flex_1())
            .when(completed_bytes > 0, |el| {
                el.child(status_chip(
                    format!("{} saved", format_bytes(completed_bytes)),
                    theme.muted_foreground,
                ))
            })
            .when(limit > 0, |el| {
                el.child(status_chip(format!("Limit {} KiB/s", limit), theme.warning))
            })
            .child(status_chip(
                if downloading > 0 {
                    format!("↓ {}", format_speed(speed))
                } else {
                    "Idle".into()
                },
                if downloading > 0 {
                    theme.foreground
                } else {
                    theme.muted_foreground
                },
            ))
    }

    fn render_queue(
        &mut self,
        window: &mut Window,
        cx: &mut Context<DownloadApp>,
    ) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let filtered = self.visible_jobs(cx);
        let selected = self.selected_id.clone();
        let query = self.search_query(cx);
        let has_query = !query.trim().is_empty();
        if filtered.is_empty()
            && !has_query
            && filter_jobs(&self.jobs, self.filter.as_index()).is_empty()
        {
            return self.render_empty(cx).into_any_element();
        }

        let viewport = window.viewport_size();
        let sidebar_w = self.settings.ui_density.sidebar_w();
        let main_w = (viewport.width.to_f64() as f32 - sidebar_w).max(0.0);
        let cols = QueueColumns::from_main_width(main_w);
        let density = self.settings.ui_density;
        let progress_style = self.settings.progress_style;
        // Cap detail so the list always keeps a usable share of the viewport.
        let detail_max_h = {
            let vh = viewport.height.to_f64() as f32;
            (vh * 0.36).clamp(DETAIL_MIN_CAP, DETAIL_MAX_H)
        };
        let detail = self.selected_job().cloned();
        let detail_open = detail.is_some();
        let sort_col = self.settings.sort_column;
        let sort_dir = self.settings.sort_direction;

        v_flex()
            .size_full()
            .bg(theme.background)
            .child(self.render_queue_toolbar(cx))
            .child(
                h_flex()
                    .h(px(34.))
                    .px_4()
                    .gap_3()
                    .items_center()
                    .flex_shrink_0()
                    .bg(theme.list_head)
                    .border_b_1()
                    .border_color(theme.border)
                    // Match the status-dot + gap in each row so metrics stay aligned.
                    .child(div().w(px(STATUS_DOT)).flex_shrink_0())
                    .child(sortable_header(
                        "Name",
                        SortColumn::Name,
                        true,
                        None,
                        false,
                        sort_col,
                        sort_dir,
                        &theme,
                        cx,
                    ))
                    .when(cols.date, |el| {
                        el.child(sortable_header(
                            "Date",
                            SortColumn::Date,
                            false,
                            Some(px(COL_DATE_W)),
                            true,
                            sort_col,
                            sort_dir,
                            &theme,
                            cx,
                        ))
                    })
                    .when(cols.speed, |el| {
                        el.child(sortable_header(
                            "Speed",
                            SortColumn::Speed,
                            false,
                            Some(px(COL_SPEED_W)),
                            true,
                            sort_col,
                            sort_dir,
                            &theme,
                            cx,
                        ))
                    })
                    .when(cols.eta, |el| {
                        el.child(sortable_header(
                            "ETA",
                            SortColumn::Eta,
                            false,
                            Some(px(COL_ETA_W)),
                            true,
                            sort_col,
                            sort_dir,
                            &theme,
                            cx,
                        ))
                    })
                    .child(sortable_header(
                        "Size",
                        SortColumn::Size,
                        false,
                        Some(px(COL_SIZE_W)),
                        true,
                        sort_col,
                        sort_dir,
                        &theme,
                        cx,
                    ))
                    // Narrow overflow column — no header text (label would wrap).
                    .child(div().w(px(COL_ACTIONS_W)).flex_shrink_0()),
            )
            .child(
                div()
                    .id("queue-scroll")
                    .flex_1()
                    .min_h_0()
                    .when(detail_open, |el| el.min_h(px(LIST_MIN_H)))
                    .overflow_y_scroll()
                    .bg(theme.list)
                    .when(filtered.is_empty(), |el| {
                        el.child(self.render_search_empty(cx))
                    })
                    .children(filtered.into_iter().enumerate().map(|(index, job)| {
                        let is_selected = selected.as_deref() == Some(job.id.as_str());
                        render_job_row(
                            job,
                            is_selected,
                            index,
                            cols,
                            main_w,
                            density,
                            progress_style,
                            cx,
                        )
                    })),
            )
            .when_some(detail, |el, job| {
                el.child(render_detail(&job, detail_max_h, cx))
            })
            .into_any_element()
    }

    fn render_queue_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let view = cx.entity();

        h_flex()
            .px_4()
            .py_2p5()
            .gap_2()
            .items_center()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                div().flex_1().min_w(px(320.)).pr_1().child(
                    Input::new(&self.search_input).w_full().prefix(
                        Icon::new(IconName::Inbox)
                            .with_size(px(14.))
                            .text_color(theme.muted_foreground),
                    ),
                ),
            )
            .child(
                Button::new("queue-overflow")
                    .ghost()
                    .icon(IconName::EllipsisVertical)
                    .tooltip("More actions")
                    .dropdown_menu_with_anchor(Corner::TopRight, move |menu, _window, menu_cx| {
                        let app = view.read(menu_cx);
                        let can_pause = app.jobs.iter().any(|j| {
                            matches!(
                                j.state,
                                JobState::Queued | JobState::Starting | JobState::Downloading
                            )
                        });
                        let can_resume = app.jobs.iter().any(|j| j.state == JobState::Paused);
                        let can_retry = app
                            .jobs
                            .iter()
                            .any(|j| matches!(j.state, JobState::Failed | JobState::Canceled));
                        let can_clear = app.jobs.iter().any(|j| j.state.is_terminal());
                        let engine = app.engine.clone();

                        menu.min_w(px(196.))
                            .item(
                                PopupMenuItem::new("Pause all")
                                    .icon(IconName::Minus)
                                    .disabled(!can_pause)
                                    .on_click({
                                        let engine = engine.clone();
                                        move |_, _, _| {
                                            engine.send(EngineCommand::PauseAll);
                                        }
                                    }),
                            )
                            .item(
                                PopupMenuItem::new("Resume all")
                                    .icon(IconName::Redo2)
                                    .disabled(!can_resume)
                                    .on_click({
                                        let engine = engine.clone();
                                        move |_, _, _| {
                                            engine.send(EngineCommand::ResumeAll);
                                        }
                                    }),
                            )
                            .separator()
                            .item(
                                PopupMenuItem::new("Retry all")
                                    .icon(IconName::Redo)
                                    .disabled(!can_retry)
                                    .on_click({
                                        let engine = engine.clone();
                                        move |_, _, _| {
                                            engine.send(EngineCommand::RetryAll);
                                        }
                                    }),
                            )
                            .separator()
                            .item(
                                PopupMenuItem::new("Clear all")
                                    .icon(IconName::Delete)
                                    .disabled(!can_clear)
                                    .on_click({
                                        let view = view.clone();
                                        move |_, window, cx| {
                                            let _ = view.update(cx, |app, cx| {
                                                app.confirm_clear_all(window, cx);
                                            });
                                        }
                                    }),
                            )
                    }),
            )
    }

    fn render_search_empty(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let reduce_motion = self.settings.reduce_motion;
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p_8()
            .child(
                v_flex()
                    .gap_3()
                    .items_center()
                    .child(empty_state_badge(
                        IconName::Inbox,
                        theme.muted_foreground,
                        theme.secondary.opacity(0.45),
                        theme.border.opacity(0.35),
                        reduce_motion,
                    ))
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child("No matching downloads"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Try a different search or clear the filter."),
                    )
                    .child(
                        Button::new("clear-search")
                            .outline()
                            .small()
                            .label("Clear search")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.search_input.update(cx, |input, cx| {
                                    input.set_value("", window, cx);
                                });
                                cx.notify();
                            })),
                    ),
            )
    }

    fn render_empty(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let filter = self.filter;
        let show_cta = matches!(filter, FilterKind::All | FilterKind::Active);
        let reduce_motion = self.settings.reduce_motion;
        let accent = theme.primary;

        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .child(
                v_flex()
                    .w(px(420.))
                    .p_8()
                    .gap_3()
                    .items_center()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border.opacity(0.4))
                    .bg(theme.secondary.opacity(0.28))
                    // Soft accent wash behind the card content.
                    .child(
                        div().relative().mb_1().child(
                            // Outer decorative ring
                            div()
                                .w(px(88.))
                                .h(px(88.))
                                .rounded_full()
                                .border_1()
                                .border_color(accent.opacity(if reduce_motion {
                                    0.12
                                } else {
                                    0.22
                                }))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .w(px(64.))
                                        .h(px(64.))
                                        .rounded_full()
                                        .bg(accent.opacity(0.12))
                                        .border_1()
                                        .border_color(accent.opacity(0.2))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            Icon::new(filter.empty_icon())
                                                .with_size(px(28.))
                                                .text_color(accent.opacity(0.95)),
                                        ),
                                ),
                        ),
                    )
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .text_color(theme.foreground)
                            .child(filter.empty_title()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_center()
                            .text_color(theme.muted_foreground)
                            .max_w(px(300.))
                            .child(filter.empty_body()),
                    )
                    .when(show_cta, |el| {
                        el.child(
                            div().pt_1().child(
                                Button::new("empty-add")
                                    .primary()
                                    .icon(IconName::Plus)
                                    .label("Add download")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_add_dialog(window, cx);
                                    })),
                            ),
                        )
                    }),
            )
    }
}
