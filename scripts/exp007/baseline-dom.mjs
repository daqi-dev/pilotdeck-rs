import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {resolve} from 'node:path';
import {createDOM,pause,waitFor} from './dom-env.mjs';
import {startMock} from './mock.mjs';
import {root,evidence,origin} from './env.mjs';
const result={kind:'Baseline original LoginForm/AuthProvider/WebSocketProvider/api modules, isolated DOM fixture',fullProduct:false,checks:[]};
let mock,dom;
try {
 mock=await startMock();const env=await createDOM('/');dom=env.dom;const w=env.w,d=w.document;
 w.eval(await readFile(resolve(root,'_build/baseline-probe.js'),'utf8'));
 await waitFor(()=>d.getElementById('baseline-status')?.textContent==='ready');
 const fill=(id,value)=>{Object.getOwnPropertyDescriptor(w.HTMLInputElement.prototype,'value').set.call(d.getElementById(id),value);d.getElementById(id).dispatchEvent(new w.Event('input',{bubbles:true}));};
 fill('username','probe');fill('password','probe-secret');await pause(30);
 d.querySelector('form').dispatchEvent(new w.Event('submit',{bubbles:true,cancelable:true}));
 await waitFor(()=>w.localStorage.getItem('auth-token')==='f1-probe-token');
 await waitFor(()=>d.getElementById('sessions')?.textContent.includes('F1 session'));
 await waitFor(()=>mock.stats.outbound.some(m=>m.type==='watch-session'));
 for(const path of ['/api/projects','/api/projects/probe/sessions'])assert(mock.stats.requests.some(r=>r.path===path&&r.authorization==='Bearer f1-probe-token'));
 result.checks.push({name:'S2 login/token/authenticated project and session requests',status:'PASS'});
 await fetch(origin+'/__probe/projects');await waitFor(()=>d.getElementById('sessions').textContent.includes('Probe 2'));
 result.checks.push({name:'S2 projects_updated refresh',status:'PASS'});
 const before=mock.stats.connections.filter(c=>c.path==='/ws').length;
 await fetch(origin+'/__probe/close?channel=/ws');const start=performance.now();
 await waitFor(()=>mock.stats.connections.filter(c=>c.path==='/ws').length>before,4000);
 result.checks.push({name:'D1 original main WS automatically reconnects',status:'PASS',delayMs:performance.now()-start});
 result.requests=mock.stats.requests;result.outbound=mock.stats.outbound;result.domErrors=env.errors;result.status='PASS_DOM_ONLY';
} catch(e){result.status='FAIL';result.error=e.stack;process.exitCode=1;}
finally {if(dom)dom.window.close();if(mock)await mock.close();await writeFile(resolve(evidence,'baseline-dom.json'),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify(result,null,2));process.exit(process.exitCode||0);}
