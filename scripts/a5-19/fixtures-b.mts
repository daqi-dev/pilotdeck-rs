import assert from 'node:assert/strict';
import fs from 'node:fs';
import { resolve, relative } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { Apparatus, FileRunRegistry, OUTPUT, ROOT, TIME, deferred, errorData, sha, verifySources } from './runtime-b.mts';
import { CASES as PRIMARY_CASES, RULES, CONTROLS, REPEATS, BASELINE_COMMIT } from './preregistration-b.mjs';
import { EXTRA_CASES } from './preregistration-b-extra.mjs';
import { checkObservation, checkCoverage, compareObservers } from './rules-b.mjs';

const CASES = [...PRIMARY_CASES, ...EXTRA_CASES];
fs.mkdirSync(OUTPUT, { recursive: true });
const preregPath = resolve(ROOT, 'scripts/a5-19/preregistration-b.mjs');
fs.writeFileSync(resolve(OUTPUT, 'preregistration.json'), JSON.stringify({ sha256: sha(fs.readFileSync(preregPath)),
  additiveSha256: sha(fs.readFileSync(resolve(ROOT, 'scripts/a5-19/preregistration-b-extra.mjs'))), rules: RULES, cases: CASES, controls: CONTROLS, repeats: REPEATS }, null, 2));
const params = (session = 'session', run = 'run') => ({ sessionKey: session, channelKey: 'fixture', runId: run, message: 'controlled fixture' });
const answer = (p: any, id: string, kind: string, session = 'session', run = 'run') => p.request(id,
  kind === 'permission' ? 'permission_decide' : 'elicitation_respond',
  { sessionKey: session, requestId: `${run}-${kind}`, ...(kind === 'permission' ? { decision: 'allow' } : { answer: { action: 'accept', content: { answer: 'fixture' } } }) });
async function reads(p: any, prefix: string, session = 'session', run = 'run') {
  await p.request(`${prefix}-events`, 'run_events', { sessionKey: session, runId: run });
  await p.request(`${prefix}-snapshot`, 'active_turn_snapshot', { sessionKey: session });
  await p.request(`${prefix}-permissions`, 'permission_list', { sessionKey: session });
  await p.request(`${prefix}-reattach`, 'run_reattach', { sessionKey: session, runId: run });
}

