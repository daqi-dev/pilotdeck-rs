// #1145 Real Node CLI + real loopback sockets. Run through capture.py's lease.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync, readdirSync, lstatSync, openSync, closeSync, renameSync, constants } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { cleanupAll } from './cleanup.mjs';

assert.equal(process.version, 'v22.22.0', '#1145 probe Node version');
const here = dirname(fileURLToPath(import.meta.url));
const output = resolve(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE ?? '/Users/da/ws/pilotdeck-rs/resources/PilotDeck');
const owner = JSON.parse(readFileSync('/tmp/pdrs-parity.lock/owner.json', 'utf8'));
assert.equal(owner.pid, process.ppid, '#1145 capture.py owns parity lease');
assert.equal(String(owner.pid), process.env.PDRS_SIGNAL_LEASE, '#1145 lease invocation identity');
const commit = spawnSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], { encoding: 'utf8' });
assert.equal(commit.status, 0, '#1145 baseline identity command');
assert.equal(commit.stdout.trim(), 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', '#1145 fixed baseline');
const require = createRequire(join(baseline, 'package.json'));
const { WebSocketServer } = require('ws');
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const save = (path, data) => writeFileSync(path, data, { flag: 'wx', mode: 0o600 });
const saveJson = (path, data) => save(path, JSON.stringify(data, null, 2) + '\n');
const BOUND_MS = 12_000;
const CLEANUP_MS = 3_000;
const token = 'd1-fixture-not-a-secret';
const endpoint = 'ws://127.0.0.1:28999/ws';
const all = JSON.parse(readFileSync(join(here, 'cases.json'), 'utf8'));
const gaps = JSON.parse(readFileSync(join(here, 'gap-source.json'), 'utf8'));
const selected = process.argv.slice(3);
assert.equal(new Set(selected).size, selected.length, '#1145 unique selected cells');
assert.ok(selected.every(id => all.some(row => row.id === id)), '#1145 known selected cells');
assert.ok(!selected.some(id => gaps.some(gap => gap.id === id)), '#1145 stdin lane suspended; retain originals and report #123 gap');
const definitions = all.filter(row => !gaps.some(gap => gap.id === row.id) && (!selected.length || selected.includes(row.id)));
const sourceFiles = ['src/cli/sdkCli.ts', 'packages/sdk/src/client.ts', 'packages/sdk/src/transport.ts',
  'dist/src/cli/pilotdeck.js', 'dist/src/cli/sdkCli.js', 'packages/sdk/dist/client.js', 'packages/sdk/dist/transport.js'];
const sourceSha256 = Object.fromEntries(sourceFiles.map(path => [path, sha(readFileSync(join(baseline, path)))]));
const producerFiles = ['probe.mjs', 'hooks.mjs', 'cases.json', 'capture.py', 'cleanup.mjs', 'gap-source.json'];
const probeSha256 = Object.fromEntries(producerFiles.map(file => [file, sha(readFileSync(join(here, file)))]));

function snapshot(root) {
  const result = {};
  function visit(path, name) {
    const stat = lstatSync(path);
    result[name] = stat.isDirectory() ? { type: 'directory', mode: stat.mode & 0o777 }
      : { type: 'file', mode: stat.mode & 0o777, size: stat.size, sha256: sha(readFileSync(path)) };
    if (stat.isDirectory()) for (const entry of readdirSync(path).sort()) visit(join(path, entry), name === '.' ? entry : `${name}/${entry}`);
  }
  visit(root, '.');
  return result;
}
async function bounded(promise, label) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`#1145 ${label} exceeded ${CLEANUP_MS}ms`)), CLEANUP_MS);
    })]);
  } finally { clearTimeout(timer); }
}

