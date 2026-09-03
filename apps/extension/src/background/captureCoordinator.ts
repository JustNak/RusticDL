/**
 * Download capture orchestration: pause / erase / handoff / restore.
 *
 * User cancel on the ask prompt aborts (no browser restore). Host errors still
 * restore the shelf item. Session state lives in captureSession (persisted).
 * This module talks to the downloads API and native host. index.ts only
 * registers listeners.
 */
import {
  isErrorResponse,
  toUserFacingMessage,
  type DownloadRequestMetadata,
  type ExtensionIntegrationSettings,
  type HostToExtensionResponse,
} from '@rusticdl/protocol';
import { createDefaultExtensionSettings } from '../shared/defaultExtensionSettings';
import browser from './browser';
import {
  downloadCreatedAction,
  filenameExtension,
  handoffUrlForCapturedDownload,
  isWeakSuggestedFilename,
  knownDownloadBytes,
  lookupRedirectSessionUrl,
  MIN_CAPTURE_BYTES,
  normalizeCaptureUrl,
  rememberDownloadRedirect,
  shouldPauseDownloadItem,
} from './captureFilter';
import {
  attachToSession,
  beginHandoff,
  CAPTURE_SESSIONS_STORAGE_KEY,
  createCaptureSessionStore,
  decideCreatedAction,
  decideFirefoxCandidateAction,
  peekCreatedAction,
  dropCaptureSession,
  finishHandoff,
  firefoxQueuedReplayAction,
  followCaptureFamily,
  createdActionShouldResume,
  probeFromDownload,
  sessionsFromStorageValue,
  sessionsToStorageValue,
  type CaptureFamilyProbe,
  type CaptureSessionStore,
  type FirefoxCandidateAction,
} from './captureSession';
import {
  applyDeterminedFilename,
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
  type FirefoxBeforeRequestDetails,
  type FirefoxCaptureCandidate,
  type FirefoxHeadersReceivedDetails,
} from './firefoxCapture';
import {
  collectHandoffAuth,
  connectionForErrorCode,
  handoffDownload,
} from './nativeMessaging';
import { setHostError, setLastResult } from './state';
import type { PopupStateResponse } from '../shared/messages';

/** Wait for size (Firefox stubs) and/or a real filename (Chromium CDN tokens). */
const SIZE_WAIT_MS = 1_500;

export type CapturedDownloadItem = browser.downloads.DownloadItem & {
  finalUrl?: string;
  byExtensionId?: string;
  cookieStoreId?: string;
  fileSize?: number;
  bytesReceived?: number;
  hasAttachment?: boolean;
};

type PendingSizeWait = {
  item: CapturedDownloadItem;
  sessionId: string;
  timer: ReturnType<typeof setTimeout>;
  waitingForSize: boolean;
  waitingForName: boolean;
};

type DownloadChangeDelta = {
  id: number;
  url?: { current?: string };
  filename?: { current?: string };
  mime?: { current?: string };
  state?: { current?: string };
  totalBytes?: { current?: number };
  fileSize?: { current?: number };
};

type HandoffAttempt = 'accepted' | 'already_claimed' | 'dismissed' | 'rejected';
type HandoffOutcome = 'accepted' | 'dismissed' | 'rejected';

type CoordinatorHostHooks = {
  updateBadge(state: PopupStateResponse): Promise<void>;
};

const pendingSizeWaits = new Map<number, PendingSizeWait>();
const pausedForCapture = new Set<number>();

type QueuedCaptureEvent =
  | { kind: 'redirect'; from: string; to: string; requestId?: string }
  | {
      kind: 'firefox-handoff';
      source: 'before-request' | 'headers-received';
      details: FirefoxBeforeRequestDetails | FirefoxHeadersReceivedDetails;
      candidate: FirefoxCaptureCandidate;
    };

const queuedCaptureEvents: QueuedCaptureEvent[] = [];

