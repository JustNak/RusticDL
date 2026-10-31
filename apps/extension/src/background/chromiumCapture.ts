/**
 * Chromium filename hints. MV3 cannot cancel via blocking webRequest, so
 * downloads.onCreated is the capture path — but Chrome fills `filename` from
 * the URL token (UUID) before it applies Content-Disposition.
 *
 * Observe response headers, the HTML `download` attribute, and
 * downloads.onDeterminingFilename so handoff can send the real name the
 * way Firefox webRequest already does.
 */
import {
  filenameExtension,
  filenameFromContentDisposition,
  httpOrigin,
  isWeakSuggestedFilename,
  knownDownloadBytes,
  mimeLooksLikeDownload,
  normalizeCaptureUrl,
  preferredSuggestedFilename,
} from './captureFilter';
import browser from './browser';

const HINT_TTL_MS = 15_000;

export type FilenameHint = {
  filename?: string;
  totalBytes?: number;
  hasAttachment?: boolean;
  mime?: string;
  ts: number;
};

export type ChromiumHeadersReceivedDetails = {
  url: string;
  responseHeaders?: Array<{ name: string; value?: string }>;
};

export type ChromiumBeforeSendHeadersDetails = {
  url: string;
  requestHeaders?: Array<{ name: string; value?: string }>;
};

export type CapturedOriginAuth = {
  origin: string;
  headers: Array<{ name: string; value: string }>;
};

export type ChromiumDeterminingFilenameItem = {
  id: number;
  url: string;
  finalUrl?: string;
  filename?: string;
};

type SuggestFn = (suggestion?: { filename?: string }) => void;

const filenameHints = new Map<string, FilenameHint>();
const determinedFilenames = new Map<number, { filename: string; ts: number }>();
const htmlDownloadHints = new Map<string, { filename: string; ts: number }>();
const REQUEST_AUTH_NAMES = new Set(['cookie', 'authorization']);
const requestAuthByUrl = new Map<string, CapturedOriginAuth & { ts: number }>();
const requestAuthByOrigin = new Map<string, CapturedOriginAuth & { ts: number }>();

function headerValue(
  headers: ChromiumHeadersReceivedDetails['responseHeaders'],
  name: string,
): string | undefined {
  const lower = name.toLowerCase();
  return headers?.find((header) => header.name.toLowerCase() === lower)?.value;
}

function contentLength(headers: ChromiumHeadersReceivedDetails['responseHeaders']): number | undefined {
  const raw = headerValue(headers, 'content-length');
  if (!raw) return undefined;
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : undefined;
}

function pruneFilenameHints(now = Date.now()): void {
  for (const [key, hint] of filenameHints) {
    if (now - hint.ts > HINT_TTL_MS) filenameHints.delete(key);
  }
  for (const [id, entry] of determinedFilenames) {
    if (now - entry.ts > HINT_TTL_MS) determinedFilenames.delete(id);
  }
  for (const [key, entry] of htmlDownloadHints) {
    if (now - entry.ts > HINT_TTL_MS) htmlDownloadHints.delete(key);
  }
  for (const [key, entry] of requestAuthByUrl) {
    if (now - entry.ts > HINT_TTL_MS) requestAuthByUrl.delete(key);
  }
  for (const [key, entry] of requestAuthByOrigin) {
    if (now - entry.ts > HINT_TTL_MS) requestAuthByOrigin.delete(key);
  }
}

/**
 * Observe Cookie / Authorization the tab actually sent. Values stay in memory
 * only and are never logged — used to replay the same hop the browser made.
 */
export function rememberRequestAuth(
  details: ChromiumBeforeSendHeadersDetails,
): CapturedOriginAuth | undefined {
  const origin = httpOrigin(details.url);
  if (!origin) return undefined;
  const headers = (details.requestHeaders ?? [])
    .filter((header) => REQUEST_AUTH_NAMES.has(header.name.toLowerCase()) && header.value)
    .map((header) => ({ name: header.name, value: header.value as string }));
  if (headers.length === 0) return undefined;
  const captured: CapturedOriginAuth & { ts: number } = { origin, headers, ts: Date.now() };
  pruneFilenameHints(captured.ts);
  requestAuthByUrl.set(normalizeCaptureUrl(details.url), captured);
  requestAuthByOrigin.set(origin, captured);
  return { origin, headers };
}

