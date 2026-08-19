//! Queue visibility: search, sidebar filter, sort, and single-selection detail.

use gpui::{App, Context};

use super::DownloadApp;
use crate::download::Job;
use crate::format::{filter_jobs, job_matches_search, sort_jobs};
use crate::persistence::save_settings;
use crate::settings::{SortColumn, SortDirection};

impl DownloadApp {
    pub(crate) fn search_query(&self, cx: &App) -> String {
        self.search_input.read(cx).value().to_string()
    }

    pub(crate) fn visible_jobs_in<'a>(&self, jobs: &'a [Job], cx: &App) -> Vec<&'a Job> {
        let query = self.search_query(cx);
        let query = query.trim().to_lowercase();
        let mut jobs: Vec<&Job> = filter_jobs(jobs, self.filter.queue_filter())
            .into_iter()
            .filter(|job| job_matches_search(job, &query))
            .collect();
        sort_jobs(
            &mut jobs,
            self.settings.sort_column,
            self.settings.sort_direction,
        );
        jobs
    }

    pub(crate) fn visible_jobs(&self, cx: &App) -> Vec<&Job> {
        self.visible_jobs_in(&self.jobs, cx)
    }

    /// Toggle or switch queue sort; persists the preference immediately.
    pub(crate) fn set_sort_column(&mut self, column: SortColumn, cx: &mut Context<Self>) {
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

    /// Detail panel job: only when exactly one id is selected.
    pub(crate) fn selected_job(&self) -> Option<&Job> {
        if self.selected_ids.len() != 1 {
            return None;
        }
        let id = self.primary_selected_id()?;
        self.jobs.iter().find(|j| j.id == id)
    }
}
