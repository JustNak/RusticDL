# Pause and resume

Pause and resume let a user stop an in-progress transfer and continue it later when the server supports byte ranges, from row actions, batch actions, or the Space shortcut on a selection.

## Sub-features

- `pause-one` pauses a selected active job.
- `resume-one` resumes a selected paused job.
- `pause-all` / `resume-all` via the queue overflow menu (More actions).
- `space-toggle` toggles pause/resume for the current selection when no dialog is open.

## How to get to it (user POV)

- Select an Active row, then use the row overflow menu or batch Pause / Resume bar.
- Press Space with one or more selected pausable jobs.
- Choose Pause all / Resume all from the title-bar More actions menu.

## Driving it with control-rusticdl

Preconditions:

- Doctor is green.
- An actively downloading job exists. Prefer a larger URL so the transfer stays Active long enough to pause (for example a multi-megabyte HTTPS file). Tiny `httpbin.org/bytes/4096` jobs may complete before you can pause.
- Focus is on the queue (not Settings, not a dialog).

- **Select job.** Click the active row. The batch action bar or selection highlight appears.
- **Pause.** Choose Pause (batch bar or Space). Run `.cursor/skills/verify-rusticdl/scripts/ui.sh pause-selection`. The row shows a paused state; `get_status` active count drops.
- **Resume.** Choose Resume (or Space again). Run `.cursor/skills/verify-rusticdl/scripts/ui.sh resume-selection`. The job returns to downloading/queued.
- **Proof.** Screenshot paused and resumed states; save `status-paused.json` and `status-resumed.json` from `scripts/ipc.sh get_status`.

## Gotchas

- Completed jobs cannot pause — seed a long-running download or assert at the paused state before completion.
- Space is ignored while a text field or dialog has focus.
- Map-authoritative resume depends on server Range support; a server without ranges may restart instead of continuing the `.part` file.
