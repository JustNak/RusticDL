//! Linux session notifications via `notify-send` (`org.freedesktop.Notifications`).
//!
//! Windows tray balloons stay in [`crate::tray`]; this module is the no-tray Linux path.

use crate::branding::APP_NAME;
use crate::settings::OsNotifyMode;

use super::policy::hard_os_eligible;

/// Exact `notify-send` program name — not a neighbor binary on `PATH`.
pub const NOTIFY_SEND_PROGRAM: &str = "notify-send";

/// argv for `notify-send -a RusticDL -- <title> <body>` (testable without spawning).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifySendInvocation {
    pub program: String,
    pub args: Vec<String>,
}

/// Linux `exec` argument limit is well above this. Stay under it, and under
/// what a notification daemon will render.
const NOTIFY_BODY_MAX_BYTES: usize = 1024;

/// `notify-send` bodies are markup on KDE, mako, and dunst.
fn escape_notify_markup(body: &str) -> String {
    let mut escaped = String::with_capacity(body.len());
    for ch in body.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Escape first, then cut on a char boundary without splitting `&amp;`, `&lt;`, or `&gt;`.
fn cap_escaped_notify_body(escaped: &str) -> String {
    if escaped.len() <= NOTIFY_BODY_MAX_BYTES {
        return escaped.to_string();
    }
    let mut end = NOTIFY_BODY_MAX_BYTES;
    while end > 0 && !escaped.is_char_boundary(end) {
        end -= 1;
    }
    if let Some(amp) = escaped[..end].rfind('&') {
        let tail = &escaped[amp..end];
        if !tail.starts_with("&amp;") && !tail.starts_with("&lt;") && !tail.starts_with("&gt;") {
            end = amp;
        }
    }
    escaped[..end].to_string()
}

impl NotifySendInvocation {
    pub fn from_payload(title: &str, body: &str) -> Self {
        Self {
            program: NOTIFY_SEND_PROGRAM.to_string(),
            args: vec![
                "-a".to_string(),
                APP_NAME.to_string(),
                "--".to_string(),
                title.to_string(),
                cap_escaped_notify_body(&escape_notify_markup(body)),
            ],
        }
    }
}

/// Whether Linux would spawn `notify-send` instead of a tray balloon at flush time.
pub fn linux_session_notify_at_flush(mode: OsNotifyMode, window_hidden_to_tray: bool) -> bool {
    #[cfg(target_os = "linux")]
    {
        hard_os_eligible(mode, window_hidden_to_tray)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (mode, window_hidden_to_tray);
        false
    }
}

/// Spawn a detached `notify-send` for the composed balloon payload. No-op off Linux.
pub fn spawn_session_notify(title: &str, body: &str) {
    #[cfg(target_os = "linux")]
    {
        let invocation = NotifySendInvocation::from_payload(title, body);
        let mut cmd = std::process::Command::new(&invocation.program);
        cmd.args(&invocation.args);
        match cmd.spawn() {
            Ok(_) => {}
            Err(err) => {
                eprintln!("rusticdl: notify-send failed: {err}");
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (title, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::{compose_balloon, BalloonOutcome, PendingOsTerminal, TerminalKind};
    use std::path::PathBuf;

    fn sample_complete(filename: &str) -> PendingOsTerminal {
        PendingOsTerminal {
            kind: TerminalKind::Complete,
            filename: filename.into(),
            error: None,
            job_id: "j1".into(),
            target_path: Some(PathBuf::from("/dl/file.zip")),
        }
    }

    #[test]
    fn notify_send_program_is_exact_binary_name() {
        let inv = NotifySendInvocation::from_payload("t", "b");
        assert_eq!(inv.program, "notify-send");
        assert_ne!(inv.program, "notify-send-gtk");
        assert_ne!(inv.program, "/usr/bin/notify-send");
    }

    #[test]
    fn session_notify_escapes_markup() {
        let inv = NotifySendInvocation::from_payload(
            "Download failed",
            "file — <a href=\"http://evil.example\">x</a> & y",
        );
        let body = inv.args.last().expect("body");
        assert_eq!(
            body,
            "file — &lt;a href=\"http://evil.example\"&gt;x&lt;/a&gt; &amp; y"
        );
        assert!(!body.contains("<a href"));
        assert!(!body.contains('<'));
        assert!(!body.contains('>'));
    }

    #[test]
    fn notify_send_argv_uses_compose_balloon_title_and_body() {
        let pending = vec![sample_complete("file.zip")];
        let payload = compose_balloon(&pending).unwrap();
        let inv = NotifySendInvocation::from_payload(&payload.title, &payload.body);
        assert_eq!(
            inv.args,
            vec!["-a", "RusticDL", "--", "Download complete", "file.zip",]
        );
        assert_eq!(payload.kind, BalloonOutcome::SingleComplete);
    }

    #[test]
    fn session_notify_caps_multibyte_filename_on_char_boundary() {
        let filename = "你".repeat(400);
        let pending = PendingOsTerminal {
            kind: TerminalKind::Fail,
            filename,
            error: Some("x".into()),
            job_id: "j1".into(),
            target_path: None,
        };
        let payload = compose_balloon(std::slice::from_ref(&pending)).unwrap();
        let inv = NotifySendInvocation::from_payload(&payload.title, &payload.body);
        let body = inv.args.last().expect("body");
        assert!(
            body.len() <= 1024,
            "notification body must be 1 KiB or less, got {}",
            body.len()
        );
        assert!(
            body.ends_with('你'),
            "cut must keep a whole multibyte character, got {body:?}"
        );
        assert_eq!(body.as_str(), "你".repeat(341));
    }

    #[test]
    fn session_notify_does_not_split_an_escape_entity() {
        let raw = format!("{}&", "a".repeat(1022));
        let inv = NotifySendInvocation::from_payload("Download failed", &raw);
        let body = inv.args.last().expect("body");
        assert!(body.len() <= 1024, "got {}", body.len());
        assert_eq!(body.as_str(), "a".repeat(1022));
        assert!(!body.ends_with('&'));
        assert!(!body.contains("&amp") || body.contains("&amp;"));
    }

    #[test]
    fn windows_path_does_not_spawn_notify_send() {
        if cfg!(target_os = "linux") {
            return;
        }
        assert!(!linux_session_notify_at_flush(OsNotifyMode::Always, false));
        assert!(!linux_session_notify_at_flush(
            OsNotifyMode::WhenHiddenToTray,
            true
        ));
    }

    #[test]
    fn linux_hidden_mode_eligible_only_when_hidden_to_tray() {
        if cfg!(target_os = "linux") {
            assert!(!linux_session_notify_at_flush(
                OsNotifyMode::WhenHiddenToTray,
                false
            ));
            assert!(linux_session_notify_at_flush(
                OsNotifyMode::WhenHiddenToTray,
                true
            ));
            assert!(linux_session_notify_at_flush(OsNotifyMode::Always, false));
            assert!(!linux_session_notify_at_flush(OsNotifyMode::Off, true));
        }
    }
}
