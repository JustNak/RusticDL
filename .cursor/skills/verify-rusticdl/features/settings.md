# Settings

Settings lets a user open the settings shell, switch categories (General, Download Engine, System, Browser, Appearance), change values, and persist them with Save settings.

## Sub-features

- `settings-open` opens Settings from the sidebar or Ctrl+,.
- `settings-categories` switches the mini-nav among General, Download Engine, System, Browser, and Appearance.
- `settings-save` persists draft values with Save settings.
- `settings-reset` restores defaults in the draft (still requires Save settings to persist).
- `settings-browser-host` shows Register browser host on the Browser category (native messaging).

## How to get to it (user POV)

- Choose `Settings` under APP in the left sidebar.
- Press Ctrl+, while focus is outside a text field.
- From Settings, use Back in the settings sidebar to return to the queue.

## Driving it with control-rusticdl

Preconditions:

- Doctor is green.
- You are willing to mutate `settings.json` under this run's `XDG_DATA_HOME` only.

- **Open Settings.** Choose `Settings`. Run `.cursor/skills/verify-rusticdl/scripts/ui.sh click-sidebar 'Settings'`. The settings sidebar appears with categories; the main pane shows General by default.
- **Open Appearance.** Choose `Appearance`. Run `.cursor/skills/verify-rusticdl/scripts/ui.sh click-sidebar 'Appearance'`. The Appearance panel is titled Appearance.
- **Save.** Choose `Save settings` in the sticky footer. Run `.cursor/skills/verify-rusticdl/scripts/ui.sh click-save-settings`. A toast or unchanged footer confirms the write; `$XDG_DATA_HOME/RusticDL/settings.json` mtime updates.
- **Leave.** Choose Back / leave settings. Run `.cursor/skills/verify-rusticdl/scripts/ui.sh leave-settings`. Queue chrome (search + Add download) returns.
- **Proof.** Screenshot the Appearance (or General) panel and copy `settings.json` beside `status.json` into `.cursor/skills/verify-rusticdl/artifacts/settings/$RUN_ID/`.

## Gotchas

- Reset defaults only updates the draft — Save settings is still required to persist.
- Browser capture settings sync to the extension only when the native host is connected; desktop-only proof does not require a browser.
- Do not point verification at the user's real `%APPDATA%\RusticDL` / `~/.local/share/RusticDL` — always isolate `XDG_DATA_HOME` on Linux.
