/**
 * Capture-matrix classifier (fixed). Used by extension-capture.spec.ts and
 * selftest-classifier.mjs. Test-only; not product code.
 */
import { EXPECTED_POST_BODY } from './ext-attack.mjs';

export const KEEP_WAIT_MS = 5000;
export const MISS_DRAIN = { minWaitMs: 5000, quietMs: 3000, maxWaitMs: 20_000, waitOutEmpty: true };
export const END_DRAIN = { minWaitMs: 0, quietMs: 3000, maxWaitMs: 20_000, waitOutEmpty: false };
export const END_RECHECK = true;
export const END_DOWNLOAD_RECHECK = true;
export const DOWNLOAD_SETTLE = { maxWaitMs: 12_000, pollMs: 50 };
export const INVALIDATE_END_SENTINEL = true;
export const INVALIDATE_BY_REF = true;
export const REQUIRE_ACCEPTED_HANDOFF = true;
export const REQUIRE_KEEP_PROOF = true;

export function parseCookieHeader(value) {
  const map = new Map();
  for (const part of String(value).split(';')) {
    const trimmed = part.trim();
    if (!trimmed) continue;
    const idx = trimmed.indexOf('=');
    if (idx === -1) continue;
    map.set(trimmed.slice(0, idx).trim(), trimmed.slice(idx + 1).trim());
  }
  return map;
}

function cookieHeaders(payload) {
  if (!payload || typeof payload !== 'object') return [];
  const auth = payload.handoffAuth;
  const blobs = [];
  for (const header of auth?.headers ?? []) blobs.push(header);
  for (const origin of auth?.originAuth ?? []) blobs.push(...(origin.headers ?? []));
  return blobs.filter((header) => header && header.name && header.name.toLowerCase() === 'cookie');
}

export function cookieMatchesNonce(payload, nonce) {
  return cookieHeaders(payload).some((header) => parseCookieHeader(header.value).get('rusticdl_e2e') === nonce);
}

export function postBodyFromPayload(payload) {
  if (!payload || typeof payload !== 'object') return null;
  for (const key of ['postBody', 'body', 'formBody', 'requestBody']) {
    if (typeof payload[key] === 'string') return payload[key];
  }
  return null;
}

export function expectedFinalUrl(origin, nonce, attempt) {
  const url = new URL('/C10/final', origin.endsWith('/') ? origin : `${origin}/`);
  url.searchParams.set('nonce', nonce);
  url.searchParams.set('attempt', attempt);
  return url.href;
}

export function isAcceptedReply(line) {
  const reply = line?.reply;
  if (!reply || typeof reply !== 'object') return false;
  return reply.ok === true
    && reply.type === 'accepted'
    && typeof line.requestId === 'string'
    && reply.requestId === line.requestId;
}

export function downloadCompletedFor(downloads, urls) {
  return browserDownloadStoppedFor(downloads, urls).completed;
}

export function downloadSearchFailed(downloads) {
  return Boolean(downloads && typeof downloads === 'object' && !Array.isArray(downloads) && downloads.error);
}

export const STOPPED_INTERRUPT_ERROR = 'USER_CANCELED';

export function matchingDownloadItems(downloads, urls) {
  const items = Array.isArray(downloads) ? downloads : [];
  const wanted = new Set((urls || []).filter((url) => typeof url === 'string' && url));
  return items.filter((item) => item && (
    (typeof item.url === 'string' && wanted.has(item.url))
    || (typeof item.finalUrl === 'string' && wanted.has(item.finalUrl))
  ));
}

/**
 * Positive proof the browser download for this click was stopped: no matching
 * chrome.downloads item (erased), or every match is `interrupted` with
 * `error === 'USER_CANCELED'` (chrome.downloads.cancel). Match on `url` or
 * `finalUrl`. Any other interrupt reason, `in_progress`, `complete`, or a
 * search `{error}` is not proof.
 */
export function browserDownloadStoppedFor(downloads, urls) {
  if (downloadSearchFailed(downloads)) {
    return {
      stopped: false,
      completed: false,
      inProgress: false,
      searchError: true,
      reason: `chrome.downloads.search failed: ${downloads.error}`,
    };
  }
  const matched = matchingDownloadItems(downloads, urls);
  if (matched.length === 0) {
    return {
      stopped: true,
      completed: false,
      inProgress: false,
      searchError: false,
      reason: null,
    };
  }
  if (matched.some((item) => item.state === 'complete')) {
    return {
      stopped: false,
      completed: true,
      inProgress: false,
      searchError: false,
      reason: 'browser download completed; capture requires erase/cancel, not state complete',
    };
  }
  const inProgress = matched.some((item) => item.state !== 'interrupted');
  if (inProgress) {
    return {
      stopped: false,
      completed: false,
      inProgress: true,
      searchError: false,
      reason: 'browser download still in_progress; capture requires erase or USER_CANCELED',
    };
  }
  const bad = matched.find((item) => item.error !== STOPPED_INTERRUPT_ERROR);
  if (bad) {
    const err = typeof bad.error === 'string' && bad.error ? bad.error : 'unknown';
    return {
      stopped: false,
      completed: false,
      inProgress: false,
      searchError: false,
      reason: `browser download interrupted with error ${err}; capture requires erase or USER_CANCELED`,
    };
  }
  return {
    stopped: true,
    completed: false,
    inProgress: false,
    searchError: false,
    reason: null,
  };
}

