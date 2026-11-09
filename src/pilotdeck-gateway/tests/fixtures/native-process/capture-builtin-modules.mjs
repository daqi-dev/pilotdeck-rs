import assert from 'node:assert/strict';
import {readFileSync,writeFileSync,mkdirSync,existsSync,realpathSync} from 'node:fs';
import {createRequire,registerHooks} from 'node:module';
import {pathToFileURL,fileURLToPath} from 'node:url';
import {join,resolve,dirname,basename} from 'node:path';
import {createHash} from 'node:crypto';
const args=process.argv.slice(2);assert.equal(args.length,2,'usage: capture-builtin-modules.mjs BASELINE OUT_DIR');
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
const {loadPilotConfig}=await import(pathToFileURL(join(base,'src/pilot/config/loadPilotConfig.ts')));
const selectors=await import(pathToFileURL(join(base,'src/composition/types.ts')));
const {createRuntimeModulePorts}=await import(pathToFileURL(join(base,'src/composition/runtimePorts.ts')));
const {createAgentLoopBindingFactory}=await import(pathToFileURL(join(base,'src/cli/AgentLoopDeploymentProfile.ts')));
const core=['agentLoop','skills','modelProvider','tools','context','knowledge'];
const builtin={enabled:true,provider:'pilotdeck'};
const cases=[{id:'absent',modules:undefined},...core.map(slot=>({id:'builtin-'+slot,modules:{[slot]:builtin}})),{id:'all-builtin',modules:Object.fromEntries(core.map(s=>[s,builtin]))},{id:'skills-disabled',modules:{skills:{enabled:false}}},{id:'knowledge-disabled',modules:{knowledge:{enabled:false}}},{id:'builtin-extra-unknown',modules:{skills:{...builtin,unknown:7}}},{id:'unknown-module',modules:{unknown:builtin}},{id:'external-tools',modules:{tools:{enabled:true,implementationId:'fixture.tools',contract:'pilotdeck.tools/v1',transport:'module-http-v2',endpoint:'http://127.0.0.1:1',catalog:[{name:'fixture_echo',description:'Fixture echo',inputSchema:{type:'object'}}]}}},{id:'invalid-required-disable',modules:{tools:{enabled:false}}},{id:'invalid-builtin-provider',modules:{skills:{enabled:true,provider:'custom'}}}];
const common={schemaVersion:1,agent:{model:'fixture/mock'},model:{providers:{fixture:{protocol:'openai',url:'http://127.0.0.1:1/v1',apiKey:'fake-only',models:{mock:{}}}}}};
const rows=[];
for(const c of cases){
 const dir=join(out,c.id),home=join(dir,'home'),project=join(dir,'project');mkdirSync(dir);mkdirSync(home);mkdirSync(project);
 const raw={...common,...(c.modules===undefined?{}:{modules:c.modules})};const yaml=JSON.stringify(raw);const configPath=join(home,'pilotdeck.yaml');writeFileSync(configPath,yaml+'\n');
 let result;
 try{
  const snap=loadPilotConfig({env:{HOME:home,PILOT_HOME:home,LANG:'en_US.UTF-8'},configPath,projectRoot:project,version:1});
  const modules=snap.config.modules;
  result={ok:true,modules:modules??null,diagnostics:snap.diagnostics};
  if(c.id!=='external-tools'){
   const ports=createRuntimeModulePorts(modules,'fixture-session');assert.deepEqual(Object.keys(ports),[]);
   assert.equal(createAgentLoopBindingFactory(modules?.agentLoop),undefined);
   result.selectors={runtimePortKeys:Object.keys(ports),agentLoopFactoryAbsent:true,skillsDisabled:selectors.isDisabledModuleBinding(modules?.skills),skillsExternal:selectors.isExternalModuleBinding(modules?.skills),knowledgeExternal:selectors.isExternalModuleBinding(modules?.knowledge)};
  } else { assert.equal(selectors.isExternalModuleBinding(modules.tools),true);result.externalTools=true; }
 } catch(e) { result={ok:false,error:{name:e.name,message:e.message,diagnostics:e.diagnostics}}; }
 if(c.id.startsWith('invalid-'))assert.equal(result.ok,false,c.id);else assert.equal(result.ok,true,c.id);
 rows.push({id:c.id,yaml,output:result});
}
await new Promise(r=>setImmediate(r));assert.deepEqual(warnings,[{name:'ExperimentalWarning',message:'SQLite is an experimental feature and might change at any time'}]);
writeFileSync(join(out,'oracle.json'),JSON.stringify({metadata:{sourceCommit:process.env.PDRS_VERIFIED_TS_SHA,node:process.version,icu:process.versions.icu,typescript:ts.version,sourceSha256:hashes,method:'full real loadPilotConfig and real runtime module/agent-loop selectors through a self-written transpile-only loader; no provider or external module invocation',projection:'config.modules, diagnostics and selector results; no time/path normalization',warningPolicy:'exactly one known Node22 builtin SQLite ExperimentalWarning from module import, recorded raw; every other warning fails',warnings},rows},null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({cases:rows.length,sourceFiles:Object.keys(hashes).length}));
