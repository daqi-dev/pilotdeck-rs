// #708 turn lifecycle matrix probe: TS expectations for the steer port and the
// interaction event port, one scenario per matrix cell. Direct imports of the
// unmodified cc7c4e6 baseline; deterministic Promise gates only, no sleeps.
// PDRS_BASELINE=<cc7c4e6> node --import <baseline>/node_modules/tsx/dist/loader.mjs generate.mts
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {readFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {bindSourceTree} from '../../baseline_source_guard.mjs';
assert.ok(process.env.PDRS_BASELINE);
const root = resolve(process.env.PDRS_BASELINE);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
bindSourceTree(root, commit, '/usr/bin/git');
const sources = [
  'src/agent/session/AgentSession.ts',
  'src/agent/session/SteerMailbox.ts',
  'src/agent/turn/TurnRunner.ts',
  'src/agent/loop/AgentLoop.ts',
  'src/agent/loop/AgentTurnCapabilities.ts',
  'src/agent/protocol/events.ts',
];
const [{AgentSession}, {SteerMailbox}, {TurnRunner}, {AgentLoop}, {createSidecarAgentTurnCapabilities}, {createAgentEventBuffer}] =
  await Promise.all(sources.map((p) => import(pathToFileURL(join(root, p)).href)));

const message = (id: string) => ({role: 'user', content: [], metadata: {queueItemId: id}});
const item = (id: string) => ({itemId: id, message: message(id), allowedReadFiles: ['/safe']});
const ids = (messages: any[]) => messages.map((m) => m.itemId);
const now = () => new Date('2026-10-25T00:00:00Z');

// A-S1 x E2/E3/E5: mailbox-level trace on a running turn.
const a1Mutations: any[] = [];
const a1 = new SteerMailbox({recordMutation: async (_t, m) => a1Mutations.push(m)});
const a1Trace: any[] = [];
a1.start('t');
a1Trace.push(await a1.enqueue('t', item('a')));
a1Trace.push(await a1.enqueue('t', item('a')));
a1Trace.push(await a1.enqueue('other', item('x')));
a1Trace.push(await a1.cancel('t', 'b'));
a1Trace.push(await a1.enqueue('t', item('b')));
a1Trace.push({drained: ids(a1.drain('t'))});
a1Trace.push(await a1.cancel('t', 'a'));
await a1.claim('t', 'a');
a1.ack('t', 'a');
a1Trace.push(a1.drainOrClose('t'));
a1Trace.push(await a1.enqueue('t', item('c')));
a1Trace.push({finished: ids(a1.finish('t'))});
a1Trace.push(await a1.enqueue('t', item('c')));

// A-S2 x E2/E4: the abort gate lives on AgentSession, never in the mailbox or
// the apply chain. After abort the session rejects steer, but a claim already
// drained by the loop still records and the wrong-turn enqueue is turn_mismatch.
const a2Mutations: any[] = [];
const a2Trace: any[] = [];
let a2Session: any;
const a2Runner = {
  sessionEventRecorder: {recordInboxMutation: async (_s: string, t: string, m: any) => a2Mutations.push({turn: t, ...m})},
  async *run(p: any) {
    p.openSteerMailbox();
    a2Trace.push(await a2Session.steer({turnId: 't', ...item('a')}));
    a2Trace.push(await a2Session.steer({turnId: 'wrong', ...item('y')}));
    a2Session.abort('probe');
    a2Trace.push(await a2Session.steer({turnId: 't', ...item('late')}));
    a2Trace.push({drained: ids(p.drainSteerMessages())});
    await p.claimSteerMessage('a');
    a2Trace.push({claimed: true});
    return {messages: [], result: {type: 'aborted', usage: {}, permissionDenials: []}};
  },
};
a2Session = new AgentSession({sessionId: 's', turnRunner: a2Runner as any});
for await (const _ of a2Session.submit({type: 'text', text: 'start'}, {turnId: 't'})) {}

// A-S2 x E4 (pre-durable half): a real TurnRunner + AgentLoop, abort injected
// between drain and apply. TS applies the drained steer anyway: claim ->
// durable -> ack all complete and steer_applied is yielded before the aborted
// terminal, with zero model calls.
const a2ApplyMutations: any[] = [];
const a2ApplyTrace: any[] = [];
const a2ApplyEvents: string[] = [];
const a2Controller = new AbortController();
const a2Mailbox = new SteerMailbox({recordMutation: async (_t, m) => a2ApplyMutations.push(m)});
a2Mailbox.start('t');
await a2Mailbox.enqueue('t', item('a'));
let a2ModelCalls = 0;
const a2Model = new Proxy({}, {get(_target: any, key: string) {
  if (key === 'then') return undefined;
  return () => {a2ModelCalls++; throw new Error(`unexpected model call: ${key}`);};
}});
const a2Capabilities = createSidecarAgentTurnCapabilities(
  {cwd: process.cwd(), runMode: 'agent'},
  {ports: {model: a2Model, toolExecution: {list: () => [], executeAll: async () => []}}, now},
);
const a2Loop = new AgentLoop({cwd: process.cwd(), runMode: 'agent'}, a2Capabilities as any);
const a2Transcript = {
  recordSessionEvent() {},
  recordAcceptedInput() {},
  recordTurnResult() {},
  async recordDurableMessage(_s: string, _t: string, m: any) {
    assert.equal(m.metadata?.queueItemId, 'a');
    a2ApplyTrace.push(['durable', 'a']);
  },
};
const a2Runner2 = new TurnRunner(a2Loop as any, a2Transcript as any, undefined, now, undefined,
  {cwd: process.cwd(), transcriptPath: '', collectFileArtifacts: false});
for await (const event of a2Runner2.run({
  sessionId: 's', turnId: 't', messages: [], input: {type: 'text', text: 'start'}, abortSignal: a2Controller.signal,
  drainSteerMessages: () => {a2Controller.abort('probe'); return a2Mailbox.drain('t');},
  drainOrCloseSteerMailbox: () => a2Mailbox.drainOrClose('t'),
  claimSteerMessage: (id: string) => a2Mailbox.claim('t', id),
  ackSteerMessage(id: string) {a2ApplyTrace.push(['ack', id, a2Controller.signal.aborted]); a2Mailbox.ack('t', id);},
  async closeSteerMailbox() {return a2Mailbox.close('t');},
}) as any) {
  a2ApplyEvents.push(event.type);
}
assert.ok(a2ApplyEvents.includes('steer_applied'), JSON.stringify({a2ApplyEvents, a2ApplyTrace}));
assert.equal(a2ModelCalls, 0);

// A-S3 x E1/E2: the registered same-ID restart race. An insert that is still
// waiting for its recorder write when finish+start runs is accepted by TS and
// the new turn drains the old message (A6.9-STEER-RUN-001, not a Rust expectation).
let a3Release: () => void;
let a3Entered: () => void;
const a3Gate = new Promise<void>((r) => {a3Release = r;});
const a3Started = new Promise<void>((r) => {a3Entered = r;});
const a3Mailbox = new SteerMailbox({recordMutation: async () => {a3Entered(); await a3Gate;}});
a3Mailbox.start('same');
const a3Old = a3Mailbox.enqueue('same', item('a'));
await a3Started;
a3Mailbox.finish('same');
a3Mailbox.start('same');
a3Release();
const a3Difference = {accepted: await a3Old, leaked: ids(a3Mailbox.drain('same'))};

// A-S4/S5 x E1/E2/E5: two sequential submits on one session through a REAL
// TurnRunner (stub loop only). The undrained steer of turn 1 is discarded by
// TurnRunner's closeSteerMailbox before AgentSession's finish; turn 2 starts
// with a clean mailbox, the old turnId no longer matches, and between turns
// steer reports no_active_turn.
const a4Mutations: any[] = [];
const a4Events: string[][] = [];
const a4Summary: any[] = [];
const a4SteerAfter: any[] = [];
let a4Session: any;
const a4Loop = {
  async *run(input: any) {
    const turn = a4Turn;
    const steer = await a4Session.steer({turnId: turn, ...item(turn === 't1' ? 'a' : 'c')});
    a4Summary.push({turn, steer});
    if (turn === 't2') {
      a4Summary.push({drained: ids(input.drainSteerMessages?.() ?? [])});
      a4Summary.push(await a4Session.steer({turnId: 't1', ...item('old')}));
    }
    yield {type: 'turn_completed', sessionId: 's', turnId: turn, result: {type: 'success', sessionId: 's', turnId: turn, usage: {}, permissionDenials: [], turns: 1}};
    return {result: {type: 'success', sessionId: 's', turnId: turn, usage: {}, permissionDenials: [], turns: 1}, messages: []};
  },
  snapshotFileState: () => ({allowedReadFiles: []}),
};
let a4Turn = 't1';
const a4Runner = new TurnRunner(a4Loop as any, {
  recordSessionEvent() {},
  recordAcceptedInput() {},
  recordTurnResult() {},
  async recordDurableMessage() {throw new Error('unapplied steers must not reach the durable transcript');},
} as any, undefined, now, undefined, {cwd: process.cwd(), transcriptPath: '', collectFileArtifacts: false});
a4Runner.sessionEventRecorder.recordInboxMutation = async (_s: string, t: string, m: any) => a4Mutations.push({turn: t, ...m});
a4Session = new AgentSession({sessionId: 's', turnRunner: a4Runner as any});
for await (const event of a4Session.submit({type: 'text', text: 'one'}, {turnId: 't1'})) {
  a4Events[0] = [...(a4Events[0] ?? []), event.type];
  if (event.type === 'steer_unapplied') a4Events[0].push(`steer_unapplied:${event.itemId}`);
}
a4Turn = 't2';
a4SteerAfter.push(await a4Session.steer({turnId: 't2', ...item('between')}));
for await (const event of a4Session.submit({type: 'text', text: 'two'}, {turnId: 't2'})) {
  a4Events[1] = [...(a4Events[1] ?? []), event.type];
  if (event.type === 'steer_unapplied') a4Events[1].push(`steer_unapplied:${event.itemId}`);
}
a4SteerAfter.push(await a4Session.steer({turnId: 't2', ...item('after')}));

// A-S1 x E6 (consumer drops the stream mid-run): the consumer stops iterating
// at the first loop event. TurnRunner's finally still runs closeSteerMailbox
// (TurnRunner.ts:442-446), so the drained-but-unapplied steers get durable
// discard records; the steer_unapplied events themselves are never observed by
// the dropped consumer.
const a6Run = async () => {
  const mutations: any[] = [];
  const closeCalls: string[][] = [];
  const events: string[] = [];
  const mailbox = new SteerMailbox({recordMutation: async (_t, m) => mutations.push(m)});
  const loop = {
    async *run(input: any) {
      assert.equal((await mailbox.enqueue('t', item('a'))).accepted, true);
      assert.equal((await mailbox.enqueue('t', item('b'))).accepted, true);
      input.drainSteerMessages?.();
      yield {type: 'model_event', sessionId: 's', turnId: 't'};
      yield {type: 'turn_completed', sessionId: 's', turnId: 't', result: {type: 'success', sessionId: 's', turnId: 't', usage: {}, permissionDenials: [], turns: 1}};
      return {result: {type: 'success', sessionId: 's', turnId: 't', usage: {}, permissionDenials: [], turns: 1}, messages: []};
    },
    snapshotFileState: () => ({allowedReadFiles: []}),
  };
  const runner = new TurnRunner(loop as any, {
    recordSessionEvent() {},
    recordAcceptedInput() {},
    recordTurnResult() {},
    async recordDurableMessage() {throw new Error('unapplied steers must not reach the durable transcript');},
  } as any, undefined, now, undefined, {cwd: process.cwd(), transcriptPath: '', collectFileArtifacts: false});
  for await (const event of runner.run({
    sessionId: 's', turnId: 't', messages: [], input: {type: 'text', text: 'start'},
    openSteerMailbox: () => mailbox.start('t'),
    drainSteerMessages: () => mailbox.drain('t'),
    drainOrCloseSteerMailbox: () => mailbox.drainOrClose('t'),
    claimSteerMessage: (id: string) => mailbox.claim('t', id),
    ackSteerMessage: (id: string) => mailbox.ack('t', id),
    async closeSteerMailbox() {const d = await mailbox.close('t'); closeCalls.push(ids(d)); return d;},
  } as any)) {
    events.push(event.type);
    if (event.type === 'model_event') break;
  }
  return {events, mutations, closeCalls};
};
const a6 = await a6Run();

// B-S1 x E3: the host event buffer keeps arrival order, drain empties it, and a
// repeated drain returns nothing new (createAgentEventBuffer, events.ts:158).
const b2Buffer = createAgentEventBuffer();
const b2First = ['e1', 'e2', 'e3'].map((type) => ({type, sessionId: 's', turnId: 't'}));
for (const event of b2First) b2Buffer.emitter(event);
const b2Drained = b2Buffer.drain();
const b2Redrained = b2Buffer.drain();
const b2Late = {type: 'e4', sessionId: 's', turnId: 't'};
b2Buffer.emitter(b2Late);
const b2LateDrained = b2Buffer.drain();

// B-S5 x E4 (tail event order): on the success path TurnRunner yields the
// unapplied steers (drained-but-unacknowledged plus close-discarded) before
// turn_completed; on the error path turn_failed comes first, then the unapplied
// steers, then turn_completed (TurnRunner.ts:408-441).
const b5Run = async (fail: boolean) => {
  const mutations: any[] = [];
  const events: string[] = [];
  const mailbox = new SteerMailbox({recordMutation: async (_t, m) => mutations.push(m)});
  const loop = {
    async *run(input: any) {
      assert.equal((await mailbox.enqueue('t', item('a'))).accepted, true);
      assert.equal((await mailbox.enqueue('t', item('b'))).accepted, true);
      input.drainSteerMessages?.();
      if (fail) throw new Error('loop failed');
      yield {type: 'turn_completed', sessionId: 's', turnId: 't', result: {type: 'success', sessionId: 's', turnId: 't', usage: {}, permissionDenials: [], turns: 1}};
      return {result: {type: 'success', sessionId: 's', turnId: 't', usage: {}, permissionDenials: [], turns: 1}, messages: []};
    },
    snapshotFileState: () => ({allowedReadFiles: []}),
  };
  const runner = new TurnRunner(loop as any, {
    recordSessionEvent() {},
    recordAcceptedInput() {},
    recordTurnResult() {},
    async recordDurableMessage() {throw new Error('unapplied steers must not reach the durable transcript');},
  } as any, undefined, now, undefined, {cwd: process.cwd(), transcriptPath: '', collectFileArtifacts: false});
  for await (const event of runner.run({
    sessionId: 's', turnId: 't', messages: [], input: {type: 'text', text: 'start'},
    openSteerMailbox: () => mailbox.start('t'),
    drainSteerMessages: () => mailbox.drain('t'),
    drainOrCloseSteerMailbox: () => mailbox.drainOrClose('t'),
    claimSteerMessage: (id: string) => mailbox.claim('t', id),
    ackSteerMessage: (id: string) => mailbox.ack('t', id),
    async closeSteerMailbox() {return mailbox.close('t');},
  } as any)) {
    events.push(event.type);
    if (event.type === 'steer_unapplied') events.push(`steer_unapplied:${event.itemId}`);
  }
  return {events, mutations};
};
const b5Success = await b5Run(false);
const b5Error = await b5Run(true);

bindSourceTree(root, commit, '/usr/bin/git');
console.log(JSON.stringify({
  commit,
  sourceSha256: Object.fromEntries(sources.map((p) => [p, createHash('sha256').update(readFileSync(join(root, p))).digest('hex')])),
  A1_running_mailbox: {trace: a1Trace, mutations: a1Mutations},
  A2_abort_gate: {trace: a2Trace, mutations: a2Mutations},
  A2_apply_after_abort: {trace: a2ApplyTrace, events: a2ApplyEvents, mutations: a2ApplyMutations, modelCalls: a2ModelCalls},
  A3_barrier_race: a3Difference,
  A4_sequential: {summary: a4Summary, events: a4Events, between: a4SteerAfter, mutations: a4Mutations},
  A6_consumer_break: a6,
  B2_event_buffer: {drained: b2Drained, redrained: b2Redrained, lateDrained: b2LateDrained},
  B5_unapplied_order: {success: b5Success, error: b5Error},
}, null, 2));
