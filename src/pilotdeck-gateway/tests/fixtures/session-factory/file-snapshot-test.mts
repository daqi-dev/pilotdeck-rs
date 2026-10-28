import fs from 'node:fs';
import promises from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
import {syncBuiltinESMExports} from 'node:module';
import {fileURLToPath} from 'node:url';
import {snapshotFiles} from './file-snapshot.mts';

export async function verifyFileSnapshot(base:string, output:string) {
 const {JsonFileSessionProjectionCheckpointStore}=await import(`${base}/src/session/projection/checkpoint/JsonFileSessionProjectionCheckpointStore.ts`);
 const root=fs.mkdtempSync(path.join(output,'snapshot-contract-'));
 const checkpoint=path.join(root,'session.jsonl.projections.json');
 const transcript=path.join(root,'session.jsonl');
 const envelope={format:'pilotdeck.session-projections',version:1,sessionId:'session-745',asOfSequence:-1,anchor:null,projections:{}};
 const replacement={...envelope,projections:{probe:{version:1,asOfSequence:-1,state:{generation:2}}}};
 const store=new JsonFileSessionProjectionCheckpointStore({path:checkpoint});
 await store.save(envelope);
 fs.writeFileSync(transcript,'transcript\n',{flag:'wx'});
 const rename=promises.rename, readdir=fs.readdirSync;
 let entered!:()=>void, release!:()=>void, temporary='';
 const ready=new Promise<void>(resolve=>{entered=resolve;});
 const released=new Promise<void>(resolve=>{release=resolve;});
 let timer:ReturnType<typeof setTimeout>|undefined;
 let save:Promise<void>|undefined;
 try{
  promises.rename=async(from,to)=>{
   assert.equal(String(to),checkpoint,'#745 checkpoint rename target');
   temporary=String(from);entered();await released;
  };
  syncBuiltinESMExports();
  save=store.save(replacement);
  await Promise.race([ready,save.then(()=>{throw Error('#745 checkpoint rename not reached');}),new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('#745 checkpoint rename hook timeout')),5000);})]);
  clearTimeout(timer);
  const staged=snapshotFiles(root,[checkpoint],true);
  assert(!staged.some(row=>row.path===path.basename(temporary)),'#745 checkpoint staged bytes excluded');
  assert(snapshotFiles(root,[checkpoint],false).some(row=>row.path===path.basename(temporary)),'#745 checkpoint final evidence retains staging');
  let renamed=false;
  fs.readdirSync=((directory:any,...args:any[])=>{
   const names=(readdir as any)(directory,...args);
   if(String(directory)===root&&!renamed){
    assert(names.includes(path.basename(temporary)),'#745 checkpoint stale directory entry present');
    fs.renameSync(temporary,checkpoint);renamed=true;release();
   }
   return names;
  }) as typeof fs.readdirSync;
  let observed:any[]=[];
  assert.doesNotThrow(()=>{observed=snapshotFiles(root,[checkpoint],true);},'#745 checkpoint rename snapshot');
  assert(renamed,'#745 checkpoint rename hook executed');
  assert(!fs.existsSync(temporary),'#745 checkpoint renamed staging absent');
  assert.deepEqual(JSON.parse(fs.readFileSync(checkpoint,'utf8')),replacement,'#745 checkpoint replacement bytes published');
  assert(observed.some(row=>row.path===path.basename(checkpoint)),'#745 checkpoint published file retained');
  fs.readdirSync=readdir;
  await save;
  console.log('PASS: #745 real TS checkpoint save; rename between readdir and lstat');
 }finally{
  clearTimeout(timer);fs.readdirSync=readdir;release();
  // No writer can outlive the hook restoration. The only hook wait is released.
  try{await save;}finally{promises.rename=rename;syncBuiltinESMExports();}
 }
 const uuid='12345678-1234-4234-8234-123456789abc';
 const unrelated=[`${checkpoint}.another.tmp`,`${checkpoint}.${process.pid}.not-a-uuid.tmp`,`${checkpoint}.${process.pid+1}.${uuid}.tmp`,`${path.join(root,'other.json')}.${process.pid}.${uuid}.tmp`];
 for(const file of unrelated)fs.writeFileSync(file,'unrelated\n',{flag:'wx'});
 const rows=snapshotFiles(root,[checkpoint],true);
 for(const file of unrelated)assert(rows.some(row=>row.path===path.basename(file)),'#745 checkpoint unrelated files retained');
 // A vanished persistent file is still invalid evidence; only owned staging is
 // excluded. Inject both metadata and content errors, never swallow ENOENT.
 const lstat=fs.lstatSync, read=fs.readFileSync;
 for(const operation of ['lstat','read'] as const){
  for(const code of ['ENOENT','EACCES']){
   const failure=Object.assign(new Error('injected '+operation+' '+code),{code});
   try{
    if(operation==='lstat')fs.lstatSync=((file:any,...args:any[])=>{if(String(file)===transcript)throw failure;return (lstat as any)(file,...args);}) as typeof fs.lstatSync;
    else fs.readFileSync=((file:any,...args:any[])=>{if(String(file)===transcript)throw failure;return (read as any)(file,...args);}) as typeof fs.readFileSync;
    assert.throws(()=>snapshotFiles(root,[checkpoint],true),error=>error===failure,'#745 checkpoint persistent errors propagate');
   }finally{fs.lstatSync=lstat;fs.readFileSync=read;}
  }
 }
 console.log('PASS: #745 checkpoint scope, final evidence, persistent ENOENT/EACCES');
}

// generate.mts runs this entry point in a process with a 15-second hard limit,
// including initial save and finally/drain; a stuck predecessor cannot outlive it.
if(process.argv[1]===fileURLToPath(import.meta.url)){
 assert.equal(process.version,'v22.22.0');
 await verifyFileSnapshot(path.resolve(process.env.PDRS_BASELINE!),path.resolve(process.argv[2]));
}
