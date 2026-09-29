//! Hyprland compositor detection and capture-HUD floating hints.
//!
//! Detection is an env probe only (`HYPRLAND_INSTANCE_SIGNATURE`) — no HIS
//! socket for the probe itself. Capture HUDs use a dedicated Wayland `app_id`
//! ([`CAPTURE_APP_ID`]) so rules and IPC can target them without floating the
//! main queue window. Pre-map `windowrule` keywords float/size/center the HUD
//! at creation; post-map IPC remains a best-effort fallback when rules miss.

#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};

/// Wayland / X11 app id for browser capture HUDs only.
///
/// Distinct from the main queue window so Hyprland class matchers can float
/// capture surfaces without touching `StartupWMClass=RusticDL`.
pub const CAPTURE_APP_ID: &str = "rusticdl-capture";

/// Named Hyprland windowrule (0.53+) updated before each capture open.
const CAPTURE_RULE_NAME: &str = "rusticdl-capture";

/// `true` when `HYPRLAND_INSTANCE_SIGNATURE` is set in the environment.
pub fn is_hyprland() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

/// Install / refresh Hyprland rules so the next `CAPTURE_APP_ID` map floats at
/// `width`×`height`, centered, without open animation.
///
/// Call **before** `open_window`. Static float/size/center rules only apply at
/// map time; post-map `setfloating` cannot undo the first tiled frame.
///
/// No-op when not on Hyprland. Best-effort: failures are ignored.
pub fn prepare_capture_window(width: u32, height: u32) {
    #[cfg(target_os = "linux")]
    {
        if !is_hyprland() {
            return;
        }
        if !install_named_capture_rules(width, height) {
            install_legacy_capture_rules_once();
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (width, height);
    }
}

/// Ask Hyprland to float the capture HUD that just opened with `title`.
///
/// Fallback when pre-map rules miss (older compositor, rule keyword rejected).
/// Targets the exact opener title after `set_window_title`. Also applies
/// `resizewindowpixel exact` + `centerwindow` so a late float lands at the
/// designed HUD size instead of a tiled leftover geometry.
///
/// No-op when not on Hyprland. Best-effort: failures are ignored. Does not
/// fall back to floating the focused window.
pub fn float_capture_windows(title: &str, width: u32, height: u32) {
    #[cfg(target_os = "linux")]
    {
        if !is_hyprland() {
            return;
        }
        let selector = window_title_selector(title);
        let _ = std::thread::Builder::new()
            .name("rusticdl-hypr-float".into())
            .spawn(move || {
                // Prefer an immediate attempt; map/title can still lag, so retry.
                for delay_ms in [0_u64, 16, 80, 200] {
                    if delay_ms > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    }
                    if dispatch_float(&selector) {
                        dispatch_size_center(&selector, width, height);
                        return;
                    }
                }
            });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (title, width, height);
    }
}

/// Hyprland `title:` matcher: FullMatch of the exact HUD title.
fn window_title_selector(title: &str) -> String {
    format!("title:^{}$", escape_ere(title))
}

/// Hyprland `class:` matcher for [`CAPTURE_APP_ID`].
fn capture_class_selector() -> String {
    format!("class:^{}$", escape_ere(CAPTURE_APP_ID))
}

/// POSIX ERE metacharacters so a HUD title / class is matched literally.
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

fn legacy_size_command(selector: &str, width: u32, height: u32) -> String {
    format!("/dispatch resizewindowpixel exact {width} {height},{selector}")
}

fn legacy_focus_command(selector: &str) -> String {
    format!("/dispatch focuswindow {selector}")
}

fn legacy_center_command() -> &'static str {
    "/dispatch centerwindow"
}

/// Hyprland 0.53+ named windowrule field write.
fn named_rule_keyword(field: &str, value: &str) -> String {
    format!("/keyword windowrule[{CAPTURE_RULE_NAME}]:{field} {value}")
}

/// Pre-0.53 anonymous class rules (installed once per process).
fn legacy_windowrulev2_float() -> String {
    format!("/keyword windowrulev2 float,{}", capture_class_selector())
}

fn legacy_windowrulev2_center() -> String {
    format!("/keyword windowrulev2 center,{}", capture_class_selector())
}

