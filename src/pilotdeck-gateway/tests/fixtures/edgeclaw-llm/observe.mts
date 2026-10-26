import assert from 'node:assert/strict';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {stub} from './stub.mts';
export const ENTRY = 'src/context/memory/edgeclaw-memory-core/src/core/skills/llm-extraction.ts';
const tick = () => new Promise<void>(resolve => setImmediate(resolve));
const realSet = globalThis.setTimeout, realClear = globalThis.clearTimeout;
const ok = (api: string, text = '  {"ok":"中😀"}  ') => ({status: 200, body: JSON.stringify(
  api === 'responses' || api === 'openai-responses' ? {output_text: text} :
  api === 'anthropic' ? {content: [{text}]} :
  api.trim().toLowerCase() === 'google' || api === 'gemini' ? {candidates: [{content: {parts: [{text}]}}]} :
  {choices: [{message: {content: text}}]},
)});
function specs() {
  const out: any[] = [];
  for (const api of ['chat', 'responses']) for (const attempt of [0, 1, 2]) {
    const prefix = Array.from({length: attempt}, () => ({status: 503, body: 'prefix'}));
    const events = [ok(api), ...[408,409,425,429,500,502,503,504,400,401,404,501].map(status => ({status, body: 'bad'})),
      {status: 200, body: '{bad'}, {kind: 'disconnect'},
      ...['fetch failed','NETWORK','ECONNRESET','econnrefused','ETIMEDOUT','socket hang up','temporary','rate limit','too many requests','timeout','boom','timed out'].map(message => ({kind: 'error', message})),
      {kind: 'headers-hang'}];
    for (const [i, event] of events.entries()) {
      out.push({name: `${api}-attempt-${attempt}-event-${i}`, api, steps: [...prefix, event], mode: event.kind === 'headers-hang' ? 'timeout' : 'normal'});
    }
  }
  for (const api of ['chat','responses','openai-responses','anthropic','google','gemini','  GOOGLE  ','unknown']) {
    out.push({name: `protocol-${api}`, api, steps: [ok(api)]});
    out.push({name: `headers-${api}`, api, headers: {'Authorization':'first', 'authorization':'second', 'Content-Type':' text/custom ', 'X-Api-Key':'custom', 'Anthropic-Version':'v-test', 'X-Goog-Api-Key':'custom-google'}, steps: [ok(api)]});
  }
  for (const model of ['google/gemini-3-pro','gemini-3.1-pro','gemini-3-flash','gemini-3.1-flash','gemini-3.1-flash-preview','gemini-3.1-flash-lite','a /中']) {
    out.push({name: `google-model-${model}`, api:'google', model, path:'/prefix/v1?lost=1#fragment', steps:[ok('google')]});
  }
  for (const timeout of ['absent','NaN','Infinity','-Infinity',0,-1,7,0.5,2147483648]) for (const api of ['chat','responses']) {
    out.push({name:`timeout-${api}-${timeout}`, api, timeout, mode: typeof timeout === 'number' && timeout <= 0 ? 'no-timeout' : 'timeout', steps:[{kind:'headers-hang'}]});
  }
  for (const api of ['chat','responses']) for (const status of [200,503]) out.push({name:`body-hang-${api}-${status}`, api, timeout:7, mode:'body-hang', steps:[{kind:'body-hang',status}]});
  for (const field of ['baseUrl','apiKey']) out.push({name:`missing-${field}`, api:'chat', missing:field, steps:[ok('chat')]});
  for (const auth of ['success','reject','pending']) out.push({name:`auth-${auth}`, api:'chat', missing:'apiKey', auth, mode:auth === 'pending' ? 'auth-pending' : 'normal', steps:[ok('chat')]});
  out.push({name:'backoff-held', api:'responses', mode:'backoff-held', steps:[{status:503,body:'bad'},ok('responses')]});
  out.push({name:'truncate-error',api:'responses',steps:[{status:400,body:'x'.repeat(299)+'😀 trailing'}]});
  const payloads: Record<string, any[]> = {
    chat:[null,{}, {choices:[{message:[]}]}, {choices:[]},{choices:[{}]},{choices:[{message:{}}]}, {choices:[{message:{content:[]}}]}, {choices:[{message:{content:[{text:' a '},{text:''},null,{text:'b'}, {text:1}]}}]}],
    responses:[null,{}, [], {output:[]},{output_text:'  ',output:[{content:[{text:'a'}, {text:''},null,{text:'b'}]}]}, {output_text:' raw '}],
    anthropic:[null,{}, {content:[]},{content:[{text:' a '},{text:''},null,{text:'b'}]}],
    google:[null,{}, {candidates:[]},{candidates:[{}, {content:{parts:[{text:' a '},{text:''},null,{text:'b'}]}}]}],
  };
  for (const [api, values] of Object.entries(payloads)) for (const [i,payload] of values.entries()) out.push({name:`extract-${api}-${i}`,api,steps:[{status:200,body:JSON.stringify(payload)}]});
  for (const [i,body] of ['','\ufeff'+ok('chat').body,'not json'].entries()) out.push({name:`json-${i}`,api:'chat',steps:[{status:200,body}]});
  for (const parse of ['success','reject']) out.push({name:`debug-${parse}`,api:'chat',parse,steps:[ok('chat','{"answer":42}')]});
  for (const [name,headers] of Object.entries({latin:{'x-text':'é'},wide:{'x-text':'中'},badname:{'bad name':'x'},badvalue:{'x-text':'a\nb'}})) out.push({name:`header-boundary-${name}`,api:'responses',headers,steps:[ok('responses')]});
  for (const api of ['chat','responses','google']) for(const baseUrl of ['not a url','http://user:pass@127.0.0.1:12345']) out.push({name:`invalid-url-${api}-${baseUrl}`,api,baseUrl,steps:[ok(api)]});
  for(const label of ['retrieval','DREAM','rewrite','capture','extract','index']) out.push({name:`telemetry-phase-${label}`,api:'responses',label,steps:[ok('responses')]});
  for(const delay of [5,7,8]) out.push({name:`timer-order-${delay}`,api:'responses',timeout:7,mode:'timer-order',steps:[{...ok('responses'),kind:'timer-response',delay}]});
  for(const api of ['chat','anthropic','google']) out.push({name:`key-surrogate-${api}`,api,apiKeyUnits:[97,55296],steps:[ok(api)]});
  out.push({name:'google-surrogate-model',api:'google',modelUnits:[55296],steps:[ok('google')]});
  return out;
}
function normalized(value: any, origin: string): any {
  if (typeof value === 'string') return value.split(origin).join('$ORIGIN');
  if (Array.isArray(value)) return value.map(v => normalized(v,origin));
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalized(v,origin)]));
  return value;
}
export async function collect(baseline: string) {
  assert.equal(process.version,'v22.22.0');
  const home = mkdtempSync(join(tmpdir(),'pdrs-llm-home-'));
  const oldHome = process.env.HOME; process.env.HOME = home;
  const oldDateNow = Date.now, oldRandom = Math.random;
  Date.now = () => 1792972800000; Math.random = () => 0.25;
  const {LlmMemoryExtractor} = await import(pathToFileURL(join(baseline,ENTRY)).href);
  const cases: any[] = [];
  try { for (const spec of specs()) {
    let now = 0, calls = 0, settled = false, headerSeen = false;
    const sleeps: number[] = [], timers: any[] = [], events: any[] = [], debug: any[] = [], fetches: any[] = [];
    const active = new Map<any,()=>void>(); let held: (()=>void)|undefined, authRelease: (()=>void)|undefined;
    const server = await stub(spec.steps.filter((s:any)=>s.kind!=='error'),()=>now);
    const realFetch = globalThis.fetch;
    const timeout = typeof spec.timeout === 'string' ? ({NaN:NaN,Infinity:Infinity,'-Infinity':-Infinity} as any)[spec.timeout] : spec.timeout;
    const input = {systemPrompt:'system 中\n"quoted"',userPrompt:'user 😀',requestLabel:spec.label??'recall fixture',...(timeout === undefined ? {} : {timeoutMs:timeout})};
    const model=spec.modelUnits?String.fromCharCode(...spec.modelUnits):(spec.model??'m');
    const config = {agents:{defaults:{model:{primary:`p/${model}`}}},models:{providers:{p:{api:spec.api,baseUrl:spec.baseUrl??server.origin+(spec.path??'/v1'),apiKey:'fixture-key',headers:spec.headers,models:[{id:model}]}}}};
    if(spec.apiKeyUnits) config.models.providers.p.apiKey=String.fromCharCode(...spec.apiKeyUnits);
    if(spec.missing) delete (config.models.providers.p as any)[spec.missing];
    (globalThis as any).setTimeout = (fn:any, ms:number, ...args:any[]) => {
      if (!(new Error().stack??'').includes('/llm-extraction.ts:')) return realSet(fn,ms,...args);
      const token = {}; const stack = new Error().stack??'';
      if(stack.includes('executeOnce')) {
        const record = {ms,cleared:false,fired:false}; timers.push(record);
        active.set(token,()=>{record.fired=true; now+=ms; fn(...args);});
        (token as any).record=record;
      } else {
        sleeps.push(ms);
        const fire=()=>{now+=ms; fn(...args);};
        if(spec.mode==='backoff-held' && !held) held=fire; else queueMicrotask(fire);
      }
      return token;
    };
    (globalThis as any).clearTimeout = (token:any) => {
      if(token?.record) {token.record.cleared=true; active.delete(token);} else realClear(token);
    };
    globalThis.fetch = async (url:any, init:any) => {
      const n=calls++; fetches.push({method:init.method,url:String(url).replace(server.origin,'$ORIGIN'),headers:Object.fromEntries(init.headers.entries()),body:init.body,at:now});
      const step=spec.steps[n]??spec.steps.at(-1);
      if(step.kind==='error') throw new TypeError(step.message);
      if(step.kind==='timer-response') return new Promise<Response>((resolve,reject)=>{
        init.signal.addEventListener('abort',()=>reject(new DOMException('This operation was aborted','AbortError')),{once:true});
        // Deadline was registered first. Dispatch by timestamp, not polling order.
        queueMicrotask(()=>{if(step.delay < timeout) {now+=step.delay;resolve(new Response(step.body));} else {[...active.values()][0]!();}});
      });
      const pending=realFetch(url,init);
      if(step.kind==='headers-hang' && spec.mode==='timeout') {
        await server.received(server.requests.length+1);
        assert.equal(active.size,1); [...active.values()][0]!();
      }
      const response=await pending; headerSeen=true; return response;
    };
    const runtime:any={telemetry:{trackFeatureLoopStage:(event:any)=>events.push(event),trackError:(e:any,event:any)=>events.push({...event,error:e.message})}};
    if(spec.auth) runtime.modelAuth={resolveApiKeyForProvider:async()=>{
      events.push({auth:'called'});
      if(spec.auth==='reject') throw new Error('auth failed');
      if(spec.auth==='pending') await new Promise<void>(resolve=>authRelease=resolve);
      return {apiKey:'resolved-key'};
    }};
    const extractor:any=new LlmMemoryExtractor(config,runtime);
    let result:any;
    const pending=extractor.callStructuredJsonWithDebug({...input, debugTrace:(v:any)=>debug.push(v),parse:(raw:string)=>{
      if(spec.parse==='reject') throw new Error('parse rejected');
      return spec.parse==='success'?JSON.parse(raw):raw;
    }}).then((value:any)=>{result={ok:value};settled=true;},(e:any)=>{result={error:e.message,name:e.name,...(e.status===undefined?{}:{status:e.status})};settled=true;});
    const watchdog=realSet(()=>{throw Error(`watchdog ${spec.name}`);},5000);
    try {
      if(['body-hang','no-timeout','auth-pending','backoff-held'].includes(spec.mode)) {
        for(let i=0;i<10000;i++) {
          if(spec.mode==='body-hang'?headerSeen:spec.mode==='no-timeout'?server.requests.length>0:spec.mode==='auth-pending'?!!authRelease:!!held) break;
          await tick();
        }
        await tick(); await tick();
        assert.equal(settled,false,`unexpected completion ${spec.name}`);
        if(spec.mode==='body-hang') {assert.equal(headerSeen,true);assert.equal(active.size,0);now+=100000;}
        if(spec.mode==='no-timeout') {assert.equal(server.requests.length,1);assert.equal(timers.length,0);}
        if(spec.mode==='auth-pending') assert.ok(authRelease);
        if(spec.mode==='backoff-held') {assert.ok(held);assert.equal(calls,1);held!();await pending;}
        else result={pending:true};
      } else await pending;
      cases.push({spec,input,expected:normalized({result,requests:server.requests,fetches,sleeps,timers,events,debug,calls},server.origin)});
    } finally {
      // Snapshot first; cleanup is not an observed product result.
      cases[cases.length-1]=JSON.parse(JSON.stringify(cases[cases.length-1]));
      if(authRelease) authRelease();
      await server.close();
      if(spec.mode==='no-timeout') { // fallback must not create another hanging request on cleanup
        globalThis.fetch=async()=>{throw new Error('fixture cleanup');};
      }
      if(spec.mode==='auth-pending') globalThis.fetch=async()=>{throw new Error('fixture cleanup');};
      if(spec.mode!=='backoff-held') await pending;
      realClear(watchdog); globalThis.fetch=realFetch; globalThis.setTimeout=realSet; globalThis.clearTimeout=realClear;
    }
  }} finally {Date.now=oldDateNow;Math.random=oldRandom;if(oldHome===undefined)delete process.env.HOME;else process.env.HOME=oldHome;rmSync(home,{recursive:true,force:true});}
  return {cases};
}