let store: CaptureSessionStore = createCaptureSessionStore();
let sessionsReady = false;
let persistChain = Promise.resolve();
let hostHooks: CoordinatorHostHooks = {
  async updateBadge() {
    // set by index.ts after badge helper exists
  },
};

function heuristicCaptureSettings(
  settings: ExtensionIntegrationSettings | null,
): ExtensionIntegrationSettings {
  return settings ?? createDefaultExtensionSettings();
}

function enqueueCaptureEvent(event: QueuedCaptureEvent): void {
  queuedCaptureEvents.push(event);
}

export function configureCaptureCoordinator(hooks: CoordinatorHostHooks): void {
  hostHooks = hooks;
}

export function captureSessionsReady(): boolean {
  return sessionsReady;
}

function sessionStorageArea(): {
  get(keys?: string | string[]): Promise<Record<string, unknown>>;
  set(items: Record<string, unknown>): Promise<void>;
} {
  const storage = browser.storage as {
    session?: {
      get(keys?: string | string[]): Promise<Record<string, unknown>>;
      set(items: Record<string, unknown>): Promise<void>;
    };
    local: {
      get(keys?: string | string[]): Promise<Record<string, unknown>>;
      set(items: Record<string, unknown>): Promise<void>;
    };
  };
  return storage.session ?? storage.local;
}

function persistSessions(): void {
  if (!sessionsReady) return;
  const snapshot = sessionsToStorageValue(store);
  persistChain = persistChain.then(
    () => sessionStorageArea().set({ [CAPTURE_SESSIONS_STORAGE_KEY]: snapshot }),
    () => sessionStorageArea().set({ [CAPTURE_SESSIONS_STORAGE_KEY]: snapshot }),
  );
}

export async function hydrateCaptureSessions(): Promise<void> {
  await persistChain;
  try {
    const stored = await sessionStorageArea().get(CAPTURE_SESSIONS_STORAGE_KEY);
    store = sessionsFromStorageValue(stored[CAPTURE_SESSIONS_STORAGE_KEY]);
  } catch {
    store = createCaptureSessionStore();
  }
  sessionsReady = true;
}

export function resetCaptureCoordinatorForTests(): void {
  store = createCaptureSessionStore();
  sessionsReady = true;
  queuedCaptureEvents.length = 0;
  pendingSizeWaits.clear();
  pausedForCapture.clear();
}

export function flushQueuedCaptureEvents(
  settings: ExtensionIntegrationSettings,
): void {
  if (!sessionsReady) return;
  const events = queuedCaptureEvents.splice(0, queuedCaptureEvents.length);
  for (const event of events) {
    switch (event.kind) {
      case 'redirect':
        followCaptureRedirect(event.from, event.to, event.requestId);
        break;
      case 'firefox-handoff':
        // Same as the live webRequest path: claim/restore is sync, the
        // ask-mode native prompt is fire-and-forget so startup is not
        // blocked for DOWNLOAD_PROMPT_TIMEOUT.
        void replayQueuedFirefoxHandoff(event, settings);
        break;
      default: {
        const _exhaustive: never = event;
        return _exhaustive;
      }
    }
  }
}

function familyProbe(
  item: Pick<CapturedDownloadItem, 'url' | 'finalUrl' | 'filename'>,
  requestId?: string,
): CaptureFamilyProbe {
  const sessionUrl =
    lookupRedirectSessionUrl(item.url) ?? lookupRedirectSessionUrl(item.finalUrl);
  return probeFromDownload(item, { requestId, sessionUrl });
}

export function followCaptureRedirect(
  from: string,
  to: string,
  requestId?: string,
): void {
  rememberDownloadRedirect(from, to);
  if (!sessionsReady) {
    enqueueCaptureEvent({ kind: 'redirect', from, to, requestId });
    return;
  }
  const sessionUrl =
    lookupRedirectSessionUrl(to) ?? lookupRedirectSessionUrl(from);
  followCaptureFamily(store, {
    urls: [from, to],
    requestId,
    sessionUrl,
  });
  persistSessions();
}

