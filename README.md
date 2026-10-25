<p align="center">
  <img src="assets/brand/logo.png" alt="RusticDL logo" width="128" height="128">
</p>

<h1 align="center">RusticDL</h1>

<p align="center">
  <strong>A local-first HTTP(S) download manager written in Rust.</strong><br>
  Queue downloads, resume interrupted transfers, and optionally hand browser downloads to the desktop app.
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
  <a href="#data-location"><strong>Data location</strong></a> ·
  <a href="#guides"><strong>Guides</strong></a> ·
  <a href="#license"><strong>License</strong></a>
</p>

## Download

Download the latest stable build from [GitHub Releases](https://github.com/JustNak/RusticDL/releases/latest).

| Asset | Contents |
| --- | --- |
| **`RusticDL-windows-x64-setup.exe`** | **Recommended** — NSIS installer (app, native host, browser host registration) |
| **`RusticDL-windows-x64.zip`** | Portable app (`rusticdl.exe`) |
| **`RusticDL-full-windows-x64.zip`** | Portable app, native host, register scripts, and browser extension packages |
| **`RusticDL-linux-x64.tar.gz`** | Linux app, native host, updater, and `install-linux.sh` |
| **`extension-chromium.zip`** | Chromium, Edge, and Brave unpacked extension |
| **`extension-firefox.zip`** | Firefox temporary-add-on package |

### Windows

Run **`RusticDL-windows-x64-setup.exe`**. The installer is per-user and needs no administrator rights; it installs under `%LOCALAPPDATA%\RusticDL\`, adds a Start Menu shortcut, and registers the native messaging host for Chrome, Edge, and Firefox. Uninstall from Apps & Features, with an option to remove app data.

For a portable build, extract `RusticDL-windows-x64.zip` or `RusticDL-full-windows-x64.zip` and run `rusticdl.exe`.

The installer does not add browser extensions; load one from the table separately (see [Browser extension](docs/browser-extension.md)). Release installers are unsigned, so Windows may show SmartScreen or a UAC prompt.

### Linux

Download **`RusticDL-linux-x64.tar.gz`**, then:

```bash
tar -xzf RusticDL-linux-x64.tar.gz
./install-linux.sh
```

This installs per-user without sudo: binaries go to `~/.local/lib/RusticDL/`, `rusticdl` is placed in `~/.local/bin`, and the script adds a desktop entry and registers the browser native host.

To run without installing, extract the archive and run `./rusticdl`. Register the host with `./scripts/register-native-host.sh` or **Settings → Browser → Register browser host**. In-app updates replace files in the install prefix or portable folder. Snap and Flatpak browsers may not see user-level native messaging hosts.

Nightly builds are pre-releases for testing. Switch to them under **Settings → General → Update channel**, or download from the [releases list](https://github.com/JustNak/RusticDL/releases).

## Features

- Add one or many HTTP/HTTPS URLs, with batch paste (one per line).
- Filter the queue by All, Active, Completed, or Failed; search by filename, URL, or path.
- Track live percent, size, speed, and ETA, with queue totals and aggregate speed in the status bar.
- Open a floating progress window for active downloads.
- Pause, resume, pause all, resume all, retry, restart, cancel, remove, or delete files (with confirmation); clear completed or failed jobs.
- Inspect and act on any job from the detail panel: open, reveal, copy path.
- Download in parallel Range segments with map-authoritative resume; interrupted jobs reconnect with backoff and reuse their segment map.
- Set the download directory, concurrency, auto-retry, an optional global speed limit, and multi-segment caps.
- Resolve same-name conflicts with rename, overwrite, or cancel; active duplicate URLs are not queued twice.
- Get Windows notifications through the tray, OS notifications, or in-app toasts.
- Customize appearance: light, dark, or system theme; accents, transparency, blur, film grain, density, corners, vignette, progress styles, and reduced motion.
- Keep queue, settings, and window geometry between runs.
- Hand downloads from Firefox or Chromium to RusticDL through the browser extension and native host; configure capture in the app.

**Built with:** [GPUI](https://gpui.rs/) and [gpui-component](https://github.com/longbridge/gpui-component) for the UI, a multi-segment Range engine with single-stream fallback, and an HTTP client with HTTP/2 and HTTP/3 (QUIC) fallback.

### Not included

- Torrents or magnets.
- Bulk archive finalize workflows.

## Data location

- Windows: `%APPDATA%\RusticDL\` (`settings.json`, `state.json`)
- Linux: `~/.local/share/RusticDL/` (`settings.json`, `state.json`)

Partial downloads are written beside the destination as `filename.ext.part`. Multi-segment jobs also store their segment map in `state.json`.

## Guides

- [Build from source](docs/build.md)
- [Browser extension](docs/browser-extension.md)
- [Download engine](docs/download-engine.md)
- [Releases and contributing](docs/releases.md)
- [Protocol (browser bridge)](docs/protocol.md)

## License

RusticDL is released under the [MIT License](LICENSE).
