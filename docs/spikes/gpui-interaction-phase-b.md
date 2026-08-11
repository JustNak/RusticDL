# GPUI interaction spike (Phase B)

**Gate:** PR-06.5 — prove modifier-click, drop target, focus clipboard APIs before multi-select (PR-07), DnD (PR-09), and clipboard watch (PR-10).

**Stack:** `gpui = 0.2.2`, `gpui-component = 0.5.1`, Windows.

**Probe code (debug-only):**

| Capability | Location |
|---|---|
| Modifier-click | `src/app/job_row.rs` — `#[cfg(debug_assertions)]` log in row `on_click` |
| File drop target | `src/app/queue_view.rs` — `can_drop` / `on_drop` on `#queue-scroll` and empty state |
| Clipboard on focus | `src/app/mod.rs` — `observe_window_activation` + `read_from_clipboard` |

Run a **debug** build, click rows with Ctrl/Shift held, drop files onto the queue, and alt-tab back into the window; watch stderr for `[spike]` lines. Release builds strip all probes.

---

## Modifier click

- **Result:** WORKS
- **Evidence:**
  - `gpui::ClickEvent::modifiers() -> Modifiers` (`gpui-0.2.2/src/interactive.rs`) returns mouse-up modifiers for `ClickEvent::Mouse`.
  - `Modifiers` fields: `control`, `shift`, `alt`, `platform`, `function`; helpers `secondary()` (Ctrl on Win/Linux, Cmd on macOS), `modified()`, `control_shift()`.
  - `MouseDownEvent` / `MouseUpEvent` also carry `modifiers: Modifiers`.
  - `on_click` signature: `Fn(&ClickEvent, &mut Window, &mut App)`.
  - Existing production pattern in gpui-component list: `e.modifiers().secondary()` on confirm (`gpui-component-0.5.1/src/list/list.rs`).
  - App probe: `src/app/job_row.rs` logs non-default modifiers under `debug_assertions`.

```rust
// Pattern for PR-07 (same as probe; event was previously ignored as `_`)
.on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
    let m = event.modifiers();
    if m.secondary() {
        // toggle-add to multi-selection
    } else if m.shift {
        // range select from anchor
    } else {
        // replace selection
    }
    // ...
}))
```

- **Caveats:**
  - Keyboard-synthesized clicks (`ClickEvent::Keyboard`) always report empty modifiers (by design).
  - Use `secondary()` for “Ctrl-click multi-select” so macOS Cmd maps correctly later.
- **Recommendation for PR-07:** **Ship ctrl/shift multi-select** via `ClickEvent::modifiers()`. Checkbox column is optional UX polish, not a fallback for missing APIs.

---

## Drag and drop

- **Result:** PARTIAL
- **Evidence:**
  - **In-app DnD:** first-class. `on_drag` / `on_drag_move` / `on_drop` / `can_drop` on elements (`gpui-0.2.2/src/elements/div.rs`); typed payload via `TypeId`; example at `gpui-0.2.2/examples/drag_drop.rs`.
  - **External OS file drops:** WORKS. Platform raises `FileDropEvent::{Entered, Pending, Submit, Exited}` with `ExternalPaths`. Window layer promotes `Entered` into `active_drag = AnyDrag { value: ExternalPaths, ... }` and `Submit` into synthetic `MouseUp`, which fires `on_drop::<ExternalPaths>` (`gpui-0.2.2/src/window.rs` ~3620–3660).
  - **Windows platform path:** `IDropTarget` only queries **`CF_HDROP`** (`gpui-0.2.2/src/platform/windows/window.rs` `DragEnter`). Non-HDROP data → `DROPEFFECT_NONE` (drop rejected).
  - **Plain text / URL string drops:** DOES_NOT_WORK via GPUI DnD on Windows. No `CF_UNICODETEXT` / URL format handling in the drop target.
  - App probe: `src/app/queue_view.rs` `#queue-scroll` + empty state accept `ExternalPaths` under `debug_assertions`.

