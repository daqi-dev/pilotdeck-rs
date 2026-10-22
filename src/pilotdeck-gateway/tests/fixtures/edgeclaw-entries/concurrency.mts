// Separate Node processes: clear one pauses after SQL and before root removal;
// clear two and a writer run to completion, then clear one resumes.
import fs from 'node:fs';
import {syncBuiltinESMExports} from 'node:module';
import {spawn} from 'node:child_process';
import {join} from 'node:path';
import {pathToFileURL,fileURLToPath} from 'node:url';
import {source,freezeClock,open,seed,candidate,disk,clean} from './observe.mts';
const sleeper=new Int32Array(new SharedArrayBuffer(4));
function syncWait(p:string){const start=performance.now();while(!fs.existsSync(p)){if(performance.now()-start>20000)throw Error('timeout '+p);Atomics.wait(sleeper,0,0,5);}}
async function wait(p:string){const start=performance.now();while(!fs.existsSync(p)){if(performance.now()-start>20000)throw Error('timeout '+p);await new Promise(r=>setTimeout(r,5));}}
if(process.argv[2]==='worker'){
 const root=process.argv[3]!,id=process.argv[4]!;process.env.HOME=join(root,'home');freezeClock();const {MemoryRepository}=await import(pathToFileURL(join(process.env.PDRS_BASELINE!,source)).href);const r=open(MemoryRepository,root),signals=join(root,'signals');
 fs.writeFileSync(join(signals,'boot-'+id),'go');syncWait(join(signals,'start-'+id));
 let result:any;
 if(id==='one') {const rm=fs.rmSync;fs.rmSync=((p:any,...args:any[])=>{if(String(p)===join(root,'memory')){fs.writeFileSync(join(signals,'paused'),'go');syncWait(join(signals,'release'));}return (rm as any)(p,...args);}) as any;syncBuiltinESMExports();result=r.clearAllMemoryData();}
 else {r.getFileMemoryStore().upsertCandidate(candidate('project','overlap-before'));const cleared=r.clearCurrentWorkspaceMemoryData();const written=r.getFileMemoryStore().upsertCandidate(candidate('feedback','overlap-after'));result={cleared,written};}
 r.close();fs.writeFileSync(join(signals,'result-'+id),JSON.stringify(clean(result,root)));
}
export async function runConcurrent(Repo:any,baseline:string,root:string){
 fs.mkdirSync(root,{recursive:true});const r=open(Repo,root);seed(r);r.close();fs.mkdirSync(join(root,'signals'));const signals=join(root,'signals'),signal=(s:string)=>fs.writeFileSync(join(signals,s),'go');
 const children:any[]=[],done:Promise<any>[]=[];
 try{for(const id of ['one','two']){const child=spawn(process.execPath,['--import',join(baseline,'node_modules/tsx/dist/loader.mjs'),fileURLToPath(import.meta.url),'worker',root,id],{env:process.env,stdio:['ignore','ignore','pipe']});children.push(child);let stderr='';child.stderr.on('data',(b:any)=>stderr+=b);done.push(new Promise((resolve,reject)=>{child.on('error',reject);child.on('close',(code:any)=>code===0?resolve(null):reject(Error(`worker ${id}: ${code} ${stderr}`)));}));await wait(join(signals,'boot-'+id));}
 signal('start-one');await wait(join(signals,'paused'));signal('start-two');await wait(join(signals,'result-two'));signal('release');await wait(join(signals,'result-one'));await Promise.all(done);
 return {results:['one','two'].map(id=>JSON.parse(fs.readFileSync(join(signals,'result-'+id),'utf8'))),disk:disk(root).filter((e:any)=>!e.relativePath.startsWith('signals'))};
 }finally{for(const c of children)if(c.exitCode===null)c.kill();}
}
