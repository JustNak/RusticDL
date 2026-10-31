# Extension capture

The Chromium extension, under default settings (ask mode), either sends one `prompt_download` to the native host for a real file download or leaves the item with the browser. This recipe drives the capture matrix through Playwright's bundled Chromium, a local fixture server, and a mock native-messaging host. It does not launch the desktop app.

## Sub-features

- `ext-positive` — `PROOF_RULES.capture` plus `PROOF_RULES.stop`: exactly one `prompt_download` for this case with accepted reply, and the browser item erased or `interrupted`+`USER_CANCELED` (match `url` or `finalUrl`). Bare `in_progress`/`complete`/other interrupt is FAIL named. `end-download-recheck` re-applies stop at end of run.
- `ext-negative` — `PROOF_RULES.keep`: zero capture lines in the mock-host log, plus this-attempt fixture serve and chrome.downloads/DOM proof. A no-op click is FAIL, except the C5/C6 blob/data keep arm, see kit-G6. A missing or wrong host `accepted` reply makes negatives INVALID.
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

Baseline at d0f7f75b (master merge base 0dd08e23): capture_rate=5/10 false_grab_rate=1/9 (CI run 41227323808, Extension e2e). PR2-PR5 are graded against this; EXT-METRICS is printed, not asserted, so graders compare it by hand.

## Proof table (`PROOF_RULES` in `apps/extension/e2e/classify.mjs`)

Capture, keep, stop, end-window recheck, end-download recheck, and the kit-G1/kit-G3/kit-G4/kit-G5 conjuncts are rows in this table (`evaluateProof`). Not every verdict goes through `PROOF_RULES`. Remaining direct sites: `classify.mjs` skip (`:854`), the `g5-final-url` original-URL `XPASS` path when capture and stop are both ok (`:929`, does not call `evaluateProof('g5-final-url')`), `isSentinelOk` (`:968`), `invalidateNegatives` (`:972`), `hostFailedHandoff` (`:1046`), the spec C2/N1 `expect`s and blocker filter (`extension-capture.spec.ts:520-529`), and the metrics `observed === 'capture'` filter (`extension-capture.spec.ts:486`). Missing, empty, partial, errored, unreadable, or ambiguous evidence on a table row is FAIL with the reason named, except kit-G6 on the blob/data keep arm.

- `capture` — evidence: mock-host log line for this case (`payload.url`, `suggestedFilename`, `source.entryPoint`, `receivedAt` in window). cause: exactly one `prompt_download` for this case URL+filename+`entryPoint=browser_download` with accepted reply (`ok`, `type=accepted`, echoed `requestId`). default: FAIL if the mock-host log is missing/empty/unreadable/ambiguous, or a capture line is for another URL/filename/entryPoint.
- `keep` — evidence: mock-host log (this case window; must be zero capture lines); fixture serve log entry for this case path+nonce+attempt (file keeps); chrome.downloads item matched by this-case path+nonce+attempt OR DOM `document.URL` / fetch marker for this path+nonce+attempt. cause: zero `prompt_download`/`enqueue_download` AND a completed this-case fixture serve (file keeps) AND this-attempt keep evidence (complete download, page render, or fetch marker). default: FAIL if mock-host/fixture/DOM/chrome.downloads is missing, the path belongs to another case, or there is no this-attempt keep evidence.
- `stop` — evidence: `chrome.downloads.search` items matched by `url` OR `finalUrl` in this click’s wanted set. cause: no matching item (erased by the extension) OR every match `interrupted` with `error===USER_CANCELED` (`chrome.downloads.cancel`). default: FAIL if the search was not read/errored/unreadable, or the item is `in_progress`, `complete`, or any other interrupt reason named.
- `end-window-recheck` — evidence: full mock-host log at end of run, filtered by this case `startedAt` and this case URL. cause: same capture predicate as the in-case drain: still empty → miss holds; exact `prompt_download` → XPASS; `enqueue_download` or wrong-shape line for this URL → FAIL. default: FAIL if the mock-host log is missing/unreadable, or a this-URL capture line is not exact `prompt_download`.
- `end-download-recheck` — evidence: end-of-run `chrome.downloads.search`, match `url` OR `finalUrl`. cause: stop rule still holds (erased or `USER_CANCELED`). default: FAIL if the search was not read/errored, or the stop cause no longer holds (`complete` / other interrupt / `in_progress`).
- `g1-miss` — evidence: mock-host log for this C1-family URL in the case window. cause: successfully read log with zero capture lines for this case (Chromium did not enqueue this URL). default: FAIL if the log is missing/unreadable, or a capture line for this URL is not exact `prompt_download`.
- `g4-miss` — evidence: mock-host log for this C4-family URL in the case window. cause: successfully read log with zero capture lines for this case (Chromium did not enqueue this URL). default: FAIL if the log is missing/unreadable, or a capture line for this URL is not exact `prompt_download`.
- `g3-false-grab` — evidence: mock-host log line for this-attempt POST URL. cause: exactly one `prompt_download` with `url=`this-attempt POST URL, `suggestedFilename=posted.zip`, `entryPoint=browser_download`. default: FAIL if the log is missing/unreadable, or the line is another case’s URL, a superstring, the wrong filename, or a non-prompt type.
- `g5-final-url` — evidence: mock-host log line for origin+`/C10/final`+this-attempt query; `chrome.downloads.search` matched by `url` OR `finalUrl`. cause: exactly one `prompt_download` with `payload.url===`final URL, accepted reply, AND stop (erased or `USER_CANCELED`). default: FAIL if the log/search is missing, the URL is evil/super/garbage, or the download is `in_progress`/`complete`/other interrupt named.

