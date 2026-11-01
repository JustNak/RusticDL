/**
 * Download capture orchestration.
 *
 * Chromium hand-off goes through captureLifecycle.ts. This file applies that
 * table and runs its effects. downloads.pause / resume / cancel / removeFile /
 * erase are not called here. Firefox still restores with downloads.download.
 *
 * User cancel on the ask prompt aborts (no browser restore). Host errors still
 * restore the shelf item. Session state lives in captureSession (persisted).
 * index.ts only registers listeners.
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
  isNonUserInitiatedResourceType,
  isWeakSuggestedFilename,
  knownDownloadBytes,
  lookupRedirectSessionUrl,
  MIN_CAPTURE_BYTES,
  normalizeCaptureUrl,
  rememberDownloadRedirect,
  sanitizeSuggestedFilename,
  shouldPauseDownloadItem,
} from './captureFilter';
import {
  attachToSession,
  beginHandoff,
  beginPending,
  CAPTURE_SESSIONS_STORAGE_KEY,
  createCaptureSessionStore,
  decideCreatedAction,
  decideFirefoxCandidateAction,
  peekCreatedAction,
  dropCaptureSession,
  finishHandoff,
  firefoxQueuedReplayAction,
  followCaptureFamily,
  probeFromDownload,
  sessionsFromStorageValue,
  sessionsToStorageValue,
  type CaptureFamilyProbe,
  type CaptureSessionStore,
  type FirefoxCandidateAction,
} from './captureSession';
import {
  applyChromiumCaptureHints,
  applyDeterminedFilename,
  lookupFilenameHint,
  lookupOriginAuth,
  lookupResourceType,
  needsDeterminedFilename,
  rememberRequestAuth,
  rememberResourceType,
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
import {
  applyCaptureTransition,
  eraseDownload,
  initialCaptureLifecycle,
  isTerminalCapturePhase,
  pauseDownload,
  readCaptureTransitionTrace,
  resetCaptureTransitionTrace,
  resumeDownload,
  type CaptureEffect,
  type CaptureEvent,
  type CaptureLifecycle,
} from './captureLifecycle';
import { setHostError, setLastResult } from './state';
import type { PopupStateResponse } from '../shared/messages';

/** Wait for size (Firefox stubs) and/or a real filename (Chromium CDN tokens). */
const SIZE_WAIT_MS = 1_500;
/** Host round-trip budget. Longer than the slow-host proof delay (1500 ms). */
const HOST_WAIT_MS = 20_000;
const CAPTURE_LIFECYCLE_STORAGE_KEY = 'rusticdl.capture-lifecycle';

export type CapturedDownloadItem = browser.downloads.DownloadItem & {
  finalUrl?: string;
  byExtensionId?: string;
  cookieStoreId?: string;
  fileSize?: number;
  bytesReceived?: number;
  hasAttachment?: boolean;
  filenameFromUrlBasename?: boolean;
  resourceType?: string;
  chromeDeterminedFilename?: string;
};

type DownloadChangeDelta = {
  id: number;
  url?: { current?: string };
  filename?: { current?: string };
  mime?: { current?: string };
  state?: { current?: string };
  paused?: { current?: boolean };
  totalBytes?: { current?: number };
  fileSize?: { current?: number };
  error?: { current?: string } | string;
};

type HandoffAttempt = 'accepted' | 'already_claimed' | 'dismissed' | 'rejected' | 'disconnected';
type HandoffOutcome = 'accepted' | 'dismissed' | 'rejected';

type CoordinatorHostHooks = {
  updateBadge(state: PopupStateResponse): Promise<void>;
};

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
const tracks = new Map<number, CaptureTrack>();
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
    const stored = await sessionStorageArea().get([
      CAPTURE_SESSIONS_STORAGE_KEY,
      CAPTURE_LIFECYCLE_STORAGE_KEY,
    ]);
    store = sessionsFromStorageValue(stored[CAPTURE_SESSIONS_STORAGE_KEY]);
    sessionsReady = true;
    restoreLifecycles(stored[CAPTURE_LIFECYCLE_STORAGE_KEY]);
  } catch {
    store = createCaptureSessionStore();
    sessionsReady = true;
  }
}

