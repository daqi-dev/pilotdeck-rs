import assert from 'node:assert/strict';
import {mkdirSync,readFileSync,writeFileSync} from 'node:fs';
import {dirname,join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawnSync} from 'node:child_process';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';
assert.equal(process.version,'v22.22.0','#748 order probe pinned Node');
const baseline=resolve(process.env.PDRS_BASELINE!),output=resolve(process.argv[2]);
const fixture=dirname(fileURLToPath(import.meta.url));
mkdirSync(output,{recursive:true});
const sourceCommit=assertBaseline(baseline),cases=[];
for(const name of ['text','tool','assistant-error','tool-error','context-cancel']) {
 const directory=join(output,name);
 const run=spawnSync(process.env.LIVE_E2E_PYTHON??'python3',['-B',join(fixture,'capture_probe.py'),name,directory],{
  encoding:'utf8',timeout:85000,maxBuffer:8*1024*1024,env:{...process.env,HOME:join(directory,'home')}});
 writeFileSync(join(directory,'probe-stdout.log'),run.stdout??'',{flag:'wx'});
 writeFileSync(join(directory,'probe-stderr.log'),run.stderr??'',{flag:'wx'});
 assert.equal(run.status,0,`#748 TS ${name}: ${run.error??''}\n${run.stdout}\n${run.stderr}`);
 const result=JSON.parse(readFileSync(join(directory,'probe.json'),'utf8'));cases.push(result);console.log(JSON.stringify(result));
}
const sourceSha256=await baselineSourceHashes(baseline,[
 'src/gateway/client/InProcessGateway.ts','src/agent/turn/TurnRunner.ts','src/agent/modules/transport/agentLoopSidecarClient.ts',
 'src/session/transcript/JsonlTranscriptWriter.ts','src/session/transcript/TranscriptEntry.ts']);
writeFileSync(join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,node:process.version,cases},null,2)+'\n',{flag:'wx'});
