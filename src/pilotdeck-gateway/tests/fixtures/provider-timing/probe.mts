import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
const root = process.env.PDRS_BASELINE ?? '/Users/da/ws/pilotdeck-rs/resources/PilotDeck';
const { streamModel } = await import(pathToFileURL(`${root}/src/model/streaming/streamModel.ts`).href);
const { TokenAccountingRuntime } = await import(pathToFileURL(`${root}/src/context/budget/TokenAccountingRuntime.ts`).href);
const realTimer = globalThis.setTimeout, realFetch = globalThis.fetch;
const sleep = (ms:number) => new Promise(r => realTimer(r, ms));
const request = { provider:'p', model:'m', messages:[] };
for (const source of ['model/streaming/streamModel.ts','context/budget/TokenAccountingRuntime.ts','network/fetch.ts']) console.log(JSON.stringify({source, node:process.version, sha256:createHash('sha256').update(readFileSync(`${root}/src/${source}`)).digest('hex')}));
const protocols = ['anthropic','openai','openai-responses','google'];
const phases = ['headers','body','response_text','count'];
const events = ['fulfilled','rejected','idle'];
const orders = ['continuation-first','parent-first','tie-parent-first','tie-parent-last'];
for (const protocol of protocols) for (const phase of phases) for (const event of events) for (const order of orders) {
  let captured = false;
  for(let attempt=0;attempt<10;attempt++){
  const id = `${protocol}/${phase}/${event}/${order}`;
  const controller = new AbortController();
  const timeline:string[] = [], timers:any[] = [], output:any[] = [], retries:any[] = [];
  const idle = event === 'idle';
  const operationMs = idle ? 40 : 20;
  const parentMs = operationMs + (order === 'continuation-first' ? 5 : order === 'parent-first' ? -5 : 0);
  const parentFirst = order !== 'tie-parent-last';
  const provider:any = { id:'p', protocol, url:protocol==='openai-responses'?'https://api.openai.com/v1':'http://example.test', apiKey:'fixture', headers:{}, models:{m:{id:'m',capabilities:{supportsStreaming:true,maxOutputTokens:4096,contextWindow:200000}}}, retry:{streamMaxRetries:1,maxRetries:1,baseDelayMs:100,maxDelayMs:100,jitter:0} };
  let armed=false, active=phase==='headers'||phase==='count'||(phase==='response_text'&&protocol==='google'), calls=0;
  let parent:any;
  const armParent=()=>{parent=realTimer(()=>{timeline.push('parent');controller.abort();},parentMs);timers.push({kind:'parent',start:parent._idleStart,delay:parent._idleTimeout});};
  globalThis.setTimeout=((callback:any,ms:any,...args:any[])=>{
    const target=active&&!armed;
    if(target){armed=true;if(parentFirst)armParent();}
    const timer:any=realTimer(()=>{timeline.push(`timer:${ms}`);callback(...args);},ms);
    timers.push({kind:target?'watchdog':'timer',start:timer._idleStart,delay:timer._idleTimeout});
    if(target&&!parentFirst&&idle)armParent();
    return timer;
  }) as any;
  const schedule=(value:any, signal?:AbortSignal, body=false)=>{
    if(!armed){armed=true;if(parentFirst)armParent();}
    const promise=new Promise((resolve,reject)=>{
      if(!idle){const t:any=realTimer(()=>{timeline.push(event);event==='rejected'?reject(new TypeError(body?'terminated':'fixture transport failure')):resolve(value);},operationMs);timers.push({kind:'completion',start:t._idleStart,delay:t._idleTimeout});}
      // A mock of undici's independent body timeout, not a parent listener.
      else if(phase==='response_text'&&protocol!=='google'){const t:any=realTimer(()=>{timeline.push('body-timeout');reject(new TypeError('terminated'));},operationMs);timers.push({kind:'body-timeout',start:t._idleStart,delay:t._idleTimeout});}
      if(signal)signal.addEventListener('abort',()=>reject(signal.reason),{once:true});
    });
    if(!parentFirst&&!parent)armParent();
    // Both actual Node timers are overdue; Node drains microtasks after each.
    const until=performance.now()+55;while(performance.now()<until){}
    timeline.push('overdue');return promise;
  };
  const data = protocol==='google'?{candidates:[{content:{role:'model',parts:[{text:'FIRST'}]},finishReason:'STOP'}]}:null;
  const sse = protocol==='anthropic'?'data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"FIRST"}}\n\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn"}}\n\n':protocol==='openai'?'data: {"choices":[{"index":0,"delta":{"content":"FIRST"},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n':'data: {"type":"response.completed","response":{"id":"r","status":"completed","output":[]}}\n\n';
  const errorText='{"error":{"message":"fixture bad request","code":400,"status":"INVALID_ARGUMENT"}}';
  let reads=0;
  const reader={read:()=>reads++?Promise.resolve({done:true}):phase==='headers'?Promise.resolve({done:false,value:new TextEncoder().encode(sse)}):schedule({done:false,value:new TextEncoder().encode(sse)},undefined,true),cancel:()=>Promise.resolve()};
  const response:any = {ok:true,status:200,headers:new Headers({'content-type':'text/event-stream'}),body:{getReader:()=>{active=true;return reader;}},text:()=>Promise.resolve(''),json:()=>Promise.resolve({input_tokens:73})};
  const badResponse:any={ok:false,status:400,statusText:'Bad Request',headers:new Headers({'content-type':'application/json'}),text:()=>schedule(errorText,undefined,true),json:()=>schedule(JSON.parse(errorText),undefined,true)};
  const transport=(_url:any,opts:any)=>{calls++;if(phase==='headers')return schedule(response,opts.signal);if(phase==='count')return schedule({...response,json:()=>Promise.resolve({input_tokens:73})},opts.signal);if(phase==='response_text')return Promise.resolve(badResponse);return Promise.resolve(response);};
  let result:any;
  try {
    if(phase==='count')result=await new TokenAccountingRuntime({modelConfig:{providers:{p:provider}},fetch:transport,timeoutMs:40,cacheSize:0}).countRequestInput(request,{signal:controller.signal});
    else {
      // Real SDK for error-body cells; injected SDK promise/iterator elsewhere,
      // matching the established provider-abort probe's boundary.
      if(protocol==='google'&&phase==='response_text')globalThis.fetch=transport as any;
      const options:any={fetch:transport,signal:controller.signal,streamTimeoutMs:40,onRetryProgress:(r:any)=>retries.push(r)};
      if(protocol==='google'&&phase!=='response_text') options.googleClientFactory=()=>({models:{generateContentStream:()=>{calls++;if(phase==='body')active=true;return phase==='headers'?schedule({next:()=>Promise.resolve({done:false,value:data}),return:()=>Promise.resolve({done:true})},controller.signal):Promise.resolve({next:()=>schedule({done:false,value:data},undefined,true),return:()=>Promise.resolve({done:true})});}}});
      for await(const item of streamModel(request,{providers:{p:provider}},options)) output.push(item);
      result={events:output};
    }
  }catch(e:any){result={name:e.name,message:e.message,events:output};}
  finally{globalThis.setTimeout=realTimer;globalThis.fetch=realFetch;}
  const error=output.find(e=>e.type==='error')?.error;
  const observable=phase==='count'?{name:null,message:result.estimatorError??null,retries:0,source:result.source,tokens:result.tokens}:{name:result.name??(error&&Object.hasOwn(error.raw??{},'name')?error.raw.name:null)??(error?'ModelProviderError':null),message:result.message??error?.message??null,retries:retries.length,delivery:result.name?'throw':error?'event':'ok'};
  const completion=timers.find(t=>t.kind===(idle?(phase==='response_text'&&protocol!=='google'?'body-timeout':'watchdog'):'completion'));
  const pt=timers.find(t=>t.kind==='parent');
  if(calls>0) {
    assert.ok(completion&&pt, `#280 ${id}: missing actual Node timers`);
    const gap = pt.start + pt.delay - completion.start - completion.delay;
    const ordered = order==='continuation-first' ? gap>0 : order==='parent-first' ? gap<0 : gap===0;
    if(!ordered){console.error(JSON.stringify({id,attempt,discard:'different actual Node deadlines',gap,timers}));await sleep(60);continue;}
    assert.equal(timers.indexOf(pt)<timers.indexOf(completion),parentFirst,`#280 ${id}: actual registration order`);
  }
  console.log(JSON.stringify({id,protocol,phase,event,order,reachable:calls>0,rawErrorName:error?.raw?.name??null,timers,timeline,result,retries,observable}));
  captured = true;await sleep(60);break;
  }
  assert.ok(captured, `#280 ${protocol}/${phase}/${event}/${order}: exhausted actual-deadline candidates`);
}

for(const protocol of protocols){
 const controller=new AbortController();const output:any[]=[];const retries:any[]=[];
 const provider:any={id:'p',protocol,url:'http://example.test',apiKey:'fixture',headers:{},models:{m:{id:'m',capabilities:{supportsStreaming:true,maxOutputTokens:4096}}},retry:{streamMaxRetries:1}};
 const rejected=()=>{const p=Promise.reject(new TypeError('fixture transport failure'));controller.abort();return p;};
 let result:any;try{for await(const e of streamModel(request,{providers:{p:provider}},{fetch:rejected,signal:controller.signal,onRetryProgress:(r:any)=>retries.push(r),googleClientFactory:()=>({models:{generateContentStream:rejected}})}))output.push(e);result={events:output};}catch(e:any){result={name:e.name,message:e.message,events:output};}
 console.log(JSON.stringify({id:`${protocol}/headers/rejected/synchronous`,protocol,phase:'headers',event:'rejected',order:'synchronous',reachable:true,result,retries,observable:{name:result.name??null,message:result.message??null,retries:retries.length,delivery:result.name?'throw':'ok'}}));
}
