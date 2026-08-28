#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileTypeKind {
    Video,
    Audio,
    Compressed,
    Images,
    Documents,
    Programs,
    Other,
}

impl FileTypeKind {
    pub const COUNT: usize = 7;

    pub const ALL: [Self; Self::COUNT] = [
        Self::Video,
        Self::Audio,
        Self::Compressed,
        Self::Images,
        Self::Documents,
        Self::Programs,
        Self::Other,
    ];

    pub fn from_filename(filename: &str) -> Self {
        let ext = std::path::Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "mkv" | "mp4" | "avi" | "webm" | "mov" | "m4v" | "wmv" | "flv" | "mpeg" | "mpg"
            | "ts" | "m2ts" => Self::Video,
            "mp3" | "flac" | "wav" | "aac" | "m4a" | "ogg" | "opus" | "wma" | "aiff" => Self::Audio,
            "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "lz4" | "zst" | "cab"
            | "iso" => Self::Compressed,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "tif" | "tiff"
            | "heic" | "avif" => Self::Images,
            "pdf" | "doc" | "docx" | "txt" | "md" | "rtf" | "odt" | "xls" | "xlsx" | "ppt"
            | "pptx" | "csv" | "json" | "xml" | "html" | "htm" | "epub" | "mobi" => Self::Documents,
            "exe" | "msi" | "bat" | "cmd" | "com" | "appx" | "msix" | "dll" | "sys" | "scr"
            | "ps1" | "sh" | "bin" | "run" | "app" | "dmg" | "pkg" | "deb" | "rpm" => {
                Self::Programs
            }
            _ => Self::Other,
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Video => 0,
            Self::Audio => 1,
            Self::Compressed => 2,
            Self::Images => 3,
            Self::Documents => 4,
            Self::Programs => 5,
            Self::Other => 6,
        }
    }

    pub fn label(self) -> &'static str {
        self.default_folder_name()
    }

    pub fn default_folder_name(self) -> &'static str {
        match self {
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Compressed => "Compressed",
            Self::Images => "Images",
            Self::Documents => "Documents",
            Self::Programs => "Programs",
            Self::Other => "Other",
        }
    }

    pub fn icon_path(self) -> &'static str {
        match self {
            Self::Video => "icons/file-video.svg",
            Self::Audio => "icons/file-audio.svg",
            Self::Compressed => "icons/file-archive.svg",
            Self::Images => "icons/file-image.svg",
            Self::Documents => "icons/file-text.svg",
            Self::Programs => "icons/file-code.svg",
            Self::Other => "icons/file.svg",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FileTypeKind;

    #[test]
    fn from_common_extensions() {
        assert_eq!(FileTypeKind::from_filename("clip.mp4"), FileTypeKind::Video);
        assert_eq!(FileTypeKind::from_filename("song.mp3"), FileTypeKind::Audio);
        assert_eq!(
            FileTypeKind::from_filename("pack.zip"),
            FileTypeKind::Compressed
        );
        assert_eq!(FileTypeKind::from_filename("notes"), FileTypeKind::Other);
        assert_eq!(
            FileTypeKind::from_filename("movie.mkv.zip"),
            FileTypeKind::Compressed
        );
        assert_eq!(
            FileTypeKind::from_filename("TRACK.FLAC"),
            FileTypeKind::Audio
        );
    }

    #[test]
    fn labels_match_default_folders() {
        assert_eq!(FileTypeKind::Compressed.label(), "Compressed");
        assert_eq!(FileTypeKind::Other.label(), "Other");
        assert_eq!(FileTypeKind::Images.default_folder_name(), "Images");
    }

    #[test]
    fn all_indexes_are_dense() {
        for (i, kind) in FileTypeKind::ALL.iter().enumerate() {
            assert_eq!(kind.index(), i);
        }
    }
}
