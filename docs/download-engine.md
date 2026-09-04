# Download engine (0.3.0)

Large files can split across parallel HTTP Range connections. Smaller files, servers without byte ranges, and legacy single-stream partials stay on one connection.

| Behavior | What it does |
| --- | --- |
| **Multi-segment** | Parallel `Range` GETs into one `.part` when the file is at or above **Min size** and preflight proves Range with 206 (including a non-zero offset). HEAD `Accept-Ranges` alone is not enough |
| **Range resume** | Single-stream jobs append from the existing `.part` length |
| **Map resume** | Multi jobs persist a segment map in `state.json` and resume each segment from `start + written`. After a map exists, file length is **not** treated as downloaded bytes (preallocate would look “complete”) |
| **Global speed limit** | One process-wide budget shared by every body reader (single-stream and segments). `0` = unlimited |
| **Fsync on pause** | Flush `.part` to disk when pausing (safer on power loss) |
| **Reconnect** | Transient network/TLS drops retry the same pinned URL with short backoff (per segment for multi; up to 5 times, 200 ms–2 s) |

## Settings → Download Engine

| Setting | Default | Clamp |
| --- | --- | --- |
| Max segments | 8 | 1–16 per job |
| Min size (MiB) | 5 | 1–1024; below this, single connection |
| Total connections | 32 | 1–256 process-wide body connections |
| Per-host connections | 8 | 1–64; cannot exceed total |
| Speed limit (KiB/s) | 0 | Shared; 0 = unlimited |

If **Max concurrent × Max segments** exceeds **Total connections**, extra segments wait on the budget — that is expected, not an error. Jobs that already have a segment map keep using that map until they finish or you Restart. The engine picks multi-connection when the file is large enough and the server supports ranges.

The engine persists `segment_map.written` only after fsync on pause, cancel, error, complete, or start identity. Live progress ticks do not write that field. Quit sends Drain, which pauses in-flight jobs and waits up to 2 seconds for them to leave `active` before flushing `state.json`. Max concurrent occupancy is reserved when a job becomes Starting.

> **Do not downgrade mid multi download.** A 0.3.0 multi job writes `transfer_format_version = 1` and a segment map. Older builds ignore those fields and can mis-resume a preallocated `.part` (holes treated as real bytes). Finish or Restart multi jobs on 0.3.0 before installing an older release.
