/** Round 2: unchanged production cleanup, with synchronous observation only. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { resolve, relative } from 'node:path';
import { Apparatus, InProcessGateway, OUTPUT, ROOT, sha, errorData, verifySources } from './runtime-b.mts';
import { CASES, REPEATS, REQUIREMENTS } from './preregistration-b-production.mjs';
import { checkObservation } from './rules-b.mjs';

const fixtureNames = ['production-close-b.mts', 'preregistration-b-production.mjs', 'run-production-b.py',
  'runtime-b.mts', 'rules-b.mjs', 'preregistration-b.mjs', 'preregistration-b-extra.mjs', 'run-b.py'];
const hashes = () => fixtureNames.map(name => ({ path: `scripts/a5-19/${name}`,
  sha256: sha(fs.readFileSync(resolve(ROOT, 'scripts/a5-19', name))) }));
fs.mkdirSync(OUTPUT, { recursive: true });
const fixtureFiles = hashes();
fs.writeFileSync(resolve(OUTPUT, 'preregistration.json'), JSON.stringify({ cases: CASES, repeats: REPEATS,
  requirements: REQUIREMENTS, fixtureFiles, commitStatus: JSON.parse(process.env.A52B_PRODUCTION_REGISTRATION!) }, null, 2));
const params = (sessionKey = 'session', runId = 'run') => ({ sessionKey, runId, channelKey: 'fixture', message: 'controlled fixture' });
const rows = (a: Apparatus, kind: string) => a.j.rows.filter(r => r.kind === kind);
const inflight = (a: Apparatus) => a.peers.map(p => ({ connection: p.name, sessions: [...p.conn.inFlightSessions] }));

function instrumentation(a: Apparatus) {
  // Remove even the existing transparent submit journal wrapper: the generator
  // under test is the exact production method, with no additional tail await.
  a.gateway.submitTurn = InProcessGateway.prototype.submitTurn;
  const disconnect = InProcessGateway.prototype.disconnectInteraction;
  a.gateway.disconnectInteraction = function(input: any) {
    const result = disconnect.call(this, input);
    assert.ok(!(result instanceof Promise), 'disconnect must stay synchronous');
    a.j.add('disconnect.result', { input, result, inFlight: inflight(a), activeRun: a.router.activeTurnRunId(input.sessionKey) });
    return result;
  };
  const begin = a.router.beginTurn.bind(a.router);
  a.router.beginTurn = (session: string, run: string) => {
    const accepted = begin(session, run);
    a.j.add('router.begin', { session, run, accepted, inFlight: inflight(a) }); return accepted;
  };
  const abort = a.router.abort.bind(a.router);
  a.router.abort = (session: string, reason: string) => {
    a.j.add('router.abort', { session, reason, activeRun: a.router.activeTurnRunId(session), inFlight: inflight(a) });
    return abort(session, reason);
  };
}
function observeWsCleanup(a: Apparatus, p: any) {
  const sessions = p.conn.inFlightSessions;
  const remove = sessions.delete.bind(sessions);
  sessions.delete = (session: string) => {
    const deleted = remove(session);
    a.j.add('wire.inflight.delete', { connection: p.name, session, deleted, inFlight: inflight(a) });
    return deleted;
  };
}
function send(a: Apparatus, p: any, id: string, session: string, run: string) {
  const input = params(session, run); a.j.add('submit.input', input); p.send(id, 'submit_turn', input);
}
async function reconnect(B: any, A: any) {
  await B.request('reconnect', 'reconnect_interaction', { sessionKey: 'session', previousBinding: A.binding });
  await B.request('resume', 'resume_session', { sessionKey: 'session' });
}
async function newTurn(a: Apparatus, c: any) {
  const A = await a.peer('A'), B = await a.peer('B');
  observeWsCleanup(a, A); observeWsCleanup(a, B);
  const multiple = c.scope === 'multi';
  if (multiple) { send(a, A, 'submit-first', 'first-session', 'first-run'); await a.waitLoop('first-run'); }
  send(a, A, 'submit-A', 'session', 'run'); await a.waitLoop('run');
  await reconnect(B, A);
  let launched = false;
  const start = () => {
    assert.equal(launched, false); launched = true;
    a.j.barrier('B-start'); send(a, B, 'submit-B', 'session', 'new-run');
  };
  const end = a.router.endTurn.bind(a.router);
  a.router.endTurn = (session: string, run: string) => {
    end(session, run);
    a.j.add('router.end', { session, run, inFlight: inflight(a) });
    if (session === 'session' && run === 'run') {
      a.j.barrier('old-end-turn');
      if (c.start === 'end-turn') start();
      if (!multiple) A.close();
    }
  };
  if (c.start === 'terminal-commit') {
    const add = a.j.add.bind(a.j);
    a.j.add = (kind: string, data: any = {}) => {
      const row = add(kind, data);
      if (!launched && kind === 'fs.rename.commit' && data.to === resolve(a.dir, 'registry.json')
        && JSON.parse(data.text).records.some(r => r.runId === 'run' && r.state === 'completed')) {
        a.j.barrier('old-terminal-commit'); start();
      }
      return row;
    };
  }
  if (multiple) {
    A.close();
    await a.j.wait(r => r.kind === 'abort.input' && r.input.sessionKey === 'first-session');
    a.j.barrier('first-abort-draining');
  }
  a.release('run'); await A.final('submit-A');
  assert.ok(launched);
  // A genuine busy response is an observation too, never an implicit skip.
  const arrival = await a.j.wait(r => r.kind === 'loop.wait' && r.run === 'new-run'
    || r.kind === 'wire.send' && r.connection === 'B' && JSON.parse(r.text).id === 'submit-B' && JSON.parse(r.text).final);
  if (arrival.kind === 'loop.wait') {
    await a.waitLoop('new-run');
    a.j.add('new-run.live', { inFlight: inflight(a) });
  }
  if (multiple) {
    a.release('first-run'); await A.final('submit-first');
  }
  await a.j.wait(r => r.kind === 'wire.close.done' && r.connection === 'A');
  a.j.barrier('old-close-done');
  if (arrival.kind === 'loop.wait') a.release('new-run');
  await B.final('submit-B');
  return ['loop:run', 'release:run', 'B-start', 'old-end-turn', 'old-close-done',
    ...(c.start === 'terminal-commit' ? ['old-terminal-commit'] : []),
    ...(multiple ? ['loop:first-run', 'first-abort-draining', 'release:first-run'] : [])];
}
async function pending(a: Apparatus, c: any) {
  const A = await a.peer('A'), B = await a.peer('B');
  observeWsCleanup(a, A); observeWsCleanup(a, B);
  send(a, A, 'submit-A', 'session', 'run'); await a.waitLoop('run');
  if (c.order === 'reconnect-first') { await reconnect(B, A); A.close(); }
  else { A.close(); await reconnect(B, A); }
  await a.j.wait(r => r.kind === 'disconnect.result');
  for (const [p, id] of [[A, 'old-answer'], [B, 'new-answer']] as any[]) {
    await p.request(id, c.kind === 'permission' ? 'permission_decide' : 'elicitation_respond',
      { sessionKey: 'session', requestId: `run-${c.kind}`, ...(c.kind === 'permission'
        ? { decision: 'allow' } : { answer: { action: 'accept', content: { answer: 'fixture' } } }) });
  }
  a.release('run'); await A.final('submit-A');
  await a.j.wait(r => r.kind === 'wire.close.done' && r.connection === 'A');
  a.j.barrier('old-close-done');
  return ['loop:run', 'release:run', 'old-close-done'];
}

const summaries: any[] = [], evidence: any[] = [];
for (const c of CASES) {
  const outcomes: any[] = [];
  for (let repetition = 1; repetition <= REPEATS; repetition++) {
    const dir = resolve(OUTPUT, 'measurements', c.id, String(repetition));
    const a = new Apparatus(dir, { kind: c.kind }); instrumentation(a);
    const watchdog = setTimeout(() => {
      fs.writeFileSync(resolve(dir, 'watchdog.json'), JSON.stringify({ caseId: c.id, timeline: a.j.rows })); process.exit(2);
    }, 20000);
    try {
      const requiredBarriers = c.kind ? await pending(a, c) : await newTurn(a, c);
      await a.peers[1].request('final-events', 'run_events', { sessionKey: 'session', runId: c.kind ? 'run' : 'new-run' });
      assert.equal(a.gateway.submitTurn, InProcessGateway.prototype.submitTurn);
      const observation = await a.observe(c.id, repetition, ['A', 'B'], requiredBarriers,
        { productionTiming: { submitTurnIsProductionMethod: true, disconnectIsSynchronous: true, artificialCleanupYields: [] } });
      checkObservation(observation);
      const path = resolve(dir, 'observation.json'); fs.writeFileSync(path, JSON.stringify(observation, null, 2));
      const outcome = { repetition, aborts: rows(a, 'abort.input').map(r => ({ session: r.input.sessionKey, activeRun: r.activeRun ?? null })),
        routerAborts: rows(a, 'router.abort').map(r => ({ session: r.session, activeRun: r.activeRun ?? null })),
        disconnects: rows(a, 'disconnect.result').map(r => ({ session: r.input.sessionKey, result: r.result })),
        answers: rows(a, 'answer.result').map(r => r.delivered),
        newRunEntered: rows(a, 'loop.wait').some(r => r.run === 'new-run'),
        newRunAborted: rows(a, 'loop.abort').some(r => r.run === 'new-run'),
        newRunHit: rows(a, 'abort.input').some(r => r.activeRun === 'new-run'),
        endTurns: rows(a, 'router.end').map(r => ({ order: r.order, session: r.session, run: r.run })),
        newRunLive: rows(a, 'new-run.live').map(r => ({ order: r.order, inFlight: r.inFlight })),
        wsDeletes: rows(a, 'wire.inflight.delete').map(r => ({ order: r.order, connection: r.connection, session: r.session, deleted: r.deleted })),
        observation: relative(ROOT, path) };
      outcomes.push(outcome);
      const refs = [path, resolve(dir, 'timeline.ndjson'), ...observation.durable.map(d => d.path), observation.registry.path];
      evidence.push({ caseId: c.id, repetition, files: refs.map(p => ({ path: relative(ROOT, p), sha256: sha(fs.readFileSync(p)) })) });
    } catch (error) {
      fs.writeFileSync(resolve(dir, 'incomplete.json'), JSON.stringify({ caseId: c.id, error: errorData(error), timeline: a.j.rows }, null, 2)); throw error;
    } finally { clearTimeout(watchdog); await a.close(); }
  }
  const distinctOutcomes = new Set(outcomes.map(({ repetition, observation, endTurns, newRunLive, wsDeletes, ...outcome }) => JSON.stringify(outcome))).size;
  summaries.push({ caseId: c.id, runs: outcomes.length, distinctOutcomes, outcomes });
  console.log(JSON.stringify({ caseId: c.id, runs: outcomes.length, distinctOutcomes, newRunHits: outcomes.filter(o => o.newRunHit).length }));
}
assert.deepEqual(hashes(), fixtureFiles, 'apparatus changed during collection');
assert.equal(evidence.length, CASES.length * REPEATS);
fs.writeFileSync(resolve(OUTPUT, 'manifest.json'), JSON.stringify({ suite: 'A5.2b round 2 production close',
  baselineCommit: 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', node: process.version,
  preregistration: JSON.parse(process.env.A52B_PRODUCTION_REGISTRATION!), fixtureFiles, requirements: REQUIREMENTS,
  expectedCases: CASES.map(c => c.id), executedCases: summaries.map(s => s.caseId), repeats: REPEATS,
  measurements: evidence.length, summaries, evidence, importedSources: verifySources() }, null, 2));
