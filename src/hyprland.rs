//! Hyprland compositor detection and capture-HUD floating hints.
//!
//! Detection is an env probe only (`HYPRLAND_INSTANCE_SIGNATURE`) — no HIS
//! socket for the probe itself. Capture HUDs use a dedicated Wayland `app_id`
//! ([`CAPTURE_APP_ID`]) so rules and IPC can target them without floating the
//! main queue window.
//!
//! Pre-map rules must land before `open_window` so the HUD maps already
//! floating at its designed size (Omarchy/Hyprland otherwise tiles for a
//! frame, then post-map float snaps it). On Hyprland 0.55+ / Omarchy the
//! config is Lua: install via `/eval hl.window_rule({...})`. Older sessions
//! still get hyprlang named `windowrule[...]` or `windowrulev2`. Post-map
//! IPC remains a best-effort fallback when rules miss, and targets the newly
//! mapped surface by Hyprland address — not a shared OS title.

#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};

/// Wayland / X11 app id for browser capture HUDs only.
///
/// Distinct from the main queue window so Hyprland class matchers can float
/// capture surfaces without touching `StartupWMClass=RusticDL`.
pub const CAPTURE_APP_ID: &str = "rusticdl-capture";

/// Named Hyprland windowrule (0.53+) updated before each capture open.
const CAPTURE_RULE_NAME: &str = "rusticdl-capture";

/// This process's mapped Hyprland clients, captured before `open_window`.
///
/// Post-map IPC uses the address that appears after this snapshot so a second
/// Confirm/Progress HUD cannot resize or raise an older one with the same title.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureWindowSnapshot {
    addresses: Vec<String>,
    /// `true` when `j/clients` parsed. Empty `addresses` is success (no HUDs yet).
    queried: bool,
}

#[cfg(target_os = "linux")]
static LEGACY_CAPTURE_RULES_INSTALLED: AtomicBool = AtomicBool::new(false);

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
/// Preference order matches compositor generations:
/// 1. Lua `hl.window_rule` via `/eval` (Hyprland 0.55+ / Omarchy)
/// 2. Named hyprlang `windowrule[name]:…` (0.53–0.54)
/// 3. Anonymous `windowrulev2` once (≤0.52)
///
/// Returns the current process's client addresses so the post-map fallback can
/// target only the HUD that maps after this call.
///
/// No-op when not on Hyprland. Best-effort: failures are ignored.
pub fn prepare_capture_window(width: u32, height: u32) -> CaptureWindowSnapshot {
    #[cfg(target_os = "linux")]
    {
        if !is_hyprland() {
            return CaptureWindowSnapshot::default();
        }
        if !install_lua_capture_rules(width, height) && !install_named_capture_rules(width, height)
        {
            install_legacy_capture_rules_once();
        }
        snapshot_our_window_addresses()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (width, height);
        CaptureWindowSnapshot::default()
    }
}

/// Ask Hyprland to float the capture HUD that just opened.
///
/// Fallback when pre-map rules miss (older compositor, rule keyword rejected).
/// `prior` must be the snapshot from [`prepare_capture_window`] taken before
/// `open_window`. Dispatch uses that new client's `address:` — never the first
/// `title:` match (OS titles are shared and stay on morph).
///
/// Also applies exact resize + center so a late float lands at the designed HUD
/// size instead of a tiled leftover geometry.
///
/// No-op when not on Hyprland. Best-effort: failures are ignored. Does not
/// fall back to floating the focused window.
pub fn float_capture_windows(title: &str, width: u32, height: u32, prior: CaptureWindowSnapshot) {
    #[cfg(target_os = "linux")]
    {
        if !is_hyprland() {
            return;
        }
        let title = title.to_string();
        let _ = std::thread::Builder::new()
            .name("rusticdl-hypr-float".into())
            .spawn(move || {
                // Prefer an immediate attempt; map/title can still lag, so retry.
                for delay_ms in [0_u64, 16, 80, 200] {
                    if delay_ms > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    }
                    let Some(address) = query_new_capture_address(&prior, &title) else {
                        continue;
                    };
                    let selector = address_selector(&address);
                    let floated = dispatch_float(&selector);
                    dispatch_size_center(&selector, width, height);
                    if floated {
                        return;
                    }
                }
            });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (title, width, height, prior);
    }
}

