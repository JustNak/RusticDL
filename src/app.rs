use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    canvas, div, hsla, linear_color_stop, linear_gradient, point, prelude::FluentBuilder, px, size,
    App, AppContext, Bounds, Context, Corner, Corners, ElementId, Entity, FontWeight, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    NavigationDirection, ParentElement, PathPromptOptions, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, WindowBounds,
};
use gpui_component::{
    button::{Button, ButtonVariant, ButtonVariants},
    clipboard::Clipboard,
    description_list::DescriptionList,
    dialog::DialogButtonProps,
    divider::Divider,
    group_box::{GroupBox, GroupBoxVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    progress::Progress,
    slider::{Slider, SliderEvent, SliderState},
    tag::Tag,
    tooltip::Tooltip,
    v_flex, ActiveTheme, Icon, IconName, Root, Sizable, StyledExt, Theme, TitleBar, WindowExt,
};

use crate::appearance::{
    accent_swatch_color, apply_appearance, apply_window_opacity, custom_accent_hsla,
    film_grain_image, noise_enabled, resolve_theme_mode, vignette_edge_alpha, vignette_enabled,
};
use crate::download::{
    open_path, reveal_in_folder, EngineCommand, EngineEvent, EngineHandle, Job, JobState,
};
use crate::format::{
    count_jobs, filter_jobs, format_bytes, format_date, format_eta, format_size, format_speed,
    job_matches_search, sort_jobs, total_completed_bytes, total_download_speed,
};
use crate::ipc::IpcBridge;
use crate::persistence::{save_jobs, save_settings, AppPaths};
use crate::prompt_window::open_browser_prompt_window;
use crate::settings::{
    AccentPreset, AppTheme, CornerRadiusScale, ProgressStyle, Settings, SortColumn, SortDirection,
    UiDensity, WindowLayout, MAX_NOISE_INTENSITY, MAX_VIGNETTE_INTENSITY, MAX_WINDOW_TRANSPARENCY,
};

/// In-app toast (bottom-right). gpui-component's Notification layer is fixed top-right.
const TOAST_AUTO_HIDE: Duration = Duration::from_secs(5);
const TOAST_MAX_STACK: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToastKind {
    Info,
    Error,
}

#[derive(Debug, Clone)]
struct Toast {
    id: u64,
    message: SharedString,
    kind: ToastKind,
}

// --- Queue layout tokens (shared by header, rows, and width budgets) ---
// Keep metric columns tight so Name keeps the bulk of the row width.
/// Fits short-date forms like `08/10/2026` / locale variants (was 68 for relative-only).
const COL_DATE_W: f32 = 80.0;
const COL_SPEED_W: f32 = 76.0;
const COL_ETA_W: f32 = 56.0;
const COL_SIZE_W: f32 = 92.0;
/// Single overflow control — no multi-icon action strip.
const COL_ACTIONS_W: f32 = 40.0;
/// Status color dot beside the filename (tooltip shows the full label).
const STATUS_DOT: f32 = 9.0;
/// Keep at least this much list height when the detail panel is open.
const LIST_MIN_H: f32 = 140.0;
/// Hard cap for the selected-job detail panel (also clamped vs viewport).
const DETAIL_MAX_H: f32 = 280.0;
const DETAIL_MIN_CAP: f32 = 180.0;

/// Which fixed metric columns fit in the main content area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueueColumns {
    date: bool,
    speed: bool,
    eta: bool,
}

impl QueueColumns {
    /// Progressive collapse so Name stays dominant; metrics hide first when tight.
    fn from_main_width(main_w: f32) -> Self {
        // With compact metrics + overflow actions, full grid fits sooner.
        if main_w >= 780.0 {
            Self {
                date: true,
                speed: true,
                eta: true,
            }
        } else if main_w >= 680.0 {
            Self {
                date: true,
                speed: true,
                eta: false,
            }
        } else if main_w >= 600.0 {
            Self {
                date: false,
                speed: true,
                eta: false,
            }
        } else {
            Self {
                date: false,
                speed: false,
                eta: false,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    All,
    Active,
    Completed,
    Failed,
    Settings,
}

impl FilterKind {
    fn as_index(self) -> i32 {
        match self {
            Self::All => 0,
            Self::Active => 1,
            Self::Completed => 2,
            Self::Failed => 3,
            Self::Settings => 4,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::All => "All downloads",
            Self::Active => "Active",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Settings => "Settings",
        }
    }

    fn subtitle(self, count: usize) -> String {
        match self {
            Self::Settings => "Preferences and defaults".into(),
            Self::All if count == 0 => "Your download queue is empty".into(),
            Self::All => format!("{count} item{}", if count == 1 { "" } else { "s" }),
            Self::Active if count == 0 => "Nothing in progress".into(),
            Self::Active => format!("{count} active"),
            Self::Completed if count == 0 => "No finished downloads yet".into(),
            Self::Completed => format!("{count} completed"),
            Self::Failed if count == 0 => "No failures".into(),
            Self::Failed => format!("{count} failed or canceled"),
        }
    }

    fn empty_title(self) -> &'static str {
        match self {
            Self::All => "No downloads yet",
            Self::Active => "No active downloads",
            Self::Completed => "No completed downloads",
            Self::Failed => "No failed downloads",
            Self::Settings => "Settings",
        }
    }

    fn empty_body(self) -> &'static str {
        match self {
            Self::All => "Paste an HTTP or HTTPS link to start a transfer.",
            Self::Active => "Queued and in-progress jobs will show up here.",
            Self::Completed => "Finished files will appear in this list.",
            Self::Failed => "Failed or canceled jobs will appear here for retry.",
            Self::Settings => "",
        }
    }

    fn empty_icon(self) -> IconName {
        match self {
            Self::All => IconName::Inbox,
            Self::Active => IconName::LoaderCircle,
            Self::Completed => IconName::CircleCheck,
            Self::Failed => IconName::TriangleAlert,
            Self::Settings => IconName::Settings,
        }
    }

    fn nav_icon(self) -> IconName {
        match self {
            Self::All => IconName::Inbox,
            Self::Active => IconName::ArrowDown,
            Self::Completed => IconName::CircleCheck,
            Self::Failed => IconName::CircleX,
            Self::Settings => IconName::Settings,
        }
    }
}

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