export async function dual(a: Apparatus, c: any, damage?: string) {
  const A = await a.peer('A');
  const B = damage === 'omit-B' ? null : await a.peer('B', c.order === 'sdk-control' ? 'sdk' : 'web');
  A.send('submit-A', 'submit_turn', params()); await a.waitLoop('run');
  if (B) await reads(B, 'B-before');
  if (c.kind) {
    if (c.order === 'handoff-double') {
      await B.request('handoff', 'reconnect_interaction', { sessionKey: 'session', previousBinding: A.binding });
      a.j.barrier('handoff');
      await answer(A, 'old-answer', c.kind); await answer(B, 'new-answer', c.kind); await answer(B, 'duplicate-answer', c.kind);
    } else if (c.order === 'raced-double') {
      await Promise.all([answer(A, 'answer-A', c.kind), answer(B, 'answer-B', c.kind)]);
      a.j.barrier('answers:both-admitted');
    } else {
      const first = c.order === 'other-first' ? B : A, second = first === A ? B : A;
      await answer(first, 'first-answer', c.kind); if (second) await answer(second, 'second-answer', c.kind);
    }
  } else if (c.order === 'sdk-control') {
    await answer(B, 'sdk-answer', 'permission'); await answer(A, 'web-double', 'permission');
  } else if (B) {
    if (c.order.startsWith('handoff-')) {
      await B.request('handoff', 'reconnect_interaction', { sessionKey: 'session', previousBinding: A.binding });
      a.j.barrier('handoff');
    }
    B.send('submit-B', 'submit_turn', params('session', c.order.endsWith('same-run') ? 'run' : 'other-run'));
    await B.final('submit-B');
  }
  if (B) await reads(B, 'B-after'); await reads(A, 'A-after');
  a.release('run'); await A.final('submit-A');
  if (B) await reads(B, 'B-final');
  return ['loop:run', 'release:run', ...(c.order.startsWith('handoff-') ? ['handoff'] : []), ...(c.order === 'raced-double' ? ['answers:both-admitted'] : [])];
}
export async function multi(a: Apparatus, c: any) {
  const A = await a.peer('A'), B = await a.peer('B');
  A.send('submit-A', 'submit_turn', params('session-A', 'run-A'));
  B.send('submit-B', 'submit_turn', params('session-B', 'run-B'));
  await a.waitLoop('run-A'); await a.waitLoop('run-B');
  await reads(A, 'cross-B', 'session-B', 'run-B'); await reads(B, 'cross-A', 'session-A', 'run-A');
  await answer(A, 'wrong-session-answer', 'permission', 'session-B', 'run-B');
  await a.gateway.recordAgentStatusMessage({ sessionKey: 'session-A', turnId: 'run-A', status: { event: 'fixture-A', level: 'info', source: 'fixture', createdAt: TIME } });
  await a.gateway.recordAgentStatusMessage({ sessionKey: 'session-B', turnId: 'run-B', status: { event: 'fixture-B', level: 'info', source: 'fixture', createdAt: TIME } });
  const order = c.order === 'A-first' ? [[A, 'run-A', 'submit-A'], [B, 'run-B', 'submit-B']] : [[B, 'run-B', 'submit-B'], [A, 'run-A', 'submit-A']];
  for (const [peer, run, request] of order as any[]) { a.release(run); await peer.final(request); }
  return ['loop:run-A', 'loop:run-B', 'release:run-A', 'release:run-B'];
}
async function closeRace(a: Apparatus, c: any, damage?: string) {
  const A = await a.peer('A'), B = await a.peer('B');
  const streamReturn = deferred();
  if (!c.kind) {
    const submit = a.gateway.submitTurn.bind(a.gateway);
    a.gateway.submitTurn = async function*(input: any) {
      yield* submit(input);
      if (input.runId === 'run') { a.j.barrier('A-stream-return-held'); await streamReturn.promise; }
    };
  }
  A.send('submit-A', 'submit_turn', params()); await a.waitLoop('run');
  const closeGate = deferred();
  const disconnect = a.gateway.disconnectInteraction.bind(a.gateway);
  a.gateway.disconnectInteraction = async (input: any) => {
    if (c.order === 'before-disconnect') { a.j.barrier('close:held'); await closeGate.promise; }
    const result = disconnect(input); a.j.add('disconnect.result', { input, result });
    if (c.order !== 'before-disconnect') { a.j.barrier('close:held'); await closeGate.promise; }
    return result;
  };
  A.close(); await a.j.wait(r => r.kind === 'barrier' && r.name === 'close:held');
  if (c.kind) {
    await B.request('reconnect', 'reconnect_interaction', { sessionKey: 'session', previousBinding: A.binding });
    await B.request('resume', 'resume_session', { sessionKey: 'session' }); a.j.barrier('reconnect:done');
    a.bypassBinding = damage === 'old-as-new';
    await answer(A, 'old-answer', c.kind); a.bypassBinding = false;
    await answer(B, 'new-answer', c.kind);
    await reads(B, 'B-pending'); closeGate.resolve(); a.j.barrier('close:released');
    // Observe actual disconnect/abort invocation before allowing loop completion.
    await a.j.wait(r => r.kind === 'disconnect.result');
    await B.request('during-close-snapshot', 'active_turn_snapshot', { sessionKey: 'session' });
    a.release('run'); await A.final('submit-A');
  } else {
    a.release('run'); await a.j.wait(r => r.kind === 'barrier' && r.name === 'A-stream-return-held');
    await B.request('resume', 'resume_session', { sessionKey: 'session' });
    B.send('submit-B', 'submit_turn', params('session', 'new-run')); await a.waitLoop('new-run');
    a.j.barrier('reconnect:done'); closeGate.resolve(); a.j.barrier('close:released');
    await a.j.wait(r => r.kind === 'abort.input'); a.j.barrier('old-abort-observed');
    a.release('new-run'); await B.final('submit-B'); streamReturn.resolve(); await A.final('submit-A');
  }
  await a.j.wait(r => r.kind === 'wire.close.done' && r.connection === 'A');
  await reads(B, 'B-final', 'session', c.kind ? 'run' : 'new-run');
  return ['loop:run', 'close:held', 'reconnect:done', 'close:released', 'release:run',
    ...(!c.kind ? ['A-stream-return-held', 'loop:new-run', 'old-abort-observed', 'release:new-run'] : [])];
}

