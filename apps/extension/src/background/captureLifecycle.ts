/**
 * Chromium download-capture hand-off as one transition table.
 *
 * Raw Chrome for Testing 156 (Playwright chromium-1248), measured by
 * e2e/raw-chrome-pause-determining.mjs: pause() inside onCreated does not
 * suppress onDeterminingFilename (`app.zip`, createdToDeterminingMs 0–1).
 * A 64 KiB body that is already fully received still reaches `complete`
 * about 1668 ms later with `paused === true`. Under Playwright the same
 * binary completes about 8 ms after pause, because the profile disables
 * safebrowsing downloads. Send only after a search shows `in_progress`
 * and `paused`, and never after `complete`.
 *
 * Erase/cancel effects are produced only from phase `accepted`.
 * The downloads.pause/resume/cancel/removeFile/erase calls live in this file.
 */
import browser from './browser';

export type CapturePhase =
  | 'created'
  | 'pausing'
  | 'paused'
  | 'naming'
  | 'sent'
  | 'accepted'
  | 'erased'
  | 'rejected'
  | 'timeout'
  | 'user-cancelled'
  | 'user-paused'
  | 'interrupted'
  | 'completed-by-browser'
  | 'resumed'
  | 'left';

export type CaptureRole = 'primary' | 'ghost';

export type CaptureLifecycle = {
  phase: CapturePhase;
  role: CaptureRole;
  wePaused: boolean;
  pauseConfirmed: boolean;
  eligible: boolean;
  browserCompleted: boolean;
  sendCount: number;
  familyAccepted: boolean;
};

export type CaptureEvent =
  | {
    type: 'on-created';
    intent: 'pause' | 'wait' | 'ignore' | 'skip-restore' | 'ghost';
    eligible: boolean;
    familyAccepted: boolean;
    wait: Array<'name' | 'size'>;
  }
  | { type: 'pause-ok' }
  | { type: 'pause-fail' }
  | { type: 'browser-complete' }
  | { type: 'browser-interrupted' }
  | { type: 'user-pause' }
  | { type: 'user-resume' }
  | { type: 'user-cancel' }
  | { type: 'on-determining-filename'; eligible: boolean }
  | { type: 'on-changed'; change: 'filename' | 'bytes' | 'other'; eligible: boolean }
  | { type: 'timer'; timer: 'name' | 'size' | 'host'; eligible: boolean }
  | { type: 'eligible'; eligible: boolean }
  | { type: 'release' }
  | { type: 'become-ghost'; familyAccepted: boolean }
  | { type: 'host-reply'; outcome: 'accept' | 'reject' | 'dismiss' | 'disconnect' }
  | { type: 'erase' }
  | { type: 'erase-ok' }
  | { type: 'erase-fail' }
  | { type: 'family'; outcome: 'accepted' | 'rejected' | 'timeout' }
  | { type: 'hydrate' }
  | { type: 'hold-lost' }
  | { type: 'duplicate' };

export type CaptureEffect =
  | { type: 'pause' }
  | { type: 'resume' }
  | { type: 'send' }
  | { type: 'erase' }
  | { type: 'schedule-timer'; timer: 'name' | 'size' | 'host' }
  | { type: 'clear-timers' }
  | { type: 'log'; event: string };

export type CaptureTransitionResult = {
  state: CaptureLifecycle;
  effects: CaptureEffect[];
};

export function initialCaptureLifecycle(): CaptureLifecycle {
  return {
    phase: 'created',
    role: 'primary',
    wePaused: false,
    pauseConfirmed: false,
    eligible: false,
    browserCompleted: false,
    sendCount: 0,
    familyAccepted: false,
  };
}

/** Test/mutant seam. A hook that returns a result replaces the table for that event. */
export const captureTransitionHooks: Array<
  (state: CaptureLifecycle, event: CaptureEvent) => CaptureTransitionResult | null
> = [];

const captureTransitionTrace: string[] = [];

export function readCaptureTransitionTrace(): readonly string[] {
  return captureTransitionTrace;
}

export function resetCaptureTransitionTrace(): void {
  captureTransitionTrace.length = 0;
}

export function resetCaptureTransitionHooks(): void {
  captureTransitionHooks.length = 0;
}

export function isTerminalCapturePhase(phase: CapturePhase): boolean {
  switch (phase) {
    case 'erased':
    case 'rejected':
    case 'timeout':
    case 'user-cancelled':
    case 'user-paused':
    case 'interrupted':
    case 'completed-by-browser':
    case 'resumed':
    case 'left':
      return true;
    case 'created':
    case 'pausing':
    case 'paused':
    case 'naming':
    case 'sent':
    case 'accepted':
      return false;
    default: {
      const _exhaustive: never = phase;
      return _exhaustive;
    }
  }
}