export function downloadItemsTerminal(downloads, urls) {
  if (downloadSearchFailed(downloads)) return true;
  const matched = matchingDownloadItems(downloads, urls);
  if (matched.length === 0) return true;
  return matched.every((item) => item.state === 'complete' || item.state === 'interrupted');
}

/**
 * Capture messages `handoffDownload` can send (nativeMessaging.ts):
 * - auto → enqueueDownload → type `enqueue_download` (lines 139–160, 191–192)
 * - ask  → promptDownload  → type `prompt_download`  (lines 162–178, 193–194)
 * ping / open_app / save_extension_settings are not captures.
 */
export const CAPTURE_MESSAGE_TYPES = Object.freeze(['prompt_download', 'enqueue_download']);

export function isCaptureMessageType(type) {
  return type === 'prompt_download' || type === 'enqueue_download';
}

function captureFieldsMatch(line, { url, filename, entryPoint = 'browser_download', startedAt }) {
  if (!line || !isCaptureMessageType(line.type)) return false;
  if (typeof startedAt === 'number' && !(line.receivedAt >= startedAt)) return false;
  const payload = line.payload ?? {};
  return payload.url === url
    && payload.suggestedFilename === filename
    && payload.source?.entryPoint === entryPoint;
}

export function exactPromptMatch(line, { url, filename, entryPoint = 'browser_download', startedAt }) {
  return line?.type === 'prompt_download' && captureFieldsMatch(line, {
    url,
    filename,
    entryPoint,
    startedAt,
  });
}

/**
 * Shared capture predicate for the in-case drain and the end-of-run recheck.
 * `lines` is the caller's window (drain slice, or the full host log).
 * Recheck sets `matchUrl` so later sentinels with a different URL cannot
 * flip a G1/G4 miss. In-case leaves it false so any capture in the drain
 * (wrong URL included) is still a capture.
 */
export function classifyCaptureLinesForCase(lines, caseLike, startedAt, { matchUrl = false } = {}) {
  const url = caseLike?.expectedUrl ?? caseLike?.url;
  const filename = caseLike?.filename;
  const captureLines = (lines || []).filter((line) => {
    if (!line || !isCaptureMessageType(line.type)) return false;
    if (typeof startedAt === 'number' && !(line.receivedAt >= startedAt)) return false;
    if (matchUrl && (line.payload ?? {}).url !== url) return false;
    return true;
  });
  const promptLines = captureLines.filter((line) => line.type === 'prompt_download');
  const enqueueLines = captureLines.filter((line) => line.type === 'enqueue_download');
  const matchLine = promptLines.find((line) => exactPromptMatch(line, {
    url,
    filename,
    startedAt,
  })) ?? null;
  return {
    captureLines,
    promptLines,
    enqueueLines,
    matchLine,
    captured: captureLines.length > 0,
    exactPositive: Boolean(matchLine) && promptLines.length === 1 && enqueueLines.length === 0,
  };
}

export function fixtureHitForAttempt(hits, { pathname, nonce, attempt }) {
  return (hits || []).some((hit) => (
    hit.pathname === pathname
    && hit.nonce === nonce
    && hit.attempt === attempt
  ));
}

function routePathname(route) {
  if (!route) return '';
  const q = route.indexOf('?');
  return q === -1 ? route : route.slice(0, q);
}

export function pathnameOfUrl(value) {
  try {
    return new URL(value).pathname;
  } catch {
    return '';
  }
}

export function fixtureServeForTarget(serves, { pathname, nonce, attempt }) {
  const rows = (serves || []).filter((row) => (
    row
    && row.pathname === pathname
    && String(row.nonce) === String(nonce)
    && String(row.attempt) === String(attempt)
  ));
  return rows.find((row) => row.completed) || rows[0] || null;
}

export function downloadKeptTarget(items, { path, nonce, attempt, triggerAt }) {
  return (items || []).some((item) => {
    if (!item || item.error || item.state !== 'complete') return false;
    let parsed;
    try {
      parsed = new URL(item.url);
    } catch {
      return false;
    }
    if (parsed.pathname !== path) return false;
    if (parsed.searchParams.get('nonce') !== String(nonce)) return false;
    if (parsed.searchParams.get('attempt') !== String(attempt)) return false;
    if (typeof triggerAt === 'number' && item.startTime) {
      const startMs = Date.parse(item.startTime);
      if (!Number.isFinite(startMs) || startMs < triggerAt - 50) return false;
    }
    return true;
  });
}

