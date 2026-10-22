import fs from 'node:fs';
import {syncBuiltinESMExports} from 'node:module';
import {join,dirname,basename} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
import {freezeClock} from '../edgeclaw-file-memory/observe.mts';
export {freezeClock};
export const source='src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';
export function clean(v:any,root:string):any {
  if(typeof v==='string')return v.replaceAll(fs.realpathSync(root),'$ROOT').replaceAll(root,'$ROOT').replace(/\.(memory|global|control\.sqlite|last_dream)\.([a-zA-Z0-9_-]+?)-[a-z0-9]+(?:-[a-z0-9]+)?(?=\/|$)/g,'.$1.$2-TEMP');
  if(Array.isArray(v))return v.map(x=>clean(x,root));
  if(v&&typeof v==='object')return Object.fromEntries(Object.entries(v).map(([k,x])=>[k,clean(x,root)]));
  return v===undefined?null:v;
}
export function open(Repo:any,root:string){return new Repo(join(root,'control.sqlite'),{memoryDir:join(root,'memory'),globalRootDir:join(root,'global'),workspaceDir:'/fixture/workspace'});}
export const candidate=(type:string,name:string)=>({type,scope:type==='user'?'global':'project',name,description:name+' description',summary:name+' summary',profile:name+' profile',capturedAt:'2026-10-22T00:00:00.000Z',sourceSessionKey:'session-'+name});
export function seed(r:any){r.insertL0Session({l0IndexId:'retained-l0',sessionKey:'retained-session',timestamp:'2026-10-22T00:00:00.000Z',messages:[{role:'user',content:'retained'}],source:'openclaw',indexed:false,createdAt:'2026-10-22T00:00:00.000Z'});r.getFileMemoryStore().upsertProjectMeta({projectName:'Snapshot project'});for(const type of ['project','feedback'])r.getFileMemoryStore().upsertCandidate(candidate(type,type));r.getGlobalUserStore().upsertCandidate(candidate('user','user'));r.setPipelineState('workspaceDir',' /fixture/workspace ');r.setPipelineState('lastDreamAt',' 2026-before ');r.setPipelineState('lastDreamStatus','success');r.setPipelineState('lastDreamSummary',' before summary ');r.setPipelineState('unrelated',{keep:true});}
const capture={method:'capture',saveAs:'before'};
const change={method:'edit',type:'project',name:'new'};
const after={method:'capture',saveAs:'after'};
const install={method:'install',before:'before',after:'after'};
const base=[capture,change,after,install];
const fault=(kind:string,contains:string,code='EIO',nth=1)=>({kind,contains,code,nth});
export const scenarios:any[]=[
{name:'roundtrip',ops:[{method:'options'},capture,{method:'stage',id:'dream'},{method:'stageCapture',id:'dream'},{method:'stageEdit',id:'dream',type:'feedback',name:'staged'},{method:'closeStage',id:'dream'},{method:'swap',id:'dream',fallback:'before'},{method:'dispose',id:'dream'},after,install,{method:'overview'},{method:'state',key:'lastDreamAt',value:'after-time'},{method:'state',key:'lastDreamSummary',value:'after-summary'},{method:'rollback'},{method:'capture'},{method:'overview'},{method:'rollback'},{method:'capture'},{method:'clear'},{method:'overview'},{method:'rollback'}]},
{name:'global-and-collation',ops:[capture,...['中文','éclair','Zebra','apple'].map(name=>({method:'edit',type:'project',name})),{method:'edit',type:'user',name:'new-user'},after,install,{method:'overview'},{method:'rollback'},{method:'capture'},{method:'rollback'},{method:'edit',type:'user',name:'drift'},{method:'overview'},{method:'rollback'}]},
{name:'runtime-values',ops:[...[null,2,'',' \t\u00a0 ', ' padded '].flatMap(value=>[{method:'state',key:'lastDreamAt',value},{method:'state',key:'lastDreamSummary',value},{method:'capture'}]),...['running','success','skipped','failed','FAILED',' success ',null,1].flatMap(value=>[{method:'state',key:'lastDreamStatus',value},{method:'capture'}])]},
{name:'stale-runtime-independent',ops:[...base,{method:'state',key:'lastDreamStatus',value:'failed'},{method:'overview'},{method:'raw',path:'memory/unmanaged.txt',content:'unmanaged ignored'},{method:'overview'},change,{method:'overview'},{method:'raw',path:'memory/Project/untracked.md',content:'not frontmatter'},{method:'overview'},{method:'edit',type:'feedback',name:'drift'},{method:'overview'},{method:'rollback'}]},
{name:'malformed-metadata',ops:[{method:'rollback'},{method:'raw',path:'last_dream/metadata.json',content:'{'},{method:'overview'},{method:'raw',path:'last_dream/metadata.json',content:'{"version":2}'},{method:'overview'},{method:'raw',path:'last_dream/metadata.json',content:'{"version":1}'},{method:'overview'},{method:'mkdir',path:'last_dream/workspace'},{method:'mkdir',path:'last_dream/global'},{method:'overview'},{method:'rollback'},{method:'raw',path:'last_dream/workspace/extra.txt',content:'ignored by live but load all files'},{method:'clear'},{method:'clear'}]},
{name:'metadata-optionals',ops:[...base,{method:'install',before:'before',after:'after',extra:{trigger:'manual',dreamTraceId:'trace',summary:'metadata summary'}},{method:'overview'},{method:'install',before:'before',after:'after',extra:{trigger:'',dreamTraceId:'',summary:''}},{method:'overview'},{method:'rollback'}]},
{name:'restore-empty-runtime',ops:[{method:'state',key:'lastDreamAt',value:''},{method:'state',key:'lastDreamStatus',value:'bad'},{method:'state',key:'lastDreamSummary',value:null},...base,{method:'state',key:'lastDreamAt',value:'after'},{method:'state',key:'lastDreamStatus',value:'running'},{method:'state',key:'lastDreamSummary',value:'after'},{method:'rollback'},{method:'capture'}]},
...[
['snapshot-workspace-write',fault('Write','.last_dream.snapshot-TEMP/workspace/MEMORY.md')],
['snapshot-global-write',fault('Write','.last_dream.snapshot-TEMP/global/')],
['snapshot-metadata-write',fault('Write','.last_dream.snapshot-TEMP/metadata.json')],
['snapshot-mkdir',fault('Mkdir','.last_dream.snapshot-TEMP/global')],
['snapshot-first-rename',fault('Rename','last_dream ->')],
['snapshot-second-rename',fault('Rename','.last_dream.snapshot-TEMP ->')],
['snapshot-copy-denied',fault('Rename','.last_dream.snapshot-TEMP ->','EACCES')],
].map(([name,f])=>({name,ops:[...base,{...install,faults:[f]},{method:'overview'}]})),
...[
['stage-workspace-write',fault('Write','.memory.dream-TEMP/MEMORY.md')],
['stage-global-write',fault('Write','.global.dream-TEMP/')],
['stage-second-mkdir',fault('Mkdir','.global.dream-TEMP')],
].map(([name,f])=>({name,ops:[capture,{method:'stage',id:'dream',faults:[f]}]})),
...[
['workspace-backup-fail',fault('Rename','memory ->')],
['workspace-install-fail',fault('Rename','.memory.dream-TEMP ->')],
['global-backup-fail',fault('Rename','global ->')],
['global-install-fail',fault('Rename','.global.dream-TEMP ->')],
['rename-permission-copy',fault('Rename','.memory.dream-TEMP ->','EPERM')],
['best-effort-backup-cleanup',fault('Remove','.memory.backup-TEMP')],
].map(([name,f])=>({name,ops:[capture,{method:'stage',id:'dream'},{method:'stageEdit',id:'dream',type:'project',name:'changed'},{method:'closeStage',id:'dream'},{method:'swap',id:'dream',fallback:'before',faults:[f]},{method:'dispose',id:'dream'},{method:'capture'}]})),
{name:'rollback-snapshot-installed-before-stage-failure',ops:[...base,{method:'rollback',faults:[fault('Write','.memory.rollback-TEMP/MEMORY.md')]},{method:'overview'},{method:'capture'}]},
{name:'rollback-global-failure',ops:[...base,{method:'rollback',faults:[fault('Rename','.global.rollback-TEMP ->')]},{method:'overview'},{method:'capture'}]},
{name:'rollback-runtime-delete-failure',ops:[...base,{method:'sql',sql:"CREATE TRIGGER fail_delete BEFORE DELETE ON pipeline_state WHEN OLD.state_key='lastDreamStatus' BEGIN SELECT RAISE(ABORT, 'injected delete'); END;"},{method:'rollback'},{method:'capture'}]},
{name:'rollback-runtime-insert-failure',ops:[...base,{method:'sql',sql:"CREATE TRIGGER fail_insert BEFORE INSERT ON pipeline_state WHEN NEW.state_key='lastDreamStatus' BEGIN SELECT RAISE(ABORT, 'injected insert'); END;"},{method:'rollback'},{method:'capture'}]},
{name:'unwritable-directory',ops:[capture,{method:'chmod',path:'.',mode:0o500},{method:'stage',id:'dream'},{method:'install',before:'before',after:'before'},{method:'chmod',path:'.',mode:0o700}]},
{name:'two-instances',ops:[capture,{method:'second'},{method:'stage',id:'one'},{method:'stage',id:'two',second:true},{method:'stageEdit',id:'one',type:'project',name:'one'},{method:'stageEdit',id:'two',type:'feedback',name:'two'},{method:'closeStage',id:'one'},{method:'closeStage',id:'two'},{method:'swap',id:'one',fallback:'before'},{method:'swap',id:'two',fallback:'before',second:true},{method:'dispose',id:'one'},{method:'dispose',id:'two'},{method:'capture'}]},
{name:'invalid-snapshot-paths',ops:[capture,...['../escape','/outside','.', '..name'].map(path=>({method:'install',before:'before',after:'before',path})),{method:'overview'}]},
{name:'copy-failure',ops:[capture,{method:'stage',id:'dream'},{method:'closeStage',id:'dream'},{method:'swap',id:'dream',fallback:'before',faults:[fault('Rename','.memory.dream-TEMP ->','EACCES'),fault('Copy','.memory.dream-TEMP ->')]},{method:'dispose',id:'dream'}]},
{name:'permission-copy-retains-symlinks',ops:[capture,{method:'raw',path:'memory/unmanaged.txt',content:'retained'},{method:'symlink',path:'memory/retained-link',target:'unmanaged.txt'},{method:'stage',id:'dream'},{method:'closeStage',id:'dream'},{method:'swap',id:'dream',fallback:'before',faults:[fault('Rename','memory ->','EPERM'),fault('Rename','.memory.dream-TEMP ->')]},{method:'dispose',id:'dream'}]},
{name:'snapshot-record-all-files',ops:[...base,{method:'raw',path:'last_dream/workspace/other.txt',content:'unmanaged snapshot record'},{method:'symlink',path:'last_dream/workspace/skip-link',target:'other.txt'},{method:'rollback'},{method:'capture'}]},
{name:'cleanup-masks-write',ops:[capture,{method:'stage',id:'dream',faults:[fault('Write','.memory.dream-TEMP/MEMORY.md'),fault('Remove','.memory.dream-TEMP','ENOSPC')]}]},
];
export function disk(root:string):any[] {
 const out:any[]=[];
 function walk(dir:string){for(const e of fs.readdirSync(dir,{withFileTypes:true})) {const p=join(dir,e.name);const relativePath=clean(p.slice(root.length+1),root);if(e.isSymbolicLink()){out.push({relativePath,link:clean(fs.readlinkSync(p),root)});}else if(e.isDirectory()){out.push({relativePath,dir:true});walk(p);}else if(/control\.sqlite/.test(e.name)) {if(!/-wal$|-shm$/.test(e.name)){const db=new DatabaseSync(p,{readOnly:true});const rows=db.prepare('SELECT * FROM pipeline_state ORDER BY state_key').all().map((r:any)=>{if(typeof r.updated_at!=='string'||!/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/.test(r.updated_at))throw Error('invalid pipeline timestamp');return {...r,updated_at:'<CLOCK>'};});const l0Rows=db.prepare('SELECT * FROM l0_sessions ORDER BY l0_index_id').all();const schema=db.prepare("SELECT name,type,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name,type").all();out.push({relativePath,rows,l0Rows,schema});db.close();}}else {out.push({relativePath,content:clean(fs.readFileSync(p,'utf8'),root)});}}}
 walk(root);return out.sort((a,b)=>a.relativePath<b.relativePath?-1:a.relativePath>b.relativePath?1:0);
}
function boundary(s:any){return {workspaceVersion:s.workspaceVersion,globalVersion:s.globalVersion,counts:s.counts,runtimeState:s.runtimeState};}
export function observe(Repo:any,root:string,ops:any[]) {
 const r=open(Repo,root);let second:any;const vars:any={};const stages:any={};const observations:any[]=[];
 for(const op of ops) {
  const repo=op.second?second:r;let result:any;const fired:any[]=[];const original:any={};const counts=new Map();
  for(const [name,kind] of Object.entries({mkdirSync:'Mkdir',writeFileSync:'Write',renameSync:'Rename',cpSync:'Copy',rmSync:'Remove'})) {
    original[name]=(fs as any)[name];(fs as any)[name]=(...args:any[])=>{
      const path=clean(String(args[0]),root)+(kind==='Rename'||kind==='Copy'?' -> '+clean(String(args[1]),root):'');
      for(const [i,f] of (op.faults??[]).entries())if(f.kind===kind&&path.includes(f.contains)){const n=(counts.get(i)??0)+1;counts.set(i,n);if(n===f.nth){fired.push(i);const e:any=new Error('injected');e.code=f.code;throw e;}}
      return original[name](...args);
    };
  }syncBuiltinESMExports();
  try {switch(op.method){
    case 'options':result=[repo.workspaceStoreOptions(),repo.globalStoreOptions()];break;
    case 'capture':result=repo.captureCurrentMemorySnapshot();if(op.saveAs)vars[op.saveAs]=result;break;
    case 'state':repo.setPipelineState(op.key,op.value);break;
    case 'edit':result=(op.type==='user'?repo.getGlobalUserStore():repo.getFileMemoryStore()).upsertCandidate(candidate(op.type,op.name));break;
    case 'stage':{const st=repo.createDreamStage(op.id);stages[op.id]=st;result={snapshot:st.snapshot,workspace:st.stagedWorkspaceRoot,global:st.stagedGlobalRoot,db:st.stagedDbPath};break;}
    case 'stageEdit':{const r=stages[op.id].repository;result=(op.type==='user'?r.getGlobalUserStore():r.getFileMemoryStore()).upsertCandidate(candidate(op.type,op.name));break;}
    case 'stageCapture':result=stages[op.id].repository.captureCurrentMemorySnapshot();break;
    case 'closeStage':stages[op.id].repository.close();break;
    case 'swap':repo.replaceLiveRootsWithStage(stages[op.id],vars[op.fallback]);break;
    case 'dispose':stages[op.id].dispose();break;
    case 'install':{const before=vars[op.before],after=vars[op.after];const snap=op.path?{...before,workspaceFiles:[{relativePath:op.path,content:'bad'}]}:before;repo.installLastDreamSnapshot(snap,{version:1,capturedAt:'2026-10-21T01:02:03.000Z',sourceAction:'dream',sourceWorkspaceDir:'/fixture/workspace',before:boundary(before),after:boundary(after),...op.extra});break;}
    case 'overview':result=repo.getLastDreamSnapshotOverview();break;
    case 'rollback':result=repo.rollbackLastDreamSnapshot();break;
    case 'clear':repo.clearLastDreamSnapshot();break;
    case 'raw':{const p=join(root,op.path);fs.mkdirSync(dirname(p),{recursive:true});fs.writeFileSync(p,op.content);break;}
    case 'mkdir':fs.mkdirSync(join(root,op.path),{recursive:true});break;
    case 'chmod':fs.chmodSync(join(root,op.path),op.mode);break;
    case 'symlink':fs.symlinkSync(op.target,join(root,op.path));break;
    case 'sql':repo.db.exec(op.sql);break;
    case 'second':second=open(Repo,root);break;
    default:throw Error('unknown method '+op.method);
  }}catch(e:any){result=e.code?{error:e.code}: {error:e.name,message:e.message};}
  finally {Object.assign(fs,original);syncBuiltinESMExports();}
  observations.push({result:clean(result,root),fired,disk:disk(root)});
 }
 for(const stage of Object.values(stages) as any[])try{stage.repository.close();}catch{}
 if(second)second.close();r.close();return observations;
}