export function resetCaptureCoordinatorForTests(): void {
  store = createCaptureSessionStore();
  sessionsReady = true;
  queuedCaptureEvents.length = 0;
  for (const track of tracks.values()) clearTrackTimers(track);
  tracks.clear();
  resetCaptureTransitionTrace();
}

export function captureTransitionTrace(): readonly string[] {
  return readCaptureTransitionTrace();
}

export async function whenCaptureIdleForTests(): Promise<void> {
  for (let attempt = 0; attempt < 40; attempt += 1) {
    const pending = [...tracks.values()].map((track) => track.drainDone);
    await Promise.all(pending);
    const busy = [...tracks.values()].some((track) => track.draining || track.queue.length > 0);
    if (!busy) return;
  }
}

export function flushQueuedCaptureEvents(
  settings: ExtensionIntegrationSettings,
): void {
  if (!sessionsReady) return;
  bindCaptureSettings(settings);
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
  return applyChromiumCaptureHints(item);
}

function needsResourceType(item: CapturedDownloadItem): boolean {
  if (!item.filenameFromUrlBasename) return false;
  const type = item.resourceType
    ?? lookupResourceType(item.finalUrl)
    ?? lookupResourceType(item.url);
  return type == null || type === '';
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
      if (response.requestId === 'native_messaging_error') return 'disconnected';
      return 'rejected';
    default: {
      const _exhaustive: never = outcome;
      return _exhaustive;
    }
  }
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
  const mime = deltaCurrent(delta.mime);
  const url = deltaCurrent(delta.url);
  const filename = deltaCurrent(delta.filename);
  const paused = deltaCurrent(delta.paused);
  const state = deltaCurrent(delta.state);
  const chromeDeterminedFilename = sanitizeSuggestedFilename(filename);
  // Keep DownloadItem.filename off the capture decision. Chrome's onChanged
  // name is send-only (C1 `app.zip` / html-evil `evil.exe`).
  const next: CapturedDownloadItem = {
    ...item,
    ...(url ? { url } : {}),
    ...(mime ? { mime } : {}),
    ...(chromeDeterminedFilename ? { chromeDeterminedFilename } : {}),
    ...(typeof paused === 'boolean' ? { paused } : {}),
    ...(state ? { state: state as CapturedDownloadItem['state'] } : {}),
    ...(typeof fileSize === 'number' && fileSize > 0 ? { fileSize } : {}),
    ...(typeof totalBytes === 'number' && totalBytes > 0 ? { totalBytes } : {}),
  };
  const known = knownDownloadBytes(next);
  if (known != null) next.totalBytes = known;
  return next;
}

