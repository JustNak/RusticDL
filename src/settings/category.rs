use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::download::FileTypeKind;

fn default_true() -> bool {
    true
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_category_folder_rejects_traversal() {
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(sanitize_category_folder_name("..", "Audio"), "Audio");
        assert_eq!(sanitize_category_folder_name("a/b", "Audio"), "b");
        assert_eq!(
            sanitize_category_folder_name(&format!("x{sep}y"), "Audio"),
            "y"
        );
        assert_eq!(sanitize_category_folder_name("Music", "Audio"), "Music");
        assert_eq!(sanitize_category_folder_name("   ", "Audio"), "Audio");
    }
}
