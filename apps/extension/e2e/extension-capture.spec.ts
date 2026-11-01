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
  drainCaptureLines,
  extensionPaths,
  isolateHome,
  launchExtensionContext,
  percentile,
  pingNativeHost,
  readFixtureHitsEvidence,
  readFixtureServesEvidence,
  readHostLog,
  readHostLogEvidence,
  searchDownloads,
  waitForServiceWorker,
  writeNativeHostManifest,
} from './harness.mjs';
import { plantEndCompleteDownloads } from './ext-attack.mjs';
import * as classify from './classify.mjs';

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
  expectedUrl?: string;
  expectedFinalUrl?: string | null;
  startedAt?: number;
  filename?: string | null;
  attempt?: string;
  nonce?: string;
  handoffAccepted?: boolean;
  downloadCompleted?: boolean;
  browserDownloadStopped?: boolean;
  downloadSearchError?: boolean;
  pageEvidence?: Record<string, unknown>;
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

async function waitForCapture(runDir: string, startedAt: number, timeoutMs: number) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const lines = captureLinesSince(runDir, startedAt);
    if (lines.length > 0) return lines;
    await sleep(50);
  }
  return captureLinesSince(runDir, startedAt);
}

function persistResult(result: CaseResult, extra: Record<string, unknown> = {}) {
  writeFileSync(
    path.join(result.evidencePath, 'result.json'),
    `${JSON.stringify({ ...result, ...extra }, null, 2)}\n`,
  );
}

function logCase(evidenceId: string, result: CaseResult) {
  // Status is not final until invalidateNegatives / end rechecks. Never print
  // `pass` here — a later INVALID must not already have been logged as pass.
  console.log(
    `${evidenceId}  staged  obs=${result.observed}  url=${result.capturedUrl ?? ''}  file=${result.suggestedFilename ?? ''}  ep=${result.entryPoint ?? ''}  reason=${result.reason ?? ''}`,
  );
}

