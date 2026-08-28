pub fn extract_http_urls(raw: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut rest = raw.trim();

    while !rest.is_empty() {
        let start = match (rest.find("https://"), rest.find("http://")) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };

        rest = &rest[start..];
        let after_scheme = if rest.starts_with("https://") { 8 } else { 7 };

        let next_scheme = match (
            rest[after_scheme..]
                .find("https://")
                .map(|i| i + after_scheme),
            rest[after_scheme..]
                .find("http://")
                .map(|i| i + after_scheme),
        ) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        let end = if let Some(ns) = next_scheme {
            rest[..ns].find(char::is_whitespace).unwrap_or(ns)
        } else {
            rest.find(char::is_whitespace).unwrap_or(rest.len())
        };

        let candidate = rest[..end]
            .trim()
            .trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | ')' | ']' | '"' | '\''));

        if (candidate.starts_with("http://") || candidate.starts_with("https://"))
            && url::Url::parse(candidate).is_ok()
        {
            if urls.last().map(String::as_str) != Some(candidate) {
                urls.push(candidate.to_string());
            }
        }

        rest = rest[end..].trim_start();
    }

    urls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_newlines_and_spaces() {
        let raw = "https://a.example/f\nhttps://b.example/g  https://c.example/h";
        assert_eq!(
            extract_http_urls(raw),
            vec![
                "https://a.example/f",
                "https://b.example/g",
                "https://c.example/h",
            ]
        );
    }

    #[test]
    fn splits_glued_schemes_like_cdn_paste() {
        let raw = "https://nexus-198.apac.tb-cdn.pw/dld/22caaf6f-a71b-4382-9e3b-51cf7f7cd8b1?token=ea24bba1-eba0-4a5d-92cd-bbe07d59b864https://nexus-198.apac.tb-cdn.pw/dld/b22eb924";
        let urls = extract_http_urls(raw);
        assert_eq!(urls.len(), 2);
        assert_eq!(
            urls[0],
            "https://nexus-198.apac.tb-cdn.pw/dld/22caaf6f-a71b-4382-9e3b-51cf7f7cd8b1?token=ea24bba1-eba0-4a5d-92cd-bbe07d59b864"
        );
        assert_eq!(urls[1], "https://nexus-198.apac.tb-cdn.pw/dld/b22eb924");
    }

    #[test]
    fn single_clean_url() {
        assert_eq!(
            extract_http_urls("  https://example.com/file.zip  "),
            vec!["https://example.com/file.zip"]
        );
    }
}
