// Real SDK state transitions with only WebSocket IO replaced in-process.
import assert from 'node:assert/strict';
import { mock } from 'node:test';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { validateFiles } from '../sdk-signals/contract.mjs';

assert.equal(process.version, 'v22.22.0', '#1145 supplemental generator Node version');
assert.equal(process.argv.length, 3, '#1145 supplemental generator OUTPUT');
const here = dirname(fileURLToPath(import.meta.url));
const parent = resolve(here, '../sdk-signals');
const baseline = process.env.PDRS_BASELINE;
const sourceBytes = readFileSync(join(parent, 'source.json'));
const source = JSON.parse(sourceBytes);
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const generatorSha256 = sha(readFileSync(fileURLToPath(import.meta.url)));
validateFiles(source, parent, baseline);
const { createQueryWithTransport } = await import(pathToFileURL(join(baseline, 'packages/sdk/dist/client.js')));
const { GatewayTransport } = await import(pathToFileURL(join(baseline, 'packages/sdk/dist/transport.js')));
const endpoint = 'ws://127.0.0.1:28999/ws';
const originalWebSocket = globalThis.WebSocket;
const watch = setTimeout(() => { throw Error('#1145 supplemental TS transitions bounded'); }, 10_000);

function fixture(requestTimeoutMs = 1000) {
  let socket, submit, abortFrame, closes = 0, resolveSubmit, resolveAbort;
  const submitted = new Promise(done => { resolveSubmit = done; });
  const aborted = new Promise(done => { resolveAbort = done; });
  const deliver = frame => socket.dispatchEvent(new MessageEvent('message', { data: JSON.stringify(frame) }));
  globalThis.WebSocket = class extends EventTarget {
    constructor(url) {
      super();
      assert.equal(url, endpoint, '#1145 supplemental socket uses isolated mock endpoint');
      socket = this;
      this.readyState = 0;
      queueMicrotask(() => { this.readyState = 1; this.dispatchEvent(new Event('open')); });
    }
    send(raw) {
      const frame = JSON.parse(raw);
      if (frame.type === 'hello') {
        queueMicrotask(() => deliver({ type: 'hello_ok', protocolVersion: '1.2',
          serverVersion: 'fixture', serverInfo: { mode: 'remote', capabilities: [] } }));
      } else if (frame.method === 'resume_session') {
        queueMicrotask(() => deliver({ type: 'response', id: frame.id, ok: true, result: { sessionKey: 's' } }));
      } else if (frame.method === 'submit_turn') {
        submit = frame; resolveSubmit();
      } else {
        assert.equal(frame.method, 'abort_turn', '#1145 supplemental expected abort RPC');
        abortFrame = frame; resolveAbort();
      }
    }
    close() { closes += 1; this.readyState = 3; this.dispatchEvent(new Event('close')); }
  };
  const transport = new GatewayTransport({ url: endpoint, token: 'fixture', requestTimeoutMs });
  const query = createQueryWithTransport('prompt', { sessionId: 's' }, transport, 'requested-run');
  return {
    query, transport, submitted, aborted, deliver,
    get submit() { return submit; },
    get abortFrame() { return abortFrame; },
    state: () => ({ pendingAbort: transport.pending.size, socketPresent: !!transport.socket, closes }),
    cleanup: () => transport.close(),
  };
}