```rust
// File-path drops (works on Windows)
div()
    .id("queue-scroll")
    .can_drop(|drag, _, _| drag.is::<ExternalPaths>())
    .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
        for path in paths.paths() {
            // e.g. open local .url / parse file list — NOT freeform http(s) from browser
        }
    }))
```

- **Implication for “drop URLs on queue” product goal:**
  - Dragging **files** (Explorer, `.url` files, some apps that put HDROP) → shippable.
  - Dragging **selected text or browser address-bar / link as text** → not supported by current GPUI Windows backend without forking/patching `IDropTarget` to also accept `CF_UNICODETEXT` (and/or `UniformResourceLocatorW`).
  - Clipboard-paste and “add download” dialog remain the reliable path for freeform URLs until that lands upstream or in-tree.
- **Recommendation for PR-09:** **Ship file-path DnD** (`ExternalPaths` → optional local-file / `.url` handling). **Defer pure text/URL OS drops** unless we invest in a platform patch. Do not block Phase B on text-DnD; pair with clipboard-on-focus / paste for URL capture.

---

## Clipboard on focus

- **Result:** WORKS
- **Evidence:**
  - `App::read_from_clipboard() -> Option<ClipboardItem>` / `write_to_clipboard` (`gpui-0.2.2/src/app.rs`).
  - Windows backend reads `CF_UNICODETEXT`, images, and `CF_HDROP` (`gpui-0.2.2/src/platform/windows/clipboard.rs`).
  - `ClipboardItem::text() -> Option<String>` concatenates string entries.
  - Window activation: `Context::observe_window_activation(window, |view, window, cx| …)` + `Window::is_window_active()` (`gpui-0.2.2/src/app/context.rs`, `window.rs`). Fires on activate **and** deactivate — gate with `is_window_active()`.
  - Focus within the tree (`on_focus_in` / `on_focus_out`) is for keyboard focus handles, not OS window activation; use **activation** for “user came back to the app”.
  - App already uses write-side UI via `gpui_component::clipboard::Clipboard` (copy buttons); read path was unused until this spike.
  - App probe: `src/app/mod.rs` `DownloadApp::new` debug activation observer.

```rust
// Pattern for PR-10 (opt-in settings flag; debounce / dedupe recommended)
cx.observe_window_activation(window, |this, window, cx| {
    if !window.is_window_active() || !this.settings.watch_clipboard_on_focus {
        return;
    }
    if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
        // parse URL(s); toast “Add from clipboard?”; avoid re-prompting same payload
    }
})
.detach();
```

- **Caveats:**
  - Read may fail if another app holds the clipboard open (`OpenClipboard` returns error) — treat as empty.
  - Fires on every activation; product must debounce, fingerprint last-seen payload, and keep **opt-in** (privacy / surprise).
  - Prefer main-window only (this subscription is already scoped to the main `Window` in `DownloadApp::new`).
- **Recommendation for PR-10:** **Ship opt-in clipboard watch on window activation** using `observe_window_activation` + `read_from_clipboard`. No alternate trigger required for the MVP path; a manual “Paste URL” action remains useful as always-available fallback.

---

## Summary matrix

| Capability | Ship in Phase B? | Approach |
|---|---|---|
| Modifier-click (Ctrl/Shift multi-select) | **Yes** | `ClickEvent::modifiers()` / `secondary()` / `shift` on job-row `on_click` |
| OS file drop onto queue/empty | **Yes** | `can_drop` + `on_drop::<ExternalPaths>` on queue surface |
| OS plain text / URL drag-drop | **No (defer)** | GPUI Windows drop target is CF_HDROP-only; need platform work or non-DnD UX |
| Clipboard read on window focus | **Yes (opt-in)** | `observe_window_activation` + `is_window_active` + `read_from_clipboard` |
| Checkbox multi-select column | Optional UX | Not required as API fallback |
| Manual paste / Add dialog | Keep | Primary freeform URL path until text-DnD exists |

### Fail-closed notes

- None of the three gates are fully missing APIs.
- The only fail-closed product claim: **do not advertise “drop any URL from the browser onto the queue”** until text/URL formats are accepted by the Windows drop target. File drops and clipboard-on-focus cover most of the intent without lying about capabilities.
