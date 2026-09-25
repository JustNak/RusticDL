//! Hyprland compositor detection and capture-HUD floating hints.
//!
//! Detection is an env probe only (`HYPRLAND_INSTANCE_SIGNATURE`) — no HIS
//! socket for the probe itself. Floating a capture HUD may talk to the Hyprland
//! IPC socket as a best-effort fallback when GPUI has no focused parent.

use crate::branding::APP_NAME;

/// `true` when `HYPRLAND_INSTANCE_SIGNATURE` is set in the environment.
pub fn is_hyprland() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

/// Window-rule style matcher for capture HUD titles (`RusticDL — …`).
///
/// All four capture surfaces (confirm, conflict/overwrite, progress, complete)
/// use an em-dash title from [`crate::prompt_window::open`].
pub fn capture_title_matcher() -> String {
    format!("title:^({APP_NAME} —)")
}

/// Ask Hyprland to float browser capture HUDs.
///
/// `WindowKind::Floating` sets an xdg parent when GPUI has a focused window of
/// this client. Browser handoffs often open while focus is in another client,
/// so the parent is missing and Hyprland would still tile. This IPC covers that
/// case in-app (no user windowrule).
///
/// No-op when not on Hyprland. Best-effort: failures are ignored.
pub fn float_capture_windows() {
    #[cfg(target_os = "linux")]
    {
        if !is_hyprland() {
            return;
        }
        let matcher = capture_title_matcher();
        let _ = std::thread::Builder::new()
            .name("rusticdl-hypr-float".into())
            .spawn(move || {
                // Map/focus can lag open_window; retry a few times.
                for delay_ms in [16_u64, 80, 200] {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    if dispatch_setfloating(Some(&matcher)) {
                        return;
                    }
                }
                // Last resort: float whatever is active (capture opens focused).
                let _ = dispatch_setfloating(None);
            });
    }
}

/// Hyprland IPC: `dispatch setfloating [window]`.
///
/// Returns `true` when the compositor replies `ok`.
#[cfg(target_os = "linux")]
fn dispatch_setfloating(window: Option<&str>) -> bool {
    let cmd = match window {
        Some(w) => format!("/dispatch setfloating {w}"),
        None => "/dispatch setfloating".to_string(),
    };
    hyprland_ipc(&cmd)
        .map(|reply| reply_is_ok(&reply))
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn reply_is_ok(reply: &str) -> bool {
    let trimmed = reply.trim();
    trimmed.is_empty() || trimmed.starts_with("ok")
}

#[cfg(target_os = "linux")]
fn hyprland_ipc(command: &str) -> std::io::Result<String> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let his = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()))?;
    let runtime =
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{}", unix_uid()));
    let path = format!("{runtime}/hypr/{his}/.socket.sock");

    let mut stream = UnixStream::connect(path)?;
    let timeout = std::time::Duration::from_millis(250);
    stream.set_write_timeout(Some(timeout))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.write_all(command.as_bytes())?;

    let mut buf = Vec::with_capacity(256);
    let mut chunk = [0_u8; 512];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
            Err(e) => return Err(e),
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(target_os = "linux")]
fn unix_uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn hyprland_when_hyprland_instance_signature_set() {
        let _guard = env_lock();
        const TOKEN: &str = "HYPRLAND_INSTANCE_SIGNATURE";
        assert_eq!(TOKEN, "HYPRLAND_INSTANCE_SIGNATURE");
        // SAFETY: test-only; serialized via ENV_LOCK.
        unsafe { std::env::set_var(TOKEN, "probe") };
        assert!(is_hyprland());
        unsafe { std::env::remove_var(TOKEN) };
        assert!(!is_hyprland());
    }

    #[test]
    fn capture_title_matcher_uses_app_name_and_em_dash() {
        let matcher = capture_title_matcher();
        assert!(
            matcher.starts_with("title:^("),
            "expected title regex prefix, got {matcher}"
        );
        assert!(
            matcher.contains(APP_NAME),
            "matcher must include APP_NAME, got {matcher}"
        );
        assert!(
            matcher.contains('—'),
            "capture titles use an em dash, got {matcher}"
        );
        assert_eq!(matcher, format!("title:^({APP_NAME} —)"));
    }

    #[test]
    fn float_capture_windows_is_noop_off_hyprland() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE") };
        // Must not panic or hang when HIS is unset (no socket connect).
        float_capture_windows();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reply_ok_accepts_ok_prefix_and_empty() {
        assert!(reply_is_ok("ok"));
        assert!(reply_is_ok("ok\n"));
        assert!(reply_is_ok(""));
        assert!(reply_is_ok("   "));
        assert!(!reply_is_ok("error"));
        assert!(!reply_is_ok("Invalid"));
    }
}
