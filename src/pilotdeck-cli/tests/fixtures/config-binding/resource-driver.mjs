import {pathToFileURL} from 'node:url';
import path from 'node:path';
const [baseline,home,project]=process.argv.slice(2);
process.env.HOME=home;process.env.PILOT_HOME=home;
globalThis.fetch=()=>{throw Error('network forbidden');};
const {loadPilotConfig}=await import(pathToFileURL(path.join(baseline,'src/pilot/config/loadPilotConfig.ts')));
console.log('LOAD_BEGIN');
try {loadPilotConfig({env:{HOME:home,PILOT_HOME:home,STAFFDECK_PROJECT_BINDING_ENABLED:'true'},projectRoot:project});console.log('LOAD_OK');}
catch(error){console.error(error.message);process.exitCode=1;}
