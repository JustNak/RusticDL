/**
 * Lightweight regression checks for Firefox download capture heuristics.
 * Run: node ./scripts/test-firefox-capture.mjs
 */
import * as esbuild from 'esbuild';
import { createRequire } from 'module';
import { mkdtempSync, writeFileSync, rmSync, readFileSync } from 'fs';
import { join } from 'path';
import { tmpdir } from 'os';
import { fileURLToPath } from 'url';

const root = fileURLToPath(new URL('..', import.meta.url));
const tmp = mkdtempSync(join(tmpdir(), 'rusticdl-capture-'));
const outfile = join(tmp, 'firefoxCapture.cjs');
const browserStub = join(tmp, 'browser-stub.js');

writeFileSync(
  browserStub,
  'module.exports = { webRequest: null };\nmodule.exports.default = module.exports;\n',
);

await esbuild.build({
  entryPoints: [join(root, 'src/background/firefoxCapture.ts')],
  bundle: true,
  platform: 'node',
  format: 'cjs',
  outfile,
  plugins: [
    {
      name: 'stub-browser',
      setup(build) {
        build.onResolve({ filter: /^\.\/browser$/ }, () => ({ path: browserStub }));
      },
    },
  ],
});

const require = createRequire(import.meta.url);
const chromiumOutfile = join(tmp, 'chromiumCapture.cjs');
await esbuild.build({
  entryPoints: [join(root, 'src/background/chromiumCapture.ts')],
  bundle: true,
  platform: 'node',
  format: 'cjs',
  outfile: chromiumOutfile,
  plugins: [
    {
      name: 'stub-browser',
      setup(build) {
        build.onResolve({ filter: /^\.\/browser$/ }, () => ({ path: browserStub }));
      },
    },
  ],
});
const {
  rememberResponseFilenameHint,
  lookupFilenameHint,
  rememberDeterminedFilename,
  resolveSuggestedFilename,
  applyDeterminedFilename,
  rememberRequestAuth,
  lookupOriginAuth,
} = require(chromiumOutfile);
const sessionOutfile = join(tmp, 'captureSession.cjs');
await esbuild.build({
  entryPoints: [join(root, 'src/background/captureSession.ts')],
  bundle: true,
  platform: 'node',
  format: 'cjs',
  outfile: sessionOutfile,
  plugins: [
    {
      name: 'stub-browser',
      setup(build) {
        build.onResolve({ filter: /^\.\/browser$/ }, () => ({ path: browserStub }));
      },
    },
  ],
});
const {
  createCaptureSessionStore,
  beginHandoff,
  beginPending,
  finishHandoff,
  followCaptureFamily,
  shouldEraseGhostSession,
  decideCreatedAction,
  peekCreatedAction,
  decideFirefoxCandidateAction,
  firefoxQueuedReplayAction,
  createdActionShouldResume,
  sessionsToStorageValue,
  sessionsFromStorageValue,
  dropCaptureSession,
} = require(sessionOutfile);
const {
  firefoxWebRequestDownloadCandidate,
  firefoxBeforeRequestDownloadCandidate,
  abortFirefoxResponseBody,
  shouldCaptureDownloadItem,
  shouldWaitForDownloadSize,
  downloadCreatedAction,
  knownDownloadBytes,
  matchesInterceptedDownload,
  followRestoreSkip,
  CAPTURE_SESSION_TTL_MS,
  RESTORE_SKIP_TTL_MS,
  canonicalDownloadFilename,
  filenameFromContentDisposition,
  isWeakSuggestedFilename,
  preferredSuggestedFilename,
  normalizeCaptureUrl,
  shouldPauseDownloadItem,
  urlIsClaimed,
  cookieStoreIdForHandoff,
  cookieUrlsForHandoff,
  handoffUrlForCapturedDownload,
  isEphemeralSignedUrl,
  isSessionGatewayUrl,
  lookupRedirectSessionUrl,
  rememberDownloadRedirect,
  resetDownloadRedirectsForTests,
  MIN_CAPTURE_BYTES,
  MIN_XHR_CAPTURE_BYTES,
} = require(outfile);

const defaultSettings = {
  enabled: true,
  downloadHandoffMode: 'ask',
  contextMenuEnabled: true,
  showProgressAfterHandoff: true,
  showBadgeStatus: true,
  excludedHosts: [],
  ignoredFileExtensions: [],
  capturedFileExtensions: [
    '7z', 'apk', 'bz2', 'cab', 'csv', 'deb', 'dmg', 'doc', 'docx', 'exe', 'gz',
    'iso', 'jar', 'msi', 'pdf', 'ppt', 'pptx', 'rar', 'rpm', 'tar', 'tgz', 'txz',
    'xls', 'xlsx', 'xz', 'zip', 'zst',
  ],
  downloadCaptureDebugLogging: false,
};

let passed = 0;
let failed = 0;

function assert(name, condition) {
  if (condition) {
    passed += 1;
    console.log(`  ok  ${name}`);
  } else {
    failed += 1;
    console.error(`  FAIL ${name}`);
  }
}

function candidate(details, settings = defaultSettings) {
  return firefoxWebRequestDownloadCandidate(details, settings);
}

console.log('Firefox capture heuristics\n');

// --- YouTube false positive (previous) ---
assert(
  'rejects YouTube suggestqueries xhr with tiny attachment f.txt',
  candidate({
    url: 'https://suggestqueries-clients6.youtube.com/complete/search?client=youtube&q=test&gs_id=0&qa=8cp=0',
    type: 'xmlhttprequest',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'text/plain; charset=utf-8' },
      { name: 'content-disposition', value: 'attachment; filename="f.txt"' },
      { name: 'content-length', value: '46' },
    ],
    originUrl: 'https://www.youtube.com/watch?v=dQw4w9WgXcQ',
    documentUrl: 'https://www.youtube.com/watch?v=dQw4w9WgXcQ',
  }) === null,
);

assert(
  'rejects suggestqueries even as type=other (URL denylist)',
  candidate({
    url: 'https://suggestqueries.google.com/complete/search?q=hello',
    type: 'other',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/octet-stream' },
      { name: 'content-disposition', value: 'attachment; filename="f.txt"' },
      { name: 'content-length', value: String(MIN_CAPTURE_BYTES + 50) },
    ],
  }) === null,
);

assert(
  'rejects bare attachment filename without captured extension',
  candidate({
    url: 'https://cdn.example.com/api/blob',
    type: 'main_frame',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'text/plain' },
      { name: 'content-disposition', value: 'attachment; filename="f.txt"' },
      { name: 'content-length', value: '46' },
    ],
  }) === null,
);

assert(
  'rejects non-captured extension even with large octet-stream attachment',
  candidate({
    url: 'https://cdn.example.com/api/blob',
    type: 'other',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/octet-stream' },
      { name: 'content-disposition', value: 'attachment; filename="f.txt"' },
      { name: 'content-length', value: String(MIN_CAPTURE_BYTES + 50) },
    ],
  }) === null,
);

