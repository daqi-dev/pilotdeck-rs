import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/agent/session/SteerMailbox.ts',
  'src/agent/turn/TurnRunner.ts',
  'src/agent/session/AgentSessionEventRecorder.ts',
  'src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts',
  'src/agent/modules/transport/agentLoopSidecarClient.ts',
  'src/agent/modules/checkpoint/hostToolCheckpoint.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const load = (source: string) => import(pathToFileURL(join(baseline, source)).href);
const { SteerMailbox } = await load(sources[0]);
const { TurnRunner } = await load(sources[1]);
const { createSidecarDefaultModuleDispatcher } = await load(sources[3]);
const { HostToolCheckpoint } = await load(sources[5]);
const owned = await mkdtemp(join(tmpdir(), 'pdrs-steer-'));
const originalCwd = process.cwd();
const json = (value: any) => JSON.parse(JSON.stringify(value));
const capture = async (f: () => any) => {
  try { return { ok: true, value: json((await f()) ?? null) }; }
  catch (error) { return { ok: false, error: (error as Error).message }; }
};
const steer = (id: string, paths?: any[], text = id) => ({
  itemId: id,
  message: { role: 'user', content: [{ type: 'text', text }], metadata: { queueItemId: id } },
  ...(paths ? { allowedReadFiles: paths } : {}),
});
const op = (op: string, id?: string, turn = 't', extra = {}) => ({ op, turn, ...(id === undefined ? {} : { id }), ...extra });
const enq = (id: string, turn = 't', paths?: string[], text = id) => op('enqueue', id, turn, { input: steer(id, paths, text) });
const start = op('start');
const begin = (key: string, action: any) => ({ op: 'begin', key, action });
const block = (mutation: string) => ({ op: 'block', mutation });
const joinOp = (key: string) => ({ op: 'join', key });
const wait = { op: 'wait' }, release = { op: 'release' };
const mailboxSpecs = [
  { name: 'identity-dedup-finish', steps: [enq('a'), op('cancel','a'), op('drain'), op('drainOrClose'), op('close'), op('finish'), start, enq('a','other'), op('cancel','a','other'), enq('a','t',[]), enq('a','t',['ignored'],'replacement'), op('drain','x','other'), op('drain'), op('drain'), op('finish','x','other'), op('finish'), enq('b'), start, enq('a'), op('finish')] },
  { name: 'cancel-tombstones', steps: [start, op('cancel','before'), op('cancel','before'), enq('before'), enq('a'), op('cancel','a'), enq('a'), op('drain'), op('close'), op('cancel','after'), enq('after'), op('finish'), start, enq('before'), op('finish')] },
  { name: 'offered-claimed-ack', steps: [start,enq('a'),op('claim','a'),op('ack','a'),op('drain'),op('cancel','a'),op('claim','a','other'),op('ack','a'),op('claim','missing','other'),op('claim','a'),op('claim','a','other'),op('cancel','a'),op('ack','a','other'),op('ack','a'),op('ack','a'),op('cancel','a'),enq('a'),op('close')] },
  { name: 'terminal-close-keeps-offered', steps: [start,enq('a','t',['/a']),enq('b'),op('drainOrClose'),op('drainOrClose'),enq('late'),op('claim','a'),op('ack','a'),op('close'),op('close'),enq('a'),op('cancel','unknown'),op('finish'),enq('a')] },
  { name: 'restart-resets-all', steps: [start,enq('a'),op('cancel','b'),op('start',undefined,'next'),enq('a'),enq('b','next'),op('drainOrClose',undefined,'t'),op('close',undefined,'t'),op('drain',undefined,'next'),op('finish',undefined,'next')] },
  { name: 'utf16-and-empty-identities', steps: [op('start',undefined,''),enq('a',''),op('cancel','a',''),op('drainOrClose',undefined,''),op('start',undefined,'\ud800'),enq('','\ud800',['','\ud800']),op('claim','','\ud800'),op('drain',undefined,'\ud800'),op('claim','','\ud801'),op('claim','','\ud800'),op('ack','','\ud800'),op('finish',undefined,'\ud800')] },
  { name: 'insert-close-race', steps: [start,block('insert'),begin('insert',enq('a')),wait,begin('close',op('close')),begin('late',enq('b')),op('drain'),release,joinOp('insert'),joinOp('close'),joinOp('late'),op('finish')] },
  { name: 'insert-drain-close-race', steps: [start,block('insert'),begin('insert',enq('a')),wait,op('drain'),op('drainOrClose'),release,joinOp('insert'),op('close')] },
  { name: 'cancel-drain-race', steps: [start,enq('a'),block('cancel'),begin('cancel',op('cancel','a')),wait,op('drain'),release,joinOp('cancel'),op('claim','a'),op('ack','a'),enq('a'),op('close')] },
  { name: 'claim-ack-race', steps: [start,enq('a'),op('drain'),block('claim'),begin('claim',op('claim','a')),wait,op('ack','a'),op('drainOrClose'),release,joinOp('claim'),op('ack','a'),op('close')] },
  { name: 'restart-during-insert', steps: [start,block('insert'),begin('insert',enq('a')),wait,op('start',undefined,'next'),release,joinOp('insert'),enq('a','next'),op('drain',undefined,'next'),op('finish',undefined,'next')] },
  { name: 'finish-during-insert', steps: [start,block('insert'),begin('insert',enq('a')),wait,op('finish'),release,joinOp('insert'),op('close')] },
  { name: 'claim-captured-entry', steps: [start,enq('a'),op('drain'),block('claim'),begin('claim',op('claim','a')),wait,start,begin('insert',enq('a')),release,joinOp('claim'),joinOp('insert'),op('ack','a'),op('drain'),op('close')] },
  { name: 'failure-recovery', steps: [start,{op:'fail',mutation:'insert'},enq('a'),enq('a'),{op:'fail',mutation:'cancel'},op('cancel','a'),op('drain'),{op:'fail',mutation:'claim'},op('claim','a'),op('ack','a'),op('claim','a'),{op:'fail',mutation:'discard'},op('close'),op('close'),op('finish')] },
  { name: 'call-order', steps: [start,begin('cancel',op('cancel','a')),begin('insert',enq('a')),joinOp('insert'),joinOp('cancel'),begin('first',enq('b','t',['first'])),begin('second',enq('b','t',['second'])),joinOp('second'),joinOp('first'),op('drain'),op('close')] },
];
const mailboxCases: any[] = [];
async function mailboxCase(spec: any) {
  const mutations: any[] = [];
  let gate: any, failure: string | undefined;
  const pending = new Map();
  const mailbox = new SteerMailbox({ recordMutation: async (turn: string, mutation: any) => {
    mutations.push(json({ turn, mutation }));
    if (failure === mutation.mutation) { failure = undefined; throw new Error('fixture storage failure'); }
    if (gate?.mutation === mutation.mutation) {
      const current = gate; current.mutation = undefined; current.enter(); await current.blocked;
    }
  }});
  function invoke(step: any): any {
    switch (step.op) {
      case 'start': return mailbox.start(step.turn);
      case 'enqueue': return mailbox.enqueue(step.turn, step.input);
      case 'cancel': return mailbox.cancel(step.turn, step.id);
      case 'claim': return mailbox.claim(step.turn, step.id);
      case 'ack': return mailbox.ack(step.turn, step.id);
      case 'drain': return mailbox.drain(step.turn);
      case 'drainOrClose': return mailbox.drainOrClose(step.turn);
      case 'close': return mailbox.close(step.turn);
      case 'finish': return mailbox.finish(step.turn);
      default: throw new Error(`unknown mailbox operation ${step.op}`);
    }
  }
  const results = [];
  for (const step of spec.steps) {
    if (step.op === 'block') {
      gate = { mutation: step.mutation };
      gate.entered = new Promise(resolve => { gate.enter = resolve; });
      gate.blocked = new Promise(resolve => { gate.release = resolve; });
    } else if (step.op === 'wait') await gate.entered;
    else if (step.op === 'release') gate.release();
    else if (step.op === 'fail') failure = step.mutation;
    else if (step.op === 'begin') pending.set(step.key, capture(() => invoke(step.action)));
    else if (step.op === 'join') { results.push(await pending.get(step.key)); pending.delete(step.key); continue; }
    else { results.push(await capture(() => invoke(step))); continue; }
    results.push({ ok: true, value: null });
  }
  assert.equal(pending.size, 0);
  mailboxCases.push({ ...spec, expected: { results, mutations } });
}

