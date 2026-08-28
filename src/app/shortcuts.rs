use gpui::{App, Context, Focusable, KeyDownEvent, Window};
use gpui_component::WindowExt;

use super::filter::FilterKind;
use super::DownloadApp;
use crate::download::JobState;

impl DownloadApp {
    /// Root key handler (capture phase). Escape is handled earlier via
    /// `intercept_keystrokes` → `handle_escape_keystroke`.
    pub(crate) fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let modifiers = &event.keystroke.modifiers;

        if window.has_active_dialog(cx) || self.any_text_input_focused(window, cx) {
            return;
        }

        let secondary_only = modifiers.secondary() && modifiers.number_of_modifiers() == 1;
        let no_mods = !modifiers.modified();

        if secondary_only && key == "n" {
            self.open_add_dialog(window, cx);
            cx.stop_propagation();
            return;
        }

        if secondary_only && key == "," {
            self.select_filter(FilterKind::Settings, window, cx);
            cx.stop_propagation();
            return;
        }

        if secondary_only && key == "a" {
            if self.queue_shortcuts_active() {
                self.shortcut_select_all_visible(cx);
            }
            cx.stop_propagation();
            return;
        }

        if no_mods && key == "delete" {
            if self.queue_shortcuts_active() {
                self.shortcut_confirm_remove(window, cx);
            }
            cx.stop_propagation();
            return;
        }

        if modifiers.shift && modifiers.number_of_modifiers() == 1 && key == "delete" {
            if self.queue_shortcuts_active() {
                self.shortcut_confirm_delete(window, cx);
            }
            cx.stop_propagation();
            return;
        }

        if no_mods && key == "space" {
            if self.queue_shortcuts_active() {
                self.shortcut_toggle_pause_resume();
            }
            cx.stop_propagation();
            return;
        }

        if no_mods && key == "/" {
            self.shortcut_focus_search(window, cx);
            cx.stop_propagation();
        }
    }

    fn queue_shortcuts_active(&self) -> bool {
        self.filter != FilterKind::Settings
    }

    fn any_text_input_focused(&self, window: &Window, cx: &App) -> bool {
        [
            &self.search_input,
            &self.dir_input,
            &self.concurrent_input,
            &self.retry_input,
            &self.speed_input,
            &self.multi_max_segments_input,
            &self.multi_min_mib_input,
            &self.max_total_connections_input,
            &self.max_connections_per_host_input,
            &self.excluded_hosts_input,
            &self.captured_extensions_input,
        ]
        .into_iter()
        .any(|input| input.focus_handle(cx).is_focused(window))
            || self
                .category_folder_inputs
                .iter()
                .any(|input| input.focus_handle(cx).is_focused(window))
    }

    fn shortcut_select_all_visible(&mut self, cx: &mut Context<Self>) {
        if !self.queue_shortcuts_active() {
            return;
        }
        let visible = self.visible_jobs(cx);
        if visible.is_empty() {
            return;
        }
        let ids: Vec<String> = visible.iter().map(|j| j.id.clone()).collect();
        self.selected_ids = ids;
        self.selection_anchor_id = self.selected_ids.first().cloned();
        cx.notify();
    }

    fn shortcut_confirm_remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.queue_shortcuts_active() || self.selected_ids.is_empty() {
            return;
        }
        if self.selected_ids.len() == 1 {
            let id = self.selected_ids[0].clone();
            let filename = self
                .jobs
                .iter()
                .find(|j| j.id == id)
                .map(|j| j.filename.clone())
                .unwrap_or_else(|| id.clone());
            self.confirm_remove(id, filename, window, cx);
        } else {
            self.confirm_remove_selected(window, cx);
        }
    }

    fn shortcut_confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.queue_shortcuts_active() || self.selected_ids.is_empty() {
            return;
        }
        if self.selected_ids.len() == 1 {
            let id = self.selected_ids[0].clone();
            let Some(job) = self.jobs.iter().find(|j| j.id == id) else {
                return;
            };
            if !job.has_deletable_file() {
                self.show_toast("No file on disk to delete.", cx);
                return;
            }
            let filename = job.filename.clone();
            self.confirm_delete(id, filename, window, cx);
        } else {
            self.confirm_delete_selected(window, cx);
        }
    }

    fn shortcut_toggle_pause_resume(&mut self) {
        if !self.queue_shortcuts_active() || self.selected_ids.is_empty() {
            return;
        }
        let any_pausable = self.selected_jobs().iter().any(|j| {
            matches!(
                j.state,
                JobState::Queued | JobState::Starting | JobState::Downloading
            )
        });
        if any_pausable {
            self.batch_pause_selected();
        } else {
            self.batch_resume_selected();
        }
    }

    fn shortcut_focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter == FilterKind::Settings {
            self.leave_settings(window, cx);
        }
        self.search_input
            .update(cx, |input, cx| input.focus(window, cx));
    }
}