assert(
  'rejects tiny xhr zip (API noise, not a file-host CDN)',
  candidate({
    url: 'https://cdn.example.com/files/app.zip',
    type: 'xmlhttprequest',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/zip' },
      { name: 'content-disposition', value: 'attachment; filename="app.zip"' },
      { name: 'content-length', value: String(MIN_CAPTURE_BYTES + 50) },
    ],
  }) === null,
);

assert(
  'captures large xhr zip from a file-host CDN',
  candidate({
    url: 'https://store1.gofile.io/download/web/token/app.zip',
    type: 'xmlhttprequest',
    method: 'GET',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/zip' },
      { name: 'content-disposition', value: 'attachment; filename="app.zip"' },
      { name: 'content-length', value: String(MIN_XHR_CAPTURE_BYTES + 1_000_000) },
    ],
  })?.reason === 'attachment_disposition',
);

// --- Nexus Mods live stats CSV (this report) ---
const nexusStats = {
  url: 'https://staticstats.nexusmods.com/live_download_counts/mods/8915.csv',
  type: 'other',
  statusCode: 200,
  originUrl: 'https://www.nexusmods.com/skyrimspecialedition/mods/8915',
  documentUrl: 'https://www.nexusmods.com/skyrimspecialedition/mods/8915',
  responseHeaders: [
    { name: 'content-type', value: 'text/plain' },
    { name: 'access-control-allow-origin', value: '*' },
    { name: 'cache-control', value: 'public, max-age=3600, immutable' },
  ],
};

assert(
  'rejects Nexus live_download_counts CSV via webRequest (text/plain + CORS)',
  candidate(nexusStats) === null,
);

assert(
  'rejects Nexus stats CSV even if Firefox types it as other with no MIME',
  candidate({
    ...nexusStats,
    responseHeaders: [
      { name: 'access-control-allow-origin', value: '*' },
    ],
  }) === null,
);

assert(
  'rejects Nexus stats CSV on downloads.onCreated (weak csv + text/plain)',
  shouldCaptureDownloadItem(
    {
      url: nexusStats.url,
      filename: 'C:\\Users\\ZeusVeilmon\\Downloads\\8915.csv',
      mime: 'text/plain',
      referrer: nexusStats.documentUrl,
    },
    defaultSettings,
  ) === false,
);

assert(
  'rejects Nexus stats CSV on downloads.onCreated even with csv still in captured list',
  shouldCaptureDownloadItem(
    {
      url: nexusStats.url,
      filename: '8915.csv',
      mime: '',
      referrer: nexusStats.documentUrl,
    },
    defaultSettings,
  ) === false,
);

assert(
  'rejects type=other zip with no Content-Type (too noisy; let downloads API decide)',
  candidate({
    url: 'https://cdn.example.com/files/app.zip',
    type: 'other',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-length', value: '5000000' },
    ],
  }) === null,
);

// --- Legitimate captures ---
assert(
  'captures main_frame zip with attachment',
  candidate({
    url: 'https://cdn.example.com/releases/app-1.2.3.zip',
    type: 'main_frame',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/zip' },
      { name: 'content-disposition', value: 'attachment; filename="app-1.2.3.zip"' },
      { name: 'content-length', value: '12000000' },
    ],
  })?.reason === 'attachment_disposition',
);

assert(
  'captures main_frame pdf navigation by MIME',
  candidate({
    url: 'https://files.example.com/docs/manual.pdf',
    type: 'main_frame',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/pdf' },
      { name: 'content-length', value: '250000' },
    ],
  })?.reason === 'download_mime_navigation',
);

assert(
  'captures octet-stream attachment with captured extension',
  candidate({
    url: 'https://dl.example.com/get?id=99',
    type: 'main_frame',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/octet-stream' },
      { name: 'content-disposition', value: 'attachment; filename="setup.exe"' },
      { name: 'content-length', value: '8000000' },
    ],
  })?.reason === 'attachment_disposition',
);

assert(
  'captures main_frame zip with no Content-Type via strong extension',
  candidate({
    url: 'https://cdn.example.com/files/app.zip',
    type: 'main_frame',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-length', value: '5000000' },
    ],
  })?.reason === 'strong_filename_navigation',
);

assert(
  'captures downloads.onCreated zip by strong filename',
  shouldCaptureDownloadItem(
    {
      url: 'https://cdn.example.com/files/app.zip',
      filename: 'app.zip',
      mime: 'application/zip',
      totalBytes: 5_000_000,
    },
    defaultSettings,
  ) === true,
);

assert(
  'captures CORS main_frame pdf without Content-Disposition',
  candidate({
    url: 'https://cdn.example.com/docs/manual.pdf',
    type: 'main_frame',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/pdf' },
      { name: 'access-control-allow-origin', value: '*' },
      { name: 'content-length', value: '250000' },
    ],
  })?.reason === 'download_mime_navigation',
);

assert(
  'captures CORS main_frame zip without Content-Disposition',
  candidate({
    url: 'https://cdn.example.com/files/app.zip',
    type: 'main_frame',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/zip' },
      { name: 'access-control-allow-origin', value: '*' },
      { name: 'content-length', value: '5000000' },
    ],
  })?.reason === 'download_mime_navigation',
);

// --- In-page PDF / Office preview fetches (Grok, chat widgets, PDF.js) ---
const grokPdf = {
  url: 'https://assets.grok.com/users/f0123456789abcdef4a6f7536f/MPC-Operating-Plan.pdf',
  type: 'xmlhttprequest',
  method: 'GET',
  statusCode: 200,
  originUrl: 'https://grok.com/',
  documentUrl: 'https://grok.com/',
  responseHeaders: [
    { name: 'content-type', value: 'application/pdf' },
    { name: 'content-length', value: String(643 * 1024) },
  ],
};

assert(
  'rejects Grok in-page PDF xhr without Content-Disposition',
  candidate(grokPdf) === null,
);

assert(
  'rejects Grok in-page PDF xhr with Content-Disposition: inline',
  candidate({
    ...grokPdf,
    responseHeaders: [
      ...grokPdf.responseHeaders,
      { name: 'content-disposition', value: 'inline; filename="MPC-Operating-Plan.pdf"' },
    ],
  }) === null,
);

assert(
  'captures Grok PDF xhr when Content-Disposition is attachment',
  candidate({
    ...grokPdf,
    responseHeaders: [
      ...grokPdf.responseHeaders,
      { name: 'content-disposition', value: 'attachment; filename="MPC-Operating-Plan.pdf"' },
    ],
  })?.reason === 'attachment_disposition',
);

assert(
  'rejects PDF.js / object embed PDF without attachment',
  candidate({
    ...grokPdf,
    type: 'object',
  }) === null,
);

assert(
  'captures object PDF when Content-Disposition is attachment',
  candidate({
    ...grokPdf,
    type: 'object',
    responseHeaders: [
      ...grokPdf.responseHeaders,
      { name: 'content-disposition', value: 'attachment; filename="MPC-Operating-Plan.pdf"' },
    ],
  })?.reason === 'attachment_disposition',
);

assert(
  'rejects large Office docx xhr without attachment',
  candidate({
    url: 'https://cdn.example.com/exports/report.docx',
    type: 'xmlhttprequest',
    method: 'GET',
    statusCode: 200,
    responseHeaders: [
      {
        name: 'content-type',
        value: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
      },
      { name: 'content-length', value: String(MIN_XHR_CAPTURE_BYTES + 50_000) },
    ],
  }) === null,
);

