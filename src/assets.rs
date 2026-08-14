use std::borrow::Cow;
use std::fs;
use std::path::PathBuf;

use gpui::{AssetSource, Result, SharedString};
use include_dir::{include_dir, Dir};

/// SVG icons baked into the binary (icon fallback when loose `assets/` is missing).
static EMBEDDED_ICONS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets/icons");

/// Title-bar logos. Paths stay `brand/logo.png` / `brand/logo-light.png`.
static EMBEDDED_LOGO_DARK: &[u8] = include_bytes!("../assets/brand/logo.png");
static EMBEDDED_LOGO_LIGHT: &[u8] = include_bytes!("../assets/brand/logo-light.png");

/// Loads SVG/icons and other static files from the project `assets/` directory.
///
/// Resolution order:
/// 1. `<exe-dir>/assets/` — release installs (cargo-packager copies `assets`)
/// 2. `CARGO_MANIFEST_DIR/assets` — local `cargo run` / `cargo build` from the repo
/// 3. Compile-time embedded icons + title-bar logos — always available as a fallback
pub struct Assets {
    base: PathBuf,
}

impl Assets {
    pub fn new() -> Self {
        // Prefer assets next to the executable (release installs), fall back to
        // the crate-local assets/ used during development.
        let exe_side = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("assets")));
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");

        let base = if exe_side.as_ref().is_some_and(|p| p.exists()) {
            exe_side.unwrap()
        } else {
            manifest
        };

        Self { base }
    }

    fn load_embedded(path: &str) -> Option<Cow<'static, [u8]>> {
        if let Some(rest) = path.strip_prefix("icons/") {
            return EMBEDDED_ICONS
                .get_file(rest)
                .map(|file| Cow::Borrowed(file.contents()));
        }
        match path {
            "brand/logo.png" => Some(Cow::Borrowed(EMBEDDED_LOGO_DARK)),
            "brand/logo-light.png" => Some(Cow::Borrowed(EMBEDDED_LOGO_LIGHT)),
            _ => None,
        }
    }

    fn list_embedded(path: &str) -> Vec<SharedString> {
        if path.is_empty() || path == "." {
            return ["icons", "brand"]
                .into_iter()
                .map(SharedString::from)
                .collect();
        }
        if path == "brand" {
            return ["logo.png", "logo-light.png"]
                .into_iter()
                .map(SharedString::from)
                .collect();
        }
        let dir = if path == "icons" {
            Some(&EMBEDDED_ICONS)
        } else {
            path.strip_prefix("icons/")
                .and_then(|rest| EMBEDDED_ICONS.get_dir(rest))
        };
        let Some(dir) = dir else {
            return Vec::new();
        };
        dir.entries()
            .iter()
            .filter_map(|entry| {
                let name = entry.path().file_name()?.to_str()?;
                Some(SharedString::from(name.to_owned()))
            })
            .collect()
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let full = self.base.join(path);
        match fs::read(&full) {
            Ok(bytes) => Ok(Some(Cow::Owned(bytes))),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::load_embedded(path)),
            Err(err) => Err(err.into()),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let full = self.base.join(path);
        match fs::read_dir(&full) {
            Ok(entries) => Ok(entries
                .filter_map(|entry| {
                    entry
                        .ok()
                        .and_then(|e| e.file_name().into_string().ok())
                        .map(SharedString::from)
                })
                .collect()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::list_embedded(path)),
            Err(err) => Err(err.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AssetSource;

    #[test]
    fn embedded_icons_include_nav_and_empty_state_svgs() {
        // These are the icons the empty-state / sidebar render on first launch.
        for path in [
            "icons/inbox.svg",
            "icons/arrow-down.svg",
            "icons/circle-check.svg",
            "icons/circle-x.svg",
            "icons/settings.svg",
            "icons/plus.svg",
            "icons/empty-box.svg",
        ] {
            let bytes = Assets::load_embedded(path)
                .unwrap_or_else(|| panic!("missing embedded asset: {path}"));
            assert!(
                !bytes.is_empty(),
                "embedded asset {path} should not be empty"
            );
            let text = std::str::from_utf8(&bytes).expect("svg is utf-8");
            assert!(
                text.contains("<svg"),
                "embedded asset {path} should look like an SVG"
            );
        }
    }

    #[test]
    fn embedded_brand_logos_include_theme_variants() {
        for path in ["brand/logo.png", "brand/logo-light.png"] {
            let bytes = Assets::load_embedded(path)
                .unwrap_or_else(|| panic!("missing embedded asset: {path}"));
            assert!(
                !bytes.is_empty(),
                "embedded asset {path} should not be empty"
            );
            // PNG magic
            assert_eq!(&bytes[..4], b"\x89PNG", "{path} should be a PNG");
        }
    }

    #[test]
    fn unused_assets_are_not_embedded() {
        assert!(Assets::load_embedded("noise.png").is_none());
        assert!(Assets::load_embedded("brand/masters/icon-master-1024.png").is_none());
        assert!(Assets::load_embedded("brand/masters/icon-master-light-1024.png").is_none());
    }

    #[test]
    fn asset_source_load_falls_back_to_embedded() {
        // Point base at a path that does not exist so disk load fails.
        let assets = Assets {
            base: PathBuf::from("__no_such_assets_dir__"),
        };
        let loaded = assets
            .load("icons/inbox.svg")
            .expect("load should not error")
            .expect("embedded fallback should find inbox.svg");
        assert!(!loaded.is_empty());
    }
}
