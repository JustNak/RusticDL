#!/usr/bin/env node
/**
 * Step 0: prove Playwright's bundled Chromium can load dist/chromium and ping
 * com.rusticdl.native_host through a mock native-messaging host.
 *
 * Tries <user-data-dir>/NativeMessagingHosts/ first, then the per-user config
 * dir for this browser build under an isolated HOME.
 */
import { chromium } from '@playwright/test';
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  writeFileSync,
  existsSync,
} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const extensionRoot = path.resolve(here, '..');
const repoRoot = path.resolve(extensionRoot, '../..');
const distChromium = path.join(extensionRoot, 'dist/chromium');
const identity = JSON.parse(
  readFileSync(path.join(extensionRoot, 'chromium-identity.json'), 'utf8'),
);
const hostName = 'com.rusticdl.native_host';
const extensionOrigin = `chrome-extension://${identity.id}/`;
const kitScripts = path.join(repoRoot, '.cursor/skills/verify-rusticdl/scripts');
const hostShim = path.join(kitScripts, 'mock-native-host.sh');
const nodeBin = process.execPath;
const realHome = process.env.HOME || os.homedir();
const display = process.env.DISPLAY || ':1';
const realXauthority = process.env.XAUTHORITY || path.join(realHome, '.Xauthority');

const RUN_ID = process.env.RUN_ID || `step0-${Date.now()}`;
const RUN_DIR = process.env.VERIFY_RUN_DIR || `/tmp/rusticdl-verify-${RUN_ID}`;
mkdirSync(RUN_DIR, { recursive: true });

