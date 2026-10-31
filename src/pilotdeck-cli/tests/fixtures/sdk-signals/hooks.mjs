// #1145 Observation-only preload for the unmodified, real TS CLI entry.
import assert from 'node:assert/strict';
import { writeSync } from 'node:fs';
import fsPromises from 'node:fs/promises';
import { syncBuiltinESMExports } from 'node:module';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.equal(process.version, 'v22.22.0', '#1145 hook Node version');
const endpoint = 'ws://127.0.0.1:28999/ws';
const counts = () => ({ SIGINT: process.listenerCount('SIGINT'), SIGTERM: process.listenerCount('SIGTERM') });
function hook(kind, detail = {}) {
  const bytes = Buffer.from(JSON.stringify({ kind, ...detail, listeners: counts() }) + '\n');
  assert.ok(bytes.length < 4096, '#1145 bounded atomic hook record');
  assert.equal(writeSync(3, bytes), bytes.length, '#1145 complete hook write');
}
const NativeWebSocket = globalThis.WebSocket;
let connection = 0;
const identities = new WeakMap();
globalThis.WebSocket = class extends NativeWebSocket {
  constructor(address, ...rest) {
    assert.equal(String(address), endpoint, '#1145 deny non-owned endpoint including 18789');
    super(address, ...rest);
    identities.set(this, ++connection);
    hook('socket-created', { connection });
  }
  close(...args) {
    const result = super.close(...args);
    hook('socket-close-called', { connection: identities.get(this) });
    return result;
  }
};
// Verify the rejection without attempting a forbidden connection.
assert.throws(() => new globalThis.WebSocket('ws://127.0.0.1:18789/ws'),
  /#1145 deny non-owned endpoint including 18789/, '#1145 forbidden endpoint negative control');
hook('endpoint-denial-verified');

const originalOnce = process.once;
process.once = function (event, listener) {
  const result = originalOnce.call(this, event, listener);
  if (event === 'SIGINT' || event === 'SIGTERM') hook('listener-installed', { event });
  return result;
};
const originalRemove = process.removeListener;
process.removeListener = function (event, listener) {
  const result = originalRemove.call(this, event, listener);
  if (event === 'SIGINT' || event === 'SIGTERM') hook('listener-removed', { event });
  return result;
};
const originalRead = fsPromises.readFile;
fsPromises.readFile = function (path, ...args) {
  const result = originalRead.call(this, path, ...args);
  if (path === '/dev/stdin') hook('stdin-read-enter');
  return result;
};
syncBuiltinESMExports();

let pendingOutput = '';
const originalWrite = process.stdout.write;
process.stdout.write = function (chunk, ...args) {
  const result = originalWrite.call(this, chunk, ...args);
  pendingOutput += Buffer.isBuffer(chunk) ? chunk.toString('utf8') : chunk;
  let newline;
  while ((newline = pendingOutput.indexOf('\n')) >= 0) {
    const line = pendingOutput.slice(0, newline);
    pendingOutput = pendingOutput.slice(newline + 1);
    let value;
    try { value = JSON.parse(line); } catch { continue; }
    if (value?.type === 'result' || value?.type === 'error') {
      hook('terminal-consumed', { type: value.type });
    }
  }
  return result;
};

const { createQuery } = await import(pathToFileURL(join(process.env.PDRS_BASELINE, 'packages/sdk/dist/client.js')));
// Creating an unstarted query does not connect. Use the real prototype, never a
// copied implementation; abort's original promise is returned without a catch.
const prototype = Object.getPrototypeOf(createQuery('hook identity', { gatewayUrl: endpoint, authToken: 'd1-fixture-not-a-secret' }));
const originalAbort = prototype.abort;
prototype.abort = function (...args) {
  const result = originalAbort.apply(this, args);
  hook('abort-enter', { done: this.done, terminalSeen: this.terminalSeen, initialized: this.initialized });
  return result;
};
process.on('exit', code => hook('process-exit', { code }));
hook('hooks-ready');
