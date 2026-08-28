use gpui::rems;
use gpui_component::{highlighter::HighlightTheme, text::TextViewStyle, Theme};

/// `TextView::scrollable(true)` requires a definite parent height (not only max_h).
const WHATS_NEW_NOTES_MAX_H: f32 = 168.0;
const WHATS_NEW_NOTES_MIN_H: f32 = 64.0;
const WHATS_NEW_NOTES_LINE_H: f32 = 20.0;

pub(super) fn changelog_text_style(theme: &Theme) -> TextViewStyle {
    let mut style = TextViewStyle::default();
    style.paragraph_gap = rems(0.28);
    style.heading_base_font_size = theme.font_size;
    style.heading_font_size = Some(std::sync::Arc::new(|level, base| match level {
        1 | 2 => base * 1.05,
        _ => base,
    }));
    style.is_dark = theme.is_dark();
    style.highlight_theme = if theme.is_dark() {
        HighlightTheme::default_dark()
    } else {
        HighlightTheme::default_light()
    };
    style
}

pub(super) fn changelog_notes_height(markdown: &str) -> f32 {
    let lines = markdown
        .lines()
        .map(|line| {
            let n = line.chars().count();
            if n == 0 {
                0usize
            } else {
                (n / 68).saturating_add(1)
            }
        })
        .sum::<usize>()
        .max(1) as f32;
    (lines * WHATS_NEW_NOTES_LINE_H + 12.0).clamp(WHATS_NEW_NOTES_MIN_H, WHATS_NEW_NOTES_MAX_H)
}

pub(super) fn format_changelog_notes(notes: &str) -> String {
    let stripped = strip_html_comments(notes);
    let extracted = extract_changelog_body(&stripped);
    collapse_blank_lines(&extracted)
}

fn extract_changelog_body(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    if let Some(start) = lines.iter().position(|l| is_changelog_heading(l)) {
        let after = &lines[start + 1..];
        let end = after
            .iter()
            .position(|l| is_changelog_tail(l))
            .unwrap_or(after.len());
        return clean_changelog_lines(&after[..end]);
    }
    clean_changelog_lines(&strip_release_boilerplate(&lines))
}

fn clean_changelog_lines(lines: &[&str]) -> String {
    lines
        .iter()
        .filter(|line| !is_full_changelog_line(line) && !is_license_line(line))
        .map(|line| strip_github_attribution(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_release_boilerplate<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut skipping = false;
    for line in lines {
        if is_changelog_tail(line) || is_license_line(line) {
            continue;
        }
        if let Some(title) = heading_title(line) {
            if is_product_version_heading(title) {
                continue;
            }
            if is_boilerplate_heading(title) {
                skipping = true;
                continue;
            }
            skipping = false;
            out.push(*line);
            continue;
        }
        if skipping {
            if line.trim().is_empty() {
                skipping = false;
            }
            continue;
        }
        out.push(*line);
    }
    out
}

fn heading_title(line: &str) -> Option<&str> {
    let t = line.trim();
    if !t.starts_with('#') {
        return None;
    }
    Some(t.trim_start_matches('#').trim())
}

fn normalize_heading(title: &str) -> String {
    title
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_ascii_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_changelog_heading(line: &str) -> bool {
    heading_title(line).is_some_and(|title| {
        matches!(
            normalize_heading(title).as_str(),
            "whats changed" | "changelog" | "changes" | "whats new"
        )
    })
}

fn is_changelog_tail(line: &str) -> bool {
    if is_full_changelog_line(line) {
        return true;
    }
    heading_title(line).is_some_and(|title| normalize_heading(title) == "new contributors")
}

fn is_boilerplate_heading(title: &str) -> bool {
    matches!(
        normalize_heading(title).as_str(),
        "downloads" | "quick start" | "license" | "new contributors"
    )
}

fn is_product_version_heading(title: &str) -> bool {
    title
        .to_ascii_lowercase()
        .starts_with(&crate::branding::APP_NAME.to_ascii_lowercase())
}

fn is_full_changelog_line(line: &str) -> bool {
    let t = line.trim();
    if heading_title(t).is_some_and(|title| normalize_heading(title) == "full changelog") {
        return true;
    }
    t.to_ascii_lowercase().starts_with("**full changelog**")
}

fn is_license_line(line: &str) -> bool {
    let t = line.trim();
    if heading_title(t).is_some_and(|title| normalize_heading(title) == "license") {
        return true;
    }
    let lower = t.to_ascii_lowercase();
    lower.starts_with("**license:**") || lower.starts_with("license:")
}

fn strip_github_attribution(line: &str) -> &str {
    let mut search_from = 0;
    let mut last_start = None;
    while let Some(rel) = line[search_from..].find(" by @") {
        let start = search_from + rel;
        let after_user = start + " by @".len();
        if let Some(in_rel) = line[after_user..].find(" in ") {
            let after_in = after_user + in_rel + " in ".len();
            let rest = line[after_in..].trim();
            if rest.starts_with("http://")
                || rest.starts_with("https://")
                || rest.starts_with('#')
                || rest.starts_with("[#")
            {
                last_start = Some(start);
            }
        }
        search_from = start + 5;
    }
    last_start
        .map(|start| line[..start].trim_end())
        .unwrap_or(line)
}

fn collapse_blank_lines(src: &str) -> String {
    let mut lines: Vec<&str> = src.lines().collect();
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut out: Vec<&str> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push("");
            }
        } else {
            out.push(line);
        }
    }
    out.join("\n")
}

