/**
 * Lightweight regression checks for Firefox download capture heuristics.
 * Run: node ./scripts/test-firefox-capture.mjs
 */
import * as esbuild from 'esbuild';
import { createRequire } from 'module';
import { mkdtempSync, writeFileSync, rmSync } from 'fs';
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
const {
  firefoxWebRequestDownloadCandidate,
  firefoxBeforeRequestDownloadCandidate,
  abortFirefoxResponseBody,
  shouldCaptureDownloadItem,
  shouldWaitForDownloadSize,
  downloadCreatedAction,
  knownDownloadBytes,
  matchesInterceptedDownload,
  canonicalDownloadFilename,
  normalizeCaptureUrl,
  shouldPauseDownloadItem,
  urlIsClaimed,
  MIN_CAPTURE_BYTES,
  MIN_XHR_CAPTURE_BYTES,
} = require(outfile);

const defaultSettings = {
  enabled: true,
  downloadHandoffMode: 'ask',
  contextMenuEnabled: true,
  showProgressAfterHandoff: true,
  showBadgeStatus: true,
  excludedHosts: ['web.telegram.org'],
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

console.log(`\n${passed} passed, ${failed} failed`);
rmSync(tmp, { recursive: true, force: true });
process.exit(failed > 0 ? 1 : 0);