async function sessionFault(a: Apparatus, c: any) {
  const A = await a.peer('A');
  await a.router.getOrCreate({ sessionKey: 'session', channelKey: 'fixture' });
  const s = a.sessions.get('session');
  const input = (event: string) => ({ sessionKey: 'session', turnId: 'run', status: { event, level: 'warn', source: 'fixture', createdAt: TIME } });
  await A.request('seed-status', 'record_agent_status_message', input('seed'));
  let recovery: any;
  if (c.fault === 'read-eio') {
    a.tap.arm('readFile', p => p === s.path);
    try { await s.store.read(); } catch (error) { a.j.add('session.read.failure', { error: errorData(error) }); }
  } else {
    a.tap.arm('appendFile', p => p === s.path, c.fault === 'partial-write' ? 'partial' : 'before');
    await A.request('fault-status', 'record_agent_status_message', input('fault'));
    // Same production writer/provider continues after failed append. Preserve
    // joined partial debris as observed; never repair it to manufacture recovery.
  }
  a.j.barrier('fault:completed');
  await A.request('recovery-status', 'record_agent_status_message', input('recovery'));
  recovery = await s.store.read();
  a.j.add('recovery.read', { recovery });
  return { barriers: ['fault:completed'], recovery };
}
async function registryFault(a: Apparatus, c: any) {
  const A = await a.peer('A'), path = resolve(a.dir, 'registry.json');
  const isAppend = c.fault.startsWith('append-'), isRead = ['read-eio', 'corrupt-snapshot'].includes(c.fault);
  if (isAppend || isRead) { A.send('seed-submit', 'submit_turn', params()); await a.waitLoop('run'); }
  const faultDiskBefore = fs.existsSync(path) ? fs.readFileSync(path, 'utf8') : null;
  if (c.fault === 'corrupt-snapshot') {
    fs.writeFileSync(path, '{"schemaVersion":1,"records":[');
    a.j.add('fs.fault', { op: 'corrupt-snapshot', path, text: fs.readFileSync(path, 'utf8') });
  } else if (c.fault.endsWith('read-eio') || c.fault === 'read-eio') a.tap.arm('readFile', p => p === path);
  else if (c.fault.includes('rename')) a.tap.arm('rename', (_p, args) => String(args[1]) === path,
    c.fault.endsWith('after') ? 'after' : 'before');
  else a.tap.arm('writeFile', p => p.startsWith(path + '.') && p.endsWith('.tmp'), c.fault.includes('partial') ? 'partial' : 'before');
  if (isRead) await A.request('fault-read', 'run_get', { sessionKey: 'session', runId: 'run' });
  else if (isAppend) {
    // At this barrier all prefix registry writes must be completed; run_events
    // flushes mutationChain before arming the actual terminal write operation.
    a.release('run'); await A.final('seed-submit');
  } else { A.send('fault-submit', 'submit_turn', params()); await A.final('fault-submit'); }
  a.j.barrier('fault:completed');
  assert.equal(a.tap.armed, null, 'registered fault must actually be exercised');
  const faultDiskAfter = fs.existsSync(path) ? fs.readFileSync(path, 'utf8') : null;
  const temporaryFiles = fs.readdirSync(a.dir).filter(n => n.endsWith('.tmp')).map(name => ({ name, text: fs.readFileSync(resolve(a.dir, name), 'utf8') }));
  if (c.fault === 'corrupt-snapshot') {
    assert.ok(faultDiskBefore); fs.writeFileSync(path, faultDiskBefore!);
    a.j.add('fixture.repair', { path, reason: 'restore only the intentionally corrupted fixture snapshot to probe read recovery', text: faultDiskBefore });
  }
  const recovery: any = { diskBefore: faultDiskBefore, diskAtFault: faultDiskAfter, temporaryFiles, queries: [] };
  for (const method of ['run_get', 'run_events', 'run_reattach']) recovery.queries.push(await A.request(`recovery-${method}`, method, { sessionKey: 'session', runId: 'run' }));
  if (isRead) { a.release('run'); await A.final('seed-submit'); }
  else if (!isAppend) {
    // A failure before rename permits a new admission; rename-after replays
    // the prior accepted record. Record both possibilities without assuming it.
    A.send('recovery-submit', 'submit_turn', params());
    const next = await a.j.wait(r => r.kind === 'loop.wait' || r.kind === 'wire.send' && JSON.parse(r.text).id === 'recovery-submit' && JSON.parse(r.text).final);
    if (next.kind === 'loop.wait') { a.release('run'); }
    await A.final('recovery-submit');
  }
  return { barriers: ['fault:completed', ...((isAppend || isRead) ? ['loop:run', 'release:run'] : [])], recovery };
}

