use std::path::{Path, PathBuf};

/// Compare download directories ignoring slash style, trailing separators, and ASCII case.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    fn key(p: &Path) -> String {
        let s = p.to_string_lossy().replace('/', "\\");
        s.trim_end_matches('\\').to_ascii_lowercase()
    }
    !a.as_os_str().is_empty() && !b.as_os_str().is_empty() && key(a) == key(b)
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
}
