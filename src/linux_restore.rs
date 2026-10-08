//! Bring a minimized Linux main window back.
//!
//! Hyprland moves the window itself (`hyprland::show_main_windows`). X11
//! `_NET_ACTIVE_WINDOW` deiconifies. Wayland `xdg_toplevel` has no
//! unminimize, and GPUI's `activate_window` only builds an xdg-activation
//! token (a "needs attention" hint that compositors often reject). On KDE
//! that token is not enough, so Show asks KWin to clear `minimized` for this
//! process. GNOME has no equivalent client request; the hidden flag stays set
//! until the compositor actually activates the window (taskbar / overview).

use std::ffi::OsStr;
use std::io::{self, ErrorKind};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::branding::MAIN_WINDOW_APP_ID;

/// Whether this restore has put the window back on screen.
///
/// A successful Hyprland move has. X11 activate deiconifies, so a non-Wayland
/// session has too. Wayland activate does not clear minimized, so the hidden
/// flag stays until [`crate::app`] sees a real activation.
pub(crate) fn restore_clears_hidden_flag(hyprland_moved: bool, wayland: bool) -> bool {
    hyprland_moved || !wayland
}

pub(crate) fn is_wayland_session(display: Option<&OsStr>) -> bool {
    display.is_some_and(|value| !value.is_empty())
}

pub(crate) fn wayland_session() -> bool {
    is_wayland_session(std::env::var_os("WAYLAND_DISPLAY").as_deref())
}

/// `XDG_CURRENT_DESKTOP` / `XDG_SESSION_DESKTOP` component, not a substring.
pub(crate) fn desktop_is_kde(desktop: Option<&str>) -> bool {
    let Some(desktop) = desktop else {
        return false;
    };
    desktop.split(':').any(|part| {
        let part = part.trim();
        part.eq_ignore_ascii_case("kde") || part.eq_ignore_ascii_case("plasma")
    })
}

/// One-shot KWin script. `pid` is this process only; nothing else is interpolated.
pub(crate) fn kwin_unminimize_script(pid: u32) -> String {
    format!(
        r#"(function () {{
    var pid = {pid};
    var list = [];
    if (workspace.windowList) {{
        list = workspace.windowList();
    }} else if (workspace.clientList) {{
        list = workspace.clientList();
    }}
    for (var i = 0; i < list.length; i++) {{
        var w = list[i];
        if (w.pid === pid) {{
            w.minimized = false;
            if (workspace.activateWindow) {{
                workspace.activateWindow(w);
            }} else if (workspace.activateClient) {{
                workspace.activateClient(w);
            }} else {{
                workspace.activeWindow = w;
            }}
        }}
    }}
}})();
"#
    )
}

pub(crate) fn kwin_script_name(pid: u32, nonce: u64) -> String {
    format!("rusticdl-show-{pid}-{nonce}")
}

/// Last integer in a `dbus-send --print-reply` body (`int32 7`).
pub(crate) fn parse_dbus_script_id(reply: &str) -> Option<u32> {
    reply.lines().rev().find_map(|line| {
        let token = line.split_whitespace().last()?;
        token.parse().ok()
    })
}

/// Set the main-window app id, then activate.
///
/// The id has to be set before `activate`: with it missing, GPUI skips the
/// token and the call is a no-op.
pub(crate) fn activate_for_restore(window: &mut gpui::Window) {
    window.set_app_id(MAIN_WINDOW_APP_ID);
    window.activate_window();
}

/// Ask KWin to unminimize this process's windows. No-op on any other desktop.
pub(crate) fn request_compositor_unminimize() {
    let current = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let session = std::env::var("XDG_SESSION_DESKTOP").ok();
    if !desktop_is_kde(current.as_deref()) && !desktop_is_kde(session.as_deref()) {
        return;
    }
    let pid = std::process::id();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let script = kwin_unminimize_script(pid);
    let name = kwin_script_name(pid, nonce);
    let _ = std::thread::Builder::new()
        .name("rusticdl-kwin-show".into())
        .spawn(move || {
            let _ = run_kwin_script(&name, &script);
        });
}

