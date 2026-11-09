import assert from 'node:assert/strict';
import {readFileSync,writeFileSync,mkdirSync,existsSync,realpathSync} from 'node:fs';
import {createRequire,registerHooks} from 'node:module';
import {pathToFileURL,fileURLToPath} from 'node:url';
import {join,resolve,dirname,basename} from 'node:path';
import {createHash} from 'node:crypto';
const args=process.argv.slice(2);assert.equal(args.length,2,'usage: capture-transport-env.mjs BASELINE OUT_DIR');
const base=realpathSync(resolve(args[0])),requested=resolve(args[1]);
const out=join(realpathSync(dirname(requested)),basename(requested));
assert.ok(out!==base&&!out.startsWith(base+'/'),'output must not be inside baseline');
mkdirSync(out,{mode:0o700});
assert.equal(process.version,'v22.22.0'); assert.equal(process.versions.icu,'78.2');assert.equal(process.env.PDRS_VERIFIED_TS_SHA,'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const req=createRequire(join(base,'package.json')),ts=req('typescript'); assert.equal(ts.version,'5.9.3');
const hashes={},warnings=[];process.on('warning',w=>warnings.push({name:w.name,message:w.message}));
function hashFile(path){return createHash('sha256').update(readFileSync(path)).digest('hex');}
// Synchronous TS loader, no esbuild subprocess; baseline sources stay read-only.
registerHooks({
 resolve(specifier,context,next){
  if((specifier.startsWith('.')||specifier.startsWith('/'))&&specifier.endsWith('.js')) {
   const url=new URL(specifier,context.parentURL);const candidate=fileURLToPath(url).slice(0,-3)+'.ts';
   if(existsSync(candidate)) return {url:pathToFileURL(candidate).href,shortCircuit:true};
  }
  return next(specifier,context);
 },
 load(url,context,next){
  if(url.startsWith('file:')&&url.endsWith('.ts')){
   const file=fileURLToPath(url),text=readFileSync(file,'utf8');
   if(file.startsWith(base+'/'))hashes[file.slice(base.length+1)]=hashFile(file);
   return {format:'module',source:ts.transpileModule(text,{compilerOptions:{module:ts.ModuleKind.ESNext,target:ts.ScriptTarget.ES2022,esModuleInterop:true}}).outputText,shortCircuit:true};
  }
  return next(url,context);
 }
});
const {resolveAgentLoopDeploymentProfile}=await import(pathToFileURL(join(base,'src/cli/AgentLoopDeploymentProfile.ts')));
const moduleDefault=join(base,'src/cli/pilotdeck-agent-loop-sidecar.js');
const cases=[
 ['default',{}],['empty',{PILOTDECK_AGENT_LOOP_TRANSPORT:''}],['native-case',{PILOTDECK_AGENT_LOOP_TRANSPORT:' NaTiVe '}],
 ['native-ignores-overrides',{PILOTDECK_AGENT_LOOP_TRANSPORT:'native',PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND:'',PILOTDECK_AGENT_LOOP_SIDECAR_PATH:'bad',PILOTDECK_AGENT_LOOP_TCP_PORT:'not-a-port'}],
 ['feff-native',{PILOTDECK_AGENT_LOOP_TRANSPORT:'﻿native﻿'}],['nel-native-invalid',{PILOTDECK_AGENT_LOOP_TRANSPORT:'native'}],
 ['stdio-default',{PILOTDECK_AGENT_LOOP_TRANSPORT:'stdio'}],
 ['stdio-command',{PILOTDECK_AGENT_LOOP_TRANSPORT:'stdio',PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND:' /fixture/command '}],
 ['stdio-relative-path',{PILOTDECK_AGENT_LOOP_TRANSPORT:'stdio',PILOTDECK_AGENT_LOOP_SIDECAR_PATH:'./custom.mjs'}],
 ['stdio-raw-spaces-path',{PILOTDECK_AGENT_LOOP_TRANSPORT:'stdio',PILOTDECK_AGENT_LOOP_SIDECAR_PATH:' ./custom.js '}],
 ['stdio-feff-path',{PILOTDECK_AGENT_LOOP_TRANSPORT:'stdio',PILOTDECK_AGENT_LOOP_SIDECAR_PATH:'﻿'}],
 ['stdio-both-and-env',{PILOTDECK_AGENT_LOOP_TRANSPORT:'STDIO',PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND:'/fixture/custom',PILOTDECK_AGENT_LOOP_SIDECAR_PATH:'child',PILOTDECK_AGENT_LOOP_TCP_HOST:'localhost',PILOTDECK_AGENT_LOOP_TCP_PORT:'1234',PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS:'23',KEEP:'fixture'}],
 ['stdio-blank-command',{PILOTDECK_AGENT_LOOP_TRANSPORT:'stdio',PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND:'﻿'}],
 ['tcp-default-host',{PILOTDECK_AGENT_LOOP_TRANSPORT:'tcp',PILOTDECK_AGENT_LOOP_TCP_PORT:'1000'}],
 ['tcp-hex-port',{PILOTDECK_AGENT_LOOP_TRANSPORT:'tcp',PILOTDECK_AGENT_LOOP_TCP_PORT:'0x50',PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS:'1e3'}],
 ['tcp-feff-values',{PILOTDECK_AGENT_LOOP_TRANSPORT:'tcp',PILOTDECK_AGENT_LOOP_TCP_HOST:'﻿localhost﻿',PILOTDECK_AGENT_LOOP_TCP_PORT:'﻿80﻿',PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS:'﻿'}],
 ['tcp-no-port',{PILOTDECK_AGENT_LOOP_TRANSPORT:'tcp'}],['tcp-blank-host',{PILOTDECK_AGENT_LOOP_TRANSPORT:'tcp',PILOTDECK_AGENT_LOOP_TCP_HOST:' ',PILOTDECK_AGENT_LOOP_TCP_PORT:'80'}],
 ['tcp-blank-port',{PILOTDECK_AGENT_LOOP_TRANSPORT:'tcp',PILOTDECK_AGENT_LOOP_TCP_PORT:'﻿'}],
 ['tcp-invalid-port',{PILOTDECK_AGENT_LOOP_TRANSPORT:'tcp',PILOTDECK_AGENT_LOOP_TCP_PORT:'80.5'}],
 ['tcp-invalid-timeout',{PILOTDECK_AGENT_LOOP_TRANSPORT:'tcp',PILOTDECK_AGENT_LOOP_TCP_PORT:'80',PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS:'0'}],
 ['invalid-transport',{PILOTDECK_AGENT_LOOP_TRANSPORT:'socket'}],
];
const rows=[];
for(const [id,env] of cases){
 const before=JSON.stringify(env);let output;
 try{
  const p=resolveAgentLoopDeploymentProfile({env,cwd:'/fixture/project'});
  const profile=JSON.parse(JSON.stringify(p));
  // Declared before capture: only Node exec and baseline-default JS artifact
  // are install-specific; explicit custom paths/whitespace remain exact.
  if(profile.command===process.execPath)profile.command='$NODE_EXEC';
  if(profile.args)profile.args=profile.args.map(v=>v===moduleDefault?'$DEFAULT_SIDECAR':v);
  output={ok:true,profile,frozen:Object.isFrozen(p),...(p.args?{argsFrozen:Object.isFrozen(p.args)}:{})};
 }catch(e){output={ok:false,error:{name:e.name,message:e.message}};}
 assert.equal(JSON.stringify(env),before);rows.push({id,env,cwd:'/fixture/project',output});
}
await new Promise(r=>setImmediate(r));assert.deepEqual(warnings,[{name:'ExperimentalWarning',message:'SQLite is an experimental feature and might change at any time'}]);
writeFileSync(join(out,'oracle.json'),JSON.stringify({metadata:{sourceCommit:process.env.PDRS_VERIFIED_TS_SHA,node:process.version,icu:process.versions.icu,typescript:ts.version,sourceSha256:hashes,method:'actual full module resolveAgentLoopDeploymentProfile; no child or transport execution',normalization:'only process.execPath => $NODE_EXEC and original module default sidecar path => $DEFAULT_SIDECAR; explicit paths/environment stay exact',warningPolicy:'exactly one known Node22 builtin SQLite ExperimentalWarning; other or duplicate warnings fail',warnings},rows},null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({cases:rows.length,sourceFiles:Object.keys(hashes).length}));
