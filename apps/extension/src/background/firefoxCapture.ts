/**
 * Firefox primary download capture via blocking webRequest.
 * Cancels eligible responses before the browser commits the file, then hands off.
 *
 * Heuristics intentionally err on the side of *not* capturing. False positives
 * (YouTube suggestqueries, Nexus stats CSVs, 3 KB HTML wait-pages named .rar,
 * Grok-style in-page PDF/Office preview fetches) are far more annoying than
 * missing an exotic download — those still fall through to downloads.onCreated
 * or the context menu. Large file-host XHRs (Gofile / Pixeldrain) are
 * intercepted so they do not become blob: downloads.
 */
import {
  isUrlHostExcludedByPatterns,
  type ExtensionIntegrationSettings,
} from '@rusticdl/protocol';
import {
  MIN_CAPTURE_BYTES,
  filenameFromContentDisposition,
  isPageOrApiMime,
  isPreviewableDownload,
  isWeakCaptureExtension,
  shouldSkipAppOwnedDownloadOrigin,
} from './captureFilter';
import browser from './browser';

/** XHR is noisy; only intercept when the body is clearly a large file. */
export const MIN_XHR_CAPTURE_BYTES = 64 * 1024;

export type FirefoxWebRequestHeader = { name: string; value?: string };

export type FirefoxHeadersReceivedDetails = {
  requestId?: string;
  url: string;
  originUrl?: string;
  documentUrl?: string;
  method?: string;
  type?: string;
  statusCode?: number;
  responseHeaders?: FirefoxWebRequestHeader[];
  incognito?: boolean;
  cookieStoreId?: string;
};

export type FirefoxCaptureCandidate = {
  url: string;
  filename?: string;
  totalBytes?: number;
  pageUrl?: string;
  referrer?: string;
  incognito?: boolean;
  cookieStoreId?: string;
  reason: string;
};

export {
  MIN_CAPTURE_BYTES,
  canonicalDownloadFilename,
  cookieStoreIdForHandoff,
  cookieUrlsForHandoff,
  downloadBasename,
  downloadCreatedAction,
  filenameFromContentDisposition,
  followRestoreSkip,
  RESTORE_SKIP_TTL_MS,
  handoffUrlForCapturedDownload,
  httpOrigin,
  isEphemeralSignedUrl,
  isPreviewableDownload,
  isSessionGatewayUrl,
  isWeakSuggestedFilename,
  lookupRedirectSessionUrl,
  rememberDownloadRedirect,
  resetDownloadRedirectsForTests,
  knownDownloadBytes,
  matchesInterceptedDownload,
  normalizeCaptureUrl,
  preferredSuggestedFilename,
  shouldCaptureDownloadItem,
  shouldPauseDownloadItem,
  shouldSkipAppOwnedDownloadOrigin,
  shouldWaitForDownloadSize,
  urlIsClaimed,
} from './captureFilter';

const DOWNLOAD_MIME = new Set([
  'application/zip',
  'application/x-zip-compressed',
  'application/pdf',
  'application/octet-stream',
  'application/x-msdownload',
  'application/x-msi',
  'application/gzip',
  'application/x-7z-compressed',
  'application/x-rar-compressed',
  'application/vnd.rar',
  'application/x-tar',
  'application/x-bzip2',
  'application/x-xz',
  'application/java-archive',
  'application/vnd.android.package-archive',
  'application/x-iso9660-image',
  'application/x-apple-diskimage',
  'application/x-debian-package',
  'application/x-redhat-package-manager',
  'application/vnd.ms-excel',
  'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
  'application/msword',
  'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
  'application/vnd.ms-powerpoint',
  'application/vnd.openxmlformats-officedocument.presentationml.presentation',
]);

/** MIME types that are almost never intentional file downloads. */
const NON_DOWNLOAD_MIME = new Set([
  'application/json',
  'application/javascript',
  'application/ecmascript',
  'application/xml',
  'application/xhtml+xml',
  'application/x-javascript',
  'application/ld+json',
  'application/manifest+json',
  'application/x-www-form-urlencoded',
  'application/graphql',
  'application/grpc',
  'application/grpc+proto',
  'application/x-protobuf',
  'application/wasm',
  'text/event-stream',
  'multipart/form-data',
]);

/**
 * Host/path patterns that commonly emit tiny attachment-like responses but are
 * never user downloads (autocomplete, telemetry, ads beacons, etc.).
 */
const NON_DOWNLOAD_URL_RE = [
  /suggestqueries/i,
  /\/complete\/search\b/i,
  /\/gen_204\b/i,
  /\/generate_204\b/i,
  /\/pagead\//i,
  /\/ptracking\b/i,
  /\/api\/stats\b/i,
  /\/log[_-]?event\b/i,
  /\/beacon\b/i,
  /\/telemetry\b/i,
  /google-analytics\.com/i,
  /\/collect\?/i,
  /doubleclick\.net/i,
  /\/safebrowsing\//i,
  /client[_-]?s\.google\.com/i,
];

