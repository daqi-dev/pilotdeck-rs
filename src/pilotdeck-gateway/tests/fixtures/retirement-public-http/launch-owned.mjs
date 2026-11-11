// Shared owned-observer records this real child and this process's actual HTTP driver.
import {readFileSync,writeFileSync,renameSync,existsSync} from 'node:fs';
import {spawn} from 'node:child_process';
import {pathToFileURL} from 'node:url';
const spec=JSON.parse(readFileSync(process.argv[2],'utf8'));
const child=spawn(spec.command[0],spec.command.slice(1),{cwd:process.cwd(),env:process.env,stdio:['ignore','inherit','inherit']});
let closed=false;const stopped=new Promise(resolve=>child.once('close',(code,signal)=>{closed=true;resolve({code,signal});}));
child.once('error',error=>{console.error(error);process.exitCode=1;});
for(const sig of ['SIGINT','SIGTERM','SIGHUP'])process.on(sig,()=>{if(!closed)child.kill(sig);});
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
while(!existsSync(spec.startFile)&&!closed)await sleep(20);
if(!closed){
 let result={code:0};
 try{process.argv=[process.execPath,spec.driver,...spec.driverArgv];await import(pathToFileURL(spec.driver).href);}catch(error){result={code:1,error:String(error.stack??error)};}
 await sleep(100); // Drain cloned HTTP response observations before acknowledging the driver.
 const tmp=spec.doneFile+'.new';writeFileSync(tmp,JSON.stringify(result)+'\n',{flag:'wx'});renameSync(tmp,spec.doneFile);
}
const exit=await stopped;process.exitCode=exit.code??1;