assert(
  'captures user-added log served as text/plain on onCreated',
  shouldCaptureDownloadItem(
    {
      url: 'https://example.com/debug/app.log',
      filename: 'app.log',
      mime: 'text/plain',
      totalBytes: 50_000,
    },
    {
      ...defaultSettings,
      capturedFileExtensions: [...defaultSettings.capturedFileExtensions, 'log'],
    },
  ) === true,
);

assert(
  'captures same-origin export.csv + octet-stream when csv is still captured',
  shouldCaptureDownloadItem(
    {
      url: 'https://www.example.com/account/export.csv',
      filename: 'export.csv',
      mime: 'application/octet-stream',
      referrer: 'https://www.example.com/account',
      totalBytes: 200_000,
    },
    defaultSettings,
  ) === true,
);

assert(
  'rejects export.csv + octet-stream when csv was dropped from captured list',
  shouldCaptureDownloadItem(
    {
      url: 'https://www.example.com/account/export.csv',
      filename: 'export.csv',
      mime: 'application/octet-stream',
      referrer: 'https://www.example.com/account',
      totalBytes: 200_000,
    },
    {
      ...defaultSettings,
      capturedFileExtensions: defaultSettings.capturedFileExtensions.filter(
        (ext) => ext !== 'csv',
      ),
    },
  ) === false,
);

assert(
  'respects disabled capture',
  candidate(
    {
      url: 'https://cdn.example.com/a.zip',
      type: 'main_frame',
      statusCode: 200,
      responseHeaders: [
        { name: 'content-type', value: 'application/zip' },
        { name: 'content-disposition', value: 'attachment; filename="a.zip"' },
        { name: 'content-length', value: '10000' },
      ],
    },
    { ...defaultSettings, enabled: false },
  ) === null,
);

// --- File-host wait-page stub (SteamRIP / Gofile / Buzzheavier) ---
const steamripStub = {
  url: 'https://buzzheavier.com/dl/Shift-At-Midnight-SteamRIP.com.rar',
  type: 'main_frame',
  method: 'GET',
  statusCode: 200,
  originUrl: 'https://buzzheavier.com/abc',
  documentUrl: 'https://buzzheavier.com/abc',
  responseHeaders: [
    { name: 'content-type', value: 'text/html; charset=utf-8' },
    {
      name: 'content-disposition',
      value: 'attachment; filename="Shift-At-Midnight-SteamRIP.com.rar"',
    },
    { name: 'content-length', value: '3280' },
  ],
};

assert(
  'rejects 3 KB HTML wait-page stub named as .rar (webRequest)',
  candidate(steamripStub) === null,
);

assert(
  'rejects 3 KB octet-stream stub named as .rar (webRequest)',
  candidate({
    ...steamripStub,
    responseHeaders: [
      { name: 'content-type', value: 'application/octet-stream' },
      {
        name: 'content-disposition',
        value: 'attachment; filename="Shift-At-Midnight-SteamRIP.com.rar"',
      },
      { name: 'content-length', value: '3280' },
    ],
  }) === null,
);

assert(
  'rejects 3 KB .rar on downloads.onCreated (strong name no longer bypasses size)',
  shouldCaptureDownloadItem(
    {
      url: steamripStub.url,
      filename: 'C:\\Users\\ZeusVeilmon\\Downloads\\Shift-At-Midnight-SteamRIP.com (1).rar',
      mime: 'application/x-rar-compressed',
      totalBytes: 3280,
    },
    defaultSettings,
  ) === false,
);

assert(
  'waits for size when onCreated .rar has unknown totalBytes',
  shouldWaitForDownloadSize(
    {
      url: steamripStub.url,
      filename: 'Shift-At-Midnight-SteamRIP.com.rar',
      mime: 'application/x-rar-compressed',
      totalBytes: -1,
    },
    defaultSettings,
  ) === true,
);

assert(
  'does not wait when 3 KB .rar size is already known',
  shouldWaitForDownloadSize(
    {
      url: steamripStub.url,
      filename: 'Shift-At-Midnight-SteamRIP.com.rar',
      mime: 'application/x-rar-compressed',
      totalBytes: 3280,
    },
    defaultSettings,
  ) === false,
);

assert(
  'captures the real 381 MB .rar attachment',
  candidate({
    url: 'https://cdn.buzzheavier.com/file/Shift-At-Midnight-SteamRIP.com.rar',
    type: 'main_frame',
    method: 'GET',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/x-rar-compressed' },
      {
        name: 'content-disposition',
        value: 'attachment; filename="Shift-At-Midnight-SteamRIP.com.rar"',
      },
      { name: 'content-length', value: String(381 * 1024 * 1024) },
    ],
  })?.reason === 'attachment_disposition',
);

assert(
  'captures large CORS CDN .rar without Content-Disposition',
  candidate({
    url: 'https://store1.gofile.io/download/web/token/Shift-At-Midnight-SteamRIP.com.rar',
    type: 'other',
    method: 'GET',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/octet-stream' },
      { name: 'access-control-allow-origin', value: '*' },
      { name: 'content-length', value: String(381 * 1024 * 1024) },
    ],
  })?.reason === 'download_mime_navigation',
);

assert(
  'still rejects CORS Nexus CSV after CDN exception',
  candidate(nexusStats) === null,
);

assert(
  'rejects POST even when it looks like a zip (cannot replay as GET)',
  candidate({
    url: 'https://1fichier.com/get/abc.zip',
    type: 'main_frame',
    method: 'POST',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/zip' },
      { name: 'content-disposition', value: 'attachment; filename="abc.zip"' },
      { name: 'content-length', value: '12000000' },
    ],
  }) === null,
);

assert(
  'rejects HEAD size probes even when they look like a large zip',
  candidate({
    url: 'https://cdn.example.com/files/app.zip',
    type: 'xmlhttprequest',
    method: 'HEAD',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/zip' },
      { name: 'content-disposition', value: 'attachment; filename="app.zip"' },
      { name: 'content-length', value: String(MIN_XHR_CAPTURE_BYTES + 1_000_000) },
    ],
  }) === null,
);

assert(
  'captures chunked xhr zip when attachment + strong mime (no Content-Length)',
  candidate({
    url: 'https://store1.gofile.io/download/web/token/app.zip',
    type: 'xmlhttprequest',
    method: 'GET',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/zip' },
      { name: 'content-disposition', value: 'attachment; filename="app.zip"' },
    ],
  })?.reason === 'attachment_disposition',
);

assert(
  'rejects chunked xhr zip without attachment (too noisy)',
  candidate({
    url: 'https://cdn.example.com/files/app.zip',
    type: 'xmlhttprequest',
    method: 'GET',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/zip' },
    ],
  }) === null,
);

assert(
  'onCreated waits when strong-name .rar has unknown totalBytes',
  downloadCreatedAction(
    {
      url: steamripStub.url,
      filename: 'Shift-At-Midnight-SteamRIP.com.rar',
      mime: 'application/x-rar-compressed',
      totalBytes: -1,
    },
    defaultSettings,
  ) === 'wait',
);

