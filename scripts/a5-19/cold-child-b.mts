/** Coordinator-only real gateway PROCESS phases, no listeners. Never run here. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { resolve, dirname } from 'node:path';
import { Apparatus, JsonlSessionEventStore, OUTPUT, TIME, upstream, verifySources, sha, ROOT } from './runtime-b.mts';
assert.equal(process.env.A52B_COORDINATOR, '1', 'coordinator-only process restart fixture');
const phase = process.env.A52B_COLD_PHASE, state = process.env.A52B_COLD_STATE;
assert.ok(['prepare', 'read'].includes(phase!)); assert.ok(['running', 'unknown'].includes(state!));
const home = process.env.PILOT_HOME!;
const dir = resolve(home, 'gateway-data');
const a = new Apparatus(dir, { journalPath: resolve(OUTPUT, `${phase}-timeline.ndjson`) });
const rawPath = resolve(dir, 'session/transcript.jsonl'), registryPath = resolve(dir, 'registry.json');
const preregistrationSha256 = sha(fs.readFileSync(resolve(ROOT, 'scripts/a5-19/preregistration-b.mjs')));
const { readTranscript } = await upstream('session/transcript/TranscriptReader');
const { replayTranscriptEntries } = await upstream('session/transcript/TranscriptReplay');
const { readWebSessionMessages } = await upstream('web/server/readSessionMessages');
try {
  const A = await a.peer('A');
  if (phase === 'prepare') {
    A.send('submit', 'submit_turn', { sessionKey: 'session', channelKey: 'fixture', runId: 'run', message: 'cold restart held run' });
    await a.waitLoop('run');
    // A real status write ensures the session read plane has durable content
    // before the process is stopped. No fabricated snapshot is used.
    await a.gateway.recordAgentStatusMessage({ sessionKey: 'session', turnId: 'run', status: { event: 'cold_fixture_wait', kind: 'status', text: 'held', createdAt: TIME } });
    if (state === 'unknown') {
      await a.registry.append({ sessionKey: 'session', runId: 'run', event: { type: 'error', runId: 'run', code: 'result_unknown', message: 'cold fixture unknown fact', recoverable: false } });
      a.j.add('cold.unknown.source', { source: 'explicit real registry append; not a real sidecar unknown' });
    }
    const before = { phase, state, pid: process.pid, pilotHome: home, preregistrationSha256,
      registryRawText: fs.readFileSync(registryPath, 'utf8'), transcriptRawText: fs.readFileSync(rawPath, 'utf8'),
      queryRecord: await a.registry.get({ sessionKey: 'session', runId: 'run' }),
      connections: a.peers.map(p => p.observation()), importedSources: verifySources() };
    fs.writeFileSync(resolve(OUTPUT, 'before.json'), JSON.stringify(before, null, 2));
    process.stdout.write('A52B_READY\n');
    // Explicit parent stdin barrier. The coordinator kills this owned PID,
    // reaps it, then starts the read phase. Promise alone would let Node exit.
    process.stdin.resume(); await new Promise(() => undefined);
  } else {
    const freshStore = new JsonlSessionEventStore({ path: rawPath });
    const durableRead = await freshStore.read();
    const replay = replayTranscriptEntries(durableRead.entries);
    // Supply only the fixture path to the production read-only Web projector.
    // Gateway's resume_session itself is observed as its actual no-op RPC.
    a.gateway.options.readSessionMessages = (input: any) => readWebSessionMessages(input, {
      projectRoot: dir, pilotHome: home, now: () => new Date(TIME),
      storage: { chatDir: dirname(rawPath), transcriptPath: rawPath },
    });
    const responses: any[] = [];
    for (const method of ['run_get', 'run_events', 'run_reattach']) responses.push(await A.request(method, method, { sessionKey: 'session', runId: 'run' }));
    responses.push(await A.request('resume', 'resume_session', { sessionKey: 'session' }));
    responses.push(await A.request('history', 'read_session_messages', { sessionKey: 'session' }));
    responses.push(await A.request('snapshot', 'active_turn_snapshot', { sessionKey: 'session' }));
    const after = { phase, state, pid: process.pid, pilotHome: home, preregistrationSha256, responses,
      registryRawText: fs.readFileSync(registryPath, 'utf8'), transcriptRawText: fs.readFileSync(rawPath, 'utf8'),
      durableRead, replay, connections: a.peers.map(p => p.observation()), importedSources: verifySources() };
    fs.writeFileSync(resolve(OUTPUT, 'after.json'), JSON.stringify(after, null, 2));
    process.stdout.write('A52B_READ_DONE\n');
  }
} finally { await a.close(); }
