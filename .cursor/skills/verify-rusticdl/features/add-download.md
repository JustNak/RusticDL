# Add download

Add download lets a user queue one or more HTTP(S) URLs from the desktop UI, optionally override the filename or folder, start the transfer, and confirm the job appears in the queue (and in IPC `queueSummary`).

## Sub-features

- `add-open` opens the Add download dialog from the title-bar button or Ctrl+N.
- `add-url` accepts a single HTTPS URL in the URL field.
- `add-start` starts the transfer with Start download.
- `add-multi` toggles Multiple URLs for batch paste (one URL per line).
- `add-ipc` enqueues via the native-host IPC `enqueue_download` path (browser handoff equivalent).

## How to get to it (user POV)

- Choose the `Add download` button in the title bar.
- Press Ctrl+N while focus is outside a text field.
- Browser extension handoff (secondary surface) sends `enqueue_download` / `prompt_download` through the native host into the same queue.

## Driving it with control-rusticdl

Preconditions:

- Doctor is green for this `RUN_ID`.
- Queue is empty, or you have recorded the pre-existing `queueSummary.total`.
- Outbound HTTPS to the test URL is allowed (default proof URL: `https://httpbin.org/bytes/4096`).

- **Open dialog.** Choose `Add download`. Run `.cursor/skills/verify-rusticdl/scripts/ui.sh click-add-download`. A dialog titled `Add download` appears with placeholder `https://example.com/file.zip`.
- **Enter URL.** Click the URL field (placeholder `https://example.com/file.zip`) and type the proof URL with a vision/`computerUse` agent — GPUI inputs often ignore xdotool keystreams. Literal URL: `https://httpbin.org/bytes/4096`. The field shows that URL (screenshot before start).
- **Start download.** Choose `Start download` (vision click, or `.cursor/skills/verify-rusticdl/scripts/ui.sh click-start-download` after the URL is visible). The dialog closes and a queue row appears (filename derived from the URL, often `4096`).
- **Confirm via IPC.** Run `.cursor/skills/verify-rusticdl/scripts/ipc.sh get_status > "$VERIFY_EVIDENCE_DIR/status-after.json"`. `queueSummary.total` is greater than before; `active` or `completed` reflects the job.
- **Proof screenshots.** `dialog-open.png` and `status-before.json` are the prepare pair. After typing the URL, save `dialog-filled.png`; after the queue updates, save `queue-after.png`. Copy `dialog-filled.png`, `queue-after.png`, `status-after.json`, and `meta.txt` into `.cursor/skills/verify-rusticdl/artifacts/add-download/$RUN_ID/`.
- **IPC entry (optional second path).** Run `.cursor/skills/verify-rusticdl/scripts/ipc.sh enqueue_download 'https://httpbin.org/bytes/512' 'ipc-proof.bin'`. A second job appears; do not count this as proof of the button path.

## Gotchas

- Clicking the dimmed overlay outside the dialog closes it (`overlay_closable`). Aim for the URL field and Start download button, not the backdrop.
- Ctrl+N is ignored while a text input is focused or another dialog is open.
- Organize-by-type may place the file under `$VERIFY_RUN_DIR/downloads/Other/` (or another type folder), not the download-directory root. Proof bytes must not land in the user's real Downloads folder.
- A toast alone is insufficient proof — capture `queueSummary` or the queue row screenshot.
- Do not use `pkill -f rusticdl` for cleanup; match the launch pidfile only.
