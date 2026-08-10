use std::borrow::Cow;
use std::fs;
use std::path::PathBuf;

use gpui::{AssetSource, Result, SharedString};

/// Loads SVG/icons and other static files from the project `assets/` directory.
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
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let full = self.base.join(path);
        match fs::read(&full) {
            Ok(bytes) => Ok(Some(Cow::Owned(bytes))),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
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
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(err) => Err(err.into()),
        }
    }
}