export function blobDataKept(items, { caseDef, triggerAt }) {
  return (items || []).some((item) => {
    if (!item || item.error) return false;
    const url = typeof item.url === 'string' ? item.url : '';
    if (typeof triggerAt !== 'number' || !item.startTime) return false;
    const startMs = Date.parse(item.startTime);
    if (!Number.isFinite(startMs) || startMs < triggerAt - 50) return false;
    if (caseDef.id === 'C5' && url.startsWith('blob:')) return true;
    if (caseDef.id === 'C6' && url.startsWith('data:')) return true;
    return false;
  });
}

export function pageRenderTarget(pageEvidence, { path, nonce, attempt }) {
  let pageUrl = null;
  try {
    if (pageEvidence?.href) pageUrl = new URL(pageEvidence.href);
  } catch {
    pageUrl = null;
  }
  if (!pageUrl) return false;
  if (pageUrl.pathname !== path) return false;
  return pageUrl.searchParams.get('nonce') === String(nonce)
    && pageUrl.searchParams.get('attempt') === String(attempt);
}

export function fetchKeptFromServe(pageEvidence, serve, { path, nonce, attempt, caseDef }) {
  if (!serve || serve.pathname !== path || !serve.completed) return false;
  if (!(Number(serve.status) >= 200 && Number(serve.status) < 400)) return false;
  let pageUrl = null;
  try {
    if (pageEvidence?.href) pageUrl = new URL(pageEvidence.href);
  } catch {
    pageUrl = null;
  }
  if (!pageUrl) return false;
  if (pageUrl.pathname !== `/page/${caseDef.id}`) return false;
  if (pageUrl.searchParams.get('nonce') !== String(nonce)) return false;
  if (pageUrl.searchParams.get('attempt') !== String(attempt)) return false;
  const status = Number(pageEvidence.fetchStatus);
  const bytes = Number(pageEvidence.fetchBytes);
  return String(pageEvidence.fetchNonce) === String(nonce)
    && String(pageEvidence.fetchAttempt) === String(attempt)
    && status === Number(serve.status)
    && bytes === Number(serve.bytes)
    && bytes > 0;
}

export function keepClickProved(caseDef, {
  hits,
  fixtureServes,
  downloads,
  pageEvidence,
  nonce,
  attempt,
  playwrightDownload,
  triggerAt,
}) {
  const path = routePathname(caseDef.route);
  const fileOnFixture = path && !path.startsWith('/page/');
  const serves = fixtureServes || [];
  const serve = fileOnFixture
    ? fixtureServeForTarget(serves, { pathname: path, nonce, attempt })
    : null;

  if (fileOnFixture) {
    const hitOk = fixtureHitForAttempt(hits, { pathname: path, nonce, attempt });
    if (!serve && !hitOk) {
      return { ok: false, reason: `no fixture hit for ${path} nonce=${nonce} attempt=${attempt}` };
    }
    if (!serve || !serve.completed) {
      return { ok: false, reason: `no completed fixture serve for ${path} nonce=${nonce} attempt=${attempt}` };
    }
  }

  const items = Array.isArray(downloads) ? downloads : [];

  if (caseDef.trigger === 'fetch-attachment') {
    if (fetchKeptFromServe(pageEvidence, serve, { path, nonce, attempt, caseDef })) {
      return { ok: true, reason: null };
    }
    return { ok: false, reason: 'fetch-attachment produced no this-attempt page-render marker' };
  }

  if (!fileOnFixture) {
    if (blobDataKept(items, { caseDef, triggerAt }) || playwrightDownload) {
      return { ok: true, reason: null };
    }
    return { ok: false, reason: 'no this-attempt chrome.downloads.search / playwright download for blob/data keep' };
  }

  const downloadKept = downloadKeptTarget(items, {
    path,
    nonce,
    attempt,
    triggerAt,
  });
  const renderKept = pageRenderTarget(pageEvidence, { path, nonce, attempt });
  if (downloadKept || renderKept) {
    return { ok: true, reason: null };
  }
  return { ok: false, reason: 'keep click produced no this-attempt download or page-render evidence' };
}

export function fidelityFor(caseDef, line, expectedUrl, pageUrl, nonce) {
  const flags = caseDef.fidelity;
  const payload = (line?.payload ?? null);
  const source = (payload?.source ?? null);
  const result = {};
  result.cookies = flags.cookies
    ? (payload && cookieMatchesNonce(payload, nonce) ? 'pass' : 'fail')
    : 'n/a';
  result.referrer = flags.referrer
    ? (source && source.referrer === pageUrl ? 'pass' : 'fail')
    : 'n/a';
  result.postBody = flags.postBody
    ? (postBodyFromPayload(payload) === EXPECTED_POST_BODY ? 'pass' : 'fail')
    : 'n/a';
  result.originalUrl = flags.originalUrl
    ? (payload && payload.url === expectedUrl ? 'pass' : 'fail')
    : 'n/a';
  return result;
}

