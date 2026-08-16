/**
 * Shared download-capture heuristics (Firefox webRequest + downloads.onCreated).
 *
 * Bias: miss an exotic file rather than steal page data fetches. Sites routinely
 * serve `.csv` / `.json` as live APIs (Nexus Mods download-count stats, etc.).
 *
 * File hosts (Gofile, Buzzheavier, SteamRIP mirrors) often emit a tiny HTML
 * "ticket" / wait page with `Content-Disposition: filename="Game.rar"` (~3 KB)
 * *before* the real archive. Catching that stub cancels the real transfer and
 * completes a 3 KB `.rar` of HTML. Known-small bodies are never captured.
 */
import {
  isUrlHostExcludedByPatterns,
  type ExtensionIntegrationSettings,
} from '@rusticdl/protocol';

/**
 * Skip known-size responses smaller than this. A 3 KB `.rar` is a wait-page
 * stub, not a user file. Context-menu still works for tiny legit files.
 */
export const MIN_CAPTURE_BYTES = 8 * 1024;

/**
 * Extensions that pages commonly fetch as data, not as user-initiated files.
 * Even if the user added them to capturedFileExtensions, filename-alone is
 * never enough — need a download MIME / top-level navigation.
 */
export const WEAK_CAPTURE_EXTENSIONS = new Set([
  'csv',
  'tsv',
  'json',
  'xml',
  'txt',
  'html',
  'htm',
  'js',
  'mjs',
  'css',
  'map',
]);

/** MIME substrings that mean "this is a real file the browser is saving". */
export const DOWNLOAD_ITEM_MIME_HINTS = [
  'octet-stream',
  'zip',
  'x-rar',
  'x-7z',
  'x-tar',
  'gzip',
  'x-bzip',
  'x-xz',
  'pdf',
  'msdownload',
  'x-msi',
  'java-archive',
  'android.package',
  'x-iso9660',
  'x-apple-diskimage',
  'x-debian',
  'x-redhat-package',
  'vnd.ms-excel',
  'vnd.ms-powerpoint',
  'msword',
  'officedocument',
];

export type DownloadItemLike = {
  url: string;
  finalUrl?: string;
  filename?: string;
  mime?: string;
  totalBytes?: number;
  fileSize?: number;
  bytesReceived?: number;
  referrer?: string;
  byExtensionId?: string;
  incognito?: boolean;
  cookieStoreId?: string;
};

/** Prefer Firefox totalBytes, then fileSize (totalBytes is often -1 onCreated). */
export function knownDownloadBytes(item: DownloadItemLike): number | undefined {
  if (item.totalBytes && item.totalBytes > 0) return item.totalBytes;
  if (item.fileSize && item.fileSize > 0) return item.fileSize;
  return undefined;
}

export function filenameExtension(filename: string | undefined): string | undefined {
  if (!filename) return undefined;
  const base = filename.split(/[\\/]/).pop() ?? filename;
  const dot = base.lastIndexOf('.');
  if (dot < 0 || dot === base.length - 1) return undefined;
  const ext = base.slice(dot + 1).toLowerCase();
  if (!/^[a-z0-9]{1,10}$/.test(ext)) return undefined;
  return ext;
}

/** Last path segment, ignoring folders Chrome/Firefox put on DownloadItem.filename. */
export function downloadBasename(filename: string | undefined): string | undefined {
  const base = filename?.split(/[\\/]/).pop()?.trim();
  return base || undefined;
}

/**
 * Parse Content-Disposition `filename` / `filename*`.
 * Chromium onCreated often only has the URL token; Firefox webRequest already
 * uses this so the desktop confirm dialog gets the real name.
 */
export function filenameFromContentDisposition(value: string): string | undefined {
  const star = value.match(/filename\*\s*=\s*(?:UTF-8''|utf-8'')?([^;]+)/i);
  if (star?.[1]) {
    try {
      return decodeURIComponent(star[1].trim().replace(/^"|"$/g, '')) || undefined;
    } catch {
      // fall through to the plain filename= parameter
    }
  }
  const plain = value.match(/filename\s*=\s*("?)([^";]+)\1/i);
  return plain?.[2]?.trim() || undefined;
}

