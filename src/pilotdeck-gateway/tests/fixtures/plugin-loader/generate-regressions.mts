// #484: real TS UTF-16, Map-order and bounded cycle probes. Baseline is read-only.
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
const baseline = resolve(process.env.PDRS_BASELINE!);
const runtimePath = 'src/extension/plugins/runtime/PluginRuntime.ts';
const loaderPath = 'src/extension/plugins/loading/PluginLoader.ts';
if (process.argv[2] === '--cycle-child') {
  // Parent has already bound the source tree, creates/removes the scratch root,
  // and kills this process on deadline. No recursive teardown inside the child.
  const { loadPluginFromPath } = await import(pathToFileURL(join(baseline, loaderPath)).href);
  const plugin = await loadPluginFromPath(process.argv[3], 'global');
  console.log(JSON.stringify({ commandCount: plugin.commands.length }));
} else {
  assert.ok(process.env.PDRS_BASELINE, 'PDRS_BASELINE required');
  assert.ok(process.argv[2], 'output directory required');
  const output = resolve(process.argv[2]);
  const sourceCommit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
  const git = realpathSync(execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
  bindBaseline({ baseline: baseline, commit: sourceCommit, gitExecutable: git });
  const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('GIT_')));
  assert.equal(execFileSync(git, ['--no-replace-objects','-C',baseline,'status','--porcelain','--untracked-files=no'], {env:gitEnv}).toString(), '');
  assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US');
  const sourceSha256 = sourceClosureSha256(baseline, sourceCommit, git, [runtimePath, loaderPath]);
  const { PluginRuntime } = await import(pathToFileURL(join(baseline, runtimePath)).href);
  const scratch = await mkdtemp(join(tmpdir(), 'pdrs-484-ts-'));
  const instructions:any[] = [];
  const inputs = [
    ['ascii-2047', 'a'.repeat(2047)], ['ascii-2048', 'a'.repeat(2048)], ['ascii-2049', 'a'.repeat(2049)],
    ['cjk-2047', '中'.repeat(2047)], ['cjk-2048', '中'.repeat(2048)], ['cjk-2049', '中'.repeat(2049)],
    ['utf8-boundary', 'a'.repeat(2047)+'中中'],
    ['pair-at-boundary', 'a'.repeat(2046)+'😀'+'x'],
    ['pair-across-boundary', 'a'.repeat(2047)+'😀'],
    ['lone-high', '\ud800'], ['lone-low', '\udfff'],
    ['trim-ecmascript', '\ufeff \t中\u00a0\ufeff'], ['not-js-space', '\u0085中\u0085'],
    ['blank', '\ufeff \n\u3000'],
  ];
  const cycleCases:any[] = [];
  let skills:any;
  let aliases:any;
  try {
    for (const [id, input] of inputs) {
      const root = join(scratch,id), path=join(root,'home/plugins/p');
      await mkdir(path,{recursive:true});
      await writeFile(join(path,'plugin.json'),JSON.stringify({name:'p',mcpServers:{server:{instructions:input}}}));
      const runtime = new PluginRuntime({projectRoot:join(root,'project'),pilotHome:join(root,'home')});
      await runtime.refresh();
      instructions.push({id, inputJson:JSON.stringify(input), resultJson:JSON.stringify(runtime.getAllMcpInstructions())});
      await runtime.dispose();
    }
    // Non-lexical insertion order, equal-priority overwrite, lower-priority
    // rejection, and higher-priority overwrite without moving the first key.
    const runtime=new PluginRuntime({projectRoot:join(scratch,'skills/project'),pilotHome:join(scratch,'skills/home')});
    const specs=[{source:'global',values:[['z','first'],['a','a'],['z','equal'],['m','m'],['a','equal-a'],['q','global-q']]},{source:'builtin',values:[['z','lower'],['b','b'],['q','lower-q']]},{source:'project',values:[['z','higher'],['y','y']]}];
    runtime.registry.replaceAll(specs.map((s,i)=>({name:'same',source:s.source,path:join(scratch,'skills',String(i)),skills:s.values.map(([name,content])=>({name,path:join(scratch,'skills',name,'SKILL.md'),content,frontmatter:{description:content},isSkill:true}))})));
    skills={specs,result:runtime.getAllSkills().map((s:any)=>({name:s.name,content:s.content,description:s.description,namespace:s.namespace}))};
    await runtime.dispose();
    const aliasRoot=join(scratch,'aliases');
    await mkdir(join(aliasRoot,'commands'),{recursive:true});
    await mkdir(join(aliasRoot,'shared'));
    await writeFile(join(aliasRoot,'shared/a.md'),'a');
    for(const name of ['left','right']) await symlink('../shared',join(aliasRoot,'commands',name));
    const {loadPluginCommands}=await import(pathToFileURL(join(baseline,'src/extension/plugins/loading/PluginCommandLoader.ts')).href);
    aliases=(await loadPluginCommands({pluginName:'alias',baseDir:join(aliasRoot,'commands')})).map((c:any)=>c.name);
    for (const links of [1,2]) {
      const root=join(scratch,'cycle-'+links);
      await mkdir(join(root,'commands'),{recursive:true});
      await writeFile(join(root,'plugin.json'),'{"name":"cycle"}');
      await writeFile(join(root,'commands/a.md'),'a');
      for(let i=0;i<links;i++) await symlink('..',join(root,'commands','link'+i));
      const child=spawnSync(process.execPath,[...process.execArgv,fileURLToPath(import.meta.url),'--cycle-child',root],{encoding:'utf8',timeout:3000,killSignal:'SIGKILL'});
      if (child.error && (child.error as NodeJS.ErrnoException).code==='ETIMEDOUT') cycleCases.push({links,timeoutMs:3000,timedOut:true});
      else { assert.equal(child.status,0,child.stderr); cycleCases.push({links,timedOut:false,...JSON.parse(child.stdout)}); }
    }
  } finally { await rm(scratch,{recursive:true,force:true}); }
  await mkdir(output,{recursive:true});
  await writeFile(join(output,'regressions.json'),JSON.stringify({sourceCommit,node:process.version,sourceSha256,instructions,skills,aliases,cycleCases},null,2)+'\n');
  console.log(JSON.stringify({instructions:instructions.length,cycleCases,output}));
}
