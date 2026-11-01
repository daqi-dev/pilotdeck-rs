import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {assertBaseline, baselineSourceHashes} from '../../_shared/baseline.mts';

const base = path.resolve(process.env.PDRS_BASELINE!);
const out = path.resolve(process.argv[2]);
assert.equal(process.version, 'v22.22.0', '#1253 Node version');
assert.equal(process.versions.icu, '78.2', '#1253 ICU version');
const sourceCommit = assertBaseline(base);
const sourceSha256 = await baselineSourceHashes(base, [
  'src/session/projection/AgentTranscriptProjections.ts',
  'src/session/projection/SessionProjectionDriver.ts',
  'src/session/events/SessionRuntime.ts',
]);
fs.mkdirSync(out, {recursive:true});
fs.writeFileSync(path.join(out, '.generation-owner'), 'session-factory/append-delta\n', {flag:'wx'});
const {SessionProjectionRegistry} = await import(`${base}/src/session/projection/SessionProjectionRegistry.ts`);
const {registerAgentTranscriptProjections} = await import(`${base}/src/session/projection/AgentTranscriptProjections.ts`);
const {SessionProjectionDriver} = await import(`${base}/src/session/projection/SessionProjectionDriver.ts`);
const {SessionRuntime} = await import(`${base}/src/session/events/SessionRuntime.ts`);
const clone = (v:any) => JSON.parse(JSON.stringify(v));
const registry = () => { const r = new SessionProjectionRegistry(); registerAgentTranscriptProjections(r); return r; };
const snapshot = (driver:any) => {
  const {values} = driver.snapshot();
  return clone({messages:values['agent-transcript.conversation'].messages,
    ...values['agent-transcript.turn-summary'], metadata:values['agent-transcript.metadata']});
};
const msg = (role:string, text:string, metadata?:any) => ({role, content:[{type:'text',text}], ...(metadata ? {metadata} : {})});
const input = (turnId:string, text:string, metadata?:any) => ({type:'accepted_input',turnId,messages:[msg('user',text)], ...(metadata ? {metadata} : {})});
const result = (turnId:string) => ({type:'turn_result',turnId,result:{type:'success',sessionId:'session-1253',turnId,
  usage:{inputTokens:2,outputTokens:3},permissionDenials:[{toolName:'shell',toolCallId:turnId,reason:'probe'}],turns:1,
  startedAt:'2026-11-01T00:00:00.000Z',completedAt:'2026-11-01T00:00:00.000Z'}});
