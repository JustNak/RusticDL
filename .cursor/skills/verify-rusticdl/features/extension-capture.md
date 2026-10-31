# Extension capture

The Chromium extension, under default settings (ask mode), either sends one `prompt_download` to the native host for a real file download or leaves the item with the browser. This recipe drives the capture matrix through Playwright's bundled Chromium, a local fixture server, and a mock native-messaging host. It does not launch the desktop app.

## Sub-features

- `ext-positive` — a user click on `[data-case=Cn]` for a capture case produces exactly one `prompt_download` with the fixture URL, suggested filename, and `source.entryPoint == "browser_download"`, the host replies `accepted` (`ok`, `type`, echoed `requestId`), and `chrome.downloads.search({})` shows that URL erased or in a terminal non-complete state (`interrupted`) at the end of a bounded settle. An item still `in_progress`, `state: complete`, or a search `{error}` is FAIL. Capture rows are re-checked at end of run; a later complete flips the row to FAIL.
- `ext-negative` — a keep case produces zero `prompt_download` / `enqueue_download` lines within 5 s. The click must have requested the item (a fixture-server hit for that attempt's nonce) and `chrome.downloads.search({})` or the rendered page must show the browser kept it. A no-op click is FAIL. A missing or wrong host `accepted` reply makes negatives INVALID.
- `ext-sentinel` — every negative batch and the whole run ends with C2; if that sentinel does not capture, every preceding negative in that round is INVALID. Under `--repeat`, a failed sentinel invalidates only that round's negatives.
- `ext-metrics` — the run writes `evidence/extension/metrics.json` and prints one `EXT-METRICS` line (capture rate, false-grab rate, click-to-host latency, payload fidelity). `fidelity.postBody` is computed from the payload; cookie fidelity requires `rusticdl_e2e=<nonce>` after parsing the Cookie header (`===`).

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
- G3 (C9) xfail counts only the exact false-grab line: `url` equals this attempt's POST URL, `suggestedFilename` is `posted.zip`, `entryPoint` is `browser_download`, `receivedAt` in the case window. Any other line is FAIL.
- G5 (C10) xfail counts only when `payload.url ===` origin + `/C10/final` + this attempt's query, the host reply is `accepted`, and the browser download for that URL is erased or `interrupted` (not `in_progress`, not `complete`, not a search error). Evil hosts, superstrings, garbage replies, an in-progress or completed download, and a later complete at end-of-run recheck are FAIL.
- G1/G4 miss-xfails drain the host log for up to `maxWaitMs` (20 s) while it stays empty, then a 3 s quiet window after the first line, and re-check the full log at the end of the run with the same capture predicate as the in-case drain (`prompt_download` and `enqueue_download`, the two types `handoffDownload` can send). A late exact `prompt_download` inside that cap is XPASS. A late wrong-shape or `enqueue_download` capture for this case URL (for example `suggestedFilename` null, `entryPoint` not `browser_download`, or auto-mode enqueue) is FAIL.
- Keep proof’s source of truth is the fixture serve log for this case’s target path + this nonce/attempt (status, bytes, body fully sent). Download-type keeps need a chrome/playwright item whose URL path is exactly that target path, created after `triggerAt`, `state: complete`. `fetch-attachment` (N5) needs a 2xx completed serve of `/N5` whose status/bytes equal the launcher page marker; a painted 200/65536 against a 500/9 or hung body is FAIL. Page-render keeps need `document.URL.pathname ===` the case target path (not `data-page=<id>-target` on `/decoy`). An N6 download does not prove N5. A G1/G4 end-window capture (`prompt_download` or `enqueue_download`) for this case URL that is not an exact `prompt_download` match is FAIL; an exact `prompt_download` match is XPASS.
- Bodies are 64 KiB unless the case says otherwise (N6 is 3 KiB; L1 is 200 MiB chunked with no Content-Length). Smaller bodies hit the 8 KiB capture gate.
- L2/L3 are skipped unless a real user action can drive them (download-shelf cancel and desktop-prompt dismiss cannot).
- An `xfail` that starts passing is XPASS and fails the job; the fix PR must flip that row in `ext-cases.json`.
- `--live` only prints the live-site list. It is never CI-gated.
- `EXT_ATTACK` is a test-only hook in `apps/extension/e2e/ext-attack.mjs` (loaded by the mock host / fixture). Never product code. Attacks can only fail the job closed; they cannot select a weaker classifier. `EXT_CLASSIFIER` is ignored by the prove script and spec (legacy exists only for `selftest-classifier.mjs`).
