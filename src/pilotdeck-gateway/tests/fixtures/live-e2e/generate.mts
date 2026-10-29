// #748 independent TS golden generation; no Rust subprocess or Rust observation is read.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {mkdirSync,readFileSync,writeFileSync} from 'node:fs';
import {dirname,join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawnSync} from 'node:child_process';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';
assert.equal(process.version,'v22.22.0','#748 generator pinned Node');
assert.equal(process.env.LIVE_E2E_MUTANT,undefined,'#748 generator rejects mutation');
assert.equal(process.env.LIVE_E2E_MITM,undefined,'#748 generator rejects MITM override');
assert.ok(process.env.PDRS_BASELINE,'#748 generator requires PDRS_BASELINE');
assert.ok(process.argv[2],'#748 generator requires output directory');
const baseline=resolve(process.env.PDRS_BASELINE), output=resolve(process.argv[2]);
const sourceCommit=assertBaseline(baseline), fixture=dirname(fileURLToPath(import.meta.url));
mkdirSync(output,{recursive:true}); mkdirSync(join(output,'expected'),{recursive:true});
const cases=['text','tool','cancel-stream','cancel-tool','drop','duplicate','early-final'];
const expectedSha256:Record<string,string>={};
const sha=(value:Buffer|string)=>createHash('sha256').update(value).digest('hex');
for(const name of cases){
 const directory=join(output,'capture',name);
 const child=spawnSync(process.env.LIVE_E2E_PYTHON ?? 'python3',['-B',join(fixture,'capture.py'),name,directory],{encoding:'utf8',timeout:80000,maxBuffer:8*1024*1024,env:process.env});
 assert.equal(child.status,0,`#748 generator ${name} failed: ${child.error??''}\n${child.stdout}\n${child.stderr}`);
 process.stdout.write(child.stdout);
 const bytes=readFileSync(join(directory,'result.json'));
 const result=JSON.parse(bytes.toString());
 assert.ok(result.events.some((event:any)=>event.type==='turn_completed'),`#748 generator ${name} must finish a real Gateway turn`);
 assert.equal(result.active,false,`#748 generator ${name} must release session admission`);
 const terminal=result.events.at(-1);
 const reason=name.startsWith('cancel-')?'aborted_streaming':name==='early-final'?'mitm_premature_final':['drop','duplicate'].includes(name)?'model_error':'completed';
 assert.equal(terminal.type,'turn_completed',`#748 ${name} terminal last`);
 assert.equal(terminal.finishReason,reason,`#748 ${name} terminal reason`);
 const summary=JSON.parse(readFileSync(join(directory,'mitm-summary.json'),'utf8'));
 assert.equal(summary.injections.length,['drop','duplicate','early-final'].includes(name)?1:0,`#748 ${name} injection count`);
 assert.ok(summary.injections.every((entry:any)=>entry.fired),`#748 ${name} injection fired`);
 if(name==='tool') assert.ok(result.toolResults.some((r:any)=>r.type==='success' && r.content.some((c:any)=>c.text?.includes('live-e2e read tool payload'))),'#748 actual read result');
 if(name==='cancel-tool') assert.equal(result.toolEntered,true,'#748 cancel inside read execution');
 if(name==='cancel-stream') {
  const requests=readFileSync(join(directory,'mock.jsonl'),'utf8').trim().split('\n').map(line=>JSON.parse(line)).filter(row=>row.connection);
  assert.equal(requests.length,1,'#748 single cancel provider request');
  assert.equal(requests[0].connection,'aborted','#748 provider interrupted');
 }

 writeFileSync(join(output,'expected',name+'.json'),bytes,{flag:'wx'}); expectedSha256[name]=sha(bytes);
}
const sourceSha256=await baselineSourceHashes(baseline,[
 'src/gateway/client/InProcessGateway.ts','src/gateway/SessionRouter.ts','src/agent/session/AgentSession.ts',
 'src/agent/turn/TurnRunner.ts','src/agent/modules/transport/agentLoopSidecarClient.ts',
 'src/cli/pilotdeck-agent-loop-sidecar.ts','src/model/streaming/streamModel.ts','src/tool/execution/ToolRuntime.ts',
 'src/tool/builtin/readFile.ts','src/gateway/run/RunRegistry.ts','src/session/transcript/JsonlTranscriptWriter.ts',
 'src/agent/session/AgentSessionEventRecorder.ts','src/agent/modules/llm/durableModelInvokerPort.ts',
]);
const apparatusSha256=Object.fromEntries(['stdio_mitm.py','io_deadline.py','driver.mts','mock_provider.mjs','determinism.mjs','capture.py'].map(name=>[name,sha(readFileSync(join(fixture,name)))]));
writeFileSync(join(output,'oracle.json'),JSON.stringify({sourceCommit,node:process.version,sourceSha256,apparatusSha256,expectedSha256},null,2)+'\n',{flag:'wx'});
console.log('#748 seven real InProcessGateway cells captured');
