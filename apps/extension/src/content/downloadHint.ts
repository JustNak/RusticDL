/**
 * Chromium MV3 service workers often never see downloads.onDeterminingFilename
 * (Playwright/CDP download interception, asleep workers). The HTML `download`
 * attribute is the user-facing name for those clicks — observe it here and
 * send it to the background before onCreated runs.
 */
import { HTML_DOWNLOAD_HINT_MESSAGE_TYPE } from '../shared/htmlDownloadHint';

function isAnchor(node: EventTarget | null | undefined): node is HTMLAnchorElement {
  return Boolean(node) && (node as HTMLElement).tagName === 'A';
}

function hintFromAnchor(anchor: HTMLAnchorElement): { url: string; filename: string } | undefined {
  const raw = anchor.getAttribute('download');
  if (raw == null || raw === '') return undefined;
  const filename = raw.split(/[\\/]/).pop()?.trim();
  const url = anchor.href;
  if (!filename || !url) return undefined;
  if (!url.startsWith('http://') && !url.startsWith('https://')) return undefined;
  return { url, filename };
}

function sendHint(url: string, filename: string): void {
  const chromeRuntime = (globalThis as {
    chrome?: {
      runtime?: {
        lastError?: { message?: string };
        sendMessage?: (message: unknown, callback?: () => void) => void;
      };
    };
  }).chrome?.runtime;
  try {
    chromeRuntime?.sendMessage?.(
      {
        type: HTML_DOWNLOAD_HINT_MESSAGE_TYPE,
        url,
        filename,
      },
      () => {
        void chromeRuntime.lastError;
      },
    );
  } catch {
    // extension context invalidated on reload
  }
}

document.addEventListener(
  'click',
  (event) => {
    const path = typeof event.composedPath === 'function' ? event.composedPath() : [];
    const candidates = path.length > 0 ? path : [event.target];
    for (const node of candidates) {
      if (!isAnchor(node)) continue;
      const hint = hintFromAnchor(node);
      if (!hint) continue;
      sendHint(hint.url, hint.filename);
      return;
    }
    const target = event.target;
    if (!(target instanceof Element)) return;
    const closest = target.closest('a');
    if (!isAnchor(closest)) return;
    const hint = hintFromAnchor(closest);
    if (hint) sendHint(hint.url, hint.filename);
  },
  true,
);
