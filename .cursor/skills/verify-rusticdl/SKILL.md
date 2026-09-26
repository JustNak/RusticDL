---
name: verify-rusticdl
description: "Drive and prove RusticDL's GPUI desktop app (Windows + Linux) with launch, doctor, xdotool/IPC harness, screenshots, and cleanup. Use when verifying queue, add-download, settings, search, or pause/resume behavior, or when a UI/IPC change needs cold-agent proof."
---

# Verify RusticDL

Project-local verification for **RusticDL**, a local-first HTTP(S) download manager. Primary surface is the **GPUI desktop app** on Windows and Linux. Secondary surfaces (do not treat as the default drive target): browser extension (`apps/extension`) and native messaging host (`apps/native-host`).

Read `features/README.md` before driving. Drive one mapped feature per proof unless the task asks for more.

## Interview summary

| Question | Answer from this repo |
| --- | --- |
| Surface | GPUI desktop window titled `RusticDL` (`src/main.rs`, `src/app/`). Extension + native host are opt-in handoff. |
| Run | `cargo build -p rusticdl` then run `target/debug/rusticdl` (or `cargo run -p rusticdl`). Needs Rust **1.89+**, C++ linker (`g++` / `libstdc++`), `libxcb`, `libxkbcommon`, `libxkbcommon-x11`, a working `DISPLAY` + `XAUTHORITY`, and a Vulkan device (real GPU or Mesa **lavapipe**). |
| Drive | `control-rusticdl` scripts in this skill: xdotool clicks for UI; Unix socket JSON for IPC (`get_status`, `enqueue_download`, `show_window`). Prefer visible button labels (`Add download`, `Settings`, `Start download`) over coordinates when a vision agent is available. |
| Observe | Window screenshots, IPC `get_status` JSON (`queueSummary`, `appVersion`), `$XDG_DATA_HOME/RusticDL/state.json` and `settings.json`, downloaded files under `$VERIFY_RUN_DIR/downloads`. |
| Isolate | **Linux:** yes — distinct `XDG_RUNTIME_DIR` / `XDG_DATA_HOME`, seeded `downloadDirectory` under the run dir, clicks bound to the pidfile window. **Windows:** helpers exit immediately (mutex + `%APPDATA%` are not isolated). Never drive an instance you did not start. |

## Launch

Use the helpers (invocation is literal):

```bash
# From repo root. Creates /tmp/rusticdl-verify-$RUN_ID/{runtime,data,evidence,app.pid,app.log}
export RUN_ID="${RUN_ID:-$(date +%Y%m%d%H%M%S)}"
source .cursor/skills/verify-rusticdl/scripts/env.sh
.cursor/skills/verify-rusticdl/scripts/launch.sh
```

Ready when:

1. `scripts/launch.sh` prints `READY pid=<n> wid=<n>`
2. That pid owns exactly one window titled `RusticDL` (`xdotool getwindowpid` matches the pidfile)
3. `scripts/doctor.sh` exits 0 (including `downloadDirectory` under `$VERIFY_RUN_DIR`)

`launch.sh` refuses non-Linux hosts and refuses to start when `rusticdl-native-host` sits next to the desktop binary (that sibling rewrites browser native-messaging manifests). On any launch failure it kills the pidfile process.

Teardown: `scripts/cleanup.sh` (kills only the pid in `$VERIFY_RUN_DIR/app.pid`).

### Environment prerequisites (Linux agents)

```bash
# Toolchain (docs/build.md): Rust 1.89+
rustup install 1.89.0 && rustup default 1.89.0

# Link + capture tools (Ubuntu 24.04 example)
sudo apt-get install -y g++ libstdc++-14-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libxcb1-dev mesa-vulkan-drivers imagemagick xdotool

# libstdc++.so often lives only under gcc's private dir — export before cargo/link:
export LIBRARY_PATH="/usr/lib/gcc/x86_64-linux-gnu/13${LIBRARY_PATH:+:$LIBRARY_PATH}"

# Headless / VNC without a real GPU — force lavapipe:
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
export VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json
```

Keep `HOME` and `XAUTHORITY` pointing at the real desktop user so X11 auth works. Isolation uses `XDG_*` only — do not replace `HOME` for verification runs.

## Doctor

Read-only health check for the instance started by `launch.sh`:

```bash
source .cursor/skills/verify-rusticdl/scripts/env.sh   # same RUN_ID
.cursor/skills/verify-rusticdl/scripts/doctor.sh
```

Doctor requires all of:

