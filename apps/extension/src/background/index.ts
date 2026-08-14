import {
  isErrorResponse,
  toUserFacingMessage,
  type DownloadRequestMetadata,
  type ExtensionIntegrationSettings,
  type HostToExtensionResponse,
} from '@rusticdl/protocol';
import browser from './browser';
import {
  downloadCreatedAction,
  knownDownloadBytes,
  matchesInterceptedDownload,
  normalizeCaptureUrl,
  shouldCaptureDownloadItem,
  urlIsClaimed,
  type InterceptedDownload,
} from './captureFilter';
import {
  firefoxWebRequestDownloadCandidate,
  getFirefoxBlockingWebRequest,
  type FirefoxCaptureCandidate,
  type FirefoxHeadersReceivedDetails,
} from './firefoxCapture';
import {
  buildContextMenuPayload,
  collectHandoffAuth,
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
/** Wait for Firefox to fill in totalBytes before capturing unknown-size items. */
const SIZE_WAIT_MS = 1_500;
const captureClaims = new Map<string, number>();
/** URLs/filenames canceled via blocking webRequest — erase leftover download items. */
const interceptedDownloads = new Map<string, InterceptedDownload>();

type PendingSizeWait = {
  item: CapturedDownloadItem;
  timer: ReturnType<typeof setTimeout>;
};
const pendingSizeWaits = new Map<number, PendingSizeWait>();

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

function pruneStaleClaims(now = Date.now()): void {
  for (const [key, ts] of captureClaims) {
    if (now - ts > CAPTURE_DEDUPE_TTL_MS) captureClaims.delete(key);
  }
  for (const [key, entry] of interceptedDownloads) {
    if (now - entry.ts > CAPTURE_DEDUPE_TTL_MS) interceptedDownloads.delete(key);
  }
}

function claimCapture(url: string): boolean {
  const key = normalizeCaptureUrl(url);
  const now = Date.now();
  pruneStaleClaims(now);
  if (captureClaims.has(key)) return false;
  captureClaims.set(key, now);
  return true;
}

function releaseCapture(url: string): void {
  captureClaims.delete(normalizeCaptureUrl(url));
}

function rememberInterceptedDownload(url: string, filename?: string): void {
  const key = normalizeCaptureUrl(url);
  const now = Date.now();
  pruneStaleClaims(now);
  interceptedDownloads.set(key, { url: key, filename, ts: now });
}

function isClaimedItem(item: CapturedDownloadItem): boolean {
  return (
    urlIsClaimed(item.finalUrl || item.url, captureClaims.keys())
    || urlIsClaimed(item.url, captureClaims.keys())
  );
}

function shouldEraseBrowserGhost(item: CapturedDownloadItem): boolean {
  pruneStaleClaims();
  if (isClaimedItem(item)) return true;
  if (!matchesInterceptedDownload(item, interceptedDownloads.values(), Date.now(), CAPTURE_DEDUPE_TTL_MS)) {
    return false;
  }
  // webRequest ghosts are interrupted/complete. An in_progress item with no
  // active claim is a later retry after a failed handoff — let it fall through.
  return item.state !== 'in_progress';
}

async function pauseBrowserDownload(id: number): Promise<void> {
  try {
    await browser.downloads.pause(id);
  } catch {
    // already finished, interrupted, or pause is unavailable
  }
}

async function resumeBrowserDownload(id: number): Promise<void> {
  try {
    await browser.downloads.resume(id);
  } catch {
    // user canceled, already complete, or resume is unavailable
  }
}

/** Cancel and drop the Firefox/Chrome shelf item so a handed-off file does not linger as failed. */
async function eraseBrowserDownload(id: number): Promise<void> {
  try {
    await browser.downloads.cancel(id);
  } catch {
    // may already be canceled / interrupted by webRequest
  }
  try {
    await browser.downloads.removeFile(id);
  } catch {
    // no dest file, or already gone — the usual "File moved or missing" case
  }
  for (let attempt = 0; attempt < 4; attempt += 1) {
    try {
      const erased = await browser.downloads.erase({ id });
      if (erased.length > 0) return;
    } catch {
      // Firefox sometimes rejects erase until state settles after cancel
    }
    await new Promise((resolve) => setTimeout(resolve, 40 * (attempt + 1)));
  }
}

/** True when desktop accepted the handoff (queued or already tracked). */
function isSuccessfulHandoff(response: HostToExtensionResponse): boolean {
  if (isErrorResponse(response) || !response.ok) return false;
  // Protocol success type is `accepted`; `dismissed` must not cancel the browser download.
  if (response.type !== 'accepted') return false;
  const status = response.payload.status;
  return status === 'queued' || status === 'duplicate_existing_job';
}

/** Firefox DownloadItem + optional Chromium fields used when capturing. */
type CapturedDownloadItem = browser.downloads.DownloadItem & {
  finalUrl?: string;
  byExtensionId?: string;
  cookieStoreId?: string;
  fileSize?: number;
  bytesReceived?: number;
};

type HandoffAttempt = 'accepted' | 'already_claimed' | 'rejected';

async function handOffUrl(
  url: string,
  settings: ExtensionIntegrationSettings,
  metadata: DownloadRequestMetadata,
  extra: {
    pageUrl?: string;
    referrer?: string;
    incognito?: boolean;
    cookieStoreId?: string;
  } = {},
): Promise<HandoffAttempt> {
  if (!claimCapture(url)) return 'already_claimed';

  const source = {
    entryPoint: 'browser_download' as const,
    extensionVersion: browser.runtime.getManifest().version,
    pageUrl: extra.pageUrl,
    referrer: extra.referrer,
    incognito: extra.incognito,
  };

  const handoffAuth = await collectHandoffAuth(url, {
    referrer: extra.referrer,
    pageUrl: extra.pageUrl,
    incognito: extra.incognito,
    cookieStoreId: extra.cookieStoreId,
  });
  const response = await handoffDownload(url, source, settings.downloadHandoffMode, {
    ...metadata,
    handoffAuth: metadata.handoffAuth ?? handoffAuth,
  });

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
    return 'rejected';
  }

  const state = await setLastResult('connected', response);
  await updateBrowserBadge(state);
  return 'accepted';
}

