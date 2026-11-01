#!/usr/bin/env node
/**
 * Browser evidence for two open suspicions.
 *
 * (a) User pause during the name wait. A weak UUID download stays in
 *     `naming` until the 1.5s timer (the determined name is still weak).
 *     The body is held open so the shelf item stays `in_progress`. This
 *     pauses that item from the service worker and prints the shelf plus
 *     prompt count.
 * (b) K28. With EXT_HOST_REPLY_DELAY_MS, after prompt_download is logged,
 *     close the service-worker target (session storage survives; runtime.reload
 *     would wipe it) and wake the worker from options.html. Then count
 *     prompt_download and print the shelf.
 *
 * Usage:
 *   EXT_HOST_REPLY_DELAY_MS=3000 node e2e/raw-chrome-suspects.mjs
 */
import { createServer } from 'node:http';
import { chmodSync, mkdtempSync, readdirSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  extensionPaths,
  isolateHome,
  launchExtensionContext,
  readHostLog,
  searchDownloads,
  waitForServiceWorker,
  writeNativeHostManifest,
} from './harness.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const paths = extensionPaths(path.resolve(here, '..'));
chmodSync(paths.hostShim, 0o755);

const zip = Buffer.alloc(65536, 0x41);
const uuid = 'aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee';

function shelfSummary(downloads) {
  if (!Array.isArray(downloads)) return downloads;
  return downloads.map((item) => ({
    id: item.id,
    state: item.state,
    paused: item.paused,
    filename: item.filename,
    url: item.url,
    bytesReceived: item.bytesReceived,
    totalBytes: item.totalBytes,
    error: item.error || '',
    exists: item.exists,
  }));
}

