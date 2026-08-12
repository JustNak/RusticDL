import {
  isErrorResponse,
  isUrlHostExcludedByPatterns,
  toUserFacingMessage,
  type DownloadRequestMetadata,
  type ExtensionIntegrationSettings,
  type HostToExtensionResponse,
} from '@rusticdl/protocol';
import browser from './browser';
import {
  firefoxWebRequestDownloadCandidate,
  getFirefoxBlockingWebRequest,
  MIN_CAPTURE_BYTES,
  type FirefoxCaptureCandidate,
  type FirefoxHeadersReceivedDetails,
} from './firefoxCapture';
import {
  buildContextMenuPayload,
  connectionForErrorCode,
  handoffDownload,
  openApp,
  pingNativeHost,
  saveExtensionSettings,
} from './nativeMessaging';
import {
  getAppearanceSettings,
  getExtensionSettings,
  getPopupState,
  setAppearanceSettings,
  setExtensionSettings,
  setHostError,
  setLastResult,
  updatePopupState,
} from './state';
import type { PopupRequest, PopupStateResponse } from '../shared/messages';

const CONTEXT_MENU_ID = 'download-with-rusticdl';
/** Periodic desktop connection / queue badge refresh (not appearance sync). */
const CONNECTION_HEALTH_ALARM_NAME = 'connection-health';
const CAPTURE_DEDUPE_TTL_MS = 15_000;
const captureClaims = new Map<string, number>();

let cachedSettings: ExtensionIntegrationSettings | null = null;

async function getCachedSettings(): Promise<ExtensionIntegrationSettings> {
  if (!cachedSettings) {
    cachedSettings = await getExtensionSettings();
  }
  return cachedSettings;
}

function rememberSettings(settings: ExtensionIntegrationSettings): ExtensionIntegrationSettings {
  cachedSettings = settings;
  return settings;
}

async function ensureContextMenu() {
  const settings = await getCachedSettings();
  await browser.contextMenus.removeAll();
  if (!settings.contextMenuEnabled) return;

  await browser.contextMenus.create({
    id: CONTEXT_MENU_ID,
    title: 'Download with RusticDL',
    contexts: ['link'],
  });
}

async function refreshConnectionState(): Promise<HostToExtensionResponse> {
  const response = await pingNativeHost();
  if (isErrorResponse(response)) {
    const connection = connectionForErrorCode(response.code);
    const state = await setHostError(
      response.code,
      // Prefer detailed host error (e.g. registration steps) over generic copy.
      response.message || toUserFacingMessage(response.code, response.message),
      connection,
    );
    await updateBrowserBadge(state);
    return response;
  }

  const state = await setLastResult('connected', response);
  if (state.extensionSettings) {
    rememberSettings(state.extensionSettings);
  }
  await ensureContextMenu();
  await updateBrowserBadge(state);
  return response;
}

async function updateBrowserBadge(state: PopupStateResponse) {
  const settings = state.extensionSettings ?? (await getCachedSettings());
  const action = browser.action ?? browser.browserAction;
  if (!action?.setBadgeText) return;

  if (!settings.showBadgeStatus) {
    await action.setBadgeText({ text: '' });
    return;
  }

  if (state.connection !== 'connected') {
    await action.setBadgeText({ text: '!' });
    await action.setBadgeBackgroundColor?.({ color: '#b45309' });
    return;
  }

  const active = state.queueSummary?.active ?? 0;
  await action.setBadgeText({ text: active > 0 ? String(Math.min(active, 99)) : '' });
  await action.setBadgeBackgroundColor?.({ color: '#2563eb' });
}

function claimCapture(url: string): boolean {
  const now = Date.now();
  for (const [key, ts] of captureClaims) {
    if (now - ts > CAPTURE_DEDUPE_TTL_MS) captureClaims.delete(key);
  }
  if (captureClaims.has(url)) return false;
  captureClaims.set(url, now);
  return true;
}

function releaseCapture(url: string): void {
  captureClaims.delete(url);
}

/** True when desktop accepted the handoff (queued or already tracked). */
function isSuccessfulHandoff(response: HostToExtensionResponse): boolean {
  if (isErrorResponse(response) || !response.ok) return false;
  // Protocol success type is `accepted`; `dismissed` must not cancel the browser download.
  if (response.type !== 'accepted') return false;
  const status = response.payload.status;
  return status === 'queued' || status === 'duplicate_existing_job';
}

function filenameExtension(filename: string | undefined): string | undefined {
  if (!filename) return undefined;
  const base = filename.split(/[\\/]/).pop() ?? filename;
  const dot = base.lastIndexOf('.');
  if (dot < 0 || dot === base.length - 1) return undefined;
  const ext = base.slice(dot + 1).toLowerCase();
  if (!/^[a-z0-9]{1,10}$/.test(ext)) return undefined;
  return ext;
}

function filenameLooksCaptured(filename: string | undefined, extensions: string[]): boolean {
  const ext = filenameExtension(filename);
  return Boolean(ext && extensions.includes(ext));
}

