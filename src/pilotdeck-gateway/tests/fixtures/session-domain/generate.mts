import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
import { cases } from './cases.mts';
import assert from 'node:assert/strict';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { mkdir, writeFile } from 'node:fs/promises';
assert.ok(process.env.PDRS_BASELINE);
assert.ok(process.argv[2]);
const baseline=resolve(process.env.PDRS_BASELINE);
const sourceCommit=assertBaseline(baseline);
const path='src/session/events/SessionDomainValidation.ts';
const sourceSha256=await baselineSourceHashes(baseline,[path]);
const {validateSessionDomainLog,validateSessionDomainEvent,createSessionDomainValidationState}=await import(pathToFileURL(join(baseline,path)).href);
const outcome=(fn:()=>any)=>{try{fn();return {ok:true};}catch(error:any){return {error:{name:error.name,...(error.code===undefined?{}:{code:error.code}),message:error.message}};}};
const scenarios=cases.map(({name,entries,code})=>{
 const log=outcome(()=>validateSessionDomainLog(entries));
 assert.equal((log as any).error?.code??(log as any).error?.name,code,name);
 let state=createSessionDomainValidationState();
 const outcomes=entries.map(entry=>outcome(()=>{state=validateSessionDomainEvent(state,structuredClone(entry));}));
 return {name,entries,outcomes,log};
});
// Changing scenarios requires updating the exact Rust count and README too.
assert.equal(scenarios.length,304,'domain oracle scenario count');
const output=resolve(process.argv[2]);await mkdir(output,{recursive:true});
await writeFile(join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,node:process.version,scenarios},null,2)+'\n');
console.log(`PASS: ${scenarios.length} real TS domain cases`);
