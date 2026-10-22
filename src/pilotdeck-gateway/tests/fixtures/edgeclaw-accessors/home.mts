// Real TS answer for os.homedir()/GENERAL_WORKSPACE_DIR with HOME unset/empty.
import {homedir} from 'node:os';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const baseline=resolve(process.env.PDRS_BASELINE!);
const {GENERAL_WORKSPACE_DIR}=await import(pathToFileURL(join(baseline,'src/context/memory/edgeclaw-memory-core/src/core/general-projects.ts')).href);
console.log(JSON.stringify({home:homedir(),general:GENERAL_WORKSPACE_DIR}));