fn strip_html_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start + 4..].find("-->") {
            Some(rel) => rest = &rest[start + 4 + rel + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_changelog_preserves_markdown_inside_section() {
        let raw = r#"<!-- generated -->
## What's new

- Fix tray exit
- Add What's new dialog

### Notes
**Important** change with `inline`

```
code fence
```

[Open notes](https://example.com)
"#;
        let out = format_changelog_notes(raw);
        assert!(!out.contains("## What's new"));
        assert!(out.contains("- Fix tray exit"));
        assert!(out.contains("- Add What's new dialog"));
        assert!(out.contains("### Notes"));
        assert!(out.contains("**Important**"));
        assert!(out.contains("`inline`"));
        assert!(out.contains("```"));
        assert!(out.contains("code fence"));
        assert!(out.contains("[Open notes](https://example.com)"));
        assert!(!out.contains("<!--"));
        assert!(!out.contains("generated"));
    }

    #[test]
    fn format_changelog_extracts_whats_changed_from_github_release() {
        let raw = r#"## RusticDL 0.3.4-nightly.20260818125318

**Nightly** pre-release from `master` for testing.

### Downloads
| Asset | Contents |
| --- | --- |
| **RusticDL-windows-x64-setup.exe** | Recommended |

### Quick start
1. Download the setup
2. Run the installer

**License:** MIT

## What's Changed
* Fix Canvas Chromium 401 on extension downloads by @JustNak in https://github.com/JustNak/RusticDL/pull/124
* Render What's New changelog as themed markdown by @JustNak in https://github.com/JustNak/RusticDL/pull/125

## New Contributors
* @someone made their first contribution in https://github.com/JustNak/RusticDL/pull/1

**Full Changelog**: https://github.com/JustNak/RusticDL/compare/a...b
"#;
        let out = format_changelog_notes(raw);
        assert_eq!(
            out,
            "* Fix Canvas Chromium 401 on extension downloads\n* Render What's New changelog as themed markdown"
        );
        assert!(!out.contains("Downloads"));
        assert!(!out.contains("Quick start"));
        assert!(!out.contains("License"));
        assert!(!out.contains("Nightly"));
        assert!(!out.contains("Full Changelog"));
        assert!(!out.contains("New Contributors"));
        assert!(!out.contains("@JustNak"));
        assert!(!out.contains("github.com"));
    }

    #[test]
    fn format_changelog_strips_multiline_comments() {
        let out = format_changelog_notes("<!--\nRelease notes generated\n-->\n- item\n");
        assert_eq!(out, "- item");
    }

    #[test]
    fn format_changelog_empty_after_comments() {
        assert!(format_changelog_notes("<!-- only -->\n\n").is_empty());
    }

    #[test]
    fn format_changelog_keeps_rules_and_lists() {
        let out = format_changelog_notes("---\n- item\n***");
        assert!(out.contains("---"));
        assert!(out.contains("- item"));
        assert!(out.contains("***"));
    }

    #[test]
    fn format_changelog_fallback_drops_boilerplate_without_heading() {
        let raw = r#"## RusticDL v0.3.2

Local-first HTTP(S) download manager.

### Downloads
| Asset | Contents |
| setup.exe | installer |

- Keep this custom note
"#;
        let out = format_changelog_notes(raw);
        assert!(out.contains("Local-first HTTP(S) download manager."));
        assert!(out.contains("- Keep this custom note"));
        assert!(!out.contains("Downloads"));
        assert!(!out.contains("setup.exe"));
        assert!(!out.contains("## RusticDL"));
    }

    #[test]
    fn format_changelog_keeps_full_changelog_titled_item() {
        let raw = r#"## What's Changed
* Full changelog in What’s New
* Keep later items
**Full Changelog**: https://github.com/JustNak/RusticDL/compare/a...b
"#;
        let out = format_changelog_notes(raw);
        assert_eq!(out, "* Full changelog in What’s New\n* Keep later items");
        assert!(!out.contains("github.com"));
    }

    #[test]
    fn format_changelog_strips_only_trailing_github_attribution() {
        let raw = r#"## What's Changed
* Revert "Fix foo by @alice in #12" by @bob in https://github.com/JustNak/RusticDL/pull/99
"#;
        let out = format_changelog_notes(raw);
        assert_eq!(out, r#"* Revert "Fix foo by @alice in #12""#);
    }
}