function withHeaderSize(item: CapturedDownloadItem): CapturedDownloadItem {
  const hint = lookupFilenameHint(item.finalUrl || item.url) ?? lookupFilenameHint(item.url);
  const next: CapturedDownloadItem = { ...item };
  if (knownDownloadBytes(item) == null && hint?.totalBytes != null) {
    next.totalBytes = hint.totalBytes;
  }
  if (hint?.hasAttachment) {
    next.hasAttachment = true;
  }
  return next;
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

function classifyHandoffResponse(response: HostToExtensionResponse): HandoffOutcome {
  if (isErrorResponse(response) || !response.ok || response.type !== 'accepted') {
    return 'rejected';
  }
  switch (response.payload.status) {
    case 'queued':
    case 'duplicate_existing_job':
      return 'accepted';
    case 'dismissed':
      return 'dismissed';
    default: {
      const _exhaustive: never = response.payload.status;
      return _exhaustive;
    }
  }
}

async function restoreBrowserDownload(snapshot: {
  url: string;
  filename?: string;
  relatedUrl?: string;
  sessionId?: string;
}): Promise<void> {
  const probe: CaptureFamilyProbe = {
    urls: [snapshot.url, snapshot.relatedUrl],
    filename: snapshot.filename,
  };
  if (snapshot.sessionId) {
    const session = store.sessions.find((entry) => entry.id === snapshot.sessionId);
    if (session && session.phase !== 'restoring') {
      finishHandoff(store, snapshot.sessionId, 'rejected');
    }
    attachToSession(store, snapshot.sessionId, probe);
  } else {
    const existing = followCaptureFamily(store, probe);
    if (existing) {
      if (existing.phase !== 'restoring') finishHandoff(store, existing.id, 'rejected');
    } else {
      const claim = beginHandoff(store, probe);
      finishHandoff(store, claim.session.id, 'rejected');
    }
  }
  persistSessions();

  try {
    const options: { url: string; filename?: string } = { url: snapshot.url };
    const name = snapshot.filename?.split(/[\\/]/).pop();
    if (name && !/[\\/:]/.test(name)) {
      options.filename = name;
    }
    await browser.downloads.download(options);
  } catch {
    // Leave the restoring session so a site retry cannot reopen the prompt.
  }
}

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
    filename?: string;
    requestId?: string;
    sessionId?: string;
  } = {},
): Promise<HandoffAttempt> {
  const probe: CaptureFamilyProbe = {
    urls: [url, extra.finalUrl],
    requestId: extra.requestId,
    sessionUrl: lookupRedirectSessionUrl(url) ?? lookupRedirectSessionUrl(extra.finalUrl),
    filename: extra.filename,
  };
  let sessionId = extra.sessionId;
  if (sessionId) {
    attachToSession(store, sessionId, probe);
    const existing = store.sessions.find((entry) => entry.id === sessionId);
    if (!existing || (existing.phase !== 'handoff' && existing.phase !== 'pending')) {
      persistSessions();
      return 'already_claimed';
    }
    if (existing.phase === 'pending') existing.phase = 'handoff';
  } else {
    const claim = beginHandoff(store, probe);
    if (!claim.ok && claim.session.phase !== 'pending') {
      persistSessions();
      return 'already_claimed';
    }
    sessionId = claim.session.id;
    if (claim.session.phase === 'pending') claim.session.phase = 'handoff';
  }
  persistSessions();

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

  const outcome = classifyHandoffResponse(response);
  switch (outcome) {
    case 'accepted': {
      finishHandoff(store, sessionId, 'accepted');
      persistSessions();
      const state = await setLastResult('connected', response);
      await hostHooks.updateBadge(state);
      return 'accepted';
    }
    case 'dismissed':
      finishHandoff(store, sessionId, 'canceled');
      persistSessions();
      return 'dismissed';
    case 'rejected':
      finishHandoff(store, sessionId, 'rejected');
      persistSessions();
      if (isErrorResponse(response)) {
        const connection = connectionForErrorCode(response.code);
        const state = await setHostError(
          response.code,
          response.message || toUserFacingMessage(response.code, response.message),
          connection,
        );
        await hostHooks.updateBadge(state);
      }
      return 'rejected';
    default: {
      const _exhaustive: never = outcome;
      return _exhaustive;
    }
  }
}

