# Extension capture

The Chromium extension, under default settings (ask mode), either sends one `prompt_download` to the native host for a real file download or leaves the item with the browser. This recipe drives the capture matrix through Playwright's bundled Chromium, a local fixture server, and a mock native-messaging host. It does not launch the desktop app.

## Sub-features

- `ext-positive` — a user click on `[data-case=Cn]` for a capture case produces exactly one `prompt_download` with the fixture URL, suggested filename, and `source.entryPoint == "browser_download"`.
- `ext-negative` — a keep case produces zero `prompt_download` / `enqueue_download` lines within 5 s, and `chrome.downloads.search({})` from the service worker shows the browser kept or rendered the item.
- `ext-sentinel` — every negative batch and the whole run ends with C2; if that sentinel does not capture, those negatives are INVALID.
- `ext-metrics` — the run writes `evidence/extension/metrics.json` and prints one `EXT-METRICS` line (capture rate, false-grab rate, click-to-host latency, payload fidelity).

## How to get to it (user POV)

- Load the unpacked Chromium extension (`apps/extension/dist/chromium`) in a profile that can see `com.rusticdl.native_host`.
- Open a fixture page such as `http://127.0.0.1:18765/page/C2?nonce=…` and choose the control labeled with that case id.
- Ask mode (the default) sends `prompt_download`. The mock host accepts; the real desktop would show the confirm dialog.
- Keep cases stay in the browser (HTML, inline PDF, image, fetch, tiny stub, blob/data URLs).

## Driving it with control-rusticdl

Preconditions:

- Linux, Node 20+, `apps/extension` built (`npm ci` and `npm run build` so `dist/chromium/manifest.json` exists).
- Playwright's bundled Chromium (`npx playwright install chromium` locally; CI uses `--with-deps`).
- Do not start `launch.sh`. The mock host is `scripts/mock-native-host.sh`, not `rusticdl-native-host`.
- Isolated Chromium `HOME` plus a copy of `XAUTHORITY` so X11 still works; native-host manifest at `<user-data-dir>/NativeMessagingHosts/com.rusticdl.native_host.json` (proven in Step 0).

Literal commands, from the repo root:

```bash
export RUN_ID="${RUN_ID:-$(date +%Y%m%d%H%M%S)}"
.cursor/skills/verify-rusticdl/scripts/prove-extension-capture.sh
.cursor/skills/verify-rusticdl/scripts/prove-extension-capture.sh --case C2
.cursor/skills/verify-rusticdl/scripts/prove-extension-capture.sh --repeat 5
.cursor/skills/verify-rusticdl/scripts/prove-extension-capture.sh --live
```

Headless CI wraps the same script with `xvfb-run`. Each case is a real click on `[data-case=Cn]`. Evidence lands in `/tmp/rusticdl-verify-$RUN_ID/evidence/extension/<case>/`. Cleanup kills only the fixture pidfile process and keeps that evidence.

## Gotchas

- Branded Chrome 137+ ignores `--load-extension`. This harness uses Playwright's bundled Chromium with `--disable-extensions-except` and `--load-extension`.
- Matching a `ping` line, a URL substring, or another case's host-log line is a FAIL. Filter by `receivedAt` after the case start, then require exact `payload.url`.
- Bodies are 64 KiB unless the case says otherwise (N6 is 3 KiB; L1 is 200 MiB chunked with no Content-Length). Smaller bodies hit the 8 KiB capture gate.
- L2/L3 are skipped unless a real user action can drive them (download-shelf cancel and desktop-prompt dismiss cannot).
- An `xfail` that starts passing is XPASS and fails the job; the fix PR must flip that row in `ext-cases.json`.
- `--live` only prints the live-site list. It is never CI-gated.