/// Hyprland `class:` matcher for [`CAPTURE_APP_ID`].
fn capture_class_selector() -> String {
    format!("class:^{}$", escape_ere(CAPTURE_APP_ID))
}

/// Hyprland `address:` matcher for one client.
fn address_selector(address: &str) -> String {
    if let Some(rest) = address.strip_prefix("address:") {
        format!("address:{rest}")
    } else {
        format!("address:{address}")
    }
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

fn lua_resize_command(selector: &str, width: u32, height: u32) -> String {
    format!(
        r#"/dispatch hl.dsp.window.resize({{ x = {width}, y = {height}, window = "{}" }})"#,
        lua_string_escape(selector)
    )
}

fn lua_center_command(selector: &str) -> String {
    format!(
        r#"/dispatch hl.dsp.window.center({{ window = "{}" }})"#,
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

/// Hyprland 0.55+ Lua config: named rule so the next capture map floats at size.
///
/// Omarchy ships Lua-only Hyprland; hyprlang `/keyword windowrule[…]` is ignored
/// there, which left only the post-map float path (tile-then-float flash).
/// Re-calling with the same `name` refreshes size for the next HUD phase.
fn lua_window_rule_command(width: u32, height: u32) -> String {
    let class_re = lua_string_escape(&format!("^{}$", escape_ere(CAPTURE_APP_ID)));
    format!(
        "/eval hl.window_rule({{ name = \"{CAPTURE_RULE_NAME}\", match = {{ class = \"{class_re}\" }}, float = true, size = {{{width}, {height}}}, center = true, no_anim = true }})"
    )
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct HyprClient {
    address: String,
    class: String,
    title: String,
    pid: u32,
    floating: bool,
}

fn parse_hypr_clients(json: &str) -> Option<Vec<HyprClient>> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let arr = value.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let address = item.get("address")?.as_str()?.to_string();
        if address.is_empty() {
            continue;
        }
        let class = item
            .get("class")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let pid = item.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let floating = item
            .get("floating")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        out.push(HyprClient {
            address,
            class,
            title,
            pid,
            floating,
        });
    }
    Some(out)
}

/// Choose the HUD that mapped after `prior`. Never the first shared-title hit.
fn pick_new_capture_address(
    prior: &CaptureWindowSnapshot,
    clients: &[HyprClient],
    our_pid: u32,
    expected_title: &str,
) -> Option<String> {
    let ours: Vec<&HyprClient> = clients.iter().filter(|c| c.pid == our_pid).collect();
    if prior.queried {
        let newcomers: Vec<&HyprClient> = ours
            .iter()
            .copied()
            .filter(|c| !prior.addresses.iter().any(|a| a == &c.address))
            .collect();
        if newcomers.is_empty() {
            return None;
        }
        let preferred: Vec<&HyprClient> = newcomers
            .iter()
            .copied()
            .filter(|c| c.class == CAPTURE_APP_ID || c.title == expected_title)
            .collect();
        let pool = if preferred.is_empty() {
            &newcomers
        } else {
            &preferred
        };
        if let Some(tiled) = pool.iter().find(|c| !c.floating) {
            return Some((*tiled).address.clone());
        }
        return pool.last().map(|c| c.address.clone());
    }
    // No reliable pre-open list: only a uniquely tiled capture class (already
    // floating HUDs are left alone so we cannot raise a same-title neighbor).
    let tiled: Vec<&HyprClient> = ours
        .iter()
        .copied()
        .filter(|c| c.class == CAPTURE_APP_ID && !c.floating)
        .collect();
    if tiled.len() == 1 {
        Some(tiled[0].address.clone())
    } else {
        None
    }
}

/// Hyprland IPC: Lua `window.float` first, then legacy `setfloating`.
///
/// Always targets `selector`. Never dispatches without a window.
#[cfg(target_os = "linux")]
fn dispatch_float(selector: &str) -> bool {
    dispatch_lua_then_legacy(
        &lua_float_command(selector),
        &legacy_float_command(selector),
    )
}

#[cfg(target_os = "linux")]
fn dispatch_lua_then_legacy(lua_cmd: &str, legacy_cmd: &str) -> bool {
    match hyprland_ipc(lua_cmd) {
        Ok(reply) => match classify_dispatch_reply(&reply) {
            DispatchReply::Ok => true,
            DispatchReply::WrongSyntax => hyprland_ipc(legacy_cmd)
                .map(|legacy| classify_dispatch_reply(&legacy) == DispatchReply::Ok)
                .unwrap_or(false),
            DispatchReply::Failed => false,
        },
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
fn dispatch_size_center(selector: &str, width: u32, height: u32) {
    let _ = dispatch_lua_then_legacy(
        &lua_resize_command(selector, width, height),
        &legacy_size_command(selector, width, height),
    );
    // 0.55 `center({ window })` does not need a focus steal. ≤0.54
    // `centerwindow` has no window argument, so focus the HUD first.
    match hyprland_ipc(&lua_center_command(selector)) {
        Ok(reply) => match classify_dispatch_reply(&reply) {
            DispatchReply::Ok => {}
            DispatchReply::WrongSyntax => {
                if hyprland_ipc(&legacy_focus_command(selector))
                    .map(|r| classify_dispatch_reply(&r) == DispatchReply::Ok)
                    .unwrap_or(false)
                {
                    let _ = hyprland_ipc(legacy_center_command());
                }
            }
            DispatchReply::Failed => {}
        },
        Err(_) => {}
    }
}

/// 0.55+ Lua `hl.window_rule` so capture HUDs map floating (Omarchy).
///
/// Returns `true` when `/eval` accepted the rule (`ok`).
#[cfg(target_os = "linux")]
fn install_lua_capture_rules(width: u32, height: u32) -> bool {
    match hyprland_ipc(&lua_window_rule_command(width, height)) {
        Ok(reply) if classify_dispatch_reply(&reply) == DispatchReply::Ok => true,
        _ => false,
    }
}

/// 0.53–0.54 named hyprlang rules: match capture class, float, size, center,
/// no open anim.
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
        named_rule_keyword("no_anim", "on"),
    ];
    let mut no_anim_ok = false;
    for cmd in fields {
        match hyprland_ipc(&cmd) {
            Ok(reply) if classify_dispatch_reply(&reply) == DispatchReply::Ok => {
                if cmd.contains(":no_anim ") {
                    no_anim_ok = true;
                }
            }
            _ => {}
        }
    }
    if !no_anim_ok {
        let _ = hyprland_ipc(&legacy_windowrulev2_noanim());
    }
    true
}

#[cfg(target_os = "linux")]
fn install_legacy_capture_rules_once() {
    if LEGACY_CAPTURE_RULES_INSTALLED.load(Ordering::SeqCst) {
        return;
    }
    // Size varies per HUD phase; post-map resize covers that on ≤0.52.
    let mut all_ok = true;
    for cmd in [
        legacy_windowrulev2_float(),
        legacy_windowrulev2_center(),
        legacy_windowrulev2_noanim(),
    ] {
        match hyprland_ipc(&cmd) {
            Ok(reply) if classify_dispatch_reply(&reply) == DispatchReply::Ok => {}
            _ => all_ok = false,
        }
    }
    if all_ok {
        LEGACY_CAPTURE_RULES_INSTALLED.store(true, Ordering::SeqCst);
    }
}

#[cfg(target_os = "linux")]
fn snapshot_our_window_addresses() -> CaptureWindowSnapshot {
    match hyprland_ipc("j/clients") {
        Ok(reply) => match parse_hypr_clients(&reply) {
            Some(clients) => {
                let pid = std::process::id();
                CaptureWindowSnapshot {
                    addresses: clients
                        .into_iter()
                        .filter(|c| c.pid == pid)
                        .map(|c| c.address)
                        .collect(),
                    queried: true,
                }
            }
            None => CaptureWindowSnapshot::default(),
        },
        Err(_) => CaptureWindowSnapshot::default(),
    }
}

#[cfg(target_os = "linux")]
fn query_new_capture_address(
    prior: &CaptureWindowSnapshot,
    expected_title: &str,
) -> Option<String> {
    let reply = hyprland_ipc("j/clients").ok()?;
    let clients = parse_hypr_clients(&reply)?;
    pick_new_capture_address(prior, &clients, std::process::id(), expected_title)
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
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[path = "hyprland_tests.rs"]
mod tests;

#[cfg(all(test, target_os = "linux"))]
#[path = "hyprland_linux_ipc_tests.rs"]
mod linux_ipc_tests;
