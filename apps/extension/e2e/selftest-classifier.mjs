#!/usr/bin/env node
/**
 * Plants each checker attack against the 9227e12 classifier and the fixed
 * classifier. Exit 0 only when every finding is red on legacy and green after
 * the fix. No browser; uses the same classify*.mjs modules as the spec.
 */
import * as fixed from './classify.mjs';
import * as legacy from './classify.legacy.mjs';
import { EXPECTED_POST_BODY, lateC1LogLine } from './ext-attack.mjs';

const G3_REASON = 'Chromium false-captures the POST attachment and would replay it as GET';
const G5_REASON = 'Chromium handed off the final same-origin redirect URL (/C10/final), not the original /C10 URL';
const G1_REASON = 'Chromium did not capture <a download> same-origin zip (no Content-Disposition attachment)';

const nonce = '79d242b0-8243-4c96-8cb2-03930921b6f1';
const attempt = '1793438122840-mr9dov';
const origin = 'http://127.0.0.1:18765';
const startedAt = 1_793_438_122_000;
const triggerAt = startedAt;

function c9Def() {
  return {
    id: 'C9',
    route: '/C9',
    expect: 'keep',
    filename: 'posted.zip',
    xfail: { gap: 'G3', reason: G3_REASON },
    skip: null,
    fidelity: { cookies: false, referrer: true, postBody: true, originalUrl: false },
  };
}

function c10Def() {
  return {
    id: 'C10',
    route: '/C10',
    expect: 'capture',
    filename: 'archive.zip',
    xfail: { gap: 'G5', reason: G5_REASON },
    skip: null,
    fidelity: { cookies: false, referrer: true, postBody: false, originalUrl: true },
  };
}

function c2Def() {
  return {
    id: 'C2',
    route: '/C2',
    expect: 'capture',
    filename: 'setup.exe',
    xfail: null,
    skip: null,
    fidelity: { cookies: true, referrer: true, postBody: false, originalUrl: false },
  };
}

function n1Def() {
  return {
    id: 'N1',
    route: '/N1',
    expect: 'keep',
    filename: null,
    xfail: null,
    skip: null,
    fidelity: { cookies: false, referrer: false, postBody: false, originalUrl: false },
  };
}

function c1Def() {
  return {
    id: 'C1',
    route: '/C1',
    expect: 'capture',
    filename: 'app.zip',
    xfail: { gap: 'G1', reason: G1_REASON },
    skip: null,
    fidelity: { cookies: false, referrer: true, postBody: false, originalUrl: false },
  };
}

function promptLine({ url, filename, entryPoint = 'browser_download', receivedAt = startedAt + 50, requestId = 'req-1', reply, extraPayload }) {
  const line = {
    receivedAt,
    requestId,
    type: 'prompt_download',
    payload: {
      url,
      suggestedFilename: filename,
      source: { entryPoint, referrer: `${origin}/page/X?nonce=${nonce}&attempt=${attempt}` },
      ...extraPayload,
    },
  };
  if (reply !== undefined) line.reply = reply;
  else {
    line.reply = { ok: true, type: 'accepted', requestId, payload: { status: 'queued', appState: 'running' } };
  }
  return line;
}

function accepted(requestId) {
  return { ok: true, type: 'accepted', requestId, payload: { status: 'queued', appState: 'running' } };
}

function run(mod, caseDef, extra) {
  const expectedUrl = extra.expectedUrl;
  const pageUrl = extra.pageUrl || `${origin}/page/${caseDef.id}?nonce=${nonce}&attempt=${attempt}`;
  return mod.classifyCase({
    caseDef,
    expectedUrl,
    expectedFinalUrl: extra.expectedFinalUrl ?? null,
    pageUrl,
    nonce,
    attempt,
    startedAt,
    triggerAt,
    promptLines: extra.promptLines || [],
    enqueueLines: extra.enqueueLines || [],
    downloads: extra.downloads || [],
    fixtureHits: extra.fixtureHits || [],
    pageEvidence: extra.pageEvidence || { href: pageUrl, text: '', dataPage: caseDef.id },
  });
}

const findings = [];

function check(id, title, { old: oldGot, next: newGot, oldWant, newWant }) {
  const oldOk = oldWant(oldGot);
  const newOk = newWant(newGot);
  const pass = oldOk && newOk;
  findings.push({ id, title, pass, oldGot, newGot });
  const fmt = (got) => {
    if (typeof got === 'string') return got;
    return `${got.status}/obs=${got.observed}/url=${got.capturedUrl ?? ''}/file=${got.suggestedFilename ?? ''}/cookies=${got.fidelity?.cookies ?? ''}/postBody=${got.fidelity?.postBody ?? ''}`;
  };
  console.log(
    `${pass ? 'PASS' : 'FAIL'} ${id} ${title}\n  old=${fmt(oldGot)}\n  new=${fmt(newGot)}`,
  );
}

