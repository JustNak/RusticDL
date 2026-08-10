mod detail;
mod filter;
mod job_row;
mod layout;
mod toast;
mod widgets;

pub use filter::FilterKind;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    canvas, div, point, prelude::FluentBuilder, px, size,
    App, AppContext, Bounds, Context, Corner, Corners, ElementId, Entity,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    NavigationDirection, ParentElement, Render, SharedString,
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
    slider::{Slider, SliderEvent, SliderState},
    v_flex, ActiveTheme, Icon, IconName, Root, Sizable, StyledExt, TitleBar, WindowExt,
};

use crate::appearance::{
    accent_swatch_color, apply_appearance, apply_window_opacity, custom_accent_hsla,
    film_grain_image, noise_enabled, resolve_theme_mode, vignette_edge_alpha, vignette_enabled,
};
use crate::branding::APP_NAME;
use crate::download::{
    reveal_in_folder, EngineCommand, EngineEvent, EngineHandle, Job, JobState,
};
use crate::format::{
    count_jobs, filter_jobs, format_bytes, format_speed,
    job_matches_search, sort_jobs, total_completed_bytes, total_download_speed,
};
use crate::ipc::IpcBridge;
use crate::persistence::{save_jobs, save_settings, AppPaths};
use crate::prompt_window::open_browser_prompt_window;
use crate::settings::{
    AccentPreset, AppTheme, CornerRadiusScale, ProgressStyle, Settings, SortColumn, SortDirection,
    UiDensity, WindowLayout, MAX_NOISE_INTENSITY, MAX_VIGNETTE_INTENSITY, MAX_WINDOW_TRANSPARENCY,
};

use detail::render_detail;
use job_row::render_job_row;
use layout::{
    COL_ACTIONS_W, COL_DATE_W, COL_ETA_W, COL_SIZE_W, COL_SPEED_W, DETAIL_MAX_H, DETAIL_MIN_CAP,
    LIST_MIN_H, QueueColumns, STATUS_DOT,
};
use toast::{Toast, ToastKind, TOAST_AUTO_HIDE, TOAST_MAX_STACK};
use widgets::{
    accent_custom_swatch, accent_hsl_slider_row, accent_preset_swatch, browse_directory,
    empty_state_badge, field_hint, field_label, nav_item, render_vignette_overlay,
    shorten_path_display, sortable_header, status_chip,
    styled_progress,
};

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
                .checked_sub(Duration::from_secs(2))
                .unwrap_or_else(Instant::now),
            browser_prompt_open_id: None,
            jobs_dirty: false,
            last_jobs_save: Instant::now()
                .checked_sub(Duration::from_secs(2))
                .unwrap_or_else(Instant::now),
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
    previous.iter().any(|job| !next.iter().any(|n| n.id == job.id))
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
        if self.ipc.take_show_window_request() {
            window.activate_window();
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
                                .child(APP_NAME),
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


