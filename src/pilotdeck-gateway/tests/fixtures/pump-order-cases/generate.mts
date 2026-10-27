// Real cc7c4e6 session, TurnRunner and stdio sidecar; only the start write is gated.
import assert from 'node:assert/strict';
import {mkdirSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0');
const root = resolve(process.env.PDRS_BASELINE!), out = resolve(process.argv[2]);
const sourceCommit = assertBaseline(root);
const sourceSha256 = await baselineSourceHashes(root, ['src/session/transcript/InMemoryTranscriptWriter.ts', 'src/permission/index.ts', 'src/cli/pilotdeck-agent-loop-sidecar.ts',
  'src/agent/session/createAgentSession.ts',
  'src/agent/session/AgentSession.ts',
  'src/agent/session/AgentSessionEventRecorder.ts',
  'src/agent/turn/TurnRunner.ts',
  'src/agent/loop/AgentLoop.ts',
  'src/agent/modules/transport/agentLoopSidecarClient.ts',
  'src/agent/modules/transport/stdioAgentLoopSidecarConnection.ts',
]);
const load = (path: string) => import(pathToFileURL(join(root, 'src', path)).href);
const {createAgentSessionWithStorage} = await load('agent/session/createAgentSession.ts');
const {createAgentLoopSidecarRuntimeFactory} = await load('agent/modules/transport/agentLoopSidecarClient.ts');
const {createStdioAgentLoopSidecarConnectionFactory} = await load('agent/modules/transport/stdioAgentLoopSidecarConnection.ts');
const {InMemoryTranscriptWriter} = await load('session/transcript/InMemoryTranscriptWriter.ts');
const {createDefaultPermissionContext} = await load('permission/index.ts');
const gate = () => {
  let resolve!: () => void;
  const promise = new Promise<void>(r => { resolve = r; });
  return {promise, resolve};
};
mkdirSync(join(out, 'home'), {recursive: true});
const cases = [];
for (const name of ['reject', 'cancel', 'open']) {
  const entered = gate(), release = gate(), events: any[] = [], wire: any[] = [];
  const counts = {connect: 0, model: 0};
  const transcript = new InMemoryTranscriptWriter();
  const connect = createStdioAgentLoopSidecarConnectionFactory({
    command: process.execPath,
    args: ['--import', join(root, 'node_modules/tsx/dist/loader.mjs'), join(root, 'src/cli/pilotdeck-agent-loop-sidecar.ts')],
    cwd: out, env: {PATH: process.env.PATH, HOME: join(out, 'home')}, killTimeoutMs: 1000,
  });
  const factory = createAgentLoopSidecarRuntimeFactory({connect: async (input: any) => {
    counts.connect++;
    const connection = await connect(input);
    return {
      send: (frame: any) => connection.send(frame), close: (reason: any) => connection.close(reason),
      async *receive() { for await (const frame of connection.receive()) { wire.push(frame); yield frame; } },
    };
  }});
  const model = {
    async prepare({request}: any) { return {request, provider: request.provider, model: request.model}; },
    async *stream() {
      counts.model++;
      yield {type: 'request_started', provider: 'test', model: 'deterministic'};
      yield {type: 'message_start', role: 'assistant'};
      yield {type: 'text_delta', text: 'hello'};
      yield {type: 'message_end', finishReason: 'stop'};
    },
  };
  const {session, handle} = createAgentSessionWithStorage({
    sessionId: 'session', collectFileArtifacts: false, transcript,
    config: {provider: 'test', model: 'deterministic', cwd: out, permissionMode: 'default',
      permissionContext: createDefaultPermissionContext({cwd: out, mode: 'default', canPrompt: false})},
    dependencies: {router: {}, ports: {model, tools: {list: () => [], executeAll: async () => []}},
      tools: {registry: {list: () => []}, scheduler: {executeAll: async () => []}}},
    agentLoopFactory: factory,
  });
  const recorder = (session as any).options.turnRunner.sessionEventRecorder;
  const start = recorder.startTurn.bind(recorder);
  recorder.startTurn = async (...args: any[]) => {
    if (name === 'reject') throw new Error('injected start failure');
    entered.resolve();
    await release.promise;
    return start(...args);
  };
  // Never leave a parked predecessor or child alive: release in finally, with
  // a hard outer deadline if the unchanged implementation stops making progress.
  const timer = setTimeout(() => { console.error(`timeout ${name}`); process.exit(1); }, 20000);
  const stream = handle.submit({type: 'text', text: 'probe'}, {turnId: 'turn'});
  let error: string | null = null, held: any = null, cancelledWhileHeld: any = null;
  let gateOpened = name === 'reject';
  try {
    for (let i = 0; i < 100; i++) {
      let settled = false;
      const pending = stream.next().then((item: any) => { settled = true; return item; });
      if (!gateOpened) {
        const progress = await Promise.race([pending.then(item => ({item})), entered.promise.then(() => ({gate: true}))]);
        if ('item' in progress) {
          if (progress.item.done) break;
          events.push(progress.item.value);
          continue;
        }
        // A complete pump interval passes while the actual start predecessor
        // remains unresolved; no provider or sidecar can have started yet.
        await new Promise(r => setTimeout(r, 550));
        held = {pending: !settled, ...counts};
        assert.deepEqual(held, {pending: true, connect: 0, model: 0});
        if (name === 'cancel') {
          session.abort('user-stop');
          await new Promise(r => setImmediate(r));
          cancelledWhileHeld = {pending: !settled, ...counts};
          assert.deepEqual(cancelledWhileHeld, held);
        }
        release.resolve();
        gateOpened = true;
      }
      const item = await pending;
      if (item.done) break;
      events.push(item.value);
      assert.ok(i < 99, 'stream must terminate within the bounded event count');
    }
  } catch (failure: any) { error = failure.message; }
  finally { release.resolve(); await stream.return(); await handle.dispose(); clearTimeout(timer); }
  const types = events.map(event => event.type);
  // The Rust live-payload peer deliberately emits only this subsequence of
  // the real stream; preserve the complete trace separately, without hiding it.
  const projection = types.filter(type => ['turn_started', 'input_accepted', 'user_prompt_submitted', 'model_request_started', 'turn_completed'].includes(type));
  const terminal = events.find(event => event.type === 'turn_completed')?.result.type ?? null;
  const row = {name, held, cancelledWhileHeld, counts, projection, terminal, error};
  if (name === 'open') {
    assert.equal(error, null);
    assert.equal(terminal, 'success');
    assert.ok(types.indexOf('model_request_started') >= 0);
    assert.ok(types.indexOf('model_request_started') < types.indexOf('turn_completed'));
  }
  writeFileSync(join(out, `${name}-raw.json`), JSON.stringify({node: process.version, row, types, events, wire, entries: transcript.entries}, null, 2) + '\n', {flag: 'wx'});
  console.log(JSON.stringify({node: process.version, row, types}));
  cases.push(row);
}
assertBaseline(root);
writeFileSync(join(out, 'oracle.json'), JSON.stringify({sourceCommit, sourceSha256, cases}, null, 2) + '\n', {flag: 'wx'});
