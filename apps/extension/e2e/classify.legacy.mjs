/**
 * Snapshot of the 9227e12 classifier (the bugs F1–F9). Used only by
 * selftest-classifier.mjs and EXT_CLASSIFIER=legacy. Not product code.
 */
export const KEEP_WAIT_MS = 5000;
export const MISS_DRAIN = { minWaitMs: 5000, quietMs: 0, maxWaitMs: 5000 };
export const END_DRAIN = { minWaitMs: 0, quietMs: 0, maxWaitMs: 0 };
export const END_RECHECK = false;
export const INVALIDATE_END_SENTINEL = false;
export const INVALIDATE_BY_REF = false;
export const REQUIRE_ACCEPTED_HANDOFF = false;
export const REQUIRE_KEEP_PROOF = false;

export function parseCookieHeader(value) {
  return String(value);
}

function cookieInHandoff(payload, nonce) {
  if (!payload || typeof payload !== 'object') return false;
  const auth = payload.handoffAuth;
  const blobs = [];
  for (const header of auth?.headers ?? []) blobs.push(header);
  for (const origin of auth?.originAuth ?? []) blobs.push(...(origin.headers ?? []));
  return blobs.some((header) => (
    header.name.toLowerCase() === 'cookie'
    && header.value.includes('rusticdl_e2e=')
    && header.value.includes(nonce)
  ));
}

export function cookieMatchesNonce(payload, nonce) {
  return cookieInHandoff(payload, nonce);
}

export function fidelityFor(caseDef, line, expectedUrl, pageUrl, nonce) {
  const flags = caseDef.fidelity;
  const payload = (line?.payload ?? null);
  const source = (payload?.source ?? null);
  const result = {};
  result.cookies = flags.cookies
    ? (payload && cookieInHandoff(payload, nonce) ? 'pass' : 'fail')
    : 'n/a';
  result.referrer = flags.referrer
    ? (source && source.referrer === pageUrl ? 'pass' : 'fail')
    : 'n/a';
  result.postBody = flags.postBody ? 'fail' : 'n/a';
  result.originalUrl = flags.originalUrl
    ? (payload && payload.url === expectedUrl ? 'pass' : 'fail')
    : 'n/a';
  return result;
}

export function expectedFinalUrl(origin, nonce, attempt) {
  const url = new URL('/C10/final', origin.endsWith('/') ? origin : `${origin}/`);
  url.searchParams.set('nonce', nonce);
  url.searchParams.set('attempt', attempt);
  return url.href;
}

export function isAcceptedReply() {
  return true;
}

export function classifyCase(input) {
  const {
    caseDef,
    expectedUrl,
    pageUrl,
    nonce,
    startedAt,
    triggerAt,
    promptLines,
    enqueueLines,
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
    startedAt,
    filename: caseDef.filename,
    skip: false,
    handoffAccepted: true,
    downloadCompleted: false,
  };

  if (caseDef.skip) {
    result.observed = 'skip';
    result.status = 'skip';
    result.reason = caseDef.skip;
    result.skip = true;
    return result;
  }

  const captured = promptLines.length + enqueueLines.length > 0;
  result.observed = captured ? 'capture' : 'keep';
  const matchLine = promptLines.find((line) => {
    const payload = line.payload ?? {};
    return payload.url === expectedUrl
      && payload.suggestedFilename === caseDef.filename
      && payload.source?.entryPoint === 'browser_download';
  }) ?? null;
  const firstPrompt = promptLines[0] ?? null;
  if (firstPrompt) {
    result.capturedUrl = firstPrompt.payload?.url ?? null;
    result.suggestedFilename = firstPrompt.payload?.suggestedFilename ?? null;
    result.entryPoint = firstPrompt.payload?.source?.entryPoint ?? null;
    result.hostReceivedAt = firstPrompt.receivedAt ?? null;
    if (result.triggerAt != null && result.hostReceivedAt != null) {
      result.latencyMs = result.hostReceivedAt - result.triggerAt;
    }
  }
  result.fidelity = fidelityFor(caseDef, (matchLine ?? firstPrompt), expectedUrl, pageUrl, nonce);
  if (caseDef.fidelity.postBody) {
    result.fidelity.postBody = 'fail';
    result.reason = [result.reason, 'protocol has no POST body field'].filter(Boolean).join('; ');
  }

  const exactPositive = Boolean(matchLine) && promptLines.length === 1 && enqueueLines.length === 0;
  const exactNegative = promptLines.length === 0 && enqueueLines.length === 0;
  const missGaps = new Set(['G1', 'G4']);
  const falseGrabGaps = new Set(['G3']);
  const finalUrlGaps = new Set(['G5']);
  const gap = caseDef.xfail?.gap ?? '';

  if (caseDef.xfail) {
    const failedForReason = (
      (missGaps.has(gap) && result.observed === 'keep' && caseDef.expect === 'capture')
      || (falseGrabGaps.has(gap) && result.observed === 'capture' && caseDef.expect === 'keep')
      || (
        finalUrlGaps.has(gap)
        && result.observed === 'capture'
        && caseDef.expect === 'capture'
        && typeof result.capturedUrl === 'string'
        && result.capturedUrl.includes('/C10/final')
        && result.suggestedFilename === caseDef.filename
        && result.entryPoint === 'browser_download'
      )
    );
    if (caseDef.expect === 'capture' && exactPositive) {
      result.status = 'XPASS';
      result.reason = `xfail ${caseDef.xfail.gap} passed`;
    } else if (caseDef.expect === 'keep' && exactNegative) {
      result.status = 'XPASS';
      result.reason = `xfail ${caseDef.xfail.gap} passed`;
    } else if (failedForReason) {
      result.status = 'xfail';
      result.reason = caseDef.xfail.reason;
    } else {
      result.status = 'FAIL';
      result.reason = `xfail ${caseDef.xfail.gap} failed for a different reason (timeout/harness/wrong line)`;
    }
  } else if (caseDef.expect === 'capture') {
    result.status = exactPositive ? 'pass' : 'FAIL';
    if (!exactPositive) {
      result.reason = `expected one prompt_download url=${expectedUrl} filename=${caseDef.filename} entryPoint=browser_download`;
    }
  } else {
    result.status = exactNegative ? 'pass' : 'FAIL';
    if (!exactNegative) {
      result.reason = 'expected keep (zero capture lines)';
    }
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

export function recheckLateMissXfails() {
  return;
}

export function hostFailedHandoff() {
  return false;
}
