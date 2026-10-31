#!/usr/bin/env node
/**
 * Test-only EXT_ATTACK hook for the mock native host and fixture server.
 * Never imported by product code (apps/extension/src, packages/, src/).
 *
 * Host attacks (mock-native-host.mjs): g3-other, g5-evil, g5-super, g5-prefix,
 * exit-after-log, exit-before-log, wrong-shape, delay-c1, inject-postbody,
 * fail-c2-N, kill-immediate.
 * Fixture attacks: n1-noop, cookie-tamper, inject-late-c1.
 */
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';

export const EXPECTED_POST_BODY = 'post-body-c9';
export const COOKIE_NAME = 'rusticdl_e2e';

export function attackName() {
  return process.env.EXT_ATTACK || '';
}

export function attackDelayMs() {
  const raw = Number(process.env.EXT_ATTACK_DELAY_MS || '15000');
  return Number.isFinite(raw) && raw >= 0 ? raw : 15_000;
}

export function cookieValueForNonce(nonce) {
  if (attackName() === 'cookie-tamper') return `${nonce}-tampered`;
  return nonce;
}

export function n1IsNoop() {
  return attackName() === 'n1-noop';
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

function c2CountPath(runDir) {
  return `${runDir}/c2prompt.count`;
}

function bumpC2Count(runDir) {
  const path = c2CountPath(runDir);
  let n = 0;
  if (existsSync(path)) {
    n = Number(readFileSync(path, 'utf8').trim()) || 0;
  }
  n += 1;
  writeFileSync(path, `${n}\n`);
  return n;
}

function failC2Nth() {
  const name = attackName();
  const match = /^fail-c2-(\d+)$/.exec(name);
  return match ? Number(match[1]) : null;
}

function mutatePayload(request, patch) {
  return {
    ...request,
    payload: { ...payloadOf(request), ...patch },
  };
}

/**
 * Apply a host-side attack. Returns true when the mock should skip its default
 * log+reply path (the hook already logged and/or replied, or exited).
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
    const count = bumpC2Count(runDir);
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

  if (attack === 'delay-c1' && request.type === 'prompt_download' && pathnameOf(url) === '/C1') {
    sleepMs(attackDelayMs());
    const reply = respond(request);
    logRequest(request, Date.now(), reply);
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

export function lateC1LogLine(fileUrl, receivedAt) {
  return {
    receivedAt,
    protocolVersion: 1,
    requestId: 'ext-attack-inject-late-c1',
    type: 'prompt_download',
    payload: {
      url: fileUrl,
      suggestedFilename: 'app.zip',
      source: { entryPoint: 'browser_download' },
    },
  };
}

export function scheduleLateC1Inject(runDir, fileUrl) {
  if (attackName() !== 'inject-late-c1') return;
  const delay = attackDelayMs();
  const logPath = `${runDir}/host-log.jsonl`;
  mkdirSync(dirname(logPath), { recursive: true });
  setTimeout(() => {
    const line = lateC1LogLine(fileUrl, Date.now());
    appendFileSync(logPath, `${JSON.stringify(line)}\n`, 'utf8');
  }, delay);
}
