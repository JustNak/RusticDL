/**
 * Security regressions for download capture + handoff auth.
 * Run: node ./scripts/test-capture-security.mjs
 */
import * as esbuild from 'esbuild';
import { createRequire } from 'module';
import { mkdtempSync, writeFileSync, rmSync } from 'fs';
import { join } from 'path';
import { tmpdir } from 'os';
import { fileURLToPath } from 'url';

const root = fileURLToPath(new URL('..', import.meta.url));
const tmp = mkdtempSync(join(tmpdir(), 'rusticdl-security-'));
const browserStub = join(tmp, 'browser-stub.js');
// Tests swap globalThis.__browser per case.
writeFileSync(
  browserStub,
  'const proxy = new Proxy({}, { get: (_t, key) => globalThis.__browser?.[key] });\n'
    + 'module.exports = proxy;\nmodule.exports.default = proxy;\n',
);

const require = createRequire(import.meta.url);
async function load(entry) {
  const outfile = join(tmp, `${entry.replace(/[\\/]/g, '_')}.cjs`);
  await esbuild.build({
    entryPoints: [join(root, entry)],
    bundle: true,
    platform: 'node',
    format: 'cjs',
    outfile,
    logLevel: 'silent',
    plugins: [{
      name: 'stub-browser',
      setup(build) {
        build.onResolve({ filter: /^\.\/browser$/ }, () => ({ path: browserStub }));
      },
    }],
  });
  return require(outfile);
}

const filter = await load('src/background/captureFilter.ts');
const chromium = await load('src/background/chromiumCapture.ts');
const firefox = await load('src/background/firefoxCapture.ts');
const guard = await load('src/background/messageGuard.ts');
const native = await load('src/background/nativeMessaging.ts');
const coordinator = await load('src/background/captureCoordinator.ts');

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

function setUserAgent(ua) {
  Object.defineProperty(globalThis, 'navigator', {
    value: { userAgent: ua, platform: 'Linux x86_64' },
    configurable: true,
    writable: true,
  });
}
const CHROME_UA = 'Mozilla/5.0 (X11; Linux x86_64) Chrome/140.0 Safari/537.36';
const FIREFOX_UA = 'Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0';

console.log('SameSite scoping\n');

assert('same host is same-site', filter.isSameSiteForCookies('https://a.com/x', 'https://a.com/y'));
assert('subdomain is same-site', filter.isSameSiteForCookies('https://dl.a.com/x', 'https://a.com/'));
assert('scheme mismatch is cross-site', !filter.isSameSiteForCookies('https://a.com/', 'http://a.com/'));
assert('other site is cross-site', !filter.isSameSiteForCookies('https://bank.com/', 'https://evil.com/'));
assert('suffix trick is cross-site', !filter.isSameSiteForCookies('https://bank.com/', 'https://evilbank.com/'));
assert('missing initiator is cross-site', !filter.isSameSiteForCookies('https://bank.com/', undefined));
assert('IP literals need exact match', !filter.isSameSiteForCookies('http://1.2.3.4/', 'http://2.3.4/'));

const nav = { sameSite: false, topLevelNavigation: true, unspecifiedIsLax: true };
const sub = { sameSite: false, topLevelNavigation: false, unspecifiedIsLax: true };
assert('cross-site nav drops Strict', !filter.cookieAllowedForHandoff({ name: 's', value: '1', sameSite: 'strict' }, nav));
assert('cross-site nav keeps Lax', filter.cookieAllowedForHandoff({ name: 'l', value: '1', sameSite: 'lax' }, nav));
assert('cross-site subresource drops Lax', !filter.cookieAllowedForHandoff({ name: 'l', value: '1', sameSite: 'lax' }, sub));
assert('cross-site subresource drops Chromium-unspecified', !filter.cookieAllowedForHandoff({ name: 'u', value: '1', sameSite: 'unspecified' }, sub));
assert('cross-site subresource keeps None', filter.cookieAllowedForHandoff({ name: 'n', value: '1', sameSite: 'no_restriction' }, sub));
assert('same-site keeps Strict', filter.cookieAllowedForHandoff({ name: 's', value: '1', sameSite: 'strict' }, { ...sub, sameSite: true }));

