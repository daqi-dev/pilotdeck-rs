// Real TS slow write: native descriptors gated for longer than the production lease.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, open, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
const baseline = resolve(process.env.PDRS_BASELINE!);
const output = resolve(process.argv[2]);
assert.equal(process.version, 'v22.22.0');
const git = execFileSync('which', ['git'], {encoding:'utf8'}).trim();
const bound = bindBaseline({ baseline, commit: 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', gitExecutable: git }).provenance;
const sources = ['src/context/budget/ToolResultBudget.ts', 'src/context/budget/ToolResultSpillPort.ts'];
const { ToolResultBudget } = await import(pathToFileURL(join(baseline,sources[0])).href);
const { createNodeToolResultSpillPort } = await import(pathToFileURL(join(baseline,sources[1])).href);
const scratch = await mkdtemp(join(tmpdir(),'pdrs-spill-lease-'));
const deferred = () => { let resolve; const promise=new Promise<void>(r=>resolve=r); return {promise,resolve}; };
const message = text => ({role:'user',content:[{type:'tool_result',toolCallId:'call',content:[{type:'text',text}]}]});
const SLOW_MS=30_050;
const watchdog=setTimeout(()=>{ console.error('probe watchdog'); process.exit(1); },45_000);
try {
 const rows=await Promise.all([false,true].map(async follower=>{
  const root=join(scratch,follower?'with':'without');
  const dir=join(root,'.pilotdeck/tool-results/session');
  const entered=deferred(),release=deferred(); let fired=false;
  const native=createNodeToolResultSpillPort();
  const port={...native,async writeTextIfAbsent(path,content){
   if(fired) return native.writeTextIfAbsent(path,content);
   fired=true; await mkdir(dirname(path),{recursive:true}); const file=await open(path,'wx',0o600);
   try {await file.writeFile(content.slice(0,4)); entered.resolve(); await release.promise; await file.writeFile(content.slice(4));}
   finally {await file.close();} return {created:true};
  }};
  const budget=new ToolResultBudget({toolResultsDir:dir,maxResultSizeChars:1,previewBytes:100,spillPort:port});
  const leader=budget.applyToMessage(message('XXXXXXXX'),{turnId:'t'});
  await entered.promise;
  const started=performance.now(); await new Promise(r=>setTimeout(r,SLOW_MS));
  assert(performance.now()-started>=30_000);
  let followerBytes=null;
  if(follower){const value=await budget.applyToMessage(message('YYYYYYYY'),{turnId:'t'});followerBytes=await readFile(join(root,value.content[0].readFilePath),'utf8');}
  release.resolve(); const ref=(await leader).content[0];
  const row={name:follower?'slow-with-follower':'slow-without-follower',heldBeyond30s:true,
    artifactName:basename(ref.path),flat:dirname(ref.path)===dir,
    leaderBytes:await readFile(ref.path,'utf8'),leaderAliasBytes:await readFile(join(root,ref.readFilePath),'utf8'),followerBytes};
  assert.equal(row.leaderBytes,'XXXXXXXX'); assert.equal(row.leaderAliasBytes,'XXXXXXXX'); assert(row.flat);
  assert.equal(followerBytes,follower?'XXXX':null);return row;
 }));
 await mkdir(output,{recursive:true});
 await writeFile(join(output,'oracle.json'),JSON.stringify({sourceCommit:bound.ref,sourceSha256:sourceClosureSha256(baseline,bound.ref,git,sources),node:process.version,rows})+'\n',{flag:'wx'});
 console.log(JSON.stringify(rows));
} finally {clearTimeout(watchdog);await rm(scratch,{recursive:true,force:true});}