/**
 * True when the name is a CDN token / Chrome temp name, not a user-facing file.
 */
export function isWeakSuggestedFilename(filename: string | undefined): boolean {
  const base = downloadBasename(filename);
  if (!base) return true;
  if (/\.crdownload$/i.test(base)) return true;
  if (/^unconfirmed\s+\d+/i.test(base)) return true;
  if (/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(base)) {
    return true;
  }
  return /^[0-9a-f]{20,}$/i.test(base);
}

/** Prefer a Content-Disposition / Chrome-determined name over a URL token. */
export function preferredSuggestedFilename(
  ...candidates: Array<string | undefined>
): string | undefined {
  const names = candidates
    .map(downloadBasename)
    .filter((value): value is string => Boolean(value));
  return names.find((name) => !isWeakSuggestedFilename(name)) ?? names[0];
}

export function isWeakCaptureExtension(ext: string | undefined): boolean {
  return Boolean(ext && WEAK_CAPTURE_EXTENSIONS.has(ext));
}

/** Page documents / XHR-style payloads — never treat as a file download. */
export function isPageOrApiMime(mime: string): boolean {
  if (!mime) return false;
  if (
    mime === 'text/plain' ||
    mime === 'text/html' ||
    mime === 'text/css' ||
    mime === 'text/javascript' ||
    mime === 'text/csv' ||
    mime === 'text/tab-separated-values' ||
    mime === 'application/json' ||
    mime === 'application/javascript' ||
    mime === 'application/xml' ||
    mime === 'application/xhtml+xml' ||
    mime === 'application/csv' ||
    mime === 'text/xml'
  ) {
    return true;
  }
  if (mime.startsWith('text/html')) return true;
  if (mime.endsWith('+json') || mime.endsWith('+xml')) return true;
  return false;
}

export function mimeLooksLikeDownload(mime: string): boolean {
  if (!mime) return false;
  return DOWNLOAD_ITEM_MIME_HINTS.some((hint) => mime.includes(hint));
}

/**
 * downloads.onCreated filter (Firefox fallback + Chromium primary).
 *
 * Never capture just because a filename exists — Firefox always supplies one,
 * and pages fetch `.csv` / `.json` as data.
 */
export function shouldCaptureDownloadItem(
  item: DownloadItemLike,
  settings: ExtensionIntegrationSettings,
): boolean {
  if (!settings.enabled || settings.downloadHandoffMode === 'off') return false;
  const url = item.finalUrl || item.url;
  if (!url || !(url.startsWith('http://') || url.startsWith('https://'))) return false;
  if (isUrlHostExcludedByPatterns(url, settings.excludedHosts)) return false;
  if (item.byExtensionId) return false;
  if (url.startsWith('blob:') || url.startsWith('data:')) return false;

  const mime = (item.mime || '').toLowerCase().split(';')[0].trim();
  const ext = filenameExtension(item.filename);
  const ignored = new Set(
    (settings.ignoredFileExtensions ?? []).map((e) => e.toLowerCase()),
  );
  if (ext && ignored.has(ext)) return false;

  const captured = new Set(settings.capturedFileExtensions.map((e) => e.toLowerCase()));
  const strongName = Boolean(ext && captured.has(ext) && !isWeakCaptureExtension(ext));
  const dispositionHint = mimeLooksLikeDownload(mime);

  // text/plain is common for user-added .log/.srt; only veto weak/unknown names.
  if (isPageOrApiMime(mime) && !strongName) return false;

  // Media MIME types are usually page assets; allow only when the filename matches
  // a user-configured *strong* captured extension (e.g. user added mp3/mp4/png).
  if (
    (mime.startsWith('image/') ||
      mime.startsWith('audio/') ||
      mime.startsWith('video/') ||
      mime.startsWith('font/')) &&
    !strongName
  ) {
    return false;
  }

  const knownBytes = knownDownloadBytes(item);
  // Known-small bodies are stubs even with a captured extension.
  if (knownBytes != null && knownBytes < MIN_CAPTURE_BYTES) {
    return false;
  }

  if (isWeakCaptureExtension(ext)) {
    return Boolean(ext && captured.has(ext) && dispositionHint);
  }

  if (strongName) return true;
  if (ext) return false;
  return dispositionHint;
}

