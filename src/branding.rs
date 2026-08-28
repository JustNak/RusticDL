//! Product branding constants for RusticDL.
//! User-facing names (what Windows, the UI, and installers should show):
//! - [`APP_NAME`] — main desktop app (window title, Start Menu, About, taskbar)
//! Technical identifiers (crate names, pipe path, registry keys) stay lowercase
//! `rusticdl` and must not be used as display names.

/// User-facing product name for the main desktop application.
pub const APP_NAME: &str = "RusticDL";

/// Built-in app version (About, update checks).
///
/// Defaults to `Cargo.toml`. Nightly CI sets `RUSTICDL_VERSION` so the binary
/// reports `X.Y.Z-nightly.YYYYMMDDHHMMSS` without rewriting the crate version.
pub const APP_VERSION: &str = env!("RUSTICDL_VERSION");

/// User-facing name for the dedicated self-update helper process.
pub const UPDATER_NAME: &str = "RusticDL Updater";

/// On-disk updater binary name (next to `rusticdl.exe` in the install dir).
pub const UPDATER_EXE_NAME: &str = "rusticdl-updater.exe";

/// About / subtitle line.
pub const APP_TAGLINE: &str = "Local-first download manager";

/// GitHub repository owner (update feed + release links).
pub const GITHUB_OWNER: &str = "JustNak";

/// GitHub repository name (update feed + release links).
pub const GITHUB_REPO: &str = "RusticDL";

/// NSIS installer asset name published on every GitHub Release.
pub const SETUP_ASSET_NAME: &str = "RusticDL-windows-x64-setup.exe";

/// Windows named-pipe path used by the native messaging host.
pub const PIPE_NAME: &str = r"\\.\pipe\rusticdl.v1";

/// AppUserModelID for taskbar / Start Menu identity (must match installer shortcuts).
#[allow(dead_code)] // used on Windows process bootstrap
pub const APP_USER_MODEL_ID: &str = "com.rusticdl.app";

/// App data folder under `%APPDATA%` / XDG.
pub const APP_DATA_DIR_NAME: &str = "RusticDL";

/// Relative path to the multi-size Windows icon (from assets root).
pub const APP_ICON_ICO: &str = "brand/icon.ico";

/// Dark-theme title-bar / chrome mark (light glyph on dark field).
pub const APP_LOGO_DARK: &str = "brand/logo.png";

/// Light-theme title-bar / chrome mark (dark glyph on light field).
pub const APP_LOGO_LIGHT: &str = "brand/logo-light.png";
