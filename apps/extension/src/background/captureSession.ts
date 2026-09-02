/**
 * Capture family sessions. Replace 15s URL maps so an ask-mode prompt (5 min)
 * and its dismiss/restore cannot be recaptured by a retry or Drive 302.
 *
 * Identity is URL + session-gateway + Firefox requestId. Filename is stored
 * on the family but is never enough to match a new download by itself.
 */
import {
  CAPTURE_SESSION_TTL_MS,
  downloadCreatedAction,
  normalizeCaptureUrl,
  type DownloadItemLike,
} from './captureFilter';
import type { ExtensionIntegrationSettings } from '@rusticdl/protocol';

export type CapturePhase = 'pending' | 'handoff' | 'restoring' | 'accepted';

export type CaptureFamilyProbe = {
  urls?: Array<string | undefined>;
  requestId?: string;
  sessionUrl?: string;
  filename?: string;
};

export type CaptureSession = {
  id: string;
  phase: CapturePhase;
  urls: string[];
  requestIds: string[];
  filename?: string;
  createdAt: number;
  updatedAt: number;
  expiresAt: number;
};

export type CaptureSessionStore = {
  sessions: CaptureSession[];
};

export type ClaimResult =
  | { ok: true; session: CaptureSession }
  | { ok: false; session: CaptureSession };

export type CreatedAction = 'ignore' | 'skip-restore' | 'erase-ghost' | 'wait' | 'handoff';

export type FirefoxCandidateAction = 'handoff' | 'cancel-ghost' | 'allow';

export const CAPTURE_SESSIONS_STORAGE_KEY = 'rusticdl.capture-sessions';

export function createCaptureSessionStore(): CaptureSessionStore {
  return { sessions: [] };
}

export function createSessionId(now = Date.now()): string {
  return `cap-${now}-${Math.random().toString(36).slice(2, 10)}`;
}

export function pruneCaptureSessions(store: CaptureSessionStore, now = Date.now()): void {
  store.sessions = store.sessions.filter((session) => session.expiresAt > now);
}

function normalizedUrlsOf(probe: CaptureFamilyProbe): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const value of [...(probe.urls ?? []), probe.sessionUrl]) {
    if (!value) continue;
    const key = normalizeCaptureUrl(value);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    out.push(key);
  }
  return out;
}

function sessionMatchesProbe(session: CaptureSession, probe: CaptureFamilyProbe): boolean {
  if (probe.requestId && session.requestIds.includes(probe.requestId)) {
    return true;
  }
  const urls = normalizedUrlsOf(probe);
  return urls.some((url) => session.urls.includes(url));
}

export function findCaptureSession(
  store: CaptureSessionStore,
  probe: CaptureFamilyProbe,
  now = Date.now(),
): CaptureSession | undefined {
  pruneCaptureSessions(store, now);
  return store.sessions.find((session) => sessionMatchesProbe(session, probe));
}

export function attachToSession(
  store: CaptureSessionStore,
  sessionId: string,
  probe: CaptureFamilyProbe,
  now = Date.now(),
): CaptureSession | undefined {
  const session = store.sessions.find((entry) => entry.id === sessionId);
  if (!session) return undefined;
  for (const url of normalizedUrlsOf(probe)) {
    if (!session.urls.includes(url)) session.urls.push(url);
  }
  if (probe.requestId && !session.requestIds.includes(probe.requestId)) {
    session.requestIds.push(probe.requestId);
  }
  if (probe.filename) session.filename = probe.filename;
  session.updatedAt = now;
  return session;
}

/** Find a family and remember this hop (Drive/CDN 302, new request URL). */
export function followCaptureFamily(
  store: CaptureSessionStore,
  probe: CaptureFamilyProbe,
  now = Date.now(),
): CaptureSession | undefined {
  const session = findCaptureSession(store, probe, now);
  if (!session) return undefined;
  attachToSession(store, session.id, probe, now);
  return session;
}

function newSession(
  probe: CaptureFamilyProbe,
  phase: CapturePhase,
  now: number,
  id: string,
): CaptureSession {
  return {
    id,
    phase,
    urls: normalizedUrlsOf(probe),
    requestIds: probe.requestId ? [probe.requestId] : [],
    filename: probe.filename,
    createdAt: now,
    updatedAt: now,
    expiresAt: now + CAPTURE_SESSION_TTL_MS,
  };
}

function claimOrCreate(
  store: CaptureSessionStore,
  probe: CaptureFamilyProbe,
  phase: 'pending' | 'handoff',
  now: number,
  idFactory: () => string,
): ClaimResult {
  const existing = followCaptureFamily(store, probe, now);
  if (existing) {
    if (existing.phase === 'pending' && phase === 'handoff') {
      existing.phase = 'handoff';
      existing.updatedAt = now;
      existing.expiresAt = now + CAPTURE_SESSION_TTL_MS;
      return { ok: true, session: existing };
    }
    return { ok: false, session: existing };
  }
  const session = newSession(probe, phase, now, idFactory());
  store.sessions.push(session);
  return { ok: true, session };
}

export function beginPending(
  store: CaptureSessionStore,
  probe: CaptureFamilyProbe,
  now = Date.now(),
  idFactory: () => string = createSessionId,
): ClaimResult {
  return claimOrCreate(store, probe, 'pending', now, idFactory);
}

export function beginHandoff(
  store: CaptureSessionStore,
  probe: CaptureFamilyProbe,
  now = Date.now(),
  idFactory: () => string = createSessionId,
): ClaimResult {
  return claimOrCreate(store, probe, 'handoff', now, idFactory);
}