        Self {
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
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
            browser_prompt_open_id: None,
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
        self.jobs = jobs;
        self.last_ui_update = Instant::now();
        let _ = save_jobs(&self.paths, &self.jobs);
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

    fn open_add_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                let est_h = match (is_advanced, is_multi) {
                    (true, true) => 560.0,
                    (true, false) => 480.0,
                    (false, true) => 400.0,
                    (false, false) => 280.0,
                };
                let view_h = window.viewport_size().height.to_f64() as f32;
                let max_top = (view_h - est_h - 20.0).max(24.0);
                let margin_top = ((view_h - est_h) * 0.5).clamp(24.0, max_top);

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
                        // Engine also splits glued schemes; do it here so filename applies to first only.
                        let urls = crate::download::extract_http_urls(&raw);
                        if urls.is_empty() {
                            app_view_ok.update(cx, |app, cx| {
                                app.show_error_toast(
                                    "Enter at least one valid HTTP(S) URL.",
                                    cx,
                                );
                            });
                            return false;
                        }

                        let filename = name_ok.read(cx).value().to_string();
                        let directory = PathBuf::from(dir_ok.read(cx).value().to_string());
                        let single_name = if urls.len() == 1 && !filename.trim().is_empty() {
                            Some(filename)
                        } else {
                            None
                        };

                        // One Add per URL keeps jobs independent; engine still re-splits defensively.
                        for (i, url) in urls.iter().enumerate() {
                            engine_ok.send(EngineCommand::Add {
                                url: url.clone(),
                                filename: if i == 0 {
                                    single_name.clone()
                                } else {
                                    None
                                },
                                directory: directory.clone(),
                                handoff_auth: None,
                                reply: None,
                            });
                        }
                        true
                    })
            }
        });
    }

    fn open_about_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.open_dialog(cx, |dialog, window, cx| {
            let theme = cx.theme().clone();
            let muted = theme.muted_foreground;

            // Match Add download: viewport-center so the card sits mid-window, not top-biased.
            let est_h = 280.0;
            let view_h = window.viewport_size().height.to_f64() as f32;
            let max_top = (view_h - est_h - 20.0).max(24.0);
            let margin_top = ((view_h - est_h) * 0.5).clamp(24.0, max_top);

            dialog
                .title(format!("About {}", crate::branding::APP_NAME))
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
                                .item("Version", env!("CARGO_PKG_VERSION"), 1)
                                .item("Engine", "Single-stream + Range resume", 1)
                                .item("License", "MIT", 1),
                        )
                        .child(div().text_xs().text_color(muted).child(format!(
                            "Data folder: %APPDATA%\\{}\\",
                            crate::branding::APP_DATA_DIR_NAME
                        ))),
                )
        });
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
                        engine.send(EngineCommand::Remove {
                            id: id.clone(),
                            delete_partial: true,
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

        apply_appearance(&self.settings, Some(window), cx);

        self.engine.send(EngineCommand::UpdateSettings {
            max_concurrent,
            auto_retry,
            speed_limit_kib: speed_limit,
        });

        self.show_toast("Settings saved.", cx);
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
                            div()
                                .text_sm()
                                .font_semibold()
                                .text_color(theme.foreground)
                                .child("RusticDL"),
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

    fn render_settings(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let theme_choice = self.settings.theme;
        let accent_preset = self.settings.accent_preset;
        let noise_pct = self.settings.noise_intensity;
        let transparency_pct = self.settings.window_transparency;
        let backdrop_blur = self.settings.backdrop_blur;
        let ui_density = self.settings.ui_density;
        let corner_radius = self.settings.corner_radius;
        let reduce_motion = self.settings.reduce_motion;
        let vignette_pct = self.settings.vignette_intensity;
        let progress_style = self.settings.progress_style;
        let accent_hue = self.settings.accent_hue;
        let accent_sat = self.settings.accent_saturation;
        let accent_light = self.settings.accent_lightness;
        let custom_color = custom_accent_hsla(accent_hue, accent_sat, accent_light);
        let data_dir = self.paths.root.display().to_string();
        let settings_pad = ui_density.settings_pad();
        let resolved_mode = resolve_theme_mode(theme_choice, None, cx);
        let mode_hint = match theme_choice {
            AppTheme::System => {
                if resolved_mode.is_dark() {
                    "Following system (currently dark)."
                } else {
                    "Following system (currently light)."
                }
            }
            AppTheme::Light => "Preview applies immediately; save to keep it.",
            AppTheme::Dark => "Preview applies immediately; save to keep it.",
        };

        div()
            .id("settings-scroll")
            .size_full()
            .overflow_y_scroll()
            .bg(theme.background)
            .p(px(settings_pad))
            .child(
                v_flex()
                    .gap_5()
                    .max_w(px(720.))
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_lg()
                                    .font_bold()
                                    .text_color(theme.foreground)
                                    .child("Settings"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Preferences and defaults"),
                            ),
                    )
                    .child(
                        GroupBox::new()
                            .outline()
                            .title(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        Icon::new(IconName::Folder)
                                            .with_size(px(14.))
                                            .text_color(theme.muted_foreground),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_semibold()
                                            .text_color(theme.foreground)
                                            .child("General"),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_4()
                                    .child(
                                        v_flex()
                                            .gap_1p5()
                                            .child(field_label("Download directory", cx))
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .child(
                                                        Input::new(&self.dir_input)
                                                            .w_full()
                                                            .flex_1(),
                                                    )
                                                    .child(
                                                        Button::new("browse-settings-dir")
                                                            .label("Browse...")
                                                            .icon(IconName::FolderOpen)
                                                            .outline()
                                                            .on_click(cx.listener(
                                                                |this, _, window, cx| {
                                                                    browse_directory(
                                                                        this.dir_input.clone(),
                                                                        cx.entity().clone(),
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            )),
                                                    ),
                                            )
                                            .child(field_hint(
                                                "Default folder for new downloads.",
                                                cx,
                                            )),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_4()
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .gap_1p5()
                                                    .child(field_label("Max concurrent", cx))
                                                    .child(
                                                        Input::new(&self.concurrent_input).w_full(),
                                                    )
                                                    .child(field_hint("Jobs running at once.", cx)),
                                            )
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .gap_1p5()
                                                    .child(field_label("Auto-retry attempts", cx))
                                                    .child(Input::new(&self.retry_input).w_full())
                                                    .child(field_hint(
                                                        "Retries after transient failures.",
                                                        cx,
                                                    )),
                                            )
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .gap_1p5()
                                                    .child(field_label("Speed limit (KiB/s)", cx))
                                                    .child(Input::new(&self.speed_input).w_full())
                                                    .child(field_hint("0 means unlimited.", cx)),
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        GroupBox::new()
                            .outline()
                            .title(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        Icon::new(IconName::Palette)
                                            .with_size(px(14.))
                                            .text_color(theme.muted_foreground),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_semibold()
                                            .text_color(theme.foreground)
                                            .child("Appearance"),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_4()
                                    // Theme
                                    .child(
                                        v_flex()
                                            .gap_2()
                                            .child(field_label("Theme", cx))
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .flex_wrap()
                                                    .child(
                                                        Button::new("theme-light")
                                                            .icon(IconName::Sun)
                                                            .label("Light")
                                                            .when(
                                                                theme_choice == AppTheme::Light,
                                                                |b| b.primary(),
                                                            )
                                                            .when(
                                                                theme_choice != AppTheme::Light,
                                                                |b| b.outline(),
                                                            )
                                                            .on_click(cx.listener(
                                                                |this, _, window, cx| {
                                                                    this.set_theme_draft(
                                                                        AppTheme::Light,
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            )),
                                                    )
                                                    .child(
                                                        Button::new("theme-dark")
                                                            .icon(IconName::Moon)
                                                            .label("Dark")
                                                            .when(
                                                                theme_choice == AppTheme::Dark,
                                                                |b| b.primary(),
                                                            )
                                                            .when(
                                                                theme_choice != AppTheme::Dark,
                                                                |b| b.outline(),
                                                            )
                                                            .on_click(cx.listener(
                                                                |this, _, window, cx| {
                                                                    this.set_theme_draft(
                                                                        AppTheme::Dark,
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            )),
                                                    )
                                                    .child(
                                                        Button::new("theme-system")
                                                            .icon(IconName::Settings)
                                                            .label("System")
                                                            .when(
                                                                theme_choice == AppTheme::System,
                                                                |b| b.primary(),
                                                            )
                                                            .when(
                                                                theme_choice != AppTheme::System,
                                                                |b| b.outline(),
                                                            )
                                                            .on_click(cx.listener(
                                                                |this, _, window, cx| {
                                                                    this.set_theme_draft(
                                                                        AppTheme::System,
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            )),
                                                    ),
                                            )
                                            .child(field_hint(mode_hint, cx)),
                                    )
                                    // Accent — preset dots + distinct Custom (rainbow ring)
                                    .child(
                                        v_flex()
                                            .gap_2()
                                            .child(
                                                h_flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .child(field_label("Color accent", cx))
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_medium()
                                                            .text_color(theme.muted_foreground)
                                                            .child(accent_preset.label()),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_1p5()
                                                    .flex_wrap()
                                                    .items_center()
                                                    .children(
                                                        AccentPreset::ALL
                                                            .into_iter()
                                                            .filter(|p| {
                                                                *p != AccentPreset::Custom
                                                            })
                                                            .map(|preset| {
                                                                accent_preset_swatch(
                                                                    preset,
                                                                    accent_preset == preset,
                                                                    accent_swatch_color(
                                                                        preset,
                                                                        accent_hue,
                                                                        accent_sat,
                                                                        accent_light,
                                                                        theme.primary,
                                                                    ),
                                                                    &theme,
                                                                    cx,
                                                                )
                                                            }),
                                                    )
                                                    // Divider: presets | custom mixer
                                                    .child(
                                                        div()
                                                            .mx_0p5()
                                                            .w(px(1.))
                                                            .h(px(18.))
                                                            .rounded_full()
                                                            .bg(theme.border.opacity(0.7)),
                                                    )
                                                    .child(accent_custom_swatch(
                                                        accent_preset == AccentPreset::Custom,
                                                        custom_color,
                                                        &theme,
                                                        cx,
                                                    )),
                                            )
                                            .when(accent_preset == AccentPreset::Custom, |this| {
                                                this.child(
                                                    v_flex()
                                                        .w_full()
                                                        .gap_2p5()
                                                        .p_3()
                                                        .rounded(theme.radius_lg)
                                                        .border_1()
                                                        .border_color(theme.border.opacity(0.45))
                                                        .bg(theme.secondary.opacity(0.28))
                                                        .child(
                                                            h_flex()
                                                                .w_full()
                                                                .items_center()
                                                                .gap_2()
                                                                .child(
                                                                    div()
                                                                        .size(px(28.))
                                                                        .rounded_full()
                                                                        .bg(custom_color)
                                                                        .border_2()
                                                                        .border_color(
                                                                            theme
                                                                                .foreground
                                                                                .opacity(0.22),
                                                                        )
                                                                        .flex_shrink_0(),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .text_xs()
                                                                        .font_semibold()
                                                                        .text_color(
                                                                            theme.muted_foreground,
                                                                        )
                                                                        .child("Mix custom accent"),
                                                                )
                                                                .child(div().flex_1())
                                                                .child(
                                                                    div()
                                                                        .text_xs()
                                                                        .font_medium()
                                                                        .text_color(
                                                                            theme.muted_foreground,
                                                                        )
                                                                        .child(format!(
                                                                            "H {:.0}  S {:.0}%  L {:.0}%",
                                                                            accent_hue,
                                                                            accent_sat,
                                                                            accent_light
                                                                        )),
                                                                ),
                                                        )
                                                        .child(accent_hsl_slider_row(
                                                            "Hue",
                                                            format!("{:.0}°", accent_hue),
                                                            Slider::new(&self.hue_slider)
                                                                .horizontal()
                                                                .w_full(),
                                                            &theme,
                                                        ))
                                                        .child(accent_hsl_slider_row(
                                                            "Saturation",
                                                            format!("{:.0}%", accent_sat),
                                                            Slider::new(&self.sat_slider)
                                                                .horizontal()
                                                                .w_full(),
                                                            &theme,
                                                        ))
                                                        .child(accent_hsl_slider_row(
                                                            "Lightness",
                                                            format!("{:.0}%", accent_light),
                                                            Slider::new(&self.light_slider)
                                                                .horizontal()
                                                                .w_full(),
                                                            &theme,
                                                        )),
                                                )
                                            })
                                            .child(field_hint(
                                                "Tints buttons, progress, selection, and links. Custom uses full HSL.",
                                                cx,
                                            )),
                                    )
                                    // Live preview strip
                                    .child(
                                        v_flex()
                                            .gap_2()
                                            .child(field_label("Preview", cx))
                                            .child(
                                                h_flex()
                                                    .gap_3()
                                                    .items_center()
                                                    .p_3()
                                                    .rounded(theme.radius_lg)
                                                    .border_1()
                                                    .border_color(theme.border.opacity(0.4))
                                                    .bg(theme.secondary.opacity(0.35))
                                                    .child(
                                                        Button::new("preview-primary")
                                                            .primary()
                                                            .label("Primary"),
                                                    )
                                                    .child(
                                                        Button::new("preview-outline")
                                                            .outline()
                                                            .label("Secondary"),
                                                    )
                                                    .child(
                                                        div().w(px(140.)).child(
                                                            styled_progress(
                                                                64.0,
                                                                theme.progress_bar,
                                                                progress_style,
                                                            ),
                                                        ),
                                                    )
                                                    .child(
                                                        div()
                                                            .px_2()
                                                            .py_1()
                                                            .rounded(theme.radius)
                                                            .bg(theme.list_active)
                                                            .border_1()
                                                            .border_color(theme.list_active_border)
                                                            .text_xs()
                                                            .text_color(theme.foreground)
                                                            .child("Selected row"),
                                                    ),
                                            ),
                                    )
                                    // Transparency (0 = solid default; max still floors alpha)
                                    .child(
                                        v_flex()
                                            .gap_1p5()
                                            .child(
                                                h_flex()
                                                    .justify_between()
                                                    .child(field_label("Transparency", cx))
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme.muted_foreground)
                                                            .child(format!(
                                                                "{transparency_pct}%"
                                                            )),
                                                    ),
                                            )
                                            .child(
                                                Slider::new(&self.opacity_slider)
                                                    .horizontal()
                                                    .w_full(),
                                            )
                                            .child(field_hint(
                                                "0% solid (default). Higher values glass the window.",
                                                cx,
                                            )),
                                    )
                                    // Backdrop blur (pairs with transparency)
                                    .child(
                                        v_flex()
                                            .gap_2()
                                            .child(field_label("Backdrop blur", cx))
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .child(
                                                        Button::new("blur-off")
                                                            .label("Off")
                                                            .when(!backdrop_blur, |b| b.primary())
                                                            .when(backdrop_blur, |b| b.outline())
                                                            .on_click(cx.listener(
                                                                |this, _, window, cx| {
                                                                    this.set_backdrop_blur(
                                                                        false, window, cx,
                                                                    );
                                                                },
                                                            )),
                                                    )
                                                    .child(
                                                        Button::new("blur-on")
                                                            .label("On")
                                                            .when(backdrop_blur, |b| b.primary())
                                                            .when(!backdrop_blur, |b| b.outline())
                                                            .on_click(cx.listener(
                                                                |this, _, window, cx| {
                                                                    this.set_backdrop_blur(
                                                                        true, window, cx,
                                                                    );
                                                                },
                                                            )),
                                                    ),
                                            )
                                            .child(field_hint(
                                                "Acrylic-style blur behind glass (when transparent).",
                                                cx,
                                            )),
                                    )
                                    // Noise
                                    .child(
                                        v_flex()
                                            .gap_1p5()
                                            .child(
                                                h_flex()
                                                    .justify_between()
                                                    .child(field_label("Noise (film grain)", cx))
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme.muted_foreground)
                                                            .child(format!("{noise_pct}%")),
                                                    ),
                                            )
                                            .child(
                                                Slider::new(&self.noise_slider)
                                                    .horizontal()
                                                    .w_full(),
                                            )
                                            .child(field_hint(
                                                "Dense film grit; strength scales with the slider. 0% off.",
                                                cx,
                                            )),
                                    )
                                    // Density
                                    .child(
                                        v_flex()
                                            .gap_2()
                                            .child(field_label("UI density", cx))
                                            .child(
                                                h_flex().gap_2().children(
                                                    UiDensity::ALL.into_iter().map(|d| {
                                                        let selected = ui_density == d;
                                                        Button::new(SharedString::from(format!(
                                                            "density-{}",
                                                            d.label()
                                                        )))
                                                        .label(d.label())
                                                        .when(selected, |b| b.primary())
                                                        .when(!selected, |b| b.outline())
                                                        .on_click(cx.listener(
                                                            move |this, _, window, cx| {
                                                                this.set_ui_density(
                                                                    d, window, cx,
                                                                );
                                                            },
                                                        ))
                                                    }),
                                                ),
                                            )
                                            .child(field_hint(
                                                "Compact tightens rows, sidebar, and settings padding.",
                                                cx,
                                            )),
                                    )
                                    // Corner radius
                                    .child(
                                        v_flex()
                                            .gap_2()
                                            .child(field_label("Corner radius", cx))
                                            .child(
                                                h_flex().gap_2().children(
                                                    CornerRadiusScale::ALL.into_iter().map(
                                                        |scale| {
                                                            let selected =
                                                                corner_radius == scale;
                                                            Button::new(SharedString::from(
                                                                format!(
                                                                    "radius-{}",
                                                                    scale.label()
                                                                ),
                                                            ))
                                                            .label(scale.label())
                                                            .when(selected, |b| b.primary())
                                                            .when(!selected, |b| b.outline())
                                                            .on_click(cx.listener(
                                                                move |this, _, window, cx| {
                                                                    this.set_corner_radius(
                                                                        scale, window, cx,
                                                                    );
                                                                },
                                                            ))
                                                        },
                                                    ),
                                                ),
                                            )
                                            .child(field_hint(
                                                "Sharp, default, or soft rounding on controls and cards.",
                                                cx,
                                            )),
                                    )
                                    // Reduce motion
                                    .child(
                                        v_flex()
                                            .gap_2()
                                            .child(field_label("Reduce motion", cx))
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .child(
                                                        Button::new("motion-off")
                                                            .label("Off")
                                                            .when(!reduce_motion, |b| b.primary())
                                                            .when(reduce_motion, |b| b.outline())
                                                            .on_click(cx.listener(
                                                                |this, _, window, cx| {
                                                                    this.set_reduce_motion(
                                                                        false, window, cx,
                                                                    );
                                                                },
                                                            )),
                                                    )
                                                    .child(
                                                        Button::new("motion-on")
                                                            .label("On")
                                                            .when(reduce_motion, |b| b.primary())
                                                            .when(!reduce_motion, |b| b.outline())
                                                            .on_click(cx.listener(
                                                                |this, _, window, cx| {
                                                                    this.set_reduce_motion(
                                                                        true, window, cx,
                                                                    );
                                                                },
                                                            )),
                                                    ),
                                            )
                                            .child(field_hint(
                                                "Prefer calmer empty states and less decorative motion.",
                                                cx,
                                            )),
                                    )
                                    // Vignette
                                    .child(
                                        v_flex()
                                            .gap_1p5()
                                            .child(
                                                h_flex()
                                                    .justify_between()
                                                    .child(field_label("Vignette", cx))
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme.muted_foreground)
                                                            .child(format!("{vignette_pct}%")),
                                                    ),
                                            )
                                            .child(
                                                Slider::new(&self.vignette_slider)
                                                    .horizontal()
                                                    .w_full(),
                                            )
                                            .child(field_hint(
                                                "Soft dark edges around the window. 0% off.",
                                                cx,
                                            )),
                                    )
                                    // Progress style
                                    .child(
                                        v_flex()
                                            .gap_2()
                                            .child(field_label("Progress style", cx))
                                            .child(
                                                h_flex().gap_2().flex_wrap().children(
                                                    ProgressStyle::ALL.into_iter().map(|style| {
                                                        let selected = progress_style == style;
                                                        Button::new(SharedString::from(format!(
                                                            "progress-{}",
                                                            style.label()
                                                        )))
                                                        .label(style.label())
                                                        .when(selected, |b| b.primary())
                                                        .when(!selected, |b| b.outline())
                                                        .on_click(cx.listener(
                                                            move |this, _, window, cx| {
                                                                this.set_progress_style(
                                                                    style, window, cx,
                                                                );
                                                            },
                                                        ))
                                                    }),
                                                ),
                                            )
                                            .child(field_hint(
                                                "How download progress bars look in the queue.",
                                                cx,
                                            )),
                                    )
                                    .child(
                                        h_flex().child(
                                            Button::new("reset-appearance")
                                                .outline()
                                                .label("Reset appearance")
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.reset_appearance_draft(window, cx);
                                                })),
                                        ),
                                    )
                                    .child(field_hint(
                                        "Preview applies immediately; save settings to persist.",
                                        cx,
                                    )),
                            ),
                    )
                    .child(
                        GroupBox::new()
                            .outline()
                            .title(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        Icon::new(IconName::Settings)
                                            .with_size(px(14.))
                                            .text_color(theme.muted_foreground),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_semibold()
                                            .text_color(theme.foreground)
                                            .child("Data"),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(field_label("App data directory", cx))
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .overflow_x_hidden()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(data_dir.clone()),
                                            )
                                            .child(
                                                Clipboard::new("copy-data-dir")
                                                    .value(SharedString::from(data_dir)),
                                            )
                                            .child(
                                                Button::new("open-data-dir")
                                                    .outline()
                                                    .small()
                                                    .icon(IconName::FolderOpen)
                                                    .label("Open")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        if let Err(msg) =
                                                            reveal_in_folder(&this.paths.root)
                                                        {
                                                            this.show_toast(msg, cx);
                                                        }
                                                    })),
                                            ),
                                    )
                                    .child(field_hint(
                                        "settings.json and state.json live here.",
                                        cx,
                                    )),
                            ),
                    )
                    .child(
                        Button::new("save-settings")
                            .primary()
                            .icon(IconName::Check)
                            .label("Save settings")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_settings(window, cx);
                            })),
                    ),
            )
    }
}