/**
 * True when onCreated fired before Firefox filled in `totalBytes` for a
 * would-be capture. Wait for downloads.onChanged so we can reject a 3 KB stub
 * instead of handing it off at `totalBytes: -1`.
 */
export function shouldWaitForDownloadSize(
  item: DownloadItemLike,
  settings: ExtensionIntegrationSettings,
): boolean {
  if (knownDownloadBytes(item) != null) return false;
  // Assume a non-stub size so wait matches the eventual capture decision.
  return shouldCaptureDownloadItem({ ...item, totalBytes: MIN_CAPTURE_BYTES }, settings);
}

/** Wait for size before capturing unknown-size strong names (3 KB wait-page stubs). */
export function downloadCreatedAction(
  item: DownloadItemLike,
  settings: ExtensionIntegrationSettings,
): 'wait' | 'capture' | 'ignore' {
  if (shouldWaitForDownloadSize(item, settings)) return 'wait';
  if (shouldCaptureDownloadItem(item, settings)) return 'capture';
  return 'ignore';
}

/** Pause immediately — do not let Firefox keep reading the body while we wait or hand off. */
export function shouldPauseDownloadItem(
  item: DownloadItemLike,
  settings: ExtensionIntegrationSettings,
): boolean {
  const action = downloadCreatedAction(item, settings);
  return action === 'capture' || action === 'wait';
}

/** Strip hash so the same file is recognized after a fragment-only change. */
export function normalizeCaptureUrl(url: string): string {
  try {
    const parsed = new URL(url);
    parsed.hash = '';
    return parsed.href;
  } catch {
    return url;
  }
}

/**
 * Firefox collision names: `file (1).rar` / `file(1).rar`.
 * Only strip 1–2 digit suffixes so `Game (2024).rar` stays intact.
 */
export function canonicalDownloadFilename(filename: string | undefined): string | undefined {
  if (!filename) return undefined;
  const base = filename.split(/[\\/]/).pop() ?? filename;
  const stripped = base.replace(/\s*\(\d{1,2}\)(?=\.[^.]+$)/, '');
  return stripped || undefined;
}

export type InterceptedDownload = {
  url: string;
  filename?: string;
  ts: number;
};

/**
 * True when this downloads.onCreated item is the browser ghost of a capture
 * we already canceled (blocking webRequest) or claimed for handoff.
 */
export function matchesInterceptedDownload(
  item: Pick<DownloadItemLike, 'url' | 'finalUrl' | 'filename'>,
  intercepted: Iterable<InterceptedDownload>,
  now = Date.now(),
  ttlMs = 15_000,
): boolean {
  const itemUrls = [item.url, item.finalUrl]
    .filter((value): value is string => Boolean(value))
    .map(normalizeCaptureUrl);
  const itemName = canonicalDownloadFilename(item.filename);

  for (const entry of intercepted) {
    if (now - entry.ts > ttlMs) continue;
    if (itemUrls.includes(normalizeCaptureUrl(entry.url))) return true;
    const entryName = canonicalDownloadFilename(entry.filename);
    if (itemName && entryName && itemName === entryName) return true;
  }
  return false;
}

export function urlIsClaimed(
  url: string | undefined,
  claimedUrls: Iterable<string>,
): boolean {
  if (!url) return false;
  const normalized = normalizeCaptureUrl(url);
  for (const claimed of claimedUrls) {
    if (normalizeCaptureUrl(claimed) === normalized) return true;
  }
  return false;
}
