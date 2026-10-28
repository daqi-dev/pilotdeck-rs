import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';

// JsonFileSessionProjectionCheckpointStore publishes these files by rename.
// Only this probe's known checkpoint paths and PID may be excluded, and only
// while writes are allowed. Final evidence still includes leftover staging.
export function snapshotFiles(root:string, checkpoints:string[], writing:boolean) {
 const temporary=(file:string)=>checkpoints.some(checkpoint=>{
  const prefix=`${checkpoint}.${process.pid}.`;
  return file.startsWith(prefix) && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\.tmp$/.test(file.slice(prefix.length));
 });
 const out:any[]=[];
 const walk=(directory:string)=>{
  for(const name of fs.readdirSync(directory).sort()){
   const file=path.join(directory,name);
   if(writing && temporary(file))continue;
   const stat=fs.lstatSync(file);
   if(stat.isDirectory()){
    out.push({path:path.relative(root,file),kind:'directory'});
    walk(file);
   }else{
    const bytes=fs.readFileSync(file);
    out.push({path:path.relative(root,file),kind:'file',utf8:bytes.toString('utf8'),base64:bytes.toString('base64'),sha256:crypto.createHash('sha256').update(bytes).digest('hex')});
   }
  }
 };
 walk(root);
 return out;
}
