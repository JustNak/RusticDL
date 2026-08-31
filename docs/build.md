# Build from source

Everything below is for people who want to compile RusticDL themselves, hack on it, or package nightlies.

## Requirements

| Tool | Notes |
| --- | --- |
| **Rust stable** | Install via [rustup](https://rustup.rs/) (rustc 1.89+ on Linux) |
| **Windows C++ build tools** | Visual Studio Build Tools with the “Desktop development with C++” workload (needed by GPUI / native graphics) |
| **Linux link libs** | `libstdc++`, `libxcb`, `libxkbcommon`, `libxkbcommon-x11` |
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
├── scripts/                native-host register / unregister (Windows + Linux)
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

- Debug: `target/debug/rusticdl.exe` (Windows) or `target/debug/rusticdl` (Linux)
- Release: `target/release/rusticdl.exe` (Windows) or `target/release/rusticdl` (Linux)

## Build the native messaging host

```bash
cargo build -p rusticdl-native-host
# or
cargo build --release -p rusticdl-native-host
```

Output: `target/*/rusticdl-native-host.exe` (Windows) or `target/*/rusticdl-native-host` (Linux)

## Tests

```bash
cargo test
```

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
    http.rs             Single-stream transfer + Range resume + reconnect
    multi.rs            Multi-segment orchestrator + map resume
    transfer.rs         Planner (multi vs single + fallback)
    segment.rs          Segment map + partition
    engine/             Scheduler + worker control
    job.rs              Job model
```

## Quick start (from source)

```bash
cargo run
```

Release build:

```bash
cargo build --release
```

## Build the Windows installer

Requires Windows + Rust. Uses [cargo-packager](https://crates.io/crates/cargo-packager) to produce an NSIS setup executable:

```powershell
# Install packager once (pinned version used by CI)
cargo install cargo-packager --locked --version 0.11.8

# Build binaries + NSIS installer
powershell -ExecutionPolicy Bypass -File scripts/package-windows.ps1
```

Output: `dist-release/RusticDL-windows-x64-setup.exe` (plus the packager’s `rusticdl_*_x64-setup.exe` name).

Packager config lives in `[package.metadata.packager]` in `Cargo.toml`, with a custom NSIS template at `installer/nsis/installer.nsi` that registers/unregisters the native messaging host on install/uninstall.

## Build the Linux tarball

```bash
cargo build --release -p rusticdl -p rusticdl-native-host -p rusticdl-updater
bash scripts/package-linux.sh
```

Output: `dist-release/RusticDL-linux-x64.tar.gz` and `dist-release/SHA256SUMS`.

See also [protocol.md](protocol.md) for the extension ↔ app wire format.
