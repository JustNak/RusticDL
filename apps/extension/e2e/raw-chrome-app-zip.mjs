#!/usr/bin/env node
/**
 * Raw Chrome for Testing proof that the extension sends Chrome's determined
 * name (`app.zip`) from downloads.onDeterminingFilename — no Playwright
 * download interception, no 1.5s timer fallback.
 *
 * Usage: node e2e/raw-chrome-app-zip.mjs
 */
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  writeFileSync,
} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const extensionRoot = path.resolve(here, '..');
const distChromium = process.env.DIST_CHROMIUM
  || path.join(extensionRoot, 'dist/chromium');
const identity = JSON.parse(
  readFileSync(path.join(extensionRoot, 'chromium-identity.json'), 'utf8'),
);
const hostShim = path.resolve(
  extensionRoot,
  '../../.cursor/skills/verify-rusticdl/scripts/mock-native-host.sh',
);
chmodSync(hostShim, 0o755);

const require = createRequire(path.join(extensionRoot, 'package.json'));
const { chromium } = require('@playwright/test');
const chromePath = process.env.CHROME_FOR_TESTING || chromium.executablePath();

if (!existsSync(path.join(distChromium, 'manifest.json'))) {
  console.error('raw-chrome-app-zip: missing dist/chromium (run npm run build)');
  process.exit(2);
}
if (!existsSync(chromePath)) {
  console.error(`raw-chrome-app-zip: chrome not found at ${chromePath}`);
  process.exit(2);
}

const tmp = mkdtempSync(path.join(os.tmpdir(), 'raw-chrome-app-zip-'));
const runDir = tmp;
const userDataDir = path.join(tmp, 'profile');
const isolatedHome = path.join(tmp, 'home');
mkdirSync(path.join(userDataDir, 'NativeMessagingHosts'), { recursive: true });
mkdirSync(path.join(isolatedHome, '.config'), { recursive: true });

writeFileSync(
  path.join(userDataDir, 'NativeMessagingHosts/com.rusticdl.native_host.json'),
  `${JSON.stringify({
    name: 'com.rusticdl.native_host',
    description: 'RusticDL Backend',
    path: hostShim,
    type: 'stdio',
    allowed_origins: [`chrome-extension://${identity.id}/`],
  }, null, 2)}\n`,
);

const zip = Buffer.alloc(65536, 0x41);
const nonce = 'aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee';
const attempt = `${Date.now()}-rawzip`;
const uuid = 'aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee';
const uuidAttempt = `${Date.now()}-rawuuid`;
let clickedAt = 0;
const events = [];

const server = createServer((req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  if (req.method === 'POST' && url.pathname === '/clicked') {
    clickedAt = Date.now();
    events.push({ kind: 'clicked', t: clickedAt });
    res.writeHead(204);
    res.end();
    return;
  }
  if (url.pathname === '/page') {
    const href = `/dl?nonce=${nonce}&attempt=${attempt}`;
    const uuidHref = `/${uuid}?nonce=${nonce}&attempt=${uuidAttempt}`;
    res.writeHead(200, { 'content-type': 'text/html' });
    res.end(`<!doctype html>
<a id="dl" download="app.zip" href="${href}">download</a>
<a id="uuid" href="${uuidHref}">uuid</a>
<script>
setTimeout(() => {
  fetch('/clicked', { method: 'POST' }).catch(() => {});
  document.getElementById('dl').click();
}, 1500);
setTimeout(() => document.getElementById('uuid').click(), 3500);
</script>`);
    return;
  }
  if (url.pathname === '/dl' || url.pathname === `/${uuid}`) {
    res.writeHead(200, {
      'content-type': 'application/octet-stream',
      'content-length': String(zip.length),
    });
    res.end(zip);
    return;
  }
  res.writeHead(404);
  res.end();
});

await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const port = server.address().port;
const pageUrl = `http://127.0.0.1:${port}/page`;
const dlUrl = `http://127.0.0.1:${port}/dl?nonce=${nonce}&attempt=${attempt}`;
const uuidUrl = `http://127.0.0.1:${port}/${uuid}?nonce=${nonce}&attempt=${uuidAttempt}`;

const env = {
  ...process.env,
  HOME: isolatedHome,
  DISPLAY: process.env.DISPLAY || ':1',
  RUN_DIR: runDir,
  VERIFY_RUN_DIR: runDir,
  NODE_BIN: process.execPath,
  PATH: `${path.dirname(process.execPath)}:${process.env.PATH || '/usr/bin:/bin'}`,
};

const proc = spawn(
  chromePath,
  [
    `--user-data-dir=${userDataDir}`,
    `--disable-extensions-except=${distChromium}`,
    `--load-extension=${distChromium}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--no-sandbox',
    '--disable-dev-shm-usage',
    pageUrl,
  ],
  { stdio: ['ignore', 'ignore', 'pipe'], env },
);

const waitMs = Number(process.env.RAW_CHROME_WAIT_MS || 8000);
await new Promise((resolve) => setTimeout(resolve, waitMs));

const logPath = path.join(runDir, 'host-log.jsonl');
const log = existsSync(logPath) ? readFileSync(logPath, 'utf8') : '';
const lines = log
  .split('\n')
  .filter(Boolean)
  .map((line) => {
    try {
      return JSON.parse(line);
    } catch {
      return null;
    }
  })
  .filter(Boolean);

const prompts = lines.filter((row) => row.type === 'prompt_download');
const appZip = prompts.find(
  (row) => row.payload?.url === dlUrl && row.payload?.suggestedFilename === 'app.zip',
);
const uuidPrompt = prompts.find((row) => row.payload?.url === uuidUrl);
const uuidAsZip = prompts.find(
  (row) =>
    typeof row.payload?.suggestedFilename === 'string'
    && /aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee\.zip/i.test(row.payload.suggestedFilename),
);

const latencyMs = appZip && clickedAt ? appZip.receivedAt - clickedAt : null;
const timerFallback = latencyMs != null && latencyMs >= 1400;

console.log(`RAW_CHROME chrome=${chromePath}`);
console.log(`RAW_CHROME url=${dlUrl}`);
console.log(`RAW_CHROME suggestedFilename=${appZip?.payload?.suggestedFilename ?? '(none)'}`);
console.log(`RAW_CHROME latencyMs=${latencyMs ?? '(none)'} clickedAt=${clickedAt}`);
console.log(`RAW_CHROME uuidPrompt=${uuidPrompt ? uuidPrompt.payload?.suggestedFilename : 'none'}`);

let failed = 0;
if (!appZip) {
  console.error('FAIL: no prompt_download suggestedFilename=app.zip for the attempt URL');
  failed += 1;
}
if (timerFallback) {
  console.error(`FAIL: latencyMs=${latencyMs} looks like the 1.5s timer, not onDeterminingFilename`);
  failed += 1;
}
if (uuidAsZip) {
  console.error('FAIL: a GUID URL was handed off as uuid.zip');
  failed += 1;
}

proc.kill('SIGKILL');
server.close();

if (failed > 0) {
  console.error('HOST_LOG');
  console.error(log || '(empty)');
  process.exit(1);
}

console.log('RAW_CHROME_APP_ZIP OK');
process.exit(0);