const dispatcherSpecs: any[] = [];
const add = (name: string, ports: string[], steps: any[]) => dispatcherSpecs.push({name,ports,steps});
const call = (payload: any, result: any = {}, failure?: string) => ({kind:'call',call:{module:'turn',payload},result,...(failure ? {failure} : {})});
const drainCall = (messages: any[]) => call({operation:'drain_steer'}, {messages});
const apply = (id: string) => ({kind:'apply',id});
add('grants-after-apply', ['drain','close'], [drainCall([steer('a',['/a','/a','']),steer('b',['/b'])]),apply('unknown'),apply('a'),apply('a'),call({operation:'drain_or_close_steer'}, {messages:[steer('c',['/c'])],closed:false,extra:7}),apply('b'),apply('c')]);
add('filtered-overwrite-empty-retains', ['drain'], [drainCall([steer('a',['old']),steer('a',['new',9,null,'\ud800']),steer('a',[]),steer('b',[false]),steer('a')]),apply('a'),apply('b')]);
add('payload-cannot-grant', ['drain'], [call({operation:'drain_steer',turnId:'other',messages:[steer('evil',['/evil'])],allowedReadFiles:['/evil']},{messages:[]}),apply('evil')]);
add('drain-only', ['drain'], [call({operation:'drain_or_close_steer'}),drainCall([])]);
add('close-only', ['close'], [call({operation:'drain_steer'}),call({operation:'drain_or_close_steer'},{messages:[],closed:true})]);
add('no-ports', [], [call({operation:'drain_steer'}),call({operation:'drain_or_close_steer'}),call({operation:'persist_compaction'})]);
add('callback-failures', ['drain','close'], [call({operation:'drain_steer'},{},'drain failed'),call({operation:'drain_or_close_steer'},{},'close failed'),drainCall([steer('a',['after'])]),apply('a')]);
add('invalid-path-array', ['drain'], [drainCall([{...steer('a'),allowedReadFiles:3}]),apply('a')]);
for (const payload of [{}, {operation:''}, {operation:null}, {operation:4}, {operation:'unknown'}, {operation:'\ud800'}, null, [], 7, 'x']) {
  add(`payload-${JSON.stringify(payload)}`, ['drain','close'], [call(payload)]);
}
const dispatcherCases: any[] = [];
async function dispatcherCase(spec: any) {
  const checkpoint = new HostToolCheckpoint(undefined, ['seed']);
  const trace: string[] = [];
  let current: any;
  const callback = async (kind: string) => {
    trace.push(kind);
    if (current.failure) throw new Error(current.failure);
    return current.result;
  };
  const dispatcher = createSidecarDefaultModuleDispatcher({
    config:{cwd:owned,permissionMode:'default'},
    input:{sessionId:'s',turnId:'t',
      ...(spec.ports.includes('drain') ? {drainSteerMessages:async () => (await callback('drain')).messages} : {}),
      ...(spec.ports.includes('close') ? {drainOrCloseSteerMailbox:() => callback('close')} : {}),
    }, checkpoint,
    modules:{model:{},capability:{execution:{list:()=>[]},runtimeContext:{bindTurn:()=>({permissionContext:()=>({})})}}},
    capabilityResultObserver:{},planTodoHandler:async()=>({}),
  });
  const results = [], grants = [];
  for (const step of spec.steps) {
    current = step;
    results.push(await capture(() => step.kind === 'apply'
      ? dispatcher.applySteerAuthorization(step.id) : dispatcher.handlers.turn(step.call)));
    grants.push(checkpoint.toolContextState().allowedReadFiles);
  }
  await dispatcher.dispose();
  dispatcherCases.push({...spec,expected:json({results,grants,trace,methods:dispatcher.manifest.hostModules.turn?.methods ?? null})});
}