function firstPrompt(promptLines) {
  return promptLines[0] ?? null;
}

function failProof(reason, extra = {}) {
  return { ok: false, verdict: 'FAIL', reason, ...extra };
}

export function asEvidence(value, label) {
  if (value == null) {
    return { ok: false, items: [], reason: `${label} missing` };
  }
  if (Array.isArray(value)) {
    return { ok: true, items: value, reason: null };
  }
  if (typeof value === 'object') {
    if (value.ok === false) {
      return {
        ok: false,
        items: Array.isArray(value.items) ? value.items : [],
        reason: value.reason || `${label} missing`,
      };
    }
    if (value.error) {
      return {
        ok: false,
        items: [],
        reason: `${label} failed: ${value.error}`,
        searchError: value.error,
      };
    }
    if (value.ok === true && Array.isArray(value.items)) {
      return { ok: true, items: value.items, reason: null };
    }
  }
  return { ok: false, items: [], reason: `${label} unreadable` };
}

function hostLinesOf(ctx) {
  if (Array.isArray(ctx.hostLog)) return ctx.hostLog;
  return [...(ctx.promptLines || []), ...(ctx.enqueueLines || [])];
}

function hostEvidence(ctx) {
  if (ctx.hostLogOk === false) {
    return { ok: false, lines: [], reason: ctx.hostLogReason || 'mock-host log missing' };
  }
  if (ctx.hostLog && typeof ctx.hostLog === 'object' && !Array.isArray(ctx.hostLog)) {
    const ev = asEvidence(ctx.hostLog, 'mock-host log');
    return { ok: ev.ok, lines: ev.items, reason: ev.reason };
  }
  return { ok: true, lines: hostLinesOf(ctx), reason: null };
}

function captureSnapshot(ctx, { matchUrl = false, url } = {}) {
  const host = hostEvidence(ctx);
  if (!host.ok) {
    return { host, capture: null };
  }
  const capture = classifyCaptureLinesForCase(
    host.lines,
    {
      url: url ?? ctx.expectedUrl,
      filename: ctx.filename ?? ctx.caseDef?.filename,
      expectedUrl: url ?? ctx.expectedUrl,
    },
    ctx.startedAt,
    { matchUrl },
  );
  return { host, capture };
}

function checkCapture(ctx) {
  const { host, capture } = captureSnapshot(ctx);
  if (!host.ok) return failProof(host.reason);
  const match = capture.matchLine;
  if (!capture.exactPositive) {
    return failProof(
      `expected one prompt_download url=${ctx.expectedUrl} filename=${ctx.filename ?? ctx.caseDef?.filename} entryPoint=browser_download; got ${JSON.stringify(capture.promptLines.concat(capture.enqueueLines))}`,
      { capture },
    );
  }
  if (!match || !isAcceptedReply(match)) {
    return failProof('missing or wrong accepted reply (ok, type=accepted, echoed requestId)', { capture });
  }
  return { ok: true, verdict: 'pass', reason: null, capture, matchLine: match };
}

function checkKeep(ctx) {
  const { host, capture } = captureSnapshot(ctx);
  if (!host.ok) return failProof(host.reason);
  if (capture.captured) {
    return failProof(
      `expected keep (zero capture lines); got ${JSON.stringify(capture.promptLines.concat(capture.enqueueLines))}`,
      { capture },
    );
  }
  const path = routePathname(ctx.caseDef.route);
  const fileOnFixture = path && !path.startsWith('/page/');
  if (fileOnFixture && ctx.fixtureServesOk === false) {
    return failProof(ctx.fixtureServesReason || 'fixture serve log missing');
  }
  if (ctx.pageEvidenceOk === false) {
    return failProof(ctx.pageEvidenceReason || 'DOM marker missing');
  }
  const downloadsEv = asEvidence(
    ctx.downloadsOk === false ? { ok: false, reason: ctx.downloadsReason || 'chrome.downloads.search not read' } : ctx.downloads,
    'chrome.downloads.search',
  );
  if (ctx.downloadsOk === false) {
    return failProof(downloadsEv.reason);
  }
  const proof = keepClickProved(ctx.caseDef, {
    hits: ctx.fixtureHits,
    fixtureServes: ctx.fixtureServes,
    downloads: Array.isArray(ctx.downloads) ? ctx.downloads : downloadsEv.items,
    pageEvidence: ctx.pageEvidence,
    nonce: ctx.nonce,
    attempt: ctx.attempt,
    playwrightDownload: Boolean(ctx.playwrightDownload),
    triggerAt: ctx.triggerAt,
  });
  if (!proof.ok) return failProof(proof.reason || 'keep click not proved');
  return { ok: true, verdict: 'pass', reason: null, capture };
}

