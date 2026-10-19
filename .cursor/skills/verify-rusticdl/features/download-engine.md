# Download engine

The download engine fetches one HTTP URL, writes the bytes under the job's download directory, and finishes the job as Completed, Failed, or another terminal state. This recipe does not open the desktop window. It calls the same engine entry `src/main.rs` uses (`spawn_engine` with `FileJobStore`, then `EngineCommand::Add`) against a local server so Content-Encoding, Content-Length, Range/206, and HEAD versus GET can be set per request.

## Sub-features

- `identity-200` — HEAD and GET are 200 with the same Content-Length and no Content-Encoding. The body is the identity payload.
- `gzip-both` — HEAD and GET both send `Content-Encoding: gzip` and the same Content-Length (the gzip size). The GET body is those gzip bytes.
- `length-mismatch` — HEAD advertises Content-Length 8192. GET is 200 with Content-Length equal to the 4096-byte identity body.
- `resume-206` — HEAD and GET advertise the identity length and `Accept-Ranges: bytes`. The first GET (no Range) sends the first 1024 bytes and closes. A later `Range` request is answered 206 with `Content-Range`.

## How to get to it (user POV)

- Add download, a browser handoff, or any other enqueue path puts a job on this engine. The transfer runs inside the desktop process; the window is only the control surface.
- The bytes that land on disk are the file named in the queue row. A failed job may leave a `.part` file or no file.
- Pause and resume of an in-progress row is the desktop control for the same engine (`features/pause-resume.md`). This recipe's `resume-206` case is the engine's own follow-up GET after a short body, not a click on Pause.

## Driving it with control-rusticdl

Preconditions:

- Linux, Rust 1.89+ (the toolchain `ci.yml` installs), `python3`, and `sha256sum`.
- Do not start `launch.sh`. This recipe is headless and does not need `DISPLAY` or Vulkan.
- Expected content is always the identity payload: 4096 bytes where byte `i` is `(i * 17 + 3) % 251`. The script prints whether the file on disk matches that sha256, and whether it matches the GET body (the wire bytes). A mismatch is an observation, not a harness failure.

Literal commands, from the repo root:

```bash
.cursor/skills/verify-rusticdl/scripts/prove-download-engine.sh identity
.cursor/skills/verify-rusticdl/scripts/prove-download-engine.sh gzip-both
.cursor/skills/verify-rusticdl/scripts/prove-download-engine.sh length-mismatch
.cursor/skills/verify-rusticdl/scripts/prove-download-engine.sh resume-206
```

All four:

```bash
.cursor/skills/verify-rusticdl/scripts/prove-download-engine.sh all
```

A custom HEAD/GET fixture (same server, one download):

```bash
.cursor/skills/verify-rusticdl/scripts/prove-download-engine.sh --scenario scenario.json
```

`scenario.json` keys: `head` and `get` objects. Each accepts `status`, `content_encoding` (`null` or `"gzip"`), `content_length` (`"identity"`, `"gzip"`, `"omit"`, or an integer), and `accept_ranges`. `get` also accepts `body` (`"identity"`, `"gzip"`, `"empty"`), `ranges` (`"ignore"` or `"206"`), and `cut_after` (integer bytes to send on a non-range GET before closing, while Content-Length stays the advertised length).

Read the script's stdout. For each case it prints `state`, `error`, `target_path`, `target_exists`, `final_sha256`, `expected_sha256`, `final_matches_expected`, `wire_sha256`, `final_matches_wire`, and the server request log (`REQ HEAD` / `REQ GET` lines). A copy is written to `.cursor/skills/verify-rusticdl/artifacts/download-engine/$RUN_ID/<case>.txt`.

The helper exits 0 when the engine process observed a job (including Failed). It exits non-zero when the fixture or the engine driver itself fails to run.

## Gotchas

- The HTTP client is built with decompression off (`no_gzip` in `src/download/client.rs`) and sends `Accept-Encoding: identity` (`src/download/fetch.rs`). A server that still sends gzip is not decoded by reqwest before the engine sees the body.
- Default `multi_min_bytes` is 5 MiB (`src/settings/model.rs`). These payloads are 4096 bytes, so the engine stays on the single-stream path.
- `resume-206` depends on the engine issuing a second GET with `Range` after the short body. The server log shows whether that request happened and whether the response was 206.
- The driver uses `EngineRuntimeConfig::default()`, including six automatic retries. A retryable failure can take tens of seconds; the resume case allows 90 seconds.
- The fixture binds `127.0.0.1` only. It is not the desktop app and does not touch `$XDG_DATA_HOME` from a `launch.sh` run.