async function handoffBrowserDownload(
  item: CapturedDownloadItem,
  settings: ExtensionIntegrationSettings,
  sessionId?: string,
) {
  const url = handoffUrlForCapturedDownload(item);
  if (!url) {
    if (sessionId) dropCaptureSession(store, sessionId);
    persistSessions();
    return;
  }

  const suggestedFilename = resolveSuggestedFilename(item);
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
    filename: suggestedFilename,
    sessionId,
  });

  await restoreIfHandoffRejected(attempt, {
    url,
    filename: suggestedFilename,
    sessionId,
  });
}

function deltaCurrent<T>(field: { current?: T } | T | undefined): T | undefined {
  if (field == null) return undefined;
  if (typeof field === 'object' && 'current' in (field as object)) {
    return (field as { current?: T }).current;
  }
  return field as T;
}

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

async function finalizePendingCapture(
  item: CapturedDownloadItem,
  settings: ExtensionIntegrationSettings,
  sessionId?: string,
) {
  const tracked = withHeaderSize(item);
  const probe = familyProbe(tracked);
  const existing = followCaptureFamily(store, probe);
  if (existing?.phase === 'restoring') {
    persistSessions();
    return;
  }
  if (existing && existing.phase !== 'pending') {
    await eraseBrowserDownload(item.id);
    persistSessions();
    return;
  }

  if (
    !settings.enabled
    || settings.downloadHandoffMode === 'off'
    || isCaptureSizeStub(tracked)
    || ignoredResolvedExtension(resolveSuggestedFilename(tracked), settings)
  ) {
    if (sessionId) dropCaptureSession(store, sessionId);
    persistSessions();
    if (pausedForCapture.has(item.id)) {
      await resumeBrowserDownload(item.id);
    }
    return;
  }
  await handoffBrowserDownload(tracked, settings, sessionId ?? existing?.id);
}

function stillWaitingForCaptureSignals(
  item: CapturedDownloadItem,
  pending: PendingSizeWait,
): boolean {
  if (pending.waitingForSize && knownDownloadBytes(item) == null) return true;
  return pending.waitingForName && needsFilenameHint(item);
}

/**
 * Stop the shelf item before any await. Known captures with a real name are
 * canceled immediately so Ask-mode does not sit on "Paused — 30 MB of 2.5 GB".
 */
export function pauseIfLikelyCapture(
  item: CapturedDownloadItem,
  settings: ExtensionIntegrationSettings | null,
): void {
  const tracked = withHeaderSize(item);
  if (!sessionsReady) {
    // Pause on the waking onCreated turn; do not touch the session store
    // until hydrate finishes (peek/follow would write into an empty store).
    if (shouldPauseDownloadItem(tracked, heuristicCaptureSettings(settings))) {
      requestPause(item.id);
    }
    return;
  }
  if (!settings) return;
  const decision = peekCreatedAction(store, tracked, settings, familyProbe(tracked));
  persistSessions();
  switch (decision) {
    case 'skip-restore':
    case 'ignore':
      return;
    case 'erase-ghost':
      void eraseBrowserDownload(item.id);
      return;
    case 'handoff':
      if (!needsFilenameHint(tracked)) {
        void eraseBrowserDownload(item.id);
        return;
      }
      requestPause(item.id);
      return;
    case 'wait':
      requestPause(item.id);
      return;
    default: {
      const _exhaustive: never = decision;
      return _exhaustive;
    }
  }
}