function checkStop(ctx) {
  const urls = (ctx.urls || [ctx.expectedUrl, ctx.capturedUrl, ctx.expectedFinalUrl]).filter(Boolean);
  if (ctx.downloadsOk === false) {
    return failProof(ctx.downloadsReason || 'chrome.downloads.search not read', {
      stopped: false,
      completed: false,
      inProgress: false,
      searchError: true,
    });
  }
  const inner = browserDownloadStoppedFor(ctx.downloads, urls);
  if (inner.searchError || !inner.stopped) {
    return failProof(
      inner.reason || 'browser download was not shown stopped (erased or USER_CANCELED)',
      inner,
    );
  }
  return {
    ok: true,
    verdict: 'pass',
    reason: null,
    stopped: true,
    completed: false,
    inProgress: false,
    searchError: false,
  };
}

function checkMissGap(gap, ctx) {
  const { host, capture } = captureSnapshot(ctx, { matchUrl: Boolean(ctx.matchUrl) });
  if (!host.ok) return failProof(host.reason);
  if (capture.exactPositive) {
    return {
      ok: false,
      verdict: 'XPASS',
      reason: `xfail ${gap} passed`,
      capture,
    };
  }
  if (capture.captured) {
    return failProof(
      `xfail ${gap} failed for a different reason (timeout/harness/wrong line)`,
      { capture },
    );
  }
  return {
    ok: true,
    verdict: 'xfail',
    reason: ctx.caseDef?.xfail?.reason || `xfail ${gap}`,
    capture,
  };
}

function checkG3(ctx) {
  const { host, capture } = captureSnapshot(ctx);
  if (!host.ok) return failProof(host.reason);
  if (capture.exactPositive) {
    return {
      ok: true,
      verdict: 'xfail',
      reason: ctx.caseDef?.xfail?.reason || 'xfail G3',
      capture,
    };
  }
  return failProof(
    `xfail G3 failed for a different reason (timeout/harness/wrong line)`,
    { capture },
  );
}

function checkG5(ctx) {
  const finalUrl = ctx.expectedFinalUrl;
  const { host, capture } = captureSnapshot(ctx, { url: finalUrl });
  if (!host.ok) return failProof(host.reason);
  const line = capture.matchLine;
  if (!(capture.exactPositive && line && isAcceptedReply(line))) {
    return failProof(
      `xfail G5 failed for a different reason (timeout/harness/wrong line)`,
      { capture },
    );
  }
  const stop = checkStop({
    ...ctx,
    capturedUrl: line?.payload?.url,
    urls: [ctx.expectedUrl, line?.payload?.url, finalUrl],
  });
  if (!stop.ok) {
    return failProof(
      stop.reason || 'xfail G5 failed for a different reason (timeout/harness/wrong line)',
      { capture, ...stop },
    );
  }
  return {
    stopped: stop.stopped,
    completed: stop.completed,
    inProgress: stop.inProgress,
    searchError: stop.searchError,
    ok: true,
    verdict: 'xfail',
    reason: ctx.caseDef?.xfail?.reason || 'xfail G5',
    capture,
  };
}

function checkEndWindow(ctx) {
  const { host, capture } = captureSnapshot(ctx, { matchUrl: true, url: ctx.expectedUrl });
  if (!host.ok) return failProof(host.reason);
  if (!capture.captured) {
    return {
      ok: true,
      verdict: 'xfail',
      reason: null,
      capture,
    };
  }
  if (capture.exactPositive) {
    return {
      ok: false,
      verdict: 'XPASS',
      reason: `xfail ${ctx.gap || ctx.item?.gap} passed`,
      capture,
      matchLine: capture.matchLine,
    };
  }
  return failProof(
    `xfail ${ctx.gap || ctx.item?.gap} failed for a different reason (timeout/harness/wrong line)`,
    { capture },
  );
}

function checkEndDownload(ctx) {
  return checkStop(ctx);
}

/**
 * Single predicate table. Every pass/fail in classify.mjs (and harness
 * capture-line / evidence-log reads) goes through evaluateProof.
 * A pass never comes from a bare chrome.downloads state, from "no error",
 * from a log that was not read, or from another case's path.
 *
 * Rewired into this table (no leftover ad-hoc pass/fail):
 * - classifyCase capture/keep/G1/G3/G4/G5 status → evaluateProof(id)
 * - recheckLateMissXfails → end-window-recheck
 * - recheckCaptureDownloads → end-download-recheck
 * - checkG3/checkG5 exact-line counts → capture.exactPositive
 * - harness captureLinesSince type===prompt|enqueue → classifyCaptureLinesForCase
 * - unread host/fixture/downloads logs → asEvidence / *Ok flags (FAIL named)
 */
