use gpui::{
    div, prelude::FluentBuilder, px, ClickEvent, Context, Corner, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt,
};

use super::layout::{QueueColumns, COL_ACTIONS_W, COL_DATE_W, COL_ETA_W, COL_SIZE_W, COL_SPEED_W};
use super::widgets::{
    ellipsize_name, metric_cell, name_char_budget, soft_tooltip, status_color, status_dot,
    styled_progress,
};
use super::DownloadApp;
use crate::download::{open_path, reveal_in_folder, EngineCommand, Job, JobState};
use crate::format::{format_date, format_eta, format_size, format_speed};
use crate::settings::{ProgressStyle, UiDensity};

pub(crate) fn render_job_row(
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
        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            // Modifier-click multi-select (PR-07): Ctrl/Cmd = toggle, Shift = range.
            // Plain click: select only (no toggle-off). Stop bubble so empty
            // queue chrome can clear selection on background click.
            let m = event.modifiers();
            if m.secondary() {
                this.toggle_select(id_for_select.clone());
            } else if m.shift {
                let visible = this.visible_jobs(cx);
                this.select_range_visible(&id_for_select, &visible);
            } else {
                this.select_only(id_for_select.clone());
            }
            cx.notify();
            cx.stop_propagation();
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
pub(crate) fn restart_icon() -> Icon {
    Icon::empty().path("icons/rotate-cw.svg")
}
