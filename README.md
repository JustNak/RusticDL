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
  <img src="https://img.shields.io/badge/Platform-Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black" alt="Linux">
  <img src="https://img.shields.io/badge/Rust-stable-orange?style=for-the-badge&logo=rust&logoColor=white" alt="Rust">
</p>

<p align="center">
  <a href="#download"><strong>Download</strong></a> ·
  <a href="#features"><strong>Features</strong></a> ·
  <a href="#quick-start"><strong>Quick start</strong></a> ·
  <a href="#guides"><strong>Guides</strong></a> ·
  <a href="#license"><strong>License</strong></a>
</p>

---

## Download

Get the latest Windows build from GitHub Releases:

**[→ Download RusticDL for Windows](https://github.com/JustNak/RusticDL/releases/latest)**

Linux builds ship as **`RusticDL-linux-x64.tar.gz`** on [Stable](https://github.com/JustNak/RusticDL/releases/latest) and [Nightly](https://github.com/JustNak/RusticDL/releases) (app + native host + updater).

Nightly (may be unstable) is an on-demand GitHub pre-release for testing new work before a Stable cut. In the app, set **Settings → General → Update channel** to **Nightly**, or grab a build from the [releases list](https://github.com/JustNak/RusticDL/releases).

| Asset | What it contains |
| --- | --- |
| **`RusticDL-windows-x64-setup.exe`** | **Recommended** — NSIS installer (app + native host + browser host registration) |
| **`RusticDL-windows-x64.zip`** | Portable desktop app (`rusticdl.exe`) |
| **`RusticDL-full-windows-x64.zip`** | Portable app + native host + register scripts + browser extension packages |
| **`RusticDL-linux-x64.tar.gz`** | Linux app + native host + updater + `install-linux.sh` |
| **`extension-chromium.zip`** | Chromium / Edge / Brave unpacked extension |
| **`extension-firefox.zip`** | Firefox temporary-add-on package |

### Install (recommended)

1. Download **`RusticDL-windows-x64-setup.exe`**.
2. Run the installer (per-user install; **no administrator rights** required).
3. Launch **RusticDL** from the Start Menu.

The installer places files under `%LOCALAPPDATA%\RusticDL\`, creates a Start Menu shortcut, and registers the browser **native messaging host** for Chrome, Edge, and Firefox.

Settings and queue state live under `%APPDATA%\RusticDL\`. Uninstall via Apps & Features (optionally remove app data).

> **Note:** The installer does **not** auto-install browser extensions. Load the extension separately (see [Browser extension](docs/browser-extension.md)). GitHub Windows installers are unsigned; Windows may show SmartScreen (“Windows protected your PC”) and/or UAC. Local source builds are also unsigned.

### Install (Linux)

1. Download **`RusticDL-linux-x64.tar.gz`** from [Releases](https://github.com/JustNak/RusticDL/releases/latest).
2. Extract and install (per-user, **no sudo**):

```bash
tar -xzf RusticDL-linux-x64.tar.gz
./install-linux.sh
```

That copies binaries to `~/.local/lib/RusticDL/`, puts `rusticdl` on `PATH` (`~/.local/bin/rusticdl`), writes a desktop entry, and registers the browser native messaging host with an absolute path.

Portable (no PATH / desktop entry): extract and run `./rusticdl`. Register the host with `./scripts/register-native-host.sh` or use **Settings → Browser → Register browser host**.

In-app updates replace files in the install prefix (or the portable extract folder). Snap/Flatpak browsers may not see user-level native messaging hosts.

Load the browser extension separately (see [Browser extension](docs/browser-extension.md)).

### Portable install (ZIP)

1. Download **`RusticDL-windows-x64.zip`** (or the full package if you want browser integration).
2. Extract anywhere you like (for example `C:\Tools\RusticDL\`).
3. Run `rusticdl.exe`.

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
- **Engine:** multi-segment Range downloads with map-authoritative resume (falls back to single-stream)
- **HTTP:** browser-like client with HTTP/2 and HTTP/3 (QUIC) fallback

### Features

- Add one or many URLs (batch paste, one per line) over HTTP/HTTPS
- Queue filters: All / Active / Completed / Failed
- Search by filename, URL, or path
- Live progress: percent, size, speed, ETA
- Open a floating progress window from the queue overflow menu or detail panel while a job is still downloading
- Status bar with totals, aggregate speed, and speed limit
- Pause / resume, pause-all / resume-all
- Retry, restart, cancel, remove from the queue, or delete the file from disk (with confirmation)
- Clear completed / clear failed
- Detail panel with full job actions (open, reveal, copy path)
- Multi-connection segmented downloads for large HTTP files (parallel Range requests)
- Map-authoritative resume: paused or failed multi jobs reuse the same segment map
- Mid-transfer reconnect with short backoff
- Concurrent downloads, auto-retry with exponential backoff, optional global speed limit
- Fsync on pause (flush `.part` to disk)
- Nested TLS/network error details (helps diagnose flaky networks)
- Windows completion notifications: tray balloons, OS notify mode (when hidden / always / off), and in-app terminal toasts when the window is visible
- Unified active-URL duplicate policy: the same URL is not queued again while an active job already exists (manual Add and browser handoff share one rule)
- Ask-mode same-name conflict popup: Rename fills `file (1).ext`, Start download stays disabled with a Duplicate Name tooltip while the typed name is taken, overwrite (replace at finalize), or cancel
- Desktop Browser capture settings panel to configure extension capture preferences from the app
- Settings for download dir, concurrency, retries, speed limit, multi-segment caps, appearance, and data folder
- Appearance: light / dark / system, accents, transparency, blur, film grain, density, corners, vignette, progress styles, reduce motion
- Persisted queue, settings, and window geometry
- Browser extension handoff (Firefox + Chromium)

### Not included (by design)

- Torrents / magnets
- Bulk archive finalize workflows

---

## Quick start

### From the installer

Install via **`RusticDL-windows-x64-setup.exe`**, then open **RusticDL** from the Start Menu.

### Linux tarball

Download **`RusticDL-linux-x64.tar.gz`**, extract, and run `./install-linux.sh` (see [Install (Linux)](#install-linux) above).

### From a portable ZIP

```powershell
# After extracting the zip
.\rusticdl.exe
```

---

## Data location

Settings and queue state are stored under:

- Windows: `%APPDATA%\RusticDL\` — `settings.json`, `state.json`
- Linux: `~/.local/share/RusticDL/` (XDG) — `settings.json`, `state.json`

Partial downloads are written next to the destination as `filename.ext.part`. Multi-segment jobs also persist a segment map in `state.json` (`transfer_format_version = 1`).

---

## Guides

- [Build from source](docs/build.md)
- [Browser extension](docs/browser-extension.md)
- [Download engine](docs/download-engine.md)
- [Releases & contributing](docs/releases.md)
- [Protocol (browser bridge)](docs/protocol.md)

---

## License

RusticDL is released under the **[MIT License](LICENSE)**.

You may use, copy, modify, merge, publish, distribute, sublicense, and sell copies of the software — including for commercial purposes — provided you include the copyright notice and permission notice in all copies or substantial portions of the Software.