fn legacy_windowrulev2_noanim() -> String {
    format!("/keyword windowrulev2 noanim,{}", capture_class_selector())
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
    } else if trimmed.starts_with("Invalid dispatcher")
        || trimmed.starts_with("Invalid")
        || trimmed.starts_with("error:")
    {
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
fn dispatch_size_center(selector: &str, width: u32, height: u32) {
    let _ = hyprland_ipc(&legacy_size_command(selector, width, height));
    // centerwindow has no window selector; focus the HUD first.
    if hyprland_ipc(&legacy_focus_command(selector))
        .map(|r| classify_dispatch_reply(&r) == DispatchReply::Ok)
        .unwrap_or(false)
    {
        let _ = hyprland_ipc(legacy_center_command());
    }
}

/// 0.53+ named rules: match capture class, float, size, center, no open anim.
///
/// Returns `true` when the compositor accepted the named-rule syntax.
#[cfg(target_os = "linux")]
fn install_named_capture_rules(width: u32, height: u32) -> bool {
    let class_re = format!("^{}$", escape_ere(CAPTURE_APP_ID));
    let match_cmd = named_rule_keyword("match:class", &class_re);
    match hyprland_ipc(&match_cmd) {
        Ok(reply) if classify_dispatch_reply(&reply) == DispatchReply::Ok => {}
        _ => return false,
    }
    let fields = [
        named_rule_keyword("float", "on"),
        named_rule_keyword("size", &format!("{width} {height}")),
        named_rule_keyword("center", "on"),
        named_rule_keyword("animation", "none"),
    ];
    for cmd in fields {
        let _ = hyprland_ipc(&cmd);
    }
    true
}

#[cfg(target_os = "linux")]
fn install_legacy_capture_rules_once() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    // Size varies per HUD phase; post-map resize covers that on ≤0.52.
    for cmd in [
        legacy_windowrulev2_float(),
        legacy_windowrulev2_center(),
        legacy_windowrulev2_noanim(),
    ] {
        let _ = hyprland_ipc(&cmd);
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
    fn capture_app_id_is_not_main_wm_class() {
        assert_eq!(CAPTURE_APP_ID, "rusticdl-capture");
        assert_ne!(
            CAPTURE_APP_ID, "RusticDL",
            "must not float the main queue window via StartupWMClass"
        );
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
    fn capture_class_selector_matches_capture_app_id() {
        assert_eq!(capture_class_selector(), "class:^rusticdl-capture$");
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
    fn legacy_size_command_is_exact_pixels_with_selector() {
        let selector = window_title_selector("RusticDL — Confirm Download");
        assert_eq!(
            legacy_size_command(&selector, 480, 268),
            "/dispatch resizewindowpixel exact 480 268,title:^RusticDL — Confirm Download$"
        );
    }

    #[test]
    fn named_rule_keywords_target_capture_class_and_size() {
        let match_cmd =
            named_rule_keyword("match:class", &format!("^{}$", escape_ere(CAPTURE_APP_ID)));
        assert_eq!(
            match_cmd,
            "/keyword windowrule[rusticdl-capture]:match:class ^rusticdl-capture$"
        );
        assert_eq!(
            named_rule_keyword("float", "on"),
            "/keyword windowrule[rusticdl-capture]:float on"
        );
        assert_eq!(
            named_rule_keyword("size", "480 268"),
            "/keyword windowrule[rusticdl-capture]:size 480 268"
        );
        assert_eq!(
            named_rule_keyword("center", "on"),
            "/keyword windowrule[rusticdl-capture]:center on"
        );
        assert_eq!(
            named_rule_keyword("animation", "none"),
            "/keyword windowrule[rusticdl-capture]:animation none"
        );
    }

    #[test]
    fn legacy_windowrulev2_commands_use_class_not_main_title() {
        assert_eq!(
            legacy_windowrulev2_float(),
            "/keyword windowrulev2 float,class:^rusticdl-capture$"
        );
        assert!(legacy_windowrulev2_center().contains("class:^rusticdl-capture$"));
        assert!(legacy_windowrulev2_noanim().contains("class:^rusticdl-capture$"));
        assert!(
            !legacy_windowrulev2_float().contains("RusticDL"),
            "class rules must not match StartupWMClass=RusticDL"
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
        assert!(
            production.contains("prepare_capture_window"),
            "pre-map rules are required to avoid tile-then-float"
        );
        assert!(
            production.contains("CAPTURE_APP_ID"),
            "capture HUDs need a dedicated app id for class rules"
        );
    }

    #[test]
    fn float_and_prepare_are_noop_off_hyprland() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE") };
        // Must not panic or hang when HIS is unset (no socket connect).
        prepare_capture_window(480, 268);
        float_capture_windows("RusticDL — Confirm Download", 480, 268);
    }
}