// These callbacks are created by the real TurnRunner; no copied callback body.
const runnerCases: any[] = [];
async function runnerCase(mode: string) {
  const trace: any[] = [], drains: any[] = [];
  const when = '2026-10-19T00:00:00.000Z';
  const a = steer('a'), b = steer('b'), c = steer('c');
  let afterApplied: any[] = [];
  const loop = {snapshotFileState:()=>({}), async *run(input: any) {
    drains.push(json(await input.drainSteerMessages()));
    drains.push(json(await input.drainOrCloseSteerMailbox()));
    await input.onDurableMessage(a.message);
    input.onSteerApplied('a');
    input.onSteerApplied('a');
    await input.onDurableMessage({role:'assistant',content:[]});
    afterApplied = json(input.messages);
    return {result:{type:'success',sessionId:'s',turnId:'t',stopReason:'completed',usage:{},permissionDenials:[],turns:1,startedAt:when,completedAt:when},messages:input.messages};
  }};
  const transcript = {recordSessionEvent(){},recordAcceptedInput(){},recordTurnResult(){},
    recordDurableMessage(_s: string,_t: string,message: any) {
      trace.push(['durable',message.metadata?.queueItemId ?? null]);
      if (mode === 'durable-fails') throw new Error('durable failed');
    },
  };
  const runner = new TurnRunner(loop,transcript,undefined,()=>new Date(when),undefined,{cwd:owned,transcriptPath:'',collectFileArtifacts:false});
  const iterator = runner.run({sessionId:'s',turnId:'t',messages:[],input:{type:'text',text:'start'},
    drainSteerMessages:()=>[a,b],drainOrCloseSteerMailbox:()=>({messages:[c],closed:false}),
    claimSteerMessage:async (id: string)=>{trace.push(['claim',id]);if(mode==='claim-fails')throw new Error('claim failed');},
    ackSteerMessage:(id: string)=>trace.push(['ack',id]),
  });
  const unapplied: string[] = [];
  for (;;) { const next = await iterator.next(); if (next.done) break; if(next.value.type==='steer_unapplied')unapplied.push(next.value.itemId); }
  runnerCases.push({mode,steers:[a,b,c],expected:{trace,drains,afterApplied,unapplied}});
}
try {
  process.chdir(owned);
  for (const spec of mailboxSpecs) await mailboxCase(spec);
  for (const spec of dispatcherSpecs) await dispatcherCase(spec);
  for (const mode of ['success','claim-fails','durable-fails']) await runnerCase(mode);
  assert.equal(mailboxCases.length,15);
  assert.equal(mailboxCases.reduce((sum, c) => sum + c.steps.length, 0),192);
  assert.equal(dispatcherCases.length,18);
  assert.equal(dispatcherCases.reduce((sum, c) => sum + c.steps.length, 0),35);
  assert.equal(runnerCases.length,3);
  assertBaseline(baseline);
  assert.deepEqual(await baselineSourceHashes(baseline,sources),sourceSha256);
  await mkdir(output,{recursive:true});
  await writeFile(join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,mailboxCases,dispatcherCases,runnerCases},null,2)+'\n');
  console.log(`mailbox=${mailboxCases.length} dispatcher=${dispatcherCases.length} runner=${runnerCases.length}`);
} finally { process.chdir(originalCwd); await rm(owned,{recursive:true,force:true}); }
