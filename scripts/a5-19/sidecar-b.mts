/** Real baseline stdio sidecar, host model barrier, real cancellation/unknown ledger. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { resolve, relative } from 'node:path';
import { Apparatus, OUTPUT, ROOT, BASELINE, TIME, deferred, clone, upstream, sha, verifySources } from './runtime-b.mts';
import { isDeepStrictEqual } from 'node:util';
import { SIDECAR_FOUR } from './preregistration-sidecar-four.mjs';
import { cleanupAll } from './cleanup-observer.mjs';
import { execFileSync } from 'node:child_process';
import { REPEATS } from './preregistration-b.mjs';
import { checkStatus, checkRegistry, requireRule } from './rules-b.mjs';
const { createAgentSession } = await upstream('agent/session/createAgentSession');
const { createAgentLoopSidecarRuntimeFactory } = await upstream('agent/modules/transport/agentLoopSidecarClient');
const { createStdioAgentLoopSidecarConnectionFactory } = await upstream('agent/modules/transport/stdioAgentLoopSidecarConnection');
const { createDefaultPermissionContext } = await upstream('permission/index');
const { AsyncQueue } = await upstream('gateway/util/AsyncQueue');
const TIMEOUT_MS = 123456;
// The sidecar validates absolute deadlines against its real clock. A historical
// transcript timestamp must not make execute expire before the controlled wait.
const registrationPath = 'scripts/a5-19/preregistration-sidecar-four.mjs';
const registrationBytes = fs.readFileSync(resolve(ROOT, registrationPath));
const registrationHead = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: ROOT, encoding: 'utf8' }).trim();
assert.deepEqual(execFileSync('git', ['show', `${registrationHead}:${registrationPath}`], { cwd: ROOT }), registrationBytes, 'four-cell registration must be committed before collection');
const collectionStartedAt = new Date().toISOString();
const observations: any[] = [];
fs.mkdirSync(OUTPUT, { recursive: true });
for (const withRegistry of [false, true]) for (const phase of ['model', 'tool']) for (let repetition = 1; repetition <= REPEATS; repetition++) {
  const caseId = `sidecar/${withRegistry ? 'registry' : 'no-registry'}/fast/${phase}`;
  const collectionTime = new Date().toISOString();
  const a = new Apparatus(resolve(OUTPUT, caseId, String(repetition)));
  a.gateway.now = () => new Date(collectionTime);
  const heldModel = deferred(), modelStarted = deferred();
  const childConnections: any[] = [];
  const oldEnqueue = AsyncQueue.prototype.enqueue, oldClose = AsyncQueue.prototype.close;
  const realSetTimeout = globalThis.setTimeout, realClearTimeout = globalThis.clearTimeout;
  let timer: any;
  globalThis.setTimeout = ((callback: any, ms: number, ...args: any[]) => {
    if (ms !== TIMEOUT_MS) return realSetTimeout(callback, ms, ...args);
    timer = { callback: () => callback(...args), cancelled: false }; a.j.add('timer.arm'); return timer;
  }) as any;
  globalThis.clearTimeout = ((handle: any) => {
    if (handle === timer) { timer.cancelled = true; a.j.add('timer.clear'); }
    else realClearTimeout(handle);
  }) as any;
  AsyncQueue.prototype.enqueue = function(event: any) { a.j.add('queue.enqueue', { event, closed: this.closed }); return oldEnqueue.call(this, event); };
  AsyncQueue.prototype.close = function() { a.j.add('queue.close', { closed: this.closed }); return oldClose.call(this); };
  const watchdog = realSetTimeout(() => {
    fs.writeFileSync(resolve(a.dir, 'watchdog.json'), JSON.stringify(a.j.rows, null, 2));
    heldModel.resolve();
    for (const connection of childConnections) connection.child.kill('SIGTERM');
    process.exit(2);
  }, 20000);
  if (!withRegistry) a.gateway.options.runRegistry = undefined;
  const originalCreate = a.createSession.bind(a);
  a.createSession = (session: string) => {
    originalCreate(session); const s = a.sessions.get(session);
    const sidecarPath = resolve(BASELINE, 'dist/src/cli/pilotdeck-agent-loop-sidecar.js');
    const connect = createStdioAgentLoopSidecarConnectionFactory({ command: process.execPath,
      args: ['--import', resolve(ROOT, 'scripts/a5-19/no-listen-child.mjs'), sidecarPath], cwd: a.dir,
      env: { ...process.env, PILOT_HOME: process.env.PILOT_HOME, A52B_CHILD_LOADLOG: resolve(a.dir, 'child-loaded.ndjson') }, killTimeoutMs: 1000 });
    let uid = 0;
    const factory = createAgentLoopSidecarRuntimeFactory({
      connect: (input: any) => {
        const connection = connect(input); childConnections.push(connection);
        a.j.add('sidecar.spawn', { pid: connection.child.pid, command: process.execPath, entry: sidecarPath, sha256: sha(fs.readFileSync(sidecarPath)) });
        const send = connection.send.bind(connection);
        let stdout = '';
        connection.child.stdout.on('data', (chunk: any) => {
          stdout += String(chunk);
          while (stdout.includes('\n')) {
            const index = stdout.indexOf('\n'), text = stdout.slice(0, index); stdout = stdout.slice(index + 1);
            if (text.trim()) a.j.add('sidecar.stdout', { text, message: JSON.parse(text) });
          }
        });
        connection.send = (message: any) => { a.j.add('sidecar.send', { message }); return send(message); };
        const receive = connection.receive.bind(connection);
        connection.receive = async function*() { for await (const message of receive()) { a.j.add('sidecar.receive', { message }); yield message; } };
        connection.child.on('exit', (code: any, signal: any) => a.j.add('sidecar.exit', { pid: connection.child.pid, code, signal }));
        return connection;
      }, uuid: () => `sidecar-${++uid}`,
      transportObserver: { observe: (observation: any) => a.j.add('sidecar.observation', { observation }) },
    });
    const model = {
      async prepare({ request }: any) { return { request, provider: request.provider, model: request.model }; },
      async *stream() {
        if (phase === 'model') { a.j.barrier('module:model-held'); modelStarted.resolve(); await heldModel.promise; }
        yield { type: 'message_start', role: 'assistant' };
        if (phase === 'tool') {
          yield { type: 'tool_call_start', id: 'held-tool', name: 'fixture_hold' };
          yield { type: 'tool_call_end', toolCall: { id: 'held-tool', name: 'fixture_hold', input: {} } };
        } else yield { type: 'text_delta', text: 'late host model' };
        yield { type: 'message_end', finishReason: phase === 'tool' ? 'tool_call' : 'stop' };
      },
    };
    const descriptors = [{ name: 'fixture_hold', description: 'Controlled host barrier', inputSchema: { type: 'object', properties: {} } }];
    const executeAll = async (calls: any[]) => { a.j.barrier('module:tool-held'); modelStarted.resolve(); await heldModel.promise; return calls.map(call => ({ toolCallId: call.id ?? call.toolCallId, name: call.name, result: { type: 'text', text: 'released' }, isError: false })); };
    const agent = createAgentSession({ sessionId: session, transcript: s.writer, collectFileArtifacts: false,
      config: { provider: 'host-fixture', model: 'controlled', cwd: a.dir, permissionMode: 'default',
        permissionContext: createDefaultPermissionContext({ cwd: a.dir, canPrompt: false }) },
      dependencies: { router: {}, now: () => new Date(TIME), ports: { model, tools: { list: () => descriptors, executeAll } },
        tools: { registry: { list: () => descriptors }, scheduler: { executeAll } } },
      agentLoopFactory: factory,
    });
    s.agent = agent;
    const submit = agent.submit.bind(agent);
    agent.submit = async function*(...args: any[]) { try { yield* submit(...args); } finally { a.j.add('session.submit.exit'); } };
    return agent;
  };
  const frames: any[] = [];
  try {
    const iterator = a.gateway.submitTurn({ sessionKey: 'session', channelKey: 'fixture', runId: 'run', message: 'hold real sidecar model', timeoutMs: TIMEOUT_MS })[Symbol.asyncIterator]();
    while (true) { const next = await iterator.next(); assert.equal(next.done, false); frames.push(clone(next.value)); if (next.value.type === (phase === 'model' ? 'model_request_started' : 'tool_call_started')) break; }
    await modelStarted.promise; a.j.barrier('consumer:paused');
    assert.ok(timer && !timer.cancelled); a.j.add('timer.fire'); timer.callback();
    await a.j.wait(r => r.kind === 'queue.close'); a.j.barrier('timeout:queue-closed');
    // Real cancel travels over Module Protocol; provider returns only after it.
    const cancel = await a.j.wait(r => r.kind === 'sidecar.send' && r.message.method === 'cancel');
    // The production client awaits the in-flight model callback before reading
    // its next protocol message. Observe raw stdout ACK passively instead of
    // demanding consumer progress behind that same barrier (would deadlock).
    await a.j.wait(r => r.kind === 'sidecar.stdout' && r.message.inReplyTo === cancel.message.messageId && r.message.kind === 'response');
    heldModel.resolve(); a.j.barrier('module:model-released');
    await a.j.wait(r => r.kind === 'session.submit.exit');
    await a.j.wait(r => r.kind === 'queue.close' && r.closed);
    while (true) { const next = await iterator.next(); if (next.done) break; frames.push(clone(next.value)); }
    const observation = await a.observe(caseId, repetition, [], [`module:${phase}-held`, 'consumer:paused', 'timeout:queue-closed', 'module:model-released']);
    observation.realtimeFrames = frames;
    observation.deadlineClock = { collectionTime, operationDeadline: a.j.rows.find(r => r.kind === 'sidecar.send' && r.message.method === 'execute')?.message.operationDeadline };
    assert.equal(observation.deadlineClock.operationDeadline, new Date(Date.parse(collectionTime) + TIMEOUT_MS).toISOString());
    if (!withRegistry) observation.registry = { applicable: false, reason: 'runRegistry deliberately not configured for no-registry cell' };
    else checkRegistry(observation);
    checkStatus(observation);
    const j = a.j.rows;
    const targetCall = j.find(r => r.kind === 'sidecar.observation' && r.observation.type === 'module_call_received'
      && r.observation.module === (phase === 'model' ? 'model' : 'capability')
      && (phase === 'model' ? r.observation.operation === 'stream_next' : ['execute', 'execute_batch'].includes(r.observation.operation)));
    assert.ok(targetCall && targetCall.order < j.find(r => r.kind === 'timer.fire').order, 'target Module call precedes explicit timeout');
    const unknown = observation.durable.flatMap(d => d.entries).filter(e => e.type === 'agent_loop_operation_terminal');
    const proof = j.some(r => r.kind === 'sidecar.observation' && r.observation.type === 'handshake_completed')
      && j.some(r => r.kind === 'sidecar.observation' && r.observation.type === 'module_call_received' && r.observation.module === (phase === 'model' ? 'model' : 'capability'));
    requireRule(proof && j.some(r => r.kind === 'sidecar.spawn') && unknown.length > 0, 'SIDECAR_IDENTITY');
    const rawPublications = j.filter(r => r.kind === 'queue.enqueue' && !r.closed).map(r => r.event);
    requireRule(isDeepStrictEqual(frames, rawPublications), 'FRAME_RAW');
    const childLoaded = fs.readFileSync(resolve(a.dir, 'child-loaded.ndjson'), 'utf8').trim().split('\n').map(JSON.parse);
    for (const loaded of childLoaded) assert.equal(sha(fs.readFileSync(loaded.path)), loaded.sha256, `child source changed: ${loaded.path}`);
    observation.childLoaded = childLoaded;
    observation.sidecarProof = { handshakeAndRealModule: proof, terminalEntries: unknown, unknownEntries: unknown.filter(e => e.outcome === 'result_unknown'), ownsOnlySpawnedChildren: true };
    fs.writeFileSync(resolve(a.dir, 'observation.json'), JSON.stringify(observation, null, 2)); observations.push(observation);
  } finally {
    heldModel.resolve();
    await cleanupAll([
      ...childConnections.map(connection => () => connection.close('fixture_cleanup')),
      () => {
        const completed = observations.at(-1);
        if (completed?.caseId === caseId && completed.repetition === repetition) {
          completed.timeline = a.j.rows;
          fs.writeFileSync(resolve(a.dir, 'observation.json'), JSON.stringify(completed, null, 2));
        }
      },
      // Expand Apparatus.close so a dispose/shutdown/writer failure cannot
      // prevent another owned resource or filesystem tap from being restored.
      () => a.gateway.dispose(),
      () => a.router.shutdown(),
      ...[...a.sessions.values()].map(session => () => session.writer.close()),
      () => a.tap.restore(),
    ], () => {
      realClearTimeout(watchdog);
      globalThis.setTimeout = realSetTimeout; globalThis.clearTimeout = realClearTimeout;
      AsyncQueue.prototype.enqueue = oldEnqueue; AsyncQueue.prototype.close = oldClose;
    });
  }
}
function signature(o: any) {
  const identities = new Map<string, Map<string, string>>();
  const normalize = (value: any): any => {
    if (Array.isArray(value)) return value.map(normalize);
    if (!value || typeof value !== 'object') return value;
    return Object.fromEntries(Object.entries(value).map(([key, item]) => {
      if (!['moduleInstanceId', 'connectionGeneration', 'streamId'].includes(key)) return [key, normalize(item)];
      let names = identities.get(key); if (!names) { names = new Map(); identities.set(key, names); }
      const original = String(item); if (!names.has(original)) names.set(original, `${key}-${names.size}`);
      return [key, names.get(original)];
    }));
  };
  const records = o.registry.applicable ? o.registry.records.map((r: any) => { const copy = { ...r }; for (const k of ['acceptedAt', 'updatedAt', 'ownerPid']) delete copy[k]; return copy; }) : undefined;
  return normalize({ realtimeFrames: o.realtimeFrames, durableEntries: o.durable[0].entries, registry: o.registry.applicable ? { records, events: o.registry.events } : o.registry,
    droppedEnqueues: o.timeline.filter(r => r.kind === 'queue.enqueue' && r.closed).map(r => r.event) });
}
const expectedCases = SIDECAR_FOUR.cases;
assert.equal(REPEATS, SIDECAR_FOUR.repeats);
assert.deepEqual([...new Set(observations.map(o => o.caseId))], expectedCases);
assert.equal(observations.length, expectedCases.length * REPEATS);
const groups = expectedCases.map(caseId => { const runs = observations.filter(o => o.caseId === caseId); const distinct = new Set(runs.map(o => JSON.stringify(signature(o)))).size; return { caseId, measurements: runs.length, distinct, stable: distinct === 1 }; });
for (const group of groups) assert.equal(group.measurements, SIDECAR_FOUR.repeats, `incomplete group ${group.caseId}`);
const distinct = Math.max(...groups.map(g => g.distinct));
const manifest = { expectedCases, groups, repeats: REPEATS, measurements: observations.length, importedSources: verifySources(),
  preregistrationSha256: sha(fs.readFileSync(resolve(ROOT, 'scripts/a5-19/preregistration-b.mjs'))),
  preregistrationCommit: JSON.parse(process.env.A52B_PREREGISTRATION_STATUS!),
  fixtureFiles: ['sidecar-b.mts', 'no-listen-child.mjs', 'runtime-b.mts', 'rules-b.mjs', 'run-b.py', 'cleanup-observer.mjs', 'preregistration-sidecar-four.mjs'].map(name => ({ path: `scripts/a5-19/${name}`, sha256: sha(fs.readFileSync(resolve(ROOT, 'scripts/a5-19', name))) })),
  registration: { path: registrationPath, sha256: sha(registrationBytes), committedBytesMatch: true, headCommit: registrationHead, collectionStartedAt, historicalRuns: 'POST_HOC: old SIDECAR_CASE registration covered only one cell' },
  stable: distinct === 1, distinct,
  observations: observations.map(o => ({ caseId: o.caseId, repetition: o.repetition, realtimeFrames: o.realtimeFrames,
    deadlineClock: o.deadlineClock, statuses: o.durable[0].entries.filter(e => e.type === 'agent_status_message'),
    terminalEntries: o.sidecarProof.terminalEntries, unknownEntries: o.sidecarProof.unknownEntries, sidecarEvents: o.timeline.filter(r => r.kind === 'sidecar.observation'),
    evidence: relative(ROOT, resolve(OUTPUT, o.caseId, String(o.repetition), 'observation.json')) })) };
fs.writeFileSync(resolve(OUTPUT, 'manifest.json'), JSON.stringify(manifest, null, 2));
assert.ok(groups.every(g => g.stable), 'unstable sidecar group; manifest retained');
console.log(`DONE real sidecar cross-check ${observations.length}`);
