// #484 repair 1. Read-only real TS probes; never derive expectations from Rust.
import assert from 'node:assert/strict';
import {execFileSync, spawnSync} from 'node:child_process';
import {realpathSync} from 'node:fs';
import {chmod, mkdir, mkdtemp, rm, symlink, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';
const baseline=resolve(process.env.PDRS_BASELINE!);
const commandPath='src/extension/plugins/loading/PluginCommandLoader.ts';
const loaderPath='src/extension/plugins/loading/PluginLoader.ts';
const runtimePath='src/extension/plugins/runtime/PluginRuntime.ts';
if(process.argv[2]==='--child') {
  const {loadPluginCommands}=await import(pathToFileURL(join(baseline,commandPath)).href);
  const result=await loadPluginCommands({pluginName:'clique',baseDir:process.argv[3]});
  console.log(JSON.stringify({commandCount:result.length}));
} else {
  assert.ok(process.env.PDRS_BASELINE,'PDRS_BASELINE required');
  assert.ok(process.argv[2],'output directory required');
  const sourceCommit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
  const git=realpathSync(execFileSync('which',['git'],{encoding:'utf8'}).trim());
  bindSourceTree(baseline,sourceCommit,git);
  const gitEnv=Object.fromEntries(Object.entries(process.env).filter(([k])=>!k.startsWith('GIT_')));
  assert.equal(execFileSync(git,['--no-replace-objects','-C',baseline,'status','--porcelain','--untracked-files=no'],{env:gitEnv}).toString(),'');
  assert.equal(new Intl.Collator().resolvedOptions().locale,'en-US');
  const sourceSha256=sourceClosureSha256(baseline,sourceCommit,git,[commandPath,loaderPath,runtimePath]);
  const {PluginRuntime}=await import(pathToFileURL(join(baseline,runtimePath)).href);
  const {loadPluginCommands}=await import(pathToFileURL(join(baseline,commandPath)).href);
  const {loadPluginFromPath,loadPluginOutputStylesFromPath}=await import(pathToFileURL(join(baseline,loaderPath)).href);
  const scratch=await mkdtemp(join(tmpdir(),'pdrs-484-traversal-'));
  const cases:any[]=[];
  const normalize=(v:any)=>JSON.parse(JSON.stringify(v).split(scratch).join('<root>'));
  const record=async(id:string,base:string)=>{
    try {cases.push({id,result:normalize(await loadPluginCommands({pluginName:'p',baseDir:base}))});}
    catch(e:any){cases.push({id,error:normalize({name:e.name,code:e.code,message:e.message})});}
  };
  try {
    const alias=join(scratch,'aliases');
    await mkdir(join(alias,'commands'),{recursive:true});
    await mkdir(join(alias,'shared'));
    await writeFile(join(alias,'shared/a.md'),'a');
    for(const name of ['left','right']) await symlink('../shared',join(alias,'commands',name));
    await record('aliases',join(alias,'commands'));
    const boundary=join(scratch,'boundary');
    const plugin=join(boundary,'p');
    // Similar textual prefix also checks that containment is component based.
    await mkdir(join(boundary,'p-outside/secret'),{recursive:true});
    await writeFile(join(boundary,'p-outside/secret/leak.md'),'SECRET');
    await writeFile(join(boundary,'p-outside/file.md'),'SECRET');
    await mkdir(plugin);
    await writeFile(join(plugin,'plugin.json'),'{"name":"p"}');
    for(const dir of ['commands','skills','output-styles']) {
      const base=join(plugin,dir);
      await mkdir(base);
      await writeFile(join(base,'a.md'),'safe');
      await symlink('../../p-outside/secret',join(base,'directory'));
      await symlink('../../p-outside/file.md',join(base,'file.md'));
      await symlink('directory',join(base,'chain'));
      await symlink('../../p-outside/secret/leak.md',join(base,'SKILL.md'));
      await record('boundary-'+dir,base);
    }
    const loaded=await loadPluginFromPath(plugin,'global');
    cases.push({id:'boundary-plugin',result:normalize({commands:loaded.commands,skills:loaded.skills,outputStyles:loaded.outputStyles})});
    const styles=await loadPluginOutputStylesFromPath(plugin,'global');
    cases.push({id:'boundary-style-only',result:normalize(styles.outputStyles)});
    for(const kind of ['directory','file']) {
      const base=join(scratch,'unreadable-'+kind);
      await mkdir(base);
      await writeFile(join(base,'a.md'),'safe');
      const denied=join(base,kind==='directory'?'denied':'denied.md');
      if(kind==='directory') {await mkdir(denied);await writeFile(join(denied,'hidden.md'),'hidden');}
      else await writeFile(denied,'hidden');
      await chmod(denied,0);
      try {await record('unreadable-'+kind,base);} finally {await chmod(denied,0o700);}
    }
    const deep=join(scratch,'depth');
    let leaf=deep;
    for(let i=0;i<65;i++)leaf=join(leaf,'d');
    await mkdir(leaf,{recursive:true});await writeFile(join(leaf,'a.md'),'a');
    await record('depth-65',deep);
    const wide=join(scratch,'entries');await mkdir(wide);
    for(let i=0;i<4097;i++)await writeFile(join(wide,`${String(i).padStart(5,'0')}.md`),'');
    const wideResult=await loadPluginCommands({pluginName:'p',baseDir:wide});
    cases.push({id:'entries-4097',result:{commandCount:wideResult.length}});
    const namesRoot=join(scratch,'server-names');
    await mkdir(join(namesRoot,'home/plugins/p'),{recursive:true});
    const servers={'a\ud800':{instructions:'high'},'a\udfff':{instructions:'low'},'a\ufffd':{instructions:'replacement'}};
    await writeFile(join(namesRoot,'home/plugins/p/plugin.json'),JSON.stringify({name:'p',mcpServers:servers}));
    const runtime=new PluginRuntime({projectRoot:join(namesRoot,'project'),pilotHome:join(namesRoot,'home')});
    await runtime.refresh();
    cases.push({id:'server-names',inputJson:JSON.stringify(servers),resultJson:JSON.stringify(runtime.getAllMcpInstructions())});
    await runtime.dispose();
    for(const k of [3,7,10,20]) {
      const base=join(scratch,'clique-'+k);await mkdir(base);
      for(let i=0;i<k;i++) {const dir=join(base,'d'+i);await mkdir(dir);await writeFile(join(dir,'a.md'),'a');}
      for(let i=0;i<k;i++)for(let j=0;j<k;j++)if(i!==j)await symlink('../d'+j,join(base,'d'+i,'link'+j));
      const child=spawnSync(process.execPath,[...process.execArgv,fileURLToPath(import.meta.url),'--child',base],{encoding:'utf8',timeout:1000,killSignal:'SIGKILL'});
      if(child.error&&(child.error as NodeJS.ErrnoException).code==='ETIMEDOUT')cases.push({id:'clique-'+k,timeoutMs:1000,timedOut:true});
      else {assert.equal(child.status,0,child.stderr);cases.push({id:'clique-'+k,timedOut:false,result:JSON.parse(child.stdout)});}
    }
  } finally {await rm(scratch,{recursive:true,force:true});}
  const output=resolve(process.argv[2]);await mkdir(output,{recursive:true});
  await writeFile(join(output,'traversal.json'),JSON.stringify({sourceCommit,node:process.version,sourceSha256,cases},null,2)+'\n');
  console.log(JSON.stringify(cases.map(c=>({id:c.id,error:c.error,timedOut:c.timedOut,count:Array.isArray(c.result)?c.result.length:c.result?.commandCount}))));
}
