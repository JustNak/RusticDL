#!/usr/bin/env node
/**
 * Local fixture server for the extension capture matrix.
 * Listens on 127.0.0.1 and localhost (cross-origin pair). Node built-ins only.
 */
import { createServer } from 'node:http';
import { appendFileSync, mkdirSync, writeFileSync } from 'node:fs';

const PORT = Number(process.env.EXT_FIXTURE_PORT || 18765);
const PIDFILE = process.env.EXT_FIXTURE_PIDFILE || '';
const RUN_DIR = process.env.RUN_DIR || process.env.VERIFY_RUN_DIR || '';
const BODY_64K = 64 * 1024;
const BODY_3K = 3 * 1024;
const BODY_200M = 200 * 1024 * 1024;
const COOKIE_NAME = 'rusticdl_e2e';
const HIT_LOG = RUN_DIR ? `${RUN_DIR}/fixture-requests.jsonl` : '';

const attackMod = process.env.EXT_ATTACK
  ? await import(new URL('../../../../apps/extension/e2e/ext-attack.mjs', import.meta.url).href)
  : null;

function bytes(size, fill = 0x41) {
  return Buffer.alloc(size, fill);
}

function nonceOf(url) {
  return url.searchParams.get('nonce') || '';
}

function withNonce(path, url) {
  const nonce = nonceOf(url);
  const next = new URL(path, `http://${url.host}`);
  if (nonce) next.searchParams.set('nonce', nonce);
  for (const [key, value] of url.searchParams.entries()) {
    if (key !== 'nonce' && !next.searchParams.has(key)) next.searchParams.set(key, value);
  }
  return `${next.pathname}${next.search}`;
}

function cookieValue(nonce) {
  if (attackMod?.cookieValueForNonce) return attackMod.cookieValueForNonce(nonce);
  return nonce;
}

function cookieHeader(nonce) {
  const value = cookieValue(nonce);
  return `${COOKIE_NAME}=${value || 'missing'}; Path=/`;
}

function logHit(req, url) {
  if (!HIT_LOG) return;
  mkdirSync(dirnameHit(HIT_LOG), { recursive: true });
  const record = {
    at: Date.now(),
    method: req.method || 'GET',
    href: url.href,
    pathname: url.pathname,
    search: url.search,
    nonce: url.searchParams.get('nonce') || '',
    attempt: url.searchParams.get('attempt') || '',
    host: req.headers.host || '',
  };
  appendFileSync(HIT_LOG, `${JSON.stringify(record)}\n`);
}

function dirnameHit(filePath) {
  const idx = filePath.lastIndexOf('/');
  return idx === -1 ? '.' : filePath.slice(0, idx);
}

function send(res, status, headers, body) {
  res.writeHead(status, headers);
  res.end(body);
}

function htmlPage(id, inner, nonce = '') {
  const cookieScript = nonce
    ? `<script>document.cookie = "${COOKIE_NAME}=${cookieValue(nonce)}; path=/";</script>`
    : '';
  return `<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>${id}</title></head>
<body>
<main>
<h1 data-page="${id}">${id}</h1>
${inner}
</main>
${cookieScript}
<script>
document.querySelectorAll("[data-case]").forEach((el) => {
  el.addEventListener("pointerdown", () => {
    window.__extTriggerAt = Date.now();
    el.setAttribute("data-trigger-at", String(window.__extTriggerAt));
  }, true);
});
</script>
</body>
</html>`;
}

