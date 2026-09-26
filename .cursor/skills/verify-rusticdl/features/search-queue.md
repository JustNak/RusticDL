# Search queue

Search queue lets a user filter visible jobs by filename, URL, or path from the title-bar search field.

## Sub-features

- `search-focus` focuses the search field from the mouse or `/` shortcut.
- `search-match` narrows the list to jobs matching the query.
- `search-clear` clears the query and restores the filtered list.
- `search-empty` shows the empty state when nothing matches.

## How to get to it (user POV)

- Click the search field (placeholder `Search name, URL, or path…`) in the title bar.
- Press `/` while focus is outside a text field and Settings is not open.
- Clear via the clear control that appears when a query is present, or delete the text.

## Driving it with control-rusticdl

Preconditions:

- Doctor is green.
- A known job exists (for example filename `4096` from the add-download proof URL).
- Not currently in Settings (leave Settings first).

- **Focus search.** Run `.cursor/skills/verify-rusticdl/scripts/ui.sh focus-search` (or press `/`). The search field is focused.
- **Match.** Click the search field and type the literal query with a vision/`computerUse` agent (GPUI inputs often ignore xdotool). Example query: `4096`. Only matching rows remain.
- **Clear.** Select all and delete, or click the clear control. Run `.cursor/skills/verify-rusticdl/scripts/ui.sh clear-search`. The full filter list returns.
- **Proof.** Screenshot matching and cleared states into the evidence dir; note the query string in `meta.txt`.

## Gotchas

- `/` does nothing useful while Settings is open — it leaves Settings then focuses search; wait for the queue chrome to return.
- Search applies on top of the current sidebar filter; an Active-only view plus a completed-only query yields empty.
- Typing into the wrong field (URL dialog still open) will not search — escape/cancel dialogs first.
