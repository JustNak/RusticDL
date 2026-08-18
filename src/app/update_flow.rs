//! Staged self-update flow extracted from `DownloadApp`.
//!
//! Toast stages (interactive + silent when an update exists):
//! 1. **Checking for update…**
//! 2a. **You're up to date** — or —
//! 2b. **Update available vX.Y.Z** `[Update]`
//! 3. On Update: flush state, snapshot What’s new, spawn **RusticDL Updater**, quit.
//! 4. Updater downloads, runs NSIS `/S`, relaunches the main app.
//! 5. **What’s new** — post-relaunch dialog with the release changelog.
//!
//! Channel (`UpdateChannel`) selects Stable (`/releases/latest`) vs Nightly
//! (`vX.Y.Z-nightly.*` pre-releases). Switching channels offers that stream’s
//! current build even when its version number is lower.
//! In-flight checks are invalidated when the channel changes.

use std::time::Duration;

use gpui::{div, px, rems, Context, ParentElement, Styled, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    dialog::DialogButtonProps,
    group_box::{GroupBox, GroupBoxVariants},
    h_flex,
    highlighter::HighlightTheme,
    text::{TextView, TextViewStyle},
    v_flex, ActiveTheme, Sizable, Theme, WindowExt,
};

use super::toast::{ToastActionKind, ToastKind};
use super::DownloadApp;
use crate::branding::{APP_VERSION, UPDATER_NAME};
use crate::persistence::{clear_pending_whats_new, save_pending_whats_new, PendingWhatsNew};
use crate::settings::UpdateChannel;
use crate::updater::{
    check_for_update, launch_updater, open_url, LaunchUpdaterOpts, UpdateCheck, UpdateInfo,
};

/// Max height for the scrollable post-update changelog body.
/// `TextView::scrollable(true)` requires a definite parent height (not only max_h).
const WHATS_NEW_NOTES_MAX_H: f32 = 168.0;
const WHATS_NEW_NOTES_MIN_H: f32 = 64.0;
const WHATS_NEW_NOTES_LINE_H: f32 = 20.0;

impl DownloadApp {
    /// Label for the single update action (check or advance cached release).
    pub(crate) fn update_action_label(&self) -> String {
        if let Some(info) = &self.available_update {
            format!("Update available v{}", info.latest_version)
        } else {
            "Check for updates".into()
        }
    }

    /// Brand menu / About: check when unknown, else re-show the Update toast.
    pub(crate) fn begin_update_action(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.update_busy {
            self.show_toast("An update is already in progress…", cx);
            return;
        }
        if let Some(info) = self.available_update.clone() {
            self.show_update_available_toast(&info, cx);
            return;
        }
        self.begin_update_check(true, cx);
    }

    /// Manual or silent GitHub Releases update check (never installs).
    pub(crate) fn begin_update_check(&mut self, interactive: bool, cx: &mut Context<Self>) {
        if self.update_busy {
            if interactive {
                self.show_toast("An update check is already running…", cx);
            }
            return;
        }
        self.update_check_gen = self.update_check_gen.wrapping_add(1);
        let check_gen = self.update_check_gen;
        self.update_busy = true;
        if interactive {
            self.replace_update_toast("Checking for update…", ToastKind::Info, None, cx);
        }
        cx.notify();
        let channel = self.settings.update_channel;
        spawn_update_check(interactive, channel, check_gen, cx);
    }

    pub(crate) fn on_update_check_finished(
        &mut self,
        interactive: bool,
        check_gen: u64,
        result: Result<UpdateCheck, String>,
        cx: &mut Context<Self>,
    ) {
        // Channel switch (or a newer check / apply) invalidates this completion.
        if check_gen != self.update_check_gen {
            return;
        }
        match result {
            Ok(UpdateCheck::UpToDate { .. }) => {
                self.available_update = None;
                self.update_busy = false;
                if interactive {
                    self.replace_update_toast("You're up to date", ToastKind::Info, None, cx);
                } else {
                    // Drop the checking toast if a silent check somehow set one.
                    self.clear_update_toast(cx);
                }
            }
            Ok(UpdateCheck::Available(info)) => {
                self.available_update = Some(info.clone());
                self.update_busy = false;
                // Interactive and silent: toast with [Update] so the user can continue
                // without hunting the brand menu.
                self.show_update_available_toast(&info, cx);
            }
            Err(message) => {
                self.update_busy = false;
                if interactive {
                    self.replace_update_toast(message, ToastKind::Error, None, cx);
                } else {
                    self.clear_update_toast(cx);
                }
            }
        }
        cx.notify();
    }