assert(
  'onCreated ignores known 3 KB .rar instead of capturing',
  downloadCreatedAction(
    {
      url: steamripStub.url,
      filename: 'Shift-At-Midnight-SteamRIP.com.rar',
      mime: 'application/x-rar-compressed',
      totalBytes: 3280,
    },
    defaultSettings,
  ) === 'ignore',
);

assert(
  'onCreated captures a large .rar once size is known',
  downloadCreatedAction(
    {
      url: steamripStub.url,
      filename: 'Shift-At-Midnight-SteamRIP.com.rar',
      mime: 'application/x-rar-compressed',
      totalBytes: 381 * 1024 * 1024,
    },
    defaultSettings,
  ) === 'capture',
);

assert(
  'treats Firefox fileSize as known bytes when totalBytes is -1',
  knownDownloadBytes({ url: steamripStub.url, totalBytes: -1, fileSize: 3280 }) === 3280,
);

assert(
  'does not wait when fileSize already shows a 3 KB stub',
  shouldWaitForDownloadSize(
    {
      url: steamripStub.url,
      filename: 'Shift-At-Midnight-SteamRIP.com.rar',
      mime: 'application/x-rar-compressed',
      totalBytes: -1,
      fileSize: 3280,
    },
    defaultSettings,
  ) === false,
);

assert(
  'rejects 3 KB .rar when only fileSize is known',
  shouldCaptureDownloadItem(
    {
      url: steamripStub.url,
      filename: 'Shift-At-Midnight-SteamRIP.com.rar',
      mime: 'application/x-rar-compressed',
      totalBytes: -1,
      fileSize: 3280,
    },
    defaultSettings,
  ) === false,
);

console.log('\nFirefox ghost download suppression\n');

assert(
  'strips Firefox (1) collision suffix without a space',
  canonicalDownloadFilename('Shift-At-Midnight-SteamRIP.com(1).rar')
    === 'Shift-At-Midnight-SteamRIP.com.rar',
);

assert(
  'strips Firefox (1) collision suffix with a space',
  canonicalDownloadFilename('C:\\\\Users\\\\ZeusVeilmon\\\\Downloads\\\\Shift-At-Midnight-SteamRIP.com (1).rar')
    === 'Shift-At-Midnight-SteamRIP.com.rar',
);

assert(
  'does not strip a 4-digit year in the filename',
  canonicalDownloadFilename('Game (2024).rar') === 'Game (2024).rar',
);

assert(
  'normalizes capture URLs by dropping the hash',
  normalizeCaptureUrl('https://cdn.example.com/file.rar#frag')
    === 'https://cdn.example.com/file.rar',
);

const intercepted = [
  {
    url: 'https://cdn.buzzheavier.com/file/Shift-At-Midnight-SteamRIP.com.rar',
    filename: 'Shift-At-Midnight-SteamRIP.com.rar',
    ts: Date.now(),
  },
];

assert(
  'matches a ghost download item by URL after webRequest cancel',
  matchesInterceptedDownload(
    {
      url: 'https://cdn.buzzheavier.com/file/Shift-At-Midnight-SteamRIP.com.rar',
      filename: 'Shift-At-Midnight-SteamRIP.com(1).rar',
    },
    intercepted,
  ) === true,
);

assert(
  'matches a ghost download item by filename when Firefox used a redirected URL',
  matchesInterceptedDownload(
    {
      url: 'https://download.pixeldrain.com/u/eFKBSa/Skedule-I-SteamRIP.com.rar',
      filename: 'C:\\\\Users\\\\ZeusVeilmon\\\\Downloads\\\\Shift-At-Midnight-SteamRIP.com(1).rar',
    },
    intercepted,
  ) === true,
);

assert(
  'does not match an unrelated download',
  matchesInterceptedDownload(
    {
      url: 'https://cdn.example.com/other.zip',
      filename: 'other.zip',
    },
    intercepted,
  ) === false,
);

assert(
  'does not match an expired intercept record',
  matchesInterceptedDownload(
    {
      url: intercepted[0].url,
      filename: intercepted[0].filename,
    },
    [{ ...intercepted[0], ts: Date.now() - 30_000 }],
  ) === false,
);

assert(
  'treats a claimed URL as already handed off',
  urlIsClaimed(
    'https://cdn.buzzheavier.com/file/Shift-At-Midnight-SteamRIP.com.rar#x',
    ['https://cdn.buzzheavier.com/file/Shift-At-Midnight-SteamRIP.com.rar'],
  ) === true,
);

const gofileCdn = {
  url: 'https://file-ap-hkg-1.gofile.io/download/web/4526a00b8d/REPO-SteamRIP.com.rar',
  method: 'GET',
  statusCode: 200,
  originUrl: 'https://gofile.io/d/eFKBSa',
  documentUrl: 'https://gofile.io/d/eFKBSa',
  responseHeaders: [
    { name: 'content-type', value: 'application/octet-stream' },
    { name: 'access-control-allow-origin', value: '*' },
    { name: 'content-length', value: String(596 * 1024 * 1024) },
    {
      name: 'content-disposition',
      value: 'attachment; filename="REPO-SteamRIP.com.rar"',
    },
  ],
};

assert(
  'captures Gofile CDN xhr .rar (SteamRIP-style fetch)',
  candidate({ ...gofileCdn, type: 'xmlhttprequest' })?.reason === 'attachment_disposition',
);

assert(
  'captures Gofile CDN .rar as type=other without waiting for onCreated',
  candidate({ ...gofileCdn, type: 'other' })?.reason === 'attachment_disposition',
);

assert(
  'captures Gofile CDN .rar even when Content-Disposition is omitted',
  candidate({
    ...gofileCdn,
    type: 'xmlhttprequest',
    responseHeaders: gofileCdn.responseHeaders.filter(
      (header) => header.name !== 'content-disposition',
    ),
  })?.reason === 'download_mime_navigation',
);

assert(
  'pauses onCreated immediately when Firefox has not filled totalBytes yet',
  shouldPauseDownloadItem(
    {
      url: gofileCdn.url,
      filename: 'REPO-SteamRIP.com.rar',
      mime: 'application/octet-stream',
      totalBytes: -1,
    },
    defaultSettings,
  ) === true,
);

assert(
  'does not pause a known 3 KB wait-page stub',
  shouldPauseDownloadItem(
    {
      url: steamripStub.url,
      filename: 'Shift-At-Midnight-SteamRIP.com.rar',
      mime: 'application/x-rar-compressed',
      totalBytes: 3280,
    },
    defaultSettings,
  ) === false,
);

assert(
  'cancels Gofile CDN .rar on onBeforeRequest (before any body)',
  firefoxBeforeRequestDownloadCandidate(
    {
      url: 'https://file-ap-hkg-1.gofile.io/download/web/4526a00b8d/RAFT-SteamRIP.com.rar',
      method: 'GET',
      type: 'xmlhttprequest',
    },
    defaultSettings,
  )?.reason === 'file_host_cdn_url',
);