## Known gaps

Kit gap ids (`kit-G1`..`kit-G6`) are separate from EXT-SCOPE gap ids. EXT-SCOPE G2 (Firefox inline PDF) is out of scope for PR1 and lands in PR3.

Product xfails in `scripts/ext-cases.json` (Chromium, not this harness):

- kit-G3 — Chromium false-captures the POST attachment and would replay it as GET (C9).
- kit-G5 — Chromium handed off the final same-origin redirect URL (`/C10/final`), not the original `/C10` URL.

Closed by the Chromium capture coordinator (header-hint / URL-basename filename, final decision at `onDeterminingFilename`):

- kit-G1 — was: Chromium did not capture `<a download>` same-origin zip with no Content-Disposition attachment (also C3 inline zip, C7 `location.href`, C8 programmatic `a.click()`).
- kit-G4 — was: Chromium misses octet-stream with no Content-Disposition and no filename in the URL (C4).

Harness (PR1 lands with this documented):

- kit-G6 (R8-1, Medium, V4) — the blob/data keep arm (C5/C6) passes on the Playwright download flag or any no-error blob/data item. It does not require a complete this-attempt item, so an extension that erases blob/data downloads still passes keep. See `blobDataKept` (`classify.mjs:274`) and the keep row’s `blobDataKept(...) || playwrightDownload` arm (`keepClickProved` / keep row, `classify.mjs:359`). File keeps (N1) still require `state === 'complete'` plus path+nonce+attempt.

Unconfirmed suspicions (not findings):

- (a) the stop match compares href strings, so a reordered query on a `complete` item counts as erased.
- (b) a late exact this-URL `prompt_download` after keep’s window is not rechecked.

## Gotchas

- Branded Chrome 137+ ignores `--load-extension`. This harness uses Playwright's bundled Chromium with `--disable-extensions-except` and `--load-extension`.
- Bodies are 64 KiB unless the case says otherwise (N6 is 3 KiB; L1 is 200 MiB chunked with no Content-Length). Smaller bodies hit the 8 KiB capture gate.
- L2/L3 are skipped unless a real user action can drive them (download-shelf cancel and desktop-prompt dismiss cannot).
- An `xfail` that starts passing is XPASS and fails the job; the fix PR must flip that row in `ext-cases.json`.
- `--live` only prints the live-site list. It is never CI-gated.
- `EXT_ATTACK` is a test-only hook in `apps/extension/e2e/ext-attack.mjs` (loaded by the mock host / fixture). Never product code. Attacks can only fail the job closed; they cannot select a weaker classifier. `EXT_CLASSIFIER` is ignored by the prove script and spec (legacy exists only for `selftest-classifier.mjs`).
- `selftest-classifier.mjs` plants F1–R7 plus R8: every `PROOF_RULES` row has at least one honest control and one mutant that must FAIL; the selftest prints a row-to-tests coverage list and fails if any row lacks either.