export function finishHandoff(
  store: CaptureSessionStore,
  sessionId: string,
  outcome: 'accepted' | 'rejected',
  now = Date.now(),
): CaptureSession | undefined {
  const session = store.sessions.find((entry) => entry.id === sessionId);
  if (!session) return undefined;
  session.phase = outcome === 'accepted' ? 'accepted' : 'restoring';
  session.updatedAt = now;
  session.expiresAt = now + CAPTURE_SESSION_TTL_MS;
  return session;
}

export function dropCaptureSession(store: CaptureSessionStore, sessionId: string): void {
  store.sessions = store.sessions.filter((session) => session.id !== sessionId);
}

export function shouldEraseGhostSession(
  store: CaptureSessionStore,
  probe: CaptureFamilyProbe,
  now = Date.now(),
): CaptureSession | undefined {
  const session = findCaptureSession(store, probe, now);
  if (!session) return undefined;
  switch (session.phase) {
    case 'restoring':
      return undefined;
    case 'pending':
    case 'handoff':
    case 'accepted':
      return session;
    default: {
      const _exhaustive: never = session.phase;
      return _exhaustive;
    }
  }
}

function actionForBlockedPhase(phase: CapturePhase): CreatedAction {
  switch (phase) {
    case 'restoring':
      return 'skip-restore';
    case 'pending':
    case 'handoff':
    case 'accepted':
      return 'erase-ghost';
    default: {
      const _exhaustive: never = phase;
      return _exhaustive;
    }
  }
}

export function probeFromDownload(
  item: Pick<DownloadItemLike, 'url' | 'finalUrl' | 'filename'>,
  extra: { requestId?: string; sessionUrl?: string } = {},
): CaptureFamilyProbe {
  return {
    urls: [item.url, item.finalUrl, extra.sessionUrl],
    requestId: extra.requestId,
    sessionUrl: extra.sessionUrl,
    filename: item.filename,
  };
}

/** Look up an existing family without opening a new session (sync pause path). */
export function peekCreatedAction(
  store: CaptureSessionStore,
  item: DownloadItemLike & { state?: string },
  settings: ExtensionIntegrationSettings,
  extra: { requestId?: string; sessionUrl?: string } = {},
  now = Date.now(),
): CreatedAction {
  const probe = probeFromDownload(item, extra);
  if (item.byExtensionId) {
    followCaptureFamily(store, probe, now);
    return 'skip-restore';
  }
  const existing = followCaptureFamily(store, probe, now);
  if (existing) {
    return actionForBlockedPhase(existing.phase);
  }
  const action = downloadCreatedAction(item, settings);
  switch (action) {
    case 'ignore':
      return 'ignore';
    case 'wait':
      return 'wait';
    case 'capture':
      return 'handoff';
    default: {
      const _exhaustive: never = action;
      return _exhaustive;
    }
  }
}

export function decideCreatedAction(
  store: CaptureSessionStore,
  item: DownloadItemLike & { state?: string },
  settings: ExtensionIntegrationSettings,
  extra: { requestId?: string; sessionUrl?: string } = {},
  now = Date.now(),
): CreatedAction {
  const peek = peekCreatedAction(store, item, settings, extra, now);
  if (peek !== 'wait' && peek !== 'handoff') {
    return peek;
  }
  const probe = probeFromDownload(item, extra);
  if (peek === 'wait') {
    const claim = beginPending(store, probe, now);
    return claim.ok ? 'wait' : actionForBlockedPhase(claim.session.phase);
  }
  const claim = beginHandoff(store, probe, now);
  return claim.ok ? 'handoff' : actionForBlockedPhase(claim.session.phase);
}

export function decideFirefoxCandidateAction(
  store: CaptureSessionStore,
  probe: CaptureFamilyProbe,
  now = Date.now(),
): FirefoxCandidateAction {
  const existing = followCaptureFamily(store, probe, now);
  if (existing) {
    return existing.phase === 'restoring' ? 'allow' : 'cancel-ghost';
  }
  const claim = beginHandoff(store, probe, now);
  if (claim.ok) return 'handoff';
  return claim.session.phase === 'restoring' ? 'allow' : 'cancel-ghost';
}

function isCaptureSession(value: unknown): value is CaptureSession {
  if (!value || typeof value !== 'object') return false;
  const raw = value as Partial<CaptureSession>;
  if (typeof raw.id !== 'string' || typeof raw.phase !== 'string') return false;
  if (raw.phase !== 'pending' && raw.phase !== 'handoff' && raw.phase !== 'restoring' && raw.phase !== 'accepted') {
    return false;
  }
  if (!Array.isArray(raw.urls) || !Array.isArray(raw.requestIds)) return false;
  if (typeof raw.createdAt !== 'number' || typeof raw.expiresAt !== 'number') return false;
  return true;
}

export function sessionsFromStorageValue(value: unknown, now = Date.now()): CaptureSessionStore {
  const store = createCaptureSessionStore();
  if (!Array.isArray(value)) return store;
  for (const raw of value) {
    if (!isCaptureSession(raw)) continue;
    store.sessions.push({
      id: raw.id,
      phase: raw.phase,
      urls: raw.urls.filter((url): url is string => typeof url === 'string').map(normalizeCaptureUrl),
      requestIds: raw.requestIds.filter((id): id is string => typeof id === 'string'),
      filename: typeof raw.filename === 'string' ? raw.filename : undefined,
      createdAt: raw.createdAt,
      updatedAt: typeof raw.updatedAt === 'number' ? raw.updatedAt : raw.createdAt,
      expiresAt: raw.expiresAt,
    });
  }
  pruneCaptureSessions(store, now);
  return store;
}

export function sessionsToStorageValue(store: CaptureSessionStore): CaptureSession[] {
  return store.sessions.map((session) => ({
    ...session,
    urls: [...session.urls],
    requestIds: [...session.requestIds],
  }));
}