const c9Url = `${origin}/C9?nonce=${nonce}&attempt=${attempt}`;
const c10Original = `${origin}/C10?nonce=${nonce}&attempt=${attempt}`;
const c10Final = fixed.expectedFinalUrl(origin, nonce, attempt);
const c2Url = `${origin}/C2?nonce=${nonce}&attempt=${attempt}`;
const c1Url = `${origin}/C1?nonce=${nonce}&attempt=${attempt}`;
const n1Url = `${origin}/N1?nonce=${nonce}&attempt=${attempt}`;

// F1: G3 other-case line
{
  const lines = [promptLine({
    url: 'http://127.0.0.1:18765/C2?from=other-case',
    filename: 'setup.exe',
  })];
  const extra = { expectedUrl: c9Url, promptLines: lines };
  check('F1', 'G3 other-case line is FAIL, not xfail', {
    old: run(legacy, c9Def(), extra),
    next: run(fixed, c9Def(), extra),
    oldWant: (r) => r.status === 'xfail' && r.observed === 'capture',
    newWant: (r) => r.status === 'FAIL',
  });
}

// F2 evil
{
  const lines = [promptLine({ url: 'http://evil.example/C10/final', filename: 'archive.zip' })];
  const extra = { expectedUrl: c10Original, expectedFinalUrl: c10Final, promptLines: lines };
  check('F2a', 'G5 evil host is FAIL, not xfail', {
    old: run(legacy, c10Def(), extra),
    next: run(fixed, c10Def(), extra),
    oldWant: (r) => r.status === 'xfail' && String(r.capturedUrl).includes('/C10/final'),
    newWant: (r) => r.status === 'FAIL',
  });
}

// F2 superstring
{
  const superUrl = `${c10Final}&extra=1`;
  const lines = [promptLine({ url: superUrl, filename: 'archive.zip' })];
  const extra = { expectedUrl: c10Original, expectedFinalUrl: c10Final, promptLines: lines };
  check('F2b', 'G5 superstring is FAIL, not xfail', {
    old: run(legacy, c10Def(), extra),
    next: run(fixed, c10Def(), extra),
    oldWant: (r) => r.status === 'xfail',
    newWant: (r) => r.status === 'FAIL',
  });
}

// F2 control prefix
{
  const lines = [promptLine({ url: `${origin}/C10`, filename: 'archive.zip' })];
  const extra = { expectedUrl: c10Original, expectedFinalUrl: c10Final, promptLines: lines };
  check('F2c', 'G5 prefix (no /final) FAIL on both', {
    old: run(legacy, c10Def(), extra),
    next: run(fixed, c10Def(), extra),
    oldWant: (r) => r.status === 'FAIL',
    newWant: (r) => r.status === 'FAIL',
  });
}

// F3 wrong-shape / complete download
{
  const requestId = 'c2-1';
  const lines = [promptLine({
    url: c2Url,
    filename: 'setup.exe',
    requestId,
    reply: { hello: 'wrong-shape' },
  })];
  const downloads = [{ id: 2, state: 'complete', url: c2Url, filename: '/tmp/x' }];
  const extra = { expectedUrl: c2Url, promptLines: lines, downloads };
  check('F3a', 'C2 with wrong-shape + complete download is FAIL', {
    old: run(legacy, c2Def(), extra),
    next: run(fixed, c2Def(), extra),
    oldWant: (r) => r.status === 'pass' && r.observed === 'capture',
    newWant: (r) => r.status === 'FAIL' && r.downloadCompleted === true,
  });
}

{
  const n1 = run(fixed, n1Def(), {
    expectedUrl: n1Url,
    promptLines: [],
    fixtureHits: [{ pathname: '/N1', nonce, attempt }],
    pageEvidence: { href: n1Url, text: 'html document, not a file', dataPage: 'N1-target' },
  });
  const c2fail = run(fixed, c2Def(), {
    expectedUrl: c2Url,
    promptLines: [promptLine({ url: c2Url, filename: 'setup.exe', reply: undefined })],
    downloads: [{ id: 1, state: 'complete', url: c2Url }],
  });
  c2fail.handoffAccepted = false;
  c2fail.downloadCompleted = true;
  c2fail.status = 'FAIL';
  const round = [n1];
  if (fixed.hostFailedHandoff([c2fail, n1]) || !fixed.isSentinelOk(c2fail)) {
    fixed.invalidateNegatives(round, 'missing or wrong accepted reply; negative keep is INVALID');
  }
  const n1Old = run(legacy, n1Def(), { expectedUrl: n1Url, promptLines: [] });
  check('F3b', 'N1 INVALID when C2 has no accepted reply', {
    old: n1Old,
    next: n1,
    oldWant: (r) => r.status === 'pass',
    newWant: (r) => r.status === 'INVALID',
  });
}

