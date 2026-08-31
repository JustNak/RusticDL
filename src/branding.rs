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

/// On-disk updater binary name (next to the app in the install dir).
#[cfg(windows)]
pub const UPDATER_EXE_NAME: &str = "rusticdl-updater.exe";
#[cfg(not(windows))]
pub const UPDATER_EXE_NAME: &str = "rusticdl-updater";

/// About / subtitle line.
pub const APP_TAGLINE: &str = "Local-first download manager";

/// GitHub repository owner (update feed + release links).
pub const GITHUB_OWNER: &str = "JustNak";

/// GitHub repository name (update feed + release links).
pub const GITHUB_REPO: &str = "RusticDL";

/// NSIS installer asset name published on every GitHub Release (Windows).
pub const SETUP_ASSET_NAME: &str = "RusticDL-windows-x64-setup.exe";

/// Linux tarball asset name published on GitHub Releases.
pub const LINUX_TARBALL_ASSET_NAME: &str = "RusticDL-linux-x64.tar.gz";

/// SHA-256 checksum list published next to release archives.
pub const CHECKSUMS_ASSET_NAME: &str = "SHA256SUMS";

/// GitHub asset the in-app updater downloads on this OS.
pub fn update_asset_name() -> &'static str {
    #[cfg(windows)]
    {
        SETUP_ASSET_NAME
    }
    #[cfg(target_os = "linux")]
    {
        LINUX_TARBALL_ASSET_NAME
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        SETUP_ASSET_NAME
    }
}

/// Native messaging host name (must match extension + JSON manifests).
pub const NATIVE_HOST_NAME: &str = "com.rusticdl.native_host";

/// On-disk native-host binary name (sibling of the desktop app).
#[cfg(windows)]
pub const NATIVE_HOST_BIN_NAME: &str = "rusticdl-native-host.exe";
#[cfg(not(windows))]
pub const NATIVE_HOST_BIN_NAME: &str = "rusticdl-native-host";

/// Windows named-pipe path used by the native messaging host.
#[allow(dead_code)] // referenced from the Windows IPC server
pub const PIPE_NAME: &str = r"\\.\pipe\rusticdl.v1";

/// Unix socket file name under `$XDG_RUNTIME_DIR` (or `/tmp/rusticdl-$UID`).
pub const UNIX_SOCKET_FILE: &str = "rusticdl.v1.sock";

/// Transport path the native host and desktop use to talk.
///
/// Windows: named pipe. Unix: `$XDG_RUNTIME_DIR/rusticdl.v1.sock`.
/// Override with `RUSTICDL_PIPE_PATH`.
pub fn ipc_transport_path() -> String {
    if let Ok(path) = std::env::var("RUSTICDL_PIPE_PATH") {
        if !path.trim().is_empty() {
            return path;
        }
    }
    #[cfg(windows)]
    {
        PIPE_NAME.to_string()
    }
    #[cfg(unix)]
    {
        unix_socket_path()
    }
    #[cfg(not(any(windows, unix)))]
    {
        PIPE_NAME.to_string()
    }
}

/// Absolute Unix socket path for the desktop ↔ native-host bridge.
#[cfg(unix)]
pub fn unix_socket_path() -> String {
    runtime_dir()
        .join(UNIX_SOCKET_FILE)
        .to_string_lossy()
        .into_owned()
}

/// Flock file held by the primary desktop instance.
#[cfg(unix)]
pub fn instance_lock_path() -> std::path::PathBuf {
    runtime_dir().join("rusticdl.lock")
}

#[cfg(unix)]
fn runtime_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return std::path::PathBuf::from(trimmed);
        }
    }
    std::path::PathBuf::from(format!("/tmp/rusticdl-{}", unix_uid()))
}

#[cfg(unix)]
fn unix_uid() -> u32 {
    unsafe { libc::getuid() }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_asset_name_is_platform_archive() {
        let name = update_asset_name();
        assert!(!name.is_empty());
        #[cfg(windows)]
        assert_eq!(name, SETUP_ASSET_NAME);
        #[cfg(target_os = "linux")]
        assert_eq!(name, LINUX_TARBALL_ASSET_NAME);
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_path_uses_xdg_runtime_dir() {
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        let path = unix_socket_path();
        match prev {
            Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        assert_eq!(path, "/run/user/1000/rusticdl.v1.sock");
        assert!(path.starts_with('/'));
        assert!(path.ends_with(UNIX_SOCKET_FILE));
    }

    #[cfg(unix)]
    #[test]
    fn ipc_transport_path_honors_override() {
        let prev = std::env::var_os("RUSTICDL_PIPE_PATH");
        std::env::set_var("RUSTICDL_PIPE_PATH", "/tmp/custom-rusticdl.sock");
        let path = ipc_transport_path();
        match prev {
            Some(value) => std::env::set_var("RUSTICDL_PIPE_PATH", value),
            None => std::env::remove_var("RUSTICDL_PIPE_PATH"),
        }
        assert_eq!(path, "/tmp/custom-rusticdl.sock");
    }
}