function noop(state: CaptureLifecycle): CaptureTransitionResult {
  return { state, effects: [] };
}

function failClosed(state: CaptureLifecycle, event: CaptureEvent): CaptureTransitionResult {
  const effects: CaptureEffect[] = [{ type: 'log', event: event.type }];
  if (isTerminalCapturePhase(state.phase)) return { state, effects };
  effects.push({ type: 'clear-timers' });
  if (state.wePaused && state.pauseConfirmed && !state.browserCompleted) {
    effects.push({ type: 'resume' });
  }
  return {
    state: {
      ...state,
      phase: 'left',
      wePaused: false,
      pauseConfirmed: false,
    },
    effects,
  };
}

function releaseHold(state: CaptureLifecycle, phase: CapturePhase): CaptureTransitionResult {
  const effects: CaptureEffect[] = [{ type: 'clear-timers' }];
  if (state.wePaused && state.pauseConfirmed && !state.browserCompleted && phase !== 'user-paused' && phase !== 'user-cancelled' && phase !== 'interrupted' && phase !== 'completed-by-browser') {
    effects.push({ type: 'resume' });
  }
  return {
    state: {
      ...state,
      phase,
      wePaused: false,
      pauseConfirmed: false,
    },
    effects,
  };
}

function armSend(state: CaptureLifecycle): CaptureTransitionResult {
  if (
    state.role !== 'primary'
    || state.sendCount > 0
    || state.browserCompleted
    || !state.pauseConfirmed
    || !state.eligible
    || isTerminalCapturePhase(state.phase)
    || state.phase === 'accepted'
    || state.phase === 'sent'
  ) {
    return noop(state);
  }
  return {
    state: { ...state, phase: 'sent', sendCount: state.sendCount + 1 },
    effects: [
      { type: 'clear-timers' },
      { type: 'schedule-timer', timer: 'host' },
      { type: 'send' },
    ],
  };
}

function applyEligible(state: CaptureLifecycle, eligible: boolean): CaptureTransitionResult {
  const next: CaptureLifecycle = { ...state, eligible };
  if (isTerminalCapturePhase(next.phase) || next.role === 'ghost' || next.browserCompleted || next.sendCount > 0) {
    return noop(next);
  }
  if (next.phase === 'sent' || next.phase === 'accepted') return noop(next);
  // Ineligible does not release. The coordinator sends `release` when the
  // wait is over and the download must stay in the browser, so a missing
  // resource type cannot resume the file before that signal arrives.
  if (!eligible) return noop(next);
  if (!next.pauseConfirmed) {
    if (next.phase === 'pausing') return noop(next);
    return {
      state: { ...next, phase: 'pausing', wePaused: true },
      effects: [{ type: 'pause' }],
    };
  }
  if (next.phase === 'paused' || next.phase === 'naming' || next.phase === 'pausing') {
    return armSend({ ...next, phase: 'paused' });
  }
  return noop(next);
}

function onCreated(state: CaptureLifecycle, event: Extract<CaptureEvent, { type: 'on-created' }>): CaptureTransitionResult {
  if (state.phase !== 'created') return noop(state);
  switch (event.intent) {
    case 'ignore':
    case 'skip-restore':
      return {
        state: { ...state, phase: 'left', eligible: false },
        effects: state.wePaused ? [{ type: 'resume' }, { type: 'clear-timers' }] : [{ type: 'clear-timers' }],
      };
    case 'ghost':
      if (event.familyAccepted) {
        return {
          state: { ...state, role: 'ghost', familyAccepted: true, phase: 'accepted' },
          effects: [],
        };
      }
      return {
        state: { ...state, role: 'ghost', familyAccepted: false, phase: 'pausing', wePaused: true },
        effects: [{ type: 'pause' }],
      };
    case 'wait':
      return {
        state: { ...state, phase: 'naming', eligible: event.eligible },
        effects: event.wait.map((timer) => ({ type: 'schedule-timer' as const, timer })),
      };
    case 'pause':
      return {
        state: {
          ...state,
          phase: 'pausing',
          wePaused: true,
          eligible: event.eligible,
        },
        effects: [
          { type: 'pause' },
          ...event.wait.map((timer) => ({ type: 'schedule-timer' as const, timer })),
        ],
      };
    default: {
      const _exhaustive: never = event.intent;
      return _exhaustive;
    }
  }
}

