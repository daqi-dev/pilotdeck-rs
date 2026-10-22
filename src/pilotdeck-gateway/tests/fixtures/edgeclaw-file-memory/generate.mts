// Expectations are produced only by executing the unmodified fixed TS store.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtempSync, mkdirSync, writeFileSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import {bindSourceTree,sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {freezeClock,replay,readBack,clean,tree} from './observe.mts';
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const source = 'src/context/memory/edgeclaw-memory-core/src/core/file-memory.ts';
const baseline = resolve(process.env.PDRS_BASELINE!); const output = resolve(process.argv[2]!);
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which',['git'],{encoding:'utf8'}).trim());
const provenance = bindSourceTree(baseline,commit,git);
const sqliteSource = 'src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';
const sourceSha256 = sourceClosureSha256(baseline,commit,git,[source,sqliteSource]);
freezeClock();
const {FileMemoryStore} = await import(pathToFileURL(join(baseline,source)).href);
const scratch = mkdtempSync(join(tmpdir(),'pdrs-file-memory-'));
const op = (method: string,input: any = {},saveAs?: string) => ({method,input,...(saveAs?{saveAs}:{})});
const candidate = (type: string,name: string,extra = {}) => ({type,scope:type==='user'?'global':'project',name,description:`${name}  description`,capturedAt:'2026-10-20T00:00:00Z',sourceSessionKey:`session-${name}`,...extra});
const project = candidate('project','  产品 🚀 Café  ',{stage:'  active\n work ',summary:'项目摘要\ud800😀',decisions:[' a ','a','b'],constraints:['never'],nextSteps:['next'],blockers:['none'],timeline:['today'],notes:['note']});
const feedback = candidate('feedback','Feedback',{rule:'  use\tRust ',why:'fast',howToApply:'always',notes:['A','A','B']});
const raw = (type: string,scope: string,name: string,body: string,extra='') => `---\nname: ${name}\ndescription: ${name}\ntype: ${type}\nscope: ${scope}\nupdated_at: 2026-10-22T00:00:00.000Z\n${extra}---\n${body}\n`;
const scenarios: any[] = [];
function run(name:string,options:any,operations:any[]) { const root = join(scratch,name); const result = replay(FileMemoryStore,root,options,operations); scenarios.push({name,options,operations,...result}); }
run('single',{},[
  op('repairManifests'),op('repairManifests'),op('getOverview'),op('getUserSummary'),op('getProjectMeta'),
  op('getMemoryRecordsByIds',{ids:['missing','Project/missing.md','./missing','a/../../missing']}),
  op('upsertCandidate',project,'project'),op('upsertCandidate',feedback,'feedback'),
  op('upsertUserProfile',candidate('user','用户😀',{profile:'工程师，住北京。喜欢Rust!x',relationships:['团队负责人','工程师']}),'user'),
  op('getUserSummary'),op('upsertCandidate',{...project,name:'Updated Name',summary:'new content'},'updated'),
  op('toCandidate',{record:'@updated'}),op('toCandidate',{record:'@feedback'}),op('toCandidate',{record:'@user'}),
  op('listMemoryEntries'),op('listMemoryEntries',{query:'café',includeDeprecated:true,limit:1,offset:0}),
  op('listMemoryEntries',{projectId:'other'}),op('listMemoryEntries',{scope:'global'}),op('countMemoryEntries',{kinds:['project']}),
  op('getMemoryRecordsByIds',{ids:['@updated.relativePath','@user.relativePath'],maxLines:2}),
  op('getMemoryRecordsByIds',{ids:['@updated.relativePath'],maxLines:0}),
  op('editEntry',{relativePath:'@feedback.relativePath',name:'Feedback edited',description:' edited ',fields:{rule:'new rule',why:'new why',howToApply:' daily ',notes:['X','X']}}),
  op('editEntry',{relativePath:'@updated.relativePath',name:'Project renamed',description:'project desc',fields:{stage:'done',decisions:[],constraints:['bound'],nextSteps:['go'],blockers:[],timeline:['then'],notes:[]}}),
  op('markEntriesDeprecated',{relativePaths:['@feedback.relativePath','missing']}),op('getOverview',{lastDreamAt:'2026-10-23'}),op('listMemoryEntries'),op('listMemoryEntries',{includeDeprecated:true}),
  op('restoreEntries',{relativePaths:['@feedback.relativePath']}),op('reassignProjectEntries',{fromProjectId:'current_project',toProjectId:'other'}),op('hasVisibleProjectMemory'),op('hasVisibleProjectMemory',{projectId:'other'}),
  op('editProjectMeta',{projectName:' Name  ',description:' long\n desc ',status:' finished '}),op('upsertProjectMeta',{status:'   '}),op('ensureProjectMeta',{projectName:'ignored'}),op('listProjectMetas'),op('listProjectIdentityHints'),
  op('editProjectMeta',{projectId:'unknown',projectName:'x',description:'x',status:'x'}),op('archiveTmpEntries'),op('listTmpEntries'),
  op('writeRaw',{relativePath:'MEMORY.md',content:'BROKEN \ud800'}),op('repairManifests'),
  op('writeRaw',{relativePath:'Project/😀𠀀é.md',content:raw('project','project','孤立\ud800 😀','## Summary\nA\r\nB','dream_attempts: +12garbage\ndeprecated: false\n')}),
  op('writeRaw',{relativePath:'Project/invalid.md',content:'---\nname: no\ntype: unknown\nscope: project\n---\nbad'}),
  op('writeRaw',{relativePath:'Project/crlf.md',content:raw('project','project','CRLF','abc').replaceAll('\n','\r\n')}),
  op('writeRaw',{relativePath:'Project/nested/hidden.md',content:raw('project','project','hidden','abc')}),
  op('writeRaw',{relativePath:'Feedback/not-md.txt',content:'ignore'}),
  op('listMemoryEntries',{includeDeprecated:true}),op('getOverview'),op('getMemoryRecordsByIds',{ids:['Project/😀𠀀é.md','Project/invalid.md','Project/crlf.md']}),
  op('getSnapshotVersion',{lastDreamAt:'2026-10-20'}),op('exportBundleRecords'),op('exportSnapshotFiles'),op('deleteEntries',{relativePaths:['@feedback.relativePath','missing']}),op('exportSnapshotFiles'),
]);
run('general',{workspaceMode:'general'},[
  op('upsertGeneralProjectMeta',{projectId:'id1',projectName:'项目 一',description:' First ',status:'open',sourceKind:'workspace_external_mirror',sourceWorkspacePath:'/virtual/外部😀',sourceProjectId:'external',dreamUpdatedAt:'dream'},'meta'),
  op('upsertGeneralProjectMeta',{projectId:'id1',projectName:'Renamed',status:'closed'}),op('upsertGeneralProjectMeta',{projectId:'id1',projectName:'Renamed',status:'   '}),
  op('ensureProjectMeta',{projectId:'id1',projectName:'unused'}),op('upsertProjectMeta',{projectName:'Automatic'}),
  op('upsertGeneralProjectMeta',{projectId:'12345678901😀尾',projectName:'Long UTF16 ID'}),
  op('upsertCandidate',candidate('project','General',{projectId:'id1',summary:'data'}),'p'),
  op('upsertCandidate',candidate('feedback','Auto Feedback',{sourceSessionKey:'other'})),
  op('getProjectMeta',{projectId:'id1'}),op('listMemoryEntries',{kinds:['general_project_meta','project','feedback'],projectId:'id1'}),
  op('editProjectMeta',{projectId:'id1',projectName:'Edited',description:'edited desc',status:'done'}),op('editProjectMeta',{projectName:'bad',description:'bad',status:'bad'}),op('editProjectMeta',{projectId:'no',projectName:'bad'}),
  op('listProjectIdentityHints'),op('getOverview'),op('hasVisibleProjectMemory',{projectId:'id1'}),op('toCandidate',{record:'@p'}),op('exportBundleRecords'),op('getSnapshotVersion'),op('exportSnapshotFiles'),
]);
run('duplicates',{manageProjectMeta:false},[
  op('upsertCandidate',candidate('project','Same Name',{summary:'first',notes:['first'],sourceSessionKey:'one'}),'one'),
  op('upsertCandidate',candidate('project','Same Name',{summary:'second',notes:['second'],sourceSessionKey:'two'}),'two'),
  op('mergeDuplicateEntries'),op('exportBundleRecords'),op('exportSnapshotFiles'),op('clearAllData',{rebuildManifest:false}),op('getOverview'),op('repairManifests'),op('clearAllData'),op('exportSnapshotFiles'),
]);
run('append',{manageProjectMeta:false,manageProjectFiles:false,userProfileRelativePath:'UserIdentity/custom😀.md',userNotesRelativeDir:'UserIdentityNotes',appendOnlyUserEntries:true,enableManifest:false},[
  op('upsertCandidate',candidate('user','追加\ud800😀',{profile:'开发者',sourceSessionKey:'session\ud801'}),'one'),op('upsertCandidate',candidate('user','追加\ud800😀',{profile:'new',sourceSessionKey:'session\ud801'})),
  op('upsertCandidate',candidate('user','追加😀',{profile:'朋友',sourceSessionKey:'two'})),op('getUserSummary'),op('upsertUserProfile',candidate('user','Profile',{profile:'画像一\n画像二'})),op('getUserSummary'),op('getOverview'),op('repairManifests'),op('exportBundleRecords'),op('exportSnapshotFiles'),
]);
run('disabled',{manageUserProfile:false,manageProjectMeta:false,manageProjectFiles:false,enableManifest:false},[
  op('upsertUserProfile',candidate('user','no')),op('upsertCandidate',candidate('user','no')),op('upsertProjectMeta'),op('upsertGeneralProjectMeta',{projectName:'no'}),op('getUserSummary'),op('repairManifests'),op('getOverview'),op('hasVisibleProjectMemory'),op('listProjectMetas'),op('exportSnapshotFiles'),
]);
run('errors',{},[
  op('editEntry',{relativePath:'missing',name:'x',description:'x'}),op('removeDir',{relativePath:'Project'}),op('listMemoryEntries'),op('repairManifests'),
  op('writeRaw',{relativePath:'Project/as-directory.md/child',content:'child'}),op('getMemoryRecordsByIds',{ids:['Project/as-directory.md']}),
  op('chmod',{relativePath:'Feedback',mode:0}),op('listMemoryEntries'),op('getMemoryRecordsByIds',{ids:['Feedback/missing.md']}),op('chmod',{relativePath:'Feedback',mode:0o755}),
  op('writeRaw',{relativePath:'MEMORY.md',content:'broken'}),op('chmod',{relativePath:'MEMORY.md',mode:0}),op('repairManifests'),op('chmod',{relativePath:'MEMORY.md',mode:0o644}),op('repairManifests'),
  op('chmod',{relativePath:'Project',mode:0o555}),op('upsertCandidate',candidate('project','Denied write')),op('chmod',{relativePath:'Project',mode:0o755}),
]);
run('preview',{},[
  op('upsertCandidate',candidate('project','Surrogates',{notes:['\ud800','\ud801','�'],sourceSessionKey:'distinct'})),
  op('listMemoryEntries',{query:'\ud800'}),
  op('writeRaw',{relativePath:'Project/partial.md',content:'---\nname: Missing time\ndescription: desc\ntype: project\nscope: project\ndream_attempts: -3tail\n---\nbody'}),
  op('upsertCandidate',candidate('project','Preview',{body:`## Heading\n${'a'.repeat(211)}😀 end\n`,sourceSessionKey:'p'})),
  op('writeRaw',{relativePath:'Project/long.md',content:raw('project','project','Long',`${'a'.repeat(219)}😀end`)}),op('getMemoryRecordsByIds',{ids:['Project/long.md']}),
]);
run('edge-cases',{manageProjectMeta:false},[
  op('upsertUserProfile',{type:'user',scope:'global',name:'',description:''},'u'),op('getUserSummary'),
  op('editEntry',{relativePath:'@u.relativePath',name:'renamed',description:'desc',fields:{}}),op('getUserSummary'),
  op('upsertCandidate',{type:'project',scope:'project',name:'',description:''}),
  op('upsertCandidate',{type:'feedback',scope:'global',name:'fallback',description:'',summary:'summary'}),
  op('writeRaw',{relativePath:'Project/crlf-body.md',content:raw('project','project','Body','one\r\ntwo\r\nthree')}),
  op('getMemoryRecordsByIds',{ids:['Project/crlf-body.md'],maxLines:2}),
  op('listMemoryEntries',{limit:0,offset:-1}),op('listMemoryEntries',{limit:1.8,offset:0.5}),op('getMemoryRecordsByIds',{ids:['Project/crlf-body.md'],maxLines:0.5}),op('countMemoryEntries',{limit:1,offset:1}),
  op('deleteEntries',{relativePaths:['Project/../missing','Project/crlf-body.md']}),
]);
run('number-render',{manageProjectMeta:false},[
  op('writeRaw',{relativePath:'Project/number.md',content:raw('project','project','Number','body','dream_attempts: -0tail\n')}),
  op('markEntriesDeprecated',{relativePaths:['Project/number.md']}),op('exportSnapshotFiles'),
  op('writeRaw',{relativePath:'Project/number.md',content:raw('project','project','Number','body','dream_attempts: 1000000000000000000000tail\n')}),
  op('markEntriesDeprecated',{relativePaths:['Project/number.md']}),op('exportSnapshotFiles'),
]);
run('edit-meta-whitelist',{},[
  op('editProjectMeta',{projectName:'Initial',description:'first edit',status:'open',dreamUpdatedAt:'must-not-be-created'}),
  op('upsertProjectMeta',{projectName:'Seeded',dreamUpdatedAt:'existing-dream'}),
  op('editProjectMeta',{projectName:'Edited',description:'second edit',status:'done',dreamUpdatedAt:'must-not-replace-existing'}),
  op('getProjectMeta'),op('exportSnapshotFiles'),
]);
run('general-utf16',{workspaceMode:'general'},[
  op('upsertGeneralProjectMeta',{projectId:'id\ud800',projectName:'Lone ID',status:'open'},'m'),
  op('getMemoryRecordsByIds',{ids:['@m.relativePath']}),op('getProjectMeta',{projectId:'id\ud800'}),
  op('editProjectMeta',{projectId:'id\ud800',projectName:'no',description:'no',status:'no'}),
  op('ensureProjectMeta',{projectId:'id\ud800',projectName:'New name'}),
  op('listMemoryEntries',{kinds:['general_project_meta'],projectId:'id\ud800'}),
  op('editEntry',{relativePath:'missing\ud800.md',name:'no',description:'no'}),
]);
run('notes-null',{manageProjectMeta:false,appendOnlyUserEntries:true,userProfileRelativePath:null},[
  op('upsertCandidate',candidate('user','Hidden append',{profile:'stable profile'})),op('getUserSummary'),op('listMemoryEntries'),op('exportSnapshotFiles'),
]);
run('origin',{manageProjectMeta:false},Array.from({length:16},(_,i)=>op('upsertCandidate',candidate('project',`Origin ${i}`,{description:'same origin',sourceSessionKey:'same-session',capturedAt:'2026-10-22T00:00:00.000Z'}))));
run('concurrent',{manageProjectMeta:false},Array.from({length:12},(_,i)=>op('upsertCandidate',candidate('project',`并发${i}😀`,{description:'test',sourceSessionKey:`s${i}`,capturedAt:'2026-10-22T00:00:00.000Z'}))));
assert.equal(scenarios.find(s=>s.name==='errors').observations[7].error,'EACCES','permission probe must actually fail, not silently skip');
const {MemoryRepository} = await import(pathToFileURL(join(baseline,sqliteSource)).href);
const wiringRoot = join(scratch,'wiring');
const repo = new MemoryRepository(join(scratch,'wiring.sqlite'),{memoryDir:join(wiringRoot,'workspace/memory'),globalRootDir:join(wiringRoot,'global'),workspaceDir:join(wiringRoot,'workspace')});
const wiringCandidate = {type:'user',scope:'global',name:'User😀',description:'身份',profile:'Rust 工程师',capturedAt:'2026-10-22T00:00:00.000Z',sourceSessionKey:'s'};
repo.getGlobalUserStore().upsertCandidate(wiringCandidate);
repo.getFileMemoryStore().repairManifests();
const wiring = {candidate:wiringCandidate,global:clean(readBack(repo.getGlobalUserStore()),wiringRoot),workspace:clean(readBack(repo.getFileMemoryStore()),wiringRoot),files:tree(wiringRoot)};
repo.close();
// Both repository stores keep their distinct options even when roots resolve
// to the same directory. The workspace write synchronously calls its provider.
const sameRoot = join(scratch,'same-root');
const sameRepo = new MemoryRepository(join(scratch,'same-root.sqlite'),{memoryDir:join(sameRoot,'.'),globalRootDir:sameRoot,workspaceDir:join(scratch,'same-workspace')});
sameRepo.getGlobalUserStore().upsertCandidate(wiringCandidate);
sameRepo.getFileMemoryStore().upsertCandidate(project);
sameRepo.getFileMemoryStore().repairManifests();
const sameRootWiring = {candidate:project,global:clean(readBack(sameRepo.getGlobalUserStore()),sameRoot),workspace:clean(readBack(sameRepo.getFileMemoryStore()),sameRoot),files:tree(sameRoot)};
sameRepo.close();
const oracle = {wiring,sameRootWiring,sourceCommit:provenance.ref,sourceSha256,nodeVersion:process.version,scenarios};
mkdirSync(output,{recursive:true}); const text = `${JSON.stringify(oracle,null,2)}\n`; writeFileSync(join(output,'oracle.json'),text);
console.log(`oracle sha256 ${createHash('sha256').update(text).digest('hex')}; ${scenarios.length} scenarios`);
rmSync(scratch,{recursive:true,force:true});
