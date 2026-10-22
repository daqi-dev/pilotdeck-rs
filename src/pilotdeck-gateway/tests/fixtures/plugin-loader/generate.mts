// Real fixed-baseline disk loader probes. No baseline or node_modules writes.
import assert from 'node:assert/strict';
import { assertPortablePaths } from './portable-paths.mjs';
import { lifecycleProbe } from './lifecycle-probe.mts';
import { execFileSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { chmod, mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';
assert.ok(process.env.PDRS_BASELINE, 'PDRS_BASELINE required');
assert.ok(process.argv[2], 'output directory required');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const ref = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
bindSourceTree(baseline, ref, git);
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('GIT_')));
assert.equal(execFileSync(git, ['--no-replace-objects', '-C', baseline, 'status', '--porcelain', '--untracked-files=no'], {env:gitEnv}).toString(), '');
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US');
const prefix = 'src/extension/plugins/';
const sources = ['discovery/discoverLocalPlugins.ts', 'discovery/PluginDirectoryResolver.ts',
  'loading/PluginLoader.ts', 'loading/PluginCommandLoader.ts', 'loading/PluginHookLoader.ts',
  'config/parsePluginManifest.ts', 'config/validatePluginSource.ts', 'config/validateMarketplaceName.ts',
  'protocol/marketplace.ts', 'runtime/PluginRegistry.ts', 'runtime/PluginRuntime.ts'].map(s => prefix + s);
