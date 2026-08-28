
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::urls::extract_http_urls;

pub const MAX_DROP_FILE_BYTES: u64 = 1_048_576;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DropFilesSummary {
    pub urls: Vec<String>,
    pub skipped: usize,
    pub errors: usize,
}

pub fn extract_urls_from_dropped_paths(paths: &[PathBuf]) -> DropFilesSummary {
    let mut summary = DropFilesSummary::default();
    for path in paths {
        match urls_from_dropped_path(path) {
            Ok(found) => {
                for url in found {
                    if !summary.urls.iter().any(|u| u == &url) {
                        summary.urls.push(url);
                    }
                }
            }
            Err(DropPathError::Skipped) => summary.skipped += 1,
            Err(DropPathError::Io) => summary.errors += 1,
        }
    }
    summary
}

#[derive(Debug)]
enum DropPathError {
    Skipped,
    Io,
}

fn urls_from_dropped_path(path: &Path) -> Result<Vec<String>, DropPathError> {
    let meta = fs::metadata(path).map_err(|_| DropPathError::Io)?;
    if meta.is_dir() {
        return Err(DropPathError::Skipped);
    }
    if meta.len() > MAX_DROP_FILE_BYTES {
        return Err(DropPathError::Skipped);
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());

    let text_like = matches!(
        ext.as_deref(),
        Some("txt" | "csv" | "url" | "webloc" | "md" | "log" | "html" | "htm" | "json")
    );

    let mut file = fs::File::open(path).map_err(|_| DropPathError::Io)?;
    let mut buf = Vec::with_capacity(meta.len().min(MAX_DROP_FILE_BYTES) as usize);
    file.by_ref()
        .take(MAX_DROP_FILE_BYTES)
        .read_to_end(&mut buf)
        .map_err(|_| DropPathError::Io)?;

    if !text_like && looks_binary(&buf) {
        return Err(DropPathError::Skipped);
    }

    let contents = match String::from_utf8(buf) {
        Ok(s) => s,
        Err(_) => return Err(DropPathError::Skipped),
    };

    if ext.as_deref() == Some("url") {
        if let Some(url) = parse_windows_url_shortcut(&contents) {
            return Ok(vec![url]);
        }
    }

    Ok(extract_http_urls(&contents))
}

pub fn parse_windows_url_shortcut(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if line.len() < 5 {
            continue;
        }
        if line
            .as_bytes()
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case(b"URL="))
        {
            let value = line[4..].trim();
            if value.is_empty() {
                continue;
            }
            let urls = extract_http_urls(value);
            if let Some(u) = urls.into_iter().next() {
                return Some(u);
            }
        }
    }
    None
}

fn looks_binary(buf: &[u8]) -> bool {
    if buf.is_empty() {
        return false;
    }
    if buf.iter().any(|&b| b == 0) {
        return true;
    }
    let sample = &buf[..buf.len().min(512)];
    let non_text = sample
        .iter()
        .filter(|&&b| b < 0x09 || (b > 0x0d && b < 0x20) || b == 0x7f)
        .count();
    non_text * 10 > sample.len() // >10% control-ish → binary
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_url_shortcut_basic() {
        let raw = "[InternetShortcut]\r\nURL=https://cdn.example.com/a.zip\r\n";
        assert_eq!(
            parse_windows_url_shortcut(raw).as_deref(),
            Some("https://cdn.example.com/a.zip")
        );
    }

    #[test]
    fn parse_url_shortcut_case_insensitive_key() {
        let raw = "url=http://example.com/x\n";
        assert_eq!(
            parse_windows_url_shortcut(raw).as_deref(),
            Some("http://example.com/x")
        );
    }

    #[test]
    fn parse_url_shortcut_ignores_non_http() {
        assert!(parse_windows_url_shortcut("URL=file:///C:/local\n").is_none());
        assert!(parse_windows_url_shortcut("URL=magnet:?xt=urn:btih:abc\n").is_none());
    }

    #[test]
    fn extract_from_txt_file() {
        let dir = std::env::temp_dir().join(format!("rusticdl-drop-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("urls.txt");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "https://a.example/f").unwrap();
        writeln!(f, "not a url").unwrap();
        writeln!(f, "https://b.example/g").unwrap();
        drop(f);

        let summary = extract_urls_from_dropped_paths(&[path]);
        assert_eq!(
            summary.urls,
            vec!["https://a.example/f", "https://b.example/g"]
        );
        assert_eq!(summary.skipped, 0);
        assert_eq!(summary.errors, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_from_url_shortcut_file() {
        let dir = std::env::temp_dir().join(format!("rusticdl-drop-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("link.url");
        fs::write(
            &path,
            "[InternetShortcut]\r\nURL=https://example.com/file.bin\r\nIconIndex=0\r\n",
        )
        .unwrap();

        let summary = extract_urls_from_dropped_paths(&[path]);
        assert_eq!(summary.urls, vec!["https://example.com/file.bin"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_huge_and_binary() {
        let dir = std::env::temp_dir().join(format!("rusticdl-drop-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let huge = dir.join("huge.txt");
        {
            let mut f = fs::File::create(&huge).unwrap();
            let chunk = vec![b'a'; 64 * 1024];
            for _ in 0..((MAX_DROP_FILE_BYTES / chunk.len() as u64) + 2) {
                f.write_all(&chunk).unwrap();
            }
        }

        let bin = dir.join("blob.dat");
        fs::write(&bin, [0u8, 1, 2, 3, 4, 255, 0, 10]).unwrap();

        let summary = extract_urls_from_dropped_paths(&[huge, bin]);
        assert!(summary.urls.is_empty());
        assert_eq!(summary.skipped, 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedupes_across_files() {
        let dir = std::env::temp_dir().join(format!("rusticdl-drop-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        fs::write(&a, "https://same.example/x\n").unwrap();
        fs::write(&b, "https://same.example/x\nhttps://other.example/y\n").unwrap();

        let summary = extract_urls_from_dropped_paths(&[a, b]);
        assert_eq!(
            summary.urls,
            vec!["https://same.example/x", "https://other.example/y"]
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
