// All expected values and SQLite bytes come from executing fixed real TS.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {mkdtempSync,mkdirSync,readFileSync,realpathSync,readdirSync,rmSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {bindSourceTree,sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {configs,expand,openRepos,invoke,fileTree,freezeClock,observe} from './observe.mts';
assert.ok(process.env.PDRS_BASELINE);assert.ok(process.argv[2]);
const baseline=resolve(process.env.PDRS_BASELINE!);const out=resolve(process.argv[2]!);
const commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git=realpathSync(process.env.PDRS_GIT||execFileSync('which',['git'],{encoding:'utf8'}).trim());
const provenance=bindSourceTree(baseline,commit,git);
const source='src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';
const sourceSha256=sourceClosureSha256(baseline,commit,git,[source]);
const root=mkdtempSync(join(tmpdir(),'pdrs-export-'));
// os.homedir() reads HOME. Set it before importing sqlite.ts so no constructor
// ever selects the real user's ~/.pilotdeck workspace.
process.env.HOME=join(root,'home');freezeClock();
const {MemoryRepository}=await import(pathToFileURL(join(baseline,source)).href);
const repos=openRepos(MemoryRepository,root);
const ws=(repo:string,action:string,input:any)=>({repo,method:'workspace',action,input});
const candidate=(type:string,name:string,extra={})=>({type,scope:type==='user'?'global':'project',name,description:`${name} description`,summary:`${name} summary`,profile:`${name} profile`,capturedAt:'2026-10-22T00:00:00.000Z',sourceSessionKey:`session-${name}`,...extra});
const seed:any[]=[];
// Persist each repo's workspaceDir like the accessors fixture, so the general
// repo's readable project catalog lists the sibling workspace stores as
// workspace_external entries — which getOverview must exclude from
// currentProjectCount. The values are fixed paths (not $ROOT, which would
// bake the run-specific scratch root into the database bytes): general's
// marker merely differs from every sibling workspace and from the pilot home.
for(const c of configs) seed.push({repo:c.id,method:'state',input:{key:'workspaceDir',value:c.id==='general'?'/fixture/General':c.workspace}});
// Project-mode memory: metas, live and deprecated entries per repo.
for(const repo of ['main','target','beta']) {
  seed.push(ws(repo,'upsertProjectMeta',{projectName:`${repo} project`}));
  seed.push(ws(repo,'upsertCandidate',candidate('project',`${repo} project memory`)));
  seed.push(ws(repo,'upsertCandidate',candidate('feedback',`${repo} feedback memory`)));
  seed.push(ws(repo,'upsertCandidate',candidate('feedback',`deprecated ${repo}`,{status:'deprecated'})));
}
// General-mode catalog: three local readable projects plus a mirror meta
// whose source workspace does not exist, so it is dropped from the local
// listing instead of becoming a catalog entry. The external entries the
// catalog does get are the sibling workspace stores seeded above.
for(const [id,name] of [['g1','Alpha'],['g2','Zebra'],['g3','中文']] as const) {
  seed.push(ws('general','upsertGeneralProjectMeta',{projectId:id,projectName:name}));
  seed.push(ws('general','upsertCandidate',candidate('project',name,{projectId:id})));
}
seed.push(ws('general','upsertGeneralProjectMeta',{projectId:'external',projectName:'External mirror',sourceKind:'workspace_external_mirror',sourceWorkspacePath:'/fixture/Alpha/../Alpha',sourceProjectId:'current_project'}));
// Global user memory shared by every repo's global store, plus the managed
// user profile file so overview's userProfileCount sees the 1-branch.
seed.push({method:'global',action:'upsertCandidate',input:candidate('user','Global user')},{method:'global',action:'upsertCandidate',input:candidate('user','Second user')},{method:'global',action:'upsertUserProfile',input:candidate('user','Global profile')});
// Pipeline state consumed by export and overview; beta's status is invalid so
// sanitizeDreamStatus must drop it from both.
seed.push(
  {repo:'main',method:'state',input:{key:'lastIndexedAt',value:'2026-10-20T01:02:03.000Z'}},
  {repo:'main',method:'state',input:{key:'lastDreamAt',value:'2026-10-21T04:05:06.000Z'}},
  {repo:'main',method:'state',input:{key:'lastDreamStatus',value:'success'}},
  {repo:'main',method:'state',input:{key:'lastDreamSummary',value:'Dream summary text'}},
  {repo:'main',method:'settings',input:{partial:{reasoningMode:'accuracy_first',autoIndexIntervalMinutes:45},defaults:{reasoningMode:'answer_first',autoIndexIntervalMinutes:30,autoDreamIntervalMinutes:60}}},
  {repo:'beta',method:'state',input:{key:'lastDreamAt',value:'2026-10-19T00:00:00.000Z'}},
  {repo:'beta',method:'state',input:{key:'lastDreamStatus',value:'bogus'}},
);
// Traces: sorted newest-first, exercising the index/dream normalizers
// (isNoOp, displayStatus) in export output.
seed.push(
  {repo:'main',method:'case',input:{record:{caseId:'case-1',startedAt:'2026-10-21T08:00:00.000Z',status:'completed',storedResults:[{relativePath:'Project/a.md',reason:'recall'}]},limit:30}},
  {repo:'main',method:'case',input:{record:{caseId:'case-2',startedAt:'2026-10-21T09:30:00.000Z',status:'running',storedResults:[]},limit:30}},
  {repo:'main',method:'index',input:{record:{indexTraceId:'idx-1',startedAt:'2026-10-21T07:00:00.000Z',status:'error',storedResults:[{relativePath:'Feedback/b.md'}],error:'boom'},limit:30}},
  {repo:'main',method:'index',input:{record:{indexTraceId:'idx-2',startedAt:'2026-10-21T07:30:00.000Z',status:'completed',storedResults:[]},limit:30}},
  {repo:'main',method:'dream',input:{record:{dreamTraceId:'dream-1',startedAt:'2026-10-21T06:00:00.000Z',status:'success',outcome:{deletedFiles:1,rewrittenProjects:2,profileUpdated:true},summary:'dream one'},limit:30}},
  {repo:'main',method:'dream',input:{record:{dreamTraceId:'dream-2',startedAt:'2026-10-21T05:00:00.000Z',status:'completed',outcome:{deletedFiles:0,rewrittenProjects:0,profileUpdated:false}},limit:30}},
);
// L0 sessions: duplicate session key plus an indexed row, so pendingSessions
// counts DISTINCT unindexed keys.
seed.push(
  {repo:'main',method:'l0',input:{l0IndexId:'l0-a',sessionKey:'session-alpha',timestamp:'2026-10-21T10:00:00.000Z',messages:[{role:'user',content:'hello'}],source:'openclaw',indexed:false,createdAt:'2026-10-21T10:00:00.000Z'}},
  {repo:'main',method:'l0',input:{l0IndexId:'l0-b',sessionKey:'session-alpha',timestamp:'2026-10-21T11:00:00.000Z',messages:[],source:'openclaw',indexed:true,createdAt:'2026-10-21T11:00:00.000Z'}},
  {repo:'main',method:'l0',input:{l0IndexId:'l0-c',sessionKey:'session-beta',timestamp:'2026-10-21T12:00:00.000Z',messages:[{role:'assistant',content:'hi'}],source:'claude',indexed:false,createdAt:'2026-10-21T12:00:00.000Z'}},
  {repo:'beta',method:'l0',input:{l0IndexId:'l0-x',sessionKey:'session-gamma',timestamp:'2026-10-21T09:00:00.000Z',messages:[],source:'openclaw',indexed:false,createdAt:'2026-10-21T09:00:00.000Z'}},
);
for(const op of seed) {const result=invoke(repos,op,root,{});assert.ok(!result?.error,JSON.stringify(result));}
for(const id of ['main','target','beta']) {
  const entries=repos[id].getFileMemoryStore().listMemoryEntries({includeDeprecated:true});
  const op=ws(id,'markEntriesDeprecated',{relativePaths:entries.filter((e:any)=>e.name.startsWith('deprecated')).map((e:any)=>e.relativePath)});
  seed.push(op);const result=invoke(repos,op,root,{});assert.ok(!result?.error,JSON.stringify(result));
}
// Distinct update times exercise newest-first sorting in list/overview.
for(const [repo,name,time] of [['main','main project memory','2026-10-23T00:00:00.000Z'],['target','target project memory','2026-10-19T00:00:00.000Z']] as const) {
  const entry=repos[repo].getFileMemoryStore().listMemoryEntries({includeDeprecated:true}).find((e:any)=>e.name===name && e.type==='project');
  assert.ok(entry);
  const path=`workspaces/${repo}/memory/${entry.relativePath}`;
  const content=readFileSync(join(root,path),'utf8').replace('updated_at: 2026-10-22T00:00:00.000Z',`updated_at: ${time}`);
  const op={repo,method:'raw',input:{path,content}};seed.push(op);invoke(repos,op,root,{});
}
for(const r of Object.values(repos) as any[])r.close();
// last_dream snapshots: target's `after` versions match the live state
// (rollbackReady true), beta's do not (warning branch). The file trees are
// copies of the live stores; versions are computed with TS's own
// snapshotVersionFromFiles algorithm over TS-produced files.
const hashText=(input:string)=>createHash('sha1').update(input).digest('hex').slice(0,10);
const snapshotVersion=(store:any)=>hashText(JSON.stringify(store.exportSnapshotFiles().filter((f:any)=>f.relativePath!=='MEMORY.md').sort((a:any,b:any)=>a.relativePath.localeCompare(b.relativePath))));
const collectTree=(dir:string)=>{const files:{relativePath:string,content:string}[]=[];const walk=(current:string,rel:string)=>{for(const e of readdirSync(current,{withFileTypes:true}).sort((a,b)=>a.name.localeCompare(b.name))){const p=join(current,e.name);const r=rel?`${rel}/${e.name}`:e.name;e.isDirectory()?walk(p,r):files.push({relativePath:r,content:readFileSync(p,'utf8')});}};walk(dir,'');return files;};
const seedSnapshot=(repo:string,afterOverride?:string)=>{
  const side=(label:string,files:{relativePath:string,content:string}[])=>{for(const f of files){const op={repo,method:'raw',input:{path:`workspaces/${repo}/last_dream/${label}/${f.relativePath}`,content:f.content}};seed.push(op);invoke(repos,op,root,{});}};
  side('workspace',collectTree(join(root,'workspaces',repo,'memory')));
  side('global',collectTree(join(root,'global')));
  const metadata={version:1,capturedAt:'2026-10-21T06:00:00.000Z',sourceAction:'dream_pipeline',sourceWorkspaceDir:expand(configs.find(c=>c.id===repo)!.workspace,root),trigger:'interval',dreamTraceId:'dream-1',summary:'snapshot summary',after:{workspaceVersion:afterOverride??snapshotVersion(repos[repo].getFileMemoryStore()),globalVersion:snapshotVersion(repos[repo].getGlobalUserStore())}};
  const op={repo,method:'raw',input:{path:`workspaces/${repo}/last_dream/metadata.json`,content:JSON.stringify(metadata)}};seed.push(op);invoke(repos,op,root,{});
};
seedSnapshot('target');
seedSnapshot('beta','wrong-version');
const seedFiles=fileTree(root);
const databases=configs.map(c=>({id:c.id,base64:readFileSync(join(root,'workspaces',c.id,'control.sqlite')).toString('base64')}));
const bundle=(extra:any)=>({formatVersion:'clawxmemory-memory-snapshot.v4',scope:'current_project',files:[],...extra});
const ops:any[]=[
  {method:'mode'},{repo:'target',method:'mode'},{repo:'general',method:'mode'},
  {method:'export',saveAs:'b1'},{repo:'target',method:'export',saveAs:'b2'},{repo:'empty',method:'export',saveAs:'bEmpty'},{repo:'beta',method:'export',saveAs:'bBeta'},
  ...['main','target','beta','empty','general'].map(repo=>({repo,method:'overview'})),
  {method:'uiSnapshot'},{method:'uiSnapshot',input:{limit:2}},{repo:'general',method:'uiSnapshot',input:{limit:0}},
  ...[{}, {scope:'project'}, {scope:'global'}, {kinds:['project']}, {kinds:[]}, {kinds:['user']}, {kinds:['project','user']}, {limit:1,offset:1}, {limit:0}, {offset:5}, {includeDeprecated:true}].map(input=>({method:'list',input})),
  // NOTE: a {projectId} option is deliberately absent — the Rust port of
  // listGlobalMemoryEntries forwards projectId to the global store while TS
  // does not; that passthrough belongs to #517's rework of accessors.rs.
  ...[{}, {scope:'project'}, {scope:'global'}, {kinds:['user']}, {kinds:[]}].map(input=>({method:'count',input})),
  // Import a TS-produced bundle, then observe the swapped state.
  {repo:'target',method:'import',input:{bundle:'@b1'},saveAs:'r1'},
  {repo:'target',method:'overview'},
  {repo:'target',method:'readState',input:{key:'lastIndexedAt'}},
  {repo:'target',method:'readState',input:{key:'lastDreamStatus'}},
  {repo:'target',method:'readState',input:{key:'recentCaseTraces'}},
  {repo:'target',method:'list',input:{}},
  {repo:'target',method:'export',saveAs:'b3'},
  // Legacy bundle with global files: staged, counted, then ignored with a
  // warning; invalid status drops, padded timestamps trim, junk traces filter.
  {repo:'empty',method:'import',input:{bundle:{formatVersion:'clawxmemory-memory-snapshot.v4',exportedAt:'2026-10-21T00:00:00.000Z',lastDreamStatus:'bogus',lastIndexedAt:'  2026-10-18T00:00:00.000Z  ',recentCaseTraces:[42,{caseId:'case-9',startedAt:'2026-10-01T00:00:00.000Z',status:'completed'}],files:[{relativePath:'global/UserIdentity/user-profile.md',content:'# Legacy profile\n'},{relativePath:'Project/Legacy.md',content:'# Legacy project note\n'}]}}},
  {repo:'empty',method:'overview'},
  // Validation errors: every message is the TS text verbatim and the live
  // roots stay untouched (verified by the next overview).
  ...[42, null, 'nope'].map(value=>({repo:'empty',method:'import',input:{bundle:value}})),
  {repo:'empty',method:'import',input:{bundle:bundle({formatVersion:'bogus.v4'})}},
  {repo:'empty',method:'import',input:{bundle:bundle({scope:'workspace'})}},
  {repo:'empty',method:'import',input:{bundle:({formatVersion:'clawxmemory-memory-snapshot.v4',scope:'current_project'})}},
  {repo:'empty',method:'import',input:{bundle:bundle({files:[42]})}},
  {repo:'empty',method:'import',input:{bundle:bundle({files:[{relativePath:'a.md'}]})}},
  {repo:'empty',method:'import',input:{bundle:bundle({files:[{content:'x'}]})}},
  {repo:'empty',method:'import',input:{bundle:bundle({files:[{relativePath:'  a/../b.md ',content:'x'}]})}},
  {repo:'empty',method:'import',input:{bundle:bundle({files:[{relativePath:'/abs.md',content:'x'}]})}},
  {repo:'empty',method:'import',input:{bundle:bundle({files:[{relativePath:'./dot.md',content:'x'}]})}},
  {repo:'empty',method:'import',input:{bundle:bundle({files:[{relativePath:'a.md',content:'1'},{relativePath:'a.md',content:'2'}]})}},
  {repo:'empty',method:'import',input:{bundle:bundle({files:[{relativePath:'projects/x.md',content:''}]})}},
  {repo:'empty',method:'import',input:{bundle:bundle({files:[{relativePath:'a/project.meta.md',content:''}]})}},
  {repo:'empty',method:'overview'},
  // Partial failure: the swap and manifest repair land, then the closed
  // database fails the runtime reset, leaving the imported files behind.
  {repo:'beta',method:'close'},
  {repo:'beta',method:'import',input:{bundle:'@b2'}},
  {repo:'beta',method:'overview'},
  {repo:'beta',method:'reopen',input:{workspace:'/fixture/Beta'}},
  {repo:'beta',method:'overview'},
];
const result=observe(MemoryRepository,root,ops);
const oracle={sourceCommit:provenance.ref,sourceSha256,nodeVersion:process.version,configs,seed,seedFiles,databases,operations:ops,...result};
mkdirSync(out,{recursive:true});const text=JSON.stringify(oracle,null,2)+'\n';writeFileSync(join(out,'oracle.json'),text);console.log(`sha256 ${createHash('sha256').update(text).digest('hex')}; ${ops.length} observations`);
rmSync(root,{recursive:true,force:true});