function pauseOk(state: CaptureLifecycle): CaptureTransitionResult {
  if (state.browserCompleted || state.phase === 'completed-by-browser') {
    return {
      state: { ...state, phase: 'completed-by-browser', browserCompleted: true, wePaused: false, pauseConfirmed: false },
      effects: [{ type: 'clear-timers' }],
    };
  }
  if (isTerminalCapturePhase(state.phase)) return noop(state);
  if (state.phase === 'accepted' || state.phase === 'sent') {
    return noop({ ...state, pauseConfirmed: true, wePaused: true });
  }
  const held: CaptureLifecycle = {
    ...state,
    pauseConfirmed: true,
    wePaused: true,
    phase: 'paused',
  };
  if (held.role === 'ghost') {
    if (held.familyAccepted) return { state: { ...held, phase: 'accepted' }, effects: [] };
    return noop(held);
  }
  if (held.eligible) return armSend(held);
  return noop(held);
}

function hostReply(state: CaptureLifecycle, outcome: 'accept' | 'reject' | 'dismiss' | 'disconnect'): CaptureTransitionResult {
  if (state.phase !== 'sent' || state.browserCompleted || state.sendCount < 1) {
    if (isTerminalCapturePhase(state.phase)) return noop(state);
    return failClosed(state, { type: 'host-reply', outcome });
  }
  switch (outcome) {
    case 'accept':
      return {
        state: { ...state, phase: 'accepted' },
        effects: [{ type: 'clear-timers' }],
      };
    case 'reject':
    case 'dismiss':
    case 'disconnect':
      return releaseHold(state, 'rejected');
    default: {
      const _exhaustive: never = outcome;
      return _exhaustive;
    }
  }
}

/**
 * `(state, event) -> next state + effects`.
 * Hooks run first so a model mutant can add an edge in memory.
 */
export function applyCaptureTransition(
  state: CaptureLifecycle,
  event: CaptureEvent,
): CaptureTransitionResult {
  captureTransitionTrace.push(event.type);
  for (const hook of captureTransitionHooks) {
    const hooked = hook(state, event);
    if (hooked) return hooked;
  }
  return captureTransition(state, event);
}