console.log('\nReferer trimming\n');
assert('same-origin keeps full referrer', filter.handoffReferrer('https://a.com/f.zip', 'https://a.com/page?q=1#h') === 'https://a.com/page?q=1');
assert('cross-origin trims to origin', filter.handoffReferrer('https://cdn.com/f.zip', 'https://a.com/secret?token=abc') === 'https://a.com/');
assert('https→http downgrade drops referrer', filter.handoffReferrer('http://a.com/f.zip', 'https://a.com/page') === undefined);
assert('non-http referrer dropped', filter.handoffReferrer('https://a.com/f.zip', 'file:///etc/passwd') === undefined);
assert('credentials stripped', filter.handoffReferrer('https://a.com/f', 'https://u:p@a.com/p') === 'https://a.com/p');

console.log('\ncollectHandoffAuth cookie store isolation\n');

function cookieBrowser({ stores, jar, calls }) {
  return {
    cookies: {
      async getAllCookieStores() { return stores; },
      async getAll(query) {
        calls.push(query);
        const storeId = query.storeId ?? 'default';
        const host = new URL(query.url).hostname;
        return (jar[storeId]?.[host] ?? []).map((c) => ({ ...c }));
      },
    },
  };
}

setUserAgent(FIREFOX_UA);
{
  const calls = [];
  globalThis.__browser = cookieBrowser({
    stores: [{ id: 'default' }, { id: 'firefox-container-1' }],
    jar: { default: { 'site.com': [{ name: 'main', value: 'SECRET', sameSite: 'lax' }] } },
    calls,
  });
  const auth = await native.collectHandoffAuth('https://site.com/f.zip', {
    cookieStoreId: 'firefox-container-1',
    referrer: 'https://site.com/',
  });
  const json = JSON.stringify(auth ?? {});
  assert('container download never reads default store', calls.every((q) => q.storeId === 'firefox-container-1'));
  assert('container download carries no default-store cookie', !json.includes('SECRET'));
}

setUserAgent(CHROME_UA);
{
  const calls = [];
  globalThis.__browser = cookieBrowser({
    // Chromium store list with no incognito flag exposed
    stores: [{ id: '0' }, { id: '1' }],
    jar: { default: { 'site.com': [{ name: 'main', value: 'SECRET', sameSite: 'lax' }] } },
    calls,
  });
  const auth = await native.collectHandoffAuth('https://site.com/f.zip', {
    incognito: true,
    referrer: 'https://site.com/',
  });
  assert('incognito with unknown store skips cookies entirely', calls.length === 0);
  assert('incognito download has no normal-profile cookie', !JSON.stringify(auth ?? {}).includes('SECRET'));
}

{
  const calls = [];
  globalThis.__browser = cookieBrowser({
    stores: [{ id: '0' }],
    jar: {
      default: {
        'bank.com': [
          { name: 'strict_sess', value: 'S1', sameSite: 'strict' },
          { name: 'lax_sess', value: 'L1', sameSite: 'lax' },
          { name: 'none_c', value: 'N1', sameSite: 'no_restriction' },
        ],
      },
    },
    calls,
  });
  const navAuth = await native.collectHandoffAuth('https://bank.com/export.zip', {
    referrer: 'https://evil.com/',
  });
  const navCookie = navAuth.headers.find((h) => h.name === 'Cookie')?.value ?? '';
  assert('cross-site nav: no Strict cookie', !navCookie.includes('S1'));
  assert('cross-site nav: Lax + None kept', navCookie.includes('L1') && navCookie.includes('N1'));

  const subAuth = await native.collectHandoffAuth('https://bank.com/export.zip', {
    referrer: 'https://evil.com/',
    topLevelNavigation: false,
  });
  const subCookie = subAuth.headers.find((h) => h.name === 'Cookie')?.value ?? '';
  assert('cross-site subresource: only None', subCookie === 'none_c=N1');

  const sameAuth = await native.collectHandoffAuth('https://bank.com/export.zip', {
    referrer: 'https://bank.com/account',
  });
  const sameCookie = sameAuth.headers.find((h) => h.name === 'Cookie')?.value ?? '';
  assert('same-site: all cookies', sameCookie.includes('S1') && sameCookie.includes('L1'));
  assert(
    'cross-site referer trimmed to origin',
    navAuth.headers.find((h) => h.name === 'Referer')?.value === 'https://evil.com/',
  );
}

console.log('\nChromium captured request auth\n');

chromium.resetRequestAuthForTests();
const tabProfiles = new Map([[1, false], [2, true]]);
chromium.setTabIncognitoResolverForTests(async (tabId) => tabProfiles.get(tabId));

chromium.rememberRequestAuth({
  url: 'https://api.site.com/v1/me',
  type: 'xmlhttprequest',
  tabId: 1,
  requestHeaders: [{ name: 'Authorization', value: 'Bearer page-api-token' }],
});
assert(
  'XHR Authorization is never recorded',
  chromium.lookupOriginAuth(['https://api.site.com/file.zip']).length === 0,
);