export function lookupOriginAuth(urls: Array<string | undefined>): CapturedOriginAuth[] {
  pruneFilenameHints();
  const byOrigin = new Map<string, CapturedOriginAuth['headers']>();
  for (const url of urls) {
    if (!url) continue;
    const fromUrl = requestAuthByUrl.get(normalizeCaptureUrl(url));
    const origin = httpOrigin(url);
    const fromOrigin = origin ? requestAuthByOrigin.get(origin) : undefined;
    const hit = fromUrl ?? fromOrigin;
    if (!hit || byOrigin.has(hit.origin)) continue;
    byOrigin.set(hit.origin, hit.headers);
  }
  return [...byOrigin.entries()].map(([origin, headers]) => ({ origin, headers }));
}

export function rememberResponseFilenameHint(
  details: ChromiumHeadersReceivedDetails,
): FilenameHint | undefined {
  const url = details.url;
  if (!url.startsWith('http://') && !url.startsWith('https://')) {
    return undefined;
  }
  const disposition = headerValue(details.responseHeaders, 'content-disposition') ?? '';
  const filename = filenameFromContentDisposition(disposition);
  const mime = (headerValue(details.responseHeaders, 'content-type') ?? '')
    .toLowerCase()
    .split(';')[0]
    .trim();
  const totalBytes = contentLength(details.responseHeaders);
  const hasAttachment = /\battachment\b/i.test(disposition);
  const downloadLike =
    Boolean(filename)
    || hasAttachment
    || mimeLooksLikeDownload(mime);
  if (!downloadLike || (!filename && totalBytes == null)) {
    return undefined;
  }
  const hint: FilenameHint = {
    filename,
    totalBytes,
    hasAttachment,
    mime: mime || undefined,
    ts: Date.now(),
  };
  pruneFilenameHints(hint.ts);
  filenameHints.set(normalizeCaptureUrl(url), hint);
  return hint;
}

export function lookupFilenameHint(url: string | undefined): FilenameHint | undefined {
  if (!url) return undefined;
  pruneFilenameHints();
  return filenameHints.get(normalizeCaptureUrl(url));
}

export function rememberDeterminedFilename(id: number, filename?: string): void {
  const name = filename?.split(/[\\/]/).pop()?.trim();
  if (!name) return;
  pruneFilenameHints();
  determinedFilenames.set(id, { filename: name, ts: Date.now() });
}

export function lookupDeterminedFilename(id: number | undefined): string | undefined {
  if (id == null) return undefined;
  pruneFilenameHints();
  return determinedFilenames.get(id)?.filename;
}

/**
 * Same-origin `<a download="app.zip">` name. Content-Disposition still wins.
 */
export function rememberHtmlDownloadHint(
  url: string | undefined,
  filename: string | undefined,
): string | undefined {
  const name = filename?.split(/[\\/]/).pop()?.trim();
  if (!url || !name) return undefined;
  if (!url.startsWith('http://') && !url.startsWith('https://')) return undefined;
  pruneFilenameHints();
  htmlDownloadHints.set(normalizeCaptureUrl(url), { filename: name, ts: Date.now() });
  return name;
}

export function lookupHtmlDownloadHint(url: string | undefined): string | undefined {
  if (!url) return undefined;
  pruneFilenameHints();
  return htmlDownloadHints.get(normalizeCaptureUrl(url))?.filename;
}

function hintNamesForDownload(item: {
  id?: number;
  url?: string;
  finalUrl?: string;
}): Array<string | undefined> {
  return [
    lookupFilenameHint(item.finalUrl)?.filename,
    lookupFilenameHint(item.url)?.filename,
    lookupHtmlDownloadHint(item.finalUrl),
    lookupHtmlDownloadHint(item.url),
    lookupDeterminedFilename(item.id),
  ];
}

/**
 * Last path segment, including extension-less names (`/get?id=5` → `get`).
 * Firefox `basenameFromUrl` requires a dot and is left unchanged.
 */