function headerValue(headers: FirefoxWebRequestHeader[] | undefined, name: string): string | undefined {
  const lower = name.toLowerCase();
  return headers?.find((h) => h.name.toLowerCase() === lower)?.value;
}

function contentType(headers: FirefoxWebRequestHeader[] | undefined): string {
  return (headerValue(headers, 'content-type') ?? '').split(';')[0].trim().toLowerCase();
}

function contentDisposition(headers: FirefoxWebRequestHeader[] | undefined): string {
  return headerValue(headers, 'content-disposition') ?? '';
}

function contentLength(headers: FirefoxWebRequestHeader[] | undefined): number | undefined {
  const raw = headerValue(headers, 'content-length');
  if (!raw) return undefined;
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : undefined;
}

export function basenameFromUrl(url: string): string | undefined {
  try {
    const path = new URL(url).pathname;
    const base = path.split('/').filter(Boolean).pop();
    if (!base || !base.includes('.')) return undefined;
    return decodeURIComponent(base);
  } catch {
    return undefined;
  }
}

export function extensionOf(name: string | undefined): string | undefined {
  if (!name) return undefined;
  const base = name.split(/[\\/]/).pop() ?? name;
  const dot = base.lastIndexOf('.');
  if (dot < 0 || dot === base.length - 1) return undefined;
  // Only accept simple alphanumeric extensions (1–10 chars).
  const ext = base.slice(dot + 1).toLowerCase();
  if (!/^[a-z0-9]{1,10}$/.test(ext)) return undefined;
  return ext;
}

function isNonDownloadMime(mime: string): boolean {
  if (!mime) return false;
  if (NON_DOWNLOAD_MIME.has(mime)) return true;
  if (isPageOrApiMime(mime)) return true;
  if (mime.startsWith('text/')) return true;
  if (mime.startsWith('image/')) return true;
  if (mime.startsWith('audio/')) return true;
  if (mime.startsWith('video/')) return true;
  if (mime.startsWith('font/')) return true;
  if (mime.endsWith('+json') || mime.endsWith('+xml')) return true;
  return false;
}

function hasCorsAllowOrigin(headers: FirefoxWebRequestHeader[] | undefined): boolean {
  return Boolean(headerValue(headers, 'access-control-allow-origin'));
}

function looksLikeNonDownloadUrl(url: string): boolean {
  return NON_DOWNLOAD_URL_RE.some((re) => re.test(url));
}

function isSuccessfulDownloadStatus(statusCode: number | undefined): boolean {
  // 206 Partial Content is common for resumable downloads / range requests.
  if (statusCode == null) return true;
  return statusCode === 200 || statusCode === 206;
}

/**
 * Decide if a Firefox response looks like a real download worth intercepting.
 */
