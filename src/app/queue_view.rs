//! Queue list, toolbar, and empty states extracted from `DownloadApp`.
//!
//! OS file drops (`ExternalPaths` / CF_HDROP) land on the queue list and empty
//! state — freeform browser text/URL drag is not supported by GPUI on Windows.

use std::collections::BTreeSet;
use std::path::PathBuf;

use gpui::{
    div, prelude::FluentBuilder, px, Context, Corner, ExternalPaths, InteractiveElement,
    IntoElement, ParentElement, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    menu::{DropdownMenu, PopupMenuItem},
    v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt,
};

use super::add_dialog::enqueue_urls;
use super::detail::render_detail;
use super::filter::FilterKind;
use super::job_row::render_job_row;
use super::layout::{
    QueueColumns, COL_ACTIONS_W, COL_DATE_W, COL_ETA_W, COL_SIZE_W, COL_SPEED_W, DETAIL_MAX_H,
    DETAIL_MIN_CAP, LIST_MIN_H, STATUS_DOT,
};
use super::widgets::{empty_state_badge, sortable_header};
use super::DownloadApp;
use crate::download::{
    extract_urls_from_dropped_paths, reveal_in_folder, EngineCommand, Job, JobState,
};
use crate::format::filter_jobs;
use crate::settings::SortColumn;

/// Cap how many distinct folders multi-reveal opens before toasting the rest.
const BATCH_REVEAL_DIR_CAP: usize = 5;

impl DownloadApp {
    /// Handle OS file-path drops on the queue surface.
    ///
    /// Reads text-like / small files (cap 1 MiB), parses `.url` shortcuts, extracts
    /// HTTP(S) URLs, and enqueues via the same Add path as the add dialog.
    pub(crate) fn handle_external_paths_drop(
        &mut self,
        paths: &ExternalPaths,
        cx: &mut Context<Self>,
    ) {
        let summary = extract_urls_from_dropped_paths(paths.paths());
        let directory = self.settings.download_directory.clone();
        let n = enqueue_urls(summary.urls, directory, None, &self.engine);

        if n > 0 {
            let mut msg = format!("Added {n} from drop");
            if summary.skipped > 0 || summary.errors > 0 {
                let mut notes = Vec::new();
                if summary.skipped > 0 {
                    notes.push(format!(
                        "{} skipped (binary/oversized)",
                        summary.skipped
                    ));
                }
                if summary.errors > 0 {
                    notes.push(format!("{} unreadable", summary.errors));
                }
                msg.push_str(&format!(" ({})", notes.join(", ")));
            }
            self.show_toast(msg, cx);
        } else if summary.skipped > 0 || summary.errors > 0 {
            // Dropped only binaries / huge / unreadable files.
            let mut parts = Vec::new();
            if summary.skipped > 0 {
                parts.push(format!(
                    "Skipped {} binary or oversized file(s)",
                    summary.skipped
                ));
            }
            if summary.errors > 0 {
                parts.push(format!("Could not read {} file(s)", summary.errors));
            }
            self.show_error_toast(parts.join(". "), cx);
        } else {
            self.show_toast("No HTTP(S) URLs found", cx);
        }
    }

