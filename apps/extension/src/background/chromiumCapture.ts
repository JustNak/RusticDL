/**
 * Chromium filename hints. MV3 cannot cancel via blocking webRequest, so
 * downloads.onCreated is the capture path — but Chrome fills `filename` from
 * the URL token (UUID) before it applies Content-Disposition.
 *
 * Observe response headers and downloads.onDeterminingFilename so handoff
 * can send the real name the way Firefox webRequest already does.
 */
import {
  filenameFromContentDisposition,
  mimeLooksLikeDownload,
  normalizeCaptureUrl,
  preferredSuggestedFilename,
} from './captureFilter';
import browser from './browser';

const HINT_TTL_MS = 15_000;

export type FilenameHint = {
  filename?: string;
  totalBytes?: number;
  ts: number;
};

export type ChromiumHeadersReceivedDetails = {
  url: string;
  responseHeaders?: Array<{ name: string; value?: string }>;
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
  const downloadLike =
    Boolean(filename)
    || /\battachment\b/i.test(disposition)
    || mimeLooksLikeDownload(mime);
  if (!downloadLike || (!filename && totalBytes == null)) {
    return undefined;
  }
  const hint: FilenameHint = { filename, totalBytes, ts: Date.now() };
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

function hintNamesForDownload(item: {
  id?: number;
  url?: string;
  finalUrl?: string;
}): Array<string | undefined> {
  return [
    lookupFilenameHint(item.finalUrl)?.filename,
    lookupFilenameHint(item.url)?.filename,
    lookupDeterminedFilename(item.id),
  ];
}

export function resolveSuggestedFilename(item: {
  id?: number;
  url?: string;
  finalUrl?: string;
  filename?: string;
}): string | undefined {
  return preferredSuggestedFilename(...hintNamesForDownload(item), item.filename);
}

export function getChromiumWebRequest(): {
  onHeadersReceived: {
    addListener(
      listener: (details: ChromiumHeadersReceivedDetails) => void,
      filter: { urls: string[] },
      extraInfoSpec: string[],
    ): void;
  };
} | null {
  if (navigator.userAgent.toLowerCase().includes('firefox')) {
    return null;
  }
  const wr = (browser as unknown as {
    webRequest?: {
      onHeadersReceived?: {
        addListener(
          listener: (details: ChromiumHeadersReceivedDetails) => void,
          filter: { urls: string[] },
          extraInfoSpec: string[],
        ): void;
      };
    };
  }).webRequest;
  return wr?.onHeadersReceived ? { onHeadersReceived: wr.onHeadersReceived } : null;
}

export function getChromiumDeterminingFilenameApi(): {
  addListener(listener: (item: ChromiumDeterminingFilenameItem, suggest: SuggestFn) => void): void;
} | null {
  if (navigator.userAgent.toLowerCase().includes('firefox')) {
    return null;
  }
  const api = (globalThis as {
    chrome?: {
      downloads?: {
        onDeterminingFilename?: {
          addListener(
            listener: (item: ChromiumDeterminingFilenameItem, suggest: SuggestFn) => void,
          ): void;
        };
      };
    };
  }).chrome?.downloads?.onDeterminingFilename;
  return api ?? null;
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