function pageFor(id, url) {
  const file = (route) => withNonce(route, url);
  const wrap = (inner) => htmlPage(id, inner, nonceOf(url));
  switch (id) {
    case 'C1':
      return wrap(`<a data-case="C1" download="app.zip" href="${file('/C1')}">C1 download attr</a>`);
    case 'C2':
      return wrap(`<a data-case="C2" href="${file('/C2')}">C2 attachment exe</a>`);
    case 'C3':
      return wrap(`<a data-case="C3" href="${file('/C3/file.zip')}">C3 inline zip</a>`);
    case 'C4':
      return wrap(`<a data-case="C4" href="${file('/get?id=5')}">C4 get id=5</a>`);
    case 'C5':
      if (attackMod?.c5PageIsNoop?.(RUN_DIR)) {
        return wrap(`<a data-case="C5" href="#" onclick="return false">C5 blob</a>`);
      }
      return wrap(`<a data-case="C5" id="c5-link" download="blob.zip">C5 blob</a>
<script>
const blob = new Blob([new Uint8Array(${BODY_64K}).fill(65)], { type: "application/zip" });
document.getElementById("c5-link").href = URL.createObjectURL(blob);
</script>`);
    case 'C6': {
      if (attackMod?.c6IsNoop?.()) {
        return wrap(`<a data-case="C6" href="#" onclick="return false">C6 data</a>`);
      }
      const dataUrl = `data:application/zip;base64,${bytes(64, 0x41).toString('base64')}`;
      return wrap(`<a data-case="C6" download="data.zip" href="${dataUrl}">C6 data</a>`);
    }
    case 'C7':
      return wrap(`<a data-case="C7" href="${file('/C7/file.zip')}">C7 location</a>
<script>
document.querySelector("[data-case=C7]").addEventListener("click", (event) => {
  event.preventDefault();
  location.href = event.currentTarget.href;
});
</script>`);
    case 'C8':
      return wrap(`<button type="button" data-case="C8" data-file-url="${file('/C8')}">C8 programmatic click</button>
<script>
document.querySelector("[data-case=C8]").addEventListener("click", () => {
  const a = document.createElement("a");
  a.download = "app.zip";
  a.href = "${file('/C8')}";
  document.body.appendChild(a);
  a.click();
});
</script>`);
    case 'C9':
      return wrap(`<form data-case="C9-form" method="POST" action="${file('/C9')}">
<input type="hidden" name="body" value="post-body-c9">
<button type="submit" data-case="C9">C9 post zip</button>
</form>`);
    case 'C10':
      return wrap(`<a data-case="C10" href="${file('/C10')}">C10 redirects</a>`);
    case 'C11':
      return wrap(`<a data-case="C11" target="_blank" rel="noopener" href="${file('/C11')}">C11 blank</a>`);
    case 'C12': {
      const localhostUrl = new URL(file('/C12'), `http://localhost:${url.port}`);
      if (nonceOf(url)) localhostUrl.searchParams.set('nonce', nonceOf(url));
      return wrap(`<a data-case="C12" href="${localhostUrl.href}">C12 cross-origin</a>`);
    }
    case 'N1':
      if (attackMod?.n1IsNoop?.()) {
        return wrap(`<a data-case="N1" href="#" onclick="return false">N1 html page</a>`);
      }
      if (attackMod?.n1IsFetch?.()) {
        return wrap(`<a data-case="N1" href="${file('/N1')}">N1 html page</a>
<script>
document.querySelector("[data-case=N1]").addEventListener("click", (event) => {
  event.preventDefault();
  fetch(event.currentTarget.href);
});
</script>`);
      }
      if (attackMod?.n1IsFakeTarget?.()) {
        return wrap(`<a data-case="N1" href="${file('/N1')}">N1 html page</a>
<script>
document.querySelector("[data-case=N1]").addEventListener("click", (event) => {
  event.preventDefault();
  fetch(event.currentTarget.href);
  const root = document.querySelector("[data-page]");
  if (root) root.setAttribute("data-page", "N1-target");
});
</script>`);
      }
      return wrap(`<a data-case="N1" href="${file('/N1')}">N1 html page</a>`);
    case 'N2':
      return wrap(`<a data-case="N2" href="${file('/N2')}">N2 inline pdf</a>`);
    case 'N3':
      return wrap(`<a data-case="N3" href="${file('/N3')}">N3 image</a>`);
    case 'N4':
      return wrap(`<a data-case="N4" href="${file('/files/app.zip')}">N4 html as zip path</a>`);
    case 'N5': {
      const n5Href = file('/N5');
      const n5Mode = attackMod?.n5Mode?.() || 'honest';
      if (n5Mode === 'noop') {
        return wrap(`<button type="button" data-case="N5">N5 fetch</button>`);
      }
      if (n5Mode === 'stay') {
        return wrap(`<button type="button" data-case="N5" data-file-url="${n5Href}">N5 fetch</button>
<script>
document.querySelector("[data-case=N5]").addEventListener("click", () => {
  fetch("${n5Href}");
});
</script>`);
      }
      if (n5Mode === 'stale') {
        return wrap(`<button type="button" data-case="N5" data-file-url="${n5Href}">N5 fetch</button>
<script>
document.querySelector("[data-case=N5]").addEventListener("click", async () => {
  const res = await fetch("${n5Href}");
  const buf = await res.arrayBuffer();
  const root = document.querySelector("[data-page]");
  if (!root) return;
  root.setAttribute("data-page", "N5-target");
  root.setAttribute("data-fetch-status", String(res.status));
  root.setAttribute("data-fetch-bytes", String(buf.byteLength));
  root.setAttribute("data-fetch-nonce", "stale-nonce");
  root.setAttribute("data-fetch-attempt", "other-attempt");
  root.textContent = "N5-target status=" + res.status + " bytes=" + buf.byteLength + " nonce=stale-nonce attempt=other-attempt";
});
</script>`);
      }
      return wrap(`<button type="button" data-case="N5" data-file-url="${n5Href}">N5 fetch</button>
<script>
document.querySelector("[data-case=N5]").addEventListener("click", async () => {
  const url = "${n5Href}";
  const res = await fetch(url);
  const buf = await res.arrayBuffer();
  const parsed = new URL(url, location.href);
  const root = document.querySelector("[data-page]");
  if (!root) return;
  root.setAttribute("data-page", "N5-target");
  root.setAttribute("data-fetch-status", String(res.status));
  root.setAttribute("data-fetch-bytes", String(buf.byteLength));
  root.setAttribute("data-fetch-nonce", parsed.searchParams.get("nonce") || "");
  root.setAttribute("data-fetch-attempt", parsed.searchParams.get("attempt") || "");
  root.textContent = "N5-target status=" + res.status + " bytes=" + buf.byteLength
    + " nonce=" + (parsed.searchParams.get("nonce") || "")
    + " attempt=" + (parsed.searchParams.get("attempt") || "");
});
</script>`);
    }
    case 'N6':
      return wrap(`<a data-case="N6" href="${file('/N6')}">N6 3kb rar</a>`);
    case 'L1':
      return wrap(`<a data-case="L1" href="${file('/L1')}">L1 chunked 200MB</a>`);
    case 'L2':
      return wrap(`<a data-case="L2" href="${file('/L2')}">L2 skip</a>`);
    case 'L3':
      return wrap(`<a data-case="L3" href="${file('/L3')}">L3 skip</a>`);
    default:
      return wrap(`<p>unknown case</p>`);
  }
}

