use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::download::FileTypeKind;
use crate::extension_settings::ExtensionIntegrationSettings;

/// Default first-run window size (logical px). Matches the designed layout:
/// sidebar + full metric columns + detail panel with comfortable breathing room.
pub const DEFAULT_WINDOW_WIDTH: f32 = 1120.0;
pub const DEFAULT_WINDOW_HEIGHT: f32 = 720.0;
/// Matches `window_min_size` in `main.rs` (progressive column-collapse floor).
pub const MIN_WINDOW_WIDTH: f32 = 960.0;
pub const MIN_WINDOW_HEIGHT: f32 = 600.0;
const MAX_WINDOW_DIM: f32 = 10_000.0;

/// Persisted main-window geometry (logical pixels).
///
/// - Fresh install: centered `DEFAULT_WINDOW_*` size, not maximized.
/// - After the user resizes/moves: restored on next launch (including maximized).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowLayout {
    pub width: f32,
    pub height: f32,
    /// Top-left origin in screen coordinates; `None` means center on the cursor's
    /// monitor work area (fallback: host window monitor, then primary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
    #[serde(default)]
    pub maximized: bool,
}

impl Default for WindowLayout {
    fn default() -> Self {
        Self {
            width: DEFAULT_WINDOW_WIDTH,
            height: DEFAULT_WINDOW_HEIGHT,
            x: None,
            y: None,
            maximized: false,
        }
    }
}

impl WindowLayout {
    pub fn sanitize(&mut self) {
        if !self.width.is_finite() {
            self.width = DEFAULT_WINDOW_WIDTH;
        }
        if !self.height.is_finite() {
            self.height = DEFAULT_WINDOW_HEIGHT;
        }
        self.width = self.width.clamp(MIN_WINDOW_WIDTH, MAX_WINDOW_DIM);
        self.height = self.height.clamp(MIN_WINDOW_HEIGHT, MAX_WINDOW_DIM);
        if let Some(x) = self.x {
            if !x.is_finite() {
                self.x = None;
            }
        }
        if let Some(y) = self.y {
            if !y.is_finite() {
                self.y = None;
            }
        }
        // Position is all-or-nothing so restore never anchors only one axis.
        if self.x.is_none() || self.y.is_none() {
            self.x = None;
            self.y = None;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppTheme {
    #[default]
    Light,
    Dark,
    System,
}

/// Preset accent colors for the Appearance section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccentPreset {
    /// Keep the built-in theme primary (no tint override).
    #[default]
    Default,
    Blue,
    Cyan,
    Emerald,
    Amber,
    Rose,
    Violet,
    Orange,
    Slate,
    Custom,
}

impl AccentPreset {
    pub const ALL: [AccentPreset; 10] = [
        AccentPreset::Default,
        AccentPreset::Blue,
        AccentPreset::Cyan,
        AccentPreset::Emerald,
        AccentPreset::Amber,
        AccentPreset::Rose,
        AccentPreset::Violet,
        AccentPreset::Orange,
        AccentPreset::Slate,
        AccentPreset::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Blue => "Blue",
            Self::Cyan => "Cyan",
            Self::Emerald => "Emerald",
            Self::Amber => "Amber",
            Self::Rose => "Rose",
            Self::Violet => "Violet",
            Self::Orange => "Orange",
            Self::Slate => "Slate",
            Self::Custom => "Custom",
        }
    }
}

/// UI spacing / row height density (Phase C).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UiDensity {
    #[default]
    Comfortable,
    Compact,
}

impl UiDensity {
    pub const ALL: [UiDensity; 2] = [UiDensity::Comfortable, UiDensity::Compact];

    pub fn label(self) -> &'static str {
        match self {
            Self::Comfortable => "Comfortable",
            Self::Compact => "Compact",
        }
    }

    pub fn row_h(self) -> f32 {
        match self {
            Self::Comfortable => 52.0,
            Self::Compact => 42.0,
        }
    }

    pub fn sidebar_w(self) -> f32 {
        match self {
            Self::Comfortable => 220.0,
            Self::Compact => 192.0,
        }
    }

    pub fn settings_pad(self) -> f32 {
        match self {
            Self::Comfortable => 24.0,
            Self::Compact => 16.0,
        }
    }

    pub fn font_size(self) -> f32 {
        match self {
            Self::Comfortable => 16.0,
            Self::Compact => 14.0,
        }
    }
}