assert(
  'cancels Pixeldrain object URL on onBeforeRequest',
  firefoxBeforeRequestDownloadCandidate(
    {
      url: 'https://pixeldrain.com/api/file/abc123/RAFT-SteamRIP.com.rar',
      method: 'GET',
    },
    defaultSettings,
  )?.reason === 'file_host_cdn_url',
);

assert(
  'cancels Buzzheavier object CDN .rar on onBeforeRequest',
  firefoxBeforeRequestDownloadCandidate(
    {
      url: 'https://cdn.buzzheavier.com/file/Shift-At-Midnight-SteamRIP.com.rar',
      method: 'GET',
    },
    defaultSettings,
  )?.reason === 'file_host_cdn_url',
);

assert(
  'does not cancel Buzzheavier /dl wait-page named as .rar on onBeforeRequest',
  firefoxBeforeRequestDownloadCandidate(steamripStub, defaultSettings) === null,
);

assert(
  'does not cancel Gofile listing pages that are not a file URL',
  firefoxBeforeRequestDownloadCandidate(
    {
      url: 'https://gofile.io/d/eFKBSa',
      method: 'GET',
      type: 'main_frame',
    },
    defaultSettings,
  ) === null,
);

assert(
  'does not cancel non-CDN .rar URLs on onBeforeRequest',
  firefoxBeforeRequestDownloadCandidate(
    {
      url: 'https://cdn.example.com/files/RAFT-SteamRIP.com.rar',
      method: 'GET',
    },
    defaultSettings,
  ) === null,
);

assert(
  'respects ignoredFileExtensions on onBeforeRequest',
  firefoxBeforeRequestDownloadCandidate(
    {
      url: 'https://file-ap-hkg-1.gofile.io/download/web/4526a00b8d/RAFT-SteamRIP.com.rar',
      method: 'GET',
    },
    { ...defaultSettings, ignoredFileExtensions: ['rar'] },
  ) === null,
);

let closed = 0;
assert(
  'closes the Firefox response filter so the body cannot continue',
  abortFirefoxResponseBody(
    {
      filterResponseData: () => ({
        onstart: null,
        ondata: null,
        onstop: null,
        onerror: null,
        close() {
          closed += 1;
        },
      }),
    },
    'req-1',
  ) === true && closed >= 1,
);

console.log('\nChromium suggested filename (CDN token vs Content-Disposition)\n');

const tokenName = '9daaa83c-7e52-4f70-a1b9-17dee6eb5cb2';
const realVideoName =
  'HELL.MODE.The.Hardcore.Gamer.Dominates.in.Another.World.with.Garbage.Balancing.S02E02.1080p.HIDI.WEB-DL.DUAL.AAC2.0.H.264-VARYG.mkv';

assert(
  'parses a quoted Content-Disposition filename',
  filenameFromContentDisposition(`attachment; filename="${realVideoName}"`) === realVideoName,
);

assert(
  'prefers filename* over the plain filename parameter',
  filenameFromContentDisposition(
    'attachment; filename="fallback.bin"; filename*=UTF-8\'\'show%20title.mkv',
  ) === 'show title.mkv',
);

assert(
  'treats a UUID path token as a weak suggested filename',
  isWeakSuggestedFilename(tokenName) === true,
);

assert(
  'treats Chrome Unconfirmed .crdownload names as weak',
  isWeakSuggestedFilename('C:\\\\Users\\\\ZeusVeilmon\\\\Downloads\\\\Unconfirmed 12345.crdownload')
    === true,
);

assert(
  'does not treat a real release name as weak',
  isWeakSuggestedFilename(realVideoName) === false,
);

assert(
  'does not treat an extension-less real name as a CDN token',
  isWeakSuggestedFilename('LICENSE') === false,
);

assert(
  'prefers the Content-Disposition name over a URL token',
  preferredSuggestedFilename(realVideoName, tokenName) === realVideoName,
);

assert(
  'captures a Chromium CDN token + octet-stream download',
  shouldCaptureDownloadItem(
    {
      url: `https://store-073.wnam.tb-cdn.example/${tokenName}`,
      filename: tokenName,
      mime: 'application/octet-stream',
      totalBytes: 950 * 1024 * 1024,
    },
    defaultSettings,
  ) === true,
);

assert(
  'unknown-size non-captured .mkv + octet-stream is ignore, not a size wait',
  downloadCreatedAction(
    {
      url: 'https://cdn.example.com/files/movie.mkv',
      filename: 'movie.mkv',
      mime: 'application/octet-stream',
      totalBytes: -1,
    },
    defaultSettings,
  ) === 'ignore',
);

assert(
  'token + octet-stream is still a capture decision (name wait is separate)',
  downloadCreatedAction(
    {
      url: `https://store-073.wnam.tb-cdn.example/${tokenName}`,
      filename: tokenName,
      mime: 'application/octet-stream',
      totalBytes: 950 * 1024 * 1024,
    },
    defaultSettings,
  ) === 'capture',
);

assert(
  'still captures a strong zip name immediately',
  downloadCreatedAction(
    {
      url: 'https://cdn.example.com/files/app.zip',
      filename: 'app.zip',
      mime: 'application/zip',
      totalBytes: 5_000_000,
    },
    defaultSettings,
  ) === 'capture',
);

assert(
  'resolved name is strong once a Content-Disposition candidate exists',
  isWeakSuggestedFilename(preferredSuggestedFilename(realVideoName, tokenName)) === false,
);

const tokenUrl = `https://store-073.wnam.tb-cdn.example/${tokenName}`;
rememberResponseFilenameHint({
  url: tokenUrl,
  responseHeaders: [
    { name: 'content-type', value: 'application/octet-stream' },
    { name: 'content-length', value: String(950 * 1024 * 1024) },
    { name: 'content-disposition', value: `attachment; filename="${realVideoName}"` },
  ],
});

assert(
  'caches Content-Disposition from Chromium webRequest headers',
  lookupFilenameHint(tokenUrl)?.filename === realVideoName,
);

assert(
  'resolves a Chromium download item to the cached Content-Disposition name',
  resolveSuggestedFilename({
    id: 42,
    url: tokenUrl,
    filename: `C:\\\\Users\\\\ZeusVeilmon\\\\Downloads\\\\${tokenName}`,
  }) === realVideoName,
);

rememberDeterminedFilename(7, realVideoName);
assert(
  'uses onDeterminingFilename when headers were missed',
  resolveSuggestedFilename({
    id: 7,
    url: 'https://cdn.example.com/download/abc',
    filename: 'abc',
  }) === realVideoName,
);

assert(
  'does not cache Content-Length from ordinary page responses',
  rememberResponseFilenameHint({
    url: 'https://cdn.example.com/assets/app.css',
    responseHeaders: [
      { name: 'content-type', value: 'text/css' },
      { name: 'content-length', value: '4096' },
    ],
  }) === undefined
    && lookupFilenameHint('https://cdn.example.com/assets/app.css') === undefined,
);

let suggestedOverride = 'unset';
const determined = applyDeterminedFilename(
  {
    id: 9,
    url: tokenUrl,
    filename: realVideoName,
  },
  (suggestion) => {
    suggestedOverride = suggestion;
  },
);
assert(
  'onDeterminingFilename observes the name without rewriting Chrome',
  determined === realVideoName && suggestedOverride === undefined,
);