/// Soft edge vignette using four linear-gradient strips.
fn render_vignette_overlay(edge_alpha: f32, is_dark: bool) -> impl IntoElement {
    let a = edge_alpha.clamp(0.0, 0.5);
    let edge = if is_dark {
        hsla(0.0, 0.0, 0.0, a)
    } else {
        hsla(0.0, 0.0, 0.08, a * 0.85)
    };
    let clear = hsla(0.0, 0.0, 0.0, 0.0);
    let band = px(96.);

    div()
        .absolute()
        .inset_0()
        .size_full()
        // Top
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(band)
                .bg(linear_gradient(
                    180.0,
                    linear_color_stop(edge, 0.0),
                    linear_color_stop(clear, 1.0),
                )),
        )
        // Bottom
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(band)
                .bg(linear_gradient(
                    0.0,
                    linear_color_stop(edge, 0.0),
                    linear_color_stop(clear, 1.0),
                )),
        )
        // Left
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(band)
                .bg(linear_gradient(
                    90.0,
                    linear_color_stop(edge, 0.0),
                    linear_color_stop(clear, 1.0),
                )),
        )
        // Right
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(band)
                .bg(linear_gradient(
                    270.0,
                    linear_color_stop(edge, 0.0),
                    linear_color_stop(clear, 1.0),
                )),
        )
}

