#!/usr/bin/env node
/**
 * Test-only EXT_ATTACK hook for the mock native host and fixture server.
 * Never imported by product code (apps/extension/src, packages/, src/).
 * Attacks can only make a prove run fail closed; they are not a classifier switch.
 */
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';

export const EXPECTED_POST_BODY = 'post-body-c9';
export const COOKIE_NAME = 'rusticdl_e2e';

export function attackName() {
  return process.env.EXT_ATTACK || '';
}

export function attackDelayMs() {
  const raw = Number(process.env.EXT_ATTACK_DELAY_MS || process.env.CHECKER_DELAY_MS || '15000');
  return Number.isFinite(raw) && raw >= 0 ? raw : 15_000;
}

export function cookieValueForNonce(nonce) {
  if (attackName() === 'cookie-tamper') return `${nonce}-tampered`;
  return nonce;
}

export function n1IsNoop() {
  return attackName() === 'n1-noop';
}

export function n1IsFetch() {
  return attackName() === 'n1-fetch';
}

export function n1IsFakeTarget() {
  return attackName() === 'n1-fake-target';
}

export function n1IsWrongPage() {
  return attackName() === 'n1-wrong-page';
}

export function n5Mode() {
  const attack = attackName();
  if (attack === 'n5-noop') return 'noop';
  if (attack === 'n5-fetch' || attack === 'n5-stay') return 'stay';
  if (attack === 'n5-stale' || attack === 'n5-stale-marker') return 'stale';
  if (attack === 'n5-forged') return 'forged';
  if (attack === 'n5-hang') return 'hang';
  if (attack === 'n5-wrong-page') return 'wrong-page';
  if (attack === 'n5-side-download') return 'side-download';
  return 'honest';
}

export function c6IsNoop() {
  return attackName() === 'c6-noop';
}

function payloadOf(request) {
  return request && typeof request.payload === 'object' && request.payload
    ? request.payload
    : {};
}

function payloadUrl(request) {
  const url = payloadOf(request).url;
  return typeof url === 'string' ? url : '';
}

function pathnameOf(url) {
  try {
    return new URL(url).pathname;
  } catch {
    return '';
  }
}

function sleepMs(ms) {
  if (ms <= 0) return;
  const buf = new Int32Array(new SharedArrayBuffer(4));
  Atomics.wait(buf, 0, 0, ms);
}

function countPath(runDir, name) {
  return `${runDir}/${name}`;
}

function bumpCount(runDir, name) {
  const path = countPath(runDir, name);
  let n = 0;
  if (existsSync(path)) {
    n = Number(readFileSync(path, 'utf8').trim()) || 0;
  }
  n += 1;
  writeFileSync(path, `${n}\n`);
  return n;
}

export function c5PageIsNoop(runDir) {
  const attack = attackName();
  if (attack === 'c5-noop') return true;
  if (attack !== 'c5-second-noop') return false;
  return bumpCount(runDir, 'c5page.count') >= 2;
}

function failC2Nth() {
  const match = /^fail-c2-(\d+)$/.exec(attackName());
  return match ? Number(match[1]) : null;
}

function mutatePayload(request, patch) {
  return {
    ...request,
    payload: { ...payloadOf(request), ...patch },
  };
}

function note(runDir, text) {
  if (!runDir) return;
  appendFileSync(`${runDir}/ext-attack.log`, `${Date.now()} ${text}\n`);
}

/**
 * Apply a host-side attack. Returns true when the mock should skip its default
 * log+reply path.
 */
