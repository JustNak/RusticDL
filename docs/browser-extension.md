# Browser extension

The companion WebExtension hands browser downloads to the desktop app over native messaging.

```text
apps/extension/     Firefox + Chromium packages
apps/native-host/   stdio native messaging bridge
packages/protocol/  shared TypeScript protocol types
docs/protocol.md    wire format notes
scripts/            register / unregister native host (Windows + Linux)
```

## Browser handoff

If you want downloads captured from Firefox / Chromium:

**Windows — NSIS installer:** the native host is registered automatically. Continue from step 4.

**Windows — portable ZIP:**

1. Prefer **`RusticDL-full-windows-x64.zip`**.
2. Run the app once so it can create data folders.
3. Register the native messaging host (PowerShell, run from the extracted folder):

```powershell
.\scripts\register-native-host.ps1 `
  -HostBinaryPath "$PWD\rusticdl-native-host.exe" `
  -DesktopBinaryPath "$PWD\rusticdl.exe"
```

4. Load the matching extension package (see [Dev setup](#dev-setup-windows)).
5. Reload an already-loaded unpacked Chromium extension so it picks up the pinned id.

**Linux — recommended install:**

1. Extract **`RusticDL-linux-x64.tar.gz`** and run `./install-linux.sh` (no sudo).
2. That copies binaries to `~/.local/lib/RusticDL/`, symlinks `~/.local/bin/rusticdl`, and writes native-messaging JSON with an **absolute** host path.
3. Launch RusticDL once (it rewrites those manifests on startup if the host sibling exists).
4. Load the matching extension package.

**Linux — portable extract:**

```bash
./scripts/register-native-host.sh \
  --host-binary "$PWD/rusticdl-native-host" \
  --desktop-binary "$PWD/rusticdl"
```

Chrome/Firefox require the manifest `path` to be an absolute file. `~` and `$PATH` lookups do not work. Snap/Flatpak browsers often cannot see user-level native messaging hosts.

## Dev setup (Windows)

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

5. Register the native host (Firefox id `rusticdl@local` and the pinned Chromium id are the defaults):

```powershell
.\scripts\register-native-host.ps1 `
  -HostBinaryPath "$PWD\target\debug\rusticdl-native-host.exe"
```

6. Start the desktop app (`cargo run`), reload the unpacked Chromium extension, open the popup, and confirm **Connected**.

## Dev setup (Linux)

1. Build the desktop app and native host:

```bash
cargo build -p rusticdl
cargo build -p rusticdl-native-host
```

2. Register the native host (absolute path):

```bash
./scripts/register-native-host.sh \
  --host-binary "$PWD/target/debug/rusticdl-native-host"
```

3. Build and load the extension the same way as Windows (`apps/extension`, then the browser-specific `dist/` folder).

4. Start the desktop app (`cargo run`). On Linux it listens on `$XDG_RUNTIME_DIR/rusticdl.v1.sock` and rewrites native-messaging manifests when a sibling host binary exists.

## Environment overrides (native host)

| Variable | Purpose |
| --- | --- |
| `RUSTICDL_PIPE_PATH` | Windows named pipe, or Linux Unix-socket path (default `$XDG_RUNTIME_DIR/rusticdl.v1.sock`) |
| `RUSTICDL_DESKTOP_PATH` | Path to `rusticdl` / `rusticdl.exe` for auto-launch |
