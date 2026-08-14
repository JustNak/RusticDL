//! Multi-select-ready selection state helpers for `DownloadApp`.
//!
//! Primary = `selected_ids.last()`. Detail panel is shown only when exactly one
//! id is selected. Plain click uses `select_only` (no toggle-off).
//! Ctrl/secondary toggles membership; Shift ranges from the anchor through the
//! current visible sorted list.

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

    /// Ctrl/Cmd-click: toggle membership; set anchor to the clicked id.
    /// Primary becomes the clicked id when adding; when removing, the previous
    /// last remaining id (if any) stays primary.
    pub(crate) fn toggle_select(&mut self, id: String) {
        if let Some(pos) = self.selected_ids.iter().position(|s| s == &id) {
            self.selected_ids.remove(pos);
        } else {
            self.selected_ids.push(id.clone());
        }
        self.selection_anchor_id = Some(id);
    }

    /// Shift-click: select the inclusive range from `selection_anchor_id` to
    /// `to_id` in **current visible sorted list order**. Keeps the original
    /// anchor. Primary = `to_id` (placed last). Falls back to `select_only`
    /// when anchor/target are missing from `visible`.
    pub(crate) fn select_range_visible(&mut self, to_id: &str, visible: &[&Job]) {
        let anchor = match self.selection_anchor_id.as_deref() {
            Some(a) => a,
            None => {
                self.select_only(to_id.to_string());
                return;
            }
        };

        let anchor_idx = visible.iter().position(|j| j.id == anchor);
        let to_idx = visible.iter().position(|j| j.id == to_id);

        let (Some(a), Some(b)) = (anchor_idx, to_idx) else {
            self.select_only(to_id.to_string());
            return;
        };

        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let mut ids: Vec<String> = visible[lo..=hi].iter().map(|j| j.id.clone()).collect();

        // Ensure clicked id is primary (last) without duplicating it.
        if let Some(pos) = ids.iter().position(|id| id == to_id) {
            let clicked = ids.remove(pos);
            ids.push(clicked);
        }

        self.selected_ids = ids;
        // Anchor stays put for subsequent Shift-clicks.
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected_ids.clear();
        self.selection_anchor_id = None;
    }

    /// Drop selected ids (and anchor) that no longer exist in `jobs`.
    /// Anchor may outlive `selected_ids` when the anchor job still exists but
    /// was deselected — that is intentional for Shift-range UX.
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