async function latestDownloadSnapshot(
  item: CapturedDownloadItem,
): Promise<CapturedDownloadItem | undefined> {
  try {
    const found = await browser.downloads.search({ id: item.id });
    const latest = found[0] as CapturedDownloadItem | undefined;
    if (!latest) return undefined;
    const merged: CapturedDownloadItem = {
      ...item,
      ...latest,
      filename: item.filename,
      filenameFromUrlBasename: item.filenameFromUrlBasename,
      chromeDeterminedFilename:
        sanitizeSuggestedFilename(latest.filename) ?? item.chromeDeterminedFilename,
      paused: typeof latest.paused === 'boolean' ? latest.paused : item.paused,
      hasAttachment: item.hasAttachment === true || latest.hasAttachment === true,
      resourceType: item.resourceType ?? latest.resourceType,
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

type TimerKind = 'name' | 'size' | 'host';

type CaptureTrack = {
  life: CaptureLifecycle;
  item: CapturedDownloadItem;
  settings: ExtensionIntegrationSettings | null;
  sessionId?: string;
  nameSettled: boolean;
  sizeSettled: boolean;
  sizeForced: boolean;
  decidedWait: boolean;
  pauseRequested: boolean;
  pendingComplete: boolean;
  resumeRequested: boolean;
  preHydrate: boolean;
  settled: boolean;
  consumed: boolean;
  timers: Partial<Record<TimerKind, ReturnType<typeof setTimeout>>>;
  queue: CaptureEffect[];
  draining: boolean;
  drainDone: Promise<void>;
};

type StoredLifecycle = {
  id: number;
  life: CaptureLifecycle;
  item: CapturedDownloadItem;
  sessionId?: string;
  nameSettled: boolean;
  sizeSettled: boolean;
  sizeForced: boolean;
  decidedWait: boolean;
};

type CreatedCaptureEvent = Extract<CaptureEvent, { type: 'on-created' }>;
type HoldDecision = 'wait' | 'drop' | 'send';

function refreshTrackedItem(item: CapturedDownloadItem): CapturedDownloadItem {
  return withHeaderSize(item);
}

function makeTrack(
  item: CapturedDownloadItem,
  settings: ExtensionIntegrationSettings | null,
): CaptureTrack {
  const hinted = refreshTrackedItem(item);
  const sized = knownDownloadBytes(hinted) != null;
  return {
    life: initialCaptureLifecycle(),
    item: hinted,
    settings,
    nameSettled: !needsDeterminedFilename(hinted),
    sizeSettled: sized,
    sizeForced: sized,
    decidedWait: false,
    pauseRequested: false,
    pendingComplete: false,
    resumeRequested: false,
    preHydrate: false,
    settled: false,
    consumed: false,
    timers: {},
    queue: [],
    draining: false,
    drainDone: Promise.resolve(),
  };
}

function clearTrackTimers(track: CaptureTrack): void {
  for (const timer of Object.values(track.timers)) {
    if (timer) clearTimeout(timer);
  }
  track.timers = {};
}

function shouldPersist(track: CaptureTrack): boolean {
  if (track.preHydrate || !track.settled || track.consumed) return false;
  if (!isTerminalCapturePhase(track.life.phase)) return true;
  return track.life.phase === 'erased' || track.life.phase === 'accepted';
}

function persistLifecycles(): void {
  if (!sessionsReady) return;
  const snapshot: StoredLifecycle[] = [];
  for (const [id, track] of tracks) {
    if (!shouldPersist(track)) continue;
    snapshot.push({
      id,
      life: { ...track.life },
      item: { ...track.item },
      sessionId: track.sessionId,
      nameSettled: track.nameSettled,
      sizeSettled: track.sizeSettled,
      sizeForced: track.sizeForced,
      decidedWait: track.decidedWait,
    });
  }
  persistChain = persistChain.then(
    () => sessionStorageArea().set({ [CAPTURE_LIFECYCLE_STORAGE_KEY]: snapshot }),
    () => sessionStorageArea().set({ [CAPTURE_LIFECYCLE_STORAGE_KEY]: snapshot }),
  );
}

function isStoredLifecycle(value: unknown): value is StoredLifecycle {
  if (!value || typeof value !== 'object') return false;
  const row = value as Partial<StoredLifecycle>;
  return typeof row.id === 'number' && !!row.life && !!row.item;
}

function restoreLifecycles(value: unknown): void {
  if (!Array.isArray(value)) return;
  for (const row of value) {
    if (!isStoredLifecycle(row)) continue;
    const existing = tracks.get(row.id);
    if (existing?.draining) continue;
    if (existing) clearTrackTimers(existing);
    const track = makeTrack(row.item, existing?.settings ?? null);
    track.life = { ...row.life };
    track.sessionId = row.sessionId;
    track.nameSettled = row.nameSettled;
    track.sizeSettled = row.sizeSettled;
    track.sizeForced = row.sizeForced;
    track.decidedWait = row.decidedWait;
    track.settled = true;
    track.pauseRequested = row.life.wePaused || row.life.phase === 'pausing' || row.life.phase === 'paused';
    track.consumed = isTerminalCapturePhase(row.life.phase);
    if (!track.settings) track.life = { ...track.life, eligible: false };
    tracks.set(row.id, track);
    dispatch(track, { type: 'hydrate' });
  }
}

function bindCaptureSettings(settings: ExtensionIntegrationSettings): void {
  for (const track of tracks.values()) {
    if (track.preHydrate || track.settings) continue;
    track.settings = settings;
    track.item = refreshTrackedItem(track.item);
    if (track.life.sendCount > 0 || isTerminalCapturePhase(track.life.phase)) continue;
    publish(track, 'hint');
  }
}

function kick(track: CaptureTrack): void {
  if (track.draining) return;
  track.draining = true;
  track.drainDone = (async () => {
    try {
      while (track.queue.length > 0) {
        const effect = track.queue.shift();
        if (!effect) break;
        await runEffect(track, effect);
      }
    } finally {
      track.draining = false;
    }
    if (track.queue.length > 0) kick(track);
  })();
}

function notifyFamily(track: CaptureTrack, outcome: 'accepted' | 'rejected' | 'timeout'): void {
  if (!track.sessionId) return;
  for (const other of tracks.values()) {
    if (other === track || other.sessionId !== track.sessionId || other.life.role !== 'ghost') continue;
    dispatch(other, { type: 'family', outcome });
  }
}

function abandonOpenSession(track: CaptureTrack): void {
  if (!sessionsReady || !track.sessionId || track.life.role !== 'primary') return;
  const session = store.sessions.find((entry) => entry.id === track.sessionId);
  if (!session) return;
  if (session.phase !== 'pending' && session.phase !== 'handoff') return;
  dropCaptureSession(store, track.sessionId);
  persistSessions();
  notifyFamily(track, 'rejected');
}

function dispatch(track: CaptureTrack, event: CaptureEvent): void {
  const result = applyCaptureTransition(track.life, event);
  track.life = result.state;
  for (const effect of result.effects) track.queue.push(effect);
  if (
    track.life.phase === 'accepted'
    && event.type !== 'erase'
    && event.type !== 'erase-ok'
    && event.type !== 'erase-fail'
  ) {
    const eraseStep = applyCaptureTransition(track.life, { type: 'erase' });
    track.life = eraseStep.state;
    for (const effect of eraseStep.effects) track.queue.push(effect);
  }
  if (track.life.role === 'primary' && isTerminalCapturePhase(track.life.phase)) {
    abandonOpenSession(track);
  }
  persistLifecycles();
  kick(track);
}

function computeEligible(track: CaptureTrack): boolean {
  const settings = track.settings ?? heuristicCaptureSettings(null);
  const item = track.item;
  const sendName = sanitizeSuggestedFilename(resolveSuggestedFilename(item));
  if (!track.nameSettled || !track.sizeSettled) return false;
  if (!settings.enabled || settings.downloadHandoffMode === 'off') return false;
  if (isCaptureSizeStub(item)) return false;
  if (ignoredResolvedExtension(sendName, settings)) return false;
  if (!sendName || isWeakSuggestedFilename(sendName)) return false;
  if (isNonUserInitiatedResourceType(item.resourceType)) return false;
  if (item.filenameFromUrlBasename && !item.resourceType) return false;
  const action = downloadCreatedAction(item, settings);
  if (action === 'ignore') return false;
  if (action === 'wait' && !track.sizeForced) return false;
  return true;
}

function classifyHold(track: CaptureTrack): HoldDecision {
  track.item = refreshTrackedItem(track.item);
  if (!track.nameSettled || !track.sizeSettled) return 'wait';
  if (needsResourceType(track.item) && !track.decidedWait) return 'wait';
  if (computeEligible(track)) return 'send';
  return 'drop';
}

function signalWaits(item: CapturedDownloadItem): Array<'name' | 'size'> {
  const wait: Array<'name' | 'size'> = [];
  if (needsDeterminedFilename(item)) wait.push('name');
  if (knownDownloadBytes(item) == null || needsResourceType(item)) wait.push('size');
  return wait;
}

function createdEvent(
  intent: CreatedCaptureEvent['intent'],
  eligible: boolean,
  familyAccepted: boolean,
  wait: Array<'name' | 'size'>,
): CreatedCaptureEvent {
  return { type: 'on-created', intent, eligible, familyAccepted, wait };
}

function createdEventFor(track: CaptureTrack, event: CreatedCaptureEvent): CreatedCaptureEvent {
  if (event.intent !== 'pause') return event;
  const hold = classifyHold(track);
  if (hold === 'drop') return { ...event, intent: 'ignore', eligible: false, wait: [] };
  return { ...event, eligible: hold === 'send' };
}

function resolveCreated(
  item: CapturedDownloadItem,
  settings: ExtensionIntegrationSettings,
): { event: CreatedCaptureEvent; sessionId?: string } {
  const extra = familyProbe(item);
  const peek = peekCreatedAction(store, item, settings, extra);
  const nameNeeded = needsDeterminedFilename(item);
  const resourceNeeded = needsResourceType(item);
  if (peek === 'handoff' && (nameNeeded || resourceNeeded)) {
    const claim = beginPending(store, extra);
    persistSessions();
    if (!claim.ok && claim.session.phase !== 'pending') {
      if (claim.session.phase === 'restoring') {
        return { event: createdEvent('skip-restore', false, false, []), sessionId: claim.session.id };
      }
      if (claim.session.phase === 'canceled') {
        return { event: createdEvent('ignore', false, false, []), sessionId: claim.session.id };
      }
      return {
        event: createdEvent('ghost', false, claim.session.phase === 'accepted', []),
        sessionId: claim.session.id,
      };
    }
    return {
      event: createdEvent('wait', false, false, signalWaits(item)),
      sessionId: claim.session.id,
    };
  }
  if (peek === 'ignore' && resourceNeeded) {
    const claim = beginPending(store, extra);
    persistSessions();
    if (!claim.ok && claim.session.phase !== 'pending') {
      if (claim.session.phase === 'restoring') {
        return { event: createdEvent('skip-restore', false, false, []), sessionId: claim.session.id };
      }
      return { event: createdEvent('ignore', false, false, []), sessionId: claim.session.id };
    }
    return {
      event: createdEvent('wait', false, false, signalWaits(item)),
      sessionId: claim.session.id,
    };
  }
  const decision = decideCreatedAction(store, item, settings, extra);
  persistSessions();
  const session = followCaptureFamily(store, extra);
  const sessionId = session?.id;
  switch (decision) {
    case 'skip-restore':
      return { event: createdEvent('skip-restore', false, false, []), sessionId };
    case 'ignore':
      return { event: createdEvent('ignore', false, false, []), sessionId };
    case 'erase-ghost':
      if (!session || session.phase === 'canceled' || session.phase === 'restoring') {
        return { event: createdEvent('ignore', false, false, []), sessionId };
      }
      return {
        event: createdEvent('ghost', false, session.phase === 'accepted', []),
        sessionId,
      };
    case 'handoff':
    case 'wait':
      if (nameNeeded) {
        return { event: createdEvent('wait', false, false, signalWaits(item)), sessionId };
      }
      return { event: createdEvent('pause', false, false, signalWaits(item)), sessionId };
    default: {
      const _exhaustive: never = decision;
      return _exhaustive;
    }
  }
}

function publish(
  track: CaptureTrack,
  kind: 'name' | 'bytes' | 'determined' | 'hint',
): void {
  track.item = refreshTrackedItem(track.item);
  if (!needsDeterminedFilename(track.item)) track.nameSettled = true;
  if (knownDownloadBytes(track.item) != null) track.sizeSettled = true;
  const hold = classifyHold(track);
  const eligible = hold === 'send';
  switch (kind) {
    case 'determined':
      dispatch(track, { type: 'on-determining-filename', eligible });
      break;
    case 'name':
      dispatch(track, { type: 'on-changed', change: 'filename', eligible });
      break;
    case 'bytes':
      dispatch(track, { type: 'on-changed', change: 'bytes', eligible });
      break;
    case 'hint':
      dispatch(track, { type: 'eligible', eligible });
      break;
    default: {
      const _exhaustive: never = kind;
      return _exhaustive;
    }
  }
  if (hold === 'drop' && track.life.role !== 'ghost' && !isTerminalCapturePhase(track.life.phase)) {
    dispatch(track, { type: 'release' });
  }
}

function scheduleTimer(track: CaptureTrack, timer: TimerKind): void {
  const existing = track.timers[timer];
  if (existing) clearTimeout(existing);
  const delay = timer === 'host' ? HOST_WAIT_MS : SIZE_WAIT_MS;
  track.timers[timer] = setTimeout(() => {
    delete track.timers[timer];
    if (timer === 'name') track.nameSettled = true;
    if (timer === 'size') {
      track.sizeSettled = true;
      track.sizeForced = true;
    }
    if (timer !== 'host') track.decidedWait = true;
    if (timer === 'host') {
      dispatch(track, { type: 'timer', timer, eligible: false });
      if (track.life.phase === 'timeout') notifyFamily(track, 'timeout');
      return;
    }
    const hold = classifyHold(track);
    dispatch(track, { type: 'timer', timer, eligible: hold === 'send' });
    if (hold === 'drop' && track.life.role !== 'ghost' && !isTerminalCapturePhase(track.life.phase)) {
      dispatch(track, { type: 'release' });
    }
  }, delay);
}

function shouldUndoPause(track: CaptureTrack): boolean {
  switch (track.life.phase) {
    case 'rejected':
    case 'timeout':
    case 'resumed':
    case 'left':
      return true;
    case 'user-paused':
    case 'user-cancelled':
    case 'interrupted':
    case 'completed-by-browser':
    case 'erased':
    case 'created':
    case 'pausing':
    case 'paused':
    case 'naming':
    case 'sent':
    case 'accepted':
      return false;
    default: {
      const _exhaustive: never = track.life.phase;
      return _exhaustive;
    }
  }
}

function interruptEvent(item: CapturedDownloadItem): CaptureEvent {
  return item.error === 'USER_CANCELED'
    ? { type: 'user-cancel' }
    : { type: 'browser-interrupted' };
}

async function confirmPause(track: CaptureTrack): Promise<void> {
  track.pauseRequested = true;
  // Captured before onChanged can merge a one-shot completion into the item.
  const startedInProgress = track.item.state !== 'complete';
  try {
    await pauseDownload(track.item.id);
  } catch {
    if (!isTerminalCapturePhase(track.life.phase)) dispatch(track, { type: 'pause-fail' });
    return;
  }
  if (shouldUndoPause(track)) {
    try {
      await resumeDownload(track.item.id);
    } catch {
      // the shelf item is already gone
    }
    return;
  }
  const latest = await latestDownloadSnapshot(track.item);
  if (shouldUndoPause(track)) {
    try {
      await resumeDownload(track.item.id);
    } catch {
      // released while the search was in flight
    }
    return;
  }
  if (!latest) {
    dispatch(track, { type: 'pause-fail' });
    return;
  }
  track.item = latest;
  if (latest.state === 'complete' || track.pendingComplete) {
    track.pendingComplete = false;
    // The body finished during pause(). Arm the send while the table still
    // sees an in-progress capture, then record the completion so accept erases.
    if (startedInProgress && track.life.phase === 'pausing') {
      dispatch(track, { type: 'pause-ok' });
    }
    if (track.life.phase === 'sent' || track.life.phase === 'pausing' || track.life.phase === 'paused') {
      dispatch(track, { type: 'browser-complete' });
    }
    return;
  }
  if (latest.state === 'interrupted') {
    dispatch(track, interruptEvent(latest));
    return;
  }
  if (latest.paused === true && latest.state === 'in_progress') {
    dispatch(track, { type: 'pause-ok' });
    return;
  }
  dispatch(track, { type: 'pause-fail' });
}

async function confirmResume(track: CaptureTrack): Promise<void> {
  track.resumeRequested = true;
  try {
    await resumeDownload(track.item.id);
  } catch {
    // user canceled, already complete, or resume is unavailable
  }
  track.resumeRequested = false;
}

async function confirmErase(track: CaptureTrack): Promise<void> {
  if (track.life.phase !== 'accepted') return;
  const latest = await latestDownloadSnapshot(track.item);
  if (latest) track.item = latest;
  if (track.life.phase !== 'accepted') return;
  const erased = await eraseDownload(track.item.id);
  if (track.life.phase !== 'accepted') return;
  dispatch(track, erased ? { type: 'erase-ok' } : { type: 'erase-fail' });
}

function noteUnsentHostMiss(track: CaptureTrack, attempt: HandoffAttempt): void {
  if (attempt === 'accepted') notifyFamily(track, 'accepted');
  else notifyFamily(track, 'rejected');
}

async function runSend(track: CaptureTrack): Promise<void> {
  const settings = track.settings;
  if (!settings || track.life.phase !== 'sent') return;
  const latest = await latestDownloadSnapshot(track.item);
  if (track.life.phase !== 'sent') return;
  if (!latest) {
    dispatch(track, track.life.browserCompleted ? { type: 'release' } : { type: 'hold-lost' });
    return;
  }
  track.item = latest;
  if (track.life.browserCompleted || latest.state === 'complete') {
    // pause-ok already queued this send. A one-shot body can reach complete
    // before the native call; still hand off, then erase on accept.
    if (!track.life.browserCompleted) dispatch(track, { type: 'browser-complete' });
    if (track.life.phase !== 'sent' || track.life.sendCount < 1) {
      if (track.life.phase === 'sent') dispatch(track, { type: 'release' });
      return;
    }
  } else if (latest.state === 'interrupted') {
    dispatch(track, interruptEvent(latest));
    return;
  } else if (!(latest.paused === true && latest.state === 'in_progress')) {
    dispatch(track, { type: 'hold-lost' });
    return;
  }
  track.item = refreshTrackedItem(track.item);
  if (classifyHold(track) !== 'send') {
    dispatch(track, { type: 'release' });
    return;
  }
  const url = handoffUrlForCapturedDownload(track.item);
  const sendName = sanitizeSuggestedFilename(resolveSuggestedFilename(track.item));
  if (!url || !sendName) {
    dispatch(track, { type: 'release' });
    return;
  }
  const attempt = await handOffUrl(url, settings, {
    suggestedFilename: sendName,
    totalBytes: knownDownloadBytes(track.item) ?? lookupFilenameHint(track.item.finalUrl || url)?.totalBytes,
  }, {
    pageUrl: track.item.referrer,
    referrer: track.item.referrer,
    incognito: track.item.incognito,
    cookieStoreId: track.item.cookieStoreId,
    finalUrl: track.item.finalUrl,
    filename: sendName,
    sessionId: track.sessionId,
  });
  if (track.life.phase !== 'sent') {
    noteUnsentHostMiss(track, attempt);
    return;
  }
  const after = await latestDownloadSnapshot(track.item);
  if (after) track.item = after;
  switch (attempt) {
    case 'accepted':
      dispatch(track, { type: 'host-reply', outcome: 'accept' });
      notifyFamily(track, 'accepted');
      return;
    case 'dismissed':
      dispatch(track, { type: 'host-reply', outcome: 'dismiss' });
      notifyFamily(track, 'rejected');
      return;
    case 'disconnected':
      dispatch(track, { type: 'host-reply', outcome: 'disconnect' });
      notifyFamily(track, 'rejected');
      return;
    case 'already_claimed':
    case 'rejected':
      dispatch(track, { type: 'host-reply', outcome: 'reject' });
      notifyFamily(track, 'rejected');
      return;
    default: {
      const _exhaustive: never = attempt;
      return _exhaustive;
    }
  }
}

async function runEffect(track: CaptureTrack, effect: CaptureEffect): Promise<void> {
  switch (effect.type) {
    case 'pause':
      await confirmPause(track);
      return;
    case 'resume':
      await confirmResume(track);
      return;
    case 'send':
      await runSend(track);
      return;
    case 'erase':
      await confirmErase(track);
      return;
    case 'schedule-timer':
      scheduleTimer(track, effect.timer);
      return;
    case 'clear-timers':
      clearTrackTimers(track);
      return;
    case 'log':
      console.info('[RusticDL] capture transition', effect.event);
      return;
    default: {
      const _exhaustive: never = effect;
      return _exhaustive;
    }
  }
}

function startPreHydrate(
  item: CapturedDownloadItem,
  settings: ExtensionIntegrationSettings | null,
): Promise<void> {
  const track = makeTrack(item, settings);
  track.preHydrate = true;
  tracks.set(item.id, track);
  const heuristic = heuristicCaptureSettings(settings);
  const hinted = track.item;
  if (shouldPauseDownloadItem(hinted, heuristic) && !needsDeterminedFilename(hinted)) {
    dispatch(track, createdEvent('pause', false, false, signalWaits(hinted)));
  } else if (needsDeterminedFilename(hinted) && shouldPauseDownloadItem(hinted, heuristic)) {
    dispatch(track, createdEvent('wait', false, false, signalWaits(hinted)));
  }
  return track.drainDone;
}

function settlePreHydrate(
  track: CaptureTrack,
  settings: ExtensionIntegrationSettings,
): Promise<void> {
  track.settings = settings;
  track.preHydrate = false;
  track.settled = true;
  track.item = refreshTrackedItem(track.item);
  if (
    track.life.sendCount > 0
    || track.life.phase === 'sent'
    || track.life.phase === 'accepted'
    || track.life.phase === 'erased'
  ) {
    return track.drainDone;
  }
  const resolved = resolveCreated(track.item, settings);
  track.sessionId = resolved.sessionId;
  if (track.life.phase === 'created') {
    dispatch(track, createdEventFor(track, resolved.event));
    return track.drainDone;
  }
  switch (resolved.event.intent) {
    case 'ignore':
    case 'skip-restore':
      dispatch(track, { type: 'release' });
      break;
    case 'ghost':
      dispatch(track, { type: 'become-ghost', familyAccepted: resolved.event.familyAccepted });
      break;
    case 'pause':
    case 'wait':
      publish(track, 'hint');
      break;
    default: {
      const _exhaustive: never = resolved.event.intent;
      return Promise.resolve(_exhaustive);
    }
  }
  return track.drainDone;
}

function startMachine(
  item: CapturedDownloadItem,
  settings: ExtensionIntegrationSettings,
): Promise<void> {
  const track = makeTrack(item, settings);
  track.settled = true;
  tracks.set(item.id, track);
  const resolved = resolveCreated(track.item, settings);
  track.sessionId = resolved.sessionId;
  dispatch(track, createdEventFor(track, resolved.event));
  return track.drainDone;
}

export function onDownloadCreated(
  item: CapturedDownloadItem,
  settings: ExtensionIntegrationSettings | null,
): Promise<void> {
  const existing = tracks.get(item.id);
  if (existing?.settled) return existing.drainDone;
  if (existing?.preHydrate && sessionsReady && settings) return settlePreHydrate(existing, settings);
  if (!sessionsReady) return startPreHydrate(item, settings);
  if (!settings) return Promise.resolve();
  return startMachine(item, settings);
}

export function onDownloadChanged(
  delta: DownloadChangeDelta,
  settings: ExtensionIntegrationSettings | null,
): void {
  const track = tracks.get(delta.id);
  if (!track) return;
  if (settings) track.settings = settings;
  track.item = mergeDownloadDelta(track.item, delta);
  track.item = refreshTrackedItem(track.item);
  const state = deltaCurrent(delta.state);
  const paused = deltaCurrent(delta.paused);
  if (state === 'complete') {
    if (track.life.phase === 'pausing' && track.life.sendCount === 0) {
      track.pendingComplete = true;
      return;
    }
    dispatch(track, { type: 'browser-complete' });
    return;
  }
  if (state === 'interrupted') {
    const error = deltaCurrent(delta.error);
    dispatch(track, error === 'USER_CANCELED' ? { type: 'user-cancel' } : { type: 'browser-interrupted' });
    return;
  }
  if (paused === true && track.pauseRequested) {
    // The one onChanged echo of our own pause(). A later paused:true is the user.
    track.pauseRequested = false;
  } else if (paused === true) {
    dispatch(track, { type: 'user-pause' });
    return;
  }
  if (
    paused === false
    && track.life.pauseConfirmed
    && track.life.wePaused
    && !track.resumeRequested
  ) {
    dispatch(track, { type: 'user-resume' });
    return;
  }
  if (delta.filename) {
    const filename = deltaCurrent(delta.filename);
    const shelf = sanitizeSuggestedFilename(filename);
    if (shelf) track.item = { ...track.item, chromeDeterminedFilename: shelf };
    publish(track, 'name');
    return;
  }
  if (delta.totalBytes || delta.fileSize) {
    publish(track, 'bytes');
  }
}

export function flushPendingFilenameHint(url: string, settings: ExtensionIntegrationSettings | null): void {
  if (!settings) return;
  const key = normalizeCaptureUrl(url);
  for (const track of tracks.values()) {
    const itemUrls = [track.item.finalUrl, track.item.url]
      .filter((value): value is string => Boolean(value))
      .map(normalizeCaptureUrl);
    if (!itemUrls.includes(key)) continue;
    track.settings = settings;
    publish(track, 'hint');
  }
}

export function onChromiumHeadersReceived(
  details: ChromiumHeadersReceivedDetails,
  settings: ExtensionIntegrationSettings | null,
): void {
  rememberResourceType(details.url, details.type);
  rememberResponseFilenameHint(details);
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
  applyDeterminedFilename(item, suggest);
  const track = tracks.get(item.id);
  if (!track) return;
  if (settings) track.settings = settings;
  const shelf = sanitizeSuggestedFilename(item.filename);
  if (shelf) track.item = { ...track.item, chromeDeterminedFilename: shelf };
  publish(track, 'determined');
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
    case 'disconnected':
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
