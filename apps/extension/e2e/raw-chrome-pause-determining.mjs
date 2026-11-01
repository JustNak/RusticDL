#!/usr/bin/env node
/**
 * Raw Chrome for Testing measurement: does downloads.onDeterminingFilename
 * still fire after downloads.pause() in onCreated?
 *
 * Not a product test. Prints one line per downloads event so the pause/name
 * order can be chosen from the output, not from the comment in the coordinator.
 *
 * Usage: node e2e/raw-chrome-pause-determining.mjs
 */
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const extensionRoot = path.resolve(here, '..');
const require = createRequire(path.join(extensionRoot, 'package.json'));
const { chromium } = require('@playwright/test');
const chromePath = process.env.CHROME_FOR_TESTING || chromium.executablePath();

const tmp = mkdtempSync(path.join(os.tmpdir(), 'raw-chrome-pause-det-'));
const extDir = path.join(tmp, 'ext');
const userDataDir = path.join(tmp, 'profile');
mkdirSync(extDir, { recursive: true });
mkdirSync(userDataDir, { recursive: true });

const arms = (process.env.RAW_ARM || 'open,pause,slow-pause').split(',').map((arm) => arm.trim()).filter(Boolean);
console.log(`PAUSE_DET chrome=${chromePath}`);

for (const arm of arms) {
  const events = [];
  const server = createServer((req, res) => {
    const url = new URL(req.url || '/', 'http://127.0.0.1');
    if (req.method === 'POST' && url.pathname === '/event') {
      const chunks = [];
      req.on('data', (chunk) => chunks.push(chunk));
      req.on('end', () => {
        const raw = Buffer.concat(chunks).toString('utf8');
        try {
          const body = JSON.parse(raw);
          body.serverAt = Date.now();
          events.push(body);
        } catch {
          events.push({ kind: 'bad-json', raw, serverAt: Date.now() });
        }
        res.writeHead(204);
        res.end();
      });
      return;
    }
    if (url.pathname === '/page') {
      const slow = arm === 'slow-pause' ? '&slow=1' : '';
      const pause = arm === 'open' ? 'open' : 'pause';
      res.writeHead(200, { 'content-type': 'text/html' });
      res.end(`<!doctype html>
<a id="dl" download="app.zip" href="/file?arm=${pause}${slow}">download</a>
<script>setTimeout(() => document.getElementById('dl').click(), 400);</script>`);
      return;
    }
    if (url.pathname === '/file') {
      const slow = url.searchParams.get('slow') === '1';
      const body = Buffer.alloc(slow ? 256 * 1024 : 64 * 1024, 0x41);
      res.writeHead(200, {
        'content-type': 'application/octet-stream',
        'content-length': String(body.length),
      });
      if (!slow) {
        res.end(body);
        return;
      }
      res.write(body.subarray(0, 32 * 1024));
      setTimeout(() => res.end(body.subarray(32 * 1024)), 4000);
      return;
    }
    res.writeHead(404);
    res.end();
  });

  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  const pageUrl = `http://127.0.0.1:${port}/page`;
  const armDir = path.join(tmp, arm);
  const armExt = path.join(armDir, 'ext');
  const armProfile = path.join(armDir, 'profile');
  mkdirSync(armExt, { recursive: true });
  mkdirSync(armProfile, { recursive: true });
  writeFileSync(path.join(armExt, 'manifest.json'), `${JSON.stringify({
    manifest_version: 3,
    name: `pause-determining-probe-${arm}`,
    version: '1.0.0',
    permissions: ['downloads'],
    host_permissions: [`http://127.0.0.1:${port}/*`],
    background: { service_worker: 'bg.js' },
  }, null, 2)}\n`);
  writeFileSync(path.join(armExt, 'bg.js'), `const port = ${port};