    /// “Update available vX.Y.Z” with an Update action button.
    pub(crate) fn show_update_available_toast(
        &mut self,
        info: &UpdateInfo,
        cx: &mut Context<Self>,
    ) {
        self.replace_update_toast(
            format!("Update available v{}", info.latest_version),
            ToastKind::Info,
            Some(("Update", ToastActionKind::ApplyUpdate)),
            cx,
        );
    }

    /// Handle primary actions from update toasts.
    pub(crate) fn on_update_toast_action(&mut self, kind: ToastActionKind, cx: &mut Context<Self>) {
        match kind {
            ToastActionKind::ApplyUpdate => {
                let Some(info) = self.available_update.clone() else {
                    self.show_toast("No update is ready to install.", cx);
                    return;
                };
                self.begin_apply_update(info, cx);
            }
        }
    }

    /// Open the post-update changelog once a `Window` is free.
    pub(crate) fn apply_pending_whats_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_show_whats_new {
            return;
        }
        if window.has_active_dialog(cx) {
            return;
        }
        let Some(pending) = self.pending_whats_new.clone() else {
            self.pending_show_whats_new = false;
            return;
        };
        self.pending_show_whats_new = false;
        self.open_whats_new_dialog(pending, window, cx);
    }

    /// Tasteful post-update changelog (Esc / mouse-back / outside / Close).
    pub(crate) fn open_whats_new_dialog(
        &mut self,
        pending: PendingWhatsNew,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Ack on open so Esc / mouse-back via `close_dialog` (no on_close) cannot re-show.
        self.ack_whats_new(cx);

        let to = pending.to_version.clone();
        let from = pending.from_version.clone();
        let html_url = pending.html_url.clone();
        let notes_markdown = pending
            .notes
            .as_ref()
            .map(|n| format_changelog_notes(n))
            .filter(|n| !n.is_empty());
        let title = format!("Updated to v{to}");
        let has_url = !html_url.trim().is_empty();
        let notes_h = notes_markdown
            .as_ref()
            .map(|n| changelog_notes_height(n))
            .unwrap_or(0.0);

        window.open_dialog(cx, move |dialog, window, cx| {
            let theme = cx.theme().clone();
            let muted = theme.muted_foreground;
            let title = title.clone();
            let from = from.clone();
            let html_url = html_url.clone();
            let notes_markdown = notes_markdown.clone();

            let est_h = if notes_markdown.is_some() {
                220.0 + notes_h
            } else {
                200.0
            };
            let view_h = window.viewport_size().height.to_f64() as f32;
            let max_top = (view_h - est_h - 20.0).max(24.0);
            let margin_top = ((view_h - est_h) * 0.5).clamp(24.0, max_top);

            let mut body = v_flex().gap_2().child(
                div()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(format!("You were on v{from}. Here’s what changed.")),
            );

            if let Some(notes) = notes_markdown {
                body = body.child(
                    GroupBox::new().outline().child(
                        div().w_full().h(px(notes_h)).child(
                            TextView::markdown("whats-new-notes-md", notes, window, cx)
                                .selectable(true)
                                .scrollable(true)
                                .text_sm()
                                .style(changelog_text_style(&theme)),
                        ),
                    ),
                );
            } else {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("No release notes were included for this version."),
                );
            }

            if has_url {
                let url = html_url.clone();
                body = body.child(
                    h_flex().child(
                        Button::new("whats-new-open-release")
                            .ghost()
                            .small()
                            .label("Open full notes")
                            .on_click(move |_, _, _| {
                                let _ = open_url(&url);
                            }),
                    ),
                );
            }

            dialog
                .title(title)
                .alert()
                // alert() disables outside-click; re-enable for light dismiss UX.
                .overlay_closable(true)
                .keyboard(true)
                .w(px(460.))
                .margin_top(px(margin_top))
                .border_color(theme.border.opacity(0.32))
                .button_props(DialogButtonProps::default().ok_text("Close"))
                .child(body)
        });
    }

    /// Drop the on-disk snapshot so the dialog does not reappear next launch.
    pub(crate) fn ack_whats_new(&mut self, _cx: &mut Context<Self>) {
        self.pending_whats_new = None;
        self.pending_show_whats_new = false;
        let _ = clear_pending_whats_new(&self.paths);
    }

    pub(crate) fn begin_apply_update(&mut self, info: UpdateInfo, cx: &mut Context<Self>) {
        if self.update_busy {
            self.show_toast("An update is already in progress…", cx);
            return;
        }
        // Invalidate any in-flight check so a late result cannot clear busy mid-handoff.
        self.update_check_gen = self.update_check_gen.wrapping_add(1);
        self.update_busy = true;
        self.begin_apply_update_inner(info, cx);
    }

    /// Persist state, snapshot What’s new, spawn RusticDL Updater, then quit.
    pub(crate) fn begin_apply_update_inner(&mut self, info: UpdateInfo, cx: &mut Context<Self>) {
        self.replace_update_toast(
            format!("Handing off to {UPDATER_NAME}…"),
            ToastKind::Info,
            None,
            cx,
        );
        cx.notify();

        // Persist before spawn/quit so a kill during install cannot race a dirty save.
        self.flush_jobs_save_now();
        self.flush_window_layout_now();

        let from_version = if info.current_version.trim().is_empty() {
            APP_VERSION.to_string()
        } else {
            info.current_version.clone()
        };

        // Snapshot notes now so the relaunched binary can show them without GitHub.
        let pending = PendingWhatsNew {
            from_version: from_version.clone(),
            to_version: info.latest_version.clone(),
            release_name: info.release_name.clone(),
            html_url: info.html_url.clone(),
            notes: info.notes.clone(),
        };
        let _ = save_pending_whats_new(&self.paths, &pending);

        let opts = LaunchUpdaterOpts {
            download_url: info.setup_download_url.clone(),
            from_version,
            to_version: info.latest_version.clone(),
            release_page: info.html_url.clone(),
            setup_size: info.setup_size,
        };

        if let Err(message) = launch_updater(&opts) {
            // Handoff failed — discard the snapshot so a normal start does not
            // claim an update that never applied.
            let _ = clear_pending_whats_new(&self.paths);
            self.update_busy = false;
            self.replace_update_toast(message, ToastKind::Error, None, cx);
            cx.notify();
            return;
        }

        // Bypass close-to-tray / hidden-window paint so quit actually tears down.
        self.force_quit_app(cx);
    }
}

