import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
import {join,resolve} from 'node:path';
import {mkdirSync,writeFileSync} from 'node:fs';
import {assertBaseline,baselineSourceHashes} from '../../_shared/baseline.mts';
assert.equal(process.version,'v22.22.0');
const {createAgentLoopSidecarRuntimeFactory}=await import(pathToFileURL(join(process.env.PDRS_BASELINE!,'src/agent/modules/transport/agentLoopSidecarClient.ts')).href);
const baseline=resolve(process.env.PDRS_BASELINE!), output=resolve(process.argv[2]);
const sourceCommit=assertBaseline(baseline);
mkdirSync(output,{recursive:true});
const observations:any[]=[];
const cases = [
 {name:'missing', patch:{}},
 ...[null,false,0,''].map(value=>({name:'final-'+JSON.stringify(value),patch:{finalMessage:value}})),
 {name:'final-valid',patch:{finalMessage:{role:'assistant',content:[{type:'text',text:'ok'}]}}},
 {name:'messages-valid',patch:{},messages:[{role:'assistant',content:[{type:'text',text:'ok'}]},{role:'user',content:[{type:'text',text:'input'}]}]},

];
for (const outcome of ['success','aborted']) {
for (const cell of cases) {
const terminal={result:{type:outcome,sessionId:'s',turnId:'t',stopReason:'completed',turns:1,...cell.patch},messages:cell.messages??[]};
const incoming:any[]=[],sent:any[]=[];let waiter:any;
const push=(value:any)=>{if(waiter){const done=waiter;waiter=undefined;done({value,done:false});}else incoming.push(value);};
const binding={protocolVersion:'2.0',moduleId:'fixture',moduleInstanceId:'instance',connectionGeneration:'current',capabilitiesVersion:'v1'};
const connection={send(m:any){sent.push(m);const reply={kind:'response',messageId:'response-'+m.messageId,inReplyTo:m.messageId,ok:true};
 if(m.method==='hello')push({...reply,...binding});
 if(m.method==='capabilities')push({...reply,...binding,payload:{capabilitiesVersion:'v1',methods:[{name:'execute',profiles:['streaming'],resumeSupport:'streaming'},{name:'resume'},{name:'ack'}]}});
 if(m.method==='execute') {push({...reply,requestId:m.requestId,streamId:'stream'});const ids={runId:m.runId,operationId:m.operationId,requestId:m.requestId};
 push({kind:'event',eventType:'agent_event',streamId:'stream',...ids,sequence:0,final:false,payload:{type:'tool_progress',sessionId:'s',turnId:'t',toolCallId:'read-1',toolName:'read',message:'reading',createdAt:'2026-10-26T00:00:00Z'}});
 push({kind:'event',eventType:'agent_event',streamId:'stream',...ids,sequence:1,final:true,outcome:outcome==='aborted'?'cancelled':'completed',payload:terminal});
 }},receive(){return {[Symbol.asyncIterator](){return this;},next():Promise<any>{return incoming.length?Promise.resolve({value:incoming.shift(),done:false}):new Promise(r=>waiter=r);}};},close(){}};
const context={permissionContext:()=>({}),toolRuntimeContext:()=>({}),executionContext:()=>({})};
const modules={event:{drain:()=>[]},model:{execution:{}},capability:{runtimeContext:{bindTurn:()=>context},execution:{list:()=>[]}},permission:{requestContext:{bindTurn:()=>context}},context:{requestIdentity:{bindTurn:()=>({contextIdentity:()=>({})})}}};
const factory=createAgentLoopSidecarRuntimeFactory({connect:async()=>connection,uuid:()=> 'id',turnComposition:()=>({forTurn:()=>({moduleHandlers:{}})})});
const runner=factory({config:{cwd:process.cwd(),permissionMode:'default'},capabilities:{transport:{}},sidecarModules:modules} as any);
const events=[];let returned:any;let timer:ReturnType<typeof setTimeout>|undefined;
try {
 await Promise.race([(async()=>{const iterator=runner.run({sessionId:'s',turnId:'t',messages:[],execution:{runId:'run',operationId:'operation'}});while(true){const next=await iterator.next();if(next.done){returned=next.value;break;}events.push(next.value);}})(),new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('#748 TS terminal probe timeout')),2000);})]);
 const last=events.at(-1);assert.equal(last?.type,'turn_completed','#748 probe reached terminal');
 observations.push({name:outcome+'/'+cell.name,input:terminal,output:returned});
} catch(error) {assert.ok(error instanceof TypeError,'#748 only baseline message-shape TypeError is a boundary result');observations.push({name:outcome+'/'+cell.name,input:terminal,error:(error as Error).message});}
finally {clearTimeout(timer);}
}
}
assert.ok(observations.every(row=>'output' in row),'#748 every supported terminal case completes');
const sourceSha256=await baselineSourceHashes(baseline,['src/agent/modules/transport/agentLoopSidecarClient.ts']);
const oracle={sourceCommit,sourceSha256,node:process.version,cases:observations};
writeFileSync(join(output,'oracle.json'),JSON.stringify(oracle,null,2)+'\n',{flag:'wx'});
for(const row of observations) console.log(JSON.stringify(row));