export const PROOF_RULES = Object.freeze([
  Object.freeze({
    id: 'capture',
    evidence: Object.freeze(['mock-host log line for this case (payload.url, suggestedFilename, source.entryPoint, receivedAt in window)']),
    cause: 'exactly one prompt_download for this case URL+filename+entryPoint=browser_download with accepted reply (ok, type=accepted, echoed requestId)',
    missingDefault: 'FAIL: mock-host log missing/empty/unreadable/ambiguous, or a capture line for another URL/filename/entryPoint',
    check: checkCapture,
  }),
  Object.freeze({
    id: 'keep',
    evidence: Object.freeze([
      'mock-host log (this case window; must be zero capture lines)',
      'fixture serve log entry for this case path+nonce+attempt',
      'chrome.downloads item matched by this-case path+nonce+attempt OR DOM document.URL / fetch marker for this path+nonce+attempt',
    ]),
    cause: 'zero prompt_download/enqueue_download AND a completed this-case fixture serve (file keeps) AND this-attempt keep evidence (complete download, page render, or fetch marker)',
    missingDefault: 'FAIL: mock-host/fixture/DOM/chrome.downloads missing, other-case path, or no this-attempt keep evidence',
    check: checkKeep,
  }),
  Object.freeze({
    id: 'stop',
    evidence: Object.freeze(['chrome.downloads.search items matched by url OR finalUrl in this click\'s wanted set']),
    cause: 'no matching item (erased by the extension) OR every match interrupted with error===USER_CANCELED (chrome.downloads.cancel)',
    missingDefault: 'FAIL: search not read/errored/unreadable, in_progress, complete, or any other interrupt reason named',
    check: checkStop,
  }),
  Object.freeze({
    id: 'end-window-recheck',
    evidence: Object.freeze(['full mock-host log at end of run, filtered by this case startedAt and this case URL']),
    cause: 'same capture predicate as the in-case drain: still empty → miss holds; exact prompt_download → XPASS; enqueue_download or wrong-shape line for this URL → FAIL',
    missingDefault: 'FAIL: mock-host log missing/unreadable, or a this-URL capture line that is not exact prompt_download',
    check: checkEndWindow,
  }),
  Object.freeze({
    id: 'end-download-recheck',
    evidence: Object.freeze(['end-of-run chrome.downloads.search, match url OR finalUrl']),
    cause: 'stop rule still holds (erased or USER_CANCELED)',
    missingDefault: 'FAIL: search not read/errored, or stop cause no longer holds (complete / other interrupt / in_progress)',
    check: checkEndDownload,
  }),
  Object.freeze({
    id: 'g1-miss',
    evidence: Object.freeze(['mock-host log for this C1-family URL in the case window']),
    cause: 'successfully read log with zero capture lines for this case (Chromium did not enqueue this URL)',
    missingDefault: 'FAIL: log missing/unreadable, or a capture line for this URL that is not exact prompt_download',
    check: (ctx) => checkMissGap('G1', ctx),
  }),
  Object.freeze({
    id: 'g4-miss',
    evidence: Object.freeze(['mock-host log for this C4-family URL in the case window']),
    cause: 'successfully read log with zero capture lines for this case (Chromium did not enqueue this URL)',
    missingDefault: 'FAIL: log missing/unreadable, or a capture line for this URL that is not exact prompt_download',
    check: (ctx) => checkMissGap('G4', ctx),
  }),
  Object.freeze({
    id: 'g3-false-grab',
    evidence: Object.freeze(['mock-host log line for this-attempt POST URL']),
    cause: 'exactly one prompt_download with url=this-attempt POST URL, suggestedFilename=posted.zip, entryPoint=browser_download',
    missingDefault: 'FAIL: log missing/unreadable, other-case URL, superstring, wrong filename, or non-prompt type',
    check: checkG3,
  }),
  Object.freeze({
    id: 'g5-final-url',
    evidence: Object.freeze([
      'mock-host log line for origin+/C10/final+this-attempt query',
      'chrome.downloads.search matched by url OR finalUrl',
    ]),
    cause: 'exactly one prompt_download with payload.url===final URL, accepted reply, AND stop (erased or USER_CANCELED)',
    missingDefault: 'FAIL: log/search missing, evil/super/garbage URL, in_progress/complete, or other interrupt named',
    check: checkG5,
  }),
]);

export const PROOF_RULE_BY_ID = Object.freeze(Object.fromEntries(
  PROOF_RULES.map((row) => [row.id, row]),
));

export function evaluateProof(id, ctx) {
  const rule = PROOF_RULE_BY_ID[id];
  if (!rule) {
    return { ...failProof(`unknown proof rule ${id}`), id };
  }
  const result = rule.check(ctx);
  return { ...result, id };
}

function gapProofId(gap) {
  switch (gap) {
    case 'G1':
      return 'g1-miss';
    case 'G4':
      return 'g4-miss';
    case 'G3':
      return 'g3-false-grab';
    case 'G5':
      return 'g5-final-url';
    default:
      return null;
  }
}

