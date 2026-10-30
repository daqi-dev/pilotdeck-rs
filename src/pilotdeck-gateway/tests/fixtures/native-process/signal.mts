import {pathToFileURL} from 'node:url';
import {join} from 'node:path';
const {createShutdownAndExit}=await import(pathToFileURL(join(process.env.PDRS_BASELINE!,'src/cli/shutdownCoordinator.ts')).href);
let release!:()=>void;const gate=new Promise<void>(r=>release=r);
const shutdown=createShutdownAndExit(async()=>{process.stdout.write('stop\n');await gate;},code=>process.exit(code));
for(const signal of ['SIGINT','SIGTERM'])process.on(signal,()=>{process.stdout.write(signal+'\n');void shutdown(0);});
process.stdin.on('data',()=>release());
process.stdout.write('ready\n');
