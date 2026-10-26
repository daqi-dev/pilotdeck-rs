// Real fixed-baseline entry + Node's bundled undici, with no network access.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {Duplex} from 'node:stream';
import {pathToFileURL} from 'node:url';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';
const ENTRY = 'src/context/memory/edgeclaw-memory-core/src/core/skills/llm-extraction.ts';
const base = resolve(process.env.PDRS_BASELINE!), out = resolve(process.argv[2]);
assert.equal(process.version, 'v22.22.0');
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], {encoding:'utf8'}).trim());
const sourceCommit = bindSourceTree(base, commit, git).ref;
const sourceSha256 = sourceClosureSha256(base, commit, git, [ENTRY, 'src/cli/proxy.ts']);
const home = mkdtempSync(join(tmpdir(), 'pdrs-llm-r1-home-'));
const oldHome = process.env.HOME; process.env.HOME = home;
const realSet = globalThis.setTimeout, realClear = globalThis.clearTimeout, realFetch = globalThis.fetch;
const tick = () => new Promise<void>(resolve => setImmediate(resolve));
const tokens: any[] = [];
let now = 0, sleeps: number[] = [], configTimers: any[] = [];
const cases: any[] = [];
try {
  const {LlmMemoryExtractor} = await import(pathToFileURL(join(base, ENTRY)).href);
  // Initialize the Node-bundled dispatcher; do not mix it with node_modules/undici.
  await realFetch('data:,');
  const Agent = (globalThis as any)[Symbol.for('undici.globalDispatcher.1')].constructor;
  const {createLongTimeoutOptions} = await import(pathToFileURL(join(base, 'src/cli/proxy.ts')).href);
  assert.equal(createLongTimeoutOptions().headersTimeout,600000);
  assert.equal(createLongTimeoutOptions().bodyTimeout,600000);

  globalThis.setTimeout = ((fn: any, ms: number, ...args: any[]) => {
    const stack = new Error().stack ?? '';
    if (fn.name !== 'onTick' && !stack.includes('/llm-extraction.ts:')) return realSet(fn, ms, ...args);
    const token: any = {active:true, at:now+ms, ms, kind:fn.name === 'onTick' ? 'fast' : stack.includes('executeOnce') ? 'config' : 'retry',
      fn:()=>fn(...args), unref(){return this;}, refresh(){this.active=true; this.at=now+ms; return this;}, cleared:false};
    tokens.push(token);
    if (token.kind === 'retry') sleeps.push(ms);
    if (token.kind === 'config') configTimers.push(token);
    return token;
  }) as any;
  globalThis.clearTimeout = ((token: any) => {
    if (tokens.includes(token)) {token.active=false; token.cleared=true;} else realClear(token);
  }) as any;
  const specs: any[] = [];
  for (const api of ['responses','chat']) for (const transportMs of [300000,600000]) {
    for (const timeout of [0,-1,200000,transportMs,transportMs+100000]) {
      specs.push({name:`headers-${api}-${transportMs}-${timeout}`,api,transportMs,timeout,mode:'headers'});
    }
  }
  for (const api of ['responses','chat']) for (const status of [200,503]) for (const transportMs of [300000,600000]) {
    for (const timeout of [200000,transportMs,transportMs+100000]) specs.push({name:`body-${api}-${status}-${transportMs}-${timeout}`,api,status,transportMs,timeout,mode:'body'});
  }
  for (const [name, headers] of Object.entries({control:{'x-test':'a\u0001b'},del:{'x-test':'a\u007fb'},nul:{'x-test':'a\u0000b'},newline:{'x-test':' \ta\nb\t '},cr:{'x-test':'a\rb'},overwritten:{'sec-fetch-mode':'a\u0001b'},duplicate:{'X-Test':'a','x-test':'\u0001'},wide:{'x-test':'中'},conversion:{'bad name':'x','x-wide':'中'},sameEntry:{'bad name':'中'}})) {
    for (const api of ['responses','chat']) specs.push({name:`header-${name}-${api}`,api,headers,timeout:100,mode:'header'});
  }
  for (const api of ['responses','chat','anthropic','google']) specs.push({name:`header-default-key-${api}`,api,apiKey:'a\u0001b',timeout:100,mode:'header'});
  for (const [name, baseUrl, headers] of [
    ['https-upper','HTTPS://fixture.invalid',{}], ['http','http://fixture.invalid',{}],
    ['range','http://fixture.invalid',{range:'bytes=0-5'}], ['range-https','HTTPS://fixture.invalid',{Range:'bytes=0-5'}],
    ['range-empty','http://fixture.invalid',{range:''}], ['range-explicit','http://fixture.invalid',{range:'bytes=0-5','accept-encoding':'gzip'}],
    ['explicit','HTTPS://fixture.invalid',{'accept-encoding':'deflate'}],
  ] as any[]) specs.push({name:`encoding-${name}`,api:'responses',baseUrl,headers,timeout:100,mode:'encoding'});
  specs.push({name:'encoding-redirect-upgrade',api:'responses',baseUrl:'http://fixture.invalid',redirect:'https://fixture.invalid/final',timeout:100,mode:'encoding'});
  specs.push({name:'encoding-range-redirect',api:'responses',headers:{range:'bytes=0-5'},baseUrl:'http://fixture.invalid',redirect:'http://fixture.invalid/final',timeout:100,mode:'encoding'});
  specs.push({name:'body-idle-reset',api:'responses',transportMs:300000,timeout:0,mode:'body',frameAt:200000});
  specs.push({name:'cancel-body-after-frame',api:'responses',transportMs:300000,timeout:0,mode:'body',frameAt:200000,cancel:true});
  for (const mode of ['headers','body']) specs.push({name:`cancel-${mode}`,api:'responses',transportMs:300000,timeout:0,mode,cancel:true});
  for (const spec of specs) {
    sleeps=[]; configTimers=[];
    let calls=0, connects=0, settled=false, result:any, sent=false, fastTicks=0, pendingBefore=false, frameSent=false, pendingAfterOriginalIdle=false;
    const sockets: Duplex[] = [], requests: string[] = [], debug:any[] = [], causes:string[] = [], encodings:string[] = [];
    const agent = new Agent({...(spec.transportMs ? {headersTimeout:spec.transportMs,bodyTimeout:spec.transportMs}:{}), connect: (_:any, cb:any) => {
      connects++;
      const socket = new Duplex({read(){},write(chunk, _encoding, done){
        requests.push(chunk.toString('latin1')); done();
        if (sent) return; sent=true;
        if (spec.mode === 'headers') return;
        const body=JSON.stringify(spec.api === 'chat' ? {choices:[{message:{content:'ok'}}]} : {output_text:'ok'});
        if(spec.frameAt) {
          tokens.push({active:true,kind:'frame',at:now+spec.frameAt,fn:()=>{frameSent=true;socket.push('{');}});
          tokens.push({active:true,kind:'observe',at:now+350000,fn:()=>{pendingAfterOriginalIdle=!settled;}});
        }
        queueMicrotask(()=>socket.push(`HTTP/1.1 ${spec.status??200} OK\r\nContent-Type: application/json\r\nContent-Length: ${spec.mode === 'body' ? 9999 : Buffer.byteLength(body)}\r\n\r\n${spec.mode === 'body' ? '' : body}`));
      }});
      socket.on('error',()=>{}); sockets.push(socket); cb(null,socket);
    }});
    const responseDispatcher = {dispatch(options:any, handler:any){
      encodings.push(options.headers['accept-encoding']);
      handler.onConnect(()=>{});
      if(spec.redirect && encodings.length===1) {handler.onHeaders(307,['location',spec.redirect],()=>{},'Redirect');handler.onComplete([]);return true;}
      handler.onHeaders(200,['content-type','application/json'],()=>{},'OK');
      handler.onData(Buffer.from('{"output_text":"ok"}')); handler.onComplete([]); return true;
    }};
    let signal:AbortSignal|undefined;
    const cancelController = new AbortController();
    globalThis.fetch = (async (url:any, options:any) => {
      calls++; sent=false; signal=options.signal;
      try {return await realFetch(url,{...options, signal:spec.cancel?AbortSignal.any([options.signal,cancelController.signal]):options.signal, dispatcher:spec.mode==='encoding'?responseDispatcher:agent});}
      catch(e:any){if(e.cause?.code)causes.push(e.cause.code);throw e;}
    }) as any;
    const extractor = new LlmMemoryExtractor({agents:{defaults:{model:{primary:'p/m'}}},models:{providers:{p:{api:spec.api,baseUrl:spec.baseUrl??'http://fixture.invalid',apiKey:spec.apiKey??'k',headers:spec.headers}}}},{});
    const promise = extractor.callStructuredJsonWithDebug({systemPrompt:'s',userPrompt:'u',requestLabel:'probe',timeoutMs:spec.timeout,parse:(raw:string)=>raw,debugTrace:(event:any)=>debug.push(event)})
      .then((ok:any)=>{result={ok};settled=true;},(e:any)=>{result={error:e.message,name:e.name};settled=true;});
    const watchdog = realSet(()=>{throw Error(`probe watchdog: ${spec.name}`);},10000);
    try {
      // All waiting is bounded; execute actual undici timer callbacks in timestamp order.
      for(let i=0;i<12000 && !settled;i++) {
        await tick();
        if(spec.cancel && (!spec.frameAt || frameSent) && sockets.length && (spec.mode==='headers' || configTimers.every(t=>t.cleared))) {
          cancelController.abort();
          // AbortError is transient: stop at the first retry sleep, before a successor.
        }
        if(spec.cancel && sleeps.length) break;
        const next=tokens.filter(t=>t.active).sort((a,b)=>a.at-b.at)[0];
        if(!next) continue;
        if(next.kind==='fast') {fastTicks++; if(fastTicks===100)pendingBefore=!settled;}
        now=Math.max(now,next.at); next.active=false; next.fn();
      }
      if(!spec.cancel) {assert.ok(settled,`bounded probe ${spec.name}`);await promise;}
      else {assert.ok(sockets.length>0);result={cancelled:true};}
      const observation={spec,expected:{result,calls,connects,sleeps:[...sleeps],debug,causes,encodings,pendingBefore,frameSent,pendingAfterOriginalIdle,
        configCleared:configTimers.map(t=>t.cleared),firstSocketDestroyed:sockets[0]?.destroyed??true,socketsDestroyed:sockets.every(s=>s.destroyed)}};
      cases.push(observation); console.log(JSON.stringify(observation));
    } finally {
      realClear(watchdog); await agent.destroy();
      assert.ok(sockets.every(s=>s.destroyed), 'owned sockets closed after agent destroy');
      // Cancellation intentionally stops before retry; no later case may release it.
      for(const token of tokens) if(token.kind!=='fast')token.active=false;
      assert.ok(!signal || !spec.cancel || cancelController.signal.aborted);
    }
  }
  mkdirSync(out,{recursive:true});
  writeFileSync(join(out,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,nodeVersion:process.version,undiciVersion:process.versions.undici,cases},null,2)+'\n',{flag:'wx'});
  console.log(JSON.stringify({cases:cases.length,node:process.version,output:join(out,'oracle.json')}));
} finally {
  globalThis.setTimeout=realSet; globalThis.clearTimeout=realClear; globalThis.fetch=realFetch;
  if(oldHome===undefined) delete process.env.HOME; else process.env.HOME=oldHome;
  rmSync(home,{recursive:true,force:true});
}