/// Progress bar variants for queue rows and settings preview.
/// `value` is 0..100.
fn styled_progress(value: f32, color: Hsla, style: ProgressStyle) -> impl IntoElement {
    let value = value.clamp(0.0, 100.0);
    match style {
        ProgressStyle::Solid => Progress::new()
            .value(value)
            .bg(color)
            .h(px(6.))
            .w_full()
            .rounded_full()
            .into_any_element(),
        ProgressStyle::Soft => Progress::new()
            .value(value)
            .bg(color.opacity(0.85))
            .h(px(4.))
            .w_full()
            .rounded_full()
            .into_any_element(),
        ProgressStyle::Glow => Progress::new()
            .value(value)
            .bg(color)
            .h(px(9.))
            .w_full()
            .rounded_full()
            .into_any_element(),
        ProgressStyle::Segmented => {
            const SEGMENTS: u32 = 12;
            let filled = ((value / 100.0) * SEGMENTS as f32).round() as u32;
            h_flex()
                .w_full()
                .gap_0p5()
                .h(px(8.))
                .items_center()
                .children((0..SEGMENTS).map(move |i| {
                    let on = i < filled;
                    div().flex_1().h_full().rounded(px(2.)).bg(if on {
                        color
                    } else {
                        color.opacity(0.16)
                    })
                }))
                .into_any_element()
        }
    }
}

