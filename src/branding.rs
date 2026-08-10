//! Product branding constants for RusticDL.
//!
//! User-facing names (what Windows, the UI, and installers should show):
//! - [`APP_NAME`] — main desktop app (window title, Start Menu, About, taskbar)
//! - [`BACKEND_NAME`] — native messaging host / bridge process (Task Manager
//!   background processes, Startup-style process lists, process overflow)
//!
//! Technical identifiers (crate names, pipe path, registry keys) stay lowercase
//! `rusticdl` and must not be used as display names.

/// User-facing product name for the main desktop application.
pub const APP_NAME: &str = "RusticDL";

/// User-facing name for the native messaging host (backend bridge process).
///
/// Shown by Windows for the host binary (FileDescription) and in host manifests.
#[allow(dead_code)] // kept as the single source of truth for packaging / docs
pub const BACKEND_NAME: &str = "RusticDL Backend";

/// About / subtitle line.
pub const APP_TAGLINE: &str = "Local-first download manager";

/// Windows named-pipe path used by the native messaging host.
pub const PIPE_NAME: &str = r"\\.\pipe\rusticdl.v1";

/// Native messaging host registry / manifest name (technical id, not display).
#[allow(dead_code)] // referenced by scripts / packaging docs
pub const NATIVE_HOST_NAME: &str = "com.rusticdl.native_host";

/// AppUserModelID for taskbar / Start Menu identity (must match installer shortcuts).
#[allow(dead_code)] // used on Windows process bootstrap
pub const APP_USER_MODEL_ID: &str = "com.rusticdl.app";

/// App data folder under `%APPDATA%` / XDG.
pub const APP_DATA_DIR_NAME: &str = "RusticDL";

/// Relative path to the multi-size Windows icon (from assets root).
pub const APP_ICON_ICO: &str = "brand/icon.ico";

/// Relative path to the square brand mark PNG.
#[allow(dead_code)] // available for future tray / about UI image
pub const APP_ICON_PNG: &str = "brand/icon-256.png";