export function applyHostAttack({
  request,
  receivedAt,
  runDir,
  logRequest,
  writeMessage,
  respond,
}) {
  const attack = attackName();
  if (!attack) return false;

  if (attack === 'kill-immediate') {
    process.exit(0);
  }

  if (attack === 'exit-before-log' && (request.type === 'prompt_download' || request.type === 'enqueue_download')) {
    process.exit(0);
  }

  const url = payloadUrl(request);
  const nth = failC2Nth();
  if (nth != null && request.type === 'prompt_download' && pathnameOf(url) === '/C2') {
    const count = bumpCount(runDir, 'c2prompt.count');
    if (count === nth) {
      writeMessage(respond(request));
      return true;
    }
  }

  if (attack === 'g3-other' && url.includes('/C9')) {
    const mutated = mutatePayload(request, {
      url: 'http://127.0.0.1:18765/C2?from=other-case',
      suggestedFilename: 'setup.exe',
    });
    const reply = respond(request);
    logRequest(mutated, receivedAt, reply);
    writeMessage(reply);
    return true;
  }

  if (attack === 'g5-evil' && url.includes('/C10/final')) {
    const mutated = mutatePayload(request, { url: 'http://evil.example/C10/final' });
    const reply = respond(request);
    logRequest(mutated, receivedAt, reply);
    writeMessage(reply);
    return true;
  }

  if (attack === 'g5-super' && url.includes('/C10/final')) {
    const next = new URL(url);
    next.searchParams.set('extra', '1');
    const mutated = mutatePayload(request, { url: next.href });
    const reply = respond(request);
    logRequest(mutated, receivedAt, reply);
    writeMessage(reply);
    return true;
  }

  if (attack === 'g5-prefix' && url.includes('/C10/final')) {
    const next = new URL(url);
    next.pathname = '/C10';
    const mutated = mutatePayload(request, { url: next.href });
    const reply = respond(request);
    logRequest(mutated, receivedAt, reply);
    writeMessage(reply);
    return true;
  }

  if (attack === 'inject-postbody' && url.includes('/C9')) {
    const mutated = mutatePayload(request, { postBody: EXPECTED_POST_BODY });
    const reply = respond(request);
    logRequest(mutated, receivedAt, reply);
    writeMessage(reply);
    return true;
  }

  if (
    (attack === 'host-delay-c1' || attack === 'delay-c1')
    && request.type === 'prompt_download'
    && pathnameOf(url) === '/C1'
  ) {
    note(runDir, `host-delay-c1 sleep ${attackDelayMs()}`);
    sleepMs(attackDelayMs());
    const reply = respond(request);
    logRequest(request, Date.now(), reply);
    writeMessage(reply);
    return true;
  }

  if (attack === 'c10-wrong-shape' && request.type === 'prompt_download' && url.includes('/C10')) {
    const reply = { hello: 'wrong-shape' };
    logRequest(request, receivedAt, reply);
    writeMessage(reply);
    return true;
  }

  if (attack === 'exit-after-log' && (request.type === 'prompt_download' || request.type === 'enqueue_download')) {
    logRequest(request, receivedAt, undefined);
    process.exit(0);
  }

  if (attack === 'wrong-shape' && (request.type === 'prompt_download' || request.type === 'enqueue_download')) {
    const reply = { hello: 'wrong-shape' };
    logRequest(request, receivedAt, reply);
    writeMessage(reply);
    return true;
  }

  return false;
}

export function lateCaptureLine(fileUrl, receivedAt, { filename, requestId }) {
  return {
    receivedAt,
    protocolVersion: 1,
    requestId,
    type: 'prompt_download',
    payload: {
      url: fileUrl,
      suggestedFilename: filename,
      source: { entryPoint: 'browser_download' },
    },
  };
}

export function lateC1LogLine(fileUrl, receivedAt) {
  return lateCaptureLine(fileUrl, receivedAt, {
    filename: 'app.zip',
    requestId: 'ext-attack-late-c1',
  });
}

const scheduledLate = new Set();

function scheduleLateLine(runDir, fileUrl, { filename, requestId, label }) {
  const key = `${runDir}:${label}`;
  if (scheduledLate.has(key)) return;
  scheduledLate.add(key);
  const delay = attackDelayMs();
  const logPath = `${runDir}/host-log.jsonl`;
  mkdirSync(dirname(logPath), { recursive: true });
  note(runDir, `scheduled ${label} delay=${delay} url=${fileUrl}`);
  setTimeout(() => {
    const line = lateCaptureLine(fileUrl, Date.now(), { filename, requestId });
    appendFileSync(logPath, `${JSON.stringify(line)}\n`, 'utf8');
    note(runDir, `late-wrote ${label} receivedAt=${line.receivedAt}`);
  }, delay);
}

export function scheduleLateC1Inject(runDir, fileUrl) {
  const attack = attackName();
  if (attack === 'late-c1' || attack === 'inject-late-c1') {
    scheduleLateLine(runDir, fileUrl, {
      filename: 'app.zip',
      requestId: 'ext-attack-late-c1',
      label: 'late-c1',
    });
    return;
  }
  if (attack === 'late-c1-nullfile') {
    scheduleLateLine(runDir, fileUrl, {
      filename: null,
      requestId: 'ext-attack-late-c1-nullfile',
      label: 'late-c1-nullfile',
    });
    return;
  }
  if (attack === 'end-null') {
    scheduleLateLine(runDir, fileUrl, {
      filename: null,
      requestId: 'ext-attack-end-null',
      label: 'end-null',
    });
  }
}

export function scheduleLateC4Inject(runDir, fileUrl) {
  if (attackName() !== 'late-c4') return;
  scheduleLateLine(runDir, fileUrl, {
    filename: 'get',
    requestId: 'ext-attack-late-c4',
    label: 'late-c4',
  });
}