function pngStub(size) {
  const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  const rest = Buffer.alloc(Math.max(0, size - signature.length), 0);
  return Buffer.concat([signature, rest]);
}

function pdfStub(size) {
  const head = Buffer.from('%PDF-1.1\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n');
  const rest = Buffer.alloc(Math.max(0, size - head.length), 0x20);
  return Buffer.concat([head, rest]);
}

function handleFile(id, req, res, url) {
  const nonce = nonceOf(url);
  const common = {
    'Cache-Control': 'no-store',
    'Set-Cookie': cookieHeader(nonce),
  };
  switch (id) {
    case 'C1':
      return send(res, 200, {
        ...common,
        'Content-Type': 'application/zip',
        'Content-Length': String(BODY_64K),
      }, bytes(BODY_64K));
    case 'C2':
      return send(res, 200, {
        ...common,
        'Content-Type': 'application/octet-stream',
        'Content-Disposition': 'attachment; filename="setup.exe"',
        'Content-Length': String(BODY_64K),
      }, bytes(BODY_64K, 0x42));
    case 'C3':
      return send(res, 200, {
        ...common,
        'Content-Type': 'application/zip',
        'Content-Disposition': 'inline',
        'Content-Length': String(BODY_64K),
      }, bytes(BODY_64K));
    case 'C4':
      return send(res, 200, {
        ...common,
        'Content-Type': 'application/octet-stream',
        'Content-Length': String(BODY_64K),
      }, bytes(BODY_64K, 0x43));
    case 'C7':
      return send(res, 200, {
        ...common,
        'Content-Type': 'application/zip',
        'Content-Length': String(BODY_64K),
      }, bytes(BODY_64K));
    case 'C8':
      return send(res, 200, {
        ...common,
        'Content-Type': 'application/zip',
        'Content-Length': String(BODY_64K),
      }, bytes(BODY_64K));
    case 'C9':
      return send(res, 200, {
        ...common,
        'Content-Type': 'application/zip',
        'Content-Disposition': 'attachment; filename="posted.zip"',
        'Content-Length': String(BODY_64K),
      }, bytes(BODY_64K));
    case 'C11':
      return send(res, 200, {
        ...common,
        'Content-Type': 'application/zip',
        'Content-Disposition': 'attachment; filename="blank.zip"',
        'Content-Length': String(BODY_64K),
      }, bytes(BODY_64K));
    case 'C12':
      return send(res, 200, {
        ...common,
        'Content-Type': 'application/zip',
        'Content-Disposition': 'attachment; filename="cross.zip"',
        'Content-Length': String(BODY_64K),
      }, bytes(BODY_64K));
    case 'N1':
      return send(res, 200, {
        ...common,
        'Content-Type': 'text/html; charset=utf-8',
      }, htmlPage('N1-target', '<p>html document, not a file</p>'));
    case 'N2':
      return send(res, 200, {
        ...common,
        'Content-Type': 'application/pdf',
        'Content-Disposition': 'inline; filename="inline.pdf"',
        'Content-Length': String(BODY_64K),
      }, pdfStub(BODY_64K));
    case 'N3':
      return send(res, 200, {
        ...common,
        'Content-Type': 'image/png',
        'Content-Length': String(BODY_64K),
      }, pngStub(BODY_64K));
    case 'N5':
      return send(res, 200, {
        ...common,
        'Content-Type': 'application/zip',
        'Content-Disposition': 'attachment; filename="fetched.zip"',
        'Content-Length': String(BODY_64K),
      }, bytes(BODY_64K));
    case 'N6':
      return send(res, 200, {
        ...common,
        'Content-Type': 'application/x-rar-compressed',
        'Content-Disposition': 'attachment; filename="stub.rar"',
        'Content-Length': String(BODY_3K),
      }, bytes(BODY_3K, 0x52));
    case 'L2':
    case 'L3':
      return send(res, 200, {
        ...common,
        'Content-Type': 'application/octet-stream',
        'Content-Disposition': `attachment; filename="${id === 'L2' ? 'cancel.bin' : 'dismiss.bin'}"`,
        'Content-Length': String(BODY_64K),
      }, bytes(BODY_64K));
    default:
      send(res, 404, { 'Content-Type': 'text/plain' }, 'not found');
  }
}