function log(entry) {
  fetch('http://127.0.0.1:' + port + '/event', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ...entry, t: Date.now() }),
  }).catch(() => {});
}
log({ kind: 'worker-start' });
chrome.downloads.onDeterminingFilename.addListener((item, suggest) => {
  log({
    kind: 'determining',
    id: item.id,
    url: item.url,
    filename: item.filename,
    state: item.state,
    paused: item.paused,
  });
  try { suggest(); } catch (error) { log({ kind: 'suggest-error', message: String(error) }); }
});
chrome.downloads.onCreated.addListener((item) => {
  const pause = item.url.includes('arm=pause');
  log({
    kind: 'created',
    id: item.id,
    url: item.url,
    filename: item.filename,
    state: item.state,
    paused: item.paused,
    willPause: pause,
  });
  if (!pause) return;
  try {
    chrome.downloads.pause(item.id, () => {
      const message = chrome.runtime.lastError && chrome.runtime.lastError.message;
      log({ kind: 'pause-callback', id: item.id, url: item.url, error: message || '' });
    });
  } catch (error) {
    log({ kind: 'pause-throw', id: item.id, message: String(error) });
  }
});
chrome.downloads.onChanged.addListener((delta) => {
  log({
    kind: 'changed',
    id: delta.id,
    state: delta.state || null,
    paused: delta.paused || null,
    filename: delta.filename || null,
    error: delta.error || null,
  });
});
if (${arm === 'pause-poll' ? 'true' : 'false'}) {
  const poll = () => {
    chrome.downloads.search({}, (items) => {
      log({
        kind: 'poll',
        items: (items || []).map((item) => ({
          id: item.id,
          state: item.state,
          paused: item.paused,
          bytesReceived: item.bytesReceived,
          totalBytes: item.totalBytes,
          error: item.error || '',
          filename: item.filename,
        })),
      });
    });
  };
  setInterval(poll, 200);
  poll();
}
`);

  const extraFlags = (process.env.RAW_EXTRA_FLAGS || '')
    .split(/\s+/)
    .map((flag) => flag.trim())
    .filter(Boolean);
  const playwrightFlags = process.env.RAW_PLAYWRIGHT_FLAGS === '1' ? [
    '--disable-field-trial-config',
    '--disable-background-networking',
    '--disable-background-timer-throttling',
    '--disable-client-side-phishing-detection',
    '--disable-component-update',
    '--disable-default-apps',
    '--disable-features=OptimizationHints,NetworkTimeServiceQuerying',
    '--disable-hang-monitor',
    '--disable-popup-blocking',
    '--disable-prompt-on-repost',
    '--disable-sync',
    '--metrics-recording-only',
    '--no-service-autorun',
  ] : [];
  const proc = spawn(chromePath, [
    `--user-data-dir=${armProfile}`,
    `--disable-extensions-except=${armExt}`,
    `--load-extension=${armExt}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--no-sandbox',
    '--disable-dev-shm-usage',
    ...playwrightFlags,
    ...extraFlags,
    pageUrl,
  ], { stdio: ['ignore', 'ignore', 'pipe'] });
  let chromeErr = '';
  proc.stderr.on('data', (chunk) => {
    chromeErr += chunk.toString('utf8');
  });
  const waitMs = Number(process.env.RAW_CHROME_WAIT_MS || (arm === 'slow-pause' ? 7000 : 4000));
  await new Promise((resolve) => setTimeout(resolve, waitMs));
  proc.kill('SIGKILL');
  await new Promise((resolve) => server.close(resolve));

  console.log(`PAUSE_DET arm=${arm} page=${pageUrl} events=${events.length}`);
  let firstCreated = null;
  let firstDetermining = null;
  let firstComplete = null;
  let pauseCallback = null;
  for (const event of events) {
    console.log(`PAUSE_DET arm=${arm} ${JSON.stringify(event)}`);
    if (event.kind === 'created' && firstCreated == null) firstCreated = event.t;
    if (event.kind === 'determining' && firstDetermining == null) firstDetermining = event.t;
    if (event.kind === 'pause-callback' && pauseCallback == null) pauseCallback = event;
    if (event.kind === 'changed' && event.state && event.state.current === 'complete' && firstComplete == null) {
      firstComplete = event.t;
    }
  }
  const determining = events.some((event) => event.kind === 'determining');
  const filename = events.find((event) => event.kind === 'determining')?.filename ?? '';
  console.log(
    `PAUSE_DET summary arm=${arm} determining=${determining} filename=${filename || '(none)'}`
    + ` createdToDeterminingMs=${firstCreated != null && firstDetermining != null ? firstDetermining - firstCreated : '(none)'}`
    + ` createdToCompleteMs=${firstCreated != null && firstComplete != null ? firstComplete - firstCreated : '(none)'}`
    + ` pauseError=${pauseCallback ? (pauseCallback.error || 'ok') : '(no callback)'}`,
  );
  if (!events.some((event) => event.kind === 'created')) {
    console.error(`PAUSE_DET FAIL arm=${arm} no onCreated`);
    if (chromeErr) console.error(chromeErr.slice(0, 1500));
    process.exitCode = 1;
  }
}
