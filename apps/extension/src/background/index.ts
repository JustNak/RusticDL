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
  filenameExtension,
  handoffUrlForCapturedDownload,
  isWeakSuggestedFilename,
  knownDownloadBytes,
  MIN_CAPTURE_BYTES,
  matchesInterceptedDownload,
  normalizeCaptureUrl,
  rememberDownloadRedirect,
  urlIsClaimed,
  type InterceptedDownload,
} from './captureFilter';
import {
  applyDeterminedFilename,
  getChromiumDeterminingFilenameApi,
  getChromiumWebRequest,
  lookupFilenameHint,
  lookupOriginAuth,
  rememberRequestAuth,
  rememberResponseFilenameHint,
  resolveSuggestedFilename,
  type ChromiumBeforeSendHeadersDetails,
  type ChromiumDeterminingFilenameItem,
  type ChromiumHeadersReceivedDetails,
} from './chromiumCapture';
import {
  abortFirefoxResponseBody,
  firefoxBeforeRequestDownloadCandidate,
  firefoxWebRequestDownloadCandidate,
  getFirefoxBlockingWebRequest,
  lookupRedirectSessionUrl,
  type FirefoxBeforeRequestDetails,
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
/** Wait for size (Firefox stubs) and/or a real filename (Chromium CDN tokens). */
const SIZE_WAIT_MS = 1_500;
const captureClaims = new Map<string, number>();
/** URLs/filenames canceled via blocking webRequest — erase leftover download items. */
const interceptedDownloads = new Map<string, InterceptedDownload>();

type PendingSizeWait = {
  item: CapturedDownloadItem;
  timer: ReturnType<typeof setTimeout>;
  waitingForSize: boolean;
  waitingForName: boolean;
};
const pendingSizeWaits = new Map<number, PendingSizeWait>();
const pausedForCapture = new Set<number>();
/** URLs we put back in Firefox after a dismissed/failed handoff — do not recapture. */
const restoreSkipUrls = new Map<string, number>();

let cachedSettings: ExtensionIntegrationSettings | null = null;

/** Blocking listeners stay sync. Null until storage loads so we do not override Off/ignore. */
function settingsForSyncCapture(): ExtensionIntegrationSettings | null {
  return cachedSettings;
}

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

const CONTEXT_MENU_PROPS = {
  id: CONTEXT_MENU_ID,
  title: 'Download with RusticDL',
  contexts: ['link'] as ['link'],
};

type ChromeRuntime = {
  lastError?: { message?: string };
};

type ChromeContextMenus = {
  create: (
    createProperties: typeof CONTEXT_MENU_PROPS,
    callback?: () => void,
  ) => string | number;
};

function chromeContextMenus(): ChromeContextMenus | undefined {
  return (globalThis as { chrome?: { contextMenus?: ChromeContextMenus } }).chrome?.contextMenus;
}

function chromeLastErrorMessage(): string | undefined {
  return (globalThis as { chrome?: { runtime?: ChromeRuntime } }).chrome?.runtime?.lastError?.message;
}

function isDuplicateMenuError(message: string | undefined): boolean {
  return Boolean(message && /duplicate id/i.test(message));
}

/** One rebuild at a time — overlapping create() calls trip Chromium's lastError. */
let contextMenuTask = Promise.resolve();

function ensureContextMenu(): Promise<void> {
  contextMenuTask = contextMenuTask.then(rebuildContextMenu, rebuildContextMenu);
  return contextMenuTask;
}

async function rebuildContextMenu(): Promise<void> {
  const settings = await getCachedSettings();
  if (!settings.contextMenuEnabled) {
    await browser.contextMenus.removeAll();
    return;
  }

  try {
    await browser.contextMenus.update(CONTEXT_MENU_ID, {
      title: CONTEXT_MENU_PROPS.title,
      contexts: CONTEXT_MENU_PROPS.contexts,
    });
    return;
  } catch {
    // Missing item: first install, or Chromium dropped persisted menus.
  }

  try {
    await createContextMenuItem();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (!isDuplicateMenuError(message)) {
      console.warn('[RusticDL] context menu create failed', error);
    }
  }
}

/**
 * webextension-polyfill does not wrap contextMenus.create. Chromium's API is
 * callback-only and logs "Unchecked runtime.lastError" unless lastError is read.
 */
function createContextMenuItem(): Promise<void> {
  const chromeMenus = chromeContextMenus();
  if (chromeMenus) {
    return new Promise((resolve, reject) => {
      chromeMenus.create(CONTEXT_MENU_PROPS, () => {
        const message = chromeLastErrorMessage();
        if (message && !isDuplicateMenuError(message)) {
          reject(new Error(message));
          return;
        }
        resolve();
      });
    });
  }

  return Promise.resolve(
    browser.contextMenus.create(CONTEXT_MENU_PROPS) as Promise<string | number> | string | number,
  ).then(() => undefined);
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

  const previousMenuEnabled = cachedSettings?.contextMenuEnabled;
  const state = await setLastResult('connected', response);
  if (state.extensionSettings) {
    rememberSettings(state.extensionSettings);
  }
  if (state.extensionSettings?.contextMenuEnabled !== previousMenuEnabled) {
    await ensureContextMenu();
  }
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
  for (const [key, ts] of restoreSkipUrls) {
    if (now - ts > CAPTURE_DEDUPE_TTL_MS) restoreSkipUrls.delete(key);
  }
}

function rememberRestoreSkip(url: string): void {
  pruneStaleClaims();
  restoreSkipUrls.set(normalizeCaptureUrl(url), Date.now());
}

function shouldSkipRestoredUrl(url: string | undefined): boolean {
  if (!url) return false;
  pruneStaleClaims();
  return restoreSkipUrls.has(normalizeCaptureUrl(url));
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

function requestPause(id: number): void {
  pausedForCapture.add(id);
  void pauseBrowserDownload(id);
}

async function pauseBrowserDownload(id: number): Promise<void> {
  try {
    await browser.downloads.pause(id);
  } catch {
    // already finished, interrupted, or pause is unavailable
  }
}

async function resumeBrowserDownload(id: number): Promise<void> {
  pausedForCapture.delete(id);
  try {
    await browser.downloads.resume(id);
  } catch {
    // user canceled, already complete, or resume is unavailable
  }
}

/** Cancel and drop the Firefox/Chrome shelf item so a handed-off file does not linger as failed. */
async function eraseBrowserDownload(id: number): Promise<void> {
  pausedForCapture.delete(id);
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

/**
 * Stop the shelf item before any await. Known captures with a real name are
 * canceled immediately so Ask-mode does not sit on "Paused — 30 MB of 2.5 GB".
 * Unknown-size or token-named items are only paused until metadata settles.
 */
function pauseIfLikelyCapture(item: CapturedDownloadItem): void {
  if (item.byExtensionId) return;
  if (shouldEraseBrowserGhost(item)) {
    void eraseBrowserDownload(item.id);
    return;
  }
  const settings = settingsForSyncCapture();
  if (!settings) return;
  const tracked = withHeaderSize(item);
  const action = downloadCreatedAction(tracked, settings);
  if (action === 'ignore') return;
  if (action === 'capture' && !needsFilenameHint(tracked)) {
    void eraseBrowserDownload(item.id);
    return;
  }
  requestPause(item.id);
}

async function restoreBrowserDownload(snapshot: {
  url: string;
  filename?: string;
}): Promise<void> {
  rememberRestoreSkip(snapshot.url);
  try {
    const options: { url: string; filename?: string } = { url: snapshot.url };
    const name = snapshot.filename?.split(/[\\/]/).pop();
    if (name && !/[\\/:]/.test(name)) {
      options.filename = name;
    }
    await browser.downloads.download(options);
  } catch {
    restoreSkipUrls.delete(normalizeCaptureUrl(snapshot.url));
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

function withHeaderSize(item: CapturedDownloadItem): CapturedDownloadItem {
  if (knownDownloadBytes(item) != null) return item;
  const hint = lookupFilenameHint(item.finalUrl || item.url) ?? lookupFilenameHint(item.url);
  if (hint?.totalBytes == null) return item;
  return { ...item, totalBytes: hint.totalBytes };
}

function needsFilenameHint(item: CapturedDownloadItem): boolean {
  const resolved = resolveSuggestedFilename(item);
  return isWeakSuggestedFilename(resolved) || filenameExtension(resolved) == null;
}

function isCaptureSizeStub(item: CapturedDownloadItem): boolean {
  const known = knownDownloadBytes(item);
  return known != null && known < MIN_CAPTURE_BYTES;
}

function ignoredResolvedExtension(
  filename: string | undefined,
  settings: ExtensionIntegrationSettings,
): boolean {
  const ext = filenameExtension(filename);
  if (!ext) return false;
  return (settings.ignoredFileExtensions ?? []).some((value) => value.toLowerCase() === ext);
}

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
    finalUrl?: string;
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
    finalUrl: extra.finalUrl,
    incognito: extra.incognito,
    cookieStoreId: extra.cookieStoreId,
    originAuth: lookupOriginAuth([url, extra.finalUrl, extra.pageUrl, extra.referrer]),
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
  const url = handoffUrlForCapturedDownload(item);
  if (!url) return;

  const suggestedFilename = resolveSuggestedFilename(item);

  // Drop the Firefox item before the desktop ask prompt. Pause still lets the
  // shelf show "30 MB of 2.5 GB" while the user confirms. Restore only if the
  // desktop rejects or the user dismisses.
  await eraseBrowserDownload(item.id);

  const attempt = await handOffUrl(url, settings, {
    suggestedFilename,
    totalBytes: knownDownloadBytes(item) ?? lookupFilenameHint(item.finalUrl || url)?.totalBytes,
  }, {
    pageUrl: item.referrer,
    referrer: item.referrer,
    incognito: item.incognito,
    cookieStoreId: item.cookieStoreId,
    finalUrl: item.finalUrl,
  });

  if (attempt === 'rejected') {
    await restoreBrowserDownload({ url, filename: suggestedFilename });
  }
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
  const sized = withHeaderSize(item);
  if (
    !settings.enabled
    || settings.downloadHandoffMode === 'off'
    || isCaptureSizeStub(sized)
    || ignoredResolvedExtension(resolveSuggestedFilename(sized), settings)
  ) {
    if (pausedForCapture.has(item.id)) {
      await resumeBrowserDownload(item.id);
    }
    return;
  }
  await handoffBrowserDownload(sized, settings);
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

  const tracked = withHeaderSize(item);
  const action = downloadCreatedAction(tracked, settings);
  if (action === 'ignore') {
    if (pausedForCapture.has(tracked.id)) {
      await resumeBrowserDownload(tracked.id);
    }
    return;
  }

  requestPause(tracked.id);

  const waitingForName = needsFilenameHint(tracked);
  if (action === 'capture' && !waitingForName) {
    await handoffBrowserDownload(tracked, settings);
    return;
  }

  const timer = setTimeout(() => {
    const pending = pendingSizeWaits.get(tracked.id);
    if (!pending) return;
    pendingSizeWaits.delete(tracked.id);
    void latestDownloadSnapshot(pending.item).then((latest) => finalizePendingCapture(latest));
  }, SIZE_WAIT_MS);
  pendingSizeWaits.set(tracked.id, {
    item: tracked,
    timer,
    waitingForSize: action === 'wait',
    waitingForName,
  });
}

function stillWaitingForCaptureSignals(
  item: CapturedDownloadItem,
  pending: PendingSizeWait,
): boolean {
  if (pending.waitingForSize && knownDownloadBytes(item) == null) return true;
  return pending.waitingForName && needsFilenameHint(item);
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
  // Size is known but Chrome still only has the URL token — keep waiting for
  // Content-Disposition / onDeterminingFilename until the timer fires.
  if (state !== 'complete' && stillWaitingForCaptureSignals(merged, pending)) {
    return;
  }

  clearPendingSizeWait(delta.id);
  if (known == null && state === 'complete') {
    void latestDownloadSnapshot(merged).then((latest) => finalizePendingCapture(latest));
    return;
  }
  void finalizePendingCapture(merged);
}

function flushPendingFilenameHint(url: string): void {
  const key = normalizeCaptureUrl(url);
  for (const [id, pending] of pendingSizeWaits) {
    const itemUrls = [pending.item.finalUrl, pending.item.url]
      .filter((value): value is string => Boolean(value))
      .map(normalizeCaptureUrl);
    if (!itemUrls.includes(key)) continue;
    pending.item = withHeaderSize(pending.item);
    if (stillWaitingForCaptureSignals(pending.item, pending)) continue;
    clearPendingSizeWait(id);
    void finalizePendingCapture(pending.item);
  }
}

function onChromiumHeadersReceived(details: ChromiumHeadersReceivedDetails): void {
  if (!rememberResponseFilenameHint(details)) return;
  flushPendingFilenameHint(details.url);
}

function onChromiumBeforeSendHeaders(details: ChromiumBeforeSendHeadersDetails): void {
  rememberRequestAuth(details);
}

function onChromiumDeterminingFilename(
  item: ChromiumDeterminingFilenameItem,
  suggest: (suggestion?: { filename?: string }) => void,
): void {
  const name = applyDeterminedFilename(item, suggest);
  const pending = pendingSizeWaits.get(item.id);
  if (!pending) return;
  pending.item = {
    ...pending.item,
    ...(name ? { filename: name } : {}),
  };
  if (stillWaitingForCaptureSignals(pending.item, pending)) return;
  clearPendingSizeWait(item.id);
  void finalizePendingCapture(pending.item);
}

function registerChromiumFilenameHints(): void {
  const webRequest = getChromiumWebRequest();
  webRequest?.onHeadersReceived?.addListener(
    onChromiumHeadersReceived,
    { urls: ['http://*/*', 'https://*/*'] },
    ['responseHeaders'],
  );
  if (webRequest?.onBeforeSendHeaders) {
    const filter = { urls: ['http://*/*', 'https://*/*'] };
    try {
      webRequest.onBeforeSendHeaders.addListener(
        onChromiumBeforeSendHeaders,
        filter,
        ['requestHeaders', 'extraHeaders'],
      );
    } catch {
      try {
        webRequest.onBeforeSendHeaders.addListener(
          onChromiumBeforeSendHeaders,
          filter,
          ['requestHeaders'],
        );
      } catch {
        // Observing request auth is optional; cookies.getAll is the fallback.
      }
    }
  }
  webRequest?.onBeforeRedirect?.addListener(
    (details) => {
      if (details.redirectUrl) rememberDownloadRedirect(details.url, details.redirectUrl);
    },
    { urls: ['http://*/*', 'https://*/*'] },
  );

  const determining = getChromiumDeterminingFilenameApi();
  determining?.addListener(onChromiumDeterminingFilename);
}

async function handoffFirefoxCandidate(
  candidate: FirefoxCaptureCandidate,
  settings: ExtensionIntegrationSettings,
) {
  const sessionUrl = lookupRedirectSessionUrl(candidate.url);
  const url = sessionUrl ?? candidate.url;
  const attempt = await handOffUrl(url, settings, {
    suggestedFilename: candidate.filename,
    totalBytes: candidate.totalBytes,
  }, {
    pageUrl: candidate.pageUrl,
    referrer: candidate.referrer,
    incognito: candidate.incognito,
    cookieStoreId: candidate.cookieStoreId,
    finalUrl: url === candidate.url ? undefined : candidate.url,
  });
  if (attempt === 'rejected') {
    await restoreBrowserDownload({
      url,
      filename: candidate.filename,
    });
  }
}

function registerFirefoxWebRequestInterception() {
  const webRequest = getFirefoxBlockingWebRequest();
  if (!webRequest) return;

  const filter = {
    urls: ['http://*/*', 'https://*/*'],
    // xmlhttprequest is included so file-host CDNs (Gofile/Pixeldrain fetch)
    // can be handed off before they become an uncatchable blob: download.
    types: ['main_frame', 'object', 'other', 'xmlhttprequest'],
  };

  webRequest.onBeforeRequest?.addListener(
    (details: FirefoxBeforeRequestDetails) => handleFirefoxBeforeRequestSync(details),
    filter,
    ['blocking'],
  );

  webRequest.onHeadersReceived.addListener(
    (details: FirefoxHeadersReceivedDetails) =>
      handleFirefoxHeadersReceivedSync(details, webRequest),
    filter,
    ['blocking', 'responseHeaders'],
  );

  webRequest.onBeforeRedirect?.addListener(
    (details) => {
      if (details.redirectUrl) rememberDownloadRedirect(details.url, details.redirectUrl);
    },
    { urls: ['http://*/*', 'https://*/*'] },
  );
}

function acceptFirefoxCandidate(
  candidate: FirefoxCaptureCandidate,
  settings: ExtensionIntegrationSettings,
  detailsType?: string,
): { cancel: true } {
  if (settings.downloadCaptureDebugLogging) {
    console.info('[RusticDL] capture candidate', {
      reason: candidate.reason,
      url: candidate.url,
      filename: candidate.filename,
      totalBytes: candidate.totalBytes,
      type: detailsType,
    });
  }
  rememberInterceptedDownload(candidate.url, candidate.filename);
  void handoffFirefoxCandidate(candidate, settings);
  return { cancel: true };
}

/** Cancel Gofile/Pixeldrain/Buzzheavier object URLs before any body is read. */
function handleFirefoxBeforeRequestSync(
  details: FirefoxBeforeRequestDetails,
): { cancel?: boolean } {
  try {
    if (shouldSkipRestoredUrl(details.url)) {
      return {};
    }
    const settings = settingsForSyncCapture();
    if (!settings) {
      return {};
    }
    const candidate = firefoxBeforeRequestDownloadCandidate(details, settings);
    if (!candidate) {
      return {};
    }
    return acceptFirefoxCandidate(candidate, settings, details.type);
  } catch {
    return {};
  }
}

/**
 * Must stay synchronous. `{ cancel: true }` is ignored after Firefox attaches a
 * download, so also close the response stream to stop the body.
 */
function handleFirefoxHeadersReceivedSync(
  details: FirefoxHeadersReceivedDetails,
  webRequest: NonNullable<ReturnType<typeof getFirefoxBlockingWebRequest>>,
): { cancel?: boolean } {
  try {
    if (shouldSkipRestoredUrl(details.url)) {
      return {};
    }
    const settings = settingsForSyncCapture();
    if (!settings) {
      return {};
    }
    const candidate = firefoxWebRequestDownloadCandidate(details, settings);
    if (!candidate) {
      return {};
    }
    abortFirefoxResponseBody(webRequest, details.requestId);
    return acceptFirefoxCandidate(candidate, settings, details.type);
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
// Chromium: observe Content-Disposition so handoff is not a URL token.
registerChromiumFilenameHints();
if (browser.downloads?.onCreated) {
  browser.downloads.onCreated.addListener((item) => {
    pauseIfLikelyCapture(item);
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