/// Decorative icon badge for empty / search-empty states.
fn empty_state_badge(
    icon: IconName,
    icon_color: Hsla,
    fill: Hsla,
    ring: Hsla,
    reduce_motion: bool,
) -> impl IntoElement {
    let outer = if reduce_motion { 56.0 } else { 64.0 };
    let inner = if reduce_motion { 44.0 } else { 48.0 };
    div()
        .w(px(outer))
        .h(px(outer))
        .rounded_full()
        .border_1()
        .border_color(ring)
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(inner))
                .h(px(inner))
                .rounded_full()
                .bg(fill)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).with_size(px(22.)).text_color(icon_color)),
        )
}

/// Compact path for secondary UI hints (e.g. Advanced row preview).
fn shorten_path_display(path: &str) -> String {
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

/// Open the platform folder picker and write the chosen path into `input`.
///
/// Uses GPUI's native path prompt (with a proper parent HWND on Windows) instead
/// of `rfd`, which often fails silently or opens behind the app window.
fn browse_directory(
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

/// Field title — stronger than hints so forms scan as Label → control → help.
fn field_label(text: &'static str, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_xs()
        .font_semibold()
        .text_color(theme.foreground)
        .child(text)
}

/// Supporting description under a field. Kept smaller/softer than `field_label`.
fn field_hint(text: impl Into<SharedString>, cx: &mut App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_xs()
        .font_normal()
        .text_color(theme.muted_foreground.opacity(0.78))
        .child(text.into())
}

/// Equal-size circular preset swatch (solid fill + selection ring).
fn accent_preset_swatch(
    preset: AccentPreset,
    selected: bool,
    swatch: Hsla,
    theme: &Theme,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let label = preset.label();
    let tip: SharedString = if preset == AccentPreset::Default {
        "Default — stock theme color".into()
    } else {
        label.to_string().into()
    };
    // Light fills (stock dark primary is often near-white) need a stronger edge
    // so they don't dissolve into the selection ring or the panel.
    let light_fill = swatch.l > 0.72;
    let fill_border = if selected {
        if light_fill {
            theme.foreground.opacity(0.35)
        } else {
            theme.background.opacity(0.35)
        }
    } else if light_fill {
        theme.border.opacity(0.85)
    } else {
        theme.border.opacity(0.45)
    };
    div()
        .id(SharedString::from(format!("accent-{label}")))
        .size(px(32.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .border_2()
        .border_color(if selected {
            // Darker ring when the fill itself is light so selection stays obvious.
            if light_fill {
                theme.muted_foreground.opacity(0.95)
            } else {
                theme.foreground.opacity(0.92)
            }
        } else {
            theme.border.opacity(0.0)
        })
        .when(!selected, |el| {
            el.hover(|s| {
                s.border_color(theme.muted_foreground.opacity(0.55))
                    .bg(theme.secondary.opacity(0.4))
            })
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.set_accent_preset(preset, window, cx);
        }))
        .child(
            div()
                .size(px(20.))
                .rounded_full()
                .bg(swatch)
                .border_1()
                .border_color(fill_border),
        )
}

/// Custom mixer entry: white disc + paintbrush — clearly not a solid preset.
fn accent_custom_swatch(
    selected: bool,
    _custom_color: Hsla,
    theme: &Theme,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let tip: SharedString = "Custom — mix your own accent".into();
    // White plate always; brush in dark ink so it stays readable on light/dark UI.
    let plate = hsla(0.0, 0.0, 0.98, 1.0);
    let brush = hsla(0.0, 0.0, 0.22, 1.0);

    div()
        .id("accent-Custom")
        .size(px(32.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .border_2()
        .border_color(if selected {
            theme.foreground.opacity(0.92)
        } else {
            theme.border.opacity(0.0)
        })
        .when(!selected, |el| {
            el.hover(|s| {
                s.border_color(theme.muted_foreground.opacity(0.55))
                    .bg(theme.secondary.opacity(0.4))
            })
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(|this, _, window, cx| {
            this.set_accent_preset(AccentPreset::Custom, window, cx);
        }))
        .child(
            div()
                .size(px(20.))
                .rounded_full()
                .bg(plate)
                .border_1()
                .border_color(theme.border.opacity(0.5))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::empty()
                        .path("icons/paintbrush.svg")
                        .with_size(px(12.))
                        .text_color(brush),
                ),
        )
}

fn accent_hsl_slider_row(
    label: &'static str,
    value: String,
    slider: impl IntoElement,
    theme: &Theme,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .w_full()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child(label),
                )
                .child(
                    div()
                        .text_xs()
                        .font_medium()
                        .text_color(theme.muted_foreground.opacity(0.85))
                        .child(value),
                ),
        )
        .child(slider)
}

fn status_chip(text: String, color: Hsla) -> impl IntoElement {
    div().text_xs().font_medium().text_color(color).child(text)
}

/// Clickable queue column header with asc/desc indicator for the active sort.
/// `center` centers the label (and sort chevron) in fixed-width metric columns.
fn sortable_header(
    label: &'static str,
    column: SortColumn,
    flex: bool,
    width: Option<gpui::Pixels>,
    center: bool,
    active_column: SortColumn,
    direction: SortDirection,
    theme: &gpui_component::Theme,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let active = active_column == column;
    let color = if active {
        theme.foreground
    } else {
        theme.muted_foreground
    };
    let tip: SharedString = if active {
        match direction {
            SortDirection::Asc => {
                format!("Sorted by {label} · ascending (click to reverse)").into()
            }
            SortDirection::Desc => {
                format!("Sorted by {label} · descending (click to reverse)").into()
            }
        }
    } else {
        format!("Sort by {label}").into()
    };

    h_flex()
        .id(SharedString::from(format!("sort-header-{label}")))
        .when(flex, |d| d.flex_1().min_w_0())
        .when_some(width, |d, w| d.w(w).flex_shrink_0())
        .gap_0p5()
        .items_center()
        .when(center, |d| d.justify_center())
        .cursor_pointer()
        .rounded(theme.radius)
        .hover(|s| s.bg(theme.secondary.opacity(0.45)))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_sort_column(column, cx);
        }))
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(color)
                .child(label),
        )
        .when(active, |el| {
            el.child(
                Icon::new(match direction {
                    SortDirection::Asc => IconName::ChevronUp,
                    SortDirection::Desc => IconName::ChevronDown,
                })
                .with_size(px(12.))
                .text_color(theme.primary),
            )
        })
}

/// Fixed-width metric cell; content is centered under the column header.
fn metric_cell(
    width: f32,
    text: impl Into<SharedString>,
    color: Hsla,
    medium: bool,
) -> impl IntoElement {
    h_flex()
        .w(px(width))
        .flex_shrink_0()
        .justify_center()
        .items_center()
        .overflow_hidden()
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_xs()
                .when(medium, |d| d.font_medium())
                .text_color(color)
                .child(text.into()),
        )
}

