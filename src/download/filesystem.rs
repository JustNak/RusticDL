use percent_encoding::percent_decode_str;
use std::path::{Path, PathBuf};
use tokio::fs;

pub async fn ensure_parent_directory(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Download path has no parent directory.".to_string())?;

    fs::create_dir_all(parent)
        .await
        .map_err(|error| format!("Could not create download directory: {error}"))
}

pub async fn metadata_len(path: &Path) -> Option<u64> {
    fs::metadata(path).await.ok().map(|metadata| metadata.len())
}

pub async fn move_to_final_path(temp_path: &Path, target_path: &Path) -> Result<PathBuf, String> {
    let final_path = allocate_final_path(target_path).await?;

    fs::rename(temp_path, &final_path)
        .await
        .map_err(|error| format!("Could not finalize downloaded file: {error}"))?;

    Ok(final_path)
}

pub async fn allocate_final_path(target_path: &Path) -> Result<PathBuf, String> {
    if !target_path.exists() {
        return Ok(target_path.to_path_buf());
    }

    let stem = target_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = target_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let parent = target_path
        .parent()
        .ok_or_else(|| "Download path has no parent directory.".to_string())?;

    for index in 1..10_000 {
        let candidate = parent.join(format!("{stem} ({index}){extension}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err("Could not allocate a unique final download path.".into())
}

pub async fn remove_partial(path: &Path) {
    let _ = fs::remove_file(path).await;
}

pub fn parse_content_disposition_filename(header_value: &str) -> Option<String> {
    if let Some(encoded) = header_value
        .split(';')
        .find_map(|segment| segment.trim().strip_prefix("filename*="))
    {
        let sanitized = decode_content_disposition_filename(encoded);
        if !sanitized.is_empty() {
            return Some(sanitized);
        }
    }

    header_value
        .split(';')
        .find_map(|segment| segment.trim().strip_prefix("filename="))
        .map(decode_content_disposition_filename)
        .filter(|value| !value.is_empty())
}

pub fn decode_content_disposition_filename(value: &str) -> String {
    let value = value.trim().trim_matches('"').trim();
    let encoded = value.split("''").nth(1).unwrap_or(value);
    let decoded = percent_decode_str(encoded).decode_utf8_lossy();
    sanitize_filename(decoded.trim())
}

pub fn derive_filename_from_url(raw_url: &str) -> Option<String> {
    let parsed = url::Url::parse(raw_url).ok()?;
    let candidate = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())?;
    let decoded = percent_decode_str(candidate).decode_utf8_lossy();
    let sanitized = sanitize_filename(&decoded);
    if sanitized.is_empty() || sanitized == "download.bin" {
        None
    } else {
        Some(sanitized)
    }
}

pub fn sanitize_filename(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            character if character.is_control() => '_',
            _ => character,
        })
        .collect();

    let mut sanitized = sanitized.trim().trim_matches('.').trim().to_string();
    if is_windows_reserved_filename(&sanitized) {
        sanitized.push('_');
    }
    if sanitized.is_empty() {
        sanitized = "download.bin".into();
    }
    sanitized
}

pub fn is_windows_reserved_filename(filename: &str) -> bool {
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(filename)
        .to_ascii_uppercase();

    matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

pub fn temp_path_for(target: &Path) -> PathBuf {
    let mut path = target.as_os_str().to_os_string();
    path.push(".part");
    PathBuf::from(path)
}

pub fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.trim();
    let range_and_total = value.strip_prefix("bytes ")?;
    let (range, total) = range_and_total.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_unsafe_names() {
        assert_eq!(sanitize_filename("a/b\\c:d?.zip"), "a_b_c_d_.zip");
    }

    #[test]
    fn parses_content_disposition() {
        let name =
            parse_content_disposition_filename("attachment; filename=\"report (final).pdf\"")
                .unwrap();
        assert_eq!(name, "report (final).pdf");
    }

    #[test]
    fn derives_filename_from_url() {
        let name = derive_filename_from_url("https://cdn.example.com/files/hello%20world.iso?x=1")
            .unwrap();
        assert_eq!(name, "hello world.iso");
    }

    #[test]
    fn parses_content_range_header() {
        let (start, end, total) = parse_content_range("bytes 100-199/1000").unwrap();
        assert_eq!((start, end, total), (100, 199, 1000));
    }
}
