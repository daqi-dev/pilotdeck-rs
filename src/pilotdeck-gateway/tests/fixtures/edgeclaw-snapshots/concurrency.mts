// Controlled cross-process overlap: both stages exist; process one pauses
// after backing up workspace, process two finishes its swap, then one resumes.
import fs from 'node:fs';
import {syncBuiltinESMExports} from 'node:module';
import {spawn} from 'node:child_process';
import {join} from 'node:path';
import {pathToFileURL,fileURLToPath} from 'node:url';
import {source,freezeClock,open,seed,candidate,disk,clean} from './observe.mts';
const sleeper=new Int32Array(new SharedArrayBuffer(4));
function waitSync(path:string){const start=performance.now();while(!fs.existsSync(path)){if(performance.now()-start>20000)throw Error('barrier timed out '+path);Atomics.wait(sleeper,0,0,5);}}
async function wait(path:string){const start=performance.now();while(!fs.existsSync(path)){if(performance.now()-start>20000)throw Error('barrier timed out '+path);await new Promise(r=>setTimeout(r,5));}}
if(process.argv[2]==='worker') {
 const root=process.argv[3]!,id=process.argv[4]!,baseline=process.env.PDRS_BASELINE!;process.env.HOME=join(root,'home');freezeClock();const {MemoryRepository}=await import(pathToFileURL(join(baseline,source)).href);const r=open(MemoryRepository,root);const signals=join(root,'signals');const signal=(s:string)=>fs.writeFileSync(join(signals,s),'ready');
 signal('boot-'+id);waitSync(join(signals,'stage-start-'+id));
 const mkdir=fs.mkdirSync,rename=fs.renameSync;let stagePause=false;
 fs.mkdirSync=((path:any,...args:any[])=>{if(!stagePause&&String(path).includes('.memory.'+id+'-')){stagePause=true;signal('stage-paused-'+id);waitSync(join(signals,'stage-release'));}return (mkdir as any)(path,...args);})as any;
 fs.renameSync=((src:any,dst:any)=>{if(id==='one'&&String(src).includes('.memory.one-')){signal('swap-paused');waitSync(join(signals,'swap-release'));}return rename(src,dst);})as any;syncBuiltinESMExports();
 const fallback=r.captureCurrentMemorySnapshot();const stage=r.createDreamStage(id);stage.repository.getFileMemoryStore().upsertCandidate(candidate(id==='one'?'project':'feedback',id));stage.repository.close();signal('ready-'+id);waitSync(join(signals,'swap-start-'+id));let result:any=null;try{r.replaceLiveRootsWithStage(stage,fallback);}catch(e:any){result={error:e.code??e.name};}stage.dispose();r.close();fs.writeFileSync(join(signals,'result-'+id),JSON.stringify(result));
} 
export async function runConcurrent(Repo:any,baseline:string,root:string){
 fs.mkdirSync(root,{recursive:true});fs.mkdirSync(join(root,'signals'));const r=open(Repo,root);seed(r);r.close();const signals=join(root,'signals');const signal=(s:string)=>fs.writeFileSync(join(signals,s),'go');
 const children:any[]=[];const done:Promise<any>[]=[];
 try{
 for(const id of ['one','two']){const child=spawn(process.execPath,['--import',join(baseline,'node_modules/tsx/dist/loader.mjs'),fileURLToPath(import.meta.url),'worker',root,id],{env:process.env,stdio:['ignore','pipe','pipe']});children.push(child);let err='';child.stderr.on('data',(b:any)=>err+=b);done.push(new Promise((resolve,reject)=>{child.on('error',reject);child.on('close',(code:any)=>code===0?resolve(null):reject(Error(`worker ${id} exit ${code}: ${err}`)));}));await wait(join(signals,'boot-'+id));}
 signal('stage-start-one');await wait(join(signals,'stage-paused-one'));signal('stage-start-two');await wait(join(signals,'stage-paused-two'));signal('stage-release');await wait(join(signals,'ready-one'));await wait(join(signals,'ready-two'));signal('swap-start-one');await wait(join(signals,'swap-paused'));signal('swap-start-two');await wait(join(signals,'result-two'));signal('swap-release');await wait(join(signals,'result-one'));await Promise.all(done);
 const r=open(Repo,root);const snapshot=clean(r.captureCurrentMemorySnapshot(),root);r.close();return {results:['one','two'].map(id=>JSON.parse(fs.readFileSync(join(signals,'result-'+id),'utf8'))),snapshot,disk:disk(root).filter(e=>!e.relativePath.startsWith('signals'))};
 }finally{for(const child of children)if(child.exitCode===null)child.kill();}
}
