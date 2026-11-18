# Protocol (RusticDL browser bridge)

## Architecture

```text
Browser extension
  → native messaging (stdio, length-prefixed JSON)
  → native host
  → Windows: named pipe \\.\pipe\rusticdl.v1
    Linux:   Unix socket $XDG_RUNTIME_DIR/rusticdl.v1.sock
  → desktop app
```

Native host name: `com.rusticdl.native_host` (display name: **RusticDL Backend**)  
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
- `prompt_download` — desktop shows an in-app confirm dialog (filename + save folder). If that name already exists in the save folder, a file-exists popup is shown instead (Rename fills `file (1).ext`, Start download rejects a taken name, overwrite, or cancel). User can start or cancel; cancel (and the 5-minute timeout) abort and do not restore the browser download. The extension protocol is unchanged.
- `open_app`
- `save_extension_settings`

Only `http` and `https` URLs are accepted.

`get_status` / `ping` return queue summary and extension integration settings. The desktop may still include `appearanceSettings` (`theme`, `accentColor`) for older clients, but the extension owns its own theme/accent in `browser.storage.local` and does not mirror the desktop look.

## Security

- Extension is untrusted; desktop re-validates URLs, sizes, and rate limits.
- Destination path is never accepted from the extension.
- Browser session headers (`handoffAuth`) are memory-only and never written to `state.json`.
- `handoffAuth.originAuth` is optional per-origin Cookie/Authorization so a Canvas → Drive redirect can keep the Drive session without sending Canvas cookies cross-origin.
- The extension only collects cookies the browser itself would have sent:
  - Cookies are read from the download's own cookie store. Incognito or container downloads never fall back to the default store. If a private download's store can't be found, no cookies are sent.
  - SameSite applies relative to the page that started the download. A cross-site navigation drops `Strict` cookies. A cross-site subresource or XHR capture keeps only `SameSite=None` cookies.
  - `Referer` follows `strict-origin-when-cross-origin`: only the origin is sent cross-origin, and nothing is sent on an https → http downgrade.
  - Chromium request auth is only observed on `main_frame` / `sub_frame` / `other` requests, never page XHR or fetch. It is tagged by tab profile so incognito and normal sessions never share it.
- A rejected handoff restores the download only in its original profile or container. Chromium can't target incognito, so a private download is not restored into the normal profile.
- The background script only accepts popup/options messages from this extension's own pages.
- Browser captures replay the LMS session URL (not a consumed Inst-FS / Drive `finalUrl`). The desktop discovers the signed Location without fetching it during preflight; a 401 on that hop remints once from the session URL.
- Named pipe rejects remote clients. The Linux socket is `0600` under `$XDG_RUNTIME_DIR`.