const context = (turnId:string, text:string) => ({type:'context_snapshot',turnId,runtimeContextMessages:[msg('user',text,{synthetic:true})]});
const messageKinds = ['assistant_message','tool_result_message','durable_message'];
const cases:any[] = [];
for (const [name, drafts] of [
  ['interleaved', [
    input('a','first',{modelSelection:{mode:'model',provider:'p',model:'m'}}),
    ...messageKinds.map(type=>({type,turnId:'a',message:msg('assistant',`pending ${type}`)})),
    context('a','pending context'), input('b','second',{modelSelection:{mode:'auto'}}),
    {type:'session_metadata',turnId:'a',metadata:{title:'title',linkedPullRequest:'one'}},
    {type:'agent_status',turnId:'a',status:'working'}, result('b'), result('a'),
    ...messageKinds.map(type=>({type,turnId:'a',message:msg('assistant',`late ${type}`)})),
    {type:'durable_message',turnId:'a',message:msg('assistant','hidden',{compactReplacement:true})},
    context('b','late context'),
    {type:'accepted_input',turnId:'c',messages:[msg('user','synthetic',{synthetic:true})],metadata:{modelSelection:{mode:'model',provider:'p'}}},
    context('a','after synthetic'),
    {type:'session_metadata',turnId:'a',metadata:{title:null,linkedPullRequest:null}},
  ]],
  ['compaction', [input('a','old'), {type:'durable_message',turnId:'a',message:msg('assistant','pending old')},
    {type:'control_boundary',turnId:'compact',boundary:{kind:'compact',subtype:'compact_boundary',snapshot:{version:1,messages:[msg('assistant','snapshot')]}}},
    input('b','after snapshot'), result('a'),
    {type:'control_boundary',turnId:'empty',boundary:{kind:'compact',subtype:'compact_boundary'}},
    {type:'control_boundary',turnId:'legacy',boundary:{kind:'compact',subtype:'compact_boundary',replacementMessages:[msg('assistant','legacy replacement')]}},
    input('c','after legacy'), result('legacy'),
    {type:'control_boundary',turnId:'other',boundary:{kind:'snip'}},
  ]],
  ['restore', [input('a','restored user'),result('a'),
    ...messageKinds.map(type=>({type,turnId:'a',message:msg('assistant',`restored ${type}`)})),
    context('a','restored context'),
  ]],
] as const) {
  const entries:any[] = [], steps:any[] = [];
  const driver = new SessionProjectionDriver({registry:registry()});
  for (const draft of drafts) {
    const sequence = entries.length + 1;
    const entry = {...draft,sessionId:'session-1253',sequence,createdAt:'2026-11-01T00:00:00.000Z',entryId:`${name}-${sequence}`,parentEntryId:entries.at(-1)?.entryId??null};
    entries.push(entry);
    // Exercise the real driver's event reducer/finalizer. This projection-only
    // fixture deliberately includes legacy/out-of-turn records; domain
    // validation is tested by the existing recorder suite.
    (driver as any).drive(entry);
    const expected = snapshot(driver);
    const replay = new SessionProjectionDriver({registry:registry(),entries});
    assert.deepEqual(expected,snapshot(replay),`#1253 ${name}/${sequence} live versus replay`);
    replay.dispose();
    steps.push({entry:clone(entry),expected});
    console.log('OBSERVED',name,sequence,entry.type,JSON.stringify(expected));
  }
  driver.dispose(); cases.push({name,steps});
}

