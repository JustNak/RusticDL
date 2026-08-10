/**
 * Firefox primary download capture via blocking webRequest.
 * Cancels eligible responses before the browser commits the file, then hands off.
 */
import {
  isUrlHostExcludedByPatterns,
  type ExtensionIntegrationSettings,
} from '@rusticdl/protocol';
import browser from './browser';

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
};

export type FirefoxCaptureCandidate = {
  url: string;
  filename?: string;
  totalBytes?: number;
  pageUrl?: string;
  referrer?: string;
  incognito?: boolean;
  reason: string;
};

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

function filenameFromContentDisposition(value: string): string | undefined {
  const star = value.match(/filename\*\s*=\s*(?:UTF-8''|utf-8'')?([^;]+)/i);
  if (star?.[1]) {
    try {
      return decodeURIComponent(star[1].trim().replace(/^"|"$/g, ''));
    } catch {
      // fall through
    }
  }
  const plain = value.match(/filename\s*=\s*("?)([^";]+)\1/i);
  return plain?.[2]?.trim() || undefined;
}

function basenameFromUrl(url: string): string | undefined {
  try {
    const path = new URL(url).pathname;
    const base = path.split('/').filter(Boolean).pop();
    if (!base || !base.includes('.')) return undefined;
    return decodeURIComponent(base);
  } catch {
    return undefined;
  }
}

function extensionOf(name: string | undefined): string | undefined {
  if (!name) return undefined;
  const base = name.split(/[\\/]/).pop() ?? name;
  const dot = base.lastIndexOf('.');
  if (dot < 0) return undefined;
  return base.slice(dot + 1).toLowerCase();
}

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
]);

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

  // Skip typical page asset types.
  const resourceType = (details.type ?? '').toLowerCase();
  if (
    ['stylesheet', 'script', 'image', 'font', 'media', 'websocket', 'ping', 'csp_report'].includes(
      resourceType,
    )
  ) {
    return null;
  }

  const headers = details.responseHeaders;
  const disposition = contentDisposition(headers);
  const mime = contentType(headers);
  const filename =
    filenameFromContentDisposition(disposition) ?? basenameFromUrl(url);
  const ext = extensionOf(filename);
  const captured = new Set(settings.capturedFileExtensions.map((e) => e.toLowerCase()));
  const hasAttachment = /\battachment\b/i.test(disposition);
  const strongExt = Boolean(ext && captured.has(ext));
  const strongMime = DOWNLOAD_MIME.has(mime) && !mime.startsWith('text/html');

  if (mime.startsWith('text/html') || mime === 'application/xhtml+xml' || mime === 'application/json') {
    // HTML/JSON pages are not downloads unless disposition is attachment with a file name.
    if (!(hasAttachment && strongExt)) {
      return null;
    }
  }

  let reason: string | null = null;
  if (hasAttachment && (strongExt || strongMime || filename)) {
    reason = 'attachment_disposition';
  } else if (strongExt) {
    reason = 'strong_filename';
  } else if (strongMime && hasAttachment) {
    reason = 'download_mime';
  } else if (strongMime && (resourceType === 'main_frame' || resourceType === 'other')) {
    // Direct navigation to a binary MIME (e.g. clicking a .zip link).
    reason = 'download_mime_navigation';
  }

  if (!reason) {
    return null;
  }

  return {
    url,
    filename: filename?.split(/[\\/]/).pop(),
    totalBytes: contentLength(headers),
    pageUrl: details.documentUrl || details.originUrl,
    referrer: details.originUrl || details.documentUrl,
    incognito: details.incognito,
    reason,
  };
}

type BlockingWebRequest = {
  onHeadersReceived: {
    addListener(
      listener: (details: FirefoxHeadersReceivedDetails) => { cancel?: boolean } | Promise<{ cancel?: boolean }>,
      filter: { urls: string[]; types?: string[] },
      extraInfoSpec: string[],
    ): void;
  };
};

export function getFirefoxBlockingWebRequest(): BlockingWebRequest | null {
  const wr = (browser as unknown as { webRequest?: BlockingWebRequest }).webRequest;
  if (!wr?.onHeadersReceived?.addListener) {
    return null;
  }
  // Heuristic: Firefox MV2 has webRequestBlocking permission; Chromium MV3 does not use this path.
  const isFirefox = navigator.userAgent.toLowerCase().includes('firefox');
  return isFirefox ? wr : null;
}