function captureTransition(state: CaptureLifecycle, event: CaptureEvent): CaptureTransitionResult {
  if (state.phase === 'created' && event.type !== 'on-created' && event.type !== 'hydrate') {
    return noop(state);
  }
  switch (event.type) {
    case 'on-created':
      return onCreated(state, event);
    case 'pause-ok':
      return pauseOk(state);
    case 'pause-fail':
      if (isTerminalCapturePhase(state.phase) || state.phase === 'sent' || state.phase === 'accepted') {
        return noop(state);
      }
      return {
        state: { ...state, phase: 'left', wePaused: false, pauseConfirmed: false, eligible: false },
        effects: state.wePaused
          ? [{ type: 'clear-timers' }, { type: 'resume' }]
          : [{ type: 'clear-timers' }],
      };
    case 'browser-complete':
      if (state.phase === 'erased') return noop({ ...state, browserCompleted: true });
      return {
        state: {
          ...state,
          browserCompleted: true,
          phase: 'completed-by-browser',
          wePaused: false,
          pauseConfirmed: false,
        },
        effects: [{ type: 'clear-timers' }],
      };
    case 'browser-interrupted':
      if (isTerminalCapturePhase(state.phase) || state.phase === 'erased') return noop(state);
      return {
        state: { ...state, phase: 'interrupted', wePaused: false, pauseConfirmed: false },
        effects: [{ type: 'clear-timers' }],
      };
    case 'user-cancel':
      if (isTerminalCapturePhase(state.phase)) return noop(state);
      return {
        state: { ...state, phase: 'user-cancelled', wePaused: false, pauseConfirmed: false },
        effects: [{ type: 'clear-timers' }],
      };
    case 'user-pause':
      if (isTerminalCapturePhase(state.phase) || state.phase === 'sent' || state.phase === 'accepted' || state.phase === 'pausing') {
        return noop(state);
      }
      return {
        state: { ...state, phase: 'user-paused', wePaused: false, pauseConfirmed: false },
        effects: [{ type: 'clear-timers' }],
      };
    case 'user-resume':
      if (state.phase === 'user-paused') return releaseHold(state, 'resumed');
      if (state.phase === 'sent' || state.phase === 'paused' || state.phase === 'naming' || state.phase === 'pausing') {
        return releaseHold({ ...state, pauseConfirmed: false }, 'resumed');
      }
      return noop(state);
    case 'on-determining-filename':
    case 'eligible':
      return applyEligible(state, event.eligible);
    case 'on-changed':
      if (event.change === 'other') return noop(state);
      return applyEligible(state, event.eligible);
    case 'timer':
      if (event.timer === 'host') {
        if (state.phase !== 'sent') return noop(state);
        return releaseHold(state, 'timeout');
      }
      if (state.phase === 'sent' || state.sendCount > 0 || isTerminalCapturePhase(state.phase)) {
        return noop(state);
      }
      return applyEligible(state, event.eligible);
    case 'release':
      if (isTerminalCapturePhase(state.phase) || state.phase === 'accepted' || state.phase === 'erased') {
        return noop(state);
      }
      if (state.phase === 'sent') return releaseHold(state, 'left');
      return releaseHold(state, 'left');
    case 'become-ghost':
      if (state.browserCompleted || state.phase === 'erased' || state.phase === 'completed-by-browser') {
        return noop({ ...state, role: 'ghost', familyAccepted: event.familyAccepted });
      }
      if (event.familyAccepted) {
        return { state: { ...state, role: 'ghost', familyAccepted: true, phase: 'accepted', eligible: false }, effects: [] };
      }
      if (state.pauseConfirmed) {
        return noop({ ...state, role: 'ghost', familyAccepted: false, phase: 'paused', eligible: false });
      }
      return {
        state: { ...state, role: 'ghost', familyAccepted: false, phase: 'pausing', wePaused: true, eligible: false },
        effects: [{ type: 'pause' }],
      };
    case 'host-reply':
      return hostReply(state, event.outcome);
    case 'erase':
      if (state.phase === 'accepted' && !state.browserCompleted) {
        return { state, effects: [{ type: 'erase' }] };
      }
      if (isTerminalCapturePhase(state.phase)) return noop(state);
      return failClosed(state, event);
    case 'erase-ok':
      if (state.phase !== 'accepted') return noop(state);
      return {
        state: { ...state, phase: 'erased', wePaused: false, pauseConfirmed: false },
        effects: [{ type: 'clear-timers' }],
      };
    case 'erase-fail':
      if (state.phase !== 'accepted') return noop(state);
      return { state, effects: [{ type: 'log', event: 'erase-fail' }] };
    case 'family':
      if (state.role !== 'ghost') return noop(state);
      if (state.browserCompleted || state.phase === 'completed-by-browser' || state.phase === 'erased') {
        return noop(state);
      }
      switch (event.outcome) {
        case 'accepted':
          if (isTerminalCapturePhase(state.phase)) return noop(state);
          return { state: { ...state, familyAccepted: true, phase: 'accepted' }, effects: [] };
        case 'rejected':
        case 'timeout':
          if (isTerminalCapturePhase(state.phase)) return noop(state);
          return releaseHold({ ...state, familyAccepted: false }, 'resumed');
        default: {
          const _exhaustive: never = event;
          return _exhaustive;
        }
      }
    case 'hydrate':
      if (state.sendCount > 0 || state.phase === 'sent' || state.phase === 'accepted' || state.phase === 'erased') {
        return noop(state);
      }
      if (state.phase === 'pausing') return { state, effects: [{ type: 'pause' }] };
      if (state.role === 'ghost' && state.familyAccepted && !isTerminalCapturePhase(state.phase)) {
        return { state: { ...state, phase: 'accepted' }, effects: [] };
      }
      if (
        state.role === 'primary'
        && state.eligible
        && state.pauseConfirmed
        && !state.browserCompleted
        && (state.phase === 'paused' || state.phase === 'naming')
      ) {
        return armSend(state);
      }
      return noop(state);
    case 'hold-lost':
      if (state.phase !== 'sent' && state.phase !== 'pausing' && state.phase !== 'paused') return noop(state);
      if (state.browserCompleted) {
        return {
          state: { ...state, phase: 'completed-by-browser', wePaused: false, pauseConfirmed: false },
          effects: [{ type: 'clear-timers' }],
        };
      }
      return releaseHold(state, 'resumed');
    case 'duplicate':
      return noop(state);
    default: {
      const _exhaustive: never = event;
      return _exhaustive;
    }
  }
}

export async function pauseDownload(id: number): Promise<void> {
  await browser.downloads.pause(id);
}

export async function resumeDownload(id: number): Promise<void> {
  await browser.downloads.resume(id);
}

/** Cancel, delete the partial file, and erase the shelf row. True when erase removed it. */
export async function eraseDownload(id: number): Promise<boolean> {
  try {
    await browser.downloads.cancel(id);
  } catch {
    // already finished or canceled
  }
  try {
    await browser.downloads.removeFile(id);
  } catch {
    // no dest file yet
  }
  for (let attempt = 0; attempt < 4; attempt += 1) {
    try {
      const erased = await browser.downloads.erase({ id });
      if (erased.length > 0) return true;
    } catch {
      // shelf row not ready
    }
    await new Promise((resolve) => setTimeout(resolve, 40 * (attempt + 1)));
  }
  return false;
}