async function handoffBrowserDownload(
  item: CapturedDownloadItem,
  settings: ExtensionIntegrationSettings,
) {
  const url = item.finalUrl || item.url;
  if (!url) return;

  // A webRequest cancel already claimed this URL. The leftover downloads API
  // item is a ghost ("File moved or missing") — drop it, do not re-handoff.
  if (shouldEraseBrowserGhost(item)) {
    await eraseBrowserDownload(item.id);
    return;
  }

  // Pause before native messaging / the desktop ask prompt so Firefox does not
  // keep writing the file. Resume only if the desktop rejects or dismisses.
  await pauseBrowserDownload(item.id);

  const attempt = await handOffUrl(url, settings, {
    suggestedFilename: item.filename?.split(/[\\/]/).pop(),
    totalBytes: knownDownloadBytes(item),
  }, {
    pageUrl: item.referrer,
    referrer: item.referrer,
    incognito: item.incognito,
    cookieStoreId: item.cookieStoreId,
  });

  if (attempt === 'rejected') {
    await resumeBrowserDownload(item.id);
    return;
  }

  await eraseBrowserDownload(item.id);
}

function deltaCurrent<T>(field: { current?: T } | T | undefined): T | undefined {
  if (field == null) return undefined;
  if (typeof field === 'object' && 'current' in (field as object)) {
    return (field as { current?: T }).current;
  }
  return field as T;
}

type DownloadChangeDelta = {
  id: number;
  url?: { current?: string };
  filename?: { current?: string };
  mime?: { current?: string };
  state?: { current?: string };
  totalBytes?: { current?: number };
  fileSize?: { current?: number };
};

function mergeDownloadDelta(
  item: CapturedDownloadItem,
  delta: DownloadChangeDelta,
): CapturedDownloadItem {
  const totalBytes = deltaCurrent(delta.totalBytes);
  const fileSize = deltaCurrent(delta.fileSize);
  const filename = deltaCurrent(delta.filename);
  const mime = deltaCurrent(delta.mime);
  const url = deltaCurrent(delta.url);
  const next: CapturedDownloadItem = {
    ...item,
    ...(url ? { url } : {}),
    ...(filename ? { filename } : {}),
    ...(mime ? { mime } : {}),
    ...(typeof fileSize === 'number' && fileSize > 0 ? { fileSize } : {}),
    ...(typeof totalBytes === 'number' && totalBytes > 0 ? { totalBytes } : {}),
  };
  const known = knownDownloadBytes(next);
  if (known != null) next.totalBytes = known;
  return next;
}