/// Corner radius scale applied to theme tokens (Phase C).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CornerRadiusScale {
    Sharp,
    #[default]
    Default,
    Soft,
}

impl CornerRadiusScale {
    pub const ALL: [CornerRadiusScale; 3] = [
        CornerRadiusScale::Sharp,
        CornerRadiusScale::Default,
        CornerRadiusScale::Soft,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Sharp => "Sharp",
            Self::Default => "Default",
            Self::Soft => "Soft",
        }
    }

    /// (radius, radius_lg) in logical px.
    pub fn radii(self) -> (f32, f32) {
        match self {
            Self::Sharp => (2.0, 4.0),
            Self::Default => (6.0, 8.0),
            Self::Soft => (10.0, 14.0),
        }
    }
}

/// Progress bar visual style (Phase D).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStyle {
    #[default]
    Solid,
    Soft,
    Glow,
    Segmented,
}

impl ProgressStyle {
    pub const ALL: [ProgressStyle; 4] = [
        ProgressStyle::Solid,
        ProgressStyle::Soft,
        ProgressStyle::Glow,
        ProgressStyle::Segmented,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid",
            Self::Soft => "Soft",
            Self::Glow => "Glow",
            Self::Segmented => "Segmented",
        }
    }
}

/// Hard floor for *effective* window alpha when transparency is maxed.
/// Slider 100% still keeps the window at least this opaque.
pub const MIN_WINDOW_OPACITY: u8 = 75;
pub const MAX_WINDOW_TRANSPARENCY: u8 = 100;
pub const MAX_NOISE_INTENSITY: u8 = 100;
pub const MAX_VIGNETTE_INTENSITY: u8 = 100;

fn default_accent_hue() -> f32 {
    220.0
}

fn default_accent_saturation() -> f32 {
    80.0
}

fn default_accent_lightness() -> f32 {
    55.0
}

/// Which queue column drives sort order (persisted as a user preference).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortColumn {
    Name,
    #[default]
    Date,
    Speed,
    Eta,
    Size,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Asc,
    #[default]
    Desc,
}

impl SortDirection {
    pub fn toggle(self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }
}

/// When to show OS (tray balloon) notifications for terminal downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OsNotifyMode {
    /// Only when the main window is hidden to the tray (recommended).
    #[default]
    WhenHiddenToTray,
    /// Always fire OS notification (subject to tray availability).
    Always,
    /// Never use OS notifications.
    Off,
}

impl OsNotifyMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::WhenHiddenToTray => "When hidden",
            Self::Always => "Always",
        }
    }
}

/// Which GitHub Releases stream the auto-updater follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    /// Latest non-prerelease (`/releases/latest`).
    #[default]
    Stable,
    /// Newest published `vX.Y.Z-nightly.*` GitHub pre-release with a setup asset.
    Nightly,
}

impl UpdateChannel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stable => "Stable",
            Self::Nightly => "Nightly",
        }
    }
}

/// One type-folder under the main download directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryFolder {
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl CategoryFolder {
    fn named(name: &str) -> Self {
        Self {
            name: name.to_string(),
            enabled: true,
        }
    }

    fn sanitize(&mut self, default_name: &str) {
        self.name = sanitize_category_folder_name(&self.name, default_name);
    }
}

/// Per-type subfolder names (and optional disable) for organize-by-type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryFolders {
    #[serde(default = "default_video_folder")]
    pub video: CategoryFolder,
    #[serde(default = "default_audio_folder")]
    pub audio: CategoryFolder,
    #[serde(default = "default_compressed_folder")]
    pub compressed: CategoryFolder,
    #[serde(default = "default_images_folder")]
    pub images: CategoryFolder,
    #[serde(default = "default_documents_folder")]
    pub documents: CategoryFolder,
    #[serde(default = "default_programs_folder")]
    pub programs: CategoryFolder,
    #[serde(default = "default_other_folder")]
    pub other: CategoryFolder,
}

