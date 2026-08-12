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
  MIN_CAPTURE_BYTES,
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

// --- User's YouTube false positive ---
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
  'rejects xhr entirely (even real-looking zip)',
  candidate({
    url: 'https://cdn.example.com/files/app.zip',
    type: 'xmlhttprequest',
    statusCode: 200,
    responseHeaders: [
      { name: 'content-type', value: 'application/zip' },
      { name: 'content-disposition', value: 'attachment; filename="app.zip"' },
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

console.log(`\n${passed} passed, ${failed} failed`);
rmSync(tmp, { recursive: true, force: true });
process.exit(failed > 0 ? 1 : 0);
