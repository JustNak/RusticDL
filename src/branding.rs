//! Product branding constants for RusticDL.

/// User-facing product name.
pub const APP_NAME: &str = "RusticDL";

/// About / subtitle line.
pub const APP_TAGLINE: &str = "Local-first download manager";

/// Windows named-pipe path used by the native messaging host.
pub const PIPE_NAME: &str = r"\\.\pipe\rusticdl.v1";

/// Native messaging host registry / manifest name.
#[allow(dead_code)] // referenced by scripts / packaging docs
pub const NATIVE_HOST_NAME: &str = "com.rusticdl.native_host";

/// App data folder under `%APPDATA%` / XDG.
pub const APP_DATA_DIR_NAME: &str = "RusticDL";

/// Relative path to the multi-size Windows icon (from assets root).
pub const APP_ICON_ICO: &str = "brand/icon.ico";

/// Relative path to the square brand mark PNG.
#[allow(dead_code)] // available for future tray / about UI image
pub const APP_ICON_PNG: &str = "brand/icon-256.png";
