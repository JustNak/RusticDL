use super::*;

fn window_title_selector(title: &str) -> String {
    format!("title:^{}$", escape_ere(title))
}

fn client(address: &str, class: &str, title: &str, pid: u32, floating: bool) -> HyprClient {
    HyprClient {
        address: address.to_string(),
        class: class.to_string(),
        title: title.to_string(),
        pid,
        floating,
    }
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
fn address_selector_prefixes_hyprland_address() {
    assert_eq!(address_selector("0xabc"), "address:0xabc");
    assert_eq!(address_selector("address:0xabc"), "address:0xabc");
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
fn lua_resize_and_center_take_a_window() {
    let selector = address_selector("0xabc");
    let resize = lua_resize_command(&selector, 480, 268);
    assert!(
        resize.starts_with("/dispatch hl.dsp.window.resize("),
        "0.55 resize is Lua, got {resize}"
    );
    assert!(
        resize.contains("x = 480") && resize.contains("y = 268"),
        "exact HUD pixels, got {resize}"
    );
    assert!(
        resize.contains(r#"window = "address:0xabc""#),
        "resize must not target the focused window, got {resize}"
    );
    assert!(
        !resize.contains("relative"),
        "omit relative so x/y are exact pixels, got {resize}"
    );
    assert!(
        !resize.contains("resizewindowpixel"),
        "Lua payload must not use the dropped hyprlang resize, got {resize}"
    );
    let center = lua_center_command(&selector);
    assert!(
        center.starts_with("/dispatch hl.dsp.window.center("),
        "0.55 center is Lua, got {center}"
    );
    assert!(
        center.contains(r#"window = "address:0xabc""#),
        "center must not rely on focus, got {center}"
    );
    assert!(
        !center.contains("centerwindow"),
        "Lua payload must not use bare centerwindow, got {center}"
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
    let match_cmd = named_rule_keyword("match:class", &format!("^{}$", escape_ere(CAPTURE_APP_ID)));
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
        named_rule_keyword("no_anim", "on"),
        "/keyword windowrule[rusticdl-capture]:no_anim on"
    );
    assert_ne!(
        named_rule_keyword("animation", "none"),
        named_rule_keyword("no_anim", "on"),
        "animation none forces a style; disable is no_anim on"
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
fn parse_hypr_clients_reads_address_class_title_pid() {
    let json = r#"[{"address":"0xaaa","class":"rusticdl-capture","title":"RusticDL — Confirm Download","pid":42,"floating":true},{"address":"0xbbb","class":"RusticDL","title":"RusticDL","pid":42,"floating":false}]"#;
    let clients = parse_hypr_clients(json).expect("clients json");
    assert_eq!(clients.len(), 2);
    assert_eq!(clients[0].address, "0xaaa");
    assert_eq!(clients[0].class, CAPTURE_APP_ID);
    assert!(clients[0].floating);
    assert_eq!(clients[1].class, "RusticDL");
    assert!(!clients[1].floating);
    assert!(parse_hypr_clients("ok").is_none());
    assert!(parse_hypr_clients("error: nope").is_none());
}

#[test]
fn pick_does_not_select_older_same_title_hud() {
    let title = "RusticDL — Confirm Download";
    let pid = 7;
    let prior = CaptureWindowSnapshot {
        addresses: vec!["0xold".into()],
        queried: true,
    };
    let only_old = [client("0xold", CAPTURE_APP_ID, title, pid, true)];
    assert_eq!(
        pick_new_capture_address(&prior, &only_old, pid, title),
        None,
        "0ms / pre-map must not treat the already-open HUD as the new one"
    );
}

#[test]
fn pick_selects_new_address_when_titles_collide() {
    let title = "RusticDL — Confirm Download";
    let pid = 7;
    let prior = CaptureWindowSnapshot {
        addresses: vec!["0xold".into()],
        queried: true,
    };
    let both = [
        client("0xold", CAPTURE_APP_ID, title, pid, true),
        client("0xnew", CAPTURE_APP_ID, title, pid, false),
    ];
    assert_eq!(
        pick_new_capture_address(&prior, &both, pid, title).as_deref(),
        Some("0xnew")
    );
}

#[test]
fn pick_prefers_capture_class_among_newcomers() {
    let prior = CaptureWindowSnapshot {
        addresses: vec![],
        queried: true,
    };
    let clients = [
        client("0xmain", "RusticDL", "RusticDL", 7, false),
        client(
            "0xhud",
            CAPTURE_APP_ID,
            "RusticDL — Confirm Download",
            7,
            false,
        ),
    ];
    assert_eq!(
        pick_new_capture_address(&prior, &clients, 7, "RusticDL — Confirm Download").as_deref(),
        Some("0xhud")
    );
}

#[test]
fn pick_without_prior_only_unique_tiled_capture() {
    let prior = CaptureWindowSnapshot::default();
    let title = "RusticDL — Downloading";
    let two_floating = [
        client("0xa", CAPTURE_APP_ID, title, 7, true),
        client("0xb", CAPTURE_APP_ID, title, 7, true),
    ];
    assert_eq!(
        pick_new_capture_address(&prior, &two_floating, 7, title),
        None,
        "without a snapshot, do not guess among already-floating same-title HUDs"
    );
    let one_tiled = [client("0xc", CAPTURE_APP_ID, title, 7, false)];
    assert_eq!(
        pick_new_capture_address(&prior, &one_tiled, 7, title).as_deref(),
        Some("0xc")
    );
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
        production.contains("lua_resize_command"),
        "0.55 size fallback must speak Lua resize, not only hyprlang"
    );
    assert!(
        production.contains("lua_center_command"),
        "0.55 center fallback must speak Lua center, not only hyprlang"
    );
    assert!(
        production.contains("prepare_capture_window"),
        "pre-map rules are required to avoid tile-then-float"
    );
    assert!(
        production.contains("CAPTURE_APP_ID"),
        "capture HUDs need a dedicated app id for class rules"
    );
    assert!(
        production.contains("pick_new_capture_address"),
        "post-map IPC must pick the newly mapped address"
    );
    assert!(
        production.contains("address_selector"),
        "post-map dispatch must target address:, not the first title match"
    );
    assert!(
        production.contains("no_anim"),
        "0.53+ disables open animation with no_anim on"
    );
    assert!(
        !production.contains("\"animation\", \"none\""),
        "animation none forces a style and still plays the open anim"
    );
    let float_fn = production
        .split("pub fn float_capture_windows")
        .nth(1)
        .expect("float_capture_windows");
    assert!(
        !float_fn
            .split("pub fn")
            .next()
            .unwrap_or(float_fn)
            .contains("window_title_selector"),
        "float thread must not dispatch on a shared title regex"
    );
}

#[test]
fn float_and_prepare_are_noop_off_hyprland() {
    let _guard = env_lock();
    unsafe { std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE") };
    // Must not panic or hang when HIS is unset (no socket connect).
    let prior = prepare_capture_window(480, 268);
    assert_eq!(prior, CaptureWindowSnapshot::default());
    float_capture_windows("RusticDL — Confirm Download", 480, 268, prior);
}
