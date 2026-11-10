import assert from 'node:assert/strict';
import {readFileSync, writeFileSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {pathToFileURL} from 'node:url';
import {join} from 'node:path';
const [base,out]=process.argv.slice(2),commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
assert.equal(process.versions.node,'22.22.0');assert.equal(process.versions.icu,'78.2');assert.equal(execFileSync('git',['-C',base,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),commit);
const rel='src/context/compaction/CompactionEngine.ts',bytes=readFileSync(join(base,rel));assert.deepEqual(bytes,execFileSync('git',['-C',base,'show',commit+':'+rel]));
const {CompactionEngine}=await import(pathToFileURL(join(base,rel)).href);
const messages=['old one','old answer','new question','new answer'].map((text,i)=>({role:i%2?'assistant':'user',content:[{type:'text',text}]}));
const cases=[];
for(const id of ['stream-failure','iterator-failure','parent-cancellation']){
 const parent=new AbortController();let invoked=0,seen:any;
 const model={stream(_request:any,signal:AbortSignal){invoked++;seen=signal;
  if(id==='stream-failure')throw Error('mock stream refused');
  return (async function*(){if(id==='iterator-failure')throw Error('mock next failed');
   await new Promise((_,reject)=>{signal.addEventListener('abort',()=>reject(signal.reason),{once:true});queueMicrotask(()=>parent.abort());});
  })();
 }};
 const engine=new CompactionEngine({model,provider:'fixture',model_:'mock',protectedToolNames:[]});
 const result=await engine.run({trigger:'manual',messages,signal:parent.signal,targetPostTokens:1,protectedToolNames:null});
 assert.equal(invoked,1);assert.equal(result.summaryGenerated,false);assert.equal(result.diagnostics[0].code,'compact_summary_failed');assert.equal(result.diagnostics[0].severity,'warning');assert(result.error);assert.equal(result.summaryMessage,undefined);
 cases.push({id,error:result.error,summaryGenerated:result.summaryGenerated,diagnostics:result.diagnostics,boundaryMarker:result.boundaryMarker,messagesToKeep:result.messagesToKeep,parentAborted:parent.signal.aborted,providerSignalIsParent:seen===parent.signal});
}
writeFileSync(out,JSON.stringify({sourceCommit:commit,sourceSha256:{[rel]:createHash('sha256').update(bytes).digest('hex')},node:process.versions.node,icu:process.versions.icu,cases},null,2)+'\n',{flag:'wx'});console.log(JSON.stringify({cases:cases.length,sha256:createHash('sha256').update(readFileSync(out)).digest('hex')}));
