use super::*;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    super::env_lock()
}

fn unique_runtime() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rusticdl-hypr-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("runtime dir");
    dir
}

fn spawn_mock_hypr(
    runtime: &Path,
    his: &str,
    handler: Arc<dyn Fn(&str) -> String + Send + Sync>,
    recorded: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let sock_dir = runtime.join("hypr").join(his);
    std::fs::create_dir_all(&sock_dir).expect("hypr dir");
    let sock_path = sock_dir.join(".socket.sock");
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).expect("bind hypr socket");
    listener.set_nonblocking(true).expect("nonblocking accept");
    std::thread::spawn(move || {
        while !stop.load(AtomicOrdering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let mut buf = Vec::new();
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
                            Err(_) => break,
                        }
                    }
                    let cmd = String::from_utf8_lossy(&buf).into_owned();
                    if cmd.is_empty() {
                        continue;
                    }
                    recorded.lock().expect("record").push(cmd.clone());
                    let reply = handler(&cmd);
                    let _ = stream.write_all(reply.as_bytes());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    })
}

fn wait_until_recorded(
    recorded: &Mutex<Vec<String>>,
    timeout: Duration,
    ready: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    let start = std::time::Instant::now();
    loop {
        let snapshot = recorded.lock().expect("record").clone();
        if ready(&snapshot) || start.elapsed() >= timeout {
            return snapshot;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn log_has_lua_center_on_new(cmds: &[String]) -> bool {
    cmds.iter()
        .any(|c| c.contains("hl.dsp.window.center(") && c.contains("address:0xnew"))
}

#[test]
fn failed_legacy_rule_install_retries_until_ok() {
    let _guard = env_lock();
    LEGACY_CAPTURE_RULES_INSTALLED.store(false, AtomicOrdering::SeqCst);
    let runtime = unique_runtime();
    let his = "legacy-retry";
    let old_his = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE");
    let old_xdg = std::env::var_os("XDG_RUNTIME_DIR");
    unsafe {
        std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", his);
        std::env::set_var("XDG_RUNTIME_DIR", &runtime);
    }

    // Named + legacy both fail (no socket). Must not stick INSTALLED.
    let _ = prepare_capture_window(480, 268);
    assert!(
        !LEGACY_CAPTURE_RULES_INSTALLED.load(AtomicOrdering::SeqCst),
        "a refused socket must not permanently skip legacy rules"
    );

    let recorded = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let handler = Arc::new(|cmd: &str| -> String {
        // Reject Lua + named hyprlang so the ≤0.52 windowrulev2 path is exercised.
        if cmd.starts_with("/eval ") || cmd.contains("windowrule[") {
            "Invalid".into()
        } else if cmd.contains("windowrulev2") || cmd == "j/clients" {
            if cmd == "j/clients" {
                "[]".into()
            } else {
                "ok".into()
            }
        } else {
            "ok".into()
        }
    });
    let server = spawn_mock_hypr(&runtime, his, handler, recorded.clone(), stop.clone());
    std::thread::sleep(Duration::from_millis(20));

    let _ = prepare_capture_window(480, 268);
    assert!(
        LEGACY_CAPTURE_RULES_INSTALLED.load(AtomicOrdering::SeqCst),
        "legacy rules must install after a later successful socket"
    );
    let first = recorded.lock().expect("record").clone();
    assert!(
        first.iter().any(|c| c.starts_with("/eval hl.window_rule(")),
        "Lua window_rule must be tried before hyprlang fallbacks, got {first:?}"
    );
    assert!(
        first.iter().any(|c| c.contains("windowrulev2 float")),
        "expected legacy float rule, got {first:?}"
    );

    recorded.lock().expect("record").clear();
    let _ = prepare_capture_window(480, 268);
    let second = recorded.lock().expect("record").clone();
    assert!(
        !second.iter().any(|c| c.contains("windowrulev2")),
        "successful install must not repeat windowrulev2, got {second:?}"
    );

    stop.store(true, AtomicOrdering::SeqCst);
    let _ = server.join();
    unsafe {
        match old_his {
            Some(v) => std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", v),
            None => std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE"),
        }
        match old_xdg {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
    }
    let _ = std::fs::remove_dir_all(&runtime);
    LEGACY_CAPTURE_RULES_INSTALLED.store(false, AtomicOrdering::SeqCst);
}

#[test]
fn prepare_installs_lua_window_rule_before_open() {
    let _guard = env_lock();
    LEGACY_CAPTURE_RULES_INSTALLED.store(false, AtomicOrdering::SeqCst);
    let runtime = unique_runtime();
    let his = "lua-rule-first";
    let old_his = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE");
    let old_xdg = std::env::var_os("XDG_RUNTIME_DIR");
    unsafe {
        std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", his);
        std::env::set_var("XDG_RUNTIME_DIR", &runtime);
    }

    let recorded = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let handler = Arc::new(|cmd: &str| -> String {
        if cmd == "j/clients" {
            "[]".into()
        } else {
            "ok".into()
        }
    });
    let server = spawn_mock_hypr(&runtime, his, handler, recorded.clone(), stop.clone());
    std::thread::sleep(Duration::from_millis(20));

    let _ = prepare_capture_window(540, 408);
    let cmds = recorded.lock().expect("record").clone();
    assert!(
        cmds.iter().any(|c| {
            c.starts_with("/eval hl.window_rule(")
                && c.contains(r#"class = "^rusticdl-capture$""#)
                && c.contains("float = true")
                && c.contains("size = {540, 408}")
                && c.contains("center = true")
                && c.contains("no_anim = true")
        }),
        "Omarchy needs Lua hl.window_rule at designed size before map, got {cmds:?}"
    );
    assert!(
        !cmds
            .iter()
            .any(|c| c.contains("windowrule[") || c.contains("windowrulev2")),
        "successful Lua install must not fall through to hyprlang rules, got {cmds:?}"
    );
    assert!(
        !LEGACY_CAPTURE_RULES_INSTALLED.load(AtomicOrdering::SeqCst),
        "Lua success must not mark legacy rules installed"
    );

    // Refresh size for a different HUD phase (confirm → conflict).
    recorded.lock().expect("record").clear();
    let _ = prepare_capture_window(480, 268);
    let refreshed = recorded.lock().expect("record").clone();
    assert!(
        refreshed.iter().any(|c| c.contains("size = {480, 268}")),
        "named Lua rule must refresh size for the next HUD, got {refreshed:?}"
    );

    stop.store(true, AtomicOrdering::SeqCst);
    let _ = server.join();
    unsafe {
        match old_his {
            Some(v) => std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", v),
            None => std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE"),
        }
        match old_xdg {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
    }
    let _ = std::fs::remove_dir_all(&runtime);
    LEGACY_CAPTURE_RULES_INSTALLED.store(false, AtomicOrdering::SeqCst);
}

#[test]
fn float_thread_does_not_dispatch_on_older_same_title_hud() {
    let _guard = env_lock();
    let runtime = unique_runtime();
    let his = "title-collide";
    let old_his = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE");
    let old_xdg = std::env::var_os("XDG_RUNTIME_DIR");
    unsafe {
        std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", his);
        std::env::set_var("XDG_RUNTIME_DIR", &runtime);
    }

    let pid = std::process::id();
    let title = "RusticDL — Confirm Download";
    let old_only = format!(
        r#"[{{"address":"0xold","class":"{CAPTURE_APP_ID}","title":"{title}","pid":{pid},"floating":true}}]"#
    );
    let with_new = format!(
        r#"[{{"address":"0xold","class":"{CAPTURE_APP_ID}","title":"{title}","pid":{pid},"floating":true}},{{"address":"0xnew","class":"{CAPTURE_APP_ID}","title":"{title}","pid":{pid},"floating":false}}]"#
    );
    let clients_calls = Arc::new(AtomicUsize::new(0));
    let clients_calls_h = clients_calls.clone();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let handler = Arc::new(move |cmd: &str| -> String {
        if cmd == "j/clients" {
            let n = clients_calls_h.fetch_add(1, AtomicOrdering::SeqCst);
            if n < 2 {
                old_only.clone()
            } else {
                with_new.clone()
            }
        } else {
            "ok".into()
        }
    });
    let server = spawn_mock_hypr(&runtime, his, handler, recorded.clone(), stop.clone());
    std::thread::sleep(Duration::from_millis(20));

    let prior = CaptureWindowSnapshot {
        addresses: vec!["0xold".into()],
        queried: true,
    };
    float_capture_windows(title, 480, 268, prior);

    // Center is the last dispatch. Do not snapshot after the first address:0xnew
    // (float/resize can land a frame earlier).
    let cmds = wait_until_recorded(
        &recorded,
        Duration::from_millis(500),
        log_has_lua_center_on_new,
    );
    assert!(
        log_has_lua_center_on_new(&cmds),
        "0.55 center fallback must be Lua center on the new address, got {cmds:?}"
    );
    assert!(
        cmds.iter().any(|c| c.contains("hl.dsp.window.resize(")
            && c.contains("address:0xnew")
            && c.contains("x = 480")),
        "0.55 size fallback must be Lua resize on the new address, got {cmds:?}"
    );
    assert!(
        !cmds
            .iter()
            .any(|c| c.contains("title:^") || c.contains("0xold")),
        "must not float/size/focus the older same-title HUD, got {cmds:?}"
    );

    stop.store(true, AtomicOrdering::SeqCst);
    let _ = server.join();
    unsafe {
        match old_his {
            Some(v) => std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", v),
            None => std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE"),
        }
        match old_xdg {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
    }
    let _ = std::fs::remove_dir_all(&runtime);
}
