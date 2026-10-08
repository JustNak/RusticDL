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
    /// `workspace.name` (e.g. `3`, `web`, `special:rusticdl`); empty if absent.
    workspace: String,
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
        let workspace = item
            .get("workspace")
            .and_then(|w| w.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(HyprClient {
            address,
            class,
            title,
            pid,
            floating,
            workspace,
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

/// Special workspace that parks the hidden main window (close to background).
#[cfg(target_os = "linux")]
const HIDDEN_WORKSPACE: &str = "special:rusticdl";

/// This process's main-window clients (capture HUDs are excluded).
#[cfg(target_os = "linux")]
fn main_window_clients(clients: &[HyprClient], our_pid: u32) -> Vec<&HyprClient> {
    clients
        .iter()
        .filter(|c| c.pid == our_pid && c.class != CAPTURE_APP_ID)
        .collect()
}

#[cfg(target_os = "linux")]
fn lua_hide_command(selector: &str) -> String {
    format!(
        r#"/dispatch hl.dsp.window.move({{ workspace = "{HIDDEN_WORKSPACE}", follow = false, window = "{}" }})"#,
        lua_string_escape(selector)
    )
}

#[cfg(target_os = "linux")]
fn legacy_hide_command(selector: &str) -> String {
    format!("/dispatch movetoworkspacesilent {HIDDEN_WORKSPACE},{selector}")
}

#[cfg(target_os = "linux")]
fn lua_show_command(workspace: &str, selector: &str) -> String {
    format!(
        r#"/dispatch hl.dsp.window.move({{ workspace = "{}", window = "{}" }})"#,
        lua_string_escape(workspace),
        lua_string_escape(selector)
    )
}

#[cfg(target_os = "linux")]
fn legacy_show_command(workspace: &str, selector: &str) -> String {
    format!("/dispatch movetoworkspace {workspace},{selector}")
}

#[cfg(target_os = "linux")]
fn lua_focus_command(selector: &str) -> String {
    format!(
        r#"/dispatch hl.dsp.focus({{ window = "{}" }})"#,
        lua_string_escape(selector)
    )
}

/// `movetoworkspace` argument for a workspace object (`{id, name}`).
///
/// Regular numbered workspaces have positive ids. Named workspaces have
/// negative ids (or none on newer Hyprland) and must be addressed as
/// `name:<name>`. Special workspaces are never a restore target.
#[cfg(target_os = "linux")]
fn workspace_target(workspace: &serde_json::Value) -> Option<String> {
    if let Some(id) = workspace.get("id").and_then(|id| id.as_i64()) {
        if id > 0 {
            return Some(id.to_string());
        }
    }
    let name = workspace.get("name").and_then(|n| n.as_str())?.trim();
    if name.is_empty() || name == "special" || name.starts_with("special:") || name.contains(',') {
        return None;
    }
    Some(format!("name:{name}"))
}

/// Regular workspace on the monitor the user is looking at.
///
/// `j/monitors[].activeWorkspace` stays the regular workspace while a special
/// is focused. Prefers the focused monitor; another monitor is used only when
/// it is the one showing our special workspace or the only monitor.
#[cfg(target_os = "linux")]
fn focused_monitor_workspace(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let monitors = value.as_array()?;
    let regular =
        |monitor: &serde_json::Value| monitor.get("activeWorkspace").and_then(workspace_target);
    let focused = monitors
        .iter()
        .find(|monitor| monitor.get("focused").and_then(|v| v.as_bool()) == Some(true));
    if let Some(target) = focused.and_then(regular) {
        return Some(target);
    }
    let special_open = monitors.iter().find(|monitor| special_open_on(monitor));
    if let Some(target) = special_open.and_then(regular) {
        return Some(target);
    }
    match monitors.as_slice() {
        [only] => regular(only),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn special_open_on(monitor: &serde_json::Value) -> bool {
    monitor
        .get("specialWorkspace")
        .and_then(|special| special.get("name"))
        .and_then(|name| name.as_str())
        .is_some_and(|name| !name.is_empty())
}

/// Where our hidden workspace is currently shown as an overlay.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Overlay {
    /// Open on any monitor.
    anywhere: bool,
    /// Open on the focused monitor (the only place a toggle closes it).
    on_focused: bool,
}

#[cfg(target_os = "linux")]
fn overlay_state(monitors_json: &str) -> Option<Overlay> {
    let value: serde_json::Value = serde_json::from_str(monitors_json).ok()?;
    let monitors = value.as_array()?;
    let shows_ours = |monitor: &serde_json::Value| {
        monitor
            .get("specialWorkspace")
            .and_then(|special| special.get("name"))
            .and_then(|name| name.as_str())
            == Some(HIDDEN_WORKSPACE)
    };
    Some(Overlay {
        anywhere: monitors.iter().any(shows_ours),
        on_focused: monitors.iter().any(|monitor| {
            monitor.get("focused").and_then(|v| v.as_bool()) == Some(true) && shows_ours(monitor)
        }),
    })
}

/// Workspace to `movetoworkspace` onto. `None` means do not dispatch
/// (never `e+0`, never a special workspace).
#[cfg(target_os = "linux")]
fn restore_workspace_id(active_json: &str, monitors_json: Option<&str>) -> Option<String> {
    if let Some(target) = monitors_json.and_then(focused_monitor_workspace) {
        return Some(target);
    }
    serde_json::from_str::<serde_json::Value>(active_json)
        .ok()
        .as_ref()
        .and_then(workspace_target)
}

/// Result of checking a hide.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HideState {
    /// On the hidden workspace and not shown as an overlay anywhere.
    Hidden,
    /// Parked there and shown as an overlay on the focused monitor, where a
    /// toggle closes it. The only state in which a toggle is safe.
    OverlayOnFocused,
    /// Evidence says it is not hidden (still on a normal workspace, or the
    /// overlay is open only on another monitor). Never toggle.
    NotHidden,
    /// State could not be read. Never toggle; see [`hide_verdict`].
    Unknown,
}

#[cfg(target_os = "linux")]
fn classify_hide(
    clients: Option<&[HyprClient]>,
    addresses: &[String],
    overlay: Option<Overlay>,
) -> HideState {
    let (Some(clients), Some(overlay)) = (clients, overlay) else {
        return HideState::Unknown;
    };
    let all_parked = !addresses.is_empty()
        && addresses.iter().all(|address| {
            clients
                .iter()
                .any(|c| &c.address == address && c.workspace == HIDDEN_WORKSPACE)
        });
    match (all_parked, overlay.anywhere, overlay.on_focused) {
        (false, _, _) => HideState::NotHidden,
        (true, false, _) => HideState::Hidden,
        (true, true, true) => HideState::OverlayOnFocused,
        (true, true, false) => HideState::NotHidden,
    }
}

/// Lua move first. A `Failed` reply (not only `Invalid` / `error:`) still
/// tries the legacy dispatcher. A socket error does not: legacy cannot
/// connect either.
#[cfg(target_os = "linux")]
fn move_tries_legacy(reply: DispatchReply) -> bool {
    match reply {
        DispatchReply::Ok => false,
        DispatchReply::WrongSyntax | DispatchReply::Failed => true,
    }
}

/// How a move/toggle dispatch ended. "Sent" is what matters for hiding: a
/// command that was never written cannot have changed anything, while one that
/// was written and went unanswered may still land.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MoveOutcome {
    /// Never written (no connection, or the deadline was already spent).
    NotSent,
    /// Written, and the compositor answered no.
    Refused,
    /// Written, but the reply was missing, late or cut off.
    Unanswered,
    Accepted,
}

#[cfg(target_os = "linux")]
impl MoveOutcome {
    /// The command may have changed state.
    fn may_have_landed(self) -> bool {
        matches!(self, MoveOutcome::Accepted | MoveOutcome::Unanswered)
    }
}

/// Lua dispatch first. The legacy dispatcher is tried only when the compositor
/// *answered* the Lua form with a rejection; a Lua command that was sent and
/// got no (or a partial) reply may already have landed, so repeating it as a
/// legacy command could apply it twice (fatal for toggles).
#[cfg(target_os = "linux")]
fn dispatch_move_then_legacy(
    lua_cmd: &str,
    legacy_cmd: &str,
    deadline: Option<std::time::Instant>,
) -> MoveOutcome {
    match hyprland_call(lua_cmd, deadline) {
        IpcResult::NotSent => MoveOutcome::NotSent,
        IpcResult::NoReply => MoveOutcome::Unanswered,
        IpcResult::Reply(reply) => {
            let class = classify_dispatch_reply(&reply);
            if !move_tries_legacy(class) {
                return MoveOutcome::Accepted;
            }
            match hyprland_call(legacy_cmd, deadline) {
                // The Lua form was rejected and the legacy one never left.
                IpcResult::NotSent => MoveOutcome::Refused,
                IpcResult::NoReply => MoveOutcome::Unanswered,
                IpcResult::Reply(reply) => {
                    if classify_dispatch_reply(&reply) == DispatchReply::Ok {
                        MoveOutcome::Accepted
                    } else {
                        MoveOutcome::Refused
                    }
                }
            }
        }
    }
}

/// Park the main window on a special workspace so it is truly off screen.
///
/// Hyprland has no minimize, so this is the only way to "hide" there. Returns
/// `false` when not on Hyprland or the compositor refused, so the caller can
/// fall back to a plain minimize rather than assume the window is gone.
#[cfg(target_os = "linux")]
pub fn hide_main_windows() -> bool {
    hide_main_windows_within(HIDE_STEPS_BUDGET, HIDE_FINAL_BUDGET)
}

/// Time for the move / toggle steps, then a final check after it.
///
/// Every socket call (connect, write and all reads) is bounded by a deadline,
/// so the UI thread blocks for at most `steps + final` plus scheduling slack:
/// about 700 ms by default.
#[cfg(target_os = "linux")]
const HIDE_STEPS_BUDGET: std::time::Duration = std::time::Duration::from_millis(400);
#[cfg(target_os = "linux")]
const HIDE_FINAL_BUDGET: std::time::Duration = std::time::Duration::from_millis(300);

/// What to report once the last check is in.
///
/// A window confirmed parked is hidden. If the state cannot be read but a move
/// was sent and may have landed, say hidden: a stale hidden flag is harmless
/// (restore clears it, or a later focus event does if the window gets one), while "visible" for a parked window loses the HUD close and the
/// no-tray notice. A move that was never sent, or was refused, proves nothing.
#[cfg(target_os = "linux")]
fn hide_verdict(state: HideState, move_may_have_landed: bool) -> bool {
    match state {
        HideState::Hidden => true,
        HideState::Unknown => move_may_have_landed,
        HideState::OverlayOnFocused | HideState::NotHidden => false,
    }
}

#[cfg(target_os = "linux")]
fn hide_main_windows_within(steps: std::time::Duration, final_check: std::time::Duration) -> bool {
    use std::time::Instant;

    if !is_hyprland() {
        return false;
    }
    let start = Instant::now();
    let steps_deadline = start + steps;
    let final_deadline = start + steps + final_check;
    let Some(windows) = our_main_windows(Some(steps_deadline)) else {
        return false;
    };
    let addresses: Vec<String> = windows.iter().map(|c| c.address.clone()).collect();
    let mut may_have_landed = false;
    for window in &windows {
        let selector = address_selector(&window.address);
        let outcome = dispatch_move_then_legacy(
            &lua_hide_command(&selector),
            &legacy_hide_command(&selector),
            Some(steps_deadline),
        );
        // Moving a window that already sits on the hidden workspace changes
        // nothing, so an `ok` there says nothing about the overlay.
        if window.workspace != HIDDEN_WORKSPACE {
            may_have_landed |= outcome.may_have_landed();
        }
    }
    let mut state = check_hidden(&addresses, Some(steps_deadline));
    if state == HideState::Hidden {
        return true;
    }
    if state == HideState::OverlayOnFocused {
        // Parked but shown as an overlay on the focused monitor (a focus-only
        // restore). One toggle: the dispatcher never repeats it as legacy
        // after an unanswered Lua attempt.
        let toggled = dispatch_move_then_legacy(
            &lua_toggle_hidden_command(),
            &legacy_toggle_hidden_command(),
            Some(steps_deadline),
        );
        may_have_landed |= toggled.may_have_landed();
    }
    state = check_hidden(&addresses, Some(final_deadline));
    hide_verdict(state, may_have_landed)
}

#[cfg(target_os = "linux")]
fn check_hidden(addresses: &[String], deadline: Option<std::time::Instant>) -> HideState {
    let clients = hyprland_ipc_until("j/clients", deadline)
        .ok()
        .and_then(|reply| parse_hypr_clients(&reply));
    let overlay = hyprland_ipc_until("j/monitors", deadline)
        .ok()
        .and_then(|reply| overlay_state(&reply));
    classify_hide(clients.as_deref(), addresses, overlay)
}

#[cfg(target_os = "linux")]
fn lua_toggle_hidden_command() -> String {
    r#"/dispatch hl.dsp.workspace.toggle_special("rusticdl")"#.to_string()
}

#[cfg(target_os = "linux")]
fn legacy_toggle_hidden_command() -> String {
    "/dispatch togglespecialworkspace rusticdl".to_string()
}

/// Overall bound for a restore on the UI thread.
#[cfg(target_os = "linux")]
const SHOW_BUDGET: std::time::Duration = std::time::Duration::from_millis(1200);

/// Bring the main window back to the user's regular workspace and focus it.
///
/// Returns whether a move dispatch succeeded. Focus-only (no numeric
/// workspace, or the compositor refused the move) is `false`. Every socket
/// call shares one [`SHOW_BUDGET`] deadline.
#[cfg(target_os = "linux")]
pub fn show_main_windows() -> bool {
    if !is_hyprland() {
        return false;
    }
    let deadline = Some(std::time::Instant::now() + SHOW_BUDGET);
    let Some(windows) = our_main_windows(deadline) else {
        return false;
    };
    let monitors = hyprland_ipc_until("j/monitors", deadline).ok();
    let active = hyprland_ipc_until("j/activeworkspace", deadline).unwrap_or_default();
    let workspace_id = restore_workspace_id(&active, monitors.as_deref());
    let mut moved = false;
    for window in windows {
        let selector = address_selector(&window.address);
        if let Some(workspace_id) = workspace_id.as_deref() {
            moved |= dispatch_move_then_legacy(
                &lua_show_command(workspace_id, &selector),
                &legacy_show_command(workspace_id, &selector),
                deadline,
            ) == MoveOutcome::Accepted;
        }
        let _ = dispatch_move_then_legacy(
            &lua_focus_command(&selector),
            &legacy_focus_command(&selector),
            deadline,
        );
    }
    moved
}

/// This process's main-window clients (never capture HUDs).
#[cfg(target_os = "linux")]
fn our_main_windows(deadline: Option<std::time::Instant>) -> Option<Vec<HyprClient>> {
    let reply = hyprland_ipc_until("j/clients", deadline).ok()?;
    let clients = parse_hypr_clients(&reply)?;
    let windows: Vec<HyprClient> = main_window_clients(&clients, std::process::id())
        .into_iter()
        .cloned()
        .collect();
    (!windows.is_empty()).then_some(windows)
}

#[cfg(target_os = "linux")]
fn hyprland_ipc(command: &str) -> std::io::Result<String> {
    hyprland_ipc_until(command, None)
}

/// Outcome of one socket round trip, keeping "never written" apart from
/// "written, no usable reply".
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, PartialEq, Eq)]
enum IpcResult {
    /// No connection, or the deadline was already gone before the write.
    NotSent,
    /// The command was written but the reply was empty, late or cut off.
    NoReply,
    /// A complete reply (the stream ended or returned a short final read).
    Reply(String),
}