async function measure(c: any, repetition: number, control?: string) {
  const dir = resolve(OUTPUT, control ? `controls/${control}` : `measurements/${c.id}/${repetition}`);
  const a = new Apparatus(dir, { kind: c.kind ?? (c.order === 'sdk-control' ? 'permission' : undefined) });
  let observation: any;
  try {
    let requiredBarriers: string[], extra: any = {};
    if (c.id.startsWith('dual/')) requiredBarriers = await dual(a, c, control === 'apparatus-omit-B-connection' ? 'omit-B' : undefined);
    else if (c.id.startsWith('multi/')) requiredBarriers = await multi(a, c);
    else if (c.id.startsWith('close/')) requiredBarriers = await closeRace(a, c, control === 'apparatus-old-binding-as-new' ? 'old-as-new' : undefined);
    else {
      const result = c.id.startsWith('store/session') ? await sessionFault(a, c) : await registryFault(a, c);
      requiredBarriers = result.barriers; extra = { fault: c.fault, recovery: result.recovery };
    }
    observation = await a.observe(c.id, repetition, c.id.startsWith('store/') ? ['A'] : ['A', 'B'], requiredBarriers, extra);
    fs.writeFileSync(resolve(dir, 'observation.json'), JSON.stringify(observation, null, 2));
    return observation;
  } catch (error) {
    fs.writeFileSync(resolve(dir, 'incomplete.json'), JSON.stringify({ caseId: c.id, error: errorData(error), timeline: a.j.rows }, null, 2)); throw error;
  } finally { await a.close(); }
}

// Whole apparatus mutations: perform actual reordering/direct calls, never
// reorder an already collected observer or tamper with its independent journal.
async function fifoControl(name: string, registry: boolean) {
  const a = new Apparatus(resolve(OUTPUT, 'controls', name));
  try {
    await a.peer('A');
    await a.router.getOrCreate({ sessionKey: 'session', channelKey: 'fixture' });
    if (registry) {
      await a.registry.accept({ sessionKey: 'session', runId: 'run', requestMaterial: 'fixture' });
      // Broken apparatus saves two calls before they enter the production FIFO
      // and dispatches them in reverse order, retaining independent admission.
      const inputs = ['one', 'two'].map(code => ({ sessionKey: 'session', runId: 'run', event: { type: 'agent_status', event: code } }));
      for (const i of inputs) a.j.add('registry.admit', { key: i.event.event });
      const realPersist = FileRunRegistry.prototype.persist.bind(a.registry);
      a.registry.persist = () => {
        const entries = a.registry.eventMap.get('\0session\0run');
        a.j.add('registry.enter', { key: entries.at(-1).event.event }); return realPersist();
      };
      await FileRunRegistry.prototype.append.call(a.registry, inputs[1]);
      await FileRunRegistry.prototype.append.call(a.registry, inputs[0]);
    } else {
      const s = a.sessions.get('session');
      const inputs = ['one', 'two'].map(event => ({ event, level: 'info', source: 'fixture', createdAt: TIME }));
      for (const i of inputs) a.j.add('status.admit', { session: 'session', key: i.event });
      // Direct store calls deliberately bypass JsonlTranscriptWriter.writeTail.
      await s.store.append('session', 'run', { type: 'agent_status_message', ...inputs[1] });
      await s.store.append('session', 'run', { type: 'agent_status_message', ...inputs[0] });
    }
    const observation = await a.observe(name, 1, ['A'], []);
    fs.writeFileSync(resolve(a.dir, 'observation.json'), JSON.stringify(observation, null, 2));
    checkObservation(observation);
  } finally { await a.close(); }
}