function writeManifest(destDir, hostPath) {
  mkdirSync(destDir, { recursive: true });
  const dest = path.join(destDir, `${hostName}.json`);
  const manifest = {
    name: hostName,
    description: 'RusticDL Backend',
    path: hostPath,
    type: 'stdio',
    allowed_origins: [extensionOrigin],
  };
  writeFileSync(dest, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
  return dest;
}

function readHostLog() {
  const logPath = path.join(RUN_DIR, 'host-log.jsonl');
  if (!existsSync(logPath)) return [];
  return readFileSync(logPath, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

function makeIsolatedHome() {
  const home = mkdtempSync(path.join(RUN_DIR, 'home-'));
  mkdirSync(path.join(home, '.config'), { recursive: true });
  if (existsSync(realXauthority)) {
    copyFileSync(realXauthority, path.join(home, '.Xauthority'));
  }
  return home;
}

function browserEnv(isolatedHome) {
  return {
    ...process.env,
    HOME: isolatedHome,
    DISPLAY: display,
    XAUTHORITY: existsSync(path.join(isolatedHome, '.Xauthority'))
      ? path.join(isolatedHome, '.Xauthority')
      : realXauthority,
    XDG_CONFIG_HOME: path.join(isolatedHome, '.config'),
    XDG_CACHE_HOME: path.join(isolatedHome, '.cache'),
    XDG_DATA_HOME: path.join(isolatedHome, '.data'),
    RUN_DIR,
    VERIFY_RUN_DIR: RUN_DIR,
    NODE_BIN: nodeBin,
    PATH: `${path.dirname(nodeBin)}:${process.env.PATH || '/usr/bin:/bin'}`,
  };
}

async function launchWithExistingUserData(label, options) {
  mkdirSync(options.userDataDir, { recursive: true });
  const result = {
    label,
    manifestPath: options.manifestPath,
    userDataDir: options.userDataDir,
    ok: false,
    error: null,
    browserVersion: null,
    executablePath: chromium.executablePath(),
    extensionId: null,
    pong: null,
    pingLogLine: null,
  };

  let context;
  try {
    context = await chromium.launchPersistentContext(options.userDataDir, {
      headless: false,
      env: browserEnv(options.isolatedHome),
      ignoreDefaultArgs: ['--disable-extensions'],
      args: [
        `--disable-extensions-except=${distChromium}`,
        `--load-extension=${distChromium}`,
        '--no-sandbox',
        '--disable-dev-shm-usage',
      ],
    });
    result.browserVersion = context.browser()?.version() ?? 'unknown';

    let worker = context.serviceWorkers()[0];
    if (!worker) {
      worker = await context.waitForEvent('serviceworker', { timeout: 30_000 });
    }
    result.extensionId = new URL(worker.url()).host;

    const pingId = `step0-${label.replace(/[^a-zA-Z0-9._-]/g, '_')}-${Date.now()}`;
    const before = Date.now();
    result.pong = await worker.evaluate(async ({ host, requestId }) => {
      try {
        return await chrome.runtime.sendNativeMessage(host, {
          protocolVersion: 1,
          requestId,
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
    }, { host: hostName, requestId: pingId });

    const log = readHostLog();
    result.pingLogLine = log.find((line) => line.requestId === pingId)
      ?? log.filter((line) => line.type === 'ping' && line.receivedAt >= before).at(-1)
      ?? null;

    if (result.pong && result.pong.ok === true && result.pong.type === 'pong') {
      result.ok = true;
    } else {
      result.error = result.pong?.message || JSON.stringify(result.pong);
    }
  } catch (error) {
    result.error = error instanceof Error ? error.message : String(error);
    result.stack = error instanceof Error ? error.stack : undefined;
  } finally {
    if (context) {
      await context.close().catch(() => undefined);
    }
  }
  return result;
}

async function readChromeVersion(isolatedHome) {
  const userDataDir = path.join(isolatedHome, 'version-profile');
  mkdirSync(userDataDir, { recursive: true });
  let context;
  try {
    context = await chromium.launchPersistentContext(userDataDir, {
      headless: false,
      env: browserEnv(isolatedHome),
      args: ['--no-sandbox', '--disable-dev-shm-usage'],
    });
    const page = await context.newPage();
    await page.goto('chrome://version/', { waitUntil: 'domcontentloaded', timeout: 15_000 });
    const text = await page.locator('body').innerText();
    return {
      browserVersion: context.browser()?.version() ?? null,
      executablePath: chromium.executablePath(),
      pageText: text.slice(0, 4000),
    };
  } catch (error) {
    return { error: error instanceof Error ? error.message : String(error) };
  } finally {
    if (context) await context.close().catch(() => undefined);
  }
}

function configCandidates(home) {
  return [
    path.join(home, '.config/chromium/NativeMessagingHosts'),
    path.join(home, '.config/google-chrome/NativeMessagingHosts'),
    path.join(home, '.config/google-chrome-for-testing/NativeMessagingHosts'),
    path.join(home, '.config/chromium-for-testing/NativeMessagingHosts'),
  ];
}

function pickConfigDir(versionInfo, home) {
  const text = `${versionInfo?.pageText || ''} ${versionInfo?.browserVersion || ''} ${chromium.executablePath()}`.toLowerCase();
  const candidates = configCandidates(home);
  if (text.includes('google-chrome-for-testing') || text.includes('chrome for testing')) {
    return candidates[2];
  }
  if (text.includes('chromium')) {
    return candidates[0];
  }
  return candidates[0];
}

async function main() {
  const report = {
    runDir: RUN_DIR,
    distChromium,
    hostShim,
    nodeBin,
    display,
    realXauthority,
    executablePath: chromium.executablePath(),
    attempts: [],
  };

  if (!existsSync(path.join(distChromium, 'manifest.json'))) {
    console.error(`BLOCKED: missing ${distChromium}/manifest.json — build the extension first`);
    process.exit(2);
  }
  if (!existsSync(hostShim)) {
    console.error(`BLOCKED: missing host shim ${hostShim}`);
    process.exit(2);
  }
  chmodSync(hostShim, 0o755);
  const hostPath = path.resolve(hostShim);

  const attempt1Home = makeIsolatedHome();
  const attempt1UserData = path.join(attempt1Home, 'chromium-profile');
  mkdirSync(attempt1UserData, { recursive: true });
  const attempt1Manifest = writeManifest(
    path.join(attempt1UserData, 'NativeMessagingHosts'),
    hostPath,
  );
  const attempt1 = await launchWithExistingUserData('user-data-dir/NativeMessagingHosts', {
    isolatedHome: attempt1Home,
    userDataDir: attempt1UserData,
    manifestPath: attempt1Manifest,
  });
  report.attempts.push(attempt1);

  if (!attempt1.ok) {
    const probeHome = makeIsolatedHome();
    const versionInfo = await readChromeVersion(probeHome);
    report.chromeVersionPage = versionInfo;
    const preferred = pickConfigDir(versionInfo, probeHome);
    const ordered = [preferred, ...configCandidates(probeHome).filter((c) => c !== preferred)];
    for (const destDir of ordered) {
      const home = makeIsolatedHome();
      const dest = destDir.replace(probeHome, home);
      const manifestPath = writeManifest(dest, hostPath);
      const attempt = await launchWithExistingUserData(`config:${path.relative(home, dest)}`, {
        isolatedHome: home,
        userDataDir: path.join(home, 'chromium-profile'),
        manifestPath,
      });
      report.attempts.push(attempt);
      if (attempt.ok) break;
    }
  }

  writeFileSync(path.join(RUN_DIR, 'step0-report.json'), `${JSON.stringify(report, null, 2)}\n`);
  console.log(JSON.stringify(report, null, 2));

  const winner = report.attempts.find((a) => a.ok);
  if (!winner) {
    console.error('BLOCKED: no native-messaging manifest path worked');
    process.exit(1);
  }
  console.log(`STEP0_OK manifest=${winner.manifestPath} version=${winner.browserVersion}`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
