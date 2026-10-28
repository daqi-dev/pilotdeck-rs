// Real AgentSession outer generator; deterministic runner/dispatch ports.
// Executor/command and durable behavior are independently measured in #743's
// 33-cell native composition matrix, not inferred from these controlled ports.
import assert from 'node:assert/strict';
import {mkdirSync, writeFileSync} from 'node:fs';
import {resolve, join} from 'node:path';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
assert.equal(process.version, 'v22.22.0');
const base = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(base);
const sourceSha256 = await baselineSourceHashes(base, ['src/agent/session/AgentSession.ts']);
const {AgentSession} = await import(join(base, 'src/agent/session/AgentSession.ts'));
function gate() { let open!: () => void; const promise = new Promise<void>(r => open=r); return {promise,open}; }
async function bounded<T>(future: Promise<T>): Promise<T> {
  let timer: any; try { return await Promise.race([future, new Promise<never>((_, r) => timer=setTimeout(()=>r(new Error('#743 gate timeout')),5000))]); }
  finally { clearTimeout(timer); }
}
const specs = [
  ['start-success',''], ['start-failure-callback','start-result'], ['start-block-callback','start-result'],
  ['start-failure-command','start-result'], ['start-block-command','start-result'],
  ['start-dispatch-reject','reject-start'], ['setup-dispatch-reject','reject-setup'],
  ['abort-start-cooperative','abort-start'], ['abort-start-ignored','abort-start'], ['abort-start-command','abort-start'],
  ['abort-turn','abort-turn'], ['abort-turn-command-end','abort-turn'], ['abort-after-terminal','abort-terminal'],
  ['abort-end-cooperative','abort-end'], ['abort-end-ignored','abort-end'], ['abort-end-command','abort-end'],
  ['end-failure-callback','end-result'], ['end-failure-command','end-result'], ['end-block-callback','end-result'],
  ['end-dispatch-reject','reject-end'], ['turn-model-failure','error'],
  ['turn-runner-throw','throw'], ['abort-runner-throw','abort-throw'],
];
const cases=[];
for (const [name, action] of specs) {
  const entered=gate(), release=gate(); const calls:any[]=[]; const events:any[]=[]; const states:any[]=[];
  let session:any, finishes=0, caught:string|null=null;
  const recorder={recordInboxMutation: async()=>{}};
  const runner={sessionEventRecorder:recorder, async *run(input:any) {
    input.openSteerMailbox();
    if (action==='abort-turn' || action==='abort-throw') { entered.open(); await release.promise; }
    if (action==='throw' || action==='abort-throw') throw new Error('START_TURN_PORT_FAILURE');
    await input.closeSteerMailbox();
    const type=input.abortSignal.aborted?'aborted':action==='error'?'error':'success';
    const result={type,usage:{},permissionDenials:[]};
    yield {type:'turn_completed',sessionId:'session-743',turnId:'turn-1',result};
    return {messages:[],result};
  }};
  const lifecycle={async dispatch(input:any) {
    calls.push({event:input.event,baseInput:input.baseInput,payload:input.payload,matchQuery:input.matchQuery,aborted:input.signal.aborted});
    if ((input.event==='SessionStart' && action==='reject-start') || (input.event==='Setup' && action==='reject-setup') || (input.event==='SessionEnd' && action==='reject-end')) throw new Error(`${input.event}_DISPATCH_REJECT`);
    if ((input.event==='SessionStart' && action==='abort-start') || (input.event==='SessionEnd' && action==='abort-end')) { entered.open(); await release.promise; }
    return {effects:[],errors:[],blockingErrors:[],pendingAsyncHooks:[]};
  }};
  session=new AgentSession({sessionId:'session-743',cwd:'/virtual/743',transcriptPath:'/virtual/743/session.jsonl',turnRunner:runner as any,lifecycle:lifecycle as any});
  const finish=session.steerMailbox.finish.bind(session.steerMailbox);
  session.steerMailbox.finish=(id:string)=>{finishes++;finish(id);};
  const stream=session.submit({type:'text',text:'hello'},{turnId:'turn-1'});
  const snapshot=()=>({status:session.state.status,currentTurnId:session.state.currentTurnId??null});
  async function next() { const value=await bounded(stream.next()); if(!value.done) { events.push(value.value); states.push(snapshot()); } return value; }
  try {
    await next();
    let step=next();
    if(action==='abort-start') { await bounded(entered.promise);session.abort('test-abort'); release.open(); }
    await step;
    step=next();
    if(action==='abort-turn'||action==='abort-throw') { await bounded(entered.promise);session.abort('test-abort');release.open(); }
    await step;
    if(action==='abort-terminal') session.abort('test-abort');
    step=next();
    if(action==='abort-end') { await bounded(entered.promise);session.abort('test-abort');release.open(); }
    await step;
    assert.equal((await next()).done,true);
  } catch(e) { caught=(e as Error).message; assert.equal((await bounded(stream.next())).done,true); }
  cases.push({name,action,events,states,calls,error:caught,final:snapshot(),finishes,done:true});
}
assert.equal(assertBaseline(base),sourceCommit);
const output=resolve(process.argv[2]);mkdirSync(output,{recursive:true});
writeFileSync(join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,node:process.version,cases},null,2)+'\n',{flag:'wx'});
console.log(`#743 generated ${cases.length} real AgentSession shell cells`);