#[cfg(target_os = "linux")]
fn hyprland_ipc_until(
    command: &str,
    deadline: Option<std::time::Instant>,
) -> std::io::Result<String> {
    match hyprland_call(command, deadline) {
        IpcResult::Reply(reply) => Ok(reply),
        IpcResult::NoReply => Ok(String::new()),
        IpcResult::NotSent => Err(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "hyprland ipc not sent",
        )),
    }
}

#[cfg(target_os = "linux")]
fn connect_within(
    path: String,
    deadline: Option<std::time::Instant>,
) -> Option<std::os::unix::net::UnixStream> {
    use std::os::unix::net::UnixStream;

    let Some(deadline) = deadline else {
        return UnixStream::connect(path).ok();
    };
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return None;
    }
    // `connect` has no timeout (a full listen backlog blocks it), so run it on
    // a helper thread and stop waiting at the deadline. The late stream, if
    // any, is dropped unused.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("rusticdl-hypr-connect".into())
        .spawn(move || {
            let _ = tx.send(UnixStream::connect(path));
        })
        .ok()?;
    rx.recv_timeout(remaining).ok()?.ok()
}

/// One IPC round trip. With a `deadline` the *whole call* (connect, write and
/// every read) is bounded by it, so neither a compositor that drips bytes nor
/// one that never accepts can stretch it.
#[cfg(target_os = "linux")]
fn hyprland_call(command: &str, deadline: Option<std::time::Instant>) -> IpcResult {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    const PER_READ: Duration = Duration::from_millis(250);
    let slice = |deadline: Option<Instant>| -> Option<Duration> {
        match deadline {
            None => Some(PER_READ),
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                (!remaining.is_zero()).then(|| remaining.min(PER_READ))
            }
        }
    };

    let Ok(his) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") else {
        return IpcResult::NotSent;
    };
    let runtime =
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{}", unix_uid()));
    let path = format!("{runtime}/hypr/{his}/.socket.sock");

    let Some(mut stream) = connect_within(path, deadline) else {
        return IpcResult::NotSent;
    };
    let Some(write_slice) = slice(deadline) else {
        return IpcResult::NotSent;
    };
    if stream.set_write_timeout(Some(write_slice)).is_err() {
        return IpcResult::NotSent;
    }
    if stream.write_all(command.as_bytes()).is_err() {
        return IpcResult::NotSent;
    }

    let mut buf = Vec::with_capacity(256);
    let mut chunk = [0_u8; 512];
    let mut complete = false;
    while let Some(read_slice) = slice(deadline) {
        if stream.set_read_timeout(Some(read_slice)).is_err() {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                complete = true;
                break;
            }
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    complete = true;
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if complete && !buf.is_empty() {
        IpcResult::Reply(String::from_utf8_lossy(&buf).into_owned())
    } else {
        // Closed without a reply, or cut off mid-reply: never a verdict.
        IpcResult::NoReply
    }
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