assert(
  'handoff prefers Canvas session URL over Drive/Inst-FS finalUrl',
  handoffUrlForCapturedDownload({
    url: 'https://school.instructure.com/files/99/download?download_frd=1',
    finalUrl: 'https://drive.google.com/uc?export=download&id=abc',
  }) === 'https://school.instructure.com/files/99/download?download_frd=1',
);

assert(
  'handoff keeps same-origin finalUrl (CDN path after redirect)',
  handoffUrlForCapturedDownload({
    url: 'https://cdn.example.com/ticket/abc',
    finalUrl: 'https://cdn.example.com/files/app.zip',
  }) === 'https://cdn.example.com/files/app.zip',
);

assert(
  'same-origin Canvas verifier is treated as a consumed hop',
  handoffUrlForCapturedDownload({
    url: 'https://school.instructure.com/files/99/download?download_frd=1',
    finalUrl: 'https://school.instructure.com/files/99/download?verifier=USED',
  }) === 'https://school.instructure.com/files/99/download?download_frd=1',
);

assert(
  'Inst-FS and Drive export URLs are ephemeral signed hops',
  isEphemeralSignedUrl('https://inst-fs-iad-prod.inscloudgate.net/files/abc?token=1')
    && isEphemeralSignedUrl('https://drive.google.com/uc?export=download&id=abc')
    && !isEphemeralSignedUrl('https://cdn.example.com/files/app.zip'),
);

assert(
  'Canvas file download URLs are session gateways',
  isSessionGatewayUrl('https://school.instructure.com/files/99/download?download_frd=1')
    && isSessionGatewayUrl('https://school.instructure.com/courses/2/files/99')
    && !isSessionGatewayUrl('https://cdn.example.com/files/app.zip'),
);

resetDownloadRedirectsForTests();
rememberDownloadRedirect(
  'https://school.instructure.com/files/99/download?download_frd=1',
  'https://inst-fs-iad-prod.inscloudgate.net/files/abc?token=1',
);
rememberDownloadRedirect(
  'https://inst-fs-iad-prod.inscloudgate.net/files/abc?token=1',
  'https://drive.google.com/uc?export=download&id=abc',
);
assert(
  'redirect chain maps Drive/Inst-FS back to the Canvas session URL',
  lookupRedirectSessionUrl('https://drive.google.com/uc?export=download&id=abc')
    === 'https://school.instructure.com/files/99/download?download_frd=1'
    && handoffUrlForCapturedDownload({
      url: 'https://inst-fs-iad-prod.inscloudgate.net/files/abc?token=1',
      finalUrl: 'https://drive.google.com/uc?export=download&id=abc',
    }) === 'https://school.instructure.com/files/99/download?download_frd=1',
);
resetDownloadRedirectsForTests();

const driveUc = 'https://drive.google.com/uc?export=download&id=abc';
const canvasSessionUrl = 'https://school.instructure.com/files/99/download?download_frd=1';
const driveAttachmentHeaders = [
  { name: 'content-type', value: 'application/zip' },
  { name: 'content-disposition', value: 'attachment; filename="file.zip"' },
  { name: 'content-length', value: '12000000' },
];

assert(
  'skips native Drive /uc?export=download onCreated even with docx + octet-stream + large size',
  shouldCaptureDownloadItem(
    {
      url: driveUc,
      filename: 'report.docx',
      mime: 'application/octet-stream',
      totalBytes: 5_000_000,
    },
    defaultSettings,
  ) === false,
);

assert(
  'skips googleusercontent hop onCreated without a remembered session',
  shouldCaptureDownloadItem(
    {
      url: 'https://doc-00-00-docs.googleusercontent.com/docs/securesc/file.docx',
      filename: 'file.docx',
      mime: 'application/octet-stream',
      totalBytes: 5_000_000,
    },
    defaultSettings,
  ) === false,
);

assert(
  'skips Telegram web.telegram.org /k/d download onCreated',
  shouldCaptureDownloadItem(
    {
      url: 'https://web.telegram.org/k/d/123',
      filename: 'notes.pdf',
      mime: 'application/pdf',
      totalBytes: 250_000,
    },
    defaultSettings,
  ) === false,
);

assert(
  'skips Dropboxusercontent file.zip onCreated',
  shouldCaptureDownloadItem(
    {
      url: 'https://dl.dropboxusercontent.com/s/x/file.zip',
      filename: 'file.zip',
      mime: 'application/zip',
      totalBytes: 5_000_000,
    },
    defaultSettings,
  ) === false,
);

assert(
  'user excludedHosts still skips a listed random host',
  shouldCaptureDownloadItem(
    {
      url: 'https://files.randomhost.example/a.zip',
      filename: 'a.zip',
      mime: 'application/zip',
      totalBytes: 5_000_000,
    },
    { ...defaultSettings, excludedHosts: ['randomhost.example'] },
  ) === false,
);

assert(
  'firefoxWebRequestDownloadCandidate skips Drive main_frame zip with attachment',
  candidate({
    url: driveUc,
    type: 'main_frame',
    statusCode: 200,
    responseHeaders: driveAttachmentHeaders,
  }) === null,
);

assert(
  'firefoxWebRequestDownloadCandidate keeps Canvas page Drive hops without a remembered redirect',
  candidate({
    url: driveUc,
    type: 'main_frame',
    statusCode: 200,
    originUrl: canvasSessionUrl,
    documentUrl: canvasSessionUrl,
    responseHeaders: driveAttachmentHeaders,
  }) !== null,
);

resetDownloadRedirectsForTests();
rememberDownloadRedirect(canvasSessionUrl, driveUc);
assert(
  'captures Canvas→Drive onCreated after rememberDownloadRedirect',
  shouldCaptureDownloadItem(
    {
      url: canvasSessionUrl,
      finalUrl: driveUc,
      filename: 'report.docx',
      mime: 'application/octet-stream',
      totalBytes: 5_000_000,
    },
    defaultSettings,
  ) === true,
);
assert(
  'firefoxWebRequestDownloadCandidate still captures Canvas→Drive after rememberDownloadRedirect',
  candidate({
    url: driveUc,
    type: 'main_frame',
    statusCode: 200,
    originUrl: canvasSessionUrl,
    documentUrl: canvasSessionUrl,
    responseHeaders: driveAttachmentHeaders,
  }) !== null,
);
resetDownloadRedirectsForTests();

assert(
  'cookie URLs are one http(s) URL per origin',
  JSON.stringify(cookieUrlsForHandoff([
    'https://school.instructure.com/files/1/download?download_frd=1',
    'https://school.instructure.com/courses/2/files',
    'https://drive.google.com/uc?id=abc',
    'blob:https://school.instructure.com/uuid',
  ])) === JSON.stringify([
    'https://school.instructure.com/files/1/download?download_frd=1',
    'https://drive.google.com/uc?id=abc',
  ]),
);

assert(
  'Chromium cookie stores without incognito do not guess a storeId',
  cookieStoreIdForHandoff([{ id: '0', tabIds: [1] }], { incognito: false }) === undefined,
);

