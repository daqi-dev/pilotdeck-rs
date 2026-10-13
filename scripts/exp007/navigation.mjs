import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {resolve} from 'node:path';
import {root,evidence} from './env.mjs';
import {createDOM,pause,waitFor} from './dom-env.mjs';
import {installBoundary} from '../../ui-rs/shared/boundary.js';
const checks=[];
let dom;
try {
 let env=await createDOM('/?nav');dom=env.dom;let w=env.w;
 const actions=[];
 const dispose=installBoundary(w,(url,replace)=>actions.push({url,replace}));
 w.eval(await readFile(resolve(root,'_build/baseline-probe.js'),'utf8'));
 await waitFor(()=>w.document.getElementById('navigate'));
 for(const entry of ['navigate','link','navlink']) {
   const before=actions.length;w.document.getElementById(entry).click();await pause(50);
   assert.equal(actions.length,before+1);assert(actions.at(-1).url.endsWith('/probe/leptos/'));assert.equal(actions.at(-1).replace,false);
   checks.push({entry,status:'PASS',actualReactRouter:true,action:actions.at(-1)});
 }
 w.history.pushState({},'','/probex');assert.equal(actions.length,3);
 checks.push({entry:'prefix boundary /probex remains old shell',status:'PASS'});
 dispose();dom.window.close();
 for(const direction of ['back','forward']) {
   env=await createDOM('/old');dom=env.dom;w=env.w;
   w.history.pushState({},'','/probe/leptos/');
   if(direction==='back')w.history.pushState({},'','/old-next');
   else {w.history.back();await waitFor(()=>w.location.pathname==='/old');}
   const traversals=[];installBoundary(w,(url,replace)=>traversals.push({url,replace}));
   w.history[direction]();await waitFor(()=>traversals.length===1);
   assert(traversals[0].url.endsWith('/probe/leptos/'));assert.equal(traversals[0].replace,true);
   checks.push({entry:direction,status:'PASS',nativeJsdomHistory:true,action:traversals[0]});dom.window.close();
 }
 env=await createDOM('/probe/leptos/');dom=env.dom;w=env.w;
 const reverse=[];installBoundary(w,(url,replace)=>reverse.push({url,replace}));
 w.history.pushState({},'','/settings');assert.equal(reverse.length,1);assert(reverse[0].url.endsWith('/settings'));
 checks.push({entry:'new shell -> unmigrated URL',status:'PASS',action:reverse[0]});
 const storage={localStorage:{'auth-token':'f1-probe-token','draft_input_probe:s':'跨壳草稿',themeMode:'dark',userLanguage:'zh'},sessionStorage:{'pilotdeck-active-project':'probe'}};
 for(const [area,values] of Object.entries(storage))for(const [key,value]of Object.entries(values))w[area].setItem(key,value);
 w.history.replaceState({},'','/probe/dioxus/');
 for(const [area,values] of Object.entries(storage))for(const [key,value]of Object.entries(values))assert.equal(w[area].getItem(key),value);
 checks.push({entry:'same-origin history does not alter storage keys',status:'PASS',realCrossDocumentReload:false});
 const result={status:'PASS_DOM_ONLY',checks,limit:'hardNavigate calls recorded, actual browser reload/BFCache/scroll/focus restoration unverified'};
 await writeFile(resolve(evidence,'navigation.json'),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify(result,null,2));
}finally{if(dom)dom.window.close();}
