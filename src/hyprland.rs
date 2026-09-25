//! Hyprland compositor detection and capture-HUD floating hints.
//!
//! Detection is an env probe only (`HYPRLAND_INSTANCE_SIGNATURE`) — no HIS
//! socket for the probe itself. Floating a capture HUD may talk to the Hyprland
//! IPC socket as a best-effort fallback when GPUI has no focused parent.

/// `true` when `HYPRLAND_INSTANCE_SIGNATURE` is set in the environment.
pub fn is_hyprland() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

/// Ask Hyprland to float the capture HUD that just opened with `title`.
///
/// `WindowKind::Floating` sets an xdg parent when GPUI has a focused window of
/// this client. Browser handoffs often open while focus is in another client,
/// so the parent is missing and Hyprland would still tile. This IPC covers that
/// case in-app (no user windowrule).
///
/// No-op when not on Hyprland. Best-effort: failures are ignored. Does not
/// fall back to floating the focused window.
pub fn float_capture_windows(title: &str) {
    #[cfg(target_os = "linux")]
    {
        if !is_hyprland() {
            return;
        }
        let selector = window_title_selector(title);
        let _ = std::thread::Builder::new()
            .name("rusticdl-hypr-float".into())
            .spawn(move || {
                // Map/focus can lag open_window; retry a few times.
                for delay_ms in [16_u64, 80, 200] {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    if dispatch_float(&selector) {
                        return;
                    }
                }
            });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = title;
    }
}

/// Hyprland `title:` matcher: FullMatch of the exact HUD title.
fn window_title_selector(title: &str) -> String {
    format!("title:^{}$", escape_ere(title))
}

/// POSIX ERE metacharacters so a HUD title is matched literally.
fn escape_ere(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

fn lua_string_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

/// Hyprland 0.55+ Lua-config IPC: `/dispatch` argument is `hl.dispatch(…)`.
fn lua_float_command(selector: &str) -> String {
    format!(
        r#"/dispatch hl.dsp.window.float({{ action = "enable", window = "{}" }})"#,
        lua_string_escape(selector)
    )
}

/// Hyprland ≤0.54 hyprlang IPC.
fn legacy_float_command(selector: &str) -> String {
    format!("/dispatch setfloating {selector}")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DispatchReply {
    Ok,
    WrongSyntax,
    Failed,
}

fn classify_dispatch_reply(reply: &str) -> DispatchReply {
    let trimmed = reply.trim();
    if trimmed.starts_with("ok") {
        DispatchReply::Ok
    } else if trimmed.starts_with("Invalid dispatcher") || trimmed.starts_with("error:") {
        DispatchReply::WrongSyntax
    } else {
        DispatchReply::Failed
    }
}

/// Hyprland IPC: Lua `window.float` first, then legacy `setfloating`.
///
/// Always targets `selector`. Never dispatches without a window.
#[cfg(target_os = "linux")]
fn dispatch_float(selector: &str) -> bool {
    match hyprland_ipc(&lua_float_command(selector)) {
        Ok(reply) => match classify_dispatch_reply(&reply) {
            DispatchReply::Ok => true,
            DispatchReply::WrongSyntax => hyprland_ipc(&legacy_float_command(selector))
                .map(|legacy| classify_dispatch_reply(&legacy) == DispatchReply::Ok)
                .unwrap_or(false),
            DispatchReply::Failed => false,
        },
        Err(_) => false,
    }
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
    fn window_title_selector_matches_exact_capture_title() {
        assert_eq!(
            window_title_selector("RusticDL — Confirm Download"),
            "title:^RusticDL — Confirm Download$"
        );
    }

    #[test]
    fn window_title_selector_escapes_ere_metacharacters() {
        assert_eq!(
            window_title_selector("RusticDL — foo.bar"),
            r"title:^RusticDL — foo\.bar$"
        );
    }

    #[test]
    fn lua_float_command_enables_named_window() {
        let selector = window_title_selector("RusticDL — Confirm Download");
        let cmd = lua_float_command(&selector);
        assert!(
            cmd.starts_with("/dispatch hl.dsp.window.float("),
            "current Hyprland evaluates /dispatch as Lua, got {cmd}"
        );
        assert!(
            cmd.contains(r#"action = "enable""#),
            "must set floating, not toggle, got {cmd}"
        );
        assert!(
            cmd.contains(r#"window = "title:^RusticDL — Confirm Download$""#),
            "must target the HUD title, got {cmd}"
        );
        assert!(
            !cmd.contains("setfloating"),
            "Lua payload must not use the dropped setfloating dispatcher, got {cmd}"
        );
    }

    #[test]
    fn lua_float_command_escapes_selector_for_lua_string() {
        let selector = window_title_selector("RusticDL — foo.bar");
        let cmd = lua_float_command(&selector);
        assert!(
            cmd.contains(r#"window = "title:^RusticDL — foo\\.bar$""#),
            "ERE backslash must be Lua-escaped, got {cmd}"
        );
    }

    #[test]
    fn legacy_float_command_includes_window_selector() {
        let selector = window_title_selector("RusticDL — Confirm Download");
        assert_eq!(
            legacy_float_command(&selector),
            "/dispatch setfloating title:^RusticDL — Confirm Download$"
        );
    }

    #[test]
    fn classify_ok_and_wrong_syntax_and_miss() {
        assert_eq!(classify_dispatch_reply("ok"), DispatchReply::Ok);
        assert_eq!(classify_dispatch_reply("ok\n"), DispatchReply::Ok);
        assert_eq!(
            classify_dispatch_reply("Invalid dispatcher"),
            DispatchReply::WrongSyntax
        );
        let lua_reject = "error: [string \"return hl.dispatch(setfloating title:^x$\")]:1: ')' expected near 'title'";
        assert_eq!(
            classify_dispatch_reply(lua_reject),
            DispatchReply::WrongSyntax
        );
        assert_eq!(
            classify_dispatch_reply("Window not found"),
            DispatchReply::Failed
        );
        assert_eq!(classify_dispatch_reply(""), DispatchReply::Failed);
    }

    #[test]
    fn production_never_floats_the_focused_window() {
        let src = include_str!("hyprland.rs");
        let production = src
            .split("#[cfg(test)]")
            .next()
            .expect("production hyprland.rs before tests");
        assert!(
            !production.contains("dispatch_float(None)"),
            "a missed title must not float m_lastWindow"
        );
        assert!(
            !production.contains("dispatch_setfloating"),
            "legacy dispatch is selector-only via legacy_float_command"
        );
        assert!(
            !production.contains("\"/dispatch setfloating\""),
            "bare setfloating floats whatever is focused"
        );
        assert!(
            production.contains("lua_float_command(selector)"),
            "current Hyprland needs the Lua float dispatcher"
        );
        assert!(
            production.contains("legacy_float_command(selector)"),
            "0.54 sessions still speak setfloating"
        );
    }

    #[test]
    fn float_capture_windows_is_noop_off_hyprland() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE") };
        // Must not panic or hang when HIS is unset (no socket connect).
        float_capture_windows("RusticDL — Confirm Download");
    }
}