function applyProofFields(result, capture, stop) {
  const prompt = capture?.matchLine
    ?? firstPrompt(capture?.promptLines || [])
    ?? capture?.enqueueLines?.[0]
    ?? null;
  if (prompt) {
    result.capturedUrl = prompt.payload?.url ?? null;
    result.suggestedFilename = prompt.payload?.suggestedFilename ?? null;
    result.entryPoint = prompt.payload?.source?.entryPoint ?? null;
    result.hostReceivedAt = prompt.receivedAt ?? null;
    if (result.triggerAt != null && result.hostReceivedAt != null) {
      result.latencyMs = result.hostReceivedAt - result.triggerAt;
    }
  }
  result.observed = capture?.captured ? 'capture' : 'keep';
  result.handoffAccepted = Boolean(prompt && isAcceptedReply(prompt));
  result.downloadCompleted = Boolean(stop?.completed);
  result.browserDownloadStopped = Boolean(stop?.ok || stop?.stopped);
  result.downloadSearchError = Boolean(stop?.searchError);
}

/**
 * Classify one matrix case. Every pass/fail is evaluateProof (PROOF_RULES).
 */
export function classifyCase(input) {
  const {
    caseDef,
    expectedUrl,
    expectedFinalUrl: finalUrl,
    pageUrl,
    nonce,
    attempt,
    startedAt,
    triggerAt,
    promptLines,
    enqueueLines,
    hostLog,
    hostLogOk,
    hostLogReason,
    downloads,
    downloadsOk,
    downloadsReason,
    fixtureHits,
    fixtureServes,
    fixtureServesOk,
    fixtureServesReason,
    pageEvidence,
    pageEvidenceOk,
    pageEvidenceReason,
    playwrightDownload,
  } = input;

  const result = {
    id: caseDef.id,
    expect: caseDef.expect,
    observed: 'keep',
    status: 'FAIL',
    gap: caseDef.xfail?.gap ?? null,
    reason: null,
    triggerAt: triggerAt ?? null,
    hostReceivedAt: null,
    latencyMs: null,
    capturedUrl: null,
    suggestedFilename: null,
    entryPoint: null,
    fidelity: {
      cookies: caseDef.fidelity.cookies ? 'fail' : 'n/a',
      referrer: caseDef.fidelity.referrer ? 'fail' : 'n/a',
      postBody: caseDef.fidelity.postBody ? 'fail' : 'n/a',
      originalUrl: caseDef.fidelity.originalUrl ? 'fail' : 'n/a',
    },
    invalid: false,
    expectedUrl,
    expectedFinalUrl: finalUrl ?? null,
    startedAt,
    filename: caseDef.filename,
    attempt,
    nonce,
    skip: false,
  };

  if (caseDef.skip) {
    result.observed = 'skip';
    result.status = 'skip';
    result.reason = caseDef.skip;
    result.skip = true;
    return result;
  }

  const ctx = {
    caseDef,
    expectedUrl,
    expectedFinalUrl: finalUrl,
    filename: caseDef.filename,
    nonce,
    attempt,
    startedAt,
    triggerAt,
    promptLines,
    enqueueLines,
    hostLog,
    hostLogOk,
    hostLogReason,
    downloads,
    downloadsOk,
    downloadsReason,
    fixtureHits,
    fixtureServes,
    fixtureServesOk,
    fixtureServesReason,
    pageEvidence,
    pageEvidenceOk,
    pageEvidenceReason,
    playwrightDownload,
  };

  const snap = captureSnapshot(ctx);
  const capture = snap.capture || {
    captureLines: [],
    promptLines: [],
    enqueueLines: [],
    matchLine: null,
    captured: false,
    exactPositive: false,
  };
  const prompt = capture.matchLine ?? firstPrompt(capture.promptLines) ?? capture.enqueueLines[0] ?? null;
  result.fidelity = fidelityFor(caseDef, prompt, expectedUrl, pageUrl, nonce);
  if (caseDef.fidelity.postBody && result.fidelity.postBody === 'fail') {
    result.reason = [result.reason, 'protocol has no POST body field'].filter(Boolean).join('; ');
  }

  ctx.capturedUrl = prompt?.payload?.url ?? null;
  ctx.urls = [expectedUrl, ctx.capturedUrl, finalUrl];
  const stop = evaluateProof('stop', ctx);
  applyProofFields(result, capture, stop);

  const gap = caseDef.xfail?.gap ?? '';
  const gapId = gapProofId(gap);

  if (caseDef.xfail) {
    if (!gapId) {
      result.status = 'FAIL';
      result.reason = `xfail ${gap} has no proof rule`;
    } else if (gapId === 'g1-miss' || gapId === 'g4-miss') {
      const miss = evaluateProof(gapId, ctx);
      result.status = miss.verdict;
      result.reason = miss.reason;
      if (miss.capture) applyProofFields(result, miss.capture, stop);
    } else if (gapId === 'g3-false-grab') {
      const g3 = evaluateProof('g3-false-grab', ctx);
      result.status = g3.verdict;
      result.reason = g3.reason;
      if (g3.capture) applyProofFields(result, g3.capture, stop);
    } else if (gapId === 'g5-final-url') {
      const cap = evaluateProof('capture', ctx);
      if (cap.ok && stop.ok) {
        result.status = 'XPASS';
        result.reason = `xfail ${gap} passed`;
        applyProofFields(result, cap.capture || capture, stop);
      } else {
        const g5 = evaluateProof('g5-final-url', ctx);
        result.status = g5.verdict;
        result.reason = g5.reason;
        applyProofFields(result, g5.capture || capture, g5.ok ? g5 : stop);
      }
    } else {
      result.status = 'FAIL';
      result.reason = `xfail ${gap} has no proof rule`;
    }
  } else if (caseDef.expect === 'capture') {
    const cap = evaluateProof('capture', ctx);
    if (cap.ok && stop.ok) {
      result.status = 'pass';
      result.reason = null;
    } else {
      result.status = 'FAIL';
      const bits = [];
      if (!cap.ok) bits.push(cap.reason);
      if (cap.ok && !stop.ok) bits.push(stop.reason);
      result.reason = bits.filter(Boolean).join('; ') || 'capture failed';
    }
    applyProofFields(result, cap.capture || capture, stop);
  } else {
    const keep = evaluateProof('keep', ctx);
    result.status = keep.verdict;
    result.reason = keep.reason;
    applyProofFields(result, keep.capture || capture, stop);
  }

  if (caseDef.fidelity.postBody && result.fidelity.postBody === 'fail') {
    result.reason = [result.reason, 'protocol has no POST body field'].filter(Boolean).join('; ');
  }
  return result;
}

