// #1082: same rootDir, two EdgeClaw generations against the real cc7c4e6 code.
//
// `serial` is what the TS baseline actually does in production: one Node event
// loop, so every `DatabaseSync` call of every generation is a synchronous block
// and no two calls overlap. `threaded` is the deterministic control: the same
// `node:sqlite` primitive with two OS threads really overlapping (worker holds
// the write lock, main runs the operation). It is NOT production TS; it records
// what `busy_timeout = 0` does when overlap is forced, which is exactly the
// situation Rust workers can create and Node cannot.
import {mkdirSync,mkdtempSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {Worker} from 'node:worker_threads';
import {lifecycle} from './lifecycle.mts';
import {load} from '../edgeclaw-native-default/observe.mts';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';

const CORE='src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';

const errorShape=(e:any)=>({name:e?.name??null,message:e?.message??String(e),...(e?.code?{code:e.code}:{})});
const ok=(fn:()=>any)=>{try{return {ok:true,value:encode(fn())}}catch(e){return {ok:false,error:errorShape(e)}}};
const encode=(value:any):any=>value===undefined?{$undefined:true}:value;

const INIT=`PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS l0_sessions (
  l0_index_id TEXT PRIMARY KEY,
  session_key TEXT NOT NULL,
  timestamp TEXT NOT NULL,
  messages_json TEXT NOT NULL,
  source TEXT NOT NULL,
  indexed INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_l0_sessions_session ON l0_sessions(session_key);
CREATE INDEX IF NOT EXISTS idx_l0_sessions_pending ON l0_sessions(indexed, timestamp);
CREATE TABLE IF NOT EXISTS pipeline_state (
  state_key TEXT PRIMARY KEY,
  state_json TEXT NOT NULL,
  updated_at TEXT NOT NULL
);`;

// A legacy (pre-v2) pipeline_state: a real older install has every other table
// already, so only the migration needs the write lock.
const LEGACY_INIT=`PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS l0_sessions (
  l0_index_id TEXT PRIMARY KEY,
  session_key TEXT NOT NULL,
  timestamp TEXT NOT NULL,
  messages_json TEXT NOT NULL,
  source TEXT NOT NULL,
  indexed INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_l0_sessions_session ON l0_sessions(session_key);
CREATE INDEX IF NOT EXISTS idx_l0_sessions_pending ON l0_sessions(indexed, timestamp);
CREATE TABLE pipeline_state (
  state_key TEXT PRIMARY KEY,
  state_json TEXT,
  state_value TEXT,
  updated_at TEXT NOT NULL
);
INSERT INTO pipeline_state (state_key, state_json, state_value, updated_at)
VALUES ('workspaceDir', NULL, '"legacy"', '2026-10-22T00:00:00.000Z');`;

// Keep one writer transaction open until the parent stores 1 in slot 1.
const HOLD=`const {workerData}=require('node:worker_threads');
const {DatabaseSync}=require('node:sqlite');
const sab=new Int32Array(workerData.sab);
const db=new DatabaseSync(workerData.path);
if(workerData.legacy){
  db.exec('PRAGMA journal_mode = WAL;');
  db.exec('BEGIN IMMEDIATE;');
  db.exec("INSERT INTO pipeline_state (state_key,state_value,updated_at) VALUES ('worker','\\"held\\"','2026-10-22T00:00:00.000Z')");
}else{
  db.exec(workerData.init);
  db.exec('BEGIN IMMEDIATE;');
  db.exec("INSERT INTO pipeline_state (state_key,state_json,updated_at) VALUES ('worker','{\\"w\\":1}','2026-10-22T00:00:00.000Z')");
}
Atomics.store(sab,0,1);Atomics.notify(sab,0);
const deadline=Date.now()+5000;
while(Atomics.load(sab,1)===0){if(Date.now()>deadline)throw new Error('#1082 worker release timeout');
  Atomics.wait(sab,1,0,50);}
db.exec('ROLLBACK;');
db.close();`;

async function hold(path:string,legacy:boolean,init:string){
  const sab=new SharedArrayBuffer(8);
  const worker=new Worker(HOLD,{eval:true,workerData:{sab,path,legacy,init}});
  const view=new Int32Array(sab);
  let failure:any=null;
  worker.once('error',(error)=>{failure=error});
  const deadline=Date.now()+5000;
  while(Atomics.load(view,0)!==1){
    if(failure)throw failure;
    if(Date.now()>deadline)throw new Error('#1082 worker never held the lock');
    Atomics.wait(view,0,0,10);
  }
  return {release:async()=>{
    Atomics.store(view,1,1);Atomics.notify(view,1);
    await new Promise<void>((resolve,reject)=>{
      worker.once('exit',()=>resolve());
      worker.once('error',reject);
    });
  }};
}

// #1082 fix-r2: use the real repository's synchronous clock boundary.
// No SQL method is mocked. The hook runs inside insertL0Session before its SQL
// statement; closing a peer or throwing cannot strand a process-wide lock.
function closeCases(Repository:any, base:string){
  const results:any={};
  for(const mode of ['normal','panic','other-root']){
    const path=join(base,mode,'memory.sqlite');
    const a=new Repository(path);
    const b=new Repository(mode==='other-root'?join(base,mode,'peer','memory.sqlite'):path);
    const row=(id:string)=>({l0IndexId:id,sessionKey:'callback',timestamp:'2026-10-22T00:00:00.000Z',messages:[],source:'openclaw',indexed:false,createdAt:'2026-10-22T00:00:00.000Z'});
    a.insertL0Session(row('seed'));
    let entered=0;
    let threw=false;
    const original=Date.prototype.toISOString;
    try{
        Date.prototype.toISOString=function(){
          Date.prototype.toISOString=original;
          entered++; b.close();
          if(mode==='panic')throw new Error('#1082 intentional callback unwind');
          return original.call(this);
        };
        a.insertL0Session({...row('next'),createdAt:''});
    }catch(error){
      if(!(error instanceof Error)||error.message!=='#1082 intentional callback unwind')throw error;
      threw=true;
    }finally{Date.prototype.toISOString=original;}
    const c=new Repository(path);
    c.setPipelineState('after-callback','ok');
    results[mode]={entered,threw,rows:c.listRecentL0(10).map((r:any)=>r.l0IndexId).sort(),after:c.getPipelineState('after-callback')};
    c.close();a.close();
  }
  return results;
}

export async function matrix(baseline:string,created:string){
  const {MemoryRepository}=await import(pathToFileURL(join(baseline,CORE)).href);
  const {Service}=await load(baseline);
  const base=created;
  const dbPath=join(base,'store','memory.sqlite');
  const workspace=join(base,'workspace');
  mkdirSync(workspace,{recursive:true});
  const open=(path=dbPath)=>new MemoryRepository(path,{workspaceDir:workspace});

  // Serial cells: the only interleaving TS production can have is between
  // synchronous calls, so these are the expected same-root outcomes.
  const serial:any={};
  serial['old-stage']=ok(()=>{const a=open();a.close();return true});
  const a=open();
  let b:any;
  serial['two-generations-open']=ok(()=>{b=open();return true});
  a.setPipelineState('old-key',{from:'old'});
  serial['new-reads-old-write']=ok(()=>b.getPipelineState('old-key'));
  b.setPipelineState('new-key',{from:'new'});
  serial['old-reads-new-write']=ok(()=>a.getPipelineState('new-key'));
  serial['old-close']=ok(()=>{a.close();return true});
  serial['new-write-after-old-close']=ok(()=>{b.setPipelineState('after-close',{ok:true});return true});
  serial['new-close']=ok(()=>{b.close();return true});
  const c=open();
  serial['reopen-reads-all']=ok(()=>[c.getPipelineState('old-key'),c.getPipelineState('new-key'),c.getPipelineState('after-close')]);
  c.close();

  // Threaded control on an already-current database: a real overlap. The stage
  // constructor still succeeds (WAL/no-op DDL need no write lock); the first
  // write gets Node's immediate busy error; the WAL read still succeeds.
  const threaded:any={};
  const normal=await hold(dbPath,false,INIT);
  let d:any;
  threaded['repository-open-while-write']=ok(()=>{d=open();return true});
  threaded['service-stage-while-write']=ok(()=>{const service=new Service({workspaceDir:workspace,rootDir:join(base,'service'),dbPath});service.close();return true});
  threaded['write-while-worker-holds-lock']=ok(()=>{d.setPipelineState('main',{m:1});return true});
  threaded['read-while-worker-holds-lock']=ok(()=>d.getPipelineState('worker'));
  await normal.release();
  threaded['write-after-worker-release']=ok(()=>{d.setPipelineState('main-after',{m:2});return true});
  threaded['read-persisted']=ok(()=>d.getPipelineState('main-after'));
  d.close();

  // Threaded control on a legacy schema: the new generation's constructor must
  // migrate, so the stage itself needs the write lock and fails immediately.
  const legacyPath=join(base,'legacy','memory.sqlite');
  mkdirSync(join(base,'legacy'),{recursive:true});
  const {DatabaseSync}=await import('node:sqlite');
  const raw=new DatabaseSync(legacyPath);
  raw.exec(LEGACY_INIT);
  raw.close();
  const legacy=await hold(legacyPath,true,LEGACY_INIT);
  threaded['stage-legacy-while-write']=ok(()=>{const e=open(legacyPath);e.close();return true});
  await legacy.release();
  threaded['stage-legacy-after-release']=ok(()=>{const e=open(legacyPath);const value=e.getPipelineState('workspaceDir');e.close();return value});

  const sourceCommit=assertBaseline(baseline);
  const sourceSha256=await baselineSourceHashes(baseline,[CORE,'src/cli/ProjectRuntimeRegistry.ts','src/cli/ProjectMemoryBundle.ts','src/context/memory/createEdgeClawMemoryProviderFromConfig.ts','src/context/memory/edgeclaw-memory-core/src/service.ts']);
  return {node:process.version,sourceCommit,sourceSha256,serial,threaded,closeCallbacks:closeCases(MemoryRepository,join(base,'close-callbacks')),lifecycle:await lifecycle(baseline,join(base,'lifecycle'))};
}
