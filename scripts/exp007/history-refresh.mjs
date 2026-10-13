// A-class transplant sample: use existing manual replay buttons and visible DOM,
// rather than window.streamLifecycle.store. This stays an old fixture experiment.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {readFile,writeFile} from 'node:fs/promises';
import {resolve} from 'node:path';
import {baseline,requireUI,root,evidence} from './env.mjs';
import {createDOM,pause,waitFor} from './dom-env.mjs';
const started=performance.now();
const esbuild=createRequire(requireUI.resolve('vite'))('esbuild');
const outfile=resolve(root,'_build/history-refresh-baseline.js');
const result={scope:'A history refresh transplant sample, baseline manual-reconcile fixture',newCandidates:'NOT_RUN: probe has no production chat/history reconciliation',nativeGeometry:false,stages:[]};
let dom;
try {
 await esbuild.build({stdin:{contents:`import ${JSON.stringify(`${baseline}/e2e/fixtures/streaming-lifecycle.jsx`)};`,loader:'jsx',resolveDir:root},outfile,bundle:true,format:'iife',platform:'browser',jsx:'automatic',nodePaths:[`${baseline}/node_modules`],loader:{'.css':'empty','.svg':'dataurl','.png':'dataurl'},define:{'import.meta.env':JSON.stringify({VITE_IS_PLATFORM:'false',VITE_DISABLE_LOCAL_AUTH:'false',DEV:false}),'process.env.NODE_ENV':'"production"'},logLevel:'warning'});
 const env=await createDOM('/e2e/fixtures/streaming-lifecycle.html?manual-reconcile');dom=env.dom;const w=env.w,d=w.document;
 const mount=d.createElement('div');mount.id='root';d.body.appendChild(mount);
 w.eval(await readFile(outfile,'utf8'));
 await waitFor(()=>[...d.querySelectorAll('button')].some(b=>b.textContent.startsWith('1.')));
 const query='图片与压缩排列回归',thought='压缩前：已确认左边是红色方块，右边是蓝色圆形。',answer='压缩后：继续回答，图片确认完成。';
 const exact=text=>[...d.querySelectorAll('p,span,div')].filter(e=>e.textContent===text&&![...e.children].some(c=>c.textContent===text));
 for(let stage=1;stage<=4;stage++){
  [...d.querySelectorAll('button')].find(b=>b.textContent.startsWith(`${stage}.`)).click();await pause(100);
  await waitFor(()=>exact(query).length===1);assert.equal(d.querySelectorAll('img[src^="data:image/svg"]').length,1);
  const thoughtToggle=[...d.querySelectorAll('button')].find(b=>/Thought process|思考过程/.test(b.textContent));
  if(thoughtToggle?.getAttribute('aria-expanded')==='false'){thoughtToggle.click();await pause(30);}
  if(stage>=2){
   await waitFor(()=>exact(thought).length===1&&exact(answer).length===1);
   const relation=exact(thought)[0].compareDocumentPosition(exact(answer)[0]);assert(relation&w.Node.DOCUMENT_POSITION_FOLLOWING);
  }
  result.stages.push({stage,oneImage:true,oneQuery:true,thoughtBeforeAnswer:stage>=2?true:null,messageRows:[...d.querySelectorAll('[data-message-key]')].map(e=>({key:e.getAttribute('data-message-key'),text:e.textContent.slice(0,180)}))});
 }
 result.domErrors=env.errors;result.status='PASS_DOM_SAMPLE';
} catch(e){result.status='FAIL';result.error=e.stack;if(dom){result.failureDOM={text:dom.window.document.body.textContent,buttons:[...dom.window.document.querySelectorAll('button')].map(b=>({text:b.textContent,expanded:b.getAttribute('aria-expanded')}))};}process.exitCode=1;}
finally{if(dom)dom.window.close();result.elapsedMs=performance.now()-started;await writeFile(resolve(evidence,'history-refresh.json'),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify(result,null,2));process.exit(process.exitCode||0);}
