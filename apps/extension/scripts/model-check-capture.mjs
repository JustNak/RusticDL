/**
 * Exhaustive check of the real Chromium capture transition table.
 * A mutant (CAPTURE_MODEL_MUTANT=erase-from-sent) must exit non-zero.
 * Run: node ./scripts/model-check-capture.mjs
 */
import * as esbuild from 'esbuild';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = fileURLToPath(new URL('.', import.meta.url));
const root = fileURLToPath(new URL('..', import.meta.url));
const mutant = process.env.CAPTURE_MODEL_MUTANT || '';

const tmp = mkdtempSync(join(tmpdir(), 'rusticdl-capture-model-'));
const stub = join(tmp, 'browser-stub.cjs');
writeFileSync(stub, 'module.exports = {};\nmodule.exports.default = module.exports;\n');
const outfile = join(tmp, 'captureLifecycle.cjs');
await esbuild.build({
  entryPoints: [join(root, 'src/background/captureLifecycle.ts')],
  bundle: true,
  platform: 'node',
  format: 'cjs',
  outfile,
  plugins: [
    {
      name: 'stub-browser',
      setup(build) {
        build.onResolve({ filter: /^\.\/browser$/ }, () => ({ path: stub, external: true }));
      },
    },
  ],
});

const lifecycle = await import(outfile);
const {
  initialCaptureLifecycle,
  applyCaptureTransition,
  captureTransitionHooks,
  isTerminalCapturePhase,
} = lifecycle;

if (mutant === 'erase-from-sent') {
  captureTransitionHooks.push((state, event) => {
    if (state.phase === 'sent' && event.type === 'erase') {
      return {
        state: { ...state, phase: 'erased', wePaused: false, pauseConfirmed: false },
        effects: [{ type: 'erase' }],
      };
    }
    return null;
  });
} else if (mutant) {
  console.error(`unknown CAPTURE_MODEL_MUTANT=${mutant}`);
  process.exit(1);
}

function stateKey(state) {
  return JSON.stringify([
    state.phase,
    state.role,
    state.wePaused,
    state.pauseConfirmed,
    state.eligible,
    state.browserCompleted,
    state.sendCount,
    state.familyAccepted,
  ]);
}

const waits = [[], ['name', 'size']];
const events = [];
for (const intent of ['pause', 'wait', 'ignore', 'skip-restore', 'ghost']) {
  for (const eligible of [false, true]) {
    for (const familyAccepted of [false, true]) {
      for (const wait of waits) {
        events.push({ type: 'on-created', intent, eligible, familyAccepted, wait });
      }
    }
  }
}
events.push(
  { type: 'pause-ok' },
  { type: 'pause-fail' },
  { type: 'browser-complete' },
  { type: 'browser-interrupted' },
  { type: 'user-pause' },
  { type: 'user-resume' },
  { type: 'user-cancel' },
);
for (const eligible of [false, true]) {
  events.push({ type: 'on-determining-filename', eligible });
  events.push({ type: 'on-changed', change: 'filename', eligible });
  events.push({ type: 'on-changed', change: 'bytes', eligible });
  events.push({ type: 'on-changed', change: 'other', eligible });
  for (const timer of ['name', 'size', 'host']) {
    events.push({ type: 'timer', timer, eligible });
  }
  events.push({ type: 'eligible', eligible });
}
events.push({ type: 'release' });
events.push({ type: 'become-ghost', familyAccepted: false });
events.push({ type: 'become-ghost', familyAccepted: true });
for (const outcome of ['accept', 'reject', 'dismiss', 'disconnect']) {
  events.push({ type: 'host-reply', outcome });
}
events.push({ type: 'erase' }, { type: 'erase-ok' }, { type: 'erase-fail' });
for (const outcome of ['accepted', 'rejected', 'timeout']) {
  events.push({ type: 'family', outcome });
}
events.push({ type: 'hydrate' }, { type: 'hold-lost' }, { type: 'duplicate' });

function edgeViolation(prev, result) {
  const erase = result.effects.some((effect) => effect.type === 'erase');
  if (erase && (prev.phase !== 'accepted' || prev.browserCompleted)) {
    return `erase from ${prev.phase} browserCompleted=${prev.browserCompleted}`;
  }
  const send = result.effects.some((effect) => effect.type === 'send');
  if (send && (prev.sendCount !== 0 || prev.browserCompleted || prev.role !== 'primary')) {
    return `second send from sendCount=${prev.sendCount} role=${prev.role} browserCompleted=${prev.browserCompleted}`;
  }
  if (result.state.sendCount > prev.sendCount + (send ? 1 : 0) || result.state.sendCount > 1) {
    return `sendCount ${prev.sendCount} -> ${result.state.sendCount}`;
  }
  if (result.state.sendCount < prev.sendCount) return 'sendCount decreased';
  if (
    result.state.phase === 'accepted'
    && prev.phase !== 'accepted'
    && (prev.browserCompleted || result.state.browserCompleted)
  ) {
    return 'accepted after the browser completed';
  }
  if (result.state.phase === 'erased' && prev.phase !== 'accepted' && prev.phase !== 'erased') {
    return `erased from ${prev.phase}`;
  }
  if (isTerminalCapturePhase(result.state.phase) && result.state.wePaused) {
    return `terminal ${result.state.phase} left paused by us`;
  }
  return null;
}