export function firefoxWebRequestDownloadCandidate(
  details: FirefoxHeadersReceivedDetails,
  settings: ExtensionIntegrationSettings,
): FirefoxCaptureCandidate | null {
  if (!settings.enabled || settings.downloadHandoffMode === 'off') {
    return null;
  }

  const url = details.url;
  if (!url.startsWith('http://') && !url.startsWith('https://')) {
    return null;
  }
  if (isUrlHostExcludedByPatterns(url, settings.excludedHosts)) {
    return null;
  }
  if (
    shouldSkipAppOwnedDownloadOrigin({
      url,
      pageUrl: details.documentUrl,
      referrer: details.originUrl,
    })
  ) {
    return null;
  }
  if (looksLikeNonDownloadUrl(url)) {
    return null;
  }
  if (!isSuccessfulDownloadStatus(details.statusCode)) {
    return null;
  }

  // POST/PUT/HEAD cannot be replayed as a desktop GET (wait forms / size probes).
  const method = (details.method ?? 'GET').toUpperCase();
  if (method !== 'GET') {
    return null;
  }

  // Navigation / plugin / opaque "other", plus large XHR file-host CDNs.
  // Tiny XHR (suggestqueries, stats) is rejected later by size + MIME.
  const resourceType = (details.type ?? '').toLowerCase();
  if (
    [
      'stylesheet',
      'script',
      'image',
      'font',
      'media',
      'websocket',
      'ping',
      'csp_report',
      'sub_frame',
      'imageset',
      'web_manifest',
      'speculative',
    ].includes(resourceType)
  ) {
    return null;
  }

  const headers = details.responseHeaders;
  const disposition = contentDisposition(headers);
  const mime = contentType(headers);
  const totalBytes = contentLength(headers);
  const dispositionName = filenameFromContentDisposition(disposition);
  const urlName = basenameFromUrl(url);
  // Prefer server-provided disposition name; URL basename is a weaker signal.
  const filename = dispositionName ?? urlName;
  const ext = extensionOf(filename);
  const captured = new Set(settings.capturedFileExtensions.map((e) => e.toLowerCase()));
  const ignored = new Set(
    (settings.ignoredFileExtensions ?? []).map((e) => e.toLowerCase()),
  );
  if (ext && ignored.has(ext)) {
    return null;
  }

  const hasAttachment = /\battachment\b/i.test(disposition);
  // Weak data extensions (csv/json/…) must never count as a "strong" filename.
  const strongExt = Boolean(ext && captured.has(ext) && !isWeakCaptureExtension(ext));
  const strongMime = DOWNLOAD_MIME.has(mime);
  // Present but non-captured extension is a veto for MIME-only capture (e.g. f.txt + octet-stream).
  const knownNonCapturedExt = Boolean(ext && !captured.has(ext));
  const isMainFrame = resourceType === 'main_frame';
  const isXhr = resourceType === 'xmlhttprequest';
  const isObject = resourceType === 'object';
  const navType = isMainFrame || isObject || resourceType === 'other';

  // CORS without attachment is usually a page data fetch (Nexus stats CSVs).
  // File-host CDNs also send ACAO on huge octet-stream bodies — those are real.
  const corsLooksLikePageData =
    hasCorsAllowOrigin(headers) &&
    !hasAttachment &&
    !((strongMime || strongExt) && (totalBytes == null || totalBytes >= MIN_CAPTURE_BYTES));
  if (corsLooksLikePageData) {
    return null;
  }

  // HTML / JSON / text named `Game.rar` is a wait-page stub, not a file.
  if (isNonDownloadMime(mime)) {
    return null;
  }

  // Tiny bodies are wait-page stubs / trackers, even with a .rar filename.
  if (totalBytes != null && totalBytes > 0 && totalBytes < MIN_CAPTURE_BYTES) {
    return null;
  }

  // fetch() + blob on Gofile/Pixeldrain never hits downloads.onCreated with an
  // http(s) URL. Intercept only large, obvious file XHRs so we can hand off
  // the real CDN link instead of a 3 KB HTML ticket.
  if (isXhr) {
    if (totalBytes != null && totalBytes < MIN_XHR_CAPTURE_BYTES) {
      return null;
    }
    if (!strongExt || !(hasAttachment || strongMime)) {
      return null;
    }
    // Chunked CDNs omit Content-Length; only take obvious file XHRs.
    if (totalBytes == null && !(hasAttachment && strongMime)) {
      return null;
    }
  }

  // Chat UIs (Grok, etc.) fetch generated PDFs/Office docs to preview them.
  // type=object is PDF.js / <embed>. Archives and attachment downloads stay.
  if (
    (isXhr || isObject) &&
    isPreviewableDownload(ext, mime) &&
    !hasAttachment
  ) {
    return null;
  }

  let reason: string | null = null;

  // Attachment must pair with a *strong* signal. Bare
  // `Content-Disposition: attachment; filename=f.txt` is not enough, even with octet-stream.
  if (hasAttachment && strongExt) {
    reason = 'attachment_disposition';
  } else if (hasAttachment && strongMime && !knownNonCapturedExt) {
    reason = dispositionName ? 'attachment_disposition' : 'download_mime';
  } else if (strongMime && (navType || isXhr) && !knownNonCapturedExt) {
    // Direct navigation / plugin / opaque / large XHR hit on a binary MIME.
    reason = 'download_mime_navigation';
  } else if (strongExt && isMainFrame && dispositionName) {
    // Top-level navigation with disposition filename matching a captured extension.
    reason = 'strong_filename_navigation';
  } else if (strongExt && isMainFrame && !mime) {
    // Some CDNs omit Content-Type on file links; only trust top-level navigations.
    // type=other is too noisy (stats CSVs, pixels) even when the path ends in .zip/.csv.
    reason = 'strong_filename_navigation';
  }

  if (!reason) {
    return null;
  }

  return {
    url,
    filename: filename?.split(/[\\/]/).pop(),
    totalBytes,
    pageUrl: details.documentUrl || details.originUrl,
    referrer: details.originUrl || details.documentUrl,
    incognito: details.incognito,
    cookieStoreId: details.cookieStoreId,
    reason,
  };
}

/**
 * Object CDNs only. Apex /dl/ hosts (buzzheavier.com, gofile.io) serve HTML
 * wait-pages that can still be named Game.rar — those stay on headersReceived.
 */