/** MIME types that mean "this is a real file the browser is saving". */
const DOWNLOAD_ITEM_MIME_HINTS = [
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

/** Firefox DownloadItem + optional Chromium fields used when capturing. */
type CapturedDownloadItem = browser.downloads.DownloadItem & {
  finalUrl?: string;
  byExtensionId?: string;
};

/**
 * downloads.onCreated filter (Firefox fallback + Chromium primary).
 *
 * Never capture just because a filename exists — Firefox always supplies one.
 * Require a captured extension and/or a download MIME, and skip tiny junk bodies.
 */
function shouldCaptureDownload(
  item: CapturedDownloadItem,
  settings: ExtensionIntegrationSettings,
): boolean {
  if (!settings.enabled || settings.downloadHandoffMode === 'off') return false;
  const url = item.finalUrl || item.url;
  if (!url || !(url.startsWith('http://') || url.startsWith('https://'))) return false;
  if (isUrlHostExcludedByPatterns(url, settings.excludedHosts)) return false;
  if (item.byExtensionId) return false;

  // Skip non-http schemes already handled; also skip blob:/data: noise if present.
  if (url.startsWith('blob:') || url.startsWith('data:')) return false;

  const mime = (item.mime || '').toLowerCase();
  if (
    mime.startsWith('text/html') ||
    mime === 'application/xhtml+xml' ||
    mime === 'application/json' ||
    mime.startsWith('image/') ||
    mime.startsWith('audio/') ||
    mime.startsWith('video/') ||
    mime.startsWith('font/')
  ) {
    return false;
  }

  const ext = filenameExtension(item.filename);
  const ignored = new Set(
    (settings.ignoredFileExtensions ?? []).map((e) => e.toLowerCase()),
  );
  if (ext && ignored.has(ext)) return false;

  const strongName = filenameLooksCaptured(item.filename, settings.capturedFileExtensions);
  const dispositionHint = DOWNLOAD_ITEM_MIME_HINTS.some((hint) => mime.includes(hint));

  // Known-size micro responses are never real archives/installers.
  const knownBytes = item.totalBytes && item.totalBytes > 0 ? item.totalBytes : undefined;
  if (knownBytes != null && knownBytes < MIN_CAPTURE_BYTES && !strongName) {
    return false;
  }

  // Require a strong signal — do NOT fall back to "any filename".
  return strongName || dispositionHint;
}

async function handOffUrl(
  url: string,
  settings: ExtensionIntegrationSettings,
  metadata: DownloadRequestMetadata,
  extra: {
    pageUrl?: string;
    referrer?: string;
    incognito?: boolean;
  } = {},
): Promise<boolean> {
  if (!claimCapture(url)) return false;

  const source = {
    entryPoint: 'browser_download' as const,
    extensionVersion: browser.runtime.getManifest().version,
    pageUrl: extra.pageUrl,
    referrer: extra.referrer,
    incognito: extra.incognito,
  };

  // Automatic capture always uses ask/auto from settings.
  const response = await handoffDownload(url, source, settings.downloadHandoffMode, metadata);

  if (isErrorResponse(response) || !isSuccessfulHandoff(response)) {
    // Allow a later retry if the desktop rejected or timed out the handoff.
    releaseCapture(url);
    if (isErrorResponse(response)) {
      const connection = connectionForErrorCode(response.code);
      const state = await setHostError(
        response.code,
        // Prefer the detailed native-messaging registration message when present.
        response.message || toUserFacingMessage(response.code, response.message),
        connection,
      );
      await updateBrowserBadge(state);
    }
    return false;
  }

  const state = await setLastResult('connected', response);
  await updateBrowserBadge(state);
  return true;
}

async function handoffBrowserDownload(
  item: CapturedDownloadItem,
  settings: ExtensionIntegrationSettings,
) {
  const url = item.finalUrl || item.url;
  if (!url) return;

  // Hand off first. Only cancel/erase the browser download after RusticDL accepts it
  // so a down desktop / dismissed ask prompt does not lose the file.
  const ok = await handOffUrl(url, settings, {
    suggestedFilename: item.filename?.split(/[\\/]/).pop(),
    totalBytes: item.totalBytes && item.totalBytes > 0 ? item.totalBytes : undefined,
  }, {
    pageUrl: item.referrer,
    referrer: item.referrer,
    incognito: item.incognito,
  });
  if (!ok) return;

  try {
    await browser.downloads.cancel(item.id);
  } catch {
    // may already be canceled
  }
  try {
    await browser.downloads.erase({ id: item.id });
  } catch {
    // optional cleanup
  }
}

async function onDownloadCreated(item: CapturedDownloadItem) {
  const settings = await getCachedSettings();
  if (!shouldCaptureDownload(item, settings)) return;
  await handoffBrowserDownload(item, settings);
}

async function handoffFirefoxCandidate(
  candidate: FirefoxCaptureCandidate,
  settings: ExtensionIntegrationSettings,
) {
  await handOffUrl(candidate.url, settings, {
    suggestedFilename: candidate.filename,
    totalBytes: candidate.totalBytes,
  }, {
    pageUrl: candidate.pageUrl,
    referrer: candidate.referrer,
    incognito: candidate.incognito,
  });
}

function registerFirefoxWebRequestInterception() {
  const webRequest = getFirefoxBlockingWebRequest();
  if (!webRequest) return;

  webRequest.onHeadersReceived.addListener(
    (details: FirefoxHeadersReceivedDetails) => {
      // Listener must return cancel synchronously when possible; we use a promise-safe path.
      return handleFirefoxHeadersReceived(details);
    },
    {
      urls: ['http://*/*', 'https://*/*'],
      // Intentionally omit xmlhttprequest + sub_frame: those are full of false positives
      // (YouTube suggestqueries, beacons, ads). Real file navigations use main_frame /
      // object / other; everything else still hits downloads.onCreated when Firefox
      // treats the response as a download.
      types: ['main_frame', 'object', 'other'],
    },
    ['blocking', 'responseHeaders'],
  );
}

async function handleFirefoxHeadersReceived(
  details: FirefoxHeadersReceivedDetails,
): Promise<{ cancel?: boolean }> {
  try {
    const settings = await getCachedSettings();
    const candidate = firefoxWebRequestDownloadCandidate(details, settings);
    if (!candidate) {
      return {};
    }
    if (settings.downloadCaptureDebugLogging) {
      console.info('[RusticDL] capture candidate', {
        reason: candidate.reason,
        url: candidate.url,
        filename: candidate.filename,
        totalBytes: candidate.totalBytes,
        type: details.type,
      });
    }
    // Blocking listeners must decide cancel synchronously-ish; we still only cancel
    // after claiming, and handOffUrl releases the claim if the desktop rejects.
    void handoffFirefoxCandidate(candidate, settings);
    return { cancel: true };
  } catch {
    return {};
  }
}

browser.runtime.onInstalled.addListener(() => {
  void ensureContextMenu();
  void refreshConnectionState();
  void browser.alarms.create(CONNECTION_HEALTH_ALARM_NAME, { periodInMinutes: 1 });
  // Drop the legacy appearance-sync alarm from older builds.
  void browser.alarms.clear('appearance-sync');
});

browser.runtime.onStartup.addListener(() => {
  void ensureContextMenu();
  void refreshConnectionState();
});

browser.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === CONNECTION_HEALTH_ALARM_NAME || alarm.name === 'appearance-sync') {
    void refreshConnectionState();
  }
});