assert(
  'Firefox cookieStoreId is preferred over store listing',
  cookieStoreIdForHandoff(
    [{ id: 'firefox-default', incognito: false }],
    { cookieStoreId: 'firefox-container-1', incognito: false },
  ) === 'firefox-container-1',
);

assert(
  'Firefox incognito store is selected only when the flag is present',
  cookieStoreIdForHandoff(
    [
      { id: 'firefox-default', incognito: false },
      { id: 'firefox-private', incognito: true },
    ],
    { incognito: true },
  ) === 'firefox-private',
);

rememberRequestAuth({
  url: 'https://school.instructure.com/files/99/download?download_frd=1',
  requestHeaders: [
    { name: 'Cookie', value: 'canvas_session=abc' },
    { name: 'Authorization', value: 'Bearer canvas-token' },
    { name: 'Accept', value: '*/*' },
  ],
});
const capturedAuth = lookupOriginAuth([
  'https://school.instructure.com/files/99/download?download_frd=1',
  'https://drive.google.com/uc?id=abc',
]);
assert(
  'captures Cookie and Authorization from Chromium request headers',
  capturedAuth.length === 1
    && capturedAuth[0].origin === 'https://school.instructure.com'
    && capturedAuth[0].headers.some((header) => header.name === 'Cookie')
    && capturedAuth[0].headers.some((header) => header.name === 'Authorization')
    && !capturedAuth[0].headers.some((header) => header.name === 'Accept'),
);

console.log('\nFirefox dismissed restore skip\n');

const restoreUrls = new Map();
const restoreIds = new Map();
const canvasSession = 'https://school.instructure.com/files/99/download?download_frd=1';
const driveHop = 'https://drive.google.com/uc?export=download&id=abc';
const docsHop = 'https://doc-00-00-docs.googleusercontent.com/docs/securesc/file.docx';
restoreUrls.set(canvasSession, Date.now());

assert(
  'skips the restored Canvas session URL',
  followRestoreSkip({
    url: canvasSession,
    requestId: 'req-restore',
    skippedUrls: restoreUrls,
    skippedRequestIds: restoreIds,
  }) === true,
);

assert(
  'same Firefox requestId follows a Drive redirect after dismiss',
  followRestoreSkip({
    url: driveHop,
    requestId: 'req-restore',
    skippedUrls: restoreUrls,
    skippedRequestIds: restoreIds,
  }) === true
    && restoreUrls.has(driveHop),
);

resetDownloadRedirectsForTests();
rememberDownloadRedirect(canvasSession, driveHop);
rememberDownloadRedirect(driveHop, docsHop);
assert(
  'skips a Drive hop whose remembered session URL was restored',
  followRestoreSkip({
    url: docsHop,
    skippedUrls: restoreUrls,
    skippedRequestIds: new Map(),
    sessionUrl: lookupRedirectSessionUrl(docsHop),
  }) === true,
);
resetDownloadRedirectsForTests();

assert(
  'does not skip an unrelated later download',
  followRestoreSkip({
    url: 'https://cdn.example.com/other.zip',
    requestId: 'req-other',
    skippedUrls: restoreUrls,
    skippedRequestIds: restoreIds,
  }) === false,
);

assert(
  'expired restore skip can be recaptured',
  followRestoreSkip({
    url: canvasSession,
    skippedUrls: new Map([[canvasSession, Date.now() - RESTORE_SKIP_TTL_MS - 1]]),
    skippedRequestIds: new Map(),
  }) === false,
);

assert(
  'onCreated matches a restored Drive file by filename after redirect',
  matchesInterceptedDownload(
    {
      url: docsHop,
      filename: 'Google Drive Folder.docx',
    },
    [{
      url: canvasSession,
      filename: 'Google Drive Folder.docx',
      ts: Date.now(),
    }],
  ) === true,
);

console.log('\nUnified onCreated / Firefox previewable policy\n');

assert(
  'rejects Grok-style PDF on onCreated without attachment (same as webRequest)',
  shouldCaptureDownloadItem(
    {
      url: grokPdf.url,
      filename: 'MPC-Operating-Plan.pdf',
      mime: 'application/pdf',
      totalBytes: 643 * 1024,
      referrer: 'https://grok.com/',
    },
    defaultSettings,
  ) === false,
);

assert(
  'captures a PDF shelf download when Content-Disposition is attachment',
  shouldCaptureDownloadItem(
    {
      url: grokPdf.url,
      filename: 'MPC-Operating-Plan.pdf',
      mime: 'application/pdf',
      totalBytes: 643 * 1024,
      hasAttachment: true,
    },
    defaultSettings,
  ) === true,
);

assert(
  'rejects MIME-only octet-stream with no filename',
  shouldCaptureDownloadItem(
    {
      url: 'https://cdn.example.com/api/blob',
      mime: 'application/octet-stream',
      totalBytes: 5_000_000,
    },
    defaultSettings,
  ) === false,
);

assert(
  'skips Drive on Firefox onBeforeRequest (app-owned origin)',
  firefoxBeforeRequestDownloadCandidate(
    {
      url: 'https://drive.google.com/uc?export=download&id=abc&confirm=1/file.zip',
      method: 'GET',
    },
    defaultSettings,
  ) === null,
);

console.log('\nCapture session coordinator (false + looping prompts)\n');

const zipItem = {
  url: 'https://cdn.example.com/files/app.zip',
  filename: 'app.zip',
  mime: 'application/zip',
  totalBytes: 5_000_000,
};
const canvasUrl = 'https://school.instructure.com/files/99/download?download_frd=1';
const driveUrl = 'https://drive.google.com/uc?export=download&id=abc';
const docsUrl = 'https://doc-00-00-docs.googleusercontent.com/docs/securesc/file.docx';

const storeA = createCaptureSessionStore();
const first = decideCreatedAction(storeA, zipItem, defaultSettings);
assert('first zip onCreated claims a handoff session', first === 'handoff');

const retryAt30s = decideCreatedAction(
  storeA,
  zipItem,
  defaultSettings,
  {},
  Date.now() + 30_000,
);
assert(
  'same URL retry 30s into an open ask-prompt does not start a second handoff',
  retryAt30s === 'erase-ghost' && storeA.sessions.length === 1,
);

const firefoxRetry = decideFirefoxCandidateAction(
  storeA,
  { urls: [zipItem.url], filename: zipItem.filename },
  Date.now() + 30_000,
);
assert(
  'Firefox webRequest retry during handoff only cancels the ghost',
  firefoxRetry === 'cancel-ghost',
);

const storeB = createCaptureSessionStore();
const claimed = beginHandoff(storeB, {
  urls: [canvasUrl],
  requestId: 'req-restore',
  filename: 'Google Drive Folder.docx',
});
finishHandoff(storeB, claimed.session.id, 'rejected');
followCaptureFamily(storeB, {
  urls: [driveUrl],
  requestId: 'req-restore',
  sessionUrl: canvasUrl,
});
assert(
  'Canvas → Drive 302 after host-error restore stays restoring',
  storeB.sessions[0]?.phase === 'restoring'
    && storeB.sessions[0].urls.includes(driveUrl),
);

