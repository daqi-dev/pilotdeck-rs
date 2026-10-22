// All expected values and SQLite bytes come from executing fixed real TS.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {mkdtempSync,mkdirSync,readFileSync,realpathSync,rmSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {bindSourceTree,sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {configs,expand,openRepos,invoke,fileTree,clean,freezeClock,observe} from './observe.mts';
assert.ok(process.env.PDRS_BASELINE);assert.ok(process.argv[2]);
const baseline=resolve(process.env.PDRS_BASELINE!);const out=resolve(process.argv[2]!);
const commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git=realpathSync(process.env.PDRS_GIT||execFileSync('which',['git'],{encoding:'utf8'}).trim());
const provenance=bindSourceTree(baseline,commit,git);
const source='src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';
const sourceSha256=sourceClosureSha256(baseline,commit,git,[source]);
const root=mkdtempSync(join(tmpdir(),'pdrs-accessors-'));
// os.homedir() reads HOME. Set it before importing general-projects.ts so no
// constructor ever selects the real user's ~/.pilotdeck workspace.
process.env.HOME=join(root,'home');freezeClock();
const {MemoryRepository}=await import(pathToFileURL(join(baseline,source)).href);
const repos=openRepos(MemoryRepository,root);
const ws=(repo:string,action:string,input:any)=>({repo,method:'workspace',action,input});
const candidate=(type:string,name:string,extra={})=>({type,scope:type==='user'?'global':'project',name,description:`${name} description`,summary:`${name} summary`,profile:`${name} profile`,capturedAt:'2026-10-22T00:00:00.000Z',sourceSessionKey:`session-${name}`,...extra});
const seed:any[]=[];
for(const c of configs) seed.push({repo:c.id,method:'state',input:['main','general','defaults'].includes(c.id)?'/fixture/general-marker':c.workspace});
for(const [repo,name] of [['alpha','éclair'],['beta','Zebra']] as const) {
  seed.push(ws(repo,'upsertProjectMeta',{projectName:name}),ws(repo,'upsertCandidate',candidate('project',`${name} project`)),ws(repo,'upsertCandidate',candidate('feedback',`${name} feedback`)),ws(repo,'upsertCandidate',candidate('feedback','deprecated',{status:'deprecated'})));
}
for(const [id,name] of [['z','Zebra'],['e','éclair'],['a','apple'],['c','中文']]) seed.push(ws('main','upsertGeneralProjectMeta',{projectId:id,projectName:name}),ws('main','upsertCandidate',candidate('project',name,{projectId:id})));
seed.push(ws('main','upsertGeneralProjectMeta',{projectId:'mirror',projectName:'Alpha mirror',sourceKind:'workspace_external_mirror',sourceWorkspacePath:'/fixture/Alpha/../Alpha',sourceProjectId:'current_project'}),ws('main','upsertCandidate',candidate('feedback','local feedback',{projectId:'mirror'})),ws('main','upsertCandidate',candidate('feedback','mirror deprecated',{projectId:'mirror'})),ws('main','upsertGeneralProjectMeta',{projectId:'orphan',projectName:'Invisible orphan',sourceKind:'workspace_external_mirror',sourceWorkspacePath:'/fixture/Missing',sourceProjectId:'current_project'}));
seed.push({method:'global',action:'upsertCandidate',input:candidate('user','User😀')},{method:'global',action:'upsertCandidate',input:candidate('user','Second')});
// The shared global store keeps two entries (existing observations unchanged).
// A dedicated `probe` store gets seven users (one later deprecated for the
// includeDeprecated probes) so limits below the entry count are discriminating:
// six active entries require both a dropped and a forwarded limit to produce
// different observations.
for(const [i,name] of ['Probe one','Probe two','Probe three','Probe four','Probe five','Probe six','Probe seven'].entries())
  seed.push({repo:'probe',method:'global',action:'upsertCandidate',input:candidate('user',name,{capturedAt:`2026-10-2${i+1}T00:00:00.000Z`})});
