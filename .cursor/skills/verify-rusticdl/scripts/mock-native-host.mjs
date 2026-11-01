#!/usr/bin/env node
/**
 * Length-prefixed native-messaging mock for the extension capture harness.
 * Speaks the same SuccessResponse shape as packages/protocol (pong / accepted).
 * Logs every fully-read request to $RUN_DIR/host-log.jsonl with receivedAt (epoch ms)
 * and the reply actually written back (proof of accepted vs missing/wrong shape).
 *
 * EXT_ATTACK loads apps/extension/e2e/ext-attack.mjs (test-only; never product code).
 */
import { appendFileSync, mkdirSync, writeSync, readSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const RUN_DIR = process.env.RUN_DIR || process.env.VERIFY_RUN_DIR;
if (!RUN_DIR) {
  process.stderr.write('mock-native-host: RUN_DIR or VERIFY_RUN_DIR is required\n');
  process.exit(1);
}

const logPath = `${RUN_DIR}/host-log.jsonl`;
mkdirSync(dirname(logPath), { recursive: true });
mkdirSync(RUN_DIR, { recursive: true });

const STDIN_FD = 0;
const STDOUT_FD = 1;
const MAX_MESSAGE_BYTES = 1024 * 1024;

const here = dirname(fileURLToPath(import.meta.url));
const attackModulePath = join(here, '../../../../apps/extension/e2e/ext-attack.mjs');
const applyHostAttack = (process.env.EXT_ATTACK || process.env.CHECKER_ATTACK)
  ? (await import(pathToFileURL(attackModulePath).href)).applyHostAttack
  : null;

function readExact(fd, size) {
  const buf = Buffer.alloc(size);
  let offset = 0;
  while (offset < size) {
    const n = readSync(fd, buf, offset, size - offset, null);
    if (n === 0) {
      const err = new Error('EOF');
      err.code = 'EOF';
      throw err;
    }
    offset += n;
  }
  return buf;
}

function writeMessage(obj) {
  const json = Buffer.from(JSON.stringify(obj), 'utf8');
  const header = Buffer.alloc(4);
  header.writeUInt32LE(json.length, 0);
  writeSync(STDOUT_FD, header);
  writeSync(STDOUT_FD, json);
}

function pongPayload() {
  return {
    appState: 'running',
    appVersion: 'mock-native-host',
    connectionState: 'connected',
    queueSummary: {
      total: 0,
      active: 0,
      attention: 0,
      queued: 0,
      downloading: 0,
      completed: 0,
      failed: 0,
    },
  };
}

function respond(request) {
  const requestId = typeof request.requestId === 'string' ? request.requestId : 'unknown';
  const type = request.type;
  if (type === 'ping' || type === 'get_status' || type === 'open_app' || type === 'save_extension_settings') {
    return {
      ok: true,
      requestId,
      type: 'pong',
      payload: pongPayload(),
    };
  }
  if (type === 'prompt_download' || type === 'enqueue_download') {
    const suggested = request.payload && typeof request.payload === 'object'
      ? request.payload.suggestedFilename
      : undefined;
    return {
      ok: true,
      requestId,
      type: 'accepted',
      payload: {
        status: 'queued',
        appState: 'running',
        jobId: `mock-${requestId}`,
        ...(typeof suggested === 'string' && suggested ? { filename: suggested } : {}),
      },
    };
  }
  return {
    ok: false,
    requestId,
    type: 'rejected',
    code: 'INVALID_PAYLOAD',
    message: `Unsupported request type: ${String(type)}`,
  };
}

function syncDelay(ms) {
  const shared = new SharedArrayBuffer(4);
  const view = new Int32Array(shared);
  Atomics.wait(view, 0, 0, ms);
}

const replyDelayMs = Number(process.env.EXT_HOST_REPLY_DELAY_MS || '0');

function logRequest(request, receivedAt, reply) {
  const line = {
    receivedAt,
    protocolVersion: request.protocolVersion,
    requestId: request.requestId,
    type: request.type,
    payload: request.payload ?? {},
    ...(reply !== undefined ? { reply } : {}),
  };
  appendFileSync(logPath, `${JSON.stringify(line)}\n`, 'utf8');
}

process.stderr.write(`mock-native-host: start log=${logPath} attack=${process.env.EXT_ATTACK || process.env.CHECKER_ATTACK || ''}\n`);

if ((process.env.EXT_ATTACK || process.env.CHECKER_ATTACK) === 'kill-immediate') {
  process.exit(0);
}

try {
  while (true) {
    const lengthBuf = readExact(STDIN_FD, 4);
    const length = lengthBuf.readUInt32LE(0);
    if (length > MAX_MESSAGE_BYTES) {
      throw new Error(`native message too large: ${length}`);
    }
    const payloadBuf = readExact(STDIN_FD, length);
    const receivedAt = Date.now();
    let request;
    try {
      request = JSON.parse(payloadBuf.toString('utf8'));
    } catch (error) {
      writeMessage({
        ok: false,
        requestId: 'invalid_request',
        type: 'rejected',
        code: 'INVALID_PAYLOAD',
        message: `Invalid JSON: ${error instanceof Error ? error.message : String(error)}`,
      });
      continue;
    }
    if (applyHostAttack) {
      const handled = applyHostAttack({
        request,
        receivedAt,
        runDir: RUN_DIR,
        logRequest,
        writeMessage,
        respond,
      });
      if (handled) {
        process.stderr.write(
          `mock-native-host: attack=${process.env.EXT_ATTACK || process.env.CHECKER_ATTACK} type=${String(request.type)} requestId=${String(request.requestId)}\n`,
        );
        continue;
      }
    }
    const reply = respond(request);
    logRequest(request, receivedAt, reply);
    process.stderr.write(
      `mock-native-host: received type=${String(request.type)} requestId=${String(request.requestId)} at=${receivedAt}\n`,
    );
    if (
      (request.type === 'prompt_download' || request.type === 'enqueue_download')
      && Number.isFinite(replyDelayMs)
      && replyDelayMs > 0
    ) {
      syncDelay(replyDelayMs);
    }
    writeMessage(reply);
  }
} catch (error) {
  if (error && error.code === 'EOF') {
    process.exit(0);
  }
  process.stderr.write(`mock-native-host: fatal ${error instanceof Error ? error.stack : String(error)}\n`);
  process.exit(1);
}