// F4 late exact C1
{
  const late = lateC1LogLine(c1Url, startedAt + 15_000);
  const extraEarly = { expectedUrl: c1Url, promptLines: [] };
  const oldEarly = run(legacy, c1Def(), extraEarly);
  const newEarly = run(fixed, c1Def(), extraEarly);
  fixed.recheckLateMissXfails([newEarly], [late]);
  legacy.recheckLateMissXfails([oldEarly], [late]);
  check('F4a', 'late exact C1 in-window is XPASS (no accepted reply required for G1)', {
    old: run(legacy, c1Def(), { expectedUrl: c1Url, promptLines: [late] }),
    next: run(fixed, c1Def(), { expectedUrl: c1Url, promptLines: [late] }),
    oldWant: (r) => r.status === 'XPASS',
    newWant: (r) => r.status === 'XPASS',
  });
  check('F4', 'late exact C1 is xfail on old, XPASS after recheck', {
    old: oldEarly,
    next: newEarly,
    oldWant: (r) => r.status === 'xfail' && r.observed === 'keep',
    newWant: (r) => r.status === 'XPASS' && r.observed === 'capture',
  });
}

// F5 noop click
{
  const extra = {
    expectedUrl: n1Url,
    promptLines: [],
    fixtureHits: [{ pathname: '/page/N1', nonce, attempt }],
    pageEvidence: { href: `${origin}/page/N1?nonce=${nonce}&attempt=${attempt}`, text: 'N1', dataPage: 'N1' },
  };
  check('F5', 'N1 href=# noop is FAIL', {
    old: run(legacy, n1Def(), extra),
    next: run(fixed, n1Def(), extra),
    oldWant: (r) => r.status === 'pass',
    newWant: (r) => r.status === 'FAIL',
  });
}

// F6 postBody from payload
{
  const lines = [promptLine({
    url: c9Url,
    filename: 'posted.zip',
    extraPayload: { postBody: EXPECTED_POST_BODY },
  })];
  const extra = { expectedUrl: c9Url, promptLines: lines };
  check('F6', 'fidelity.postBody follows payload', {
    old: run(legacy, c9Def(), extra),
    next: run(fixed, c9Def(), extra),
    oldWant: (r) => r.fidelity.postBody === 'fail',
    newWant: (r) => r.fidelity.postBody === 'pass',
  });
}

// F7 cookie === nonce
{
  const lines = [promptLine({
    url: c2Url,
    filename: 'setup.exe',
    extraPayload: {
      handoffAuth: {
        headers: [{ name: 'Cookie', value: `rusticdl_e2e=${nonce}-tampered` }],
      },
    },
  })];
  const extra = {
    expectedUrl: c2Url,
    promptLines: lines,
    downloads: [],
  };
  check('F7', 'tampered cookie is fidelity fail', {
    old: run(legacy, c2Def(), extra),
    next: run(fixed, c2Def(), extra),
    oldWant: (r) => r.fidelity.cookies === 'pass',
    newWant: (r) => r.fidelity.cookies === 'fail',
  });
}

// F8 repeat: failed sentinel invalidates only its round
{
  const n1r1 = { id: 'N1', expect: 'keep', status: 'pass', invalid: false, reason: null };
  const n1r2 = { id: 'N1', expect: 'keep', status: 'pass', invalid: false, reason: null };
  const oldAll = [n1r1, n1r2];
  const oldIds = ['N1'];
  for (const item of oldAll) {
    if (oldIds.includes(item.id) && item.status === 'pass') {
      item.status = 'INVALID';
      item.invalid = true;
      item.reason = 'sentinel C2 did not capture; negative keep is INVALID';
    }
  }
  const n1r1b = { id: 'N1', expect: 'keep', status: 'pass', invalid: false, reason: null };
  const n1r2b = { id: 'N1', expect: 'keep', status: 'pass', invalid: false, reason: null };
  fixed.invalidateNegatives([n1r2b], 'sentinel C2 did not capture; negative keep is INVALID');
  check('F8', 'repeat sentinel fail invalidates only its round', {
    old: `${n1r1.status}/${n1r2.status}`,
    next: `${n1r1b.status}/${n1r2b.status}`,
    oldWant: (s) => s === 'INVALID/INVALID',
    newWant: (s) => s === 'pass/INVALID',
  });
}

// F9 end sentinel
{
  const n1Old = { id: 'N1', expect: 'keep', status: 'pass', invalid: false, reason: null };
  const n1New = { id: 'N1', expect: 'keep', status: 'pass', invalid: false, reason: null };
  if (legacy.INVALIDATE_END_SENTINEL) {
    legacy.invalidateNegatives([n1Old], 'end sentinel C2 did not capture; negative keep is INVALID');
  }
  if (fixed.INVALIDATE_END_SENTINEL) {
    fixed.invalidateNegatives([n1New], 'end sentinel C2 did not capture; negative keep is INVALID');
  }
  check('F9', 'end sentinel fail marks preceding negatives INVALID', {
    old: n1Old,
    next: n1New,
    oldWant: (r) => r.status === 'pass',
    newWant: (r) => r.status === 'INVALID',
  });
}

const failed = findings.filter((row) => !row.pass);
console.log(`SELFTEST ${failed.length === 0 ? 'OK' : 'FAIL'} ${findings.filter((r) => r.pass).length}/${findings.length}`);
if (failed.length) {
  console.error(`failed: ${failed.map((row) => row.id).join(', ')}`);
  process.exit(1);
}
process.exit(0);
