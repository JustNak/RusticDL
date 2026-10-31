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

/**
 * Classify one matrix case. Does not mutate host logs.
 *
 * Early returns / gap branches (each has a fail-closed sibling attack):
 * - skip → skip (L2/L3; not a keep pass)
 * - G1/G4 exactPositiveLine → XPASS (`late-c1` / `late-c4` / `end-exact`); wrong shape → FAIL (`late-c1-nullfile` / `end-null` / `end-badep` / `end-enqueue`)
 * - G1/G4 empty keep → xfail; observed capture that is not exact → FAIL (`plant-c1` / `enqueue-c1`)
 * - expect-capture xfail + fullHandoff → XPASS (G5 prefix rewrite)
 * - expect-keep xfail + exactNegative → XPASS (no keep-xfail rows today)
 * - G3 exactFalseGrab → xfail; other/super/badfile/badep → FAIL (`g3-other`)
 * - G5 exactFinal + accepted + erased or interrupted USER_CANCELED → xfail; other interrupt/complete/in_progress/search error → FAIL
 * - capture fullHandoff → pass; missing accepted, in_progress, complete, or search error → FAIL (`wrong-shape` / `slow-c2` / `search-error`)
 * - keep: exactNegative && keepClickProved → pass, else FAIL
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
    downloads,
    fixtureHits,
    fixtureServes,
    pageEvidence,
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

  const capture = classifyCaptureLinesForCase(
    [...(promptLines || []), ...(enqueueLines || [])],
    { url: expectedUrl, filename: caseDef.filename },
    startedAt,
  );
  const matchLine = capture.matchLine;
  const prompt = matchLine ?? firstPrompt(capture.promptLines) ?? capture.enqueueLines[0] ?? null;
  if (prompt) {
    result.capturedUrl = prompt.payload?.url ?? null;
    result.suggestedFilename = prompt.payload?.suggestedFilename ?? null;
    result.entryPoint = prompt.payload?.source?.entryPoint ?? null;
    result.hostReceivedAt = prompt.receivedAt ?? null;
    if (result.triggerAt != null && result.hostReceivedAt != null) {
      result.latencyMs = result.hostReceivedAt - result.triggerAt;
    }
  }
  result.fidelity = fidelityFor(caseDef, prompt, expectedUrl, pageUrl, nonce);
  if (caseDef.fidelity.postBody && result.fidelity.postBody === 'fail') {
    result.reason = [result.reason, 'protocol has no POST body field'].filter(Boolean).join('; ');
  }

  const captured = capture.captured;
  result.observed = captured ? 'capture' : 'keep';

  const exactPositiveLine = capture.exactPositive;
  const exactNegative = capture.promptLines.length === 0 && capture.enqueueLines.length === 0;
  const accepted = Boolean(prompt && isAcceptedReply(prompt));
  const stop = browserDownloadStoppedFor(downloads, [expectedUrl, result.capturedUrl, finalUrl]);
  const completed = stop.completed;
  const stopped = stop.stopped;
  const fullHandoff = exactPositiveLine && Boolean(matchLine && isAcceptedReply(matchLine)) && stopped;

  const gap = caseDef.xfail?.gap ?? '';
  const missGaps = new Set(['G1', 'G4']);
  const falseGrabGaps = new Set(['G3']);
  const finalUrlGaps = new Set(['G5']);

  const exactFalseGrab = capture.promptLines.length === 1
    && capture.enqueueLines.length === 0
    && exactPromptMatch(capture.promptLines[0], {
      url: expectedUrl,
      filename: caseDef.filename,
      startedAt,
    });

  const exactFinalCapture = capture.promptLines.length === 1
    && capture.enqueueLines.length === 0
    && exactPromptMatch(capture.promptLines[0], {
      url: finalUrl,
      filename: caseDef.filename,
      startedAt,
    });

  if (caseDef.xfail) {
    if (missGaps.has(gap) && exactPositiveLine) {
      result.status = 'XPASS';
      result.reason = `xfail ${caseDef.xfail.gap} passed`;
    } else if (caseDef.expect === 'capture' && fullHandoff) {
      result.status = 'XPASS';
      result.reason = `xfail ${caseDef.xfail.gap} passed`;
    } else if (caseDef.expect === 'keep' && exactNegative) {
      result.status = 'XPASS';
      result.reason = `xfail ${caseDef.xfail.gap} passed`;
    } else if (missGaps.has(gap) && result.observed === 'keep' && caseDef.expect === 'capture') {
      result.status = 'xfail';
      result.reason = caseDef.xfail.reason;
    } else if (falseGrabGaps.has(gap) && exactFalseGrab) {
      result.status = 'xfail';
      result.reason = caseDef.xfail.reason;
    } else if (finalUrlGaps.has(gap) && exactFinalCapture && accepted && stopped) {
      result.status = 'xfail';
      result.reason = caseDef.xfail.reason;
    } else if (finalUrlGaps.has(gap) && exactFinalCapture && accepted && !stopped) {
      result.status = 'FAIL';
      result.reason = stop.reason
        || `xfail ${caseDef.xfail.gap} failed for a different reason (timeout/harness/wrong line)`;
    } else {
      result.status = 'FAIL';
      result.reason = `xfail ${caseDef.xfail.gap} failed for a different reason (timeout/harness/wrong line)`;
    }
  } else if (caseDef.expect === 'capture') {
    if (fullHandoff) {
      result.status = 'pass';
    } else {
      result.status = 'FAIL';
      const bits = [];
      if (!exactPositiveLine) {
        bits.push(`expected one prompt_download url=${expectedUrl} filename=${caseDef.filename} entryPoint=browser_download; got ${JSON.stringify(capture.promptLines.concat(capture.enqueueLines))}`);
      }
      if (exactPositiveLine && !accepted) {
        bits.push('missing or wrong accepted reply (ok, type=accepted, echoed requestId)');
      }
      if (exactPositiveLine && !stopped) {
        bits.push(stop.reason || 'browser download was not shown stopped (erased or USER_CANCELED)');
      }
      result.reason = bits.join('; ') || 'capture failed';
    }
  } else {
    const proof = keepClickProved(caseDef, {
      hits: fixtureHits,
      fixtureServes,
      downloads,
      pageEvidence,
      nonce,
      attempt,
      playwrightDownload: Boolean(playwrightDownload),
      triggerAt,
    });
    if (exactNegative && proof.ok) {
      result.status = 'pass';
    } else {
      result.status = 'FAIL';
      result.reason = exactNegative
        ? (proof.reason || 'keep click not proved')
        : `expected keep (zero capture lines); got ${JSON.stringify(capture.promptLines.concat(capture.enqueueLines))}`;
    }
  }

  result.handoffAccepted = accepted;
  result.downloadCompleted = completed;
  result.browserDownloadStopped = stopped;
  result.downloadSearchError = stop.searchError;
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
  for (const item of results) {
    if (item.status !== 'xfail') continue;
    if (!item.gap || !['G1', 'G4'].includes(item.gap)) continue;
    const capture = classifyCaptureLinesForCase(hostLog, item, item.startedAt, { matchUrl: true });
    if (!capture.captured) continue;
    const first = capture.matchLine ?? capture.captureLines[0];
    item.observed = 'capture';
    item.capturedUrl = first?.payload?.url ?? null;
    item.suggestedFilename = first?.payload?.suggestedFilename ?? null;
    item.entryPoint = first?.payload?.source?.entryPoint ?? null;
    item.hostReceivedAt = first?.receivedAt ?? null;
    if (item.triggerAt != null && item.hostReceivedAt != null) {
      item.latencyMs = item.hostReceivedAt - item.triggerAt;
    }
    if (capture.exactPositive) {
      item.status = 'XPASS';
      item.reason = `xfail ${item.gap} passed`;
    } else {
      item.status = 'FAIL';
      item.reason = `xfail ${item.gap} failed for a different reason (timeout/harness/wrong line)`;
    }
  }
}

export function recheckCaptureDownloads(results, downloads) {
  for (const item of results) {
    if (item.skip || item.status === 'INVALID') continue;
    const captureRow = item.expect === 'capture'
      && item.observed === 'capture'
      && (item.status === 'pass' || item.status === 'xfail' || item.status === 'XPASS');
    if (!captureRow) continue;
    const stop = browserDownloadStoppedFor(downloads, [
      item.expectedUrl,
      item.capturedUrl,
      item.expectedFinalUrl,
    ]);
    if (stop.stopped) continue;
    item.status = 'FAIL';
    item.downloadCompleted = stop.completed;
    item.browserDownloadStopped = false;
    item.downloadSearchError = stop.searchError;
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