export function isFileHostObjectCdnHost(host: string): boolean {
  const name = host.toLowerCase();
  if (name === 'gofile.io' || name === 'www.gofile.io') return false;
  if (name === 'buzzheavier.com' || name === 'www.buzzheavier.com') return false;
  if (name.endsWith('.gofile.io')) return true;
  if (name === 'pixeldrain.com' || name.endsWith('.pixeldrain.com')) return true;
  if (name === 'cdn.buzzheavier.com' || name.endsWith('.cdn.buzzheavier.com')) return true;
  return false;
}

export type FirefoxBeforeRequestDetails = {
  requestId?: string;
  url: string;
  originUrl?: string;
  documentUrl?: string;
  method?: string;
  type?: string;
  incognito?: boolean;
  cookieStoreId?: string;
};

/**
 * Cancel known file-host CDN object URLs before Firefox classifies them as a
 * download. `{ cancel: true }` on onHeadersReceived is ignored once Firefox
 * has already attached a download (Gofile then shows "Paused — 30 MB of 2.5 GB").
 */
export function firefoxBeforeRequestDownloadCandidate(
  details: FirefoxBeforeRequestDetails,
  settings: ExtensionIntegrationSettings,
): FirefoxCaptureCandidate | null {
  if (!settings.enabled || settings.downloadHandoffMode === 'off') {
    return null;
  }
  const method = (details.method ?? 'GET').toUpperCase();
  if (method !== 'GET') {
    return null;
  }
  const url = details.url;
  if (!url.startsWith('http://') && !url.startsWith('https://')) {
    return null;
  }
  if (isUrlHostExcludedByPatterns(url, settings.excludedHosts)) {
    return null;
  }

  let host: string;
  try {
    host = new URL(url).hostname;
  } catch {
    return null;
  }
  if (!isFileHostObjectCdnHost(host)) {
    return null;
  }

  const filename = basenameFromUrl(url);
  const ext = extensionOf(filename);
  if (!ext || isWeakCaptureExtension(ext)) {
    return null;
  }
  const ignored = new Set(
    (settings.ignoredFileExtensions ?? []).map((value) => value.toLowerCase()),
  );
  if (ignored.has(ext)) {
    return null;
  }
  const captured = new Set(settings.capturedFileExtensions.map((value) => value.toLowerCase()));
  if (!captured.has(ext)) {
    return null;
  }

  return {
    url,
    filename,
    pageUrl: details.documentUrl || details.originUrl,
    referrer: details.originUrl || details.documentUrl,
    incognito: details.incognito,
    cookieStoreId: details.cookieStoreId,
    reason: 'file_host_cdn_url',
  };
}

export type FirefoxStreamFilter = {
  onstart: ((event: unknown) => void) | null;
  ondata: ((event: unknown) => void) | null;
  onstop: ((event: unknown) => void) | null;
  onerror: ((event: unknown) => void) | null;
  close(): void;
};

type BlockingListener = (
  details: FirefoxHeadersReceivedDetails,
) => { cancel?: boolean } | Promise<{ cancel?: boolean }>;

type BlockingWebRequest = {
  onHeadersReceived: {
    addListener(
      listener: BlockingListener,
      filter: { urls: string[]; types?: string[] },
      extraInfoSpec: string[],
    ): void;
  };
  onBeforeRequest?: {
    addListener(
      listener: (
        details: FirefoxBeforeRequestDetails,
      ) => { cancel?: boolean },
      filter: { urls: string[]; types?: string[] },
      extraInfoSpec: string[],
    ): void;
  };
  onBeforeRedirect?: {
    addListener(
      listener: (details: { url: string; redirectUrl?: string; requestId?: string }) => void,
      filter: { urls: string[]; types?: string[] },
    ): void;
  };
  filterResponseData?: (requestId: string) => FirefoxStreamFilter;
};

/**
 * Close the response stream so Firefox cannot keep reading the file after it
 * has already turned the request into a download (cancel is ignored then).
 */
export function abortFirefoxResponseBody(
  webRequest: Pick<BlockingWebRequest, 'filterResponseData'>,
  requestId: string | undefined,
): boolean {
  if (!requestId || !webRequest.filterResponseData) {
    return false;
  }
  try {
    const filter = webRequest.filterResponseData(requestId);
    const close = () => {
      try {
        filter.close();
      } catch {
        // already closed
      }
    };
    filter.onstart = close;
    filter.ondata = close;
    filter.onstop = close;
    filter.onerror = close;
    close();
    return true;
  } catch {
    return false;
  }
}

export function getFirefoxBlockingWebRequest(): BlockingWebRequest | null {
  const wr = (browser as unknown as { webRequest?: BlockingWebRequest }).webRequest;
  if (!wr?.onHeadersReceived?.addListener) {
    return null;
  }
  // Heuristic: Firefox MV2 has webRequestBlocking permission; Chromium MV3 does not use this path.
  const isFirefox = navigator.userAgent.toLowerCase().includes('firefox');
  return isFirefox ? wr : null;
}
