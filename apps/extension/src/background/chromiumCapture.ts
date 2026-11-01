/**
 * Chromium filename hints. MV3 cannot cancel via blocking webRequest, so
 * downloads.onCreated is the capture path — but Chrome fills `filename` from
 * the URL token (UUID) before it applies Content-Disposition.
 *
 * Capture decision uses header hints + a sanitized URL basename when
 * onCreated is empty. The name sent to the host comes from
 * downloads.onDeterminingFilename (Chrome's final name), then disposition,
 * then that basename. Page-controlled HTML `download` is not an authority.
 */
import {
  filenameExtension,
  filenameFromContentDisposition,
  httpOrigin,
  isNonUserInitiatedResourceType,
  isWeakSuggestedFilename,
  knownDownloadBytes,
  mimeLooksLikeDownload,
  normalizeCaptureUrl,
  preferredSuggestedFilename,
  sanitizeSuggestedFilename,
} from './captureFilter';
import browser from './browser';

const HINT_TTL_MS = 15_000;

export type FilenameHint = {
  filename?: string;
  totalBytes?: number;
  hasAttachment?: boolean;
  mime?: string;
  resourceType?: string;
  ts: number;
};

export type ChromiumHeadersReceivedDetails = {
  url: string;
  type?: string;
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
const resourceTypes = new Map<string, { type: string; ts: number }>();
const REQUEST_AUTH_NAMES = new Set(['cookie', 'authorization']);
const requestAuthByUrl = new Map<string, CapturedOriginAuth & { ts: number }>();
const requestAuthByOrigin = new Map<string, CapturedOriginAuth & { ts: number }>();

function selfExtensionId(): string | undefined {
  const g = globalThis as {
    chrome?: { runtime?: { id?: string } };
    browser?: { runtime?: { id?: string } };
  };
  return g.chrome?.runtime?.id || g.browser?.runtime?.id || browser.runtime?.id;
}

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
  for (const [key, entry] of resourceTypes) {
    if (now - entry.ts > HINT_TTL_MS) resourceTypes.delete(key);
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
  pruneFilenameHints();
  const key = normalizeCaptureUrl(url);
  const existing = filenameHints.get(key);
  const resourceType = isNonUserInitiatedResourceType(existing?.resourceType)
    ? existing?.resourceType
    : details.type;
  const hint: FilenameHint = {
    filename: sanitizeSuggestedFilename(filename),
    totalBytes,
    hasAttachment,
    mime: mime || undefined,
    resourceType,
    ts: Date.now(),
  };
  filenameHints.set(key, hint);
  return hint;
}

export function lookupFilenameHint(url: string | undefined): FilenameHint | undefined {
  if (!url) return undefined;
  pruneFilenameHints();
  return filenameHints.get(normalizeCaptureUrl(url));
}

export function rememberDeterminedFilename(id: number, filename?: string): void {
  const name = sanitizeSuggestedFilename(filename);
  if (!name) return;
  pruneFilenameHints();
  determinedFilenames.set(id, { filename: name, ts: Date.now() });
}

export function rememberResourceType(url: string | undefined, type: string | undefined): void {
  if (!url || !type) return;
  pruneFilenameHints();
  const key = normalizeCaptureUrl(url);
  const existing = resourceTypes.get(key)?.type;
  // iframe / <object> veto is sticky per URL so a later fetch/img cannot
  // flip the same download into a capture (W195-1).
  if (isNonUserInitiatedResourceType(existing)) return;
  resourceTypes.set(key, { type, ts: Date.now() });
}

export function lookupResourceType(url: string | undefined): string | undefined {
  if (!url) return undefined;
  pruneFilenameHints();
  return resourceTypes.get(normalizeCaptureUrl(url))?.type
    ?? filenameHints.get(normalizeCaptureUrl(url))?.resourceType;
}

export function lookupDeterminedFilename(id: number | undefined): string | undefined {
  if (id == null) return undefined;
  pruneFilenameHints();
  return determinedFilenames.get(id)?.filename;
}

/**
 * Last path segment, including extension-less names (`/get?id=5` → `get`).
 * Encoded `%2f`/`%5c` and `..` are rejected. Firefox `basenameFromUrl` is unchanged.
 */
export function downloadUrlBasename(url: string | undefined): string | undefined {
  if (!url) return undefined;
  try {
    const base = new URL(url).pathname.split('/').filter(Boolean).pop()?.trim();
    return sanitizeSuggestedFilename(base);
  } catch {
    return undefined;
  }
}

export function resolveSuggestedFilename(item: {
  id?: number;
  url?: string;
  finalUrl?: string;
  filename?: string;
  chromeDeterminedFilename?: string;
}): string | undefined {
  // Chrome's determined name is the send-name authority, even when it has no
  // extension (`export`). A later candidate with an extension (`x.zip`) must
  // not replace it — that was the page-hint / first-with-ext leak.
  const determined = sanitizeSuggestedFilename(lookupDeterminedFilename(item.id));
  if (determined && !isWeakSuggestedFilename(determined)) {
    return determined;
  }
  // Playwright's CDP allowAndName skips onDeterminingFilename. Raw Chrome and
  // CDP `default` fire it; the shelf name is send-only fallback, never a
  // capture-decision input.
  const shelfName = sanitizeSuggestedFilename(item.chromeDeterminedFilename);
  if (shelfName && !isWeakSuggestedFilename(shelfName)) {
    return shelfName;
  }
  const names = [
    sanitizeSuggestedFilename(lookupFilenameHint(item.finalUrl)?.filename),
    sanitizeSuggestedFilename(lookupFilenameHint(item.url)?.filename),
    sanitizeSuggestedFilename(item.filename),
    downloadUrlBasename(item.finalUrl || item.url),
  ];
  const withExt = names.find(
    (name) => Boolean(filenameExtension(name)) && !isWeakSuggestedFilename(name),
  );
  return withExt ?? preferredSuggestedFilename(determined, ...names);
}

/**
 * True while Chrome may still replace an extensionless URL token (C1 `C1` /
 * `/dl` → `app.zip` from onDeterminingFilename). False as soon as a non-weak
 * determined or shelf name is known, or the send name already has an
 * extension (`huge.bin`). Extensionless G4 (`get`) still waits — skipping
 * that wait sent the URL token before Chrome determined `app.zip`.
 */
export function needsDeterminedFilename(item: {
  id?: number;
  url?: string;
  finalUrl?: string;
  filename?: string;
  mime?: string;
  hasAttachment?: boolean;
  filenameFromUrlBasename?: boolean;
  chromeDeterminedFilename?: string;
}): boolean {
  const determined = sanitizeSuggestedFilename(lookupDeterminedFilename(item.id));
  if (determined && !isWeakSuggestedFilename(determined)) return false;
  const shelf = sanitizeSuggestedFilename(item.chromeDeterminedFilename);
  if (shelf && !isWeakSuggestedFilename(shelf)) return false;
  const resolved = resolveSuggestedFilename(item);
  if (isWeakSuggestedFilename(resolved)) return true;
  if (filenameExtension(resolved) != null) return false;
  return true;
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
  filenameFromUrlBasename?: boolean;
  resourceType?: string;
  byExtensionId?: string;
  chromeDeterminedFilename?: string;
};

/**
 * Copy header-hint size / attachment / disposition onto a Chromium onCreated
 * item. Empty `filename` falls back to the sanitized URL basename for the
 * capture decision only — not Chrome's determined name (that is send-only).
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
  const resourceType =
    lookupResourceType(item.finalUrl)
    ?? lookupResourceType(item.url)
    ?? hint?.resourceType;
  if (resourceType) next.resourceType = resourceType;
  // Chrome sets byExtensionId when our onDeterminingFilename listener runs.
  // That is not "this extension started the download" (restore uses sessions).
  const selfId = selfExtensionId();
  if (selfId && next.byExtensionId === selfId) {
    next.byExtensionId = undefined;
  }

  const original = sanitizeSuggestedFilename(item.filename);
  const emptyOrWeak = !original || isWeakSuggestedFilename(original);
  if (emptyOrWeak) {
    const dispositionName = sanitizeSuggestedFilename(hint?.filename);
    const urlBase = downloadUrlBasename(item.finalUrl || item.url);
    if (dispositionName) {
      next.filename = dispositionName;
      next.filenameFromUrlBasename = false;
    } else if (urlBase) {
      next.filename = urlBase;
      next.filenameFromUrlBasename = true;
    } else {
      next.filename = original;
    }
  } else {
    next.filename = original;
  }
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
  // Prefer the native chrome event. The polyfill may expose a dummy that
  // never fires, which is why raw Chrome logged determining while we sent
  // the URL token.
  return g.chrome?.downloads?.onDeterminingFilename ?? null;
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
