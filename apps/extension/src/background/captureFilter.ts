/**
 * Shared download-capture heuristics (Firefox webRequest + downloads.onCreated).
 *
 * Bias: miss an exotic file rather than steal page data fetches. Sites routinely
 * serve `.csv` / `.json` as live APIs (Nexus Mods download-count stats, etc.).
 */
import {
  isUrlHostExcludedByPatterns,
  type ExtensionIntegrationSettings,
} from '@rusticdl/protocol';

/** Skip known-size responses smaller than this unless they look like a real file. */
export const MIN_CAPTURE_BYTES = 1024;

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
  referrer?: string;
  byExtensionId?: string;
  incognito?: boolean;
};

export function filenameExtension(filename: string | undefined): string | undefined {
  if (!filename) return undefined;
  const base = filename.split(/[\\/]/).pop() ?? filename;
  const dot = base.lastIndexOf('.');
  if (dot < 0 || dot === base.length - 1) return undefined;
  const ext = base.slice(dot + 1).toLowerCase();
  if (!/^[a-z0-9]{1,10}$/.test(ext)) return undefined;
  return ext;
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

  const knownBytes = item.totalBytes && item.totalBytes > 0 ? item.totalBytes : undefined;
  if (knownBytes != null && knownBytes < MIN_CAPTURE_BYTES && !strongName) {
    return false;
  }

  if (isWeakCaptureExtension(ext)) {
    return Boolean(ext && captured.has(ext) && dispositionHint);
  }

  if (strongName) return true;
  if (ext) return false;
  return dispositionHint;
}
