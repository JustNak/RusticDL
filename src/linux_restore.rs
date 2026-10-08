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
            }} else if (workspace.activeClient !== undefined) {{
                workspace.activeClient = w;
            }} else {{
                workspace.activeWindow = w;
            }}
        }}
    }}
}})();
"#
    )
}

/// Unique per request (random), so concurrent or repeated Shows can never
/// unload each other's script.
pub(crate) fn kwin_script_name(pid: u32, nonce: &str) -> String {
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

const DBUS_TIMEOUT: Duration = Duration::from_millis(1000);
const RUN_TIMEOUT: Duration = Duration::from_millis(2000);
/// How long to wait for a `loadScript` that timed out to land before the
/// second unload sweep.
const LATE_LOAD_GRACE: Duration = Duration::from_millis(750);

/// Ask KWin to unminimize this process's windows. No-op on any other desktop.
///
/// Call once per Show (see `DownloadApp::restore_main_window_now`).
pub(crate) fn request_compositor_unminimize() {
    let current = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let session = std::env::var("XDG_SESSION_DESKTOP").ok();
    if !desktop_is_kde(current.as_deref()) && !desktop_is_kde(session.as_deref()) {
        return;
    }
    // The script file must live somewhere only this user can write, because
    // KWin executes it with session-bus access. No runtime dir, no KWin path.
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|dir| dir.is_absolute() && dir.is_dir())
    else {
        return;
    };
    let pid = std::process::id();
    let name = kwin_script_name(pid, &uuid::Uuid::new_v4().simple().to_string());
    let script = kwin_unminimize_script(pid);
    let _ = std::thread::Builder::new()
        .name("rusticdl-kwin-show".into())
        .spawn(move || {
            let _ = run_kwin_script(&dir, &name, &script);
        });
}

/// Create the script file exclusively: new file only, never through a
/// symlink, owner-only.
fn write_script_file(
    dir: &std::path::Path,
    name: &str,
    script: &str,
) -> io::Result<std::path::PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let path = dir.join(format!("{name}.js"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    if let Err(error) = file.write_all(script.as_bytes()) {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

fn kwin_method(args: &[&str], limit: Duration) -> io::Result<std::process::Output> {
    let mut full = vec![
        "--session",
        "--type=method_call",
        "--print-reply",
        "--dest=org.kde.KWin",
    ];
    full.extend_from_slice(args);
    dbus_send(&full, limit)
}

fn unload_script(name_arg: &str) {
    let _ = kwin_method(
        &[
            "/Scripting",
            "org.kde.kwin.Scripting.unloadScript",
            name_arg,
        ],
        DBUS_TIMEOUT,
    );
}

fn run_kwin_script(dir: &std::path::Path, name: &str, script: &str) -> io::Result<()> {
    let path = write_script_file(dir, name, script)?;
    let path_arg = format!("string:{}", path.display());
    let name_arg = format!("string:{name}");

    let result = (|| {
        let output = match kwin_method(
            &[
                "/Scripting",
                "org.kde.kwin.Scripting.loadScript",
                &path_arg,
                &name_arg,
            ],
            DBUS_TIMEOUT,
        ) {
            Ok(output) => output,
            Err(error) => {
                // The request may still have reached KWin: sweep again after
                // it has had time to land.
                std::thread::sleep(LATE_LOAD_GRACE);
                return Err(error);
            }
        };
        if !output.status.success() {
            return Err(io::Error::other("kwin loadScript failed"));
        }
        let reply = String::from_utf8_lossy(&output.stdout);
        let id = parse_dbus_script_id(&reply)
            .ok_or_else(|| io::Error::other("kwin loadScript returned no id"))?;
        // KWin 6 exposes the script at /Scripting/Script<id>; KWin 5 used /<id>.
        // `run` must be a real method call (a signal is silently ignored) and
        // its reply arrives once the script has been evaluated, so the file
        // is not removed underneath it.
        for object in [format!("/Scripting/Script{id}"), format!("/{id}")] {
            if let Ok(output) = kwin_method(&[&object, "org.kde.kwin.Script.run"], RUN_TIMEOUT) {
                if output.status.success() {
                    return Ok(());
                }
            }
        }
        Err(io::Error::other("kwin script run failed"))
    })();

    unload_script(&name_arg);
    if result.is_err() {
        unload_script(&name_arg);
    }
    let _ = std::fs::remove_file(&path);
    result
}

fn dbus_send(args: &[&str], limit: Duration) -> io::Result<std::process::Output> {
    let mut command = Command::new("dbus-send");
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    output_with_timeout(command, limit)
}

/// Run `command`, killing the child through its handle on timeout (never by
/// raw pid, which could have been reused).
fn output_with_timeout(mut command: Command, limit: Duration) -> io::Result<std::process::Output> {
    use std::io::Read;

    let mut child = command.spawn()?;
    let deadline = std::time::Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            let mut stdout = Vec::new();
            if let Some(mut pipe) = child.stdout.take() {
                let _ = pipe.read_to_end(&mut stdout);
            }
            return Ok(std::process::Output {
                status,
                stdout,
                stderr: Vec::new(),
            });
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(ErrorKind::TimedOut, "dbus-send timed out"));
        }
        std::thread::sleep(Duration::from_millis(10));
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
        assert!(script.contains("workspace.activeClient = w"));
        assert!(!script.contains("activateClient"));
        assert!(!script.contains("e+0"));
        assert!(!script.contains("special:"));
        assert!(!script.contains("{pid}"));
        let name = kwin_script_name(4242, "ab12");
        assert_eq!(name, "rusticdl-show-4242-ab12");
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn dbus_script_id_is_the_reply_integer() {
        let reply = "method return time=123.456 sender=:1.2 -> dest=:1.3 serial=2 reply_serial=2\n   int32 7\n";
        assert_eq!(parse_dbus_script_id(reply), Some(7));
        assert_eq!(parse_dbus_script_id("no id here"), None);
        assert_eq!(parse_dbus_script_id(""), None);
    }

    #[test]
    fn script_names_are_unique_per_request() {
        let a = kwin_script_name(1, &uuid::Uuid::new_v4().simple().to_string());
        let b = kwin_script_name(1, &uuid::Uuid::new_v4().simple().to_string());
        assert_ne!(a, b);
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rusticdl-restore-test-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn script_file_is_new_private_and_refuses_existing_paths() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("new");
        let path = write_script_file(&dir, "s1", "x").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // An existing file is never overwritten.
        assert!(write_script_file(&dir, "s1", "y").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn script_file_does_not_follow_planted_symlinks() {
        let dir = scratch_dir("link");
        let victim = dir.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("s2.js")).unwrap();
        assert!(write_script_file(&dir, "s2", "evil").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timed_out_child_is_killed_through_its_handle() {
        let mut command = Command::new("sleep");
        command.arg("30").stdout(Stdio::piped());
        let began = std::time::Instant::now();
        let error = output_with_timeout(command, Duration::from_millis(200)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(began.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn finished_child_output_is_returned() {
        let mut command = Command::new("echo");
        command.arg("int32 7").stdout(Stdio::piped());
        let output = output_with_timeout(command, Duration::from_secs(5)).unwrap();
        assert!(output.status.success());
        assert_eq!(
            parse_dbus_script_id(&String::from_utf8_lossy(&output.stdout)),
            Some(7)
        );
    }
}