fn run_kwin_script(name: &str, script: &str) -> io::Result<()> {
    let path = std::env::temp_dir().join(format!("{name}.js"));
    std::fs::write(&path, script)?;
    let path_arg = format!("string:{}", path.display());
    let name_arg = format!("string:{name}");
    let loaded = dbus_send(
        &[
            "--session",
            "--print-reply",
            "--dest=org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting.loadScript",
            &path_arg,
            &name_arg,
        ],
        Duration::from_millis(1000),
    );
    let output = match loaded {
        Ok(output) => output,
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
    };
    if !output.status.success() {
        let _ = std::fs::remove_file(&path);
        return Err(io::Error::other("kwin loadScript failed"));
    }
    let reply = String::from_utf8_lossy(&output.stdout);
    let Some(id) = parse_dbus_script_id(&reply) else {
        let _ = dbus_send(
            &[
                "--session",
                "--dest=org.kde.KWin",
                "/Scripting",
                "org.kde.kwin.Scripting.unloadScript",
                &name_arg,
            ],
            Duration::from_millis(1000),
        );
        let _ = std::fs::remove_file(&path);
        return Err(io::Error::other("kwin loadScript returned no id"));
    };
    let id = id.to_string();
    // KWin 6 exposes the script at /Scripting/Script<id>; KWin 5 used /<id>.
    let kwin6_path = format!("/Scripting/Script{id}");
    let kwin5_path = format!("/{id}");
    let _ = dbus_send(
        &[
            "--session",
            "--dest=org.kde.KWin",
            &kwin6_path,
            "org.kde.kwin.Script.run",
        ],
        Duration::from_millis(1000),
    );
    let _ = dbus_send(
        &[
            "--session",
            "--dest=org.kde.KWin",
            &kwin5_path,
            "org.kde.kwin.Script.run",
        ],
        Duration::from_millis(1000),
    );
    let _ = dbus_send(
        &[
            "--session",
            "--dest=org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting.unloadScript",
            &name_arg,
        ],
        Duration::from_millis(1000),
    );
    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn dbus_send(args: &[&str], limit: Duration) -> io::Result<std::process::Output> {
    let mut command = Command::new("dbus-send");
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    output_with_timeout(command, limit)
}

fn output_with_timeout(mut command: Command, limit: Duration) -> io::Result<std::process::Output> {
    let child = command.spawn()?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(limit) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let _ = Command::new("kill").arg(pid.to_string()).status();
            Err(io::Error::new(ErrorKind::TimedOut, "dbus-send timed out"))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("dbus-send waiter exited"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_restore_keeps_hidden_flag_until_activation() {
        assert!(!restore_clears_hidden_flag(false, true));
        assert!(restore_clears_hidden_flag(true, true));
        assert!(restore_clears_hidden_flag(false, false));
    }

    #[test]
    fn wayland_session_needs_a_display_name() {
        assert!(!is_wayland_session(None));
        assert!(!is_wayland_session(Some(OsStr::new(""))));
        assert!(is_wayland_session(Some(OsStr::new("wayland-0"))));
    }

    #[test]
    fn kde_desktop_matches_component_not_substring() {
        assert!(desktop_is_kde(Some("KDE")));
        assert!(desktop_is_kde(Some("KDE:plasmashell")));
        assert!(desktop_is_kde(Some("plasma")));
        assert!(!desktop_is_kde(Some("GNOME")));
        assert!(!desktop_is_kde(Some("ubuntu:GNOME")));
        assert!(!desktop_is_kde(Some("Hyprland")));
        assert!(!desktop_is_kde(None));
    }

    #[test]
    fn kwin_script_mentions_only_our_pid() {
        let script = kwin_unminimize_script(4242);
        assert_eq!(script.matches("4242").count(), 1);
        assert!(script.contains("w.minimized = false"));
        assert!(script.contains("workspace.activateWindow"));
        assert!(!script.contains("e+0"));
        assert!(!script.contains("special:"));
        assert!(!script.contains("{pid}"));
        let name = kwin_script_name(4242, 9);
        assert_eq!(name, "rusticdl-show-4242-9");
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn dbus_script_id_is_the_reply_integer() {
        let reply = "method return time=123.456 sender=:1.2 -> dest=:1.3 serial=2 reply_serial=2\n   int32 7\n";
        assert_eq!(parse_dbus_script_id(reply), Some(7));
        assert_eq!(parse_dbus_script_id("no id here"), None);
        assert_eq!(parse_dbus_script_id(""), None);
    }
}