export async function onDownloadCreated(
  item: CapturedDownloadItem,
  settings: ExtensionIntegrationSettings,
): Promise<void> {
  if (!sessionsReady) return;
  const tracked = withHeaderSize(item);
  const extra = familyProbe(tracked);
  const decision = decideCreatedAction(store, tracked, settings, extra);
  persistSessions();

  switch (decision) {
    case 'skip-restore':
    case 'ignore':
      if (createdActionShouldResume(decision) && pausedForCapture.has(tracked.id)) {
        await resumeBrowserDownload(tracked.id);
      }
      return;
    case 'erase-ghost':
      await eraseBrowserDownload(tracked.id);
      return;
    case 'handoff': {
      const session = followCaptureFamily(store, extra);
      await handoffBrowserDownload(tracked, settings, session?.id);
      return;
    }
    case 'wait': {
      const session = followCaptureFamily(store, extra);
      requestPause(tracked.id);
      const timer = setTimeout(() => {
        const pending = pendingSizeWaits.get(tracked.id);
        if (!pending) return;
        pendingSizeWaits.delete(tracked.id);
        void latestDownloadSnapshot(pending.item).then((latest) =>
          finalizePendingCapture(latest, settings, pending.sessionId),
        );
      }, SIZE_WAIT_MS);
      pendingSizeWaits.set(tracked.id, {
        item: tracked,
        sessionId: session?.id ?? '',
        timer,
        waitingForSize: downloadCreatedAction(tracked, settings) === 'wait',
        waitingForName: needsFilenameHint(tracked),
      });
      return;
    }
    default: {
      const _exhaustive: never = decision;
      return _exhaustive;
    }
  }
}

export function onDownloadChanged(
  delta: DownloadChangeDelta,
  settings: ExtensionIntegrationSettings | null,
): void {
  if (!sessionsReady || !settings) return;
  const pending = pendingSizeWaits.get(delta.id);
  if (!pending) return;

  const merged = mergeDownloadDelta(pending.item, delta);
  pending.item = merged;
  const existing = followCaptureFamily(store, familyProbe(merged));
  persistSessions();
  if (existing && existing.phase !== 'pending' && existing.phase !== 'handoff') {
    if (existing.phase === 'restoring') {
      clearPendingSizeWait(delta.id);
      return;
    }
    clearPendingSizeWait(delta.id);
    void eraseBrowserDownload(delta.id);
    return;
  }

  const known = knownDownloadBytes(merged);
  const state = deltaCurrent(delta.state);
  if (known == null && state !== 'complete') return;
  if (state !== 'complete' && stillWaitingForCaptureSignals(merged, pending)) {
    return;
  }

  clearPendingSizeWait(delta.id);
  if (known == null && state === 'complete') {
    void latestDownloadSnapshot(merged).then((latest) =>
      finalizePendingCapture(latest, settings, pending.sessionId),
    );
    return;
  }
  void finalizePendingCapture(merged, settings, pending.sessionId);
}

export function flushPendingFilenameHint(url: string, settings: ExtensionIntegrationSettings | null): void {
  if (!settings) return;
  const key = normalizeCaptureUrl(url);
  for (const [id, pending] of pendingSizeWaits) {
    const itemUrls = [pending.item.finalUrl, pending.item.url]
      .filter((value): value is string => Boolean(value))
      .map(normalizeCaptureUrl);
    if (!itemUrls.includes(key)) continue;
    pending.item = withHeaderSize(pending.item);
    if (stillWaitingForCaptureSignals(pending.item, pending)) continue;
    clearPendingSizeWait(id);
    void finalizePendingCapture(pending.item, settings, pending.sessionId);
  }
}