async function capture(input) {
  const directory = join(output, 'raw', input.id);
  mkdirSync(directory, { recursive: true });
  const runtime = join(directory, 'runtime');
  mkdirSync(runtime, { mode: 0o700 });
  for (const name of ['home', 'pilot', 'project', 'tmp', 'bin', 'xdg-config', 'xdg-cache', 'xdg-data', 'xdg-state']) {
    mkdirSync(join(runtime, name), { mode: 0o700 });
  }
  save(join(runtime, 'pilot/pilotdeck.yaml'), 'schemaVersion: 1\n');
  save(join(runtime, 'project/keep.txt'), Buffer.from('P08 fixture\0\xff\n', 'latin1'));
  save(join(runtime, 'openssl.cnf'), '');
  const initial = snapshot(runtime);
  const env = { LANG: 'C', LC_ALL: 'C', TZ: 'UTC', TERM: 'dumb', NO_COLOR: '1',
    ANALYTICS_ENABLED: '0', DOCKER_CONTAINER: '1', PDRS_BASELINE: baseline,
    PILOTDECK_GATEWAY_HOST: '127.0.0.1', PILOTDECK_GATEWAY_PORT: '28999',
    PILOTDECK_SDK_GATEWAY_URL: endpoint, PILOTDECK_GATEWAY_URL: endpoint,
    PILOTDECK_SDK_AUTH_TOKEN: token, COMMIT_HASH: commit.stdout.trim() };
  for (const [key, path] of Object.entries({ HOME: 'home', USERPROFILE: 'home', CFFIXED_USER_HOME: 'home',
    PILOT_HOME: 'pilot', PILOTDECK_CONFIG_PATH: 'pilot/pilotdeck.yaml', XDG_CONFIG_HOME: 'xdg-config',
    XDG_CACHE_HOME: 'xdg-cache', XDG_DATA_HOME: 'xdg-data', XDG_STATE_HOME: 'xdg-state', TMPDIR: 'tmp',
    TMP: 'tmp', TEMP: 'tmp', PWD: 'project', PATH: 'bin', OPENSSL_CONF: 'openssl.cnf' })) env[key] = join(runtime, path);
  const argv = [input.leaf, ...(input.leaf === 'resume' ? ['d1-session'] : []),
    ...(input.stage === 'stdin' ? [] : ['fixture prompt']), '--stream-json'];
  if (input.stage === 'query-model') argv.push('--project', 'd1-project', '--model', 'fixture/model');
  const trace = [], hooks = [], wire = [], stdout = [], stderr = [], closes = [];
  let child, closedPromise, timer, server, stdinFd, hookBuffer = '', error;
  let connections = 0, active = 0, registered = false, triggerReached = false, signalDelivered = false;
  let aborts = 0, secondDelivered = false, expired = false, cleanupStarted = false, exit;
  let held, stream, pendingAbort, streamReady = false, terminalConsumed = false, resolvedHeld = false;
  let abortEntered = false, terminalAfterAbort = false, lateAbortHandled = false;
  let closeFailure;
  const failedClose = new Promise((_, reject) => { closeFailure = reject; });
  // Attach immediately so a spawn/setup failure cannot leave a rejected promise
  // unobserved before the main close race is installed.
  failedClose.catch(() => {});
  let allClosed;
  const closedSockets = new Promise(done => { allClosed = done; });
  const record = row => { const value = { seq: trace.length, ...row }; trace.push(value); return value; };
  const send = (socket, frame) => {
    const row = record({ kind: 'wire-send', connection: socket.identity, frame, raw: JSON.stringify(frame), cleanup: cleanupStarted });
    wire.push(row);
    assert.equal(socket.readyState, 1, '#1145 send only to open owned socket');
    socket.send(row.raw);
  };
  const reply = (request, ok = true) => send(request.socket, request.frame.type === 'hello'
    ? { type: 'hello_ok', protocolVersion: '1.2', serverVersion: '0.1.0-mock', serverInfo: { mode: 'remote', projectKey: 'd1-project', sessionCount: 1, capabilities: [] } }
    : ok ? { type: 'response', id: request.frame.id, ok: true, result: request.frame.method === 'model_catalog_list'
      ? { items: [{ id: 'fixture/model', provider: 'fixture', model: 'model' }] }
      : request.frame.method === 'abort_turn' ? { aborted: true } : { sessionKey: 'd1-session' } }
      : { type: 'response', id: request.frame.id, ok: false, error: { code: 'server_error', message: 'd1 fixture rejected' } });
  const terminal = (kind, final) => send(stream.socket, { type: 'event', id: stream.frame.id, seq: 0, final,
    event: kind === 'failed' ? { type: 'error', code: 'server_error', message: 'd1 terminal failed' }
      : kind === 'unknown' ? { type: 'input_accepted' } : { type: 'turn_completed', result: 'wire terminal', usage: {} } });
  function arm(label) {
    clearTimeout(timer);
    timer = setTimeout(() => {
      expired = true;
      record({ kind: 'observation-deadline', label, milliseconds: BOUND_MS });
      cleanupStarted = true;
      if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
      bounded(closedPromise, 'deadline child/pipe cleanup').catch(closeFailure);
    }, BOUND_MS);
  }
  function reach(kind) {
    if (triggerReached) return;
    triggerReached = true;
    record({ kind: 'trigger-reached', trigger: kind });
    if (input.stage === 'control' && input.action === 'hold' && !input.signal) {
      // Only the cancellation self-check consumes this atomically published
      // marker; lifecycle actions above continue to use client/wire events.
      const marker = join(directory, 'cell-ready.json');
      const temporary = marker + '.tmp';
      saveJson(temporary, { pid: child.pid, id: input.id });
      renameSync(temporary, marker);
    }
    arm('post-trigger');
    if (input.signal) {
      signalDelivered = child.kill(input.signal);
      record({ kind: 'signal-sent', signal: input.signal, delivered: signalDelivered });
      assert.ok(signalDelivered, '#1145 signal delivered at reachable hook');
    }
  }
  function maybeTrigger() {
    if (input.stage.startsWith('query-') && held && registered) reach(input.stage);
    if (['stream', 'abort-terminal'].includes(input.stage) && streamReady && registered) reach('stream-held');
    if (input.stage === 'terminal' && terminalConsumed && registered) reach('client-terminal-consumed');
  }
  function onHook(hook) {
    hooks.push(record({ kind: 'client-hook', hook, cleanup: cleanupStarted }));
    if (hook.kind === 'listener-installed' && hook.listeners.SIGINT === 1 && hook.listeners.SIGTERM === 1) registered = true;
    if (hook.kind === 'stdin-read-enter' && input.stage === 'stdin') {
      assert.deepEqual(hook.listeners, { SIGINT: 0, SIGTERM: 0 }, '#1145 stdin precedes signal listeners');
      reach('client-stdin-read-enter');
    }
    if (hook.kind === 'abort-enter') abortEntered = true;
    if (hook.kind === 'abort-enter' && held && !resolvedHeld && input.action !== 'hold') {
      resolvedHeld = true;
      record({ kind: 'predecessor-action', action: input.action, stage: input.stage });
      if (input.action === 'reject' && input.stage === 'query-hello') {
        record({ kind: 'server-reject-hello', connection: held.socket.identity });
        held.socket.close(4003, 'auth_failed');
      } else reply(held, input.action !== 'reject');
    }
    if (hook.kind === 'terminal-consumed') {
      terminalConsumed = true;
      if (input.stage === 'abort-terminal') terminalAfterAbort = true;
      if (input.stage === 'abort-terminal' && !input.final) {
        reply(pendingAbort); lateAbortHandled = true;
        record({ kind: 'late-abort-handled' });
      }
    }
    if (input.stage === 'abort-terminal' && input.final && hook.kind === 'listener-removed'
      && hook.listeners.SIGINT === 0 && hook.listeners.SIGTERM === 0 && pendingAbort) {
      // The peer may have closed by this point. Record the unreachable late
      // response explicitly; never mislabel it as consumed by the client.
      if (pendingAbort.socket.readyState === 1) reply(pendingAbort);
      else record({ kind: 'abort-response-unreachable-after-client-close' });
      lateAbortHandled = true;
      record({ kind: 'late-abort-handled' });
      pendingAbort = undefined;
    }
    maybeTrigger();
  }
  function onFrame(socket, raw) {
    const frame = JSON.parse(raw);
    wire.push(record({ kind: 'wire-receive', connection: socket.identity, frame, raw, cleanup: cleanupStarted }));
    const request = { socket, frame };
    if (frame.type === 'hello') {
      assert.equal(frame.token, token, '#1145 synthetic authentication');
      if (input.stage === 'query-hello' && socket.identity === 2) { held = request; maybeTrigger(); return; }
      reply(request); return;
    }
    assert.equal(frame.type, 'request', '#1145 request frame');
    if (socket.identity === 1) {
      assert.equal(frame.method, input.leaf === 'run' ? 'new_session' : 'resume_session', '#1145 expected control predecessor');
      if (input.stage === 'control') { held = request; reach('server-control-request'); return; }
      if (input.stage === 'control-reject') reach('server-control-reject');
      reply(request, input.stage !== 'control-reject'); return;
    }
    if ((input.stage === 'query-resume' && frame.method === 'resume_session')
      || (input.stage === 'query-model' && frame.method === 'model_catalog_list')) {
      held = request; maybeTrigger(); return;
    }
    if (frame.method === 'resume_session' || frame.method === 'model_catalog_list') {
      if (input.stage === 'query-reject') reach('server-query-reject');
      reply(request, input.stage !== 'query-reject'); return;
    }
    if (frame.method === 'submit_turn') {
      stream = request; streamReady = true;
      if (input.stage === 'terminal') terminal(input.terminal, false);
      else if (['complete', 'failed', 'unknown'].includes(input.stage)) {
        reach('server-terminal-final'); terminal(input.stage, true);
      } else if (input.stage === 'response-error') {
        reach('server-response-error'); reply(request, false);
      }
      maybeTrigger(); return;
    }
    assert.equal(frame.method, 'abort_turn', '#1145 expected abort RPC');
    aborts += 1;
    if (input.second && aborts === 1) {
      secondDelivered = child.kill(input.second);
      record({ kind: 'second-signal-sent', signal: input.second, delivered: secondDelivered });
      assert.ok(secondDelivered, '#1145 second signal delivered after abort RPC');
    }
    if (input.stage === 'abort-terminal') { pendingAbort = request; terminal('completed', input.final); }
    else if (input.action !== 'hold') reply(request, input.action !== 'error');
  }
  try {
    server = new WebSocketServer({ host: '127.0.0.1', port: 28999 });
    await bounded(once(server, 'listening'), 'mock listen');
    server.on('connection', socket => {
      socket.identity = ++connections; active += 1;
      wire.push(record({ kind: 'connection', connection: socket.identity }));
      socket.on('error', err => { error ??= String(err); });
      socket.on('close', (code, reason) => {
        const row = record({ kind: 'socket-close', connection: socket.identity, code, reason: reason.toString(), cleanup: cleanupStarted });
        closes.push(row); wire.push(row); active -= 1;
        if (active === 0) allClosed();
      });
      socket.on('message', bytes => {
        try { onFrame(socket, bytes.toString()); }
        catch (err) { error ??= String(err); cleanupStarted = true; child.kill('SIGKILL'); }
      });
    });
    if (input.stage === 'stdin') {
      const fifo = join(directory, 'stdin.fifo');
      const made = spawnSync('/usr/bin/mkfifo', ['-m', '600', fifo], { timeout: CLEANUP_MS, encoding: 'utf8' });
      assert.equal(made.status, 0, `#1145 create owned blocking FIFO: ${made.stderr}`);
      // O_RDWR avoids an open handshake and intentionally keeps EOF pending.
      // Unlike Node's socketpair stdio, this descriptor has no O_NONBLOCK flag.
      stdinFd = openSync(fifo, constants.O_RDWR);
    }
    child = spawn(process.execPath, ['--import', join(here, 'hooks.mjs'), join(baseline, 'dist/src/cli/pilotdeck.js'), ...argv],
      { cwd: join(runtime, 'project'), env, stdio: [stdinFd ?? 'pipe', 'pipe', 'pipe', 'pipe'] });
    closedPromise = once(child, 'close');
    child.on('exit', (code, signal) => {
      // Exit determines the lifecycle result; pipe drain has its separate
      // cleanup bound and must not turn a natural exit into a timeout.
      clearTimeout(timer);
      exit = { code, signal, cleanup: cleanupStarted }; record({ kind: 'child-exit', ...exit });
      bounded(closedPromise, 'exit pipe drain').catch(closeFailure);
    });
    child.stdout.on('data', bytes => stdout.push(bytes));
    child.stderr.on('data', bytes => stderr.push(bytes));
    child.stdio[3].on('data', bytes => {
      hookBuffer += bytes.toString();
      let index;
      while ((index = hookBuffer.indexOf('\n')) >= 0) {
        const line = hookBuffer.slice(0, index); hookBuffer = hookBuffer.slice(index + 1);
        try { onHook(JSON.parse(line)); }
        catch (err) { error ??= String(err); cleanupStarted = true; child.kill('SIGKILL'); }
      }
    });
    child.stdin?.on('error', err => { if (err.code !== 'EPIPE') error ??= String(err); });
    child.stdin?.end();
    arm('startup-or-natural-exit');
    await Promise.race([closedPromise, failedClose]);
    clearTimeout(timer);
    assert.equal(hookBuffer, '', '#1145 no truncated client confirmation');
    if (active > 0) await bounded(closedSockets, 'connections after child exit');
    const final = snapshot(runtime);
    const milestones = {
      observer: hooks.some(row => row.hook.kind === 'hooks-ready'), trigger: triggerReached,
      firstSignal: !input.signal || signalDelivered,
      abortEntered: !input.signal || ['control', 'stdin'].includes(input.stage) || abortEntered,
      secondSignal: !input.second || secondDelivered,
      predecessorAction: !['release', 'reject'].includes(input.action) || resolvedHeld,
      terminalConsumed: input.stage !== 'terminal' || terminalConsumed,
      terminalAfterAbort: input.stage !== 'abort-terminal' || terminalAfterAbort,
      lateAbortHandled: input.stage !== 'abort-terminal' || lateAbortHandled,
      blockingStdin: input.stage !== 'stdin' || !/(?:EAGAIN|EPERM|EACCES).*?(?:read|open)/.test(Buffer.concat(stderr).toString()),
    };
    const missingMilestones = Object.keys(milestones).filter(key => !milestones[key]);
    const status = error ? 'INVALID' : missingMilestones.length ? 'TRIGGER_NOT_REACHED'
      : expired ? 'OBSERVED_TIMEOUT' : 'OBSERVED_EXIT';
    const capture = { input: { ...input, argv }, status, nodeVersion: process.version,
      stdinTransport: input.stage === 'stdin' ? 'owned-blocking-fifo' : 'closed-node-pipe',
      milestones, missingMilestones,
      triggerReached, signalDelivered, secondDelivered, aborts, connections, activeAfterExit: active,
      naturalExit: exit?.cleanup ? null : exit, cleanupExit: exit?.cleanup ? exit : null,
      stdout: Buffer.concat(stdout).toString(), stderrRaw: Buffer.concat(stderr).toString(),
      error: error ?? null, initial, final, hooks, wire, closes, trace };
    save(join(directory, 'stdout.bin'), Buffer.concat(stdout));
    save(join(directory, 'stderr.bin'), Buffer.concat(stderr));
    save(join(directory, 'hooks.ndjson'), hooks.map(row => JSON.stringify(row) + '\n').join(''));
    save(join(directory, 'wire.ndjson'), wire.map(row => JSON.stringify(row) + '\n').join(''));
    saveJson(join(directory, 'capture.json'), capture);
    console.log(JSON.stringify({ id: input.id, status, exit: capture.naturalExit, cleanup: capture.cleanupExit, aborts, missingMilestones, error }));
    return capture;
  } finally {
    clearTimeout(timer);
    cleanupStarted = true;
    await cleanupAll([
      async () => {
        if (child && child.exitCode === null && child.signalCode === null) {
          child.kill('SIGKILL'); await bounded(closedPromise, 'child cleanup');
        }
      },
      () => { if (stdinFd !== undefined) closeSync(stdinFd); },
      async () => {
        if (server) {
          for (const socket of server.clients) socket.terminate();
          await bounded(new Promise(done => server.close(done)), 'mock cleanup');
        }
      },
    ]);
  }
}
const captures = [], failures = [];
for (const input of definitions) {
  try { captures.push(await capture(input)); }
  catch (error) {
    const row = { id: input.id, status: 'INVALID', error: String(error) };
    failures.push(row); console.log(JSON.stringify(row));
    saveJson(join(output, 'raw', input.id, 'failure.json'), row);
  }
}
assert.deepEqual(Object.fromEntries(sourceFiles.map(path => [path, sha(readFileSync(join(baseline, path)))])),
  sourceSha256, '#1145 baseline files unchanged throughout capture');
assert.deepEqual(Object.fromEntries(producerFiles.map(file => [file, sha(readFileSync(join(here, file)))])),
  probeSha256, '#1145 producer files unchanged throughout capture');
saveJson(join(output, 'source.json'), { sourceCommit: commit.stdout.trim(), nodeVersion: process.version,
  kind: 'real-ts-cli-real-websocket-observation-hooks', observationMs: BOUND_MS, cleanupMs: CLEANUP_MS,
  sourceSha256, probeSha256,
  captures, gaps, failures });
assert.equal(failures.length + captures.filter(row => row.status === 'INVALID' || row.status === 'TRIGGER_NOT_REACHED').length,
  0, '#1145 complete capture without harness failures');