export function downloadUrlBasename(url: string | undefined): string | undefined {
  if (!url) return undefined;
  try {
    const base = new URL(url).pathname.split('/').filter(Boolean).pop()?.trim();
    if (!base) return undefined;
    try {
      return decodeURIComponent(base) || undefined;
    } catch {
      return base;
    }
  } catch {
    return undefined;
  }
}

export function resolveSuggestedFilename(item: {
  id?: number;
  url?: string;
  finalUrl?: string;
  filename?: string;
}): string | undefined {
  const names = [
    ...hintNamesForDownload(item),
    item.filename,
    downloadUrlBasename(item.finalUrl || item.url),
  ];
  const withExt = names.find(
    (name) => Boolean(filenameExtension(name)) && !isWeakSuggestedFilename(name),
  );
  return withExt ?? preferredSuggestedFilename(...names);
}

type ChromiumHintedItem = {
  id?: number;
  url: string;
  finalUrl?: string;
  filename?: string;
  mime?: string;
  totalBytes?: number;
  fileSize?: number;
  hasAttachment?: boolean;
};

/**
 * Copy header-hint size / attachment / filename onto a Chromium onCreated
 * item, then fall back to the determined name and the URL basename when
 * `item.filename` is empty.
 */
export function applyChromiumCaptureHints<T extends ChromiumHintedItem>(item: T): T {
  const hint = lookupFilenameHint(item.finalUrl || item.url) ?? lookupFilenameHint(item.url);
  const next: T = { ...item };
  if (knownDownloadBytes(item) == null && hint?.totalBytes != null) {
    next.totalBytes = hint.totalBytes;
  }
  if (hint?.hasAttachment) {
    next.hasAttachment = true;
  }
  if (!next.mime && hint?.mime) {
    next.mime = hint.mime;
  }
  const resolved = resolveSuggestedFilename(next);
  if (resolved) next.filename = resolved;
  return next;
}

type ChromiumWebRequestApi = {
  onHeadersReceived?: {
    addListener(
      listener: (details: ChromiumHeadersReceivedDetails) => void,
      filter: { urls: string[] },
      extraInfoSpec: string[],
    ): void;
  };
  onBeforeSendHeaders?: {
    addListener(
      listener: (details: ChromiumBeforeSendHeadersDetails) => void,
      filter: { urls: string[] },
      extraInfoSpec: string[],
    ): void;
  };
  onBeforeRedirect?: {
    addListener(
      listener: (details: { url: string; redirectUrl?: string }) => void,
      filter: { urls: string[] },
    ): void;
  };
};

export function getChromiumWebRequest(): ChromiumWebRequestApi | null {
  if (navigator.userAgent.toLowerCase().includes('firefox')) {
    return null;
  }
  const wr = (browser as unknown as { webRequest?: ChromiumWebRequestApi }).webRequest;
  if (!wr?.onHeadersReceived && !wr?.onBeforeSendHeaders) {
    return null;
  }
  return wr ?? null;
}

export function getChromiumDeterminingFilenameApi(): {
  addListener(listener: (item: ChromiumDeterminingFilenameItem, suggest: SuggestFn) => void): void;
} | null {
  if (navigator.userAgent.toLowerCase().includes('firefox')) {
    return null;
  }
  const g = globalThis as {
    chrome?: {
      downloads?: {
        onDeterminingFilename?: {
          addListener(
            listener: (item: ChromiumDeterminingFilenameItem, suggest: SuggestFn) => void,
          ): void;
        };
      };
    };
    browser?: {
      downloads?: {
        onDeterminingFilename?: {
          addListener(
            listener: (item: ChromiumDeterminingFilenameItem, suggest: SuggestFn) => void,
          ): void;
        };
      };
    };
  };
  return g.chrome?.downloads?.onDeterminingFilename
    ?? g.browser?.downloads?.onDeterminingFilename
    ?? null;
}

/**
 * Observe Chrome's determined name. Always `suggest()` with no override —
 * rewriting the shelf name is a side effect when we are not capturing.
 * Chrome times out if suggest is not called synchronously.
 */
export function applyDeterminedFilename(
  item: ChromiumDeterminingFilenameItem,
  suggest: SuggestFn,
): string | undefined {
  try {
    suggest();
  } catch {
    // already settled / API rejected a second suggest
  }
  rememberDeterminedFilename(item.id, item.filename);
  return resolveSuggestedFilename(item);
}