const sourceSha256 = sourceClosureSha256(baseline, ref, git, sources);
const load = (s:string) => import(pathToFileURL(join(baseline,prefix+s)).href);
const { discoverPluginPaths, discoverSkillPaths } = await load('discovery/discoverLocalPlugins.ts');
const { resolvePluginDirectories } = await load('discovery/PluginDirectoryResolver.ts');
const { parsePluginManifest } = await load('config/parsePluginManifest.ts');
const { validatePluginSourcePath } = await load('config/validatePluginSource.ts');
const { validateMarketplaceName } = await load('config/validateMarketplaceName.ts');
const { resolveMarketplaceReference } = await load('protocol/marketplace.ts');
const { loadPluginFromPath, loadSkillFromPath, loadPluginOutputStylesFromPath } = await load('loading/PluginLoader.ts');
const { loadStandaloneSkill } = await load('loading/PluginCommandLoader.ts');
const { PluginRuntime } = await load('runtime/PluginRuntime.ts');
const { PluginRegistry } = await load('runtime/PluginRegistry.ts');
const capture = async (f:()=>any) => { try { return { value:await f() }; } catch(e) { return {error:(e as Error).message}; } };
const manifestInputs:any[] = [null, [], 3, 'name', {}, {name:null}, {name:3}, {name:''}, {name:' \t\ufeff'},
  {name:' kept ',version:3,description:false,commands:5,agents:[1],skills:null,hooks:[],mcpServers:[],lspServers:3,outputStyles:[1],marketplace:{name:'m'},mcpb:'x.MCPB',settings:[]},
  {name:'all',version:'',description:'d',commands:['a','b'],agents:'agent',skills:[],hooks:{},mcpServers:{},lspServers:{},outputStyles:'out',marketplace:{name:'m',plugin:'p',version:'1',source:'git',url:'u',extra:1},mcpb:'x.mcpb',settings:{enabled:false},extra:9},
  ...['marketplace','git','zip','mcpb','unknown',null,3].map(source=>({name:'x',hooks:'h.json',commands:'',skills:['s'],marketplace:{name:'m',plugin:'p',source,version:2,url:false},mcpb:'x.dxt'})),
];
const manifestCases = await Promise.all(manifestInputs.map(async raw=>({raw,...await capture(()=>parsePluginManifest(raw))})));
const marketplaceNames = ['', '.', '..', 'a b', 'a/b', 'a\\b', 'a..b', 'INLINE', 'builtin', 'é', 'a\t', 'official-pilot', 'xPilotDeck_officialx', 'pilot.plugins', 'pilotdeck-marketplace', 'a_officialPilot', 'community', 'pilotage', 'xpilotplugins', 'official'];
const nameCases = marketplaceNames.map(name=>({name,error:validateMarketplaceName(name)??null}));
const referenceCases = ['marketplace','git','zip','mcpb','unknown',undefined].map(source=>{
  const reference = {name:'m',plugin:'p',...(source===undefined?{}:{source})};
  return {reference,value:resolveMarketplaceReference(reference)};
});
type File = {path:string,text?:string,dir?:boolean,link?:string,mode?:number};
type Scenario = {id:string,files:File[],builtins?:any[],enabled?:Record<string,boolean>,builtinSkills?:boolean,remove?:string[],invalid?:{path:string,text:string}[]};
const w = (path:string,text:string):File=>({path,text});
const m = (dir:string,value:any):File=>w(dir+'/plugin.json',JSON.stringify(value));
const G='home/plugins', P='project/.pilotdeck/plugins';
const scenarios:Scenario[] = [
  {id:'empty-and-missing',files:[]},
  {id:'valid-contributions',files:[m(`${G}/folder`,{name:'manifest-id',commands:['custom','more/','absent'],skills:'skill-set',outputStyles:'styles',hooks:{PreToolUse:[{matcher:'*',hooks:[{type:'command',command:'echo ok'},{type:'callback'}]}]},mcpServers:{server:{command:'node',args:['server.js']}},lspServers:{typescript:{command:'tsserver'}}}),
    w(`${G}/folder/custom/z.md`,'z'),w('outside-markdown/linked.md','markdown linked file'),{path:`${G}/folder/custom/link.md`,link:'outside-markdown/linked.md'},{path:`${G}/folder/custom/broken.md`,link:'missing.md'},w(`${G}/folder/custom/A.MD`,'---\ndescription: "hello"\nargument-hint: <file>\nflag: true\nother: false\nnumber: 0x10\nbin: 0b11\noctal: 0o10\nexp: 1e3\nempty:\nnan: NaN\ninfinity: Infinity\nq: \'\n__proto__: ignored\n: empty key\nno separator\nrepeat: first\nrepeat: last\n---\nbody\n'),
    w(`${G}/folder/custom/nested/SKILL.md`,'---\nx: 1\n---\nskill command'),w(`${G}/folder/custom/SKILL.md`,'root skill command'),w(`${G}/folder/custom/deep/a.md`,'---\nx: unterminated'),
    w(`${G}/folder/more/crlf.md`,'---\r\nx: 1\r\n---\r\nraw'),w(`${G}/folder/more/.hidden.md`,'hidden'),w(`${G}/folder/more/ignored.txt`,'no'),
    w(`${G}/folder/skill-set/README.md`,'not a skill'),w(`${G}/folder/skill-set/root/Skill.MD`,'---\ndescription: skill\n---\nbody'),w(`${G}/folder/styles/concise.md`,'style'),w(`${G}/folder/skill-set/unicode/SKILL.md`,'unicode case folding skill'),
    w(`${G}/folder/commands/ignored.md`,'not configured'),w(`${G}/folder/index.js`,'throw new Error("MUST NOT EXECUTE")')]},
  {id:'invalid-and-loose-option-types',files:[m(`${G}/z-wrong-name`,{name:9}),m(`${G}/y-no-name`,{}),m(`${G}/x-array`,[]),m(`${G}/w-null`,null),w(`${G}/v-bad-json/plugin.json`,'{"name": }'),
    {path:`${G}/u-no-manifest`,dir:true},m(`${G}/loose`,{name:'loose',version:7,commands:[1],skills:9,outputStyles:false,hooks:[],mcpServers:[],lspServers:'x',settings:{enabled:false},enabled:false}),w(`${G}/loose/commands/default.md`,'default'),w(`${G}/loose/hooks/hooks.json`,'{"Stop":[{"hooks":[{"type":"prompt","prompt":"p"}]}]}'),w(`${G}/plain-file`,'not dir')]},
  {id:'duplicates-and-priority',files:[m(`${G}/a`,{name:'dup'}),m(`${G}/z`,{name:'dup'}),m(`${G}/same`,{name:'global'}),w(`${G}/same/commands/run.md`,'global'),m(`${P}/same`,{name:'project'}),w(`${P}/same/commands/run.md`,'project'),m(`${P}/b`,{name:'dup'}),w('home/skills/shared/SKILL.md','global skill'),w('project/.pilotdeck/skills/shared/skill.md','project skill'),w('builtin-skills/shared/Skill.MD','builtin skill')], builtinSkills:true},
  {id:'symlinks-and-unreadable',files:[m('outside-real',{name:'linked'}),w('outside-real/commands/ok.md','linked command'),{path:`${G}/linked`,link:'outside-real'},{path:`${G}/broken`,link:'missing'},w('ordinary-file','file'),{path:`${G}/file-link`,link:'ordinary-file'},m(`${G}/blocked`,{name:'blocked'}),{path:`${G}/blocked`,mode:0},m(`${G}/one`,{name:'one'}),{path:'project/.pilotdeck/plugins',dir:true,mode:0},w('outside-skill/skill.md','linked skill'),{path:'home/skills/linked',link:'outside-skill'}]},
  {id:'skills-root-only',files:[w('home/skills/read/Skill.MD','---\ndescription: standalone\n---\nroot'),w('home/skills/read/reference.md','never exposed'),w('home/skills/read/deep/SKILL.md','never exposed'),m('home/skills/read',{name:'still-skill'}),w('home/skills/no-root/sub/SKILL.md','nested only'),{path:'home/skills/bad/SKILL.md',dir:true},w('builtin-skills/builtin/SKILL.md','builtin'),w('home/skills/unicode-only/SKILL.md','discovery uses non-unicode regex, ignored')],builtinSkills:true},
  {id:'builtin-loading-and-disable',files:[m('builtin/good',{name:'disk-name'}),w('builtin/good/commands/go.md','disk'),m('builtin/off',{name:'disabled-disk'}),m(`${G}/enabled-false`,{name:'global-not-disabled',enabled:false}),w(`${G}/enabled-false/commands/go.md`,'disk')],
    builtins:[{name:'original-name',path:'builtin/good',source:'builtin',commands:[{name:'original',content:'original'}]},{name:'fallback',path:'builtin/missing',source:'builtin',commands:[{name:'fallback',content:'fallback'}]},{name:'off-original',path:'builtin/off',source:'builtin'},{name:'injected-global',path:'builtin/missing-global',source:'global'}],enabled:{'disabled-disk':false,fallback:false,'global-not-disabled':false,'injected-global':false}},
  {id:'hook-files-and-configured-paths',files:[m(`${G}/external`,{name:'ext',hooks:'custom.json',commands:['/custom','../shared','missing'],skills:[]}),w(`${G}/external/custom.json`,'{"Stop":[{"hooks":[{"type":"command","command":"true"}]}],"Unknown":[]}'),w(`${G}/external/custom/a.md`,'absolute-looking dir joined below plugin'),w(`${G}/shared/b.md`,'outside plugin'),m(`${G}/bad-hooks`,{name:'bad',hooks:'bad.json'}),w(`${G}/bad-hooks/bad.json`,'invalid JSON'),m(`${G}/read-failure`,{name:'read-failure',commands:['custom','good']}),w(`${G}/read-failure/custom/ok.md`,'discard whole configured dir'),w(`${G}/read-failure/custom/no.md`,'unreadable'),{path:`${G}/read-failure/custom/no.md`,mode:0},w(`${G}/read-failure/good/ok.md`,'retained')]},
  {id:'discovery-unicode-order',files:['z-lower','a','Z-upper','Å','_','10','2','😀','\ue000','.hidden'].map(n=>m(`${G}/${n}`,{name:n}))},
  {id:'refresh-staging-rollback',files:[m(`${G}/keep`,{name:'keep'}),w(`${G}/keep/commands/run.md`,'old')],invalid:[{path:`${G}/keep/plugin.json`,text:'{}'}],remove:[`${G}/keep`]},
];
for (const scenario of scenarios) assertPortablePaths(scenario.files.map(f => f.path));
const scratch = await mkdtemp(join(tmpdir(),'pdrs-plugin-loader-'));
const normalize = (v:any,root:string):any => typeof v==='string'?v.split(root).join('<root>'):Array.isArray(v)?v.map(x=>normalize(x,root)):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,x])=>[k,normalize(x,root)])):v;
function plugin(p:any) { return {name:p.name,path:p.path,source:p.source,commands:p.commands??[],skills:p.skills??[],outputStyles:p.outputStyles??[],...(p.hooksConfig===undefined?{}:{hooksConfig:p.hooksConfig}),...(p.mcpServers===undefined?{}:{mcpServers:p.mcpServers}),...(p.lspServers===undefined?{}:{lspServers:p.lspServers})}; }
function report(r:any) { return {generation:r.generation,next:r.next.map(plugin),stagedFailures:r.stagedFailures}; }
const results:any[]=[];
const serverCases:any[]=[];
try {
  for(const scenario of scenarios) {
    const root=join(scratch,scenario.id); await mkdir(root,{recursive:true});
    const permissions:File[]=[];
    for(const file of scenario.files) {
      const path=join(root,file.path); await mkdir(dirname(path),{recursive:true});
      if(file.mode!==undefined) { if(file.dir) await mkdir(path,{recursive:true}); permissions.push(file); continue; }
      if(file.link!==undefined) await symlink(join(root,file.link),path);
      else if(file.dir) await mkdir(path,{recursive:true});
      else await writeFile(path,file.text!);
    }
    const builtins=(scenario.builtins??[]).map(b=>({...b,path:join(root,b.path),manifest:{name:b.name},commands:(b.commands??[]).map((c:any)=>({...c,path:join(root,b.path,'fake.md'),frontmatter:{},isSkill:false}))}));
    const options={projectRoot:join(root,'project'),pilotHome:join(root,'home'),...(scenario.builtinSkills?{builtinSkillsRoot:join(root,'builtin-skills')} : {}),builtinPlugins:builtins,builtinPluginsEnabled:scenario.enabled??{}};
    for(const f of permissions) await chmod(join(root,f.path),f.mode!);
    try {
      const paths=resolvePluginDirectories(options);
      const discovered=await discoverPluginPaths([{path:paths.globalPluginsDir,source:'global'},{path:paths.projectPluginsDir,source:'project'}]);
      const skills=await discoverSkillPaths([...(scenario.builtinSkills?[{path:join(root,'builtin-skills'),source:'builtin'}]:[]),{path:paths.globalSkillsDir,source:'global'},{path:paths.projectSkillsDir,source:'project'}]);
      const attempts=[];
      for(const candidate of discovered) attempts.push({...candidate,kind:'plugin',...await capture(async()=>plugin(await loadPluginFromPath(candidate.path,candidate.source)))});
      for(const candidate of skills) attempts.push({...candidate,kind:'skill',...await capture(async()=>plugin(await loadSkillFromPath(candidate.path,candidate.source)))});
      const styleAttempts=[];
      for(const candidate of discovered) styleAttempts.push({...candidate,...await capture(()=>loadPluginOutputStylesFromPath(candidate.path,candidate.source))});
      const noSkill=await capture(()=>loadStandaloneSkill({name:'absent',skillDir:root}));
      const runtime=new PluginRuntime(options);
      const first=await runtime.refreshWithReport();
      const catalog=runtime.getAllCommands();
      const active=runtime.snapshotContributions().plugins.map(plugin);
      const steps=[report(first)];
      if(scenario.invalid) {
        const lease=runtime.acquireCommandCatalog();
        for(const f of scenario.invalid) await writeFile(join(root,f.path),f.text);
        steps.push(report(await runtime.refreshWithReport()));
        for(const p of scenario.remove??[]) await rm(join(root,p),{recursive:true});
        steps.push(report(await runtime.refreshWithReport()));
        await lease.release();
      }
      await runtime.dispose();
      results.push(normalize({...scenario,discovered,skills,attempts,styleAttempts,noSkill,steps,active,catalog},root));
    } finally { for(const f of permissions) await chmod(join(root,f.path),0o700); }
  }
  for(const depth of [0,150]) {
    let config:any={leaf:true}; for(let i=0;i<depth;i++) config={next:config};
    const raw=JSON.stringify({name:'servers',mcpServers:{server:config},lspServers:{server:config}});
    const path=join(scratch,'server-depth-'+depth); await mkdir(path); await writeFile(join(path,'plugin.json'),raw);
    const loaded=await loadPluginFromPath(path,'global');
    serverCases.push({depth,raw,mcp:JSON.stringify(loaded.mcpServers),lsp:JSON.stringify(loaded.lspServers)});
  }
  {
    const raw=JSON.stringify({name:'ordered-servers',mcpServers:{zeta:{command:'node',args:['z.js'],env:{Z:'z',A:'a'}},'10':{z:1,a:2},'2':{},alpha:{last:true,first:false}},lspServers:{zulu:{command:'lsp',args:[]},alpha:{z:1,a:2}}});
    const path=join(scratch,'server-order'); await mkdir(path); await writeFile(join(path,'plugin.json'),raw);
    const loaded=await loadPluginFromPath(path,'global');
    const runtime=new PluginRuntime({projectRoot:join(path,'project'),pilotHome:join(path,'home'),builtinPlugins:[loaded]});
    await runtime.refreshWithReport();
    serverCases.push({depth:0,raw,mcp:JSON.stringify(loaded.mcpServers),lsp:JSON.stringify(loaded.lspServers),mergedMcp:JSON.stringify(runtime.mcpServers()),mergedLsp:JSON.stringify(runtime.lspServers())});
    await runtime.dispose();
  }
  const lifecycleCases=await lifecycleProbe(PluginRuntime,PluginRegistry,join(scratch,'lifecycle'));
  const sourcePathCases=[['<root>/safe','<root>'],['<root>/safe/../x','<root>/safe'],['<root>/safe/x','<root>/safe'],['<root>/safe/x','<root>/safe/'],['<root>/safe-other','<root>/safe'],['<root>/outside-real','<root>/home/plugins'],['<root>/safe','/'],['<root>/safe/..','<root>']].map(([path,allowedRoot])=>({path,allowedRoot,value:validatePluginSourcePath(path.replace('<root>',scratch),allowedRoot.replace('<root>',scratch))}));
  await mkdir(output,{recursive:true});
  await writeFile(join(output,'oracle.json'),JSON.stringify({sourceCommit:ref,sourceSha256,node:process.version,manifestCases,nameCases,referenceCases,sourcePathCases,serverCases,lifecycleCases,scenarios:results},null,2)+'\n');
} finally { await rm(scratch,{recursive:true,force:true}); }
