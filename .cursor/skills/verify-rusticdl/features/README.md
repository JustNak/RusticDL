# RusticDL verification map

This directory is the maintained source for verifying user-facing behavior of the RusticDL **desktop** app. Read this index before driving, then use the matching feature file as the recipe.

Secondary surfaces (browser extension, native host) are noted in gotchas where they matter. Do not treat them as the default harness target unless a feature file says so.

## Baseline preconditions

- Launch with `.cursor/skills/verify-rusticdl/scripts/launch.sh` after `source …/scripts/env.sh`.
- Use a disposable run directory: `/tmp/rusticdl-verify-$RUN_ID` with isolated `XDG_RUNTIME_DIR` and `XDG_DATA_HOME`.
- Run `scripts/doctor.sh` and require `ok: true`, `appState: running`, and a `RusticDL` window.
- Never drive an instance that was not started by this verification run.
- On Windows, do not attempt a second side-by-side instance (hard single-instance mutex).

## Driving conventions

- Start every recipe from baseline unless its preconditions say otherwise.
- Prefer visible labels (`Add download`, `Settings`, `Start download`, sidebar filter names) and IPC request types over raw coordinates.
- Treat every command as literal. Keep quoted URLs and flags unchanged.
- Run desktop actions through `control-rusticdl` (`scripts/ui.sh`, `scripts/screenshot.sh`).
- Run bridge actions through `scripts/ipc.sh`.
- Restore or discard verification downloads after a mutation. Do not remove proof artifacts during cleanup.

## Proof and skip reporting

- Capture the user action and the resulting state, not only the final screen.
- UI proof includes screenshots with the `RusticDL` brand visible and an IPC `get_status` JSON dump.
- Mutation proof includes `queueSummary` and/or `state.json` and/or a file on disk.
- Record the feature ID and entry point used in `meta.txt` beside the artifacts.
- Report an unreachable path with the attempted command and the unmet precondition.
- Do not report a skipped entry point as verified through a different path.

## Feature entry contract

Each feature file starts with an H1 title and one paragraph describing the user-visible behavior. It then uses exactly four H2 sections in this order.

1. `Sub-features`
2. `How to get to it (user POV)`
3. `Driving it with control-rusticdl`
4. `Gotchas`

## Features

- [Add download](./add-download.md) — open the add dialog, enter an HTTP(S) URL, start the transfer, confirm it in the queue.
- [Queue filters](./queue-filters.md) — sidebar All / type / Active / Completed / Failed filters.
- [Search queue](./search-queue.md) — title-bar search by name, URL, or path.
- [Settings](./settings.md) — open Settings categories and Save settings.
- [Pause and resume](./pause-resume.md) — pause/resume a selected active job from the queue.