function signature(o: any) {
  const connectionNames = new Map(o.connections.map(c => [c.binding.connectionId, c.name]));
  const firstGeneration = Math.min(...o.connections.map(c => c.binding.generation));
  const clean = (v: any): any => {
    if (Array.isArray(v)) return v.map(clean);
    if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v)
      .filter(([k]) => !['acceptedAt', 'updatedAt', 'ownerPid', 'lastModified', 'errorId'].includes(k))
      .map(([k, x]) => [k, k === 'connectionId' ? connectionNames.get(x) ?? x : k === 'generation' ? Number(x) - firstGeneration
        : typeof x === 'string' ? x.replaceAll(OUTPUT, '<OUTPUT>').replaceAll(o.caseId + '/' + o.repetition, '<CASE>') : clean(x)]));
    return typeof v === 'string' ? v.replaceAll(OUTPUT, '<OUTPUT>').replaceAll(o.caseId + '/' + o.repetition, '<CASE>') : v;
  };
  return clean({ connections: o.connections.map(c => ({ name: c.name, frames: c.frames })),
    durable: o.durable.map(d => ({ session: d.session, entries: d.entries, invalid: d.invalid, diagnostics: d.fresh.diagnostics })),
    registry: { records: o.registry.records, events: o.registry.events }, answers: o.timeline.filter(r => r.kind === 'answer.result').map(r => ({ call: r.call, delivered: r.delivered })),
    aborts: o.timeline.filter(r => r.kind === 'abort.input').map(r => ({ input: r.input, activeRun: r.activeRun })) });
}
function summary(o: any) {
  return { caseId: o.caseId, connections: o.connections.map(c => ({ connection: c.name,
    events: c.frames.filter(f => f.type === 'event').map(f => `${f.id}/${f.seq}${f.final ? '/final' : ''}:${f.event.type}:${f.event.code ?? f.event.finishReason ?? ''}`),
    notifications: c.frames.filter(f => f.type === 'notification').map(f => `${f.name}:${f.payload?.type ?? ''}`),
    responses: c.frames.filter(f => f.type === 'response').map(f => ({ id: f.id, ok: f.ok, result: f.result, error: f.error })) })),
    durable: o.durable.map(d => ({ session: d.session, entries: d.entries.map(e => `${e.sequence}:${e.type}:${e.event ?? ''}`), invalid: d.invalid, diagnostics: d.fresh.diagnostics })),
    registry: o.registry.applicable ? o.registry.records.map(r => ({ ...r, requestMaterial: '(see raw)',
      events: o.registry.events[`${r.projectKey ?? ''}\0${r.sessionKey}\0${r.runId}`] })) : o.registry,
    answers: o.timeline.filter(r => r.kind === 'answer.result'), aborts: o.timeline.filter(r => r.kind === 'abort.input'),
    faults: o.timeline.filter(r => r.kind === 'fs.fault').map(r => ({ op: r.op, mode: r.mode })),
    evidence: relative(ROOT, resolve(OUTPUT, 'measurements', o.caseId, String(o.repetition), 'observation.json')) };
}
export async function runRecorded() {
const observations: any[] = [], summaries: any[] = [];
for (const c of CASES) {
  const runs: any[] = [];
  for (let repetition = 1; repetition <= REPEATS; repetition++) {
    const watchdog = setTimeout(() => { fs.writeFileSync(resolve(OUTPUT, 'watchdog.json'), JSON.stringify({ caseId: c.id, repetition })); process.exit(2); }, 20000);
    try { const o = await measure(c, repetition); checkObservation(o); observations.push(o); runs.push(o); }
    finally { clearTimeout(watchdog); }
  }
  const distinct = new Set(runs.map(o => JSON.stringify(signature(o)))).size;
  const row = { ...summary(runs[0]), runs: runs.length, distinct, stable: distinct === 1,
    alternatives: [...new Map(runs.map(o => [JSON.stringify(signature(o)), summary(o)])).values()] };
  summaries.push(row); console.log(JSON.stringify({ caseId: c.id, runs: runs.length, distinct }));
}
checkCoverage(observations);
const controls: any[] = [];
for (const c of CONTROLS) {
  let hit: string | undefined;
  try {
    if (c.name.includes('bypass-')) await fifoControl(c.name, c.name.includes('registry'));
    else if (c.name === 'apparatus-omit-B-connection') checkObservation(await measure(CASES[0], 1, c.name));
    else if (c.name === 'apparatus-old-binding-as-new') checkObservation(await measure(CASES.find(c => c.id === 'close/permission/after-disconnect'), 1, c.name));
    else {
      const source = c.name === 'attempt-as-success' ? observations.find(o => o.caseId === 'store/session/write-eio') : observations.find(o => o.caseId === 'dual/permission/owner-first');
      const bad = structuredClone(source);
      if (c.name === 'drop-frame' || c.name === 'both-sides-bad') bad.connections[0].frames.pop();
      if (c.name === 'omit-registry-events') Object.values(bad.registry.events)[0].pop();
      if (c.name === 'ignore-result-overwrite') bad.registry.records[0].result = { type: 'error', code: 'fake-prior-result' };
      if (c.name === 'double-answer-success') bad.timeline.find(r => r.kind === 'answer.result' && !r.delivered).delivered = true;
      if (c.name === 'attempt-as-success') {
        const fake = { type: 'agent_status_message', sessionId: 'session', turnId: 'run', sequence: 999, event: 'fault', createdAt: TIME };
        bad.durable[0].entries.push(fake); bad.durable[0].rawText += JSON.stringify(fake) + '\n';
        bad.durable[0].fresh.entries.push(fake);
      }
      const controlDirectory = resolve(OUTPUT, 'controls', c.name);
      fs.mkdirSync(controlDirectory, { recursive: true });
      fs.writeFileSync(resolve(controlDirectory, 'damaged-observer.json'), JSON.stringify({
        sourceCase: source.caseId, sourceRepetition: source.repetition, bad,
        ...(c.name === 'both-sides-bad' ? { identicalBadRight: structuredClone(bad) } : {}),
      }, null, 2));
      if (c.name === 'both-sides-bad') compareObservers(bad, structuredClone(bad)); else checkObservation(bad);
    }
  } catch (error) { hit = error.rule; }
  assert.equal(hit, c.rule, `${c.name} must reject with exact preregistered rule`);
  controls.push({ ...c, hitRule: hit, detected: true });
  const controlDirectory = resolve(OUTPUT, 'controls', c.name);
  fs.mkdirSync(controlDirectory, { recursive: true });
  fs.writeFileSync(resolve(controlDirectory, 'receipt.json'), JSON.stringify(controls.at(-1), null, 2));
}
const manifest = { baselineCommit: BASELINE_COMMIT, node: process.version, generatedAt: new Date().toISOString(),
  preregistrationSha256: sha(fs.readFileSync(preregPath)), additivePreregistrationSha256: sha(fs.readFileSync(resolve(ROOT, 'scripts/a5-19/preregistration-b-extra.mjs'))), preregistrationCommit: JSON.parse(process.env.A52B_PREREGISTRATION_STATUS!),
  expectedCases: CASES.map(c => c.id), executedCases: summaries.map(c => c.caseId), repeats: REPEATS,
  measurements: observations.length, summaries, unstableGroups: summaries.filter(s => !s.stable).map(s => s.caseId), controls,
  importedSources: verifySources(), fixtureFiles: ['preregistration-b.mjs', 'preregistration-b-extra.mjs', 'rules-b.mjs', 'runtime-b.mts', 'fixtures-b.mts', 'run-b.py']
    .map(name => ({ path: `scripts/a5-19/${name}`, sha256: sha(fs.readFileSync(resolve(ROOT, 'scripts/a5-19', name))) })),
  externalCases: { coldRestart: 'NOT_RUN: coordinator-only restart script', realWs: 'NOT_RUN: transport-neutral production dispatcher only', bridgeReconnect: 'NOT_RUN: separate D5 gate' } };
fs.writeFileSync(resolve(OUTPUT, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
console.log(`DONE ${observations.length} measurements, ${controls.length} controls; unstable=${manifest.unstableGroups.length}`);
}
if (process.env.A52B_LIBRARY_ONLY !== '1') await runRecorded();