export function isSentinelOk(result) {
  return result?.status === 'pass' && result?.observed === 'capture';
}

export function invalidateNegatives(items, reason) {
  for (const item of items) {
    if (item.status === 'pass' && item.expect === 'keep') {
      item.status = 'INVALID';
      item.invalid = true;
      item.reason = reason;
    }
  }
}

export function recheckLateMissXfails(results, hostLog) {
  const ev = asEvidence(hostLog, 'mock-host log');
  for (const item of results) {
    if (item.status !== 'xfail') continue;
    if (!item.gap || !['G1', 'G4'].includes(item.gap)) continue;
    const proof = evaluateProof('end-window-recheck', {
      caseDef: { filename: item.filename, xfail: { gap: item.gap, reason: item.reason } },
      expectedUrl: item.expectedUrl,
      filename: item.filename,
      startedAt: item.startedAt,
      hostLog: ev.ok ? ev.items : { ok: false, reason: ev.reason },
      hostLogOk: ev.ok,
      hostLogReason: ev.reason,
      gap: item.gap,
      item,
      matchUrl: true,
    });
    if (proof.verdict === 'xfail' && proof.ok) continue;
    const capture = proof.capture;
    const first = capture?.matchLine ?? capture?.captureLines?.[0];
    if (first) {
      item.observed = 'capture';
      item.capturedUrl = first?.payload?.url ?? null;
      item.suggestedFilename = first?.payload?.suggestedFilename ?? null;
      item.entryPoint = first?.payload?.source?.entryPoint ?? null;
      item.hostReceivedAt = first?.receivedAt ?? null;
      if (item.triggerAt != null && item.hostReceivedAt != null) {
        item.latencyMs = item.hostReceivedAt - item.triggerAt;
      }
    }
    item.status = proof.verdict;
    item.reason = proof.reason;
  }
}

export function recheckCaptureDownloads(results, downloads) {
  const ev = asEvidence(downloads, 'chrome.downloads.search');
  for (const item of results) {
    if (item.skip || item.status === 'INVALID') continue;
    const captureRow = item.expect === 'capture'
      && item.observed === 'capture'
      && (item.status === 'pass' || item.status === 'xfail' || item.status === 'XPASS');
    if (!captureRow) continue;
    const stop = evaluateProof('end-download-recheck', {
      expectedUrl: item.expectedUrl,
      capturedUrl: item.capturedUrl,
      expectedFinalUrl: item.expectedFinalUrl,
      downloads: ev.ok ? ev.items : downloads,
      downloadsOk: ev.ok || Boolean(ev.searchError),
      downloadsReason: ev.reason,
      urls: [item.expectedUrl, item.capturedUrl, item.expectedFinalUrl],
    });
    if (stop.ok) continue;
    item.status = 'FAIL';
    item.downloadCompleted = Boolean(stop.completed);
    item.browserDownloadStopped = false;
    item.downloadSearchError = Boolean(stop.searchError);
    item.reason = stop.reason
      || (item.gap
        ? `xfail ${item.gap} failed for a different reason (timeout/harness/wrong line)`
        : 'browser download was not shown stopped (erased or USER_CANCELED)');
  }
}

export function hostFailedHandoff(results) {
  return results.some((item) => (
    item.expect === 'capture'
    && item.observed === 'capture'
    && item.status === 'FAIL'
    && (
      item.handoffAccepted === false
      || item.downloadCompleted === true
      || item.downloadSearchError === true
      || item.browserDownloadStopped === false
    )
  ));
}
