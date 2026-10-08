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
        .and_then(|dir| normalized_runtime_dir(&dir))
        .filter(|dir| runtime_dir_is_private(dir))
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

/// Lexically clean an absolute path: `.` and trailing separators go, and any
/// `..` rejects it (so `<symlink>/.` and `<symlink>/` cannot dodge the checks).
fn normalized_runtime_dir(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    use std::path::Component;
    if !dir.is_absolute() {
        return None;
    }
    let mut clean = std::path::PathBuf::new();
    for component in dir.components() {
        match component {
            Component::ParentDir => return None,
            Component::CurDir => {}
            other => clean.push(other.as_os_str()),
        }
    }
    Some(clean)
}

/// The runtime dir must be a directory we own that nobody else can enter,
/// otherwise a hostile `XDG_RUNTIME_DIR` would reopen the shared-path hole.
fn runtime_dir_is_private(dir: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(dir) = normalized_runtime_dir(dir) else {
        return false;
    };
    std::fs::symlink_metadata(&dir).is_ok_and(|meta| {
        !meta.file_type().is_symlink()
            && meta.is_dir()
            && dir_is_private(meta.uid(), meta.mode(), unsafe { libc::getuid() })
    })
}

fn dir_is_private(owner: u32, mode: u32, our_uid: u32) -> bool {
    owner == our_uid && mode & 0o077 == 0
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

type Bus<'a> = &'a dyn Fn(&[&str], Duration) -> io::Result<std::process::Output>;

fn unload_script(bus: Bus, name_arg: &str) -> bool {
    bus(
        &[
            "/Scripting",
            "org.kde.kwin.Scripting.unloadScript",
            name_arg,
        ],
        DBUS_TIMEOUT,
    )
    .is_ok_and(|output| output.status.success())
}

/// Ids KWin already exposes as script objects, on either path layout: KWin 6
/// registers `/Scripting/Script<id>`, KWin 5 registers `/<id>`. `None` when
/// either introspection fails (the caller must then not run anything).
fn taken_script_ids(bus: Bus) -> Option<Vec<u32>> {
    let introspect = |object: &str| {
        let output = bus(
            &[object, "org.freedesktop.DBus.Introspectable.Introspect"],
            DBUS_TIMEOUT,
        )
        .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let mut ids = parse_script_ids(&introspect("/Scripting")?);
    ids.extend(parse_script_ids(&introspect("/")?));
    Some(ids)
}

/// Child object names that are a script id: `Script<N>` (KWin 6) or a bare
/// `<N>` (KWin 5).
fn parse_script_ids(introspection: &str) -> Vec<u32> {
    introspection
        .split("node name=\"")
        .skip(1)
        .filter_map(|rest| {
            let name = rest.split('"').next()?;
            name.strip_prefix("Script").unwrap_or(name).parse().ok()
        })
        .collect()
}

/// How many fresh names to try when the id KWin hands out is already in use.
const MAX_LOAD_ATTEMPTS: usize = 4;

fn run_kwin_script(dir: &std::path::Path, name: &str, script: &str) -> io::Result<()> {
    run_kwin_script_with(&kwin_method, dir, name, script)
}

/// KWin assigns a script the id `scripts.size()`, so after a lower-numbered
/// script was unloaded the next id equals a live script's. Our object then
/// never registers at that path, and `Script.run` there would run (and
/// "succeed" on) someone else's script. Never use `Scripting.start`: it
/// re-applies the user's plugin settings and starts every other app's idle
/// script, and it returns before our script is evaluated.
///
/// Instead, snapshot the ids in use (both KWin 6 and KWin 5 layouts) and keep
/// loading under fresh names until the returned id is free; the colliding
/// loads stay loaded meanwhile (unloading would hand out the same id again).
/// `run` is invoked only on an id that was free, i.e. on our own object, and
/// every name we loaded is unloaded at the end. `Ok` means that `run` call
/// succeeded; no free id, or an unreadable bus, is an error.
fn run_kwin_script_with(
    bus: Bus,
    dir: &std::path::Path,
    first_name: &str,
    script: &str,
) -> io::Result<()> {
    let taken = taken_script_ids(bus)
        .ok_or_else(|| io::Error::other("kwin scripting objects could not be listed"))?;

    let mut loaded: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut result = Err(io::Error::other("kwin gave no free script id"));
    for attempt in 0..MAX_LOAD_ATTEMPTS {
        let name = if attempt == 0 {
            first_name.to_string()
        } else {
            format!("{first_name}-{attempt}")
        };
        let path = match write_script_file(dir, &name, script) {
            Ok(path) => path,
            Err(error) => {
                result = Err(error);
                break;
            }
        };
        let name_arg = format!("string:{name}");
        loaded.push((name_arg.clone(), path.clone()));
        let path_arg = format!("string:{}", path.display());
        let output = match bus(
            &[
                "/Scripting",
                "org.kde.kwin.Scripting.loadScript",
                &path_arg,
                &name_arg,
            ],
            DBUS_TIMEOUT,
        ) {
            Ok(output) if output.status.success() => output,
            Ok(_) => {
                result = Err(io::Error::other("kwin loadScript failed"));
                break;
            }
            Err(error) => {
                // The request may still have reached KWin; the sweep below
                // waits for it to land.
                std::thread::sleep(LATE_LOAD_GRACE);
                result = Err(error);
                break;
            }
        };
        let Some(id) = parse_dbus_script_id(&String::from_utf8_lossy(&output.stdout)) else {
            result = Err(io::Error::other("kwin loadScript returned no id"));
            break;
        };
        if taken.contains(&id) {
            continue;
        }
        // Whichever layout the running KWin uses, only our object lives at
        // this id, so the other path simply does not exist.
        result = Err(io::Error::other("kwin script run failed"));
        for object in [format!("/Scripting/Script{id}"), format!("/{id}")] {
            // `run` must be a real method call, and its reply arrives once the
            // script has been evaluated, so the file is not removed under it.
            if let Ok(output) = bus(&[&object, "org.kde.kwin.Script.run"], RUN_TIMEOUT) {
                if output.status.success() {
                    result = Ok(());
                    break;
                }
            }
        }
        break;
    }

    let mut swept_late = false;
    for (name_arg, path) in &loaded {
        if (!unload_script(bus, name_arg) || result.is_err()) && !unload_script(bus, name_arg) {
            if !swept_late {
                std::thread::sleep(LATE_LOAD_GRACE);
                swept_late = true;
            }
            let _ = unload_script(bus, name_arg);
        }
        let _ = std::fs::remove_file(path);
    }
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

    #[test]
    fn runtime_dir_symlink_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("rt");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(runtime_dir_is_private(&dir));
        let link = dir.with_extension("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(!runtime_dir_is_private(&link));
        let mut slashed = link.clone().into_os_string();
        slashed.push("/");
        assert!(!runtime_dir_is_private(std::path::Path::new(&slashed)));
        let dotted = link.join(".");
        assert!(!runtime_dir_is_private(&dotted));
        assert!(!runtime_dir_is_private(
            &dir.join("..").join(dir.file_name().unwrap())
        ));
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runtime_dir_must_be_ours_and_private() {
        assert!(dir_is_private(1000, 0o40700, 1000));
        assert!(!dir_is_private(1001, 0o40700, 1000));
        assert!(!dir_is_private(1000, 0o40755, 1000));
        assert!(!dir_is_private(1000, 0o40710, 1000));
    }

    use std::sync::Mutex;

    fn ok(stdout: &str) -> io::Result<std::process::Output> {
        use std::os::unix::process::ExitStatusExt;
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    /// A tiny KWin: ids are `scripts.size()`, objects register only at free
    /// paths, `Script.run` on a path records which script it actually hit.
    struct FakeKwin {
        kwin5: bool,
        introspect_fails: bool,
        forced_ids: Mutex<Vec<u32>>,
        /// (name, id) of every live script; ours are appended as loaded.
        live: Mutex<Vec<(String, u32)>>,
        /// path -> owner name
        objects: Mutex<Vec<(String, String)>>,
        log: Mutex<Vec<String>>,
        ran: Mutex<Vec<String>>,
    }

    impl FakeKwin {
        fn new(kwin5: bool, others: &[(&str, u32)]) -> Self {
            let fake = Self {
                kwin5,
                introspect_fails: false,
                forced_ids: Mutex::new(Vec::new()),
                live: Mutex::new(Vec::new()),
                objects: Mutex::new(Vec::new()),
                log: Mutex::new(Vec::new()),
                ran: Mutex::new(Vec::new()),
            };
            for (name, id) in others {
                fake.register(name, *id);
            }
            fake
        }

        fn path(&self, id: u32) -> String {
            if self.kwin5 {
                format!("/{id}")
            } else {
                format!("/Scripting/Script{id}")
            }
        }

        fn register(&self, name: &str, id: u32) {
            self.live.lock().unwrap().push((name.into(), id));
            let path = self.path(id);
            let mut objects = self.objects.lock().unwrap();
            if !objects.iter().any(|(p, _)| *p == path) {
                objects.push((path, name.into()));
            }
        }

        fn call(&self, args: &[&str]) -> io::Result<std::process::Output> {
            let method = args.get(1).copied().unwrap_or("");
            self.log
                .lock()
                .unwrap()
                .push(format!("{} {}", args[0], method));
            match method {
                "org.freedesktop.DBus.Introspectable.Introspect" if self.introspect_fails => {
                    Err(io::Error::other("no bus"))
                }
                "org.freedesktop.DBus.Introspectable.Introspect" => {
                    let objects = self.objects.lock().unwrap();
                    let nodes: String = objects
                        .iter()
                        .filter_map(|(path, _)| {
                            if args[0] == "/Scripting" && path.starts_with("/Scripting/") {
                                Some(path.trim_start_matches("/Scripting/").to_string())
                            } else if args[0] == "/" && !path.starts_with("/Scripting") {
                                Some(path.trim_start_matches('/').to_string())
                            } else {
                                None
                            }
                        })
                        .map(|n| format!("<node name=\"{n}\"/>"))
                        .collect();
                    ok(&format!("<node>{nodes}<node name=\"Scripting\"/></node>"))
                }
                "org.kde.kwin.Scripting.loadScript" => {
                    let name = args[3].trim_start_matches("string:").to_string();
                    let forced = {
                        let mut forced = self.forced_ids.lock().unwrap();
                        (!forced.is_empty()).then(|| forced.remove(0))
                    };
                    let id = forced.unwrap_or(self.live.lock().unwrap().len() as u32);
                    self.register(&name, id);
                    ok(&format!("   int32 {id}\n"))
                }
                "org.kde.kwin.Scripting.unloadScript" => {
                    let name = args[2].trim_start_matches("string:");
                    self.live.lock().unwrap().retain(|(n, _)| n != name);
                    self.objects.lock().unwrap().retain(|(_, o)| o != name);
                    ok("   boolean true\n")
                }
                "org.kde.kwin.Script.run" => {
                    let owner = self
                        .objects
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|(p, _)| p == args[0])
                        .map(|(_, o)| o.clone());
                    match owner {
                        Some(owner) => {
                            self.ran.lock().unwrap().push(owner);
                            ok("")
                        }
                        None => Err(io::Error::other("no such object")),
                    }
                }
                other => panic!("unexpected KWin call: {other}"),
            }
        }
    }

    fn run_fake(fake: &FakeKwin, name: &str) -> io::Result<()> {
        let dir = scratch_dir(name);
        let bus = |args: &[&str], _: Duration| fake.call(args);
        let result = run_kwin_script_with(&bus, &dir, name, "x");
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    fn only_ours_ran(fake: &FakeKwin, prefix: &str) {
        let ran = fake.ran.lock().unwrap();
        assert!(!ran.is_empty());
        assert!(ran.iter().all(|owner| owner.starts_with(prefix)), "{ran:?}");
        let log = fake.log.lock().unwrap().join("\n");
        assert!(!log.contains("Scripting.start"), "{log}");
        assert!(
            fake.live
                .lock()
                .unwrap()
                .iter()
                .all(|(n, _)| !n.starts_with(prefix)),
            "every name we loaded is unloaded"
        );
    }

    #[test]
    fn introspection_lists_script_ids_in_both_layouts() {
        let xml = r#"<node><node name="Script0"/><node name="Script3"/><node name="7"/><node name="other"/></node>"#;
        assert_eq!(parse_script_ids(xml), vec![0, 3, 7]);
    }

    #[test]
    fn fresh_id_runs_our_object() {
        let fake = FakeKwin::new(false, &[("a", 0)]);
        assert!(run_fake(&fake, "n1").is_ok());
        only_ours_ran(&fake, "n1");
    }

    #[test]
    fn colliding_id_reloads_under_a_fresh_name_and_runs_only_ours() {
        // Script0 was unloaded; the next id (2) equals live Script2.
        let fake = FakeKwin::new(false, &[("o1", 1), ("o2", 2)]);
        assert!(run_fake(&fake, "n2").is_ok());
        only_ours_ran(&fake, "n2");
        assert!(!fake.ran.lock().unwrap().iter().any(|o| o == "o2"));
    }

    #[test]
    fn kwin5_layout_collision_is_detected_on_the_bare_id_path() {
        let fake = FakeKwin::new(true, &[("o1", 1), ("o2", 2)]);
        assert!(run_fake(&fake, "n5").is_ok());
        only_ours_ran(&fake, "n5");
        assert!(!fake.ran.lock().unwrap().iter().any(|o| o == "o2"));
    }

    #[test]
    fn kwin5_fresh_id_runs_on_the_bare_id_path() {
        let fake = FakeKwin::new(true, &[("o0", 0)]);
        assert!(run_fake(&fake, "n6").is_ok());
        only_ours_ran(&fake, "n6");
        assert!(fake
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "/1 org.kde.kwin.Script.run"));
    }

    #[test]
    fn no_free_id_is_an_error_and_nothing_runs() {
        let fake = FakeKwin::new(false, &[("o1", 1), ("o2", 2)]);
        *fake.forced_ids.lock().unwrap() = vec![1, 2, 1, 2, 1, 2];
        assert!(run_fake(&fake, "n7").is_err());
        assert!(fake.ran.lock().unwrap().is_empty());
        assert!(fake
            .live
            .lock()
            .unwrap()
            .iter()
            .all(|(n, _)| !n.starts_with("n7")));
        let loads = fake
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.ends_with(" org.kde.kwin.Scripting.loadScript"))
            .count();
        assert_eq!(loads, MAX_LOAD_ATTEMPTS);
    }

    #[test]
    fn unreadable_bus_is_an_error_before_anything_is_loaded() {
        let mut fake = FakeKwin::new(false, &[]);
        fake.introspect_fails = true;
        assert!(run_fake(&fake, "n8").is_err());
        let log = fake.log.lock().unwrap().join("\n");
        assert!(!log.contains("loadScript"));
    }
}
