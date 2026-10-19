import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {sha,bind} from './guard.mjs';
import {cases} from './domain-matrix.mjs';
import {compare} from './domain-compare.mjs';
import {checkControls} from './domain-controls.mjs';
const [out,baseline,oldSix]=process.argv.slice(2);assert.ok(out&&baseline&&oldSix);
for(const p of [out,baseline,oldSix])assert.ok(path.isAbsolute(p));assert.ok(!fs.existsSync(out),'preserve previous runs');
const scriptRoot=path.dirname(fileURLToPath(import.meta.url));
const scriptNames=['domain-run.mjs','domain-child.mjs','domain-matrix.mjs','domain-commit.mjs','domain-compare.mjs','domain-controls.mjs','domain-transport.mjs','domain-preregister.md','protocol.mjs','guard.mjs','fixture.mjs','offline.mjs'];
const scripts=Object.fromEntries(scriptNames.map(n=>[n,sha(fs.readFileSync(path.join(scriptRoot,n)))]));
const six=JSON.parse(fs.readFileSync(oldSix));assert.equal(six.sourceHead,'379ec5191650b0285d98afbcc2f3db1bec9c2cf5');
const tools=Object.fromEntries(['node','git','loader'].map(key=>[key,{path:six.tools[key],sha256:six.tools[key+'Sha256']} ]));
for(const [key,value]of Object.entries(tools))assert.equal(sha(fs.readFileSync(value.path)),value.sha256,'old tool identity '+key);
assert.equal(fs.realpathSync(process.execPath),fs.realpathSync(tools.node.path));
const oldBlobs=six.cases.map(item=>{const p=path.join(path.dirname(oldSix),item.path);assert.equal(sha(fs.readFileSync(p)),item.sha256);return {path:p,sha256:item.sha256};});
const native=oldBlobs[0];const before=bind(baseline,tools.git.path);
const preregCommit=execFileSync(tools.git.path,['-C',scriptRoot,'log','-1','--format=%H','--','domain-preregister.md']).toString().trim();assert.ok(preregCommit.startsWith('756658d'),'preregistration must precede execution');
const sizeTree=p=>{let n=0;for(const e of fs.readdirSync(p,{withFileTypes:true})){const q=path.join(p,e.name);n+=e.isDirectory()?sizeTree(q):fs.statSync(q).blocks*512;}return n;};
const budgetRoot=path.dirname(out);assert.ok(sizeTree(budgetRoot)<12*1024*1024,'reserve 4MiB within shared 16MiB budget before a new child pair');
fs.mkdirSync(out);const scratch=path.join(out,'isolated');fs.mkdirSync(scratch);
const env={TSX_DISABLE_CACHE:'1',PATH:path.dirname(tools.node.path)+':/usr/bin:/bin',PDRS_ORDER_PROBE:'owned-default-six',HOME:path.join(scratch,'home'),XDG_CONFIG_HOME:path.join(scratch,'config'),XDG_CACHE_HOME:path.join(scratch,'cache'),XDG_DATA_HOME:path.join(scratch,'data'),TMPDIR:path.join(scratch,'tmp'),PILOT_HOME:path.join(scratch,'pilot'),PILOTDECK_SKIP_BROWSER_OPEN:'1',PILOT_SKIP_BROWSER_OPEN:'1',SKIP_BROWSER_OPEN:'1',PILOTDECK_SKIP_BROWSER:'1'};
for(const key of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR','PILOT_HOME'])fs.mkdirSync(env[key],{recursive:true});
const children=[];let failure;let summary;const started=Date.now();
async function run(mode,oracle) {
  assert.ok(Date.now()-started<600000,'total execution window');
  const output=path.join(out,mode+'.json');const spec={mode,output,scratch,baseline,tools,scriptRoot,scripts,native,oracle};const specPath=path.join(out,mode+'-spec.json');fs.writeFileSync(specPath,JSON.stringify(spec,null,2)+'\n');
  const argv=['--import',path.join(scriptRoot,'offline.mjs'),'--import',tools.loader.path,path.join(scriptRoot,'domain-child.mjs'),specPath];
  const child=spawn(tools.node.path,argv,{cwd:scratch,env,stdio:['ignore','pipe','pipe']});let bytes=0;const chunks=[];let killed=false;
  const consume=data=>{bytes+=data.length;if(bytes>1048576){killed=true;child.kill('SIGKILL');}else chunks.push(data);};child.stdout.on('data',consume);child.stderr.on('data',consume);
  const timer=setTimeout(()=>{killed=true;child.kill('SIGKILL');},60000);
  const exit=await new Promise(resolve=>{child.once('error',error=>resolve({spawnError:String(error)}));child.once('close',(code,signal)=>resolve({code,signal}));});clearTimeout(timer);
  fs.writeFileSync(path.join(out,mode+'-child.log'),Buffer.concat(chunks));
  children.push({mode,pid:child.pid,argv:[tools.node.path,...argv],specPath,specSha256:sha(fs.readFileSync(specPath)),exit,killed,outputBytes:bytes,childReaped:child.exitCode!==null||child.signalCode!==null});
  assert.equal(exit.code,0,'child failed '+mode);assert.equal(killed,false);assert.ok(fs.statSync(output).size<=1048576,'child report bound');assert.ok(sizeTree(budgetRoot)<=16*1024*1024,'shared evidence budget');return JSON.parse(fs.readFileSync(output));
}
try {
  const oracle=await run('oracle');const oracleIdentity={path:path.join(out,'oracle.json'),sha256:sha(fs.readFileSync(path.join(out,'oracle.json')))};
  fs.writeFileSync(path.join(out,'oracle-fixed.json'),JSON.stringify({preregCommit,oracle:oracleIdentity,fixedBeforeCandidate:true},null,2)+'\n');
  const candidate=await run('candidate',oracleIdentity);summary=compare(oracle,candidate);
  const controls=checkControls(oracle,candidate);
  summary.controls=controls;summary.nativeOriginals={sourceHead:six.sourceHead,sixIndex:{path:oldSix,sha256:sha(fs.readFileSync(oldSix))},blobs:oldBlobs,executedCurrentRust:false};
}catch(error){failure={message:String(error),stack:error.stack};}
finally {
  let after;try {after=bind(baseline,tools.git.path);assert.deepEqual(after,before);for(const [key,value]of Object.entries(tools))assert.equal(sha(fs.readFileSync(value.path)),value.sha256);for(const [name,digest]of Object.entries(scripts))assert.equal(sha(fs.readFileSync(path.join(scriptRoot,name))),digest);for(const blob of oldBlobs)assert.equal(sha(fs.readFileSync(blob.path)),blob.sha256);}catch(error){failure??={message:'postflight '+String(error)};}
  let totalBytes=0;const visit=p=>{for(const item of fs.readdirSync(p,{withFileTypes:true})){const name=path.join(p,item.name);if(item.isDirectory())visit(name);else totalBytes+=fs.statSync(name).size;}};visit(out);if(sizeTree(budgetRoot)>16*1024*1024)failure??={message:'16MiB evidence budget exceeded'};
  fs.writeFileSync(path.join(out,'runner.json'),JSON.stringify({status:failure?'FAIL':'RECORDED_NOT_ACCEPTED',failure,summary,children,tools,scripts,preregCommit,sourceBefore:before,sourceAfter:after,elapsedMs:Date.now()-started,evidenceBytesBeforeRunner:totalBytes,environment:env,noCargo:true},null,2)+'\n');
}
if(failure){process.stderr.write(failure.message+'\n');process.exitCode=1;}else process.stdout.write(JSON.stringify({status:summary.status,rows:cases.length,controls:summary.controls.length})+'\n');