export function onChromiumHeadersReceived(
  details: ChromiumHeadersReceivedDetails,
  settings: ExtensionIntegrationSettings | null,
): void {
  if (!rememberResponseFilenameHint(details)) return;
  flushPendingFilenameHint(details.url, settings);
}

export function onChromiumBeforeSendHeaders(details: ChromiumBeforeSendHeadersDetails): void {
  rememberRequestAuth(details);
}

export function onChromiumDeterminingFilename(
  item: ChromiumDeterminingFilenameItem,
  suggest: (suggestion?: { filename?: string }) => void,
  settings: ExtensionIntegrationSettings | null,
): void {
  const name = applyDeterminedFilename(item, suggest);
  const pending = pendingSizeWaits.get(item.id);
  if (!pending || !settings) return;
  pending.item = {
    ...pending.item,
    ...(name ? { filename: name } : {}),
  };
  followCaptureFamily(store, familyProbe(pending.item));
  persistSessions();
  if (stillWaitingForCaptureSignals(pending.item, pending)) return;
  clearPendingSizeWait(item.id);
  void finalizePendingCapture(pending.item, settings, pending.sessionId);
}

async function restoreIfHandoffRejected(
  attempt: HandoffAttempt,
  snapshot: {
    url: string;
    filename?: string;
    relatedUrl?: string;
    sessionId?: string;
  },
): Promise<void> {
  switch (attempt) {
    case 'accepted':
    case 'already_claimed':
    case 'dismissed':
      return;
    case 'rejected':
      await restoreBrowserDownload(snapshot);
      return;
    default: {
      const _exhaustive: never = attempt;
      return _exhaustive;
    }
  }
}

async function handoffFirefoxCandidate(
  candidate: FirefoxCaptureCandidate,
  settings: ExtensionIntegrationSettings,
  sessionId?: string,
  requestId?: string,
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
    filename: candidate.filename,
    requestId,
    sessionId,
  });
  await restoreIfHandoffRejected(attempt, {
    url,
    filename: candidate.filename,
    relatedUrl: candidate.url !== url ? candidate.url : undefined,
    sessionId,
  });
}

function acceptFirefoxCandidate(
  candidate: FirefoxCaptureCandidate,
  settings: ExtensionIntegrationSettings,
  requestId?: string,
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
  const probe: CaptureFamilyProbe = {
    urls: [candidate.url, lookupRedirectSessionUrl(candidate.url)],
    requestId,
    sessionUrl: lookupRedirectSessionUrl(candidate.url),
    filename: candidate.filename,
  };
  const session = followCaptureFamily(store, probe);
  persistSessions();
  void handoffFirefoxCandidate(candidate, settings, session?.id, requestId);
  return { cancel: true };
}

function applyFirefoxAction(
  action: FirefoxCandidateAction | null,
  candidate: FirefoxCaptureCandidate | null,
  settings: ExtensionIntegrationSettings,
  details: { requestId?: string; type?: string },
): { cancel?: boolean } {
  switch (action) {
    case null:
    case 'allow':
      return {};
    case 'cancel-ghost':
      return { cancel: true };
    case 'handoff':
      if (!candidate) return {};
      return acceptFirefoxCandidate(candidate, settings, details.requestId, details.type);
    default: {
      const _exhaustive: never = action;
      return _exhaustive;
    }
  }
}

function decideFirefoxRequest(
  details: { url: string; requestId?: string },
  candidate: FirefoxCaptureCandidate | null,
): FirefoxCandidateAction | null {
  const existing = followCaptureFamily(store, {
    urls: [details.url],
    requestId: details.requestId,
    sessionUrl: lookupRedirectSessionUrl(details.url),
  });
  if (existing?.phase === 'restoring') return 'allow';
  if (!candidate) return null;
  if (existing) return 'cancel-ghost';
  return decideFirefoxCandidateAction(store, {
    urls: [candidate.url],
    requestId: details.requestId,
    sessionUrl: lookupRedirectSessionUrl(candidate.url),
    filename: candidate.filename,
  });
}