/// Run a GitHub Releases update check on a background thread and deliver the result to the UI.
pub(crate) fn spawn_update_check(
    interactive: bool,
    channel: UpdateChannel,
    check_gen: u64,
    cx: &mut Context<DownloadApp>,
) {
    let delay = if interactive {
        Duration::from_millis(0)
    } else {
        Duration::from_secs(4)
    };

    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("Could not start update runtime: {e}"))
            .and_then(|rt| rt.block_on(check_for_update(channel)));
        let _ = tx.send_blocking(result);
    });

    cx.spawn(async move |this, cx| {
        let result = rx
            .recv()
            .await
            .unwrap_or_else(|_| Err("Update check was cancelled unexpectedly.".into()));
        let _ = this.update(cx, |app, cx| {
            app.on_update_check_finished(interactive, check_gen, result, cx);
        });
    })
    .detach();
}

/// Compact Markdown style that follows the desktop theme (density, dark/light).
fn changelog_text_style(theme: &Theme) -> TextViewStyle {
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

/// Fit the notes pane to the extracted changelog; cap so the first paint stays short.
fn changelog_notes_height(markdown: &str) -> f32 {
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

/// Prepare GitHub release Markdown for the What’s New dialog: changelog only.
fn format_changelog_notes(notes: &str) -> String {
    let stripped = strip_html_comments(notes);
    let extracted = extract_changelog_body(&stripped);
    collapse_blank_lines(&extracted)
}

/// Keep the “What’s Changed” / Changelog section; drop release boilerplate.
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

/// Drop Downloads / Quick start / License / product-title blocks when no changelog heading exists.
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
    let stripped = line
        .trim()
        .trim_start_matches(['*', '-', '#', ' '])
        .trim_start_matches('*')
        .trim();
    stripped.to_ascii_lowercase().starts_with("full changelog")
}

fn is_license_line(line: &str) -> bool {
    let t = line.trim();
    if heading_title(t).is_some_and(|title| normalize_heading(title) == "license") {
        return true;
    }
    let lower = t.to_ascii_lowercase();
    lower.starts_with("**license:**") || lower.starts_with("license:")
}

/// Drop GitHub’s auto `by @user in <url|#n>` suffix from a change line.
fn strip_github_attribution(line: &str) -> &str {
    let mut search_from = 0;
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
                return line[..start].trim_end();
            }
        }
        search_from = start + 5;
    }
    line
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
}