browser.contextMenus.onClicked.addListener((info, tab) => {
  void (async () => {
    const settings = await getCachedSettings();
    if (!settings.contextMenuEnabled) return;
    const payload = buildContextMenuPayload(info, tab);
    if (!payload) {
      await setHostError('INVALID_URL', 'The selected link did not include a URL.', 'error');
      return;
    }
    await updatePopupState({ isSubmitting: true });
    const mode = settings.downloadHandoffMode === 'auto' ? 'auto' : 'ask';
    const response = await handoffDownload(payload.url, payload.source, mode);
    if (isErrorResponse(response)) {
      const connection = connectionForErrorCode(response.code);
      const state = await setHostError(
        response.code,
        response.message || toUserFacingMessage(response.code, response.message),
        connection,
      );
      await updateBrowserBadge(state);
      return;
    }
    const state = await setLastResult('connected', response);
    await updateBrowserBadge(state);
  })();
});

// Firefox primary: blocking webRequest. downloads.onCreated is fallback.
registerFirefoxWebRequestInterception();
if (browser.downloads?.onCreated) {
  browser.downloads.onCreated.addListener((item) => {
    void onDownloadCreated(item);
  });
}

browser.runtime.onMessage.addListener((message: PopupRequest) => {
  return handlePopupMessage(message);
});

async function handlePopupMessage(message: PopupRequest): Promise<PopupStateResponse | void> {
  switch (message.type) {
    case 'popup_get_state':
      return getPopupState();
    case 'popup_ping': {
      await refreshConnectionState();
      return getPopupState();
    }
    case 'popup_open_app': {
      const response = await openApp();
      if (isErrorResponse(response)) {
        await setHostError(
          response.code,
          response.message || toUserFacingMessage(response.code, response.message),
          connectionForErrorCode(response.code),
        );
      } else {
        await setLastResult('connected', response);
      }
      return getPopupState();
    }
    case 'popup_open_options': {
      await browser.runtime.openOptionsPage();
      return getPopupState();
    }
    case 'extension_settings_update': {
      const settings = rememberSettings(await setExtensionSettings(message.settings));
      const response = await saveExtensionSettings(settings);
      if (!isErrorResponse(response) && response.ok && response.type === 'pong') {
        await setLastResult('connected', response);
      }
      await ensureContextMenu();
      return getPopupState();
    }
    case 'appearance_settings_get': {
      const appearanceSettings = await getAppearanceSettings();
      return updatePopupState({ appearanceSettings });
    }
    case 'appearance_settings_update': {
      await setAppearanceSettings(message.settings);
      return getPopupState();
    }
    default:
      return getPopupState();
  }
}

void ensureContextMenu();
void refreshConnectionState();