impl Default for CategoryFolders {
    fn default() -> Self {
        Self {
            video: default_video_folder(),
            audio: default_audio_folder(),
            compressed: default_compressed_folder(),
            images: default_images_folder(),
            documents: default_documents_folder(),
            programs: default_programs_folder(),
            other: default_other_folder(),
        }
    }
}

impl CategoryFolders {
    pub fn get(&self, kind: FileTypeKind) -> &CategoryFolder {
        match kind {
            FileTypeKind::Video => &self.video,
            FileTypeKind::Audio => &self.audio,
            FileTypeKind::Compressed => &self.compressed,
            FileTypeKind::Images => &self.images,
            FileTypeKind::Documents => &self.documents,
            FileTypeKind::Programs => &self.programs,
            FileTypeKind::Other => &self.other,
        }
    }

    pub fn get_mut(&mut self, kind: FileTypeKind) -> &mut CategoryFolder {
        match kind {
            FileTypeKind::Video => &mut self.video,
            FileTypeKind::Audio => &mut self.audio,
            FileTypeKind::Compressed => &mut self.compressed,
            FileTypeKind::Images => &mut self.images,
            FileTypeKind::Documents => &mut self.documents,
            FileTypeKind::Programs => &mut self.programs,
            FileTypeKind::Other => &mut self.other,
        }
    }

    pub fn name(&self, kind: FileTypeKind) -> &str {
        let name = self.get(kind).name.as_str();
        if name.is_empty() {
            kind.default_folder_name()
        } else {
            name
        }
    }

    pub fn folder_if_enabled(&self, kind: FileTypeKind) -> Option<&str> {
        let entry = self.get(kind);
        if entry.enabled {
            Some(self.name(kind))
        } else {
            None
        }
    }

    pub fn sanitize(&mut self) {
        for kind in FileTypeKind::ALL {
            self.get_mut(kind).sanitize(kind.default_folder_name());
        }
    }
}

fn default_video_folder() -> CategoryFolder {
    CategoryFolder::named(FileTypeKind::Video.default_folder_name())
}
fn default_audio_folder() -> CategoryFolder {
    CategoryFolder::named(FileTypeKind::Audio.default_folder_name())
}
fn default_compressed_folder() -> CategoryFolder {
    CategoryFolder::named(FileTypeKind::Compressed.default_folder_name())
}
fn default_images_folder() -> CategoryFolder {
    CategoryFolder::named(FileTypeKind::Images.default_folder_name())
}
fn default_documents_folder() -> CategoryFolder {
    CategoryFolder::named(FileTypeKind::Documents.default_folder_name())
}
fn default_programs_folder() -> CategoryFolder {
    CategoryFolder::named(FileTypeKind::Programs.default_folder_name())
}
fn default_other_folder() -> CategoryFolder {
    CategoryFolder::named(FileTypeKind::Other.default_folder_name())
}

fn default_organize_by_file_type() -> bool {
    true
}

fn default_sidebar_library_expanded() -> bool {
    true
}

/// Compare download directories ignoring slash style, trailing separators, and ASCII case.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    fn key(p: &Path) -> String {
        let s = p.to_string_lossy().replace('/', "\\");
        s.trim_end_matches('\\').to_ascii_lowercase()
    }
    !a.as_os_str().is_empty() && !b.as_os_str().is_empty() && key(a) == key(b)
}