function firefoxCandidateFromQueued(
  event: Extract<QueuedCaptureEvent, { kind: 'firefox-handoff' }>,
  settings: ExtensionIntegrationSettings,
): FirefoxCaptureCandidate | null {
  switch (event.source) {
    case 'before-request':
      return firefoxBeforeRequestDownloadCandidate(
        event.details as FirefoxBeforeRequestDetails,
        settings,
      );
    case 'headers-received':
      return firefoxWebRequestDownloadCandidate(
        event.details as FirefoxHeadersReceivedDetails,
        settings,
      );
    default: {
      const _exhaustive: never = event.source;
      return _exhaustive;
    }
  }
}

function replayQueuedFirefoxHandoff(
  event: Extract<QueuedCaptureEvent, { kind: 'firefox-handoff' }>,
  settings: ExtensionIntegrationSettings,
): void {
  const probe: CaptureFamilyProbe = {
    urls: [event.candidate.url, lookupRedirectSessionUrl(event.candidate.url)],
    requestId: event.details.requestId,
    sessionUrl: lookupRedirectSessionUrl(event.candidate.url),
    filename: event.candidate.filename,
  };
  const existing = followCaptureFamily(store, probe);
  persistSessions();
  const stillCandidate = firefoxCandidateFromQueued(event, settings);
  const action = firefoxQueuedReplayAction(existing?.phase, stillCandidate != null);
  switch (action) {
    case 'ignore':
      return;
    case 'restore':
      void restoreBrowserDownload({
        url: event.candidate.url,
        filename: event.candidate.filename,
        sessionId: existing?.id,
      });
      return;
    case 'handoff':
      void handoffFirefoxCandidate(
        stillCandidate ?? event.candidate,
        settings,
        existing?.id,
        event.details.requestId,
      );
      return;
    default: {
      const _exhaustive: never = action;
      return _exhaustive;
    }
  }
}

export function handleFirefoxBeforeRequestSync(
  details: FirefoxBeforeRequestDetails,
  settings: ExtensionIntegrationSettings | null,
): { cancel?: boolean } {
  try {
    if (!sessionsReady) {
      const candidate = firefoxBeforeRequestDownloadCandidate(
        details,
        heuristicCaptureSettings(settings),
      );
      if (!candidate) return {};
      enqueueCaptureEvent({
        kind: 'firefox-handoff',
        source: 'before-request',
        details,
        candidate,
      });
      return { cancel: true };
    }
    if (!settings) return {};
    const candidate = firefoxBeforeRequestDownloadCandidate(details, settings);
    const action = decideFirefoxRequest(details, candidate);
    persistSessions();
    return applyFirefoxAction(action, candidate, settings, details);
  } catch {
    return {};
  }
}

export function handleFirefoxHeadersReceivedSync(
  details: FirefoxHeadersReceivedDetails,
  webRequest: NonNullable<ReturnType<typeof getFirefoxBlockingWebRequest>>,
  settings: ExtensionIntegrationSettings | null,
): { cancel?: boolean } {
  try {
    if (!sessionsReady) {
      const candidate = firefoxWebRequestDownloadCandidate(
        details,
        heuristicCaptureSettings(settings),
      );
      if (!candidate) return {};
      abortFirefoxResponseBody(webRequest, details.requestId);
      enqueueCaptureEvent({
        kind: 'firefox-handoff',
        source: 'headers-received',
        details,
        candidate,
      });
      return { cancel: true };
    }
    if (!settings) return {};
    const candidate = firefoxWebRequestDownloadCandidate(details, settings);
    const action = decideFirefoxRequest(details, candidate);
    persistSessions();
    if (action === 'cancel-ghost' || action === 'handoff') {
      abortFirefoxResponseBody(webRequest, details.requestId);
    }
    return applyFirefoxAction(action, candidate, settings, details);
  } catch {
    return {};
  }
}