function finishes(state) {
  if (!isTerminalCapturePhase(state.phase)) return false;
  if (state.wePaused) return false;
  if (state.phase === 'erased') return !state.browserCompleted;
  return true;
}

const parent = new Map();
const states = new Map();
const queue = [];
const start = initialCaptureLifecycle();
const startKey = stateKey(start);
states.set(startKey, start);
parent.set(startKey, null);
queue.push({ state: start, depth: 0 });

let maxDepth = 0;
let sawSend = false;
let sawLegalErase = false;
let sawCompleteBeforeSend = false;
const STATE_CAP = 8000;

function fail(message, key, event) {
  const steps = [];
  if (event) steps.push(event);
  let cursor = key;
  const seen = new Set();
  while (cursor && parent.get(cursor) && !seen.has(cursor)) {
    seen.add(cursor);
    const step = parent.get(cursor);
    steps.push(step.event);
    cursor = step.from;
  }
  steps.reverse();
  console.error(`COUNTEREXAMPLE ${message}`);
  for (const step of steps) console.error(JSON.stringify(step));
  process.exit(1);
}

while (queue.length > 0) {
  const current = queue.shift();
  maxDepth = Math.max(maxDepth, current.depth);
  const fromKey = stateKey(current.state);
  for (const event of events) {
    const result = applyCaptureTransition(current.state, event);
    const violation = edgeViolation(current.state, result);
    const nextKey = stateKey(result.state);
    if (violation) fail(violation, fromKey, event);
    if (result.effects.some((effect) => effect.type === 'send')) sawSend = true;
    if (result.effects.some((effect) => effect.type === 'erase') && current.state.phase === 'accepted') {
      sawLegalErase = true;
    }
    if (
      event.type === 'browser-complete'
      && current.state.sendCount === 0
      && result.state.phase === 'completed-by-browser'
      && result.state.sendCount === 0
    ) {
      sawCompleteBeforeSend = true;
    }
    if (!states.has(nextKey)) {
      if (states.size >= STATE_CAP) {
        console.error(`COUNTEREXAMPLE state explosion at ${states.size}`);
        process.exit(1);
      }
      states.set(nextKey, result.state);
      parent.set(nextKey, { from: fromKey, event });
      queue.push({ state: result.state, depth: current.depth + 1 });
    }
  }
}

for (const [key, state] of states) {
  let progress = false;
  for (const event of events) {
    const result = applyCaptureTransition(state, event);
    const moved = stateKey(result.state) !== key
      || result.effects.some((effect) => effect.type === 'send' || effect.type === 'erase' || effect.type === 'resume' || effect.type === 'pause');
    if (moved) {
      progress = true;
      break;
    }
  }
  if (!progress && !(isTerminalCapturePhase(state.phase) && !state.wePaused)) {
    fail(`sink with no progress phase=${state.phase} wePaused=${state.wePaused}`, key, { type: 'duplicate' });
  }
  if (state.wePaused && !finishes(state)) {
    let escape = false;
    const seen = new Set([key]);
    const pending = [state];
    while (pending.length > 0 && !escape) {
      const cursor = pending.shift();
      for (const event of events) {
        const result = applyCaptureTransition(cursor, event);
        if (finishes(result.state) || result.state.sendCount > cursor.sendCount) {
          escape = true;
          break;
        }
        const next = stateKey(result.state);
        if (!seen.has(next)) {
          seen.add(next);
          pending.push(result.state);
        }
      }
    }
    if (!escape) fail(`paused-by-us state cannot reach a browser-kept or erased end phase=${state.phase}`, key, null);
  }
}

if (!sawSend || !sawLegalErase || !sawCompleteBeforeSend) {
  console.error(`COUNTEREXAMPLE vacuous sawSend=${sawSend} sawLegalErase=${sawLegalErase} sawCompleteBeforeSend=${sawCompleteBeforeSend}`);
  process.exit(1);
}

console.log(`states explored=${states.size} bound=${maxDepth} events=${events.length}`);
console.log('MODEL-CHECK OK');

if (!mutant) {
  const child = spawnSync(process.execPath, [fileURLToPath(import.meta.url)], {
    env: { ...process.env, CAPTURE_MODEL_MUTANT: 'erase-from-sent' },
    encoding: 'utf8',
  });
  const output = `${child.stdout || ''}${child.stderr || ''}`;
  if (child.status === 0 || !output.includes('COUNTEREXAMPLE')) {
    console.error(`mutant erase-from-sent was not detected status=${child.status}`);
    console.error(output);
    process.exit(1);
  }
  console.log(`MUTANT erase-from-sent exit=${child.status}`);
  const counter = output.split('\n').find((line) => line.startsWith('COUNTEREXAMPLE'));
  console.log(counter);
}

rmSync(tmp, { recursive: true, force: true });
