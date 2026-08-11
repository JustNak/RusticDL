//! Queue list, toolbar, and empty states extracted from `DownloadApp`.

use gpui::{
    div, prelude::FluentBuilder, px, Context, Corner, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    menu::{DropdownMenu, PopupMenuItem},
    v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt,
};

use super::detail::render_detail;
use super::filter::FilterKind;
use super::job_row::render_job_row;
use super::layout::{
    QueueColumns, COL_ACTIONS_W, COL_DATE_W, COL_ETA_W, COL_SIZE_W, COL_SPEED_W, DETAIL_MAX_H,
    DETAIL_MIN_CAP, LIST_MIN_H, STATUS_DOT,
};
use super::widgets::{empty_state_badge, sortable_header};
use super::DownloadApp;
use crate::download::{EngineCommand, JobState};
use crate::format::filter_jobs;
use crate::settings::SortColumn;

impl DownloadApp {
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
            .when_some(detail, |el, job| {
                el.child(render_detail(&job, detail_max_h, cx))
            })
            .into_any_element()
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
