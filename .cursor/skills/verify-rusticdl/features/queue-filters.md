# Queue filters

Queue filters let a user narrow the main list to All downloads (with optional file-type children), Active, Completed, or Failed jobs using the left sidebar.

## Sub-features

- `filter-all` shows the full library under All downloads.
- `filter-type` narrows to Video, Audio, Compressed, Images, Documents, Programs, or Other.
- `filter-active` shows queued/starting/downloading jobs.
- `filter-completed` shows finished jobs.
- `filter-failed` shows failed jobs.

## How to get to it (user POV)

- Choose `All downloads`, a type row, `Active`, `Completed`, or `Failed` in the left sidebar.
- Counts on each row reflect the current queue.

## Driving it with control-rusticdl

Preconditions:

- Doctor is green.
- At least one completed job exists (seed via Add download proof URL) so Completed is non-empty.
- Sidebar library is expanded (default) so type rows are visible.

- **Open Completed.** Choose `Completed`. Run `.cursor/skills/verify-rusticdl/scripts/ui.sh click-sidebar 'Completed'`. The main list shows only completed rows; the Completed badge is highlighted.
- **Return to All.** Choose `All downloads`. Run `.cursor/skills/verify-rusticdl/scripts/ui.sh click-sidebar 'All downloads'`. The list shows every job again.
- **Type filter.** Choose `Other` (or the type that holds the seed file). Run `.cursor/skills/verify-rusticdl/scripts/ui.sh click-sidebar 'Other'`. Jobs of other types disappear from the list.
- **Proof.** Screenshot before/after with `.cursor/skills/verify-rusticdl/scripts/screenshot.sh filter-completed.png`. Confirm sidebar counts match `get_status` queueSummary fields where applicable.

## Gotchas

- Empty filters show the empty-cube illustration, not an error — seed data before asserting rows.
- `Settings` is also in the sidebar; choosing it leaves the queue chrome (search / Add download hide). Leave Settings before asserting queue filters.
- Collapsing All downloads hides type rows; expand it again if type filters are missing.
