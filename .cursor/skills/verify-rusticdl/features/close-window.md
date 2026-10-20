# Close window

Closing the main window does not quit while Close to tray is on (the default). On Linux, Ctrl+Q quits. What the close button does depends on the desktop: tray hide, Hyprland special-workspace hide, minimize, or no hide at all when this session cannot minimize.

## Sub-features

- `close-keeps-process` — the caption close button leaves the `rusticdl` pid running.
- `close-hide-or-minimize` — the window is hidden or minimized when this desktop is one `hide_main_window` supports.
- `ctrl-q-quits` — Ctrl+Q on the main window exits the process.

## How to get to it (user POV)

- Click the window's close button (Linux caption close, or the GPUI close button when caption buttons are on the right).
- The setting is System → Close to tray, saved with Save settings. Default is on (`close_to_tray: true` in `src/settings/model.rs`).
- Press Ctrl+Q to quit after the window has been closed or hidden (`is_quit_chord` in `src/app/tray_lifecycle.rs`, handled in `src/app/mod.rs`).
- The brand menu's Exit item also quits. This recipe uses Ctrl+Q.

## Driving it with control-rusticdl

Preconditions:

- Doctor is green for this `RUN_ID`.
- `XDG_CURRENT_DESKTOP` is whatever the session actually has. Do not export a desktop name to force a branch.
- This is not a Hyprland session unless you are on Hyprland. The Xvfb route does not exercise Hyprland hide/restore.

- **Record the live pid.** `cat "$VERIFY_RUN_DIR/app.pid"`.
- **Close.** Run `.cursor/skills/verify-rusticdl/scripts/ui.sh click-window-close`.
- **Process.** The pid from `app.pid` is still alive (`kill -0`). `scripts/ipc.sh get_status` still returns `"ok": true`.
- **Window.** `xwininfo -id "$(cat "$VERIFY_RUN_DIR/window.id")"` still runs. Record whether the map state is `IsViewable` or `IsUnMapped`. On a desktop that is not in the minimize list and has no tray, `hide_main_window` returns false (`src/tray.rs`) and the window stays viewable while the process stays up.
- **Quit.** Run `.cursor/skills/verify-rusticdl/scripts/ui.sh quit`. The pid exits.
- **Proof.** Save `status-after-close.json` from `get_status` and `meta.txt` (pid before, map state, pid after Ctrl+Q) under `.cursor/skills/verify-rusticdl/artifacts/close-window/$RUN_ID/`.

`scripts/doctor.sh` requires a mapped `RusticDL` window. After a hide that unmaps the window, doctor fails even though the process is the one this run started. Use the pid and `get_status` checks above for this feature. `scripts/cleanup.sh` is still how you stop a process that Ctrl+Q did not exit.

## Gotchas

- Close to tray defaults on. A recipe that closes the window and then expects the process to die will hang until `cleanup.sh`.
- With no StatusNotifier host, the action is not HideToTray. `decide_close_action` in `src/app/tray_lifecycle.rs` picks Minimize on Linux, HideWithoutTray on Hyprland, and Quit when the setting is off.
- `desktop_supports_minimize` (`src/tray.rs`) is false when `XDG_CURRENT_DESKTOP` is unset or is not a listed desktop. The close click then leaves the window on screen.
- Hyprland hide and restore (`src/hyprland.rs`, `src/linux_restore.rs`) are a different route. Do not mark them verified from an Xvfb run.
- `ui.sh click-window-close` clicks the right-edge caption button (x = width − 23, y = 24). Set `VERIFY_CONTROLS_LEFT=1` only when GTK puts close on the left (`sidebar_on_right` in `src/app/layout.rs`); the click is then x = 23.
