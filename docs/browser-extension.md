# Browser extension

The companion WebExtension hands browser downloads to the desktop app over native messaging.

```text
apps/extension/     Firefox + Chromium packages
apps/native-host/   stdio native messaging bridge
packages/protocol/  shared TypeScript protocol types
docs/protocol.md    wire format notes
scripts/            register / unregister native host (Windows)
```

## Browser handoff

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

4. Load the matching extension package (see [Dev setup (Windows)](#dev-setup-windows)).
5. Reload an already-loaded unpacked Chromium extension so it picks up the pinned id.

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

## Environment overrides (native host)

| Variable | Purpose |
| --- | --- |
| `RUSTICDL_PIPE_PATH` | Named pipe path (default `\\.\pipe\rusticdl.v1`) |
| `RUSTICDL_DESKTOP_PATH` | Path to `rusticdl.exe` for auto-launch |