chromium.rememberRequestAuth({
  url: 'https://private.site.com/file.zip',
  type: 'main_frame',
  tabId: 2,
  requestHeaders: [{ name: 'Cookie', value: 'private=1' }],
});
await new Promise((resolve) => setTimeout(resolve, 0));
assert(
  'incognito tab auth not used for normal download',
  chromium.lookupOriginAuth(['https://private.site.com/file.zip'], { incognito: false }).length === 0,
);
assert(
  'incognito tab auth used for incognito download',
  chromium.lookupOriginAuth(['https://private.site.com/file.zip'], { incognito: true }).length === 1,
);

chromium.rememberRequestAuth({
  url: 'https://normal.site.com/file.zip',
  type: 'main_frame',
  tabId: 1,
  requestHeaders: [{ name: 'Cookie', value: 'normal=1' }],
});
await new Promise((resolve) => setTimeout(resolve, 0));
assert(
  'normal tab auth not used for incognito download',
  chromium.lookupOriginAuth(['https://normal.site.com/file.zip'], { incognito: true }).length === 0,
);
chromium.rememberRequestAuth({
  url: 'https://unknown.site.com/file.zip',
  type: 'other',
  tabId: -1,
  requestHeaders: [{ name: 'Cookie', value: 'u=1' }],
});
assert(
  'unknown-profile auth never serves incognito',
  chromium.lookupOriginAuth(['https://unknown.site.com/file.zip'], { incognito: true }).length === 0,
);

console.log('\nRestore stays in the original profile\n');

assert(
  'Chromium incognito restore is skipped',
  coordinator.restoreDownloadOptions({ url: 'https://a.com/f.zip', incognito: true }, false) === null,
);
const ffRestore = coordinator.restoreDownloadOptions(
  { url: 'https://a.com/f.zip', incognito: true, cookieStoreId: 'firefox-private', filename: 'f.zip' },
  true,
);
assert(
  'Firefox restore keeps private + container',
  ffRestore?.incognito === true && ffRestore?.cookieStoreId === 'firefox-private' && ffRestore?.filename === 'f.zip',
);
assert(
  'restore drops path-like filename',
  coordinator.restoreDownloadOptions({ url: 'https://a.com/f', filename: 'C:evil' }, false)?.filename === undefined,
);

console.log('\nFirefox candidate navigation flag\n');
const settings = {
  enabled: true,
  downloadHandoffMode: 'ask',
  excludedHosts: [],
  ignoredFileExtensions: [],
  capturedFileExtensions: ['zip'],
};
const objectCandidate = firefox.firefoxWebRequestDownloadCandidate({
  url: 'https://bank.com/export.zip',
  type: 'object',
  method: 'GET',
  statusCode: 200,
  originUrl: 'https://evil.com/',
  documentUrl: 'https://evil.com/',
  responseHeaders: [
    { name: 'content-type', value: 'application/zip' },
    { name: 'content-length', value: '200000' },
  ],
}, settings);
assert('object embed is not a top-level navigation', objectCandidate?.topLevelNavigation === false);
const navCandidate = firefox.firefoxWebRequestDownloadCandidate({
  url: 'https://site.com/file.zip',
  type: 'main_frame',
  method: 'GET',
  statusCode: 200,
  responseHeaders: [
    { name: 'content-type', value: 'application/zip' },
    { name: 'content-length', value: '200000' },
  ],
}, settings);
assert('main_frame is a top-level navigation', navCandidate?.topLevelNavigation === true);

console.log('\nBackground message sender guard\n');
const id = 'abcdefghijklmnop';
const base = `chrome-extension://${id}/`;
assert('own popup accepted', guard.isTrustedPopupSender({ id, url: `${base}popup.html` }, id, base));
assert('other extension rejected', !guard.isTrustedPopupSender({ id: 'zzz', url: 'chrome-extension://zzz/x.html' }, id, base));
assert('web page rejected', !guard.isTrustedPopupSender({ id, url: 'https://evil.com/' }, id, base));
assert('missing sender rejected', !guard.isTrustedPopupSender(undefined, id, base));
assert('unknown message type rejected', !guard.isPopupRequest({ type: 'enqueue_anything' }));
assert('settings update without settings rejected', !guard.isPopupRequest({ type: 'extension_settings_update' }));
assert('valid ping accepted', guard.isPopupRequest({ type: 'popup_ping' }));

rmSync(tmp, { recursive: true, force: true });
console.log(`\n${passed} passed, ${failed} failed`);
if (failed > 0) process.exit(1);