function streamChunked(res, extraHeaders, totalBytes) {
  res.writeHead(200, {
    'Content-Type': 'application/octet-stream',
    'Content-Disposition': 'attachment; filename="huge.bin"',
    'Transfer-Encoding': 'chunked',
    ...extraHeaders,
  });
  const chunk = bytes(64 * 1024, 0x48);
  let sent = 0;
  const writeMore = () => {
    while (sent < totalBytes) {
      const remain = totalBytes - sent;
      const slice = remain >= chunk.length ? chunk : chunk.subarray(0, remain);
      const ok = res.write(slice);
      sent += slice.length;
      if (!ok) {
        res.once('drain', writeMore);
        return;
      }
    }
    res.end();
  };
  reqOnClose(res, writeMore);
  writeMore();
}

function reqOnClose(res, _writeMore) {
  res.on('close', () => undefined);
  res.on('error', () => undefined);
}

function onRequest(req, res) {
  const host = req.headers.host || `127.0.0.1:${PORT}`;
  const url = new URL(req.url || '/', `http://${host}`);
  const nonce = nonceOf(url);
  logHit(req, url);
  if (RUN_DIR && attackMod) {
    if (url.pathname === '/C1' && attackMod.scheduleLateC1Inject) {
      attackMod.scheduleLateC1Inject(RUN_DIR, url.href);
    }
    if (url.pathname === '/get' && attackMod.scheduleLateC4Inject) {
      attackMod.scheduleLateC4Inject(RUN_DIR, url.href);
    }
  }

  if (url.pathname === '/health') {
    send(res, 200, { 'Content-Type': 'application/json' }, JSON.stringify({ ok: true, pid: process.pid, port: PORT }));
    return;
  }

  if (nonce) {
    res.setHeader('Set-Cookie', cookieHeader(nonce));
  }

  if (url.pathname.startsWith('/page/')) {
    const id = url.pathname.slice('/page/'.length);
    send(res, 200, {
      'Content-Type': 'text/html; charset=utf-8',
      'Set-Cookie': cookieHeader(nonce),
      'Cache-Control': 'no-store',
    }, pageFor(id, url));
    return;
  }

  if (url.pathname === '/C10') {
    res.writeHead(302, { Location: withNonce('/C10/hop2', url), 'Set-Cookie': cookieHeader(nonce) });
    res.end();
    return;
  }
  if (url.pathname === '/C10/hop2') {
    res.writeHead(302, { Location: withNonce('/C10/final', url), 'Set-Cookie': cookieHeader(nonce) });
    res.end();
    return;
  }
  if (url.pathname === '/C10/final') {
    send(res, 200, {
      'Content-Type': 'application/zip',
      'Content-Disposition': 'attachment; filename="archive.zip"',
      'Content-Length': String(BODY_64K),
      'Set-Cookie': cookieHeader(nonce),
    }, bytes(BODY_64K));
    return;
  }

  if (url.pathname === '/get') {
    handleFile('C4', req, res, url);
    return;
  }
  if (url.pathname === '/files/app.zip') {
    send(res, 200, {
      'Content-Type': 'text/html; charset=utf-8',
      'Set-Cookie': cookieHeader(nonce),
    }, htmlPage('N4-target', '<p>path looks like zip, served as html</p>'));
    return;
  }
  if (url.pathname === '/C3/file.zip') {
    handleFile('C3', req, res, url);
    return;
  }
  if (url.pathname === '/C7/file.zip') {
    handleFile('C7', req, res, url);
    return;
  }
  if (url.pathname === '/L1') {
    streamChunked(res, { 'Set-Cookie': cookieHeader(nonce) }, BODY_200M);
    return;
  }

  const id = url.pathname.slice(1);
  if (/^[CNL][0-9]+$/.test(id)) {
    handleFile(id, req, res, url);
    return;
  }

  send(res, 404, { 'Content-Type': 'text/plain' }, `no route ${url.pathname}`);
}

function listen(host) {
  return new Promise((resolve, reject) => {
    const server = createServer(onRequest);
    server.on('error', (error) => {
      if (error && error.code === 'EADDRINUSE' && host !== '127.0.0.1') {
        resolve({ host, skipped: true, error: error.message });
        return;
      }
      reject(error);
    });
    server.listen(PORT, host, () => {
      resolve({ host, server, skipped: false });
    });
  });
}

const servers = [];
const first = await listen('127.0.0.1');
servers.push(first);
for (const host of ['localhost', '::1']) {
  try {
    servers.push(await listen(host));
  } catch (error) {
    servers.push({ host, skipped: true, error: error instanceof Error ? error.message : String(error) });
  }
}

if (PIDFILE) {
  writeFileSync(PIDFILE, `${process.pid}\n`);
}

const bound = servers.filter((s) => !s.skipped).map((s) => s.host);
console.log(`READY port=${PORT} pid=${process.pid} listen=${bound.join(',')}`);

function shutdown() {
  for (const entry of servers) {
    if (entry.server) entry.server.close();
  }
  process.exit(0);
}
process.on('SIGTERM', shutdown);
process.on('SIGINT', shutdown);
