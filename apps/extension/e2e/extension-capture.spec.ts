import { expect, test } from '@playwright/test';
import {
  chmodSync,
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  captureLinesSince,
  extensionPaths,
  isolateHome,
  launchExtensionContext,
  percentile,
  pingNativeHost,
  readHostLog,
  searchDownloads,
  waitForServiceWorker,
  writeNativeHostManifest,
} from './harness.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const paths = extensionPaths(path.resolve(here, '..'));
const casesPath = path.join(paths.repoRoot, '.cursor/skills/verify-rusticdl/scripts/ext-cases.json');
const matrix = JSON.parse(readFileSync(casesPath, 'utf8')) as Matrix;

type FidelityFlags = {
  cookies: boolean;
  referrer: boolean;
  postBody: boolean;
  originalUrl: boolean;
};

type CaseDef = {
  id: string;
  route: string;
  page: string;
  trigger: string;
  expect: 'capture' | 'keep';
  filename: string | null;
  query?: Record<string, string>;
  xfail: null | { gap: string; reason: string };
  skip: string | null;
  fidelity: FidelityFlags;
};

type Matrix = {
  cases: CaseDef[];
  live: Array<{ id: string; expect: string; note: string }>;
  negativeWaitMs: number;
};

type CaseResult = {
  id: string;
  expect: 'capture' | 'keep';
  observed: 'capture' | 'keep' | 'skip' | 'error';
  status: 'pass' | 'xfail' | 'skip' | 'FAIL' | 'XPASS' | 'INVALID';
  gap: string | null;
  reason: string | null;
  evidencePath: string;
  triggerAt: number | null;
  hostReceivedAt: number | null;
  latencyMs: number | null;
  capturedUrl: string | null;
  suggestedFilename: string | null;
  entryPoint: string | null;
  fidelity: Record<string, 'pass' | 'fail' | 'n/a'>;
  invalid: boolean;
};

function envOr(name: string, fallback: string): string {
  return process.env[name] && process.env[name] !== '' ? process.env[name] as string : fallback;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function fileUrlFor(caseDef: CaseDef, origin: string, nonce: string, attempt: string): string {
  const url = new URL(caseDef.route, origin);
  url.searchParams.set('nonce', nonce);
  url.searchParams.set('attempt', attempt);
  for (const [key, value] of Object.entries(caseDef.query ?? {})) {
    url.searchParams.set(key, value);
  }
  return url.href;
}

function pageUrlFor(caseDef: CaseDef, origin: string, nonce: string, attempt: string): string {
  const url = new URL(caseDef.page, origin);
  url.searchParams.set('nonce', nonce);
  url.searchParams.set('attempt', attempt);
  return url.href;
}

function cookieInHandoff(payload: Record<string, unknown> | undefined, nonce: string): boolean {
  if (!payload || typeof payload !== 'object') return false;
  const auth = payload.handoffAuth as {
    headers?: Array<{ name: string; value: string }>;
    originAuth?: Array<{ origin: string; headers: Array<{ name: string; value: string }> }>;
  } | undefined;
  const blobs: Array<{ name: string; value: string }> = [];
  for (const header of auth?.headers ?? []) blobs.push(header);
  for (const origin of auth?.originAuth ?? []) blobs.push(...origin.headers);
  return blobs.some((header) => (
    header.name.toLowerCase() === 'cookie'
    && header.value.includes('rusticdl_e2e=')
    && header.value.includes(nonce)
  ));
}

function fidelityFor(caseDef: CaseDef, line: Record<string, unknown> | null, expectedUrl: string, pageUrl: string, nonce: string) {
  const flags = caseDef.fidelity;
  const payload = (line?.payload ?? null) as Record<string, unknown> | null;
  const source = (payload?.source ?? null) as Record<string, unknown> | null;
  const result: Record<string, 'pass' | 'fail' | 'n/a'> = {};

  result.cookies = flags.cookies
    ? (payload && cookieInHandoff(payload, nonce) ? 'pass' : 'fail')
    : 'n/a';
  result.referrer = flags.referrer
    ? (source && source.referrer === pageUrl ? 'pass' : 'fail')
    : 'n/a';
  result.postBody = flags.postBody
    ? 'fail'
    : 'n/a';
  result.originalUrl = flags.originalUrl
    ? (payload && payload.url === expectedUrl ? 'pass' : 'fail')
    : 'n/a';
  return result;
}

async function waitForCapture(runDir: string, startedAt: number, timeoutMs: number) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const lines = captureLinesSince(runDir, startedAt);
    if (lines.length > 0) return lines;
    await sleep(50);
  }
  return captureLinesSince(runDir, startedAt);
}