fn nav_item(
    label: &'static str,
    filter: FilterKind,
    count: i32,
    active: bool,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let bg = if active {
        theme.sidebar_accent
    } else {
        theme.transparent
    };
    let fg = if active {
        theme.sidebar_accent_foreground
    } else {
        theme.sidebar_foreground
    };
    let icon_color = if active {
        theme.sidebar_primary
    } else {
        theme.muted_foreground
    };

    h_flex()
        .id(SharedString::from(format!("nav-{label}")))
        .h(px(36.))
        .px_2()
        .gap_2()
        .items_center()
        .rounded(theme.radius)
        .bg(bg)
        .hover(|s| {
            s.bg(if active {
                theme.sidebar_accent
            } else {
                theme.secondary.opacity(0.55)
            })
        })
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, window, cx| {
            this.select_filter(filter, window, cx);
        }))
        .child(
            Icon::new(filter.nav_icon())
                .with_size(px(15.))
                .text_color(icon_color),
        )
        .child(
            div()
                .flex_1()
                .text_sm()
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .text_color(fg)
                .child(label),
        )
        .when(count >= 0, |el| {
            let empty = count == 0;
            el.child(
                div()
                    .min_w(px(22.))
                    .px_1p5()
                    .py_0p5()
                    .rounded_full()
                    .bg(if active && !empty {
                        theme.sidebar_primary
                    } else if active {
                        theme.sidebar_primary.opacity(0.12)
                    } else {
                        theme.muted.opacity(0.55)
                    })
                    .text_xs()
                    .font_semibold()
                    .text_center()
                    .text_color(if active && !empty {
                        theme.sidebar_primary_foreground
                    } else if active {
                        theme.sidebar_primary
                    } else {
                        theme.muted_foreground
                    })
                    .child(count.to_string()),
            )
        })
}

fn status_color(tone: i32, theme: &gpui_component::Theme) -> Hsla {
    match tone {
        1 => theme.primary,
        2 => theme.success,
        3 => theme.warning,
        4 => theme.danger,
        _ => theme.muted_foreground,
    }
}

fn status_tag(status: &'static str, tone: i32) -> Tag {
    // Text badge kept for the detail panel only.
    match tone {
        1 => Tag::primary().small().child(status),
        2 => Tag::success().small().child(status),
        3 => Tag::warning().small().child(status),
        4 => Tag::danger().small().child(status),
        _ => Tag::secondary().small().child(status),
    }
}

/// Compact circular status indicator. Hover shows the full status label.
fn status_dot(
    job_id: &str,
    status: &'static str,
    color: Hsla,
    tip_color: Hsla,
) -> impl IntoElement {
    let label: SharedString = status.into();
    div()
        .id(SharedString::from(format!("status-dot-{job_id}")))
        .flex_shrink_0()
        .w(px(STATUS_DOT))
        .h(px(STATUS_DOT))
        .rounded_full()
        .bg(color)
        .border_1()
        .border_color(color.opacity(0.45))
        .tooltip(move |window, cx| soft_tooltip(label.clone(), tip_color, window, cx))
}

