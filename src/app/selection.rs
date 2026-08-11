//! Multi-select-ready selection state helpers for `DownloadApp`.
//!
//! Primary = `selected_ids.last()`. Detail panel is shown only when exactly one
//! id is selected. Plain click uses `select_only` (no toggle-off).

use super::DownloadApp;
use crate::download::Job;

impl DownloadApp {
    /// Primary selection id (`selected_ids.last()`), used for detail when N==1.
    pub(crate) fn primary_selected_id(&self) -> Option<&str> {
        self.selected_ids.last().map(String::as_str)
    }

    pub(crate) fn is_selected(&self, id: &str) -> bool {
        self.selected_ids.iter().any(|s| s == id)
    }

    /// Replace selection with a single id and set the range anchor.
    /// Does not toggle-off when already selected (deliberate UX).
    pub(crate) fn select_only(&mut self, id: String) {
        self.selected_ids.clear();
        self.selected_ids.push(id.clone());
        self.selection_anchor_id = Some(id);
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected_ids.clear();
        self.selection_anchor_id = None;
    }

    /// Drop selected ids (and anchor) that no longer exist in `jobs`.
    pub(crate) fn prune_selection(&mut self, jobs: &[Job]) {
        self.selected_ids
            .retain(|id| jobs.iter().any(|j| &j.id == id));
        if let Some(anchor) = &self.selection_anchor_id {
            if !jobs.iter().any(|j| &j.id == anchor) {
                self.selection_anchor_id = None;
            }
        }
    }
}
