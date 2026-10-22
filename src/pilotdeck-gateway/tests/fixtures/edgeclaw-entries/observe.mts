import fs from 'node:fs';
import {createHash} from 'node:crypto';
import {join,dirname} from 'node:path';
import {open as snapshotOpen,seed as snapshotSeed,disk,clean,freezeClock,source,candidate} from '../edgeclaw-snapshots/observe.mts';
export {disk,clean,freezeClock,source,candidate};
export function open(Repo:any,root:string,general=false){return general?new Repo(join(root,'control.sqlite'),{memoryDir:join(root,'current/memory'),globalRootDir:join(root,'global'),workspaceDir:join(process.env.HOME!,'.pilotdeck')}):snapshotOpen(Repo,root);}
export function seed(r:any){snapshotSeed(r);r.setPipelineState('indexingSettings',{reasoningMode:'low',autoIndexIntervalMinutes:17,autoDreamIntervalMinutes:33});r.setPipelineState('null-state',null);}
const edit=(id:string)=>({method:'editMemoryEntry',input:{id,name:' Edited ',description:'new\n description',fields:{stage:'next',decisions:['one','one','two']}}});
const selected=(method:string,which:string[]=['project','user'])=>({method,select:which});
const clear=(method:string)=>({method});
export const scenarios:any[]=[
 {name:'entry-lifecycle',ops:[{method:'getProjectMeta'},{method:'ensureProjectMeta',input:{projectName:'ignored existing'}},{method:'editProjectMeta',input:{projectName:'Renamed',description:'new meta',status:'paused'}},selected('records',['user','project','project','missing']),{method:'records',ids:['',' ','external:', 'external: :x','external:unknown:Project/x.md','global\\UserIdentity\\user-profile.md'],maxLines:2},{...edit(''),select:['project']},{...edit(''),select:['user']},selected('deprecateMemoryEntries'),selected('deprecateMemoryEntries'),{method:'countMemoryEntries'},{method:'countMemoryEntries',input:{includeDeprecated:true,limit:1,offset:90}},{method:'records',select:['project','user'],maxLines:3},selected('restoreMemoryEntries'),selected('restoreMemoryEntries'),selected('deleteMemoryEntries',['project','project','user']),selected('restoreMemoryEntries'),selected('deleteMemoryEntries'),{method:'getProjectMeta'},{method:'getSnapshotVersion'}]},
 {name:'invalid-entry-operations',ops:[...['missing','../escape','external:unknown:x','global/missing'].map(id=>edit(id)),{method:'editMemoryEntry',select:['project'],input:{name:'',description:'',fields:{rule:' rule ',notes:['a','a','b']}}},...['deprecateMemoryEntries','restoreMemoryEntries','deleteMemoryEntries'].map(method=>({method,ids:['missing','../escape','external:unknown:x','global/missing','']})),{method:'archiveTmpEntries',input:{ids:['missing'],targetProjectId:'x'}},{method:'editProjectMeta',input:{projectId:'unknown',projectName:'bad',description:'bad',status:'bad'}},{method:'getSnapshotVersion'}]},
 {name:'counts-and-version',ops:[...[{}, {scope:'global'},{scope:'project'},{kinds:[]},{kinds:['feedback']},{query:'PROJECT'},{query:'no match'},{projectId:'missing'},{includeTmp:true,includeDeprecated:true}].map(input=>({method:'countMemoryEntries',input})),...['',null,2,false,{x:1}].flatMap(value=>[{method:'state',key:'lastDreamAt',value},{method:'getSnapshotVersion'}]),{method:'state',key:'recentCaseTraces',value:{bad:true}},{method:'getSnapshotVersion'},{method:'raw',path:'memory/MEMORY.md',content:'derived changed'},{method:'getSnapshotVersion'}]},
 ...['clearAllMemoryData','clearCurrentWorkspaceMemoryData','clearSessionMemoryData'].map(method=>({name:method,ops:[{method:'snapshot'},clear(method),clear(method),{method:'getProjectMeta'},{method:'records',select:['user']},{method:'getSnapshotVersion'}]})),
 {name:'empty-meta',ops:[clear('clearCurrentWorkspaceMemoryData'),{method:'getProjectMeta'},{method:'ensureProjectMeta',input:{projectName:'Created',description:'created description',status:'done'}},{method:'getProjectMeta'}]},
 {name:'clear-session-selection',ops:[selected('deprecateMemoryEntries',['project']),{method:'clearSessionMemoryData',session:' session-project '},{method:'clearSessionMemoryData',session:'session-user'},{method:'clearSessionMemoryData',session:'retained-session'},{method:'clearSessionMemoryData',session:' \t\u00a0 '},{method:'clearSessionMemoryData',session:'missing'}]},
 ...['clearAllMemoryData','clearCurrentWorkspaceMemoryData'].flatMap(method=>[
 {name:method+'-dangling-root',ops:[{method:'replaceWithLink',path:'memory',target:'missing-target'},clear(method)]},
 {name:method+'-sql-fail',ops:[{method:'sql',sql:"CREATE TRIGGER fail_clear BEFORE DELETE ON pipeline_state WHEN OLD.state_key='indexingSettings' BEGIN SELECT RAISE(ABORT,'injected clear'); END;"},clear(method)]},
 {name:method+'-workspace-remove-fail',ops:[{method:'snapshot'},{method:'chmod',path:'memory/Project',mode:0o500},clear(method)]},
 {name:method+'-symlink-root',ops:[{method:'snapshot'},{method:'raw',path:'outside',content:'do not remove target'},{method:'replaceWithLink',path:'memory',target:'outside'},clear(method)]},
 {name:method+'-snapshot-remove-fail',ops:[{method:'snapshot'},{method:'chmod',path:'last_dream',mode:0o500},clear(method)]},
 {name:method+(method==='clearCurrentWorkspaceMemoryData'?'-restore-settings-fail':'-ignores-restore-trigger'),ops:[{method:'sql',sql:"CREATE TRIGGER fail_restore BEFORE INSERT ON pipeline_state WHEN NEW.state_key='workspaceDir' BEGIN SELECT RAISE(ABORT,'injected restore'); END;"},clear(method)]},
 ]),
 {name:'clear-all-global-fail',ops:[{method:'snapshot'},{method:'chmod',path:'global/UserIdentityNotes',mode:0o500},clear('clearAllMemoryData')]},
 {name:'clear-session-file-before-sql-fail',ops:[{method:'sql',sql:"CREATE TRIGGER fail_l0 BEFORE DELETE ON l0_sessions BEGIN SELECT RAISE(ABORT,'injected l0'); END;"},{method:'clearSessionMemoryData',session:'session-project'},{method:'clearSessionMemoryData',session:'retained-session'}]},
 {name:'two-instances',ops:[{method:'second'},clear('clearCurrentWorkspaceMemoryData'),{method:'upsert',second:true,type:'project',name:'late'},clear('clearAllMemoryData'),{method:'upsert',second:true,type:'user',name:'late-user'},clear('clearSessionMemoryData'),{method:'getSnapshotVersion',second:true}]},
 {name:'general-meta',general:true,ops:[{method:'peer'},{method:'records',select:['peer','peer','project','user']},{method:'records',ids:['external:unknown:Project/missing','external::bad']},{method:'getProjectMeta'},{method:'ensureProjectMeta',input:{}},{method:'editProjectMeta',input:{projectName:'bad',description:'bad',status:'bad'}},{method:'editProjectMeta',input:{projectId:'missing',projectName:'bad',description:'bad',status:'bad'}},{method:'countMemoryEntries'},{method:'clearAllMemoryData'},{method:'getProjectMeta'}]},
 {name:'edit-entry-write-fail',ops:[{method:'chmodEntry',select:['project'],mode:0o400},{...edit(''),select:['project']}]},
 {name:'edit-meta-write-fail',ops:[{method:'chmod',path:'memory/project.meta.md',mode:0o400},{method:'editProjectMeta',input:{projectName:'unwritable',description:'cannot write',status:'done'}}]},
 {name:'write-failure-order',ops:[{method:'chmod',path:'global/UserIdentityNotes',mode:0o500},selected('deleteMemoryEntries'),{method:'chmod',path:'global/UserIdentityNotes',mode:0o700},{method:'chmod',path:'memory/Project',mode:0o500},selected('deprecateMemoryEntries'),selected('restoreMemoryEntries')]},
];
export function writable(root:string){fs.chmodSync(root,0o700);for(const e of fs.readdirSync(root,{withFileTypes:true}))if(e.isDirectory())writable(join(root,e.name));}
export function observe(Repo:any,root:string,c:any){
 const r=open(Repo,root,c.general),ids:any={missing:'missing'};let second:any;
 for(const e of r.listMemoryEntries({includeDeprecated:true,limit:5000}))ids[e.type]=e.relativePath;
 const observations=[];
 for(const op of c.ops){const repo=op.second?second:r;let result:any;
  const selectedIds=op.select?op.select.map((key:string)=>ids[key]??'missing'):op.ids??[];
  try{switch(op.method){
  case 'peer':{const peer=new Repo(join(root,'peer/control.sqlite'),{memoryDir:join(root,'peer/memory'),globalRootDir:join(root,'peer-global'),workspaceDir:'/fixture/external'});peer.setPipelineState('workspaceDir','/fixture/external');const record=peer.getFileMemoryStore().upsertCandidate(candidate('project','external-record'));ids.peer='external:'+createHash('sha1').update('/fixture/external').digest('hex').slice(0,10)+':'+record.relativePath;peer.close();break;}
  case 'records':result=repo.getMemoryRecordsByIds(selectedIds,op.maxLines??80);break;
  case 'editMemoryEntry':result=repo.editMemoryEntry({...op.input,...(op.select?{id:selectedIds[0]}:{})});break;
  case 'deprecateMemoryEntries':case 'restoreMemoryEntries':case 'deleteMemoryEntries':result=repo[op.method](selectedIds);break;
  case 'getProjectMeta':case 'getSnapshotVersion':case 'clearAllMemoryData':case 'clearCurrentWorkspaceMemoryData':result=repo[op.method]();break;
  case 'clearSessionMemoryData':result=repo.clearSessionMemoryData(op.session??'session-project');break;
  case 'countMemoryEntries':case 'ensureProjectMeta':case 'editProjectMeta':case 'archiveTmpEntries':result=repo[op.method](op.input??{});break;
  case 'state':repo.setPipelineState(op.key,op.value);break;
  case 'sql':repo.db.exec(op.sql);break;
  case 'raw':fs.mkdirSync(dirname(join(root,op.path)),{recursive:true});fs.writeFileSync(join(root,op.path),op.content);break;
  case 'chmodEntry':fs.chmodSync(join(root,selectedIds[0].startsWith('global/')?selectedIds[0]:'memory/'+selectedIds[0]),op.mode);break;
  case 'chmod':fs.chmodSync(join(root,op.path),op.mode);break;
  case 'replaceWithLink':fs.rmSync(join(root,op.path),{recursive:true,force:true});fs.symlinkSync(op.target,join(root,op.path));break;
  case 'snapshot':fs.mkdirSync(join(root,'last_dream/workspace'),{recursive:true});fs.writeFileSync(join(root,'last_dream/workspace/saved'),'snapshot');fs.writeFileSync(join(root,'last_dream/metadata.json'),'{}');break;
  case 'second':second=open(Repo,root,c.general);break;
  case 'upsert':result=(op.type==='user'?repo.getGlobalUserStore():repo.getFileMemoryStore()).upsertCandidate(candidate(op.type,op.name));break;
  default:throw Error('unknown method '+op.method);
  }}catch(e:any){result=e.code?{error:e.code,...(e.code==='ERR_SQLITE_ERROR'?{message:e.message}:{})}:{error:e.name,message:e.message};}
  observations.push({result:clean(result,root),disk:disk(root)});
 }
 if(second)second.close();r.close();return observations;
}
