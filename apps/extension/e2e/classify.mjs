/**
 * Capture-matrix classifier (fixed). Used by extension-capture.spec.ts and
 * selftest-classifier.mjs. Test-only; not product code.
 */
import { EXPECTED_POST_BODY } from './ext-attack.mjs';

export const KEEP_WAIT_MS = 5000;
export const MISS_DRAIN = { minWaitMs: 5000, quietMs: 3000, maxWaitMs: 20_000, waitOutEmpty: true };
export const END_DRAIN = { minWaitMs: 0, quietMs: 3000, maxWaitMs: 20_000, waitOutEmpty: false };
export const END_RECHECK = true;
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
  const items = Array.isArray(downloads) ? downloads : [];
  const wanted = new Set((urls || []).filter((url) => typeof url === 'string' && url));
  return items.some((item) => item && wanted.has(item.url) && item.state === 'complete');
}

export function exactPromptMatch(line, { url, filename, entryPoint = 'browser_download', startedAt }) {
  if (!line || line.type !== 'prompt_download') return false;
  if (typeof startedAt === 'number' && !(line.receivedAt >= startedAt)) return false;
  const payload = line.payload ?? {};
  return payload.url === url
    && payload.suggestedFilename === filename
    && payload.source?.entryPoint === entryPoint;
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
 * - G1/G4 exactPositiveLine → XPASS (`late-c1` / `late-c4`); wrong shape → FAIL (`late-c1-nullfile`)
 * - G1/G4 empty keep → xfail; observed capture that is not exact → FAIL (`plant-c1`)
 * - expect-capture xfail + fullHandoff → XPASS (G5 prefix rewrite)
 * - expect-keep xfail + exactNegative → XPASS (no keep-xfail rows today)
 * - G3 exactFalseGrab → xfail; other/super/badfile/badep → FAIL (`g3-other`)
 * - G5 exactFinal + accepted + !complete → xfail; garbage/complete → FAIL (`c10-wrong-shape`)
 * - capture fullHandoff → pass; missing accepted or complete download → FAIL (`wrong-shape`)
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

  const matchLine = promptLines.find((line) => exactPromptMatch(line, {
    url: expectedUrl,
    filename: caseDef.filename,
    startedAt,
  })) ?? null;
  const prompt = matchLine ?? firstPrompt(promptLines);
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

  const captured = promptLines.length + enqueueLines.length > 0;
  result.observed = captured ? 'capture' : 'keep';

  const exactPositiveLine = Boolean(matchLine) && promptLines.length === 1 && enqueueLines.length === 0;
  const exactNegative = promptLines.length === 0 && enqueueLines.length === 0;
  const accepted = Boolean(prompt && isAcceptedReply(prompt));
  const completed = downloadCompletedFor(downloads, [expectedUrl, result.capturedUrl, finalUrl]);
  const fullHandoff = exactPositiveLine && Boolean(matchLine && isAcceptedReply(matchLine)) && !completed;

  const gap = caseDef.xfail?.gap ?? '';
  const missGaps = new Set(['G1', 'G4']);
  const falseGrabGaps = new Set(['G3']);
  const finalUrlGaps = new Set(['G5']);

  const exactFalseGrab = promptLines.length === 1
    && enqueueLines.length === 0
    && exactPromptMatch(promptLines[0], {
      url: expectedUrl,
      filename: caseDef.filename,
      startedAt,
    });

  const exactFinalCapture = promptLines.length === 1
    && enqueueLines.length === 0
    && exactPromptMatch(promptLines[0], {
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
    } else if (finalUrlGaps.has(gap) && exactFinalCapture && accepted && !completed) {
      result.status = 'xfail';
      result.reason = caseDef.xfail.reason;
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
        bits.push(`expected one prompt_download url=${expectedUrl} filename=${caseDef.filename} entryPoint=browser_download; got ${JSON.stringify(promptLines.concat(enqueueLines))}`);
      }
      if (exactPositiveLine && !accepted) {
        bits.push('missing or wrong accepted reply (ok, type=accepted, echoed requestId)');
      }
      if (exactPositiveLine && completed) {
        bits.push('browser download completed; capture requires erase/cancel, not state complete');
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
        : `expected keep (zero capture lines); got ${JSON.stringify(promptLines.concat(enqueueLines))}`;
    }
  }

  result.handoffAccepted = accepted;
  result.downloadCompleted = completed;
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

function promptForCaseUrl(line, expectedUrl, startedAt) {
  if (!line || line.type !== 'prompt_download') return false;
  if (typeof startedAt === 'number' && !(line.receivedAt >= startedAt)) return false;
  return (line.payload ?? {}).url === expectedUrl;
}

export function recheckLateMissXfails(results, hostLog) {
  for (const item of results) {
    if (item.status !== 'xfail') continue;
    if (!item.gap || !['G1', 'G4'].includes(item.gap)) continue;
    const lines = (hostLog || []).filter((line) => promptForCaseUrl(line, item.expectedUrl, item.startedAt));
    if (lines.length === 0) continue;
    const match = lines.length === 1
      && exactPromptMatch(lines[0], {
        url: item.expectedUrl,
        filename: item.filename,
        startedAt: item.startedAt,
      })
      ? lines[0]
      : null;
    const first = lines[0];
    item.observed = 'capture';
    item.capturedUrl = (match ?? first)?.payload?.url ?? null;
    item.suggestedFilename = (match ?? first)?.payload?.suggestedFilename ?? null;
    item.entryPoint = (match ?? first)?.payload?.source?.entryPoint ?? null;
    item.hostReceivedAt = (match ?? first)?.receivedAt ?? null;
    if (item.triggerAt != null && item.hostReceivedAt != null) {
      item.latencyMs = item.hostReceivedAt - item.triggerAt;
    }
    if (match) {
      item.status = 'XPASS';
      item.reason = `xfail ${item.gap} passed`;
    } else {
      item.status = 'FAIL';
      item.reason = `xfail ${item.gap} failed for a different reason (timeout/harness/wrong line)`;
    }
  }
}

export function hostFailedHandoff(results) {
  return results.some((item) => (
    item.expect === 'capture'
    && item.observed === 'capture'
    && (item.handoffAccepted === false || item.downloadCompleted === true)
    && item.status === 'FAIL'
  ));
}
