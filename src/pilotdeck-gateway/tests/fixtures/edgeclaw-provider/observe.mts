// M20: real provider and builder; deferred promises determine every ordering.
import assert from 'node:assert/strict';
import {mkdtempSync, mkdirSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {stub} from '../edgeclaw-llm/stub.mts';

export const ENTRY = 'src/context/memory/EdgeClawMemoryProvider.ts';
export const SOURCES = [ENTRY, 'src/context/memory/MemoryResolver.ts',
  'src/context/memory/MemoryAttachmentBuilder.ts', 'src/cli/ProjectMemoryBundle.ts',
  'src/context/memory/edgeclaw-memory-core/src/service.ts',
  'src/context/memory/edgeclaw-memory-core/src/core/retrieval/reasoning-loop.ts',
  'src/context/memory/edgeclaw-memory-core/src/core/skills/llm-extraction.ts'];
export const NOW = '2026-10-22T00:00:00.000Z';
export const RANDOM = [0.25, 0.5, 0.75];
export function encode(x: any): any {
  if (x === undefined) return {$undefined: true};
  if (Array.isArray(x)) return x.map(encode);
  if (x && typeof x === 'object') return Object.fromEntries(Object.entries(x).map(([k,v]) => [k,encode(v)]));
  return x;
}
function deferred() {
  let resolve!: (x?: any) => void, reject!: (e: any) => void;
  const promise = new Promise<any>((a,b) => {resolve=a; reject=b;});
  return {promise, resolve, reject};
}
export const MESSAGES = [
  {role:'user', content:[{type:'text', text:'  question  '}]},
  {role:'assistant', content:[{type:'text', text:' old answer '}], metadata:{forkCarryover:true}},
  {role:'assistant', content:[{type:'text', text:' final answer '}, {type:'tool_result', content:[{type:'text',text:' tool output '}]}]},
  {role:'assistant', content:[{type:'text',text:'   '}]},
];
const R = {systemContext:'  recalled\ncontext  ', context:'ignored fallback', trace:{id:'trace'}, debug:{route:'project',mode:'llm'}};
const begin = (query='q1', session='s1') => ({op:'begin',query,session});
const resolve = (query='q1', value:any=R) => ({op:'resolve',query,value});
const cap = (errored:any=false, session='s1') => ({op:'capture',session,errored});
export function specs() {
  const variants:any[] = [
    ['resolve', [begin(),resolve(),cap(),cap()]],
    ['reject', [begin(),{op:'reject',query:'q1',error:'service rejected'},cap()]],
    ['empty', [begin(),resolve('q1',{}),cap()]],
    ['capture', [cap(),cap()]],
    ['errored', [cap(true),cap()]],
    ['capture-error', [{op:'captureError',value:true},cap(),cap()]],
    ['save-error', [{op:'saveError',value:true},cap(),cap()]],
    ['save-missing', [{op:'saveEnabled',value:false},cap(),{op:'saveEnabled',value:true},cap()]],
    ['overwrite', [begin(),resolve(),begin('q2'),resolve('q2',{context:' second ',intent:'explicit'}),cap(),cap()]],
    ['reverse', [begin(),begin('q2'),resolve('q2'),resolve('q1',{context:' first finishes last ',debug:{mode:'fallback'}}),cap()]],
    ['capture-inflight', [begin(),cap(),resolve(),cap(),cap()]],
    ['resolve-in-capture', [begin(),{op:'resolveInCapture',query:'q1',value:R},cap(),{op:'awaitResult',query:'q1'},cap()]],
    ['timeout-late', [{op:'builder',query:'q1',session:'s1',timeout:true},{op:'timeout'},cap(),resolve(),cap(),cap()]],
    ['abort-late', [{op:'builder',query:'q1',session:'s1'},{op:'abort'},cap(),resolve(),cap()]],
    ['never-abort', [{op:'builder',query:'q1',session:'s1'},{op:'abort'},cap()]],
    ['dispose-late', [begin(),{op:'dispose'},cap(),resolve(),cap(),cap()]],
  ];
  const all = [false,true].flatMap(pending => variants.map(([event,ops]) => ({
    name:`${pending?'pending':'none'}-${event}`, pending, ops,
  })));
  all.push(...[
    {name:'sessions',pending:false,ops:[begin(),resolve(),begin('q2','s2'),resolve('q2'),cap(false,'s2'),cap()]},
    {name:'empty-precedence',pending:false,ops:[begin(),resolve('q1',{systemContext:'',context:'fallback',trace:null,debug:{mode:'mode'}}),cap()]},
    {name:'context-fallback',pending:false,ops:[begin(),resolve('q1',{systemContext:null,context:' \ufeff context \u00a0',intent:'',debug:{route:'route'}}),cap('false')]},
    {name:'explicit-options',pending:false,options:{retrievalMode:'explicit',source:'custom'},ops:[begin(),resolve(),cap()]},
    {name:'pre-aborted',pending:false,ops:[{op:'preAbort'},{op:'builder',query:'q1',session:'s1'},{op:'awaitBuilder'},cap()]},
    {name:'direct-pre-aborted',pending:false,ops:[{op:'preAbort'},begin(),resolve(),cap()]},
    {name:'clock-boundaries',pending:false,ops:[begin(),{op:'tick',value:'2026-10-22T00:00:01.000Z'},resolve(),{op:'tick',value:'2026-10-22T00:00:02.000Z'},cap()]},
    {name:'malformed-context',pending:false,ops:[begin(),resolve('q1',{systemContext:true})]},
    {name:'malformed-capture',pending:false,ops:[begin(),resolve('q1',{systemContext:true}),{op:'captureOutcome',session:'s1',errored:false},cap()]},
  ] as any[]);
  return all;
}

async function cell(baseline:string, spec:any) {
  const {EdgeClawMemoryProvider} = await import(pathToFileURL(join(baseline,ENTRY)).href);
  const {MemoryAttachmentBuilder} = await import(pathToFileURL(join(baseline,SOURCES[2])).href);
  const {ProjectMemoryBundle} = await import(pathToFileURL(join(baseline,SOURCES[3])).href);
  const calls:any[]=[], telemetry:any[]=[], results:any[]=[], traces:any[]=[];
  const gates = new Map<string,ReturnType<typeof deferred>>(), inflight = new Map<string,Promise<any>>();
  const signals = new Map<string,AbortSignal>();
  const parent = new AbortController();
  let captureError=false, saveError=false, closed=false, fireTimeout:(()=>void)|undefined, building:Promise<any>|undefined;
  let now=NOW;
  let resolveInCapture:any;
  const save = (record:any) => {traces.push(record); if(saveError || closed) throw Error('save failed');};
  const service:any = {
    retrieveContext(query:string, options:any) {
      signals.set(query,options.signal);
      calls.push({method:'retrieve',query,options:{...options,signal:{aborted:options.signal.aborted}}});
      const gate=deferred(); gates.set(query,gate); return gate.promise;
    },
    captureTurn(messages:any,input:any) {calls.push({method:'capture',messages,input}); if(resolveInCapture) {const op=resolveInCapture;resolveInCapture=undefined;gates.get(op.query)!.resolve(op.value);} if(captureError || closed) throw Error('capture failed'); return {captured:true,normalizedMessages:messages,sessionKey:input.sessionKey};},
    saveCaseTrace:save,
    close() {closed=true;calls.push({method:'close'});},
  };
  const provider:any = new EdgeClawMemoryProvider({service,...spec.options,now:()=>new Date(now),telemetry:{
    trackFeatureLoopStage(event:any){telemetry.push({method:'stage',event});},
    trackError(error:any,event:any){telemetry.push({method:'error',error:{name:error.name,message:error.message},event});},
  }});
  const providerHasDispose = typeof provider.dispose !== 'undefined';
  const originalRetrieve = provider.retrieve.bind(provider);
  provider.retrieve = (input:any) => {const p=originalRetrieve(input); inflight.set(input.query,p); return p;};
  const bundle = new ProjectMemoryBundle({projectRoot:'/workspace',createProvider:()=>({provider,service})});
  bundle.stage();
  const builder = new MemoryAttachmentBuilder(provider);
  const input = (query:string,session:string) => ({query,sessionId:session,projectRoot:'/workspace',recentMessages:MESSAGES,signal:parent.signal});
  const realSetTimeout = globalThis.setTimeout, realClearTimeout=globalThis.clearTimeout;
  const timer:any={fixture:true};
  globalThis.setTimeout = ((f:any,ms:any,...args:any[]) => {
    if(ms===1234) {fireTimeout=()=>f(...args);return timer;}
    return realSetTimeout(f,ms,...args);
  }) as any;
  globalThis.clearTimeout = ((t:any)=>{if(t!==timer) realClearTimeout(t);}) as any;
  try {
    if(spec.pending) {provider.retrieve(input('seed','s1'));gates.get('seed')!.resolve({context:' seed context ',intent:'seed'});await inflight.get('seed');}
    for(const op of spec.ops) {
      switch(op.op) {
        case 'begin': provider.retrieve(input(op.query,op.session));break;
        case 'resolve': gates.get(op.query)!.resolve(op.value);results.push({query:op.query,value:await inflight.get(op.query)});break;
        case 'resolveInCapture':resolveInCapture=op;break;
        case 'awaitResult':results.push({query:op.query,value:await inflight.get(op.query)});break;
        case 'reject': gates.get(op.query)!.reject(Error(op.error));results.push({query:op.query,value:await inflight.get(op.query)});break;
        case 'capture': await provider.captureTurn({sessionId:op.session,projectRoot:'/workspace',messages:MESSAGES,errored:op.errored});break;
        case 'captureOutcome': {
          let error:string|null=null;
          try {await provider.captureTurn({sessionId:op.session,projectRoot:'/workspace',messages:MESSAGES,errored:op.errored});}
          catch (caught) {error=(caught as Error).message;}
          results.push({captureError:error});break;
        }
        case 'captureError':captureError=op.value;break;
        case 'saveError':saveError=op.value;break;
        case 'saveEnabled':service.saveCaseTrace=op.value?save:undefined;break;
        case 'builder':building=builder.build({...input(op.query,op.session),timeoutMs:op.timeout?1234:0});break;
        case 'timeout':assert.ok(fireTimeout,'#777 timeout must be armed');fireTimeout();results.push({builder:await building});break;
        case 'abort':parent.abort('parent aborted');results.push({builder:await building});break;
        case 'preAbort':parent.abort('pre-aborted');break;
        case 'awaitBuilder':results.push({builder:await building});break;
        case 'dispose':await bundle.dispose();break;
        case 'tick':now=op.value;break;
        default:throw Error(`unknown op ${op.op}`);
      }
    }
    return {spec,providerHasDispose,calls,telemetry,results,traces,finalSignals:Object.fromEntries([...signals].map(([query,signal])=>[query,signal.aborted]))};
  } finally {globalThis.setTimeout=realSetTimeout;globalThis.clearTimeout=realClearTimeout;}
}

async function realServiceAbort(baseline:string,root:string,resetRandom:()=>void,closeEarly=false) {
  const {EdgeClawMemoryProvider} = await import(pathToFileURL(join(baseline,ENTRY)).href);
  const {EdgeClawMemoryService} = await import(pathToFileURL(join(baseline,SOURCES[4])).href);
  const response={choices:[{message:{content:'{"route":"none"}'}}]};
  const script=[{status:200,body:JSON.stringify(response)}];
  const server=await stub(script,()=>Date.parse(NOW));
  const realFetch=globalThis.fetch, entered=deferred(), release=deferred();
  let transportSignal:any;
  // Gate actual dispatch. The abort occurs after the LLM invocation entered,
  // before its HTTP request is sent, proving the request still proceeds.
  globalThis.fetch=(async (url:any,init:any)=>{transportSignal=init?.signal;entered.resolve();await release.promise;return realFetch(url,init);}) as any;
  const service=new EdgeClawMemoryService({workspaceDir:join(root,'workspace'),rootDir:join(root,'store'),llm:{provider:'p',model:'m',modelRef:'p/m',baseUrl:server.origin+'/v1',apiKey:'fixture-key',apiType:'openai-completions'}});
  const provider=new EdgeClawMemoryProvider({service});
  const parent=new AbortController();
  try {
    // Module loading may consume Math.random; the operation's input sequence
    // starts here, after imports and construction, identically in each cell.
    resetRandom();
    const pending=provider.retrieve({query:'remember my preference',sessionId:'s1',projectRoot:'/workspace',recentMessages:[],signal:parent.signal});
    if(closeEarly) service.close();
    await Promise.race([entered.promise,pending.then(()=>{throw Error('#777 real service skipped LLM dispatch');})]);
    parent.abort('parent aborted');
    const llmSignalAbortedAtParentAbort=transportSignal?.aborted ?? null;
    release.resolve();
    const result=await pending;
    assert.equal(server.requests.length,1,'#777 exactly one real LLM request');
    return {...(closeEarly?{closedBeforeDispatch:true}:{}),script,parentAborted:parent.signal.aborted,llmSignalAbortedAtParentAbort,requests:server.requests,result};
  } finally {release.resolve();globalThis.fetch=realFetch;if(!closeEarly)service.close();await server.close();}
}

export async function collect(baseline:string) {
  assert.equal(process.version,'v22.22.0','#777 exact Node baseline');
  const root=mkdtempSync(join(tmpdir(),'pdrs-provider-'));
  const home=join(root,'home');mkdirSync(home,{recursive:true});
  const oldHome=process.env.HOME, RealDate=Date, random=Math.random;
  let draw=0;
  class FixedDate extends RealDate {constructor(...args:any[]){super(...((args.length?args:[NOW]) as [any]));} static now(){return RealDate.parse(NOW);}}
  (globalThis as any).Date=FixedDate;Math.random=()=>RANDOM[draw++%RANDOM.length];process.env.HOME=home;
  try {
    const scenarios=[];
    // Cells patch global timers; run sequentially, never concurrently.
    for(const spec of specs()) scenarios.push(await cell(baseline,spec));
    return encode({scenarios,realServiceAbort:await realServiceAbort(baseline,root,()=>{draw=0;}),realServiceClose:await realServiceAbort(baseline,join(root,'closed'),()=>{draw=0;},true)});
  }
  finally {(globalThis as any).Date=RealDate;Math.random=random;if(oldHome===undefined)delete process.env.HOME;else process.env.HOME=oldHome;rmSync(root,{recursive:true,force:true});}
}
