use std::path::PathBuf;

use gpui::{Context, Window};
use gpui_component::WindowExt;

use super::confirm_dialogs;
use super::DownloadApp;
use crate::appearance::{apply_appearance, apply_window_opacity};
use crate::download::{EngineCommand, EngineRuntimeConfig, FileTypeKind};
use crate::extension_settings::{DownloadHandoffMode, ExtensionIntegrationSettings};
use crate::persistence::save_settings;
use crate::settings::{
    AccentPreset, AppTheme, CornerRadiusScale, OsNotifyMode, ProgressStyle, Settings, UiDensity,
};
use crate::startup::apply_launch_at_startup;

impl DownloadApp {
    /// Settings snapshot safe for incidental disk writes (layout, sort).
    /// Keeps committed extension when the user has unsaved Browser capture previews.
    pub(crate) fn settings_for_disk(&self) -> Settings {
        let mut settings = self.settings.clone();
        if self.extension_settings_dirty {
            settings.extension = self.extension_committed.clone();
        }
        settings
    }

    /// Pull extension settings from the IPC bridge when safe.
    ///
    /// While dirty, keeps the live preview (`settings.extension`) but still
    /// advances `extension_committed` from the bridge so incidental disk
    /// flushes do not overwrite a newer extension-saved snapshot.
    pub(crate) fn sync_extension_settings_from_bridge(&mut self, force_text_refresh: bool) {
        let Some(extension) = self.ipc.extension_settings() else {
            return;
        };
        if self.extension_settings_dirty {
            if self.extension_committed != extension {
                self.extension_committed = extension;
            }
            return;
        }
        if self.settings.extension == extension {
            return;
        }
        self.settings.extension = extension.clone();
        self.extension_committed = extension;
        if force_text_refresh || self.filter == super::FilterKind::Settings {
            self.extension_text_inputs_stale = true;
        }
    }

    pub(crate) fn refresh_extension_text_inputs(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let excluded = self.settings.extension.excluded_hosts.join("\n");
        let captured = self.settings.extension.captured_file_extensions.join(", ");
        self.excluded_hosts_input
            .update(cx, |i, cx| i.set_value(excluded, window, cx));
        self.captured_extensions_input
            .update(cx, |i, cx| i.set_value(captured, window, cx));
        self.extension_text_inputs_stale = false;
    }

    pub(crate) fn save_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            .unwrap_or(6);
        let speed_limit = self
            .speed_input
            .read(cx)
            .value()
            .parse::<u32>()
            .unwrap_or(0);
        let multi_max_segments = self
            .multi_max_segments_input
            .read(cx)
            .value()
            .parse::<u32>()
            .unwrap_or(8);
        let multi_min_mib = self
            .multi_min_mib_input
            .read(cx)
            .value()
            .parse::<u64>()
            .unwrap_or(5)
            .max(1);
        let max_total_connections = self
            .max_total_connections_input
            .read(cx)
            .value()
            .parse::<u32>()
            .unwrap_or(32);
        let max_connections_per_host = self
            .max_connections_per_host_input
            .read(cx)
            .value()
            .parse::<u32>()
            .unwrap_or(8);

        self.settings.download_directory = download_directory;
        self.settings.max_concurrent_downloads = max_concurrent;
        self.settings.auto_retry_attempts = auto_retry;
        self.settings.speed_limit_kib_per_second = speed_limit;
        self.settings.multi_max_segments = multi_max_segments;
        self.settings.multi_min_bytes = multi_min_mib.saturating_mul(1024 * 1024);
        self.settings.max_total_connections = max_total_connections;
        self.settings.max_connections_per_host = max_connections_per_host;
        self.settings.multi_connection_enabled = self.draft_multi_connection_enabled;

        let excluded_hosts = self
            .excluded_hosts_input
            .read(cx)
            .value()
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        let captured_extensions = self
            .captured_extensions_input
            .read(cx)
            .value()
            .split(|c: char| c == ',' || c.is_whitespace())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        self.settings.extension.excluded_hosts = excluded_hosts;
        self.settings.extension.captured_file_extensions = captured_extensions;