const cases = [];
try {
  for (const type of ['turn_started', 'turn.started']) {
    for (const [label, runId] of [['ordinary', 'server-run'], ['empty', ''], ['lone-surrogate', '\ud800']]) {
      const f = fixture();
      try {
        const event = { type, runId };
        const reading = f.query.next();
        await f.submitted;
        f.deliver({ type: 'event', id: f.submit.id, seq: 0, final: false, event });
        await reading;
        const aborting = f.query.abort('CLI interrupted');
        await f.aborted;
        const abortParamsJson = JSON.stringify(f.abortFrame.params);
        f.deliver({ type: 'response', id: f.abortFrame.id, ok: true, result: { aborted: true } });
        await aborting;
        cases.push({ id: `run-id-${type}-${label}`, kind: 'run-id', input: { eventJson: JSON.stringify(event) },
          output: { abortParamsJson, ...f.state() } });
      } finally { f.cleanup(); }
    }
  }
  for (const kind of ['response-error', 'sequence-gap']) {
    const f = fixture();
    try {
      const reading = f.query.next().then(() => false, () => true);
      await f.submitted;
      const aborting = f.query.abort('CLI interrupted');
      await f.aborted;
      f.deliver(kind === 'response-error'
        ? { type: 'response', id: f.submit.id, ok: false, error: { code: 'server_error', message: 'stream rejected' } }
        : { type: 'event', id: f.submit.id, seq: 2, final: false, event: { type: 'turn_started' } });
      const readRejected = await reading;
      const beforeReply = f.state();
      f.deliver({ type: 'response', id: f.abortFrame.id, ok: true, result: { aborted: true } });
      await aborting;
      // Sequence errors mention a fresh RPC UUID. This fixture observes only
      // lifecycle state; it does not rewrite or normalize any CLI original.
      cases.push({ id: `pending-abort-${kind}`, kind: 'pending-abort', input: { failure: kind },
        output: { readRejected, beforeReply, afterReply: f.state() } });
    } finally { f.cleanup(); }
  }
  for (const type of ['toString', '__proto__', { toString: 'x' }]) {
    const f = fixture();
    try {
      const event = { type };
      const reading = (async () => {
        try { for await (const event of f.query) event.type.includes('assistant'); }
        catch (error) { return error.message; }
        throw Error('#1145 supplemental error path must reject');
      })();
      await f.submitted;
      f.deliver({ type: 'event', id: f.submit.id, seq: 0, final: false, event });
      const message = await reading;
      const state = f.state();
      cases.push({ id: `event-error-${typeof type === 'string' ? type : 'object-to-primitive'}`,
        kind: 'event-error', input: { eventJson: JSON.stringify(event) },
        output: { message, errorKind: state.closes === 1 ? 'consumer' : 'iterator', ...state } });
    } finally { f.cleanup(); }
  }
  for (const mode of ['ready-flood', 'idle', 'reply-before-deadline', 'failed-stream']) {
    const f = fixture(30_000);
    let reading;
    try {
      reading = f.query.next().then(value => ({ value }), error => ({ error: error.message }));
      await f.submitted;
      // Install only after initialization; every abort timer is real SDK code.
      mock.timers.enable({ apis: ['setTimeout'] });
      let settled = false;
      const aborting = f.query.abort('CLI interrupted').then(
        () => { settled = true; return { resolved: true }; },
        error => { settled = true; return { resolved: false, message: error.message }; },
      );
      await f.aborted;
      let consumed = 0;
      if (mode === 'ready-flood') {
        // Queue zero-interval non-final events before consuming them. The RPC
        // timer is independent of both the queue and the iterator's readiness.
        for (let seq = 0; seq < 64; seq += 1)
          f.deliver({ type: 'event', id: f.submit.id, seq, final: false, event: { type: 'turn_started' } });
        await reading;
        consumed += 1;
        for (; consumed < 32; consumed += 1) await f.query.next();
      } else if (mode === 'failed-stream') {
        f.deliver({ type: 'response', id: f.submit.id, ok: false, error: { code: 'server_error', message: 'stream rejected' } });
        assert.equal((await reading).error, 'stream rejected', '#1145 failed stream leaves abort pending');
      }
      mock.timers.tick(29_999);
      await Promise.resolve();
      assert.equal(settled, false, '#1145 abort deadline not early');
      const beforeDeadline = f.state();
      if (mode === 'ready-flood') {
        for (; consumed < 48; consumed += 1) await f.query.next();
      }
      if (mode === 'reply-before-deadline')
        f.deliver({ type: 'response', id: f.abortFrame.id, ok: true, result: { aborted: true } });
      else
        mock.timers.tick(1);
      const result = await aborting;
      let readyAfterDeadline = false;
      if (mode === 'ready-flood') {
        const next = await f.query.next();
        readyAfterDeadline = !next.done;
        consumed += 1;
        assert.equal(readyAfterDeadline, true, '#1145 expired abort still has ready non-final stream frames');
      }
      const row = { id: `abort-deadline-${mode}`, kind: 'abort-deadline', input: { mode, timeoutMs: 30_000 },
        output: { beforeDeadline, consumed, readyAfterDeadline, ...result, ...f.state() } };
      cases.push(row);
      console.log(JSON.stringify(row));
    } finally {
      f.cleanup();
      if (reading) await reading;
      mock.timers.reset();
    }
  }
  assert.equal(cases.length, 15, '#1145 fixed supplemental matrix denominator');
  assert.equal(new Set(cases.map(row => row.id)).size, 15, '#1145 unique supplemental matrix cells');
  validateFiles(source, parent, baseline);
  assert.equal(sha(readFileSync(join(parent, 'source.json'))), sha(sourceBytes), '#1145 supplemental parent source unchanged');
  assert.equal(sha(readFileSync(fileURLToPath(import.meta.url))), generatorSha256, '#1145 supplemental generator unchanged');
  const oracle = { sourceCommit: source.sourceCommit, nodeVersion: process.version, generatorSha256,
    signalSourceSha256: sha(sourceBytes), sourceSha256: source.sourceSha256,
    kind: 'real-ts-sdk-state-with-websocket-io-double', matrixCells: 15, cases };
  const output = resolve(process.argv[2]);
  mkdirSync(output, { recursive: true });
  writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
  console.log('#1145 generated 15 real TS supplemental state observations');
} finally {
  clearTimeout(watch);
  globalThis.WebSocket = originalWebSocket;
}
