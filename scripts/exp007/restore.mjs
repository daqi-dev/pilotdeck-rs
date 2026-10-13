// Verify Rust bootstrap restores existing shared keys. No simulated browser reload claim.
import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {root,evidence} from './env.mjs';
import {createDOM,waitFor} from './dom-env.mjs';
import {startMock} from './mock.mjs';
const candidate=process.argv[2];if(!['leptos','dioxus'].includes(candidate))throw new Error('candidate required');
const mock=await startMock();let dom;let result={candidate,status:'FAIL'};
try{
 const env=await createDOM(`/probe/${candidate}/`);dom=env.dom;const w=env.w;
 w.localStorage.setItem('auth-token','f1-probe-token');w.localStorage.setItem('draft_input_probe:s','跨壳尚未发送草稿');w.sessionStorage.setItem('pilotdeck-active-project','probe');
 const app=await import(pathToFileURL(resolve(root,`_build/dist-${candidate}/probe.js`)));await app.default({module_or_path:await readFile(resolve(root,`_build/dist-${candidate}/probe_bg.wasm`))});
 await waitFor(()=>w.document.getElementById('composer')?.value==='跨壳尚未发送草稿');await waitFor(()=>w.document.getElementById('sessions')?.textContent.includes('F1 session'));
 assert.equal(w.sessionStorage.getItem('pilotdeck-active-project'),'probe');assert.equal(w.localStorage.getItem('auth-token'),'f1-probe-token');
 result={candidate,status:'PASS_BOOTSTRAP_DOM_ONLY',tokenRetained:true,draftRestored:true,activeProjectRetained:true,browserCrossDocumentReload:false};

}catch(error){result.error=error.stack;process.exitCode=1;}finally{if(dom)dom.window.close();await mock.close();await writeFile(resolve(evidence,`${candidate}-restore.json`),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify(result,null,2));process.exit(process.exitCode||0);}