        for (i, kind) in FileTypeKind::ALL.iter().enumerate() {
            let raw = self.category_folder_inputs[i].read(cx).value().to_string();
            self.settings.category_folders.get_mut(*kind).name = raw;
        }

        self.settings.sanitize_appearance();
        let multi_segs = self.settings.multi_max_segments.to_string();
        let multi_mib = (self.settings.multi_min_bytes / (1024 * 1024))
            .max(1)
            .to_string();
        let max_total = self.settings.max_total_connections.to_string();
        let max_host = self.settings.max_connections_per_host.to_string();
        let concurrent = self.settings.max_concurrent_downloads.to_string();
        let retry = self.settings.auto_retry_attempts.to_string();
        self.concurrent_input
            .update(cx, |i, cx| i.set_value(concurrent, window, cx));
        self.retry_input
            .update(cx, |i, cx| i.set_value(retry, window, cx));
        self.multi_max_segments_input
            .update(cx, |i, cx| i.set_value(multi_segs, window, cx));
        self.multi_min_mib_input
            .update(cx, |i, cx| i.set_value(multi_mib, window, cx));
        self.max_total_connections_input
            .update(cx, |i, cx| i.set_value(max_total, window, cx));
        self.max_connections_per_host_input
            .update(cx, |i, cx| i.set_value(max_host, window, cx));

        self.extension_settings_dirty = false;
        self.extension_committed = self.settings.extension.clone();
        self.refresh_extension_text_inputs(window, cx);
        self.refresh_category_folder_inputs(window, cx);
        let _ = save_settings(&self.paths, &self.settings);
        self.ipc.update_settings(&self.settings);

        if let Err(msg) = apply_launch_at_startup(
            self.settings.launch_at_startup,
            self.settings.startup_minimized,
        ) {
            self.show_toast(format!("Startup setting: {msg}"), cx);
        }

        self.sync_tray_lifetime(cx);

        apply_appearance(&self.settings, Some(window), cx);

        self.engine.send(EngineCommand::UpdateSettings(
            EngineRuntimeConfig::from_settings(&self.settings),
        ));