function promptsFor(runDir, url) {
  return readHostLog(runDir).filter((line) => (
    line.type === 'prompt_download' && (!url || line.payload?.url === url)
  ));
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

const server = createServer((req, res) => {
  const url = new URL(req.url || '/', 'http://127.0.0.1');
  if (url.pathname === `/uuid-wait/${uuid}`) {
    res.writeHead(200, {
      'content-type': 'application/zip',
      'content-disposition': `attachment; filename="${uuid}"`,
      'content-length': String(zip.length),
    });
    res.write(zip.subarray(0, 4096));
    setTimeout(() => {
      if (!res.writableEnded) res.end(zip.subarray(4096));
    }, 4500);
    return;
  }
  if (url.pathname === '/known') {
    res.writeHead(200, {
      'content-type': 'application/octet-stream',
      'content-disposition': 'attachment; filename="app.zip"',
      'content-length': String(zip.length),
    });
    res.end(zip);
    return;
  }
  if (url.pathname === '/page') {
    res.writeHead(200, { 'content-type': 'text/html' });
    res.end(`<!doctype html>
<a id="name" href="/uuid-wait/${uuid}">name</a>
<a id="known" download="app.zip" href="/known">known</a>`);
    return;
  }
  res.writeHead(404);
  res.end();
});

await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const port = server.address().port;
const origin = `http://127.0.0.1:${port}`;

const runDir = mkdtempSync(path.join(os.tmpdir(), 'raw-chrome-suspects-'));
const userDataDir = path.join(runDir, 'profile');
const isolated = isolateHome(runDir);
writeNativeHostManifest({
  userDataDir,
  hostPath: paths.hostShim,
  allowedOrigin: paths.extensionOrigin,
});

process.env.EXT_HOST_REPLY_DELAY_MS = process.env.EXT_HOST_REPLY_DELAY_MS || '3000';
const launched = await launchExtensionContext({
  userDataDir,
  isolatedHome: isolated.home,
  runDir,
  distChromium: paths.distChromium,
});
const context = launched.context;
const extensionId = paths.identity.id;

let exitCode = 0;
try {
  const worker = await waitForServiceWorker(context);
  const page = context.pages()[0] || await context.newPage();
  await page.goto(`${origin}/page`, { waitUntil: 'domcontentloaded' });

  const nameUrl = `${origin}/uuid-wait/${uuid}`;
  const clickedAt = Date.now();
  await page.locator('#name').click();
  let live = null;
  let saw = null;
  for (let i = 0; i < 40; i += 1) {
    const items = await worker.evaluate(async () => {
      const rows = await chrome.downloads.search({});
      return rows.map((item) => ({
        id: item.id,
        state: item.state,
        paused: item.paused,
        filename: item.filename,
        url: item.url,
      }));
    });
    const row = items.find((item) => item.url === nameUrl);
    if (row) {
      saw = row;
      if (row.state === 'in_progress' && row.paused !== true) {
        live = row;
        break;
      }
      if (row.state !== 'in_progress' || row.paused === true) break;
    }
    await sleep(25);
  }
  const paused = await worker.evaluate(async (id) => {
    if (id == null) return { paused: false, reason: 'no in_progress unpaused item' };
    await chrome.downloads.pause(id);
    const [after] = await chrome.downloads.search({ id });
    return {
      paused: true,
      id,
      state: after?.state ?? '',
      pausedNow: after?.paused === true,
      filename: after?.filename ?? '',
    };
  }, live ? live.id : null);
  const pauseMs = Date.now() - clickedAt;
  await sleep(Math.max(0, 2200 - (Date.now() - clickedAt)));
  const nameShelf = shelfSummary(await searchDownloads(worker));
  const namePrompts = promptsFor(runDir, nameUrl);
  console.log(`USER_PAUSE saw=${JSON.stringify(saw)} pauseMs=${pauseMs} snapshotMs=${Date.now() - clickedAt}`);
  console.log(`USER_PAUSE during name wait action=${JSON.stringify(paused)}`);
  console.log(`USER_PAUSE prompts=${namePrompts.length} shelf=${JSON.stringify(nameShelf)}`);
  const nameRow = Array.isArray(nameShelf)
    ? nameShelf.find((item) => item.url === nameUrl)
    : null;
  if (!live || pauseMs >= 1500 || namePrompts.length !== 0 || !nameRow || nameRow.paused !== true) exitCode = 1;

  const knownUrl = `${origin}/known`;
  const beforeKnown = Date.now();
  await page.locator('#known').click();
  let seenPrompt = false;
  for (let i = 0; i < 40; i += 1) {
    if (promptsFor(runDir, knownUrl).some((line) => line.receivedAt >= beforeKnown)) {
      seenPrompt = true;
      break;
    }
    await sleep(50);
  }
  console.log(`K28 prompt logged before stop=${seenPrompt}`);
  await sleep(200);
  const generation = await worker.evaluate(() => {
    globalThis.__rusticdlSuspectGeneration = 1;
    return 1;
  }).catch((error) => `err:${error instanceof Error ? error.message : String(error)}`);
  console.log(`K28 generation before stop=${generation}`);
  const browser = context.browser();
  const cdp = browser ? await browser.newBrowserCDPSession() : null;
  let stopMethod = 'none';
  if (cdp) {
    const { targetInfos } = await cdp.send('Target.getTargets');
    const workers = (targetInfos || []).filter((target) => (
      target.type === 'service_worker' && String(target.url || '').includes(extensionId)
    ));
    console.log(`K28 targets before=${JSON.stringify(workers.map((target) => ({
      id: target.targetId,
      url: target.url,
    })))}`);
    const closed = [];
    for (const target of workers) {
      const result = await cdp.send('Target.closeTarget', { targetId: target.targetId });
      closed.push({ id: target.targetId, success: result?.success === true });
    }
    console.log(`K28 closeTarget=${JSON.stringify(closed)}`);
    if (closed.some((row) => row.success)) stopMethod = 'closeTarget';
  }
  if (stopMethod === 'none') {
    stopMethod = 'runtime.reload';
    await worker.evaluate(() => {
      chrome.runtime.reload();
      return true;
    }).catch((error) => {
      console.log(`K28 reload evaluate=${error instanceof Error ? error.message : String(error)}`);
    });
  }
  const wakePage = await context.newPage();
  const swWait = context.waitForEvent('serviceworker', { timeout: 3000 }).catch((error) => {
    console.log(`K28 serviceworker event=${error instanceof Error ? error.message : String(error)}`);
    return null;
  });
  await wakePage.goto(`chrome-extension://${extensionId}/options.html`, {
    waitUntil: 'domcontentloaded',
    timeout: 15000,
  }).catch((error) => {
    console.log(`K28 options goto=${error instanceof Error ? error.message : String(error)}`);
  });
  let nextWorker = context.serviceWorkers().find((candidate) => candidate !== worker) || null;
  const eventWorker = await swWait;
  if (!nextWorker && eventWorker) nextWorker = eventWorker;
  for (let i = 0; i < 40 && !nextWorker; i += 1) {
    nextWorker = context.serviceWorkers()[0] || null;
    if (nextWorker) break;
    await sleep(100);
  }
  let generationAfter = 'no-worker';
  if (nextWorker) {
    generationAfter = await nextWorker.evaluate(() => globalThis.__rusticdlSuspectGeneration || 0).catch((error) => (
      `err:${error instanceof Error ? error.message : String(error)}`
    ));
  }
  console.log(`K28 stopMethod=${stopMethod} workerBack=${Boolean(nextWorker)} sameObject=${nextWorker === worker} generationAfter=${generationAfter}`);
  const delayMs = Number(process.env.EXT_HOST_REPLY_DELAY_MS || '3000');
  await sleep(delayMs + 2000);
  const knownPrompts = promptsFor(runDir, knownUrl);
  const knownShelf = nextWorker
    ? shelfSummary(await searchDownloads(nextWorker))
    : { error: 'no service worker' };
  const downloadsDir = path.join(isolated.home, 'Downloads');
  let files = [];
  try {
    files = readdirSync(downloadsDir);
  } catch {
    files = [];
  }
  console.log(`K28 prompts=${knownPrompts.length} shelf=${JSON.stringify(knownShelf)}`);
  console.log(`K28 files=${JSON.stringify(files)}`);
  const knownRow = Array.isArray(knownShelf)
    ? knownShelf.find((item) => item.url === knownUrl)
    : null;
  const orphanPause = knownRow?.paused === true && knownRow?.state === 'in_progress';
  const lost = knownPrompts.length === 0
    || (knownRow?.state === 'interrupted' && knownRow?.error !== 'USER_CANCELED')
    || (!knownRow && files.length === 0);
  console.log(`K28 orphanPause=${orphanPause} lost=${lost}`);
  if (knownPrompts.length !== 1 || orphanPause || lost || !nextWorker) exitCode = 1;
} finally {
  await context.close().catch(() => {});
  server.close();
}

process.exit(exitCode);
