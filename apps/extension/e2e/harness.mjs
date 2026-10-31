/**
 * Shared launch helpers for the Chromium extension e2e harness.
 * Native-host lookup that worked in Step 0: <user-data-dir>/NativeMessagingHosts/.
 */
import { chromium } from '@playwright/test';
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));

export const HOST_NAME = 'com.rusticdl.native_host';

export function extensionPaths(extensionRoot = path.resolve(here, '..')) {
  const repoRoot = path.resolve(extensionRoot, '../..');
  const identity = JSON.parse(
    readFileSync(path.join(extensionRoot, 'chromium-identity.json'), 'utf8'),
  );
  return {
    extensionRoot,
    repoRoot,
    distChromium: path.join(extensionRoot, 'dist/chromium'),
    identity,
    extensionOrigin: `chrome-extension://${identity.id}/`,
    hostShim: path.join(repoRoot, '.cursor/skills/verify-rusticdl/scripts/mock-native-host.sh'),
  };
}

export function writeNativeHostManifest({ userDataDir, hostPath, allowedOrigin }) {
  const destDir = path.join(userDataDir, 'NativeMessagingHosts');
  mkdirSync(destDir, { recursive: true });
  const dest = path.join(destDir, `${HOST_NAME}.json`);
  const manifest = {
    name: HOST_NAME,
    description: 'RusticDL Backend',
    path: hostPath,
    type: 'stdio',
    allowed_origins: [allowedOrigin],
  };
  writeFileSync(dest, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
  return dest;
}

export function isolateHome(runDir, realHome = process.env.HOME || os.homedir()) {
  const home = path.join(runDir, 'isolated-home');
  mkdirSync(path.join(home, '.config'), { recursive: true });
  mkdirSync(path.join(home, '.cache'), { recursive: true });
  mkdirSync(path.join(home, '.data'), { recursive: true });
  const realXauthority = process.env.XAUTHORITY || path.join(realHome, '.Xauthority');
  if (existsSync(realXauthority)) {
    copyFileSync(realXauthority, path.join(home, '.Xauthority'));
  }
  return { home, realXauthority };
}

export function browserEnv({ isolatedHome, runDir, nodeBin = process.execPath }) {
  const xauth = path.join(isolatedHome, '.Xauthority');
  return {
    ...process.env,
    HOME: isolatedHome,
    DISPLAY: process.env.DISPLAY || ':1',
    XAUTHORITY: existsSync(xauth) ? xauth : (process.env.XAUTHORITY || ''),
    XDG_CONFIG_HOME: path.join(isolatedHome, '.config'),
    XDG_CACHE_HOME: path.join(isolatedHome, '.cache'),
    XDG_DATA_HOME: path.join(isolatedHome, '.data'),
    RUN_DIR: runDir,
    VERIFY_RUN_DIR: runDir,
    NODE_BIN: nodeBin,
    EXT_ATTACK: process.env.EXT_ATTACK || '',
    EXT_ATTACK_DELAY_MS: process.env.EXT_ATTACK_DELAY_MS || '',
    PATH: `${path.dirname(nodeBin)}:${process.env.PATH || '/usr/bin:/bin'}`,
  };
}

export async function launchExtensionContext({
  userDataDir,
  isolatedHome,
  runDir,
  distChromium,
}) {
  mkdirSync(userDataDir, { recursive: true });
  mkdirSync(path.join(runDir, 'browser-downloads'), { recursive: true });
  const context = await chromium.launchPersistentContext(userDataDir, {
    headless: false,
    acceptDownloads: true,
    downloadsPath: path.join(runDir, 'browser-downloads'),
    env: browserEnv({ isolatedHome, runDir }),
    ignoreDefaultArgs: ['--disable-extensions'],
    args: [
      `--disable-extensions-except=${distChromium}`,
      `--load-extension=${distChromium}`,
      '--no-sandbox',
      '--disable-dev-shm-usage',
    ],
  });
  return {
    context,
    browserVersion: context.browser()?.version() ?? 'unknown',
    executablePath: chromium.executablePath(),
  };
}

export async function waitForServiceWorker(context, timeoutMs = 30_000) {
  const existing = context.serviceWorkers()[0];
  if (existing) return existing;
  return context.waitForEvent('serviceworker', { timeout: timeoutMs });
}

export async function pingNativeHost(worker, requestId) {
  return worker.evaluate(async ({ host, id }) => {
    try {
      return await chrome.runtime.sendNativeMessage(host, {
        protocolVersion: 1,
        requestId: id,
        type: 'ping',
        payload: {},
      });
    } catch (error) {
      return {
        ok: false,
        requestId: 'native_messaging_error',
        type: 'rejected',
        message: error instanceof Error ? error.message : String(error),
      };
    }
  }, { host: HOST_NAME, id: requestId });
}

export async function searchDownloads(worker) {
  return worker.evaluate(async () => {
    try {
      return await chrome.downloads.search({});
    } catch (error) {
      return { error: error instanceof Error ? error.message : String(error) };
    }
  });
}

export function readHostLog(runDir) {
  const logPath = path.join(runDir, 'host-log.jsonl');
  if (!existsSync(logPath)) return [];
  return readFileSync(logPath, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

export function captureLinesSince(runDir, startMs) {
  return readHostLog(runDir).filter((line) => (
    line.receivedAt >= startMs
    && (line.type === 'prompt_download' || line.type === 'enqueue_download')
  ));
}

export function readFixtureHits(runDir) {
  const logPath = path.join(runDir, 'fixture-requests.jsonl');
  if (!existsSync(logPath)) return [];
  return readFileSync(logPath, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

export function readFixtureServes(runDir) {
  const logPath = path.join(runDir, 'fixture-serves.jsonl');
  if (!existsSync(logPath)) return [];
  return readFileSync(logPath, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export async function drainCaptureLines(runDir, startedAt, {
  minWaitMs = 0,
  quietMs = 0,
  maxWaitMs = 0,
  waitOutEmpty = false,
} = {}) {
  const t0 = Date.now();
  if (minWaitMs > 0) await sleep(minWaitMs);
  if (maxWaitMs <= 0) return captureLinesSince(runDir, startedAt);
  const sigOf = (lines) => `${lines.length}:${lines.at(-1)?.receivedAt ?? 0}`;
  let lastSig = sigOf(captureLinesSince(runDir, startedAt));
  let quietFrom = Date.now();
  while (Date.now() - t0 < maxWaitMs) {
    await sleep(50);
    const lines = captureLinesSince(runDir, startedAt);
    const sig = sigOf(lines);
    if (sig !== lastSig) {
      lastSig = sig;
      quietFrom = Date.now();
      continue;
    }
    if (lines.length === 0 && waitOutEmpty) {
      continue;
    }
    if (quietMs <= 0 || Date.now() - quietFrom >= quietMs) {
      break;
    }
  }
  return captureLinesSince(runDir, startedAt);
}

export function percentile(values, p) {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const idx = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[idx];
}