        self.show_toast("Settings saved.", cx);
        cx.notify();
    }

    pub(crate) fn refresh_category_folder_inputs(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (i, kind) in FileTypeKind::ALL.iter().enumerate() {
            let name = self.settings.category_folders.name(*kind).to_string();
            self.category_folder_inputs[i].update(cx, |input, cx| {
                input.set_value(name.clone(), window, cx);
            });
        }
    }

    pub(crate) fn toggle_sidebar_library(&mut self, cx: &mut Context<Self>) {
        self.settings.sidebar_library_expanded = !self.settings.sidebar_library_expanded;
        let _ = save_settings(&self.paths, &self.settings_for_disk());
        cx.notify();
    }

    pub(crate) fn set_organize_by_file_type(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.organize_by_file_type = on;
        cx.notify();
    }

    pub(crate) fn set_category_enabled(
        &mut self,
        kind: FileTypeKind,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.category_folders.get_mut(kind).enabled = on;
        cx.notify();
    }

    pub(crate) fn set_draft_multi_connection_enabled(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft_multi_connection_enabled = on;
        cx.notify();
    }

    pub(crate) fn set_close_to_tray(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.close_to_tray = on;
        self.sync_tray_lifetime(cx);
        cx.notify();
    }

    pub(crate) fn set_update_channel(
        &mut self,
        channel: crate::settings::UpdateChannel,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.update_channel == channel {
            return;
        }
        self.settings.update_channel = channel;
        self.available_update = None;
        self.update_check_gen = self.update_check_gen.wrapping_add(1);
        self.update_busy = false;
        self.clear_update_toast(cx);
        cx.notify();
    }

    fn update_extension_settings_draft(
        &mut self,
        f: impl FnOnce(&mut ExtensionIntegrationSettings),
        cx: &mut Context<Self>,
    ) {
        f(&mut self.settings.extension);
        self.extension_settings_dirty = true;
        cx.notify();
    }

    pub(crate) fn set_extension_enabled(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_extension_settings_draft(|ext| ext.enabled = on, cx);
    }

    pub(crate) fn set_download_handoff_mode(
        &mut self,
        mode: DownloadHandoffMode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_extension_settings_draft(|ext| ext.download_handoff_mode = mode, cx);
    }

    pub(crate) fn set_context_menu_enabled(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_extension_settings_draft(|ext| ext.context_menu_enabled = on, cx);
    }

    pub(crate) fn set_show_badge_status(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_extension_settings_draft(|ext| ext.show_badge_status = on, cx);
    }

    pub(crate) fn set_show_progress_after_handoff(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_extension_settings_draft(|ext| ext.show_progress_after_handoff = on, cx);
    }

    pub(crate) fn set_download_capture_debug_logging(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_extension_settings_draft(|ext| ext.download_capture_debug_logging = on, cx);
    }

    pub(crate) fn set_launch_at_startup(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.launch_at_startup = on;
        if !on {
            self.settings.startup_minimized = false;
        }
        cx.notify();
    }

    pub(crate) fn set_startup_minimized(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.startup_minimized = on && self.settings.launch_at_startup;
        cx.notify();
    }

    pub(crate) fn set_os_notify_mode(
        &mut self,
        mode: OsNotifyMode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.os_notify_mode = mode;
        self.sync_tray_lifetime(cx);
        if mode == OsNotifyMode::Off {
            self.os_notify_buffer.clear();
        }
        cx.notify();
    }

    pub(crate) fn set_notify_on_complete(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.notify_on_complete = on;
        cx.notify();
    }

    pub(crate) fn set_notify_on_fail(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.notify_on_fail = on;
        cx.notify();
    }

    pub(crate) fn set_clipboard_watch_enabled(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.clipboard_watch_enabled = on;
        if !on {
            self.last_clipboard_urls_key = None;
        }
        cx.notify();
    }

    /// On main-window activation: optionally offer HTTP(S) clipboard URLs.
    ///
    /// Safety: never enqueues without a confirm dialog. Skips when disabled,
    /// tray-hidden, a dialog is already open, or the same URL set was just offered.
    pub(crate) fn on_window_activated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings.clipboard_watch_enabled {
            return;
        }
        if self.window_hidden_to_tray {
            return;
        }
        if window.has_active_dialog(cx) {
            return;
        }

        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let urls = crate::download::extract_http_urls(&text);
        if urls.is_empty() {
            return;
        }

        let key = confirm_dialogs::clipboard_urls_key(&urls);
        if self.last_clipboard_urls_key == Some(key) {
            return;
        }
        self.last_clipboard_urls_key = Some(key);
        self.confirm_add_clipboard_urls(urls, window, cx);
    }

    pub(crate) fn set_theme_draft(
        &mut self,
        theme: AppTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.theme = theme;
        apply_appearance(&self.settings, Some(window), cx);
        cx.notify();
    }

    pub(crate) fn set_accent_preset(
        &mut self,
        preset: AccentPreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.accent_preset = preset;
        apply_appearance(&self.settings, Some(window), cx);
        cx.notify();
    }

    pub(crate) fn reset_appearance_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(crate) fn sync_window_chrome(&mut self, window: &mut Window) {
        let pct = self.settings.window_transparency;
        let key = pct.saturating_add(if self.settings.backdrop_blur { 128 } else { 0 });
        if self.applied_window_transparency == Some(key) {
            return;
        }
        apply_window_opacity(window, pct, self.settings.backdrop_blur);
        self.applied_window_transparency = Some(key);
    }

    pub(crate) fn set_ui_density(
        &mut self,
        density: UiDensity,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.ui_density = density;
        apply_appearance(&self.settings, Some(window), cx);
        cx.notify();
    }

    pub(crate) fn set_corner_radius(
        &mut self,
        scale: CornerRadiusScale,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.corner_radius = scale;
        apply_appearance(&self.settings, Some(window), cx);
        cx.notify();
    }

    pub(crate) fn set_backdrop_blur(
        &mut self,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.backdrop_blur = on;
        apply_appearance(&self.settings, Some(window), cx);
        cx.notify();
    }

    pub(crate) fn set_reduce_motion(
        &mut self,
        on: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.reduce_motion = on;
        cx.notify();
    }

    pub(crate) fn set_progress_style(
        &mut self,
        style: ProgressStyle,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.progress_style = style;
        cx.notify();
    }
}