/// Keep a single folder name (no `..`, no separators). Empty / invalid → `default_name`.
pub fn sanitize_category_folder_name(raw: &str, default_name: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return default_name.to_string();
    }
    let component = Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(trimmed);
    if component == "." || component == ".." {
        return default_name.to_string();
    }
    let sanitized = crate::download::sanitize_filename(component);
    if sanitized.is_empty() || sanitized == "download.bin" {
        default_name.to_string()
    } else {
        sanitized
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub download_directory: PathBuf,
    /// Save new downloads under `{download_directory}/{Type}/filename`.
    #[serde(default = "default_organize_by_file_type")]
    pub organize_by_file_type: bool,
    /// Sidebar type tree under All downloads is expanded.
    #[serde(default = "default_sidebar_library_expanded")]
    pub sidebar_library_expanded: bool,
    #[serde(default)]
    pub category_folders: CategoryFolders,
    pub max_concurrent_downloads: u32,
    pub auto_retry_attempts: u32,
    pub speed_limit_kib_per_second: u32,
    /// Max parallel segments per multi download (clamped 1–16).
    #[serde(default = "default_multi_max_segments")]
    pub multi_max_segments: u32,
    /// Files smaller than this use a single connection (bytes).
    #[serde(default = "default_multi_min_bytes")]
    pub multi_min_bytes: u64,
    /// Process-wide cap on concurrent download body connections.
    #[serde(default = "default_max_total_connections")]
    pub max_total_connections: u32,
    /// Per-host connection budget for multi-segment downloads.
    #[serde(default = "default_max_connections_per_host")]
    pub max_connections_per_host: u32,
    /// When false, planner stays on single-stream unless a live map forces Multi.
    #[serde(default = "default_true")]
    pub multi_connection_enabled: bool,
    /// Stable vs Nightly (`vX.Y.Z-nightly.*` pre-release) update stream.
    #[serde(default)]
    pub update_channel: UpdateChannel,
    pub theme: AppTheme,
    /// Accent palette; `Default` keeps the stock theme primary.
    #[serde(default)]
    pub accent_preset: AccentPreset,
    /// Hue in degrees 0..360, used when `accent_preset == Custom`.
    #[serde(default = "default_accent_hue")]
    pub accent_hue: f32,
    /// Saturation 0..100, used when `accent_preset == Custom`.
    #[serde(default = "default_accent_saturation")]
    pub accent_saturation: f32,
    /// Lightness 0..100, used when `accent_preset == Custom`.
    #[serde(default = "default_accent_lightness")]
    pub accent_lightness: f32,
    /// Film-grain overlay strength 0..100 (0 = off).
    #[serde(default)]
    pub noise_intensity: u8,
    /// Window transparency 0..100 (0 = solid / default, 100 = max glass).
    /// Effective alpha never drops below [`MIN_WINDOW_OPACITY`]%.
    #[serde(default)]
    pub window_transparency: u8,
    /// When true and transparency > 0, request OS backdrop blur / acrylic when available.
    #[serde(default)]
    pub backdrop_blur: bool,
    #[serde(default)]
    pub ui_density: UiDensity,
    #[serde(default)]
    pub corner_radius: CornerRadiusScale,
    /// Prefer static UI (no decorative motion) when true.
    #[serde(default)]
    pub reduce_motion: bool,
    /// Edge vignette strength 0..100 (0 = off).
    #[serde(default)]
    pub vignette_intensity: u8,
    #[serde(default)]
    pub progress_style: ProgressStyle,
    #[serde(default)]
    pub sort_column: SortColumn,
    #[serde(default)]
    pub sort_direction: SortDirection,
    /// Last main-window size / position / maximized state.
    #[serde(default)]
    pub window_layout: WindowLayout,
    /// When true, the window close button hides the app to the system tray
    /// (notification overflow) instead of quitting.
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    /// Launch RusticDL automatically when the user signs in to Windows.
    #[serde(default)]
    pub launch_at_startup: bool,
    /// When launching at sign-in, start hidden in the tray (requires tray).
    #[serde(default)]
    pub startup_minimized: bool,
    /// When to fire OS tray-balloon notifications for completed/failed downloads.
    #[serde(default)]
    pub os_notify_mode: OsNotifyMode,
    /// Show notifications when a download completes successfully.
    #[serde(default = "default_true")]
    pub notify_on_complete: bool,
    /// Show notifications when a download fails (after retries).
    #[serde(default = "default_true")]
    pub notify_on_fail: bool,
    /// When true, on main-window focus gain offer to add HTTP(S) URLs found on the clipboard.
    /// Never auto-downloads; always confirms. Off by default.
    #[serde(default)]
    pub clipboard_watch_enabled: bool,
    /// Browser extension integration preferences (source of truth for the companion extension).
    #[serde(default)]
    pub extension: ExtensionIntegrationSettings,
}

fn default_true() -> bool {
    true
}

fn default_multi_max_segments() -> u32 {
    8
}