/// Smaller, muted tooltip used for status dots and full filenames.
fn soft_tooltip(
    text: SharedString,
    tip_color: Hsla,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyView {
    Tooltip::new(text)
        .text_xs()
        .font_normal()
        .text_color(tip_color)
        .py_0()
        .px_1p5()
        .build(window, cx)
}

/// Approximate how many characters fit in the Name column (text-sm / semibold).
fn name_char_budget(main_w: f32, cols: QueueColumns) -> usize {
    // Row chrome always present: padding + status dot + size + actions + gaps.
    let mut used = 32.0 + STATUS_DOT + COL_SIZE_W + COL_ACTIONS_W + 12.0 * 5.0;
    if cols.date {
        used += COL_DATE_W + 12.0;
    }
    if cols.speed {
        used += COL_SPEED_W + 12.0;
    }
    if cols.eta {
        used += COL_ETA_W + 12.0;
    }
    let name_px = (main_w - used).max(96.0);
    // ~8px average advance for semibold text-sm on Windows.
    ((name_px / 8.0) as usize).clamp(16, 200)
}

/// Force a visible "..." when the label is longer than the name column can show.
/// (GPUI's text-overflow ellipsis is unreliable for this flex layout.)
fn ellipsize_name(name: &str, max_chars: usize) -> SharedString {
    let count = name.chars().count();
    if count <= max_chars {
        return SharedString::from(name.to_string());
    }
    let keep = max_chars.saturating_sub(3).max(1);
    let head: String = name.chars().take(keep).collect();
    SharedString::from(format!("{head}..."))
}

fn render_job_row(
    job: Job,
    selected: bool,
    index: usize,
    cols: QueueColumns,
    main_w: f32,
    density: UiDensity,
    progress_style: ProgressStyle,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let view = cx.entity();
    let id = job.id.clone();
    let id_for_select = job.id.clone();
    let id_actions = job.id.clone();
    let filename_for_remove = job.filename.clone();

    let show_progress = matches!(
        job.state,
        JobState::Starting | JobState::Downloading | JobState::Paused
    );
    // Action availability is resolved when the row overflow menu opens.

    let speed = if matches!(job.state, JobState::Downloading | JobState::Starting) {
        format_speed(job.speed)
    } else {
        "—".into()
    };
    let eta = if job.state == JobState::Downloading {
        format_eta(job.eta_secs)
    } else {
        "—".into()
    };
    let size = format_size(&job);
    let date = format_date(job.created_at);
    let status = job.state.label();
    let progress = job.progress as f32;
    let filename_tip: SharedString = job.filename.clone().into();
    let filename_label = ellipsize_name(&job.filename, name_char_budget(main_w, cols));
    let tone = job.state.tone();
    let accent = status_color(tone, &theme);
    let progress_color = if job.state == JobState::Paused {
        theme.warning
    } else {
        theme.progress_bar
    };
    let row_h = if show_progress {
        px(density.row_h_progress())
    } else {
        px(density.row_h())
    };

    let row_bg = if selected {
        theme.list_active
    } else if index % 2 == 1 {
        theme.list_even
    } else {
        theme.list
    };

    // Fixed-height table row: never grows with wrapped text or flex stretch.
    // Horizontal padding matches the header so metric columns share the same grid.
    h_flex()
        .id(SharedString::from(format!("job-row-{}", id)))
        .h(row_h)
        .max_h(row_h)
        .flex_shrink_0()
        .px_4()
        .gap_3()
        .items_center()
        .border_b_1()
        .border_color(theme.border.opacity(0.7))
        .bg(row_bg)
        .hover(|s| {
            s.bg(if selected {
                theme.list_active
            } else {
                theme.list_hover
            })
        })
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            if this.selected_id.as_deref() == Some(id_for_select.as_str()) {
                this.selected_id = None;
            } else {
                this.selected_id = Some(id_for_select.clone());
            }
            cx.notify();
        }))
        // Status as a color dot (tooltip = full label), then the filename.
        .child(status_dot(&id, status, accent, theme.muted_foreground))
        .child(
            // Name takes remaining width; metrics stay fixed and compact.
            v_flex()
                .flex_1()
                .gap_1p5()
                .min_w_0()
                .justify_center()
                .child(h_flex().w_full().min_w_0().items_center().child({
                    // Explicit "..." when too long; hover shows the full name.
                    let tip_color = theme.muted_foreground;
                    div()
                        .id(SharedString::from(format!("job-name-{id}")))
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .tooltip(move |window, cx| {
                            soft_tooltip(filename_tip.clone(), tip_color, window, cx)
                        })
                        .child(filename_label)
                }))
                .when(show_progress, |el| {
                    el.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .w_full()
                            .min_w_0()
                            .child(div().w_full().flex_1().min_w_0().child(styled_progress(
                                progress,
                                progress_color,
                                progress_style,
                            )))
                            .child(
                                div()
                                    .w(px(40.))
                                    .flex_shrink_0()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{:.0}%", progress)),
                            ),
                    )
                }),
        )
        .when(cols.date, |el| {
            el.child(metric_cell(COL_DATE_W, date, theme.muted_foreground, false))
        })
        .when(cols.speed, |el| {
            el.child(metric_cell(
                COL_SPEED_W,
                speed,
                if matches!(job.state, JobState::Downloading | JobState::Starting) {
                    theme.foreground
                } else {
                    theme.muted_foreground
                },
                true,
            ))
        })
        .when(cols.eta, |el| {
            el.child(metric_cell(COL_ETA_W, eta, theme.muted_foreground, false))
        })
        .child(metric_cell(COL_SIZE_W, size, theme.foreground, true))
        .child(
            h_flex()
                .w(px(COL_ACTIONS_W))
                .flex_shrink_0()
                .justify_end()
                .items_center()
                .child(
                    Button::new(SharedString::from(format!("row-overflow-{id_actions}")))
                        .ghost()
                        .small()
                        .icon(IconName::EllipsisVertical)
                        .tooltip("Actions")
                        .dropdown_menu_with_anchor(Corner::TopRight, {
                            let view = view.clone();
                            let id = id_actions.clone();
                            let filename = filename_for_remove.clone();
                            move |menu, _window, menu_cx| {
                                let app = view.read(menu_cx);
                                let engine = app.engine.clone();
                                let job = app.jobs.iter().find(|j| j.id == id);
                                let can_pause = job.is_some_and(|j| {
                                    matches!(
                                        j.state,
                                        JobState::Queued
                                            | JobState::Starting
                                            | JobState::Downloading
                                    )
                                });
                                let can_resume = job.is_some_and(|j| j.state == JobState::Paused);
                                let can_retry = job.is_some_and(|j| {
                                    matches!(j.state, JobState::Failed | JobState::Canceled)
                                });
                                let can_open = job.is_some_and(|j| {
                                    j.state == JobState::Completed && j.target_path.exists()
                                });
                                let can_remove = job.is_some_and(|j| {
                                    j.state.is_terminal() || j.state == JobState::Paused
                                });

                                let mut menu = menu.min_w(px(180.));

                                if can_pause {
                                    menu = menu.item(
                                        PopupMenuItem::new("Pause").icon(IconName::Minus).on_click(
                                            {
                                                let engine = engine.clone();
                                                let id = id.clone();
                                                move |_, _, _| {
                                                    engine.send(EngineCommand::Pause(id.clone()));
                                                }
                                            },
                                        ),
                                    );
                                }
                                if can_resume {
                                    menu = menu.item(
                                        PopupMenuItem::new("Resume")
                                            .icon(IconName::Redo2)
                                            .on_click({
                                                let engine = engine.clone();
                                                let id = id.clone();
                                                move |_, _, _| {
                                                    engine.send(EngineCommand::Resume(id.clone()));
                                                }
                                            }),
                                    );
                                }
                                if can_retry {
                                    menu = menu.item(
                                        PopupMenuItem::new("Retry").icon(IconName::Redo).on_click(
                                            {
                                                let engine = engine.clone();
                                                let id = id.clone();
                                                move |_, _, _| {
                                                    engine.send(EngineCommand::Retry(id.clone()));
                                                }
                                            },
                                        ),
                                    );
                                }
                                if can_pause || can_resume || can_retry {
                                    menu = menu.separator();
                                }

                                if can_open {
                                    menu = menu.item(
                                        PopupMenuItem::new("Open file")
                                            .icon(IconName::ExternalLink)
                                            .on_click({
                                                let view = view.clone();
                                                let id = id.clone();
                                                move |_, _window, cx| {
                                                    let _ = view.update(cx, |app, cx| {
                                                        if let Some(job) =
                                                            app.jobs.iter().find(|j| j.id == id)
                                                        {
                                                            if let Err(msg) =
                                                                open_path(&job.target_path)
                                                            {
                                                                app.show_toast(msg, cx);
                                                            }
                                                        }
                                                    });
                                                }
                                            }),
                                    );
                                }

                                menu = menu.item(
                                    PopupMenuItem::new("Show in folder")
                                        .icon(IconName::FolderOpen)
                                        .on_click({
                                            let view = view.clone();
                                            let id = id.clone();
                                            move |_, _window, cx| {
                                                let _ = view.update(cx, |app, cx| {
                                                    if let Some(job) =
                                                        app.jobs.iter().find(|j| j.id == id)
                                                    {
                                                        let path = if job.target_path.exists() {
                                                            job.target_path.clone()
                                                        } else {
                                                            job.temp_path.clone()
                                                        };
                                                        if let Err(msg) = reveal_in_folder(&path) {
                                                            app.show_toast(msg, cx);
                                                        }
                                                    }
                                                });
                                            }
                                        }),
                                );

                                menu.separator().item(
                                    PopupMenuItem::new(if can_remove {
                                        "Remove"
                                    } else {
                                        "Cancel"
                                    })
                                    .icon(if can_remove {
                                        IconName::Delete
                                    } else {
                                        IconName::Close
                                    })
                                    .on_click({
                                        let view = view.clone();
                                        let engine = engine.clone();
                                        let id = id.clone();
                                        let filename = filename.clone();
                                        move |_, window, cx| {
                                            if can_remove {
                                                let _ = view.update(cx, |app, cx| {
                                                    app.confirm_remove(
                                                        id.clone(),
                                                        filename.clone(),
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            } else {
                                                engine.send(EngineCommand::Cancel(id.clone()));
                                            }
                                        }
                                    }),
                                )
                            }
                        }),
                ),
        )
}

/// Circular arrow — reads as “start over”, unlike redo’s curved arrow.
fn restart_icon() -> Icon {
    Icon::empty().path("icons/rotate-cw.svg")
}

/// Inline “Label value” pair used in the detail meta row (no card chrome).
fn detail_pair(
    label: &'static str,
    value: impl Into<SharedString>,
    theme: &Theme,
) -> impl IntoElement {
    let value = value.into();
    let is_placeholder = value.as_ref() == "—" || value.as_ref().is_empty();
    let value_color = if is_placeholder {
        theme.muted_foreground.opacity(0.7)
    } else {
        theme.foreground
    };
    h_flex()
        .gap_2()
        .items_baseline()
        .flex_shrink_0()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .text_xs()
                .font_medium()
                .text_color(value_color)
                .whitespace_nowrap()
                .child(value),
        )
}

/// Thin vertical rule between meta pairs — same language as the status bar separators.
fn detail_meta_sep(theme: &Theme) -> impl IntoElement {
    div()
        .w(px(1.))
        .h(px(14.))
        .flex_shrink_0()
        .mx_0p5()
        .bg(theme.border.opacity(0.85))
}

fn render_detail(job: &Job, max_h: f32, cx: &mut Context<DownloadApp>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let tone = job.state.tone();
    let accent = status_color(tone, &theme);
    let size = format_size(job);
    let speed = if matches!(job.state, JobState::Downloading | JobState::Starting) {
        format_speed(job.speed)
    } else {
        "—".into()
    };
    let eta = if job.state == JobState::Downloading {
        format_eta(job.eta_secs)
    } else {
        "—".into()
    };
    let resume = if job.resume_supported {
        "Supported"
    } else {
        "Unavailable"
    };
    let progress = format!("{:.1}%", job.progress);
    let retries = job.retry_attempts.to_string();
    let path = job.target_path.to_string_lossy().to_string();
    let path_tip: SharedString = path.clone().into();
    let tip_color = theme.muted_foreground;
    let url = job.url.clone();
    let error = job.error.clone();
    let id = job.id.clone();
    let filename = job.filename.clone();
    let filename_tip: SharedString = job.filename.clone().into();

    let can_pause = matches!(
        job.state,
        JobState::Queued | JobState::Starting | JobState::Downloading
    );
    let can_resume = job.state == JobState::Paused;
    let can_retry = matches!(job.state, JobState::Failed | JobState::Canceled);
    // Restart wipes partial progress and starts from zero — only useful after a
    // failed or canceled transfer, not on completed jobs.
    let can_restart = matches!(job.state, JobState::Failed | JobState::Canceled);
    let can_open = job.state == JobState::Completed && job.target_path.exists();
    let can_remove = job.state.is_terminal() || job.state == JobState::Paused;
    let can_cancel = !job.state.is_terminal() && job.state != JobState::Paused;

    // Height-capped inspector: scrolls internally so the job list keeps space.
    // Flat surfaces only — hierarchy comes from type and a single top border, not nested cards.
    v_flex()
        .id("job-detail")
        .flex_shrink_0()
        .max_h(px(max_h))
        .min_h_0()
        .border_t_1()
        .border_color(theme.border)
        .bg(theme.secondary.opacity(0.28))
        .child(
            div()
                .id("job-detail-scroll")
                .max_h(px(max_h))
                .min_h_0()
                .overflow_y_scroll()
                .px_5()
                .pt_3()
                .pb_3()
                .child(
                    v_flex()
                        .gap_3()
                        // ── Header ──
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_2p5()
                                .items_center()
                                .child(
                                    Icon::new(match job.state {
                                        JobState::Completed => IconName::CircleCheck,
                                        JobState::Failed | JobState::Canceled => {
                                            IconName::TriangleAlert
                                        }
                                        JobState::Paused => IconName::Minus,
                                        _ => IconName::File,
                                    })
                                    .with_size(px(16.))
                                    .text_color(accent)
                                    .flex_shrink_0(),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .gap_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .child(
                                            // Soft character clamp — GPUI text-overflow is unreliable
                                            // in nested flex, same approach as the queue Name column.
                                            div()
                                                .id(SharedString::from(format!(
                                                    "detail-name-{}",
                                                    job.id
                                                )))
                                                .min_w_0()
                                                .text_sm()
                                                .font_semibold()
                                                .text_color(theme.foreground)
                                                .child(ellipsize_name(&job.filename, 72))
                                                .tooltip(move |window, cx| {
                                                    soft_tooltip(
                                                        filename_tip.clone(),
                                                        tip_color,
                                                        window,
                                                        cx,
                                                    )
                                                }),
                                        )
                                        .child(
                                            h_flex()
                                                .gap_1p5()
                                                .items_center()
                                                .min_w_0()
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .overflow_hidden()
                                                        .whitespace_nowrap()
                                                        .text_ellipsis()
                                                        .text_xs()
                                                        .text_color(theme.muted_foreground)
                                                        .child(url.clone()),
                                                )
                                                .child(
                                                    Clipboard::new(SharedString::from(format!(
                                                        "copy-url-{}",
                                                        job.id
                                                    )))
                                                    .value(SharedString::from(url.clone())),
                                                ),
                                        ),
                                )
                                .child(status_tag(job.state.label(), tone))
                                .child(
                                    Button::new("detail-close")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Close)
                                        .tooltip("Hide details")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.selected_id = None;
                                            cx.notify();
                                        })),
                                ),
                        )
                        // ── Meta row: inline label/value pairs ──
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_3()
                                .items_center()
                                .flex_wrap()
                                .child(detail_pair("Size", size, &theme))
                                .child(detail_meta_sep(&theme))
                                .child(detail_pair("Speed", speed, &theme))
                                .child(detail_meta_sep(&theme))
                                .child(detail_pair("ETA", eta, &theme))
                                .child(detail_meta_sep(&theme))
                                .child(detail_pair("Progress", progress, &theme))
                                .child(detail_meta_sep(&theme))
                                .child(detail_pair("Resume", resume, &theme))
                                .child(detail_meta_sep(&theme))
                                .child(detail_pair("Retries", retries, &theme)),
                        )
                        // ── Path ──
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_3()
                                .items_center()
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("Path"),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("detail-path-{}", job.id)))
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_xs()
                                        .text_color(theme.foreground)
                                        .child(path.clone())
                                        .tooltip(move |window, cx| {
                                            soft_tooltip(path_tip.clone(), tip_color, window, cx)
                                        }),
                                )
                                .child(
                                    Clipboard::new(SharedString::from(format!(
                                        "detail-copy-path-{}",
                                        id
                                    )))
                                    .value(SharedString::from(path.clone())),
                                ),
                        )
                        .when_some(error, |el, err| {
                            // Error keeps a light tint — semantic, not decorative card chrome.
                            el.child(
                                h_flex()
                                    .gap_2()
                                    .items_start()
                                    .child(
                                        Icon::new(IconName::TriangleAlert)
                                            .with_size(px(14.))
                                            .text_color(theme.danger),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_xs()
                                            .text_color(theme.danger)
                                            .child(err),
                                    ),
                            )
                        })
                        // ── Actions ──
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .flex_wrap()
                                .pt_1()
                                .when(can_pause, |el| {
                                    let id = id.clone();
                                    el.child(
                                        Button::new("detail-pause")
                                            .outline()
                                            .small()
                                            .icon(IconName::Minus)
                                            .label("Pause")
                                            .on_click(cx.listener(move |this, _, _, _| {
                                                this.engine.send(EngineCommand::Pause(id.clone()));
                                            })),
                                    )
                                })
                                .when(can_resume, |el| {
                                    let id = id.clone();
                                    el.child(
                                        Button::new("detail-resume")
                                            .outline()
                                            .small()
                                            .icon(IconName::Redo2)
                                            .label("Resume")
                                            .on_click(cx.listener(move |this, _, _, _| {
                                                this.engine.send(EngineCommand::Resume(id.clone()));
                                            })),
                                    )
                                })
                                .when(can_retry, |el| {
                                    let id = id.clone();
                                    el.child(
                                        Button::new("detail-retry")
                                            .outline()
                                            .small()
                                            .icon(IconName::Redo)
                                            .label("Retry")
                                            .on_click(cx.listener(move |this, _, _, _| {
                                                this.engine.send(EngineCommand::Retry(id.clone()));
                                            })),
                                    )
                                })
                                .when(can_restart, |el| {
                                    let id = id.clone();
                                    el.child(
                                        Button::new("detail-restart")
                                            .outline()
                                            .small()
                                            .icon(restart_icon())
                                            .label("Restart")
                                            .tooltip("Discard progress and download from the start")
                                            .on_click(cx.listener(move |this, _, _, _| {
                                                this.engine
                                                    .send(EngineCommand::Restart(id.clone()));
                                            })),
                                    )
                                })
                                .when(can_cancel, |el| {
                                    let id = id.clone();
                                    el.child(
                                        Button::new("detail-cancel")
                                            .outline()
                                            .small()
                                            .danger()
                                            .icon(IconName::Close)
                                            .label("Cancel")
                                            .on_click(cx.listener(move |this, _, _, _| {
                                                this.engine.send(EngineCommand::Cancel(id.clone()));
                                            })),
                                    )
                                })
                                .when(can_open, |el| {
                                    let id = id.clone();
                                    el.child(
                                        Button::new("detail-open")
                                            .outline()
                                            .small()
                                            .icon(IconName::ExternalLink)
                                            .label("Open")
                                            .on_click(cx.listener(move |this, _, _window, cx| {
                                                if let Some(job) =
                                                    this.jobs.iter().find(|j| j.id == id)
                                                {
                                                    if let Err(msg) = open_path(&job.target_path) {
                                                        this.show_toast(msg, cx);
                                                    }
                                                }
                                            })),
                                    )
                                })
                                .child({
                                    let id = id.clone();
                                    Button::new("detail-reveal")
                                        .outline()
                                        .small()
                                        .icon(IconName::FolderOpen)
                                        .label("Open")
                                        .tooltip("Open containing folder")
                                        .on_click(cx.listener(move |this, _, _window, cx| {
                                            if let Some(job) = this.jobs.iter().find(|j| j.id == id)
                                            {
                                                let path = if job.target_path.exists() {
                                                    job.target_path.clone()
                                                } else {
                                                    job.temp_path.clone()
                                                };
                                                if let Err(msg) = reveal_in_folder(&path) {
                                                    this.show_toast(msg, cx);
                                                }
                                            }
                                        }))
                                })
                                .when(can_remove, |el| {
                                    let id = id.clone();
                                    let filename = filename.clone();
                                    el.child(
                                        Button::new("detail-remove")
                                            .danger()
                                            .small()
                                            .icon(IconName::Delete)
                                            .label("Remove")
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.confirm_remove(
                                                    id.clone(),
                                                    filename.clone(),
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    )
                                }),
                        ),
                ),
        )
}