function clearPendingSizeWait(id: number): PendingSizeWait | undefined {
  const pending = pendingSizeWaits.get(id);
  if (!pending) return undefined;
  clearTimeout(pending.timer);
  pendingSizeWaits.delete(id);
  return pending;
}

async function latestDownloadSnapshot(item: CapturedDownloadItem): Promise<CapturedDownloadItem> {
  try {
    const found = await browser.downloads.search({ id: item.id });
    const latest = found[0] as CapturedDownloadItem | undefined;
    if (!latest) return item;
    const merged: CapturedDownloadItem = {
      ...item,
      ...latest,
      cookieStoreId: latest.cookieStoreId ?? item.cookieStoreId,
      fileSize: latest.fileSize && latest.fileSize > 0 ? latest.fileSize : item.fileSize,
    };
    const known = knownDownloadBytes(merged)
      ?? (latest.state === 'complete' && latest.bytesReceived && latest.bytesReceived > 0
        ? latest.bytesReceived
        : undefined);
    if (known != null) merged.totalBytes = known;
    return merged;
  } catch {
    return item;
  }
}

async function finalizePendingCapture(item: CapturedDownloadItem) {
  if (shouldEraseBrowserGhost(item)) {
    await eraseBrowserDownload(item.id);
    return;
  }
  const settings = await getCachedSettings();
  if (!shouldCaptureDownloadItem(item, settings)) return;
  await handoffBrowserDownload(item, settings);
}

async function onDownloadCreated(item: CapturedDownloadItem) {
  // webRequest already canceled this transfer; the downloads API still emits a
  // failed shelf item. Drop it before any size-wait / second handoff.
  if (shouldEraseBrowserGhost(item)) {
    await eraseBrowserDownload(item.id);
    return;
  }

  const settings = await getCachedSettings();
  if (shouldEraseBrowserGhost(item)) {
    await eraseBrowserDownload(item.id);
    return;
  }

  const action = downloadCreatedAction(item, settings);
  if (action === 'capture') {
    await handoffBrowserDownload(item, settings);
    return;
  }
  if (action !== 'wait') return;

  const timer = setTimeout(() => {
    const pending = pendingSizeWaits.get(item.id);
    if (!pending) return;
    pendingSizeWaits.delete(item.id);
    void latestDownloadSnapshot(pending.item).then((latest) => finalizePendingCapture(latest));
  }, SIZE_WAIT_MS);
  pendingSizeWaits.set(item.id, { item, timer });
}

function onDownloadChanged(delta: DownloadChangeDelta) {
  const pending = pendingSizeWaits.get(delta.id);
  if (!pending) return;

  const merged = mergeDownloadDelta(pending.item, delta);
  pending.item = merged;

  if (shouldEraseBrowserGhost(merged)) {
    clearPendingSizeWait(delta.id);
    void eraseBrowserDownload(delta.id);
    return;
  }

  const known = knownDownloadBytes(merged);
  const state = deltaCurrent(delta.state);
  if (known == null && state !== 'complete') return;

  clearPendingSizeWait(delta.id);
  if (known == null && state === 'complete') {
    void latestDownloadSnapshot(merged).then((latest) => finalizePendingCapture(latest));
    return;
  }
  void finalizePendingCapture(merged);
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
    cookieStoreId: candidate.cookieStoreId,
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
      // xmlhttprequest is included so file-host CDNs (Gofile/Pixeldrain fetch)
      // can be handed off before they become an uncatchable blob: download.
      // Heuristics still reject tiny/XHR-API noise (see firefoxCapture).
      types: ['main_frame', 'object', 'other', 'xmlhttprequest'],
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
    // Cancel before Firefox commits a shelf item. Remember the URL so the
    // downloads.onCreated ghost ("File moved or missing") can be erased.
    rememberInterceptedDownload(candidate.url, candidate.filename);
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
    const handoffAuth = await collectHandoffAuth(payload.url, {
      referrer: payload.source.referrer,
      pageUrl: payload.source.pageUrl,
      incognito: payload.source.incognito,
      cookieStoreId: (tab as { cookieStoreId?: string } | undefined)?.cookieStoreId,
    });
    const response = await handoffDownload(payload.url, payload.source, mode, { handoffAuth });
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
if (browser.downloads?.onChanged) {
  browser.downloads.onChanged.addListener((delta) => {
    onDownloadChanged(delta);
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
    default:
      return getPopupState();
  }
}

void ensureContextMenu();
void refreshConnectionState();
