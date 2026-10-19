import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {bind,sha} from './guard.mjs';
const directory=path.dirname(fileURLToPath(import.meta.url));
const scriptNames=['run.mjs','guard.mjs','probe.mjs','protocol.mjs','offline.mjs','fixture.mjs','stdio-host.mjs'];
const scriptsBefore=Object.fromEntries(scriptNames.map(name=>[name,sha(fs.readFileSync(path.join(directory,name)))]));
const flags=new Map();
for(let i=2;i<process.argv.length;i+=2){const k=process.argv[i],v=process.argv[i+1];assert.ok(k?.startsWith('--')&&v&&!flags.has(k),'unique --key value arguments required');flags.set(k,v);}
const required=['node','node-sha256','git','git-sha256','loader','loader-sha256','baseline','output'];
const allowed=[...required,'cargo-binary','cargo-sha256'];
for(const key of flags.keys())assert.ok(allowed.includes(key.slice(2)),'unknown option '+key);
for(const key of required)assert.ok(flags.has('--'+key),'missing --'+key);
assert.equal(flags.has('--cargo-binary'),flags.has('--cargo-sha256'),'optional Rust identity requires path and SHA together');
const get=key=>flags.get('--'+key);
for(const key of ['node','git','loader','baseline','output',...(get('cargo-binary')?['cargo-binary']:[])])assert.ok(path.isAbsolute(get(key)),'absolute --'+key+' required');
const identity={};
for(const key of ['node','git','loader',...(get('cargo-binary')?['cargo-binary']:[])]){
  const p=fs.realpathSync(get(key)),digest=sha(fs.readFileSync(p));
  assert.equal(digest,get(key==='cargo-binary'?'cargo-sha256':key+'-sha256'),'identity mismatch: '+key);
  identity[key]={path:p,sha256:digest};
}
let workspace=directory;
while(!fs.existsSync(path.join(workspace,'.git'))){const parent=path.dirname(workspace);assert.notEqual(parent,workspace,'workspace .git missing');workspace=parent;}
const marker=path.join(workspace,'.git');
const gitDirectory=fs.statSync(marker).isDirectory()?marker:path.resolve(workspace,fs.readFileSync(marker,'utf8').trim().replace(/^gitdir: /,''));
const commonFile=path.join(gitDirectory,'commondir');
const common=fs.existsSync(commonFile)?path.resolve(gitDirectory,fs.readFileSync(commonFile,'utf8').trim()):gitDirectory;
const mainWorkspace=path.dirname(common);
const baseline=fs.realpathSync(get('baseline'));
for(const key of ['node','git']){
  fs.accessSync(identity[key].path,fs.constants.X_OK);
  for(const root of [workspace,mainWorkspace,baseline])assert.ok(identity[key].path!==root&&!identity[key].path.startsWith(root+path.sep),'interpreter inside source workspace: '+key);
}
const before=bind(baseline,identity.git.path);
const out=get('output'),parent=fs.realpathSync(path.dirname(out));
const evidenceRoots=[workspace,mainWorkspace].map(root=>path.join(root,'_tmp'));
assert.ok(evidenceRoots.some(root=>parent===root||parent.startsWith(root+path.sep)),'output parent must be under workspace _tmp');
assert.ok(!fs.existsSync(out),'refuse to overwrite output directory');
fs.mkdirSync(out);const scratch=path.join(out,'isolated');fs.mkdirSync(scratch);
const env={PATH:path.dirname(identity.node.path)+':/usr/bin:/bin',PDRS_ORDER_PROBE:'owned-default-six',
  HOME:path.join(scratch,'home'),XDG_CONFIG_HOME:path.join(scratch,'config'),XDG_CACHE_HOME:path.join(scratch,'cache'),XDG_DATA_HOME:path.join(scratch,'data'),TMPDIR:path.join(scratch,'tmp'),PILOT_HOME:path.join(scratch,'pilot'),
  PILOTDECK_SKIP_BROWSER_OPEN:'1',PILOT_SKIP_BROWSER_OPEN:'1',SKIP_BROWSER_OPEN:'1',PILOTDECK_SKIP_BROWSER:'1'};
for(const key of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR','PILOT_HOME'])fs.mkdirSync(env[key],{recursive:true});
const argv=['--import',path.join(directory,'offline.mjs'),'--import',identity.loader.path,path.join(directory,'probe.mjs'),baseline,path.join(out,'result.json'),scratch];
const log=fs.openSync(path.join(out,'child.log'),'wx');
const child=spawn(identity.node.path,argv,{cwd:scratch,env,stdio:['ignore','pipe','pipe']});
const logLimit=1024*1024;let logBytes=0,receivedLogBytes=0,outputLimitExceeded=false,logFailure;
const capture=chunk=>{
  receivedLogBytes+=chunk.length;
  const accepted=Math.min(chunk.length,logLimit-logBytes);
  try{if(accepted){const written=fs.writeSync(log,chunk,0,accepted);logBytes+=written;if(written!==accepted)throw new Error('short owned child log write');}}
  catch(error){logFailure??=String(error);child.kill('SIGKILL');}
  if(accepted<chunk.length){outputLimitExceeded=true;child.kill('SIGKILL');}
};
child.stdout.on('data',capture);child.stderr.on('data',capture);
for(const stream of [child.stdout,child.stderr])stream.on('error',error=>{logFailure??=String(error);child.kill('SIGKILL');});
let killed=false;const watchdog=setTimeout(()=>{killed=true;child.kill('SIGKILL');},30000);
let spawnError;
const closed=new Promise(resolve=>{child.once('error',error=>{spawnError=String(error);});child.once('close',(code,signal)=>resolve({code,signal,spawnError}));});
let exit;
try{exit=await closed;}
finally{
  clearTimeout(watchdog);
  if(child.exitCode===null&&child.signalCode===null){child.kill('SIGKILL');await closed;}
  fs.closeSync(log);
}
let after,postflightError;try{after=bind(baseline,identity.git.path);assert.deepEqual(after,before);}catch(error){postflightError=String(error);}
const scripts={};for(const name of scriptNames)scripts[name]=sha(fs.readFileSync(path.join(directory,name)));
try{assert.deepEqual(scripts,scriptsBefore);for(const [key,value]of Object.entries(identity))assert.equal(sha(fs.readFileSync(value.path)),value.sha256,'postflight identity '+key);}catch(error){postflightError??=String(error);}
fs.writeFileSync(path.join(out,'runner.json'),JSON.stringify({status:exit.code===0&&!killed&&!postflightError&&!outputLimitExceeded&&!logFailure?'RECORDED_NOT_ACCEPTED':'FAIL',argv:[identity.node.path,...argv],identity,sourceBefore:before,sourceAfter:after,postflightError,scriptsBefore,scriptsAfter:scripts,exit,killed,outputLimitExceeded,logFailure,logLimit,logBytes,receivedLogBytes,childReaped:true,rust:'NOT_RUN',cargoBinaryExecuted:false,environment:env},null,2)+'\n');
process.exitCode=exit.code===0&&!killed&&!postflightError&&!outputLimitExceeded&&!logFailure?0:1;