test('extension capture matrix', async () => {
  const runDir = envOr('VERIFY_RUN_DIR', `/tmp/rusticdl-verify-e2e-${Date.now()}`);
  const nonce = envOr('EXT_NONCE', crypto.randomUUID());
  const fixtureOrigin = envOr('EXT_FIXTURE_ORIGIN', 'http://127.0.0.1:18765');
  const fixtureLocalhost = envOr('EXT_FIXTURE_LOCALHOST', 'http://localhost:18765');
  const onlyCase = process.env.EXT_CASE || '';
  const repeat = Math.max(1, Number(process.env.EXT_REPEAT || '1'));
  const live = process.env.EXT_LIVE === '1';
  const evidenceRoot = path.join(runDir, 'evidence', 'extension');
  mkdirSync(evidenceRoot, { recursive: true });

  if (!existsSync(path.join(paths.distChromium, 'manifest.json'))) {
    throw new Error(`missing ${paths.distChromium}/manifest.json`);
  }
  chmodSync(paths.hostShim, 0o755);

  const isolated = isolateHome(runDir);
  const userDataDir = envOr('EXT_USER_DATA_DIR', path.join(runDir, 'chromium-profile'));
  const manifestPath = writeNativeHostManifest({
    userDataDir,
    hostPath: paths.hostShim,
    allowedOrigin: paths.extensionOrigin,
  });

  const launched = await launchExtensionContext({
    userDataDir,
    isolatedHome: isolated.home,
    runDir,
    distChromium: paths.distChromium,
  });
  const context = launched.context;

  try {
    const worker = await waitForServiceWorker(context);
    const extensionId = new URL(worker.url()).host;
    const pingId = `doctor-${Date.now()}`;
    const pingBefore = Date.now();
    const pong = await pingNativeHost(worker, pingId);
    const pingLine = readHostLog(runDir).find((line) => line.requestId === pingId)
      ?? readHostLog(runDir).filter((line) => line.type === 'ping' && line.receivedAt >= pingBefore).at(-1)
      ?? null;
    const doctor = {
      ok: pong?.ok === true && pong?.type === 'pong',
      browserVersion: launched.browserVersion,
      executablePath: launched.executablePath,
      extensionId,
      manifestPath,
      pong,
      pingLine,
    };
    writeFileSync(path.join(runDir, 'doctor.json'), `${JSON.stringify(doctor, null, 2)}\n`);
    expect(doctor.ok, `sentinel ping failed: ${JSON.stringify(pong)}`).toBe(true);
    expect(extensionId).toBe(paths.identity.id);

    const selected = matrix.cases.filter((row) => !onlyCase || row.id === onlyCase || row.id === 'C2');
    const latencies: number[] = [];
    const results: CaseResult[] = [];

    const runCase = async (caseDef: CaseDef, evidenceId: string): Promise<CaseResult> => {
      const evidenceDir = path.join(evidenceRoot, evidenceId);
      mkdirSync(evidenceDir, { recursive: true });
      const attempt = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      const pageUrl = pageUrlFor(caseDef, fixtureOrigin, nonce, attempt);
      const fallbackUrl = caseDef.id === 'C12'
        ? fileUrlFor(caseDef, fixtureLocalhost, nonce, attempt)
        : fileUrlFor(caseDef, fixtureOrigin, nonce, attempt);
      const result: CaseResult = {
        id: caseDef.id,
        expect: caseDef.expect,
        observed: 'keep',
        status: 'FAIL',
        gap: caseDef.xfail?.gap ?? null,
        reason: null,
        evidencePath: evidenceDir,
        triggerAt: null,
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
      };

      if (caseDef.skip) {
        result.observed = 'skip';
        result.status = 'skip';
        result.reason = caseDef.skip;
        writeFileSync(path.join(evidenceDir, 'result.json'), `${JSON.stringify(result, null, 2)}\n`);
        writeFileSync(path.join(evidenceDir, 'host-log.jsonl'), '');
        writeFileSync(path.join(evidenceDir, 'downloads.json'), '[]\n');
        return result;
      }

      const page = context.pages()[0] || await context.newPage();
      await page.goto(pageUrl, { waitUntil: 'domcontentloaded' });
      const locator = page.locator(`[data-case="${caseDef.id}"]`);
      await locator.waitFor({ state: 'visible', timeout: 10_000 });
      const expectedUrl = await page.evaluate((id) => {
        const el = document.querySelector(`[data-case="${id}"]`);
        if (!el) return '';
        if (el instanceof HTMLAnchorElement && el.href && !el.href.startsWith('blob:') && !el.href.startsWith('data:')) {
          return el.href;
        }
        if (el instanceof HTMLButtonElement && el.form?.action) return el.form.action;
        const raw = el.getAttribute('data-file-url');
        if (raw) return new URL(raw, location.href).href;
        return '';
      }, caseDef.id) || fallbackUrl;

      const startedAt = Date.now();
      const triggerAt = await page.evaluate(() => Date.now());
      result.triggerAt = triggerAt;
      const popupPromise = caseDef.trigger === 'target-blank'
        ? page.waitForEvent('popup', { timeout: 8_000 }).catch(() => null)
        : Promise.resolve(null);

      await locator.click({ timeout: 10_000 });
      await popupPromise;

      const missXfail = Boolean(caseDef.xfail && ['G1', 'G4'].includes(caseDef.xfail.gap));
      const keepWait = (caseDef.expect === 'keep' && !caseDef.xfail) || missXfail;
      const waitMs = keepWait
        ? matrix.negativeWaitMs
        : caseDef.id === 'L1'
          ? 20_000
          : 10_000;
      const captureWait = keepWait
        ? sleep(waitMs).then(() => captureLinesSince(runDir, startedAt))
        : waitForCapture(runDir, startedAt, waitMs);
      const lines = await captureWait;
      const hostSlice = readHostLog(runDir).filter((line) => line.receivedAt >= startedAt);
      writeFileSync(
        path.join(evidenceDir, 'host-log.jsonl'),
        hostSlice.map((line) => JSON.stringify(line)).join('\n') + (hostSlice.length ? '\n' : ''),
      );

      const promptLines = lines.filter((line) => line.type === 'prompt_download');
      const enqueueLines = lines.filter((line) => line.type === 'enqueue_download');
      const sw = context.serviceWorkers()[0] || worker;
      const downloads = await searchDownloads(sw);
      writeFileSync(path.join(evidenceDir, 'downloads.json'), `${JSON.stringify(downloads, null, 2)}\n`);

      const shotPage = context.pages().at(-1) ?? page;
      try {
        await shotPage.screenshot({ path: path.join(evidenceDir, 'page.png'), fullPage: true });
      } catch {
        writeFileSync(path.join(evidenceDir, 'page.png.missing.txt'), 'screenshot failed\n');
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
      result.fidelity = fidelityFor(
        caseDef,
        (matchLine ?? firstPrompt) as Record<string, unknown> | null,
        expectedUrl,
        pageUrl,
        nonce,
      );
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
          result.reason = `expected one prompt_download url=${expectedUrl} filename=${caseDef.filename} entryPoint=browser_download; got ${JSON.stringify(promptLines.concat(enqueueLines))}`;
        }
      } else {
        result.status = exactNegative ? 'pass' : 'FAIL';
        if (!exactNegative) {
          result.reason = `expected keep (zero capture lines in ${matrix.negativeWaitMs}ms); got ${JSON.stringify(promptLines.concat(enqueueLines))}`;
        }
      }

      if (result.latencyMs != null && result.observed === 'capture') {
        latencies.push(result.latencyMs);
      }
      writeFileSync(path.join(evidenceDir, 'result.json'), `${JSON.stringify({ ...result, expectedUrl, pageUrl, nonce }, null, 2)}\n`);
      return result;
    };

    const runSentinel = async (label: string): Promise<boolean> => {
      const sentinel = matrix.cases.find((row) => row.id === 'C2');
      if (!sentinel) throw new Error('C2 missing from ext-cases.json');
      const sent = await runCase(sentinel, label);
      results.push(sent);
      return sent.status === 'pass' && sent.observed === 'capture';
    };

    for (let round = 0; round < repeat; round += 1) {
      const suffix = repeat > 1 ? `-r${round + 1}` : '';
      const positives = selected.filter((row) => row.expect === 'capture' && !row.skip);
      const negatives = selected.filter((row) => row.expect === 'keep' && row.id !== 'C2');

      for (const row of positives) {
        if (row.id === 'C2') continue;
        results.push(await runCase(row, `${row.id}${suffix}`));
      }
      const c2 = selected.find((row) => row.id === 'C2');
      if (c2) results.push(await runCase(c2, `C2${suffix}`));

      const negativeIds: string[] = [];
      for (const row of negatives) {
        const item = await runCase(row, `${row.id}${suffix}`);
        results.push(item);
        negativeIds.push(item.id);
      }
      const sentinelOk = await runSentinel(`C2-sentinel-negatives${suffix}`);
      if (!sentinelOk) {
        for (const item of results) {
          if (negativeIds.includes(item.id) && item.status === 'pass') {
            item.status = 'INVALID';
            item.invalid = true;
            item.reason = 'sentinel C2 did not capture; negative keep is INVALID';
            writeFileSync(path.join(item.evidencePath, 'result.json'), `${JSON.stringify(item, null, 2)}\n`);
          }
        }
      }
      const endOk = await runSentinel(`C2-sentinel-end${suffix}`);
      if (!endOk) {
        console.error('sentinel C2 at end of run did not capture');
      }
    }

    if (live) {
      writeFileSync(
        path.join(evidenceRoot, 'live-report.json'),
        `${JSON.stringify({ note: 'live sites report-only; not CI-gated', sites: matrix.live }, null, 2)}\n`,
      );
      console.log('LIVE report-only (not driven):');
      for (const site of matrix.live) {
        console.log(`LIVE ${site.id} expect=${site.expect} ${site.note}`);
      }
    }

    const latestById = new Map<string, CaseResult>();
    for (const item of results) {
      if (!item.id.startsWith('C2-') && !item.evidencePath.includes('sentinel')) {
        latestById.set(item.id, item);
      }
    }
    for (const item of results) {
      if (['C1', 'C2', 'C3', 'C4', 'C5', 'C6', 'C7', 'C8', 'C9', 'C10', 'C11', 'C12', 'N1', 'N2', 'N3', 'N4', 'N5', 'N6', 'L1', 'L2', 'L3'].includes(item.id)
        && !item.evidencePath.includes('sentinel')) {
        latestById.set(item.id, item);
      }
    }

    const scored = [...latestById.values()];
    const positives = matrix.cases.filter((row) => row.expect === 'capture' && !row.skip);
    const negativeDefs = matrix.cases.filter((row) => row.expect === 'keep' && !row.skip);
    const capturedPositives = positives.filter((row) => latestById.get(row.id)?.observed === 'capture').length;
    const falseGrabs = negativeDefs.filter((row) => latestById.get(row.id)?.observed === 'capture').length;
    const skipRows = matrix.cases.filter((row) => row.skip);
    const metrics = {
      captureRate: {
        capturedPositives,
        positiveCases: positives.length,
        value: positives.length ? capturedPositives / positives.length : null,
      },
      falseGrabRate: {
        negativeWithCapture: falseGrabs,
        negativeCases: negativeDefs.length,
        value: negativeDefs.length ? falseGrabs / negativeDefs.length : null,
      },
      clickToHostLatencyMs: {
        samples: latencies,
        n: latencies.length,
        p50: percentile(latencies, 50),
        p95: percentile(latencies, 95),
        perCase: scored
          .filter((row) => row.latencyMs != null)
          .map((row) => ({ id: row.id, latencyMs: row.latencyMs })),
      },
      fidelity: scored.map((row) => ({ id: row.id, fidelity: row.fidelity })),
      skips: skipRows.map((row) => ({ id: row.id, reason: row.skip })),
    };
    writeFileSync(path.join(evidenceRoot, 'metrics.json'), `${JSON.stringify(metrics, null, 2)}\n`);
    const extMetrics = `EXT-METRICS capture_rate=${capturedPositives}/${positives.length} false_grab_rate=${falseGrabs}/${negativeDefs.length} latency_ms p50=${metrics.clickToHostLatencyMs.p50} p95=${metrics.clickToHostLatencyMs.p95} n=${latencies.length} skips=${skipRows.length}`;
    console.log(extMetrics);
    for (const skip of skipRows) {
      console.log(`SKIP ${skip.id} ${skip.skip}`);
    }
    writeFileSync(path.join(runDir, 'matrix-results.json'), `${JSON.stringify(results, null, 2)}\n`);

    const blockers = results.filter((row) => row.status === 'FAIL' || row.status === 'XPASS' || row.status === 'INVALID');
    const c2 = latestById.get('C2');
    const n1 = latestById.get('N1');
    if (!onlyCase || onlyCase === 'C2') {
      expect(c2?.status, `C2 must pass, got ${JSON.stringify(c2)}`).toBe('pass');
    }
    if (!onlyCase || onlyCase === 'N1') {
      expect(n1?.status, `N1 must pass, got ${JSON.stringify(n1)}`).toBe('pass');
    }
    expect(blockers, `blocking results: ${JSON.stringify(blockers, null, 2)}`).toHaveLength(0);
  } finally {
    await context.close();
  }
});