assert(
  'restoring Drive hop is not treated as a ghost to erase',
  shouldEraseGhostSession(storeB, { urls: [docsUrl], requestId: 'req-restore', sessionUrl: canvasUrl })
    === undefined,
);

assert(
  'onCreated after host-error restore+redirect is skip-restore, not a new prompt',
  decideCreatedAction(
    storeB,
    { url: docsUrl, filename: 'Google Drive Folder.docx', mime: 'application/pdf', hasAttachment: true, totalBytes: 20_000 },
    defaultSettings,
    { requestId: 'req-restore', sessionUrl: canvasUrl },
  ) === 'skip-restore',
);

const storeC = createCaptureSessionStore();
const longPrompt = beginHandoff(storeC, { urls: [zipItem.url], filename: zipItem.filename });
const after15s = Date.now() + 15_001;
finishHandoff(storeC, longPrompt.session.id, 'rejected', after15s);
assert(
  'host-error restore after 15s still skips recapture (session TTL is the prompt window, not 15s)',
  decideCreatedAction(
    storeC,
    zipItem,
    defaultSettings,
    {},
    after15s + 1_000,
  ) === 'skip-restore',
);

const cancelStore = createCaptureSessionStore();
const userCancel = beginHandoff(cancelStore, { urls: [zipItem.url], filename: zipItem.filename });
finishHandoff(cancelStore, userCancel.session.id, 'canceled');
assert(
  'user cancel marks the family canceled',
  cancelStore.sessions[0]?.phase === 'canceled',
);
assert(
  'onCreated after user cancel erases the ghost instead of restoring',
  decideCreatedAction(cancelStore, zipItem, defaultSettings) === 'erase-ghost'
    && shouldEraseGhostSession(cancelStore, { urls: [zipItem.url] }) != null,
);
assert(
  'Firefox retry after user cancel is cancel-ghost, not allow',
  decideFirefoxCandidateAction(cancelStore, { urls: [zipItem.url], filename: zipItem.filename })
    === 'cancel-ghost',
);
const canceledDump = sessionsToStorageValue(cancelStore);
const canceledReloaded = sessionsFromStorageValue(canceledDump, Date.now() + 30_000);
assert(
  'canceled session reloads and still erases ghosts',
  canceledReloaded.sessions[0]?.phase === 'canceled'
    && decideCreatedAction(canceledReloaded, zipItem, defaultSettings, {}, Date.now() + 30_000)
      === 'erase-ghost',
);

const storeD = createCaptureSessionStore();
const accepted = beginHandoff(storeD, { urls: [zipItem.url], filename: zipItem.filename });
finishHandoff(storeD, accepted.session.id, 'accepted');
const dumped = sessionsToStorageValue(storeD);
const reloaded = sessionsFromStorageValue(dumped, Date.now() + 30_000);
assert(
  'session reload from storage still skips an accepted family',
  decideCreatedAction(reloaded, zipItem, defaultSettings, {}, Date.now() + 30_000) === 'erase-ghost',
);

const expired = sessionsFromStorageValue(dumped, Date.now() + CAPTURE_SESSION_TTL_MS + 1);
assert(
  'expired session can be recaptured after the prompt window',
  decideCreatedAction(expired, zipItem, defaultSettings) === 'handoff',
);

assert(
  'filename-only does not erase an unrelated report.pdf',
  shouldEraseGhostSession(
    storeD,
    { urls: ['https://other.example.com/report.pdf'], filename: 'app.zip' },
  ) === undefined,
);

const storeE = createCaptureSessionStore();
beginPending(storeE, { urls: [zipItem.url] });
dropCaptureSession(storeE, storeE.sessions[0].id);
assert(
  'dropping a pending stub wait allows the later real file to claim',
  decideCreatedAction(storeE, zipItem, defaultSettings) === 'handoff',
);

const peekStore = createCaptureSessionStore();
assert(
  'peek does not open a session (sync pause path)',
  peekCreatedAction(peekStore, zipItem, defaultSettings) === 'handoff'
    && peekStore.sessions.length === 0,
);

assert(
  'skip-restore and ignore resume a download paused before hydrate',
  createdActionShouldResume('skip-restore')
    && createdActionShouldResume('ignore')
    && !createdActionShouldResume('handoff')
    && !createdActionShouldResume('wait')
    && !createdActionShouldResume('erase-ghost'),
);

const restorePauseStore = createCaptureSessionStore();
const hostRejected = beginHandoff(restorePauseStore, { urls: [zipItem.url], filename: zipItem.filename });
finishHandoff(restorePauseStore, hostRejected.session.id, 'rejected');
assert(
  'host-error site retry is skip-restore so a pre-hydrate pause must be released',
  decideCreatedAction(restorePauseStore, zipItem, defaultSettings) === 'skip-restore'
    && createdActionShouldResume('skip-restore'),
);

assert(
  'queued Firefox cancel before hydrate hands off when still a candidate',
  firefoxQueuedReplayAction(undefined, true) === 'handoff',
);
assert(
  'queued Firefox cancel restores when settings later say Off/ignore',
  firefoxQueuedReplayAction(undefined, false) === 'restore',
);
assert(
  'queued Firefox cancel during restoring does not open a second prompt',
  firefoxQueuedReplayAction('restoring', true) === 'restore',
);
assert(
  'queued Firefox cancel of an in-flight handoff is treated as a ghost',
  firefoxQueuedReplayAction('handoff', true) === 'ignore',
);
assert(
  'queued Firefox cancel of an accepted family stays canceled',
  firefoxQueuedReplayAction('accepted', true) === 'ignore',
);
assert(
  'queued Firefox cancel of a user-canceled family stays canceled',
  firefoxQueuedReplayAction('canceled', true) === 'ignore',
);
assert(
  'queued Firefox cancel on a pending wait hands off when still a candidate',
  firefoxQueuedReplayAction('pending', true) === 'handoff',
);
assert(
  'queued Firefox cancel on a pending wait restores when no longer a candidate',
  firefoxQueuedReplayAction('pending', false) === 'restore',
);

assert(
  'strong main_frame zip still captures via webRequest',
  candidate({
    url: zipItem.url,
    type: 'main_frame',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/zip' },
      { name: 'content-disposition', value: 'attachment; filename="app.zip"' },
      { name: 'content-length', value: '5000000' },
    ],
  })?.reason === 'attachment_disposition',
);

const indexSrc = readFileSync(join(root, 'src/background/index.ts'), 'utf8');
const registerIdx = indexSrc.indexOf('registerDownloadCaptureListeners(whenCaptureReady)');
const hydrateIdx = indexSrc.indexOf('const whenCaptureReady = (async () => {');
assert(
  'MV3 startup registers download listeners on the first turn after starting hydrate',
  hydrateIdx !== -1 && registerIdx !== -1 && hydrateIdx < registerIdx,
);
assert(
  'MV3 startup does not await hydrate before addListener',
  !/await hydrateCaptureSessions\(\);\s*await getCachedSettings\(\);\s*registerDownloadCaptureListeners/.test(indexSrc),
);

console.log(`\n${passed} passed, ${failed} failed`);
rmSync(tmp, { recursive: true, force: true });
process.exit(failed > 0 ? 1 : 0);