for(const op of seed) {const result=invoke(repos,op,root);assert.ok(!result?.error,JSON.stringify(result));}
// Deprecate one entry in each store that the includeDeprecated probes use. The
// default view stays unchanged; JS truthiness of the *option* is what flips.
for(const [repoId,storeId,name] of [['main','workspace','mirror deprecated'],['probe','global','Probe seven']] as const) {
  const store=storeId==='workspace'?repos[repoId].getFileMemoryStore():repos[repoId].getGlobalUserStore();
  const entry=store.listMemoryEntries({includeDeprecated:true}).find((e:any)=>e.name===name);
  assert.ok(entry,`missing ${name}`);
  const op={repo:repoId,method:storeId,action:'markEntriesDeprecated',input:{relativePaths:[entry.relativePath]}};
  seed.push(op);const result=invoke(repos,op,root);assert.ok(!result?.error,JSON.stringify(result));
}
for(const id of ['alpha','beta']) {
  const entries=repos[id].getFileMemoryStore().listMemoryEntries({includeDeprecated:true});
  const op=ws(id,'markEntriesDeprecated',{relativePaths:entries.filter((e:any)=>e.name==='deprecated').map((e:any)=>e.relativePath)});
  seed.push(op);const result=invoke(repos,op,root);assert.ok(!result?.error,JSON.stringify(result));
}
// Distinct update times exercise newest-first sorting and mirror max-time
// aggregation; filenames/content are derived from TS, then used as inputs.
for(const [id,name,time] of [['alpha','éclair project','2026-10-23T00:00:00.000Z'],['main','éclair','2026-10-19T00:00:00.000Z']]) {
  const entry=repos[id].getFileMemoryStore().listMemoryEntries({includeDeprecated:true}).find((e:any)=>e.name===name && e.type==='project');
  assert.ok(entry);
  const path=`workspaces/${id}/memory/${entry.relativePath}`;
  const content=readFileSync(join(root,path),'utf8').replace('updated_at: 2026-10-22T00:00:00.000Z',`updated_at: ${time}`);
  const op={method:'raw',input:{path,content}};seed.push(op);invoke(repos,op,root);
}
for(const r of Object.values(repos) as any[])r.close();
// A malformed external DB must be ignored, and a non-directory must be skipped.
mkdirSync(join(root,'workspaces','bad','memory'),{recursive:true});writeFileSync(join(root,'workspaces','bad','control.sqlite'),'not sqlite');
writeFileSync(join(root,'workspaces','plain'),'not a directory');
const seedFiles=fileTree(root).filter((e:any)=>e.relativePath!=='workspaces/bad/control.sqlite');
seedFiles.push({relativePath:'workspaces/bad/control.sqlite',content:'not sqlite'});
const databases=configs.map(c=>({id:c.id,base64:readFileSync(join(root,'workspaces',c.id,'control.sqlite')).toString('base64')}));
const extBeta=`external-project:${createHash('sha1').update('/fixture/Beta::current_project').digest('hex').slice(0,10)}`;
const ops:any[]=[{method:'state',input:'$ROOT/home/.pilotdeck'},{repo:'general',method:'state',input:'$ROOT/home/.pilotdeck'},
{method:'mode'},{method:'roots'},{method:'accessorIdentity'},{repo:'defaults',method:'mode'},{repo:'defaults',method:'roots'},{repo:'defaults',method:'listReadableProjectCatalog'},{repo:'alpha',method:'mode'},{repo:'alpha',method:'listReadableProjectCatalog'},
{repo:'empty',method:'listReadableProjectCatalog'},{method:'listReadableProjectCatalog'},
{method:'project',input:{id:'missing'}},{method:'project',input:{id:'mirror'}},{method:'project',input:{id:extBeta}},
...[{}, {includeExternal:false},{kinds:['feedback']},{query:'local'}, {includeDeprecated:true}, {kinds:[]}].map(options=>({method:'entries',input:{id:'mirror',options}})),
{method:'entries',input:{id:extBeta}},{method:'entries',input:{id:extBeta,options:{includeExternal:false}}},{method:'entries',input:{id:'missing'}},
// includeExternal is `?? true` then JS truthiness: 0/"" skip external entries, 1 includes.
{method:'entries',input:{id:extBeta,options:{includeExternal:0}}},{method:'entries',input:{id:extBeta,options:{includeExternal:''}}},
{method:'entries',input:{id:extBeta,options:{includeExternal:1}}},{method:'entries',input:{id:'mirror',options:{includeExternal:0}}},
{method:'entries',input:{id:'mirror',options:{includeExternal:null}}},
// listReadableProjectEntries hardcodes limit 5000/offset 0; caller values are ignored.
{method:'entries',input:{id:'mirror',options:{limit:1,offset:1}}},{method:'entries',input:{id:'mirror',options:{query:'local',limit:1}}},
// Project kinds is forwarded to the store's `new Set(...)`: arrays filter,
// strings iterate to characters (no matches), null/undefined take the default,
// and numbers/booleans/plain objects throw from the store call. An external-only
// project with includeExternal:false never reaches a store, so it returns [].
{method:'entries',input:{id:'mirror',options:{kinds:42}}},{method:'entries',input:{id:'mirror',options:{kinds:true}}},
{method:'entries',input:{id:'mirror',options:{kinds:{}}}},{method:'entries',input:{id:'mirror',options:{kinds:null}}},
{method:'entries',input:{id:'mirror',options:{kinds:'project'}}},{method:'entries',input:{id:'mirror',options:{kinds:''}}},
{method:'entries',input:{id:'z',options:{kinds:42}}},
{method:'entries',input:{id:extBeta,options:{kinds:42}}},{method:'entries',input:{id:extBeta,options:{kinds:42,includeExternal:false}}},
...[{}, {scope:'project'},{kinds:['project']},{kinds:[]},{kinds:['feedback','user']},{query:'Second'}, {limit:1,offset:1},{includeDeprecated:true}].map(input=>({method:'listGlobalMemoryEntries',input})),
// Only whitelisted keys reach the global store; non-array kinds throws before scope.
{method:'listGlobalMemoryEntries',input:{projectId:'x'}},{method:'listGlobalMemoryEntries',input:{kinds:'project'}},
{method:'listGlobalMemoryEntries',input:{kinds:'project',scope:'project'}},{method:'listGlobalMemoryEntries',input:{kinds:42}},
{method:'listGlobalMemoryEntries',input:{kinds:null}},
{method:'listGlobalMemoryEntries',input:{query:'',limit:'2',offset:null,includeTmp:true,projectId:'x'}},
// Six-entry global store: `limit`/`offset` are forwarded only when
// `typeof === "number"` (NaN included), `query` only when truthy. Strings,
// booleans and null are dropped and therefore return all six active entries; 2.5 is forwarded
// and slices to two; NaN is forwarded and slices to none. `$number` encodes NaN
// because JSON cannot (decoded by observe.mts and edgeclaw_accessors.rs).
...[{}, {limit:'2'},{limit:true},{limit:null},{limit:2.5},{limit:2},{limit:0},{limit:{$number:'NaN'}},
    {offset:'1'},{offset:null},{offset:1},{offset:2.5},{query:''},{query:0},{query:'Probe three'},
    {kinds:['user']},{scope:'project'}].map(input=>({repo:'probe',method:'listGlobalMemoryEntries',input})),
// A non-whitelisted projectId must not reach the store: forwarded, the store's
// single-mode project filter returns [] instead of the two sliced entries.
{repo:'probe',method:'listGlobalMemoryEntries',input:{limit:2,projectId:'x'}},
{method:'getUserSummary'},
{method:'raw',input:{path:'workspaces/main/memory/MEMORY.md',content:'broken manifest'}},{method:'repairWorkspaceManifest'},{method:'repairWorkspaceManifest'},
{method:'state',input:'/fixture/Beta'},{method:'listReadableProjectCatalog'},{method:'state',input:'$ROOT/home/.pilotdeck'},
{repo:'alpha',method:'state',input:'/fixture/AlphaChanged'},{method:'listReadableProjectCatalog'},
{repo:'alpha',method:'state',input:'/fixture/Alpha'}, {method:'listReadableProjectCatalog'},
// Non-string truthy query: both accessors forward it (JS truthiness), then the
// TS store's eager normalizeWhitespace throws `(value ?? "").replace is not a
// function`. Falsy 0 is dropped by the accessor, so it still returns entries.
{method:'entries',input:{id:'mirror',options:{query:true}}},
{method:'entries',input:{id:'mirror',options:{query:1}}},
{method:'entries',input:{id:'mirror',options:{query:{}}}},
{method:'entries',input:{id:'mirror',options:{query:[]}}},
{method:'entries',input:{id:'mirror',options:{query:0}}},
{repo:'probe',method:'listGlobalMemoryEntries',input:{query:true}},
{repo:'probe',method:'listGlobalMemoryEntries',input:{query:1}},
{repo:'probe',method:'listGlobalMemoryEntries',input:{query:{}}},
{repo:'probe',method:'listGlobalMemoryEntries',input:{query:[]}},
{repo:'probe',method:'listGlobalMemoryEntries',input:{query:0}},
// includeDeprecated is a JS truthy OR on the caller option; the entry's own
// deprecated field stays boolean. One deprecated entry exists in each store.
{method:'entries',input:{id:'mirror',options:{includeDeprecated:1}}},
{method:'entries',input:{id:'mirror',options:{includeDeprecated:'yes'}}},
{method:'entries',input:{id:'mirror',options:{includeDeprecated:[]}}},
{method:'entries',input:{id:'mirror',options:{includeDeprecated:{}}}},
{method:'entries',input:{id:'mirror',options:{includeDeprecated:0}}},
{repo:'probe',method:'listGlobalMemoryEntries',input:{includeDeprecated:1}},
{repo:'probe',method:'listGlobalMemoryEntries',input:{includeDeprecated:'yes'}},
{repo:'probe',method:'listGlobalMemoryEntries',input:{includeDeprecated:[]}},
{repo:'probe',method:'listGlobalMemoryEntries',input:{includeDeprecated:{}}},
{repo:'probe',method:'listGlobalMemoryEntries',input:{includeDeprecated:0}},
{method:'remove',input:{path:'workspaces/beta/memory'}},{method:'listReadableProjectCatalog'},
{repo:'empty',method:'close'},{repo:'empty',method:'listReadableProjectCatalog'},{repo:'alpha',method:'close'},{repo:'alpha',method:'listReadableProjectCatalog'},
{method:'close'},{method:'close'},{method:'mode'},{method:'roots'},{method:'accessorIdentity'},{repo:'defaults',method:'mode'},{repo:'defaults',method:'roots'},{repo:'defaults',method:'listReadableProjectCatalog'},{method:'getUserSummary'},
{method:'listGlobalMemoryEntries'},{method:'repairWorkspaceManifest'},{method:'readState'},{method:'listReadableProjectCatalog'},
{method:'reopen',input:{workspace:'/fixture/NowSingle'}},{method:'mode'},{method:'listReadableProjectCatalog'},
{method:'reopen',input:{workspace:'$ROOT/home/.pilotdeck'}},{method:'mode'},{method:'listReadableProjectCatalog'}];
const result=observe(MemoryRepository,root,ops);
const oracle={sourceCommit:provenance.ref,sourceSha256,nodeVersion:process.version,configs,seed,seedFiles,databases,operations:ops,...result};
mkdirSync(out,{recursive:true});const text=JSON.stringify(oracle,null,2)+'\n';writeFileSync(join(out,'oracle.json'),text);console.log(`sha256 ${createHash('sha256').update(text).digest('hex')}; ${ops.length} observations`);
rmSync(root,{recursive:true,force:true});