function deferred() { let resolve!:()=>void; const promise=new Promise<void>(r=>{resolve=r;}); return {promise,resolve}; }
async function bounded<T>(p:Promise<T>) { let timer:any; try { return await Promise.race([p,new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(Error('#1253 gate exceeded 10 seconds')),10000);})]); } finally {clearTimeout(timer);} }
const runtime = new SessionRuntime({now:()=>new Date('2026-11-01T00:00:00.000Z'),uuid:(()=>{let n=0;return()=>`gate-${++n}`;})()});
const entered=deferred(), release=deferred(); let first=true;
runtime.subscribe(async()=>{if(first){first=false;entered.resolve();await bounded(release.promise);}});
const driver = new SessionProjectionDriver({runtime,registry:registry()});
const observe = () => ({writer:runtime.snapshotState(),state:snapshot(driver)});
const before=observe();
const one=runtime.append('session-1253','one',{type:'session_metadata',metadata:{title:'one'}} as any);
await bounded(entered.promise);
const two=runtime.append('session-1253','two',{type:'session_metadata',metadata:{title:'two'}} as any);
const held=observe();
const abort=new AbortController();
const waiter=Promise.race([one,new Promise((_,reject)=>abort.signal.addEventListener('abort',()=>reject(Error('CANCEL_1253')),{once:true}))]);
abort.abort(); await assert.rejects(waiter,/CANCEL_1253/,'#1253 cancelled waiter');
const cancelled=observe();
assert.deepEqual(held,before,'#1253 predecessor held before publish');
assert.deepEqual(cancelled,held,'#1253 cancelling waiter leaves append pending');
release.resolve();await bounded(Promise.all([one,two]));const settled=observe();driver.dispose();
assert.equal(settled.writer.sequence,2,'#1253 both queued appends settle');
const concurrency={before,held,cancelled,settled};
console.log('OBSERVED concurrency',JSON.stringify(concurrency));
// #1253 fix-r1: observe real TS replay/hydration and duplicate admission.
// A repeated Rust durable notification is not a second TS appendRecorded call.
const deliveryEntries = cases.find(row=>row.name==='restore').steps.slice(0,3).map((step:any)=>step.entry);
const canonical = new SessionProjectionDriver({registry:registry(),entries:deliveryEntries.slice(0,2)});
const completed = snapshot(canonical);
canonical.hydrate(deliveryEntries.slice(0,2));
(canonical as any).drive(deliveryEntries[2]);
const late = snapshot(canonical);
const replayLate = new SessionProjectionDriver({registry:registry(),entries:deliveryEntries});
assert.deepEqual(late,snapshot(replayLate),'#1253 hydrated completed turn accepts late message');
canonical.dispose(); replayLate.dispose();
const admission:any[]=[];
for(const entries of [deliveryEntries.slice(0,2),deliveryEntries]) {
  const rt = new SessionRuntime(); rt.restore(entries);
  const d = new SessionProjectionDriver({runtime:rt,registry:registry(),entries});
  const before=snapshot(d);let failure:any;
  try {await rt.appendRecorded(deliveryEntries[1]);} catch(error:any) {failure={name:error.name,code:error.code,message:error.message};}
  assert(failure,'#1253 duplicate/stale TS recorded append rejects');
  assert.deepEqual(snapshot(d),before,'#1253 rejected TS entry does not double count');
  admission.push({watermark:entries.at(-1).sequence,failure,state:snapshot(d)});d.dispose();
}
const delivery={entries:deliveryEntries,completed,late,admission};
console.log('OBSERVED delivery',JSON.stringify(delivery));
const metadataDrafts:any[]=[input('metadata','metadata user',{modelSelection:{mode:'auto'}}),
  {type:'session_metadata',turnId:'metadata',metadata:{title:'before',linkedPullRequest:{number:1,url:'https://example.invalid/1',repository:'fixture'}}},
  result('metadata'),
  {type:'session_metadata',turnId:'metadata',metadata:{title:'after',linkedPullRequest:{number:2,url:'https://example.invalid/2',repository:'fixture'},tag:'suffix'}},
  {type:'session_metadata',turnId:'metadata',metadata:{title:null,linkedPullRequest:null}},
  input('next','next user',{modelSelection:{mode:'model',provider:'probe',model:'fixture'}})];
const metadataEntries=metadataDrafts.map((draft,at)=>({...draft,sessionId:'session-1253',sequence:at+1,
  createdAt:'2026-11-01T00:00:00.000Z',entryId:`metadata-${at+1}`,parentEntryId:at?`metadata-${at}`:null}));
const metadataDriver=new SessionProjectionDriver({registry:registry(),entries:metadataEntries.slice(0,3)});
const checkpoint=metadataDriver.checkpoint();
metadataDriver.hydrate(metadataEntries,checkpoint);
const metadataExpected=snapshot(metadataDriver);
const metadataReplay=new SessionProjectionDriver({registry:registry(),entries:metadataEntries});
assert.deepEqual(metadataExpected,snapshot(metadataReplay),'#1253 metadata checkpoint suffix equals replay');
const anchor=Object.fromEntries(['sequence','entryId','parentEntryId','turnId','type','createdAt'].map(key=>[key,metadataEntries[2][key]]));
const envelope={format:'pilotdeck.session-projections',version:1,sessionId:'session-1253',asOfSequence:3,anchor,projections:checkpoint};
const checkpointMetadata={entries:metadataEntries,envelope,expected:metadataExpected};
metadataDriver.dispose();metadataReplay.dispose();
console.log('OBSERVED checkpointMetadata',JSON.stringify(checkpointMetadata));

fs.writeFileSync(path.join(out,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,node:process.version,icu:process.versions.icu,cases,concurrency,delivery,checkpointMetadata},null,2)+'\n',{flag:'wx'});
console.log('PASS: #1253 real TS append delta matrix');
