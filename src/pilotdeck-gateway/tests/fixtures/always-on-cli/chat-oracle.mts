import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdir,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const root=resolve(process.env.PDRS_BASELINE??'');
assert.equal(process.version,'v22.22.0');
assert.equal(execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const sources=['src/always-on/tool/AlwaysOnChatHistoryTool.ts','src/session/transcript/TranscriptReplay.ts','src/session/projection/AgentTranscriptProjections.ts'];
const verify=()=>{for(const source of sources) assert.equal(execFileSync('git',['-C',root,'hash-object',source],{encoding:'utf8'}).trim(),execFileSync('git',['-C',root,'rev-parse',`HEAD:${source}`],{encoding:'utf8'}).trim());};
verify();const out=process.argv[2];assert.ok(out?.startsWith('/private/tmp/'));await mkdir(out,{recursive:false});
const {createAlwaysOnChatHistoryTool}=await import(pathToFileURL(join(root,sources[0])).href);
const msg=(role:string,text:string)=>({role,content:[{type:'text',text}]});
const records=[];
for(const scenario of ['normal','incomplete','empty','alias','truncate','compact','split-surrogate']){
 const calls=[];
 const entries=scenario==='empty'?[]:[
  {type:'session_metadata',sessionId:'real',turnId:'t',metadata:{title:'Saved title'}},
  {type:'accepted_input',sessionId:'real',turnId:'t',messages:[msg('user','  user one  ')]},
  {type:'assistant_message',sessionId:'real',turnId:'t',message:msg('assistant',scenario==='truncate'?'x'.repeat(301):scenario==='split-surrogate'?'x'.repeat(299)+'🙂':' answer ')},
  ...(scenario==='compact'?[{type:'control_boundary',sessionId:'real',turnId:'t',boundary:{kind:'compact',subtype:'compact_boundary',snapshot:{version:1,messages:[msg('user','summary')]}}}]:[]),
  ...(scenario==='incomplete'?[]:[{type:'turn_result',sessionId:'real',turnId:'t',result:{type:'success',sessionId:'real',turnId:'t',stopReason:'completed',usage:{},permissionDenials:[],turns:1,startedAt:'2026-01-02T03:04:05.678Z',completedAt:'2026-01-02T03:05:05.678Z'}}]),
 ];
 const tool=createAlwaysOnChatHistoryTool({
  runContexts:{getDiscovery:()=>({projectKey:'/project',paths:{pilotHome:'/home'},runId:'run',chatSessionAliases:new Map([['alias','real']])})},
  sessionTranscriptReader:{read:async (input:unknown)=>{calls.push(['read',input]);return {entries,diagnostics:[]};}},
  sessionCatalog:{list:async (input:unknown)=>{calls.push(['list',input]);return [{sessionId:'real',summary:'catalog title'}];}},
 } as never);
 try { records.push({scenario,entries,calls,result:await tool.execute({sessionId:scenario==='alias'?'alias':'real'},{sessionId:'phase'} as never)}); }
 catch(e){records.push({scenario,entries,calls,error:{code:e?.code,message:e?.message}});}
}
verify();await writeFile(join(out,'oracle.json'),JSON.stringify({records},null,2)+'\n');