    pub(crate) fn render_queue(
        &mut self,
        window: &mut Window,
        cx: &mut Context<DownloadApp>,
    ) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let filtered = self.visible_jobs(cx);
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
        let multi_selected = self.selected_ids.len() > 1;
        let detail = if multi_selected {
            None
        } else {
            self.selected_job().cloned()
        };
        let bottom_open = multi_selected || detail.is_some();
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
                // File-path drops (CF_HDROP → ExternalPaths). Not freeform browser URL drag.
                div()
                    .id("queue-scroll")
                    .flex_1()
                    .min_h_0()
                    .when(bottom_open, |el| el.min_h(px(LIST_MIN_H)))
                    .overflow_y_scroll()
                    .bg(theme.list)
                    .can_drop(|drag, _, _| drag.is::<ExternalPaths>())
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                        this.handle_external_paths_drop(paths, cx);
                    }))
                    // Empty chrome / non-row background clears selection.
                    .on_click(cx.listener(|this, _, _, cx| {
                        if !this.selected_ids.is_empty() {
                            this.clear_selection();
                            cx.notify();
                        }
                    }))
                    .when(filtered.is_empty(), |el| {
                        el.child(self.render_search_empty(cx))
                    })
                    .children(filtered.into_iter().enumerate().map(|(index, job)| {
                        let is_selected = self.is_selected(job.id.as_str());
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
            .when(multi_selected, |el| el.child(self.render_batch_bar(cx)))
            .when_some(detail, |el, job| {
                el.child(render_detail(&job, detail_max_h, cx))
            })
            .into_any_element()
    }

    /// Batch action bar when more than one job is selected (replaces detail).
    pub(crate) fn render_batch_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let count = self.selected_ids.len();
        let selected = self.selected_jobs();

        let can_pause = selected.iter().any(|j| {
            matches!(
                j.state,
                JobState::Queued | JobState::Starting | JobState::Downloading
            )
        });
        let can_resume = selected.iter().any(|j| j.state == JobState::Paused);
        let can_retry = selected
            .iter()
            .any(|j| matches!(j.state, JobState::Failed | JobState::Canceled));
        let can_remove = selected
            .iter()
            .any(|j| j.state.is_terminal() || j.state == JobState::Paused);

        h_flex()
            .id("batch-action-bar")
            .w_full()
            .flex_shrink_0()
            .px_4()
            .py_2p5()
            .gap_2()
            .items_center()
            .flex_wrap()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.secondary.opacity(0.28))
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child(format!("{count} selected")),
            )
            .child(div().flex_1())
            .when(can_pause, |el| {
                el.child(
                    Button::new("batch-pause")
                        .outline()
                        .small()
                        .icon(IconName::Minus)
                        .label("Pause")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.batch_pause_selected();
                            cx.notify();
                        })),
                )
            })
            .when(can_resume, |el| {
                el.child(
                    Button::new("batch-resume")
                        .outline()
                        .small()
                        .icon(IconName::Redo2)
                        .label("Resume")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.batch_resume_selected();
                            cx.notify();
                        })),
                )
            })
            .when(can_retry, |el| {
                el.child(
                    Button::new("batch-retry")
                        .outline()
                        .small()
                        .icon(IconName::Redo)
                        .label("Retry")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.batch_retry_selected();
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("batch-reveal")
                    .outline()
                    .small()
                    .icon(IconName::FolderOpen)
                    .label("Open folder")
                    .tooltip("Open containing folder(s)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.batch_reveal_selected(cx);
                    })),
            )
            .when(can_remove, |el| {
                el.child(
                    Button::new("batch-remove")
                        .danger()
                        .small()
                        .icon(IconName::Delete)
                        .label("Remove…")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_remove_selected(window, cx);
                        })),
                )
            })
            .child(
                Button::new("batch-clear-selection")
                    .ghost()
                    .small()
                    .label("Clear selection")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.clear_selection();
                        cx.notify();
                    })),
            )
    }

    pub(crate) fn selected_jobs(&self) -> Vec<&Job> {
        self.jobs
            .iter()
            .filter(|j| self.selected_ids.iter().any(|id| id == &j.id))
            .collect()
    }

    pub(crate) fn batch_pause_selected(&self) {
        for job in self.selected_jobs() {
            if matches!(
                job.state,
                JobState::Queued | JobState::Starting | JobState::Downloading
            ) {
                self.engine.send(EngineCommand::Pause(job.id.clone()));
            }
        }
    }

    pub(crate) fn batch_resume_selected(&self) {
        for job in self.selected_jobs() {
            if job.state == JobState::Paused {
                self.engine.send(EngineCommand::Resume(job.id.clone()));
            }
        }
    }

    fn batch_retry_selected(&self) {
        for job in self.selected_jobs() {
            if matches!(job.state, JobState::Failed | JobState::Canceled) {
                self.engine.send(EngineCommand::Retry(job.id.clone()));
            }
        }
    }

    /// Reveal unique parent folders for the selection; cap opens and toast if many.
    fn batch_reveal_selected(&mut self, cx: &mut Context<Self>) {
        let mut parents: BTreeSet<PathBuf> = BTreeSet::new();
        for job in self.selected_jobs() {
            let path = if job.target_path.exists() {
                job.target_path.clone()
            } else {
                job.temp_path.clone()
            };
            if let Some(parent) = path.parent() {
                parents.insert(parent.to_path_buf());
            } else {
                parents.insert(path);
            }
        }

        if parents.is_empty() {
            self.show_toast("No folders to open.", cx);
            return;
        }

        let total = parents.len();
        let mut opened = 0usize;
        let mut last_err: Option<String> = None;
        for (i, dir) in parents.into_iter().enumerate() {
            if i >= BATCH_REVEAL_DIR_CAP {
                break;
            }
            match reveal_in_folder(&dir) {
                Ok(()) => opened += 1,
                Err(msg) => last_err = Some(msg),
            }
        }

        if opened == 0 {
            self.show_toast(
                last_err.unwrap_or_else(|| "Could not open folder.".into()),
                cx,
            );
        } else if total > BATCH_REVEAL_DIR_CAP {
            self.show_toast(format!("Opened {opened} of {total} folders (capped)."), cx);
        } else if total > 1 {
            self.show_toast(format!("Opened {opened} folders."), cx);
        }
    }

    pub(crate) fn render_queue_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
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

    pub(crate) fn render_search_empty(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
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

    pub(crate) fn render_empty(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let filter = self.filter;
        let show_cta = matches!(filter, FilterKind::All | FilterKind::Active);
        let reduce_motion = self.settings.reduce_motion;
        let accent = theme.primary;

        // Same ExternalPaths drop path as #queue-scroll; empty area must also accept drops.
        div()
            .id("queue-empty")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .can_drop(|drag, _, _| drag.is::<ExternalPaths>())
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.handle_external_paths_drop(paths, cx);
            }))
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