test('extension capture matrix', async () => {
  const runDir = envOr('VERIFY_RUN_DIR', `/tmp/rusticdl-verify-e2e-${Date.now()}`);
  const nonce = envOr('EXT_NONCE', crypto.randomUUID());
  const fixtureOrigin = envOr('EXT_FIXTURE_ORIGIN', 'http://127.0.0.1:18765');
  const fixtureLocalhost = envOr('EXT_FIXTURE_LOCALHOST', 'http://localhost:18765');
  const onlyCases = (process.env.EXT_CASE || '').split(',').map((id) => id.trim()).filter(Boolean);
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

    const selected = matrix.cases.filter((row) => (
      onlyCases.length === 0
      || onlyCases.includes(row.id)
      || row.id === 'C2'
    ));
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

      if (caseDef.skip) {
        const skipped = classify.classifyCase({
          caseDef,
          expectedUrl: fallbackUrl,
          expectedFinalUrl: null,
          pageUrl,
          nonce,
          attempt,
          startedAt: Date.now(),
          triggerAt: null,
          promptLines: [],
          enqueueLines: [],
          downloads: [],
          fixtureHits: [],
          pageEvidence: {},
        }) as CaseResult;
        skipped.evidencePath = evidenceDir;
        persistResult(skipped, { expectedUrl: fallbackUrl, pageUrl, nonce, attempt });
        writeFileSync(path.join(evidenceDir, 'host-log.jsonl'), '');
        writeFileSync(path.join(evidenceDir, 'downloads.json'), '[]\n');
        logCase(evidenceId, skipped);
        return skipped;
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
      const expectedFinal = caseDef.id === 'C10'
        ? classify.expectedFinalUrl(fixtureOrigin, nonce, attempt)
        : null;

      const startedAt = Date.now();
      const triggerAt = await page.evaluate(() => Date.now());
      const popupPromise = caseDef.trigger === 'target-blank'
        ? page.waitForEvent('popup', { timeout: 8_000 }).catch(() => null)
        : Promise.resolve(null);
      const downloadPromise = (caseDef.id === 'C5' || caseDef.id === 'C6')
        ? page.waitForEvent('download', { timeout: 8_000 }).catch(() => null)
        : Promise.resolve(null);

      await locator.click({ timeout: 10_000 });
      await popupPromise;
      const playwrightDownload = await downloadPromise;
      if (caseDef.trigger === 'fetch-attachment') {
        await page.locator('[data-fetch-attempt]').waitFor({ state: 'attached', timeout: 2000 }).catch(() => null);
      }

      const missXfail = Boolean(caseDef.xfail && ['G1', 'G4'].includes(caseDef.xfail.gap));
      const keepWait = (caseDef.expect === 'keep' && !caseDef.xfail) || missXfail;
      let lines;
      if (missXfail) {
        lines = await drainCaptureLines(runDir, startedAt, classify.MISS_DRAIN);
      } else if (keepWait) {
        lines = await drainCaptureLines(runDir, startedAt, {
          minWaitMs: matrix.negativeWaitMs,
          quietMs: 0,
          maxWaitMs: matrix.negativeWaitMs,
        });
      } else {
        const waitMs = caseDef.id === 'L1' ? 20_000 : 10_000;
        lines = await waitForCapture(runDir, startedAt, waitMs);
      }
      const hostSlice = readHostLog(runDir).filter((line) => line.receivedAt >= startedAt);
      writeFileSync(
        path.join(evidenceDir, 'host-log.jsonl'),
        hostSlice.map((line) => JSON.stringify(line)).join('\n') + (hostSlice.length ? '\n' : ''),
      );

      const split = classify.classifyCaptureLinesForCase(lines, {
        url: expectedUrl,
        expectedUrl,
        filename: caseDef.filename,
      }, startedAt);
      const promptLines = split.promptLines;
      const enqueueLines = split.enqueueLines;
      const sw = context.serviceWorkers()[0] || worker;
      const matchPreview = [...promptLines, ...enqueueLines].find((line) => (
        (line.payload ?? {}).url === expectedUrl
        || (expectedFinal && (line.payload ?? {}).url === expectedFinal)
      ));
      const trackedUrls = [expectedUrl, expectedFinal, matchPreview?.payload?.url].filter(Boolean);
      let downloads = await searchDownloads(sw);
      if (caseDef.expect === 'capture' && (promptLines.length + enqueueLines.length) > 0) {
        // A one-shot body can reach state=complete while the host reply is
        // still in flight. downloadItemsTerminal treats complete as terminal,
        // which would freeze that snapshot and hide the later erase. Keep
        // polling until the row is gone, interrupted, or the settle budget
        // ends. A row that is still complete at the end is still a failure.
        const settleUntil = Date.now() + classify.DOWNLOAD_SETTLE.maxWaitMs;
        while (Date.now() < settleUntil) {
          if (classify.downloadSearchFailed(downloads)) break;
          const matched = classify.matchingDownloadItems(downloads, trackedUrls);
          const completeLeft = matched.some((item: { state?: string }) => item?.state === 'complete');
          if (classify.downloadItemsTerminal(downloads, trackedUrls) && !completeLeft) break;
          await sleep(classify.DOWNLOAD_SETTLE.pollMs);
          downloads = await searchDownloads(sw);
        }
      }
      writeFileSync(path.join(evidenceDir, 'downloads.json'), `${JSON.stringify(downloads, null, 2)}\n`);

      const shotPage = page;
      try {
        await shotPage.screenshot({ path: path.join(evidenceDir, 'page.png'), fullPage: true });
      } catch {
        writeFileSync(path.join(evidenceDir, 'page.png.missing.txt'), 'screenshot failed\n');
      }
      const pageEvidence = await shotPage.evaluate(() => {
        const root = document.querySelector('[data-page]');
        return {
          href: location.href,
          title: document.title,
          text: (document.body && document.body.innerText ? document.body.innerText : '').slice(0, 2000),
          dataPage: root?.getAttribute('data-page') || null,
          fetchStatus: root?.getAttribute('data-fetch-status') || null,
          fetchBytes: root?.getAttribute('data-fetch-bytes') || null,
          fetchNonce: root?.getAttribute('data-fetch-nonce') || null,
          fetchAttempt: root?.getAttribute('data-fetch-attempt') || null,
        };
      }).catch(() => ({
        href: '',
        title: '',
        text: '',
        dataPage: null,
        fetchStatus: null,
        fetchBytes: null,
        fetchNonce: null,
        fetchAttempt: null,
      }));

      const hostEv = readHostLogEvidence(runDir);
      const hitEv = readFixtureHitsEvidence(runDir);
      const serveEv = readFixtureServesEvidence(runDir);
      const fixtureHits = (hitEv.ok ? hitEv.items : []).filter((hit: { attempt?: string }) => hit.attempt === attempt);
      const fixtureServes = (serveEv.ok ? serveEv.items : []).filter((row: { attempt?: string }) => row.attempt === attempt);
      writeFileSync(path.join(evidenceDir, 'fixture-hits.json'), `${JSON.stringify(fixtureHits, null, 2)}\n`);
      writeFileSync(path.join(evidenceDir, 'fixture-serves.json'), `${JSON.stringify(fixtureServes, null, 2)}\n`);

      const classified = classify.classifyCase({
        caseDef,
        expectedUrl,
        expectedFinalUrl: expectedFinal,
        pageUrl,
        nonce,
        attempt,
        startedAt,
        triggerAt,
        promptLines,
        enqueueLines,
        hostLog: hostEv.ok ? hostSlice : { ok: false, reason: hostEv.reason, items: [] },
        hostLogOk: hostEv.ok,
        hostLogReason: hostEv.reason,
        downloads,
        downloadsOk: classify.downloadSearchFailed(downloads) || Array.isArray(downloads),
        downloadsReason: classify.downloadSearchFailed(downloads)
          ? `chrome.downloads.search failed: ${(downloads as { error?: string }).error}`
          : undefined,
        fixtureHits,
        fixtureServes,
        fixtureServesOk: serveEv.ok,
        fixtureServesReason: serveEv.reason,
        pageEvidence,
        pageEvidenceOk: Boolean(pageEvidence && typeof pageEvidence === 'object'),
        playwrightDownload: Boolean(playwrightDownload),
      }) as CaseResult;
      classified.evidencePath = evidenceDir;
      classified.pageEvidence = pageEvidence;
      persistResult(classified, { expectedUrl, expectedFinalUrl: expectedFinal, pageUrl, nonce, attempt, pageEvidence });
      if (classified.latencyMs != null && classified.observed === 'capture') {
        latencies.push(classified.latencyMs);
      }
      logCase(evidenceId, classified);
      return classified;
    };

    const runSentinel = async (label: string): Promise<CaseResult> => {
      const sentinel = matrix.cases.find((row) => row.id === 'C2');
      if (!sentinel) throw new Error('C2 missing from ext-cases.json');
      const sent = await runCase(sentinel, label);
      results.push(sent);
      return sent;
    };

    for (let round = 0; round < repeat; round += 1) {
      const suffix = repeat > 1 ? `-r${round + 1}` : '';
      const positives = selected.filter((row) => row.expect === 'capture' && !row.skip);
      const negatives = selected.filter((row) => row.expect === 'keep' && row.id !== 'C2');
      const roundNegatives: CaseResult[] = [];
      const roundCaptureAttempts: CaseResult[] = [];

      for (const row of positives) {
        if (row.id === 'C2') continue;
        const item = await runCase(row, `${row.id}${suffix}`);
        results.push(item);
        roundCaptureAttempts.push(item);
      }
      const c2 = selected.find((row) => row.id === 'C2');
      if (c2) {
        const item = await runCase(c2, `C2${suffix}`);
        results.push(item);
        roundCaptureAttempts.push(item);
      }

      for (const row of negatives) {
        const item = await runCase(row, `${row.id}${suffix}`);
        results.push(item);
        roundNegatives.push(item);
      }
      const batchSentinel = await runSentinel(`C2-sentinel-negatives${suffix}`);
      const hostDead = classify.hostFailedHandoff(roundCaptureAttempts.concat(batchSentinel));
      const batchOk = classify.isSentinelOk(batchSentinel) && !hostDead;
      if (!batchOk) {
        classify.invalidateNegatives(
          roundNegatives,
          hostDead
            ? 'missing or wrong accepted reply; negative keep is INVALID'
            : 'sentinel C2 did not capture; negative keep is INVALID',
        );
        for (const item of roundNegatives) persistResult(item);
      }
      const endSentinel = await runSentinel(`C2-sentinel-end${suffix}`);
      const endOk = classify.isSentinelOk(endSentinel);
      if (!endOk) {
        classify.invalidateNegatives(
          roundNegatives,
          'end sentinel C2 did not capture; negative keep is INVALID',
        );
        for (const item of roundNegatives) persistResult(item);
      }
    }

    if (classify.END_RECHECK) {
      await drainCaptureLines(runDir, 0, classify.END_DRAIN);
      const endHost = readHostLogEvidence(runDir);
      classify.recheckLateMissXfails(
        results,
        endHost.ok ? endHost.items : { ok: false, reason: endHost.reason, items: [] },
      );
      if (classify.END_DOWNLOAD_RECHECK) {
        const swEnd = context.serviceWorkers()[0] || worker;
        const endDownloads = plantEndCompleteDownloads(results, await searchDownloads(swEnd));
        writeFileSync(path.join(evidenceRoot, 'downloads-end.json'), `${JSON.stringify(endDownloads, null, 2)}\n`);
        classify.recheckCaptureDownloads(results, endDownloads);
      }
      for (const item of results) persistResult(item);
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
    const stressVariant = process.env.EXT_STRESS_VARIANT || 'default';
    const badRepeats = new Set<string>();
    for (const item of results) {
      const evidenceId = path.basename(item.evidencePath);
      console.log(
        `FINAL ${evidenceId} status=${item.status} obs=${item.observed} reason=${item.reason ?? ''}`,
      );
      if (item.status === 'FAIL' || item.status === 'XPASS' || item.status === 'INVALID') {
        const repeatMatch = evidenceId.match(/-r(\d+)/);
        badRepeats.add(repeatMatch ? repeatMatch[1] : '1');
      }
    }
    console.log(`STRESS variant=${stressVariant} repeats=${repeat} failed=${badRepeats.size}`);
    for (const skip of skipRows) {
      console.log(`SKIP ${skip.id} ${skip.skip}`);
    }
    writeFileSync(path.join(runDir, 'matrix-results.json'), `${JSON.stringify(results, null, 2)}\n`);

    const blockers = results.filter((row) => row.status === 'FAIL' || row.status === 'XPASS' || row.status === 'INVALID');
    const c2 = latestById.get('C2');
    const n1 = latestById.get('N1');
    if (onlyCases.length === 0 || onlyCases.includes('C2')) {
      expect(c2?.status, `C2 must pass, got ${JSON.stringify(c2)}`).toBe('pass');
    }
    if (onlyCases.length === 0 || onlyCases.includes('N1')) {
      expect(n1?.status, `N1 must pass, got ${JSON.stringify(n1)}`).toBe('pass');
    }
    expect(blockers, `blocking results: ${JSON.stringify(blockers, null, 2)}`).toHaveLength(0);
  } finally {
    await context.close();
  }
});