- Pidfile process still alive and is the rusticdl binary path recorded at launch
- Window titled `RusticDL` still mapped and owned by that pid (zero or >1 matches is a fail)
- IPC `get_status` returns `"ok": true`, `"appState": "running"`, and `"connectionState": "connected"`
- Response `appVersion` is non-empty
- Data root is under this run's `XDG_DATA_HOME` (`…/RusticDL/settings.json` exists)
- `settings.json` `downloadDirectory` is a path under `$VERIFY_RUN_DIR` (not the user's Downloads folder)

If anything looks off, run doctor before retrying a drive.

## Drive

Harness name: **control-rusticdl** (scripts under `.cursor/skills/verify-rusticdl/scripts/`).

```bash
# IPC (newline-delimited JSON on the Unix socket / named pipe)
.cursor/skills/verify-rusticdl/scripts/ipc.sh get_status
.cursor/skills/verify-rusticdl/scripts/ipc.sh show_window

# UI — focus window, click by label heuristics, screenshot
.cursor/skills/verify-rusticdl/scripts/ui.sh focus
.cursor/skills/verify-rusticdl/scripts/ui.sh click-add-download
.cursor/skills/verify-rusticdl/scripts/ui.sh click-start-download
.cursor/skills/verify-rusticdl/scripts/ui.sh click-sidebar 'Settings'
.cursor/skills/verify-rusticdl/scripts/screenshot.sh home.png
```

Stable handles (prefer these over raw coordinates):

| Handle | Where |
| --- | --- |
| Window title `RusticDL` | Main window |
| Button label `Add download` | Title bar |
| Dialog title `Add download` / button `Start download` | Add dialog |
| Sidebar labels `All downloads`, `Active`, `Completed`, `Failed`, `Settings`, `About` | Left nav |
| Search placeholder `Search name, URL, or path…` | Title bar (`/` focuses it) |
| Shortcut Ctrl+N | Open add dialog (when no text field focused) |
| Shortcut Ctrl+, | Open Settings |
| IPC types `get_status`, `enqueue_download`, `prompt_download`, `show_window` | Socket/pipe |

GPUI does not expose ARIA/CDP. When label hit-testing is unavailable, `ui.sh` falls back to geometry relative to the launched pid's `RusticDL` window (documented in that script). A vision/`computerUse` agent may click by visible label instead — still record the same evidence paths.

**Text entry caveat:** GPUI `InputState` fields often ignore xdotool-typed keys and clipboard paste from automation. For Add download / Search / Settings inputs, drive typing with a vision/`computerUse` agent (click the field, type the literal string). Mouse clicks for `Add download`, `Start download`, and sidebar labels via `ui.sh` are reliable.

Feature recipes live in `features/`. Start from baseline (doctor green, empty or known queue).

## Evidence

Named durable location (survives cleanup):

```text
.cursor/skills/verify-rusticdl/artifacts/<feature-id>/<RUN_ID>/
```

Also keep the run working copy while the app is up:

```text
$VERIFY_RUN_DIR/evidence/   # default /tmp/rusticdl-verify-$RUN_ID/evidence/
```

`scripts/cleanup.sh` removes the process and may remove `$VERIFY_RUN_DIR/runtime` scratch, but **never** deletes `.cursor/skills/verify-rusticdl/artifacts/` or `$VERIFY_RUN_DIR/evidence/` once proofs are copied into `artifacts/`.

Proof standards:

- Exercise the real user path (button / sidebar / dialog), not only IPC enqueue, unless the feature file says IPC is an entry point.
- Capture **action** and **result** (dialog with URL + queue/status after start).
- Verify side effects: `get_status.queueSummary` and/or `state.json` and/or file on disk.
- Record `feature-id`, `RUN_ID`, and commands used next to the artifacts (`meta.txt`).

Minimum set for a UI feature (`scripts/prove-add-download.sh finish` requires these names):

1. `dialog-filled.png` — identity `RusticDL` visible, URL entered
2. `queue-after.png` — resulting queue state
3. `status-after.json` — IPC `get_status` after the action
4. `meta.txt` — feature id, RUN_ID, pid, commands

The prepare pair (captured by `prove-add-download.sh prepare`) is `dialog-open.png` and `status-before.json`.

## Cleanup

```bash
source .cursor/skills/verify-rusticdl/scripts/env.sh
.cursor/skills/verify-rusticdl/scripts/cleanup.sh
```

Rules:

- Kill only the pid in `$VERIFY_RUN_DIR/app.pid` (never `pkill -f rusticdl` — that matches unrelated shells).
- Copy evidence into `.cursor/skills/verify-rusticdl/artifacts/<feature-id>/<RUN_ID>/` **before** deleting any temp dirs.
- Leave durable artifacts on disk; confirm they still exist after cleanup.

## Helpers

| Script | Purpose |
| --- | --- |
| `scripts/env.sh` | Resolve `RUN_ID`, `VERIFY_RUN_DIR`, XDG isolation, Vulkan, `LIBRARY_PATH` |
| `scripts/launch.sh` | Build if needed, start app, wait for window + socket, write pidfile |
| `scripts/doctor.sh` | Read-only instance health |
| `scripts/ipc.sh` | Send one IPC request; print response line |
| `scripts/ui.sh` | Focus / click helpers for the launched pid's desktop window |
| `scripts/screenshot.sh` | Capture that window into the evidence dir (fails if the grab misses) |
| `scripts/cleanup.sh` | Stop the launched pid; preserve evidence + artifacts |
| `scripts/prove-add-download.sh` | End-to-end proof of the `add-download` feature |

All scripts are executable. Source `env.sh` in the same shell before calling the others when you set `RUN_ID` yourself.

## Maintenance

When UI labels, IPC types, or launch requirements change, run `/maintain-verification-skill` and update the feature map plus this file together.
