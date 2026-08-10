# Protocol (RusticDL browser bridge)

## Architecture

```text
Browser extension
  → native messaging (stdio, length-prefixed JSON)
  → native host
  → named pipe \\.\pipe\rusticdl.v1
  → desktop app
```

Native host name: `com.rusticdl.native_host`  
Protocol version: `1`

## Extension → host

```json
{
  "protocolVersion": 1,
  "requestId": "uuid",
  "type": "enqueue_download",
  "payload": {}
}
```

Supported types:

- `ping` / `get_status`
- `enqueue_download`
- `prompt_download` — desktop shows an in-app confirm dialog (filename + save folder). User can start or dismiss; 5-minute timeout dismisses.
- `open_app`
- `save_extension_settings`

Only `http` and `https` URLs are accepted.

`get_status` / `ping` return queue summary and extension integration settings. The desktop may still include `appearanceSettings` (`theme`, `accentColor`) for older clients, but the extension owns its own theme/accent in `browser.storage.local` and does not mirror the desktop look.

## Security

- Extension is untrusted; desktop re-validates URLs, sizes, and rate limits.
- Destination path is never accepted from the extension.
- Browser session headers (`handoffAuth`) are memory-only and never written to `state.json`.
- Named pipe rejects remote clients.
