//! Persisted user preferences and related value types.
//!
//! Callers keep importing from `crate::settings::{…}`; this module is a facade
//! over window, appearance, sort, system, category, and the Settings aggregate.

mod appearance;
mod category;
mod model;
mod paths;
mod sort;
mod system;
mod window;

pub use appearance::{
    AccentPreset, AppTheme, CornerRadiusScale, ProgressStyle, UiDensity, MAX_NOISE_INTENSITY,
    MAX_VIGNETTE_INTENSITY, MAX_WINDOW_TRANSPARENCY, MIN_WINDOW_OPACITY,
};
pub use category::{sanitize_category_folder_name, CategoryFolder, CategoryFolders};
pub use model::Settings;
pub use paths::{default_download_directory, same_dir};
pub use sort::{SortColumn, SortDirection};
pub use system::{OsNotifyMode, UpdateChannel};
pub use window::{
    WindowLayout, DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH,
};
