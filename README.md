<p align="center">
  <img src="assets/brand/logo.png" alt="RusticDL logo" width="128" height="128">
</p>

<h1 align="center">RusticDL</h1>

<p align="center">
  <strong>A local-first HTTP(S) download manager written in Rust.</strong><br>
  Fast queueing, resume support, rich appearance controls, and optional browser handoff.
</p>

<p align="center">
  <a href="https://github.com/JustNak/RusticDL/releases/latest"><img src="https://img.shields.io/github/v/release/JustNak/RusticDL?style=for-the-badge&label=Download&color=0ea5e9" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-green?style=for-the-badge" alt="MIT License"></a>
  <a href="https://github.com/JustNak/RusticDL/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/JustNak/RusticDL/ci.yml?branch=master&style=for-the-badge&label=CI" alt="CI status"></a>
  <img src="https://img.shields.io/badge/Platform-Windows-0078D6?style=for-the-badge&logo=windows&logoColor=white" alt="Windows">
  <img src="https://img.shields.io/badge/Rust-stable-orange?style=for-the-badge&logo=rust&logoColor=white" alt="Rust">
</p>

<p align="center">
  <a href="#download"><strong>Download</strong></a> ·
  <a href="#features"><strong>Features</strong></a> ·
  <a href="#quick-start"><strong>Quick start</strong></a> ·
  <a href="#browser-extension"><strong>Browser extension</strong></a> ·
  <a href="#build-from-source"><strong>Build from source</strong></a> ·
  <a href="#license"><strong>License</strong></a>
</p>

---

## Download

Get the latest Windows build from GitHub Releases:

**[→ Download RusticDL for Windows](https://github.com/JustNak/RusticDL/releases/latest)**

| Asset | What it contains |
| --- | --- |
| **`RusticDL-windows-x64-setup.exe`** | **Recommended** — NSIS installer (app + native host + browser host registration) |
| **`RusticDL-windows-x64.zip`** | Portable desktop app (`rusticdl.exe`) |
| **`RusticDL-full-windows-x64.zip`** | Portable app + native host + register scripts + browser extension packages |
| **`extension-chromium.zip`** | Chromium / Edge / Brave unpacked extension |
| **`extension-firefox.zip`** | Firefox temporary-add-on package |

### Install (recommended)

1. Download **`RusticDL-windows-x64-setup.exe`**.
2. Run the installer (per-user install; **no administrator rights** required).
3. Launch **RusticDL** from the Start Menu.

The installer places files under `%LOCALAPPDATA%\RusticDL\`, creates a Start Menu shortcut, and registers the browser **native messaging host** for Chrome, Edge, and Firefox.

Settings and queue state live under `%APPDATA%\RusticDL\`. Uninstall via Apps & Features (optionally remove app data).

> **Note:** The installer does **not** auto-install browser extensions. Load the extension separately (see below). SmartScreen may warn on unsigned builds until code signing is added.

### Portable install (ZIP)

1. Download **`RusticDL-windows-x64.zip`** (or the full package if you want browser integration).
2. Extract anywhere you like (for example `C:\Tools\RusticDL\`).
3. Run `rusticdl.exe`.

### Browser handoff (optional)

If you want downloads captured from Firefox / Chromium:

**With the NSIS installer:** the native host is registered automatically. Continue from step 4.

**With a portable ZIP:**

1. Prefer **`RusticDL-full-windows-x64.zip`**.
2. Run the app once so it can create data folders.
3. Register the native messaging host (PowerShell, run from the extracted folder):

```powershell
.\scripts\register-native-host.ps1 `
  -HostBinaryPath "$PWD\rusticdl-native-host.exe" `
  -DesktopBinaryPath "$PWD\rusticdl.exe"
```

4. Load the matching extension package (see [Browser extension](#browser-extension)).
5. For Chromium, re-run the register script with the extension id from `chrome://extensions`.

---

## What is RusticDL?

RusticDL is a **simple, local-first download manager** focused on everyday HTTP/HTTPS files:

- Queue multiple URLs and watch live progress
- Pause / resume when the server supports byte ranges
- Retry, restart, open files, and reveal in Explorer
- Customize look and feel (theme, accent, transparency, density, motion)
- Optionally hand browser downloads to the desktop app via a native messaging host

**Stack**

- **UI:** [GPUI](https://gpui.rs/) (Zed’s GPU-accelerated UI) + [gpui-component](https://github.com/longbridge/gpui-component)
- **Engine:** single-stream downloads with Range resume
- **HTTP:** browser-like client with HTTP/2 and HTTP/3 (QUIC) fallback

### Features

- Add one or many URLs (batch paste, one per line) over HTTP/HTTPS
- Queue filters: All / Active / Completed / Failed
- Search by filename, URL, or path
- Live progress: percent, size, speed, ETA
- Status bar with totals, aggregate speed, and speed limit
- Pause / resume, pause-all / resume-all
- Retry, restart, cancel, remove (with confirmation)
- Clear completed / clear failed
- Detail panel with full job actions (open, reveal, copy path)
- Concurrent downloads, auto-retry with exponential backoff, optional global speed limit
- Nested TLS/network error details (helps diagnose flaky networks)
- Settings for download dir, concurrency, retries, limit, appearance, and data folder
- Appearance: light / dark / system, accents, transparency, blur, film grain, density, corners, vignette, progress styles, reduce motion
- Persisted queue, settings, and window geometry
- Browser extension handoff (Firefox + Chromium)

### Not included (by design)

- Torrents / magnets
- Multi-connection segmented downloads
- Bulk archive finalize workflows

---

## Quick start

### From the installer

Install via **`RusticDL-windows-x64-setup.exe`**, then open **RusticDL** from the Start Menu.

### From a portable ZIP

```powershell
# After extracting the zip
.\rusticdl.exe
```

### From source (developers)

```bash
cargo run
```

Release build:

```bash
cargo build --release
```

### Build the Windows installer (developers)

Requires Windows + Rust. Uses [cargo-packager](https://crates.io/crates/cargo-packager) to produce an NSIS setup executable:

```powershell
# Install packager once (pinned version used by CI)
cargo install cargo-packager --locked --version 0.11.8

# Build binaries + NSIS installer
powershell -ExecutionPolicy Bypass -File scripts/package-windows.ps1
```

Output: `dist-release/RusticDL-windows-x64-setup.exe` (plus the packager’s `rusticdl_*_x64-setup.exe` name).

Packager config lives in `[package.metadata.packager]` in `Cargo.toml`, with a custom NSIS template at `installer/nsis/installer.nsi` that registers/unregisters the native messaging host on install/uninstall.

---

# Build from source

Everything below is for people who want to compile RusticDL themselves, hack on it, or package nightlies.

## Requirements

| Tool | Notes |
| --- | --- |
| **Rust stable** | Install via [rustup](https://rustup.rs/) |
| **Windows C++ build tools** | Visual Studio Build Tools with the “Desktop development with C++” workload (needed by GPUI / native graphics) |
| **Node.js 20+** | Only if you build the browser extension |
| **npm** | Comes with Node |

HTTP/3 support is enabled in this repo via `.cargo/config.toml` (`reqwest_unstable`). You do not need extra env vars for a normal build.

## Workspace layout

```text
.
├── src/                    Desktop app (GPUI)
├── apps/
│   ├── extension/          WebExtension (Firefox + Chromium)
│   └── native-host/        Native messaging bridge (stdio → named pipe)
├── packages/protocol/      Shared TypeScript protocol types
├── scripts/                Windows native-host register / unregister
├── assets/brand/           Icons and logo
└── docs/protocol.md        Extension ↔ app wire format
```

## Build the desktop app

```bash
# Debug
cargo build -p rusticdl

# Release
cargo build --release -p rusticdl
```

Binary output:

- Debug: `target/debug/rusticdl.exe`
- Release: `target/release/rusticdl.exe`

## Build the native messaging host

```bash
cargo build -p rusticdl-native-host
# or
cargo build --release -p rusticdl-native-host
```

Output: `target/*/rusticdl-native-host.exe`

## Tests

```bash
cargo test
```

## Browser extension

The companion WebExtension hands browser downloads to the desktop app over native messaging.

```text
apps/extension/     Firefox + Chromium packages
apps/native-host/   stdio native messaging bridge
packages/protocol/  shared TypeScript protocol types
docs/protocol.md    wire format notes
scripts/            register / unregister native host (Windows)
```

### Dev setup (Windows)

1. Build the desktop app and native host:

```powershell
cargo build -p rusticdl
cargo build -p rusticdl-native-host
```

2. Register the native host (use absolute paths):

```powershell
.\scripts\register-native-host.ps1 `
  -HostBinaryPath "$PWD\target\debug\rusticdl-native-host.exe"
```

3. Build the extension:

```powershell
cd apps\extension
npm install
npm run build
```

4. Load the extension (use the **browser-specific** folder):

   - **Chromium:** `chrome://extensions` → Load unpacked → `apps/extension/dist/chromium`
   - **Firefox:** `about:debugging#/runtime/this-firefox` → **Load Temporary Add-on…** → select  
     `apps/extension/dist/firefox/manifest.json`  
     (do **not** load the Chromium folder in Firefox — that is Manifest V3 and will fail)

5. Register extension ids. Firefox uses a fixed id from the manifest (`rusticdl@local`). Chromium needs the generated id after first load:

```powershell
# Firefox (default id already matches the Firefox package)
.\scripts\register-native-host.ps1 `
  -HostBinaryPath "$PWD\target\debug\rusticdl-native-host.exe" `
  -FirefoxExtensionId "rusticdl@local"

# Chromium: re-run with the id from chrome://extensions
.\scripts\register-native-host.ps1 `
  -HostBinaryPath "$PWD\target\debug\rusticdl-native-host.exe" `
  -ChromiumExtensionId "<id-from-chrome-extensions-page>"
```

6. Start the desktop app (`cargo run`), open the extension popup, and confirm **Connected**.

### Environment overrides (native host)

| Variable | Purpose |
| --- | --- |
| `RUSTICDL_PIPE_PATH` | Named pipe path (default `\\.\pipe\rusticdl.v1`) |
| `RUSTICDL_DESKTOP_PATH` | Path to `rusticdl.exe` for auto-launch |

## Data location

Settings and queue state are stored under:

- Windows: `%APPDATA%\RusticDL\`
  - `settings.json`
  - `state.json`

Partial downloads are written next to the destination as `filename.ext.part`.

## Source map (desktop)

```text
src/
  main.rs               GPUI application bootstrap + IPC startup
  app.rs                Root view, queue UI, settings, dialogs
  settings.rs           Settings model
  extension_settings.rs Browser integration settings
  appearance.rs         Theme accent, transparency, noise helpers
  ipc/                  Named-pipe server for the native host
  persistence.rs        JSON load/save
  format.rs             Display helpers
  download/
    client.rs           Shared reqwest client
    filesystem.rs       Paths, filenames, finalize
    handoff.rs          Browser session header helpers
    http.rs             Single-stream transfer + resume
    engine.rs           Scheduler + worker control
    job.rs              Job model
```

## Continuous integration

GitHub Actions workflows live in `.github/workflows/`:

| Workflow | When it runs | What it does |
| --- | --- | --- |
| **CI** (`ci.yml`) | Push / PR to `master` | `cargo fmt` check, `clippy`, `test`, extension typecheck + build |
| **Release** (`release.yml`) | Tag `v*` (e.g. `v0.1.0`) | Build Windows release binaries, NSIS setup.exe, extension zips; publish a GitHub Release |

To cut a new release from a clean tree:

```bash
git tag v0.1.1
git push origin v0.1.1
```

The release workflow builds assets and attaches them to the GitHub Release automatically.

---

## Contributing / attribution

Issues and pull requests are welcome.

If you **fork, modify, redistribute, or ship** RusticDL (including commercial products), keep the MIT copyright notice and license text intact, and credit the original project:

- Project: **RusticDL**
- Author / maintainer: **[JustNak](https://github.com/JustNak)**
- Upstream: https://github.com/JustNak/RusticDL

That attribution requirement is part of the MIT license terms for this repository.

---

## License

RusticDL is released under the **[MIT License](LICENSE)**.

You may use, copy, modify, merge, publish, distribute, sublicense, and sell copies of the software — including for commercial purposes — provided you include the copyright notice and permission notice in all copies or substantial portions of the Software.