fn default_multi_min_bytes() -> u64 {
    5 * 1024 * 1024
}

fn default_max_total_connections() -> u32 {
    32
}

fn default_max_connections_per_host() -> u32 {
    8
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_directory: default_download_directory(),
            organize_by_file_type: true,
            sidebar_library_expanded: true,
            category_folders: CategoryFolders::default(),
            max_concurrent_downloads: 3,
            auto_retry_attempts: 6,
            speed_limit_kib_per_second: 0,
            multi_max_segments: default_multi_max_segments(),
            multi_min_bytes: default_multi_min_bytes(),
            max_total_connections: default_max_total_connections(),
            max_connections_per_host: default_max_connections_per_host(),
            multi_connection_enabled: true,
            update_channel: UpdateChannel::Stable,
            theme: AppTheme::Light,
            accent_preset: AccentPreset::Default,
            accent_hue: default_accent_hue(),
            accent_saturation: default_accent_saturation(),
            accent_lightness: default_accent_lightness(),
            noise_intensity: 0,
            window_transparency: 0,
            backdrop_blur: false,
            ui_density: UiDensity::Comfortable,
            corner_radius: CornerRadiusScale::Default,
            reduce_motion: false,
            vignette_intensity: 0,
            progress_style: ProgressStyle::Solid,
            sort_column: SortColumn::Date,
            sort_direction: SortDirection::Desc,
            window_layout: WindowLayout::default(),
            close_to_tray: true,
            launch_at_startup: false,
            startup_minimized: false,
            os_notify_mode: OsNotifyMode::WhenHiddenToTray,
            notify_on_complete: true,
            notify_on_fail: true,
            clipboard_watch_enabled: false,
            extension: ExtensionIntegrationSettings::default(),
        }
    }
}

impl Settings {
    /// Clamp download / engine limit fields to safe ranges.
    pub fn sanitize_download_limits(&mut self) {
        self.max_concurrent_downloads = self.max_concurrent_downloads.clamp(1, 64);
        self.auto_retry_attempts = self.auto_retry_attempts.min(100);
        self.multi_max_segments = self.multi_max_segments.clamp(1, 16);
        self.multi_min_bytes = self.multi_min_bytes.clamp(1024 * 1024, 1024 * 1024 * 1024);
        self.max_total_connections = self.max_total_connections.clamp(1, 256);
        self.max_connections_per_host = self.max_connections_per_host.clamp(1, 64);
        // Per-host cannot exceed process-wide total.
        self.max_connections_per_host = self
            .max_connections_per_host
            .min(self.max_total_connections);
    }

    /// Clamp appearance fields to safe ranges (call after load / before save).
    pub fn sanitize_appearance(&mut self) {
        self.noise_intensity = self.noise_intensity.min(MAX_NOISE_INTENSITY);
        // Slider is 0–100; the alpha floor is applied when painting, not here.
        self.window_transparency = self.window_transparency.min(MAX_WINDOW_TRANSPARENCY);
        self.vignette_intensity = self.vignette_intensity.min(MAX_VIGNETTE_INTENSITY);
        self.accent_hue = self.accent_hue.rem_euclid(360.0);
        self.accent_saturation = self.accent_saturation.clamp(0.0, 100.0);
        self.accent_lightness = self.accent_lightness.clamp(0.0, 100.0);
        self.window_layout.sanitize();
        self.extension.sanitize();
        self.category_folders.sanitize();
        self.sanitize_download_limits();
    }

    /// Destination folder for a new download.
    ///
    /// An explicit directory that is not the configured download directory wins
    /// (Add Advanced / Browse). Otherwise, when organize is on, append the
    /// type folder for `filename` (or stay at the root if that type is disabled).
    pub fn resolve_save_directory(&self, filename: &str, explicit_dir: Option<&Path>) -> PathBuf {
        if let Some(explicit) = explicit_dir {
            if !same_dir(explicit, &self.download_directory) {
                return explicit.to_path_buf();
            }
        }
        if !self.organize_by_file_type {
            return self.download_directory.clone();
        }
        let kind = FileTypeKind::from_filename(filename);
        match self.category_folders.folder_if_enabled(kind) {
            Some(name) => self.download_directory.join(name),
            None => self.download_directory.clone(),
        }
    }

