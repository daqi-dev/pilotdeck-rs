// #887: execute the fixed TS entry points; no copied catalog algorithm.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { mkdtemp, mkdir, writeFile, chmod, symlink, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
assert.equal(process.version, 'v22.22.0');
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US');
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE!);
const output = resolve(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(execFileSync('which', ['git'], {encoding:'utf8'}).trim());
bindBaseline({ baseline, commit, gitExecutable: git });
const sources = ['src/gateway/dialog/commands.ts', 'src/cli/GatewayCommandCatalogBundle.ts'];
const sourceSha256 = sourceClosureSha256(baseline, commit, git, sources);
const { listCommands } = await import(pathToFileURL(join(baseline,sources[0])).href);
const { GatewayCommandCatalogBundle } = await import(pathToFileURL(join(baseline,sources[1])).href);
const P='project/.pilotdeck/commands', S='project/.pilotdeck/skills', U='home/commands', H='home/skills';
const file = (path:string,text:string) => ({path,text});
const scenarios:any[] = [
  {id:'empty',files:[]},
  {id:'selected-cycle',files:[file(`${P}/cycle.md`,'---\nself: &x [*x]\n---\ncycle')],requests:[{inspectGraph:true}]},
  {id:'excluded-cycles',files:[file(`${P}/clear.md`,'---\nself: &x [*x]\n---\noverridden'),file(`${P}/loop.md`,'---\ndescription: loop\nself: &x [*x]\n---\nbody')],requests:[{query:'clear'},{query:'not-present'},{limit:1}]},
  {id:'normalized-paths',files:[file(`${P}/go.md`,'normalized'),file(`${U}/user.md`,'home normalized')],home:'<root>/missing/../home',requests:[{projectKey:'<root>/missing/../project'}]},
  {id:'disk',files:[file(`${P}/deep/go.md`,'# Go'),file(`${P}/.hidden.md`,'hidden'),file(`${P}/ignore.MD`,'upper'),file(`${P}/ignore.txt`,'text'),file(`${U}/user.md`,'user'),file(`${S}/skill/SKILL.md`,'skill'),file(`${H}/user-skill/SKILL.md`,'user skill'),file(`${S}/empty/SKILL.md`,''),file(`${S}/nested/deep/SKILL.md`,'not top level'),file('outside/ok.md','linked command'),{path:`${P}/link.md`,link:'outside/ok.md'},{path:`${P}/linked-dir`,link:'outside'},file('outside-skill/SKILL.md','linked skill'),{path:`${S}/linked`,link:'outside-skill'},{path:`${S}/broken`,link:'absent'},file(`${S}/ordinary.md`,'not dir'),{path:`${S}/dir/SKILL.md`,dir:true}]},
  {id:'metadata',files:[file(`${P}/set.md`,'---\n!!set {a: null, b: null}\n---\nset'),file(`${P}/map.md`,'---\n!!omap\n- a: x\n- b: y\n---\nmap'),file(`${P}/nested-set.md`,'---\ndescription: known\nnested: !!set {a: null}\n---\nbody'),file(`${P}/yaml.md`,'---\r\ndescription: ""\r\nargumentHint: "<x>"\r\nargument-hint: ignored\r\naliases: [needle, 2]\r\n---\r\n# body'),file(`${P}/fallback.md`,'---\ndescription: 42\nargumentHint: false\n---\n\n### Heading'),file(`${P}/bad.md`,'---\nx: [bad\n---\n## fallback'),file(`${P}/scalar.md`,'---\n42\n---\nscalar'),file(`${P}/array.md`,'---\n[a, b]\n---\narray'),file(`${P}/nofence.md`,'---broken---   # First\nsecond'),file(`${P}/empty.md`,''),file(`${P}/bom.md`,'\ufeff---\ndescription: value\n---\nbody'),file(`${P}/duplicate.md`,'---\nx: 1\nx: 2\n---\nlast'),file(`${P}/unclosed.md`,'---\ndescription: unterminated'),file(`${S}/hint/SKILL.md`,'---\ndescription: skill\nargumentHint: hidden\n---\nbody'),{path:`${P}/bytes.md`,bytes:[255,35,32,65]},file(`${P}/aliases.md`,'---\naliases: [needle, 42, needle]\n---\nbody')],requests:[{}, {query:'needle'}]},
  {id:'precedence',files:[file(`${P}/clear.md`,'disk clear'),file(`${P}/shared.md`,'project command'),file(`${S}/shared/SKILL.md`,'project skill'),file(`${U}/shared.md`,'user command'),file(`${H}/shared/SKILL.md`,'user skill'),file(`${S}/ps/SKILL.md`,'project skill winner'),file(`${U}/ps.md`,'user loser'),file(`${U}/us.md`,'user command winner'),file(`${H}/us/SKILL.md`,'user skill loser'),file(`${H}/skill-only/SKILL.md`,'user skill winner')],commands:['clear','shared','ps','us','skill-only'].map(name=>({name,description:'plugin loser'})).concat([{name:'shared',description:'second plugin loser'}])},
  {id:'extensions',files:[],commands:[{name:' \t\ufeff '},{name:'  plug  ',description:'',argumentHint:'',namespace:' \ufeff '},{name:'plug',description:'later'},{name:'/path/deep',description:'  ',argumentHint:'<x>',namespace:' custom '},{name:'Z',namespace:'project'},{name:'new',namespace:'builtin'}]},
  {id:'sorting',files:['z','A','b','é','中','10','2','_','😀'].map(n=>file(`${P}/${n}.md`,n)).concat(['a','Z','ä','9'].map(n=>file(`${U}/${n}.md`,n))),commands:[{name:'ext',namespace:'project'},{name:'abc',namespace:'builtin'},{name:'é',namespace:'plugin'},{name:'plugin',namespace:'z'}]},
  {id:'query',files:[file(`${P}/match.md`,'---\ndescription: 😀ABABA İSTANBUL\naliases: [ABABA, 3]\n---\nbody')],requests:[{query:' ABA '},{query:'😀'},{query:'i̇'},{query:'更新'},{query:'unknown-no-match'},{query:' \ufeff '}]},
  {id:'pagination',files:[file(`${P}/extra.md`,'extra')],requests:[{limit:1,pages:true},{limit:100},{limit:2,offset:999},{query:'clear',limit:1,offset:0,noisyCursor:true},{query:'clear',offset:0,signatureQuery:'different'},{offset:-1},{offset:1.5},{offset:'0'},{offset:0,signatureProject:'other'}]},
  {id:'validation',files:[],requests:[{projectKey:''},{query:'x'.repeat(257)},{query:'😀'.repeat(129)},{query:'x'.repeat(256)},{limit:0},{limit:101},{cursor:'invalid'}]},
  {id:'read-errors',files:[file(`${P}/blocked.md`,'must not read'),{path:`${P}/blocked.md`,mode:0},file(`${S}/blocked/SKILL.md`,'must not read'),{path:`${S}/blocked/SKILL.md`,mode:0},file(U,'not a directory'),{path:H,dir:true,mode:0}]},
];
const normalize = (v:any,root:string):any => {
  if(typeof v==='string') return v.split(root).join('<root>');
  if(Array.isArray(v)) return v.map(x=>normalize(x,root));
  if(v&&typeof v==='object') return Object.fromEntries(Object.entries(v).map(([k,x])=>{
    if(k==='nextCursor') { const c=JSON.parse(Buffer.from(x as string,'base64url').toString()); c.signature=JSON.parse(Buffer.from(c.signature,'base64url').toString()); return [k,normalize(c,root)]; }
    return [k,normalize(x,root)];
  })); return v;
};
const scratch = await mkdtemp(join(tmpdir(),'pdrs-887-'));
const cases:any[]=[];
try {
  for(const scenario of scenarios) {
    const root=join(scratch,scenario.id); await mkdir(root,{recursive:true});
    for(const f of scenario.files) {
      const path=join(root,f.path); await mkdir(dirname(path),{recursive:true});
      if(f.dir) await mkdir(path,{recursive:true});
      else if(f.link) await symlink(join(root,f.link),path);
      else if(f.mode===undefined) await writeFile(path,f.bytes?Buffer.from(f.bytes):f.text,{flag:'wx'});
      if(f.mode!==undefined) await chmod(path,f.mode);
    }
    const results=[];
    try {
      for(const request of scenario.requests??[{}]) {
        const input:any={projectKey:join(root,'project'),...request};
        input.projectKey=input.projectKey.replaceAll('<root>',root);
        if('offset' in request) input.cursor=Buffer.from(JSON.stringify({offset:request.offset,signature:Buffer.from(JSON.stringify({projectKey:request.signatureProject??input.projectKey,query:request.signatureQuery??(input.query?.trim().toLocaleLowerCase()??'')})).toString('base64url')})).toString('base64url');
        if(request.noisyCursor) input.cursor='!!'+input.cursor+'\n';
        const pages=[];
        try {
          if(request.inspectGraph) {
            const value=await listCommands(input,join(root,'home'));
            const metadata:any=value.custom[0].metadata;
            let jsonSerializable=true;
            try { JSON.stringify(value); } catch(error) { assert.ok(error instanceof TypeError); jsonSerializable=false; }
            results.push({boundary:{listReturned:true,selfCycle:metadata.self[0]===metadata.self,jsonSerializable}});
            continue;
          }
          do { assert.ok(pages.length<100,'#887 page deadline'); const value=await listCommands(input,(scenario.home??join(root,'home')).replaceAll('<root>',root),scenario.commands??[]); pages.push(normalize(value,root)); input.cursor=value.nextCursor; }
          while(request.pages&&input.cursor);
          results.push({pages});
        } catch(e:any) {results.push({error:e.message,code:e.code});}
      }
    } finally { for(const f of scenario.files) if(f.mode!==undefined) await chmod(join(root,f.path),0o700); }
    cases.push({...scenario,results});
  }
} finally { await rm(scratch,{recursive:true,force:true}); }
const lifecycle:any[]=[];
for(const mode of ['success','list-error','resolve-error','refresh-error','acquire-error','release-error','both-errors','list-pending','release-pending']) {
  const events:string[]=[]; const mark=(s:string)=>events.push(s);
  let unblock!:()=>void, entered!:()=>void;
  const gate=new Promise<void>(r=>unblock=r), reached=new Promise<void>(r=>entered=r);
  const runtime={pluginRuntime:{refresh:async()=>{mark('refresh');if(mode==='refresh-error')throw Error('refresh failed');},acquireCommandCatalog:()=>{mark('acquire');if(mode==='acquire-error')throw Error('acquire failed');return {contributions:{commands:[]},release:async()=>{mark('release');if(mode==='release-pending'){entered();await gate;}if(['release-error','both-errors'].includes(mode))throw Error('release failed');}};}}};
  const bundle=new GatewayCommandCatalogBundle({pilotHome:'home',resolveProjectKey:async()=>{mark('resolve');if(mode==='resolve-error')throw Error('resolve failed');return 'project';},resolveRuntime:()=>{mark('runtime');return runtime;},warn:()=>mark('warn'),providers:{listCommands:async()=>{mark('list');if(mode==='list-pending'){entered();await gate;}if(['list-error','both-errors'].includes(mode))throw Error('list failed');return {pinned:[],builtIn:[],custom:[]};}}});
  let settled=false; const result=bundle.commandsList({projectKey:'requested'}).then(value=>({value}),error=>({error:error.message})).finally(()=>{settled=true;});
  let pending:any={};
  if(mode.endsWith('pending')) { await Promise.race([reached,new Promise((_,reject)=>{const timer=setTimeout(()=>reject(Error('probe deadline')),2000);timer.unref();})]); pending={before:[...events],settled};unblock(); }
  lifecycle.push({mode,...pending,...await Promise.race([result,new Promise((_,reject)=>{const timer=setTimeout(()=>reject(Error("#887 settle deadline")),2000);timer.unref();})]),events});
}
bindBaseline({ baseline, commit, gitExecutable: git });
await mkdir(output,{recursive:true});
const oracle={sourceCommit:commit,node:process.version,sourceSha256,cases,lifecycle};
await writeFile(join(output,'oracle.json'),JSON.stringify(oracle,null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({node:process.version,cases:cases.map(c=>({id:c.id,results:c.results})),lifecycle},null,2));
