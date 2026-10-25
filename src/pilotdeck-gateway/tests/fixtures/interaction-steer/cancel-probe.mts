// Real baseline abort/drain and claim -> durable -> abort -> ack boundaries.
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
const sources = ['src/agent/session/AgentSession.ts', 'src/agent/session/SteerMailbox.ts',
  'src/agent/turn/TurnRunner.ts', 'src/agent/loop/AgentLoop.ts', 'src/agent/loop/AgentTurnCapabilities.ts'];
const [{AgentSession}, {SteerMailbox}, {TurnRunner}, {AgentLoop}, {createSidecarAgentTurnCapabilities}] =
  await Promise.all(sources.map(p => import(pathToFileURL(join(root, p)).href)));
const item = {itemId:'a', message:{role:'user',content:[],metadata:{queueItemId:'a'}}, allowedReadFiles:['/safe']};
const ids = (messages:any[]) => messages.map(m => m.itemId);
const abortDrain = [];
for (const operation of ['drain', 'drainOrClose']) {
  let observed;
  let session;
  const runner = {
    sessionEventRecorder:{recordInboxMutation:async()=>{}},
    async *run(input) {
      input.openSteerMailbox();
      assert.deepEqual(await session.steer({turnId:'t',...item}), {accepted:true});
      session.abort('probe');
      assert.equal(input.abortSignal.aborted, true);
      const first = operation === 'drain' ? {messages:input.drainSteerMessages()} : input.drainOrCloseSteerMailbox();
      const terminal = input.drainOrCloseSteerMailbox();
      observed = {operation, first, terminal};
      assert.deepEqual(ids(first.messages), ['a']);
      if (operation === 'drainOrClose') assert.equal(first.closed, false);
      assert.deepEqual(terminal, {messages:[],closed:true});
      await input.closeSteerMailbox();
      return {messages:[],result:{type:'aborted',usage:{},permissionDenials:[]}};
    },
  };
  session = new AgentSession({sessionId:'s',turnRunner:runner});
  for await (const _ of session.submit({type:'text',text:'start'},{turnId:'t'})) {}
  abortDrain.push(observed);
}
const trace = [], closeCalls = [], events = [];
const controller = new AbortController();
const mailbox = new SteerMailbox({recordMutation:async(_turn, mutation)=>trace.push(['mutation',mutation.mutation,mutation.itemId])});
mailbox.start('t');
await mailbox.enqueue('t',item);
let modelCalls = 0;
const model = new Proxy({}, {get(_target,key) {
  if (key === 'then') return undefined;
  return () => {modelCalls++; throw new Error(`unexpected model call: ${String(key)}`);};
}});
const config = {cwd:process.cwd(),runMode:'agent'};
const capabilities = createSidecarAgentTurnCapabilities(config, {ports:{model,toolExecution:{list:()=>[],executeAll:async()=>[]}},now:()=>new Date('2026-10-25T00:00:00Z')});
const loop = new AgentLoop(config,capabilities);
const transcript = {
  recordSessionEvent(){},recordAcceptedInput(){},recordTurnResult(){},
  async recordDurableMessage(_s,_t,message) {
    assert.equal(message.metadata?.queueItemId,'a');
    trace.push(['durable','a']);
    controller.abort('after-durable');
    trace.push(['abort',controller.signal.aborted]);
  },
};
const runner = new TurnRunner(loop,transcript,undefined,()=>new Date('2026-10-25T00:00:00Z'),undefined,
  {cwd:process.cwd(),transcriptPath:'',collectFileArtifacts:false});
for await (const event of runner.run({sessionId:'s',turnId:'t',messages:[],input:{type:'text',text:'start'},abortSignal:controller.signal,
  drainSteerMessages:()=>mailbox.drain('t'),drainOrCloseSteerMailbox:()=>mailbox.drainOrClose('t'),
  claimSteerMessage:(id)=>mailbox.claim('t',id),
  ackSteerMessage(id){trace.push(['ack',id,controller.signal.aborted]);mailbox.ack('t',id);},
  async closeSteerMailbox(){const discarded=await mailbox.close('t');closeCalls.push(ids(discarded));return discarded;},
})) {
  events.push(event.type);
  if(event.type === 'steer_applied') {assert.equal(event.itemId,'a'); break;}
}
assert.ok(events.includes('steer_applied'),JSON.stringify({events,trace}));
assert.equal(modelCalls,0);
assert.deepEqual(trace,[['mutation','insert','a'],['mutation','claim','a'],['durable','a'],['abort',true],['ack','a',true]]);
assert.deepEqual(loop.snapshotFileState().allowedReadFiles,['/safe']);
assert.ok(closeCalls.length > 0);
assert.ok(closeCalls.every(items=>items.length === 0));
assert.deepEqual(await mailbox.close('t'),[]);
bindSourceTree(root,commit,'/usr/bin/git');
console.log(JSON.stringify({commit,sourceSha256:Object.fromEntries(sources.map(p=>[p,createHash('sha256').update(readFileSync(join(root,p))).digest('hex')])),
  abortDrain,durableThenAbort:{trace,events,allowedReadFiles:loop.snapshotFileState().allowedReadFiles,closeCalls,modelCalls}},null,2));