    /// Reset all appearance fields to defaults (keeps download prefs).
    pub fn reset_appearance(&mut self) {
        let defaults = Settings::default();
        self.theme = defaults.theme;
        self.accent_preset = defaults.accent_preset;
        self.accent_hue = defaults.accent_hue;
        self.accent_saturation = defaults.accent_saturation;
        self.accent_lightness = defaults.accent_lightness;
        self.noise_intensity = defaults.noise_intensity;
        self.window_transparency = defaults.window_transparency;
        self.backdrop_blur = defaults.backdrop_blur;
        self.ui_density = defaults.ui_density;
        self.corner_radius = defaults.corner_radius;
        self.reduce_motion = defaults.reduce_motion;
        self.vignette_intensity = defaults.vignette_intensity;
        self.progress_style = defaults.progress_style;
    }

    /// Factory defaults for every preference except window geometry and download folder.
    ///
    /// Used by Settings → Reset defaults (draft only until Save).
    pub fn reset_to_defaults_preserving_layout_and_dir(&mut self) {
        let keep_dir = self.download_directory.clone();
        let keep_layout = self.window_layout.clone();
        *self = Settings::default();
        self.download_directory = keep_dir;
        self.window_layout = keep_layout;
        self.sanitize_appearance();
    }
}

pub fn default_download_directory() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_clamps_transparency_and_noise() {
        let mut s = Settings::default();
        s.window_transparency = 10;
        s.noise_intensity = 200;
        s.accent_saturation = 150.0;
        s.accent_lightness = -10.0;
        s.sanitize_appearance();
        assert_eq!(s.window_transparency, 10);
        s.window_transparency = 200;
        s.sanitize_appearance();
        assert_eq!(s.window_transparency, MAX_WINDOW_TRANSPARENCY);
        assert_eq!(s.noise_intensity, MAX_NOISE_INTENSITY);
        assert_eq!(s.accent_saturation, 100.0);
        assert_eq!(s.accent_lightness, 0.0);
    }

    #[test]
    fn legacy_json_without_appearance_fields_deserializes() {
        let json = r#"{
            "downloadDirectory": "C:/dl",
            "maxConcurrentDownloads": 2,
            "autoRetryAttempts": 3,
            "speedLimitKibPerSecond": 0,
            "theme": "dark"
        }"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.theme, AppTheme::Dark);
        assert_eq!(s.accent_preset, AccentPreset::Default);
        assert_eq!(s.window_transparency, 0); // solid by default
        assert_eq!(s.noise_intensity, 0);
        assert!(!s.backdrop_blur);
        assert_eq!(s.ui_density, UiDensity::Comfortable);
        assert_eq!(s.corner_radius, CornerRadiusScale::Default);
        assert!(!s.reduce_motion);
        assert_eq!(s.vignette_intensity, 0);
        assert_eq!(s.progress_style, ProgressStyle::Solid);
        assert_eq!(s.window_layout, WindowLayout::default());
        assert_eq!(s.update_channel, UpdateChannel::Stable);
        // New system prefs: close-to-tray defaults on for download-manager UX.
        assert!(s.close_to_tray);
        assert!(!s.launch_at_startup);
        assert!(!s.startup_minimized);
        assert_eq!(s.os_notify_mode, OsNotifyMode::WhenHiddenToTray);
        assert!(s.notify_on_complete);
        assert!(s.notify_on_fail);
        assert!(!s.clipboard_watch_enabled);
        assert!(s.organize_by_file_type);
        assert!(s.sidebar_library_expanded);
        assert_eq!(s.category_folders.audio.name, "Audio");
        // Multi limits default when legacy JSON omits those keys.
        assert_eq!(s.multi_max_segments, 8);
        assert_eq!(s.multi_min_bytes, 5 * 1024 * 1024);
        assert_eq!(s.max_total_connections, 32);
        assert_eq!(s.max_connections_per_host, 8);
        assert!(s.multi_connection_enabled);
    }

    #[test]
    fn sanitize_download_limits_clamps() {
        let mut s = Settings::default();
        s.max_concurrent_downloads = 0;
        s.auto_retry_attempts = 500;
        s.multi_max_segments = 99;
        s.multi_min_bytes = 100;
        s.max_total_connections = 0;
        s.max_connections_per_host = 0;
        s.sanitize_download_limits();
        assert_eq!(s.max_concurrent_downloads, 1);
        assert_eq!(s.auto_retry_attempts, 100);
        assert_eq!(s.multi_max_segments, 16);
        assert_eq!(s.multi_min_bytes, 1024 * 1024);
        assert_eq!(s.max_total_connections, 1);
        assert_eq!(s.max_connections_per_host, 1);

        // Per-host is clamped to total.
        s.max_total_connections = 4;
        s.max_connections_per_host = 32;
        s.sanitize_download_limits();
        assert_eq!(s.max_total_connections, 4);
        assert_eq!(s.max_connections_per_host, 4);
    }

    #[test]
    fn window_layout_sanitize_clamps_and_defaults() {
        let mut layout = WindowLayout {
            width: 100.0,
            height: f32::NAN,
            x: Some(f32::INFINITY),
            y: Some(40.0),
            maximized: true,
        };
        layout.sanitize();
        assert_eq!(layout.width, MIN_WINDOW_WIDTH);
        assert_eq!(layout.height, DEFAULT_WINDOW_HEIGHT);
        assert!(layout.x.is_none());
        assert!(layout.y.is_none());
        assert!(layout.maximized);
    }

    #[test]
    fn density_and_radius_tokens() {
        assert!(UiDensity::Compact.row_h() < UiDensity::Comfortable.row_h());
        assert!(UiDensity::Compact.sidebar_w() < UiDensity::Comfortable.sidebar_w());
        let (sharp, _) = CornerRadiusScale::Sharp.radii();
        let (soft, soft_lg) = CornerRadiusScale::Soft.radii();
        assert!(sharp < soft);
        assert!(soft < soft_lg);
    }

    #[test]
    fn sanitize_clamps_vignette() {
        let mut s = Settings::default();
        s.vignette_intensity = 200;
        s.sanitize_appearance();
        assert_eq!(s.vignette_intensity, MAX_VIGNETTE_INTENSITY);
    }

    #[test]
    fn reset_to_defaults_preserves_layout_and_dir() {
        let keep_dir = PathBuf::from("C:/my/custom/downloads");
        let keep_layout = WindowLayout {
            width: 1400.0,
            height: 900.0,
            x: Some(12.0),
            y: Some(34.0),
            maximized: true,
        };

        let mut s = Settings::default();
        s.download_directory = keep_dir.clone();
        s.window_layout = keep_layout.clone();
        // Mutate fields that must return to defaults (including custom accent HSL).
        s.max_concurrent_downloads = 9;
        s.auto_retry_attempts = 1;
        s.speed_limit_kib_per_second = 512;
        s.theme = AppTheme::Dark;
        s.accent_preset = AccentPreset::Custom;
        s.accent_hue = 12.0;
        s.accent_saturation = 90.0;
        s.accent_lightness = 40.0;
        s.noise_intensity = 40;
        s.window_transparency = 25;
        s.backdrop_blur = true;
        s.ui_density = UiDensity::Compact;
        s.corner_radius = CornerRadiusScale::Soft;
        s.reduce_motion = true;
        s.vignette_intensity = 30;
        s.progress_style = ProgressStyle::Glow;
        s.sort_column = SortColumn::Name;
        s.sort_direction = SortDirection::Asc;
        s.close_to_tray = false;
        s.launch_at_startup = true;
        s.startup_minimized = true;
        s.os_notify_mode = OsNotifyMode::Always;
        s.notify_on_complete = false;
        s.notify_on_fail = false;
        s.clipboard_watch_enabled = true;
        s.organize_by_file_type = false;
        s.sidebar_library_expanded = false;
        s.category_folders.audio.name = "Music".into();
        s.category_folders.programs.enabled = false;
        s.extension.enabled = false;
        s.extension.excluded_hosts = vec!["example.com".into()];

        s.reset_to_defaults_preserving_layout_and_dir();

        // Expected = full Default with only preserve fields overlaid (catches new fields).
        let mut expected = Settings::default();
        expected.download_directory = keep_dir;
        expected.window_layout = keep_layout;
        assert_eq!(s, expected);
        // Explicit HSL guards if someone later rewrites the helper field-by-field.
        let defaults = Settings::default();
        assert_eq!(s.accent_hue, defaults.accent_hue);
        assert_eq!(s.accent_saturation, defaults.accent_saturation);
        assert_eq!(s.accent_lightness, defaults.accent_lightness);
        assert_eq!(s.accent_preset, defaults.accent_preset);
        assert!(s.organize_by_file_type);
        assert_eq!(s.category_folders, CategoryFolders::default());
    }

    #[test]
    fn reset_to_defaults_is_idempotent_when_already_default() {
        let keep_dir = PathBuf::from("/tmp/kept");
        let keep_layout = WindowLayout {
            width: 1300.0,
            height: 800.0,
            x: None,
            y: None,
            maximized: false,
        };
        let mut s = Settings::default();
        s.download_directory = keep_dir.clone();
        s.window_layout = keep_layout.clone();
        s.reset_to_defaults_preserving_layout_and_dir();

        let mut expected = Settings::default();
        expected.download_directory = keep_dir;
        expected.window_layout = keep_layout;
        assert_eq!(s, expected);
    }

    #[test]
    fn same_dir_normalizes_slashes_and_case() {
        assert!(same_dir(
            Path::new(r"C:\Users\You\Downloads"),
            Path::new(r"c:/Users/You/Downloads/")
        ));
        assert!(!same_dir(
            Path::new(r"C:\Users\You\Downloads"),
            Path::new(r"C:\Users\You\Downloads\Audio")
        ));
    }

    #[test]
    fn sanitize_category_folder_rejects_traversal() {
        assert_eq!(sanitize_category_folder_name("..", "Audio"), "Audio");
        assert_eq!(sanitize_category_folder_name("a/b", "Audio"), "b");
        assert_eq!(sanitize_category_folder_name(r"x\y", "Audio"), "y");
        assert_eq!(sanitize_category_folder_name("Music", "Audio"), "Music");
        assert_eq!(sanitize_category_folder_name("   ", "Audio"), "Audio");
    }

    #[test]
    fn resolve_save_directory_routes_when_organize_on() {
        let mut s = Settings::default();
        s.download_directory = PathBuf::from(r"C:\dl");
        assert_eq!(
            s.resolve_save_directory("song.mp3", None),
            PathBuf::from(r"C:\dl\Audio")
        );
        assert_eq!(
            s.resolve_save_directory("clip.mp4", None),
            PathBuf::from(r"C:\dl\Video")
        );
        assert_eq!(
            s.resolve_save_directory("notes", None),
            PathBuf::from(r"C:\dl\Other")
        );
        assert_eq!(
            s.resolve_save_directory("pack.zip", Some(Path::new(r"C:\dl"))),
            PathBuf::from(r"C:\dl\Compressed")
        );
    }

    #[test]
    fn resolve_save_directory_explicit_other_dir_wins() {
        let mut s = Settings::default();
        s.download_directory = PathBuf::from(r"C:\dl");
        assert_eq!(
            s.resolve_save_directory("song.mp3", Some(Path::new(r"D:\other"))),
            PathBuf::from(r"D:\other")
        );
    }

    #[test]
    fn resolve_save_directory_respects_organize_off_and_disabled_type() {
        let mut s = Settings::default();
        s.download_directory = PathBuf::from(r"C:\dl");
        s.organize_by_file_type = false;
        assert_eq!(
            s.resolve_save_directory("song.mp3", None),
            PathBuf::from(r"C:\dl")
        );
        s.organize_by_file_type = true;
        s.category_folders.audio.enabled = false;
        assert_eq!(
            s.resolve_save_directory("song.mp3", None),
            PathBuf::from(r"C:\dl")
        );
        s.category_folders.audio.name = "Music".into();
        s.category_folders.audio.enabled = true;
        assert_eq!(
            s.resolve_save_directory("song.mp3", None),
            PathBuf::from(r"C:\dl\Music")
        );
    }
}
