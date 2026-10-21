// Real baseline execution; no filesystem writes, Git calls or external network.
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import {createServer} from 'node:http';
import {createServer as createNetServer} from 'node:net';
export async function record(baseline: string, httpOnly=false) {
  const load = (file: string) => import(pathToFileURL(join(baseline, file)).href);
  const { ProviderHealthTracker } = await load('src/router/health/ProviderHealthTracker.ts');
  const { createNativeRouterProviderHealthPort } = await load('src/router/health/RouterProviderHealthPort.ts');
  const { isFallbackEligible, planFallback } = await load('src/router/fallback/runFallbackChain.ts');
  const { createRouterRuntime } = await load('src/router/RouterRuntime.ts');
  const { ModelRequestError } = await load('src/model/protocol/errors.ts');
  const { streamModel } = await load('src/model/streaming/streamModel.ts');
  const originalRandom = Math.random; Math.random = () => 0.5;
  const healthSpecs = [
    { name: 'default-cooldown', actions: [ ['state','p'], ...Array.from({length:5},()=>['failure','p']), ['time',29999], ['skip','p'], ['failure','p'], ['time',30000], ['available','p'], ['state','p'], ['failure','p'], ['time',59999], ['state','p'], ['time',60000], ['state','p'], ['success','p'], ['state','p'] ] },
    { name: 'window-map-reset', options:{windowSize:3}, actions:[['success','2'],['failure','1'],['failure','2'],['failure','2'],['success','2'],['reset','2'],['failure','2'],['resetAll'],['state','x']] },
    { name: 'half-open-below-threshold', options:{degradeThreshold:1,openThreshold:2,openDurationMs:10,windowSize:0}, actions:[['failure','p'],['failure','p'],['time',10],['state','p'],['success','p'],['failure','p'],['success','p']] },
    { name: 'success-without-cooldown-read', actions:[...Array.from({length:5},()=>['failure','p']),['time',50000],['success','p']] },
  ];
  const healthCases = healthSpecs.map(spec => {
    let now=0; const tracker=new ProviderHealthTracker({...spec.options,now:()=>now});
    const steps=spec.actions.map(([action,arg]:any[])=> {
      let result;
      if(action==='time') now=arg;
      if(action==='state') result=tracker.getState(arg);
      if(action==='skip') result=tracker.shouldSkip(arg);
      if(action==='available') result=tracker.isAvailable(arg);
      if(action==='success') tracker.recordSuccess(arg);
      if(action==='failure') tracker.recordFailure(arg);
      if(action==='reset') tracker.reset(arg);
      if(action==='resetAll') tracker.resetAll();
      return {result,snapshot:[...tracker.snapshot()]};
    });
    return {...spec,steps};
  });
  let now=0; const port=createNativeRouterProviderHealthPort({now:()=>now});
  const portActions:any[]=[];
  const portStep=(action:string,input:any)=>{ const skip=action==='skip'?port.shouldSkip(input):undefined; if(action==='failure')port.recordFailure(input); if(action==='success')port.recordSuccess(input); portActions.push({action,input,skip}); };
  const base={sessionId:'s',providerId:'p',providerGeneration:1};
  for(let i=0;i<5;i++)portStep('failure',base);
  for(const input of [base,{...base,providerGeneration:'1'},{...base,providerGeneration:2},{...base,sessionId:'other'},{sessionId:'s',providerId:'p'},{...base,providerGeneration:null}])portStep('skip',input);
  now=30000; portActions.push({action:'time',now});portStep('skip',base);portStep('failure',base);portStep('skip',base);port.dispose();portActions.push({action:'dispose'});portStep('skip',base);
  const eligibility:any[]=[];
  for(const code of ['invalid_tool_arguments','billing','model_not_found','auth_error','prompt_too_long','request_too_large','context_overflow','network_error','timeout','rate_limit_error'])for(const retryable of [false,true])for(const recovery of [{},{recoverableViaCompact:true},{recoverableViaImageStrip:true}]) {
    const error={code,retryable,...recovery};eligibility.push({error,eligible:isFallbackEligible(error)});
  }
  const ref=(provider:string)=>({id:`${provider}/m`,provider,model:'m'});
  const request={provider:'a',model:'m',messages:[{role:'user',content:[{type:'text',text:'hello'}]}],maxOutputTokens:100};
  const decision={provider:'a',model:'m',scenarioType:'default',isSubagent:false,orchestrating:false,resolvedFrom:'scenario',mutations:{}};
  const failure=(code='server_error',retryable=true)=>({type:'error',error:{provider:'a',protocol:'anthropic',code,message:'failed',retryable}});
  const frame={type:'message_start',messageId:'id'};
  const ok=[frame,{type:'text_delta',text:'ok'},{type:'usage',usage:{inputTokens:3,outputTokens:2}},{type:'message_end',finishReason:'stop'}];
  const config={enabled:true,fallback:{default:[ref('b')]},transientRetry:{enabled:false},zeroUsageRetry:{enabled:false}};
  const sse=(event:any)=>`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`;
  const start=sse({type:'message_start',message:{id:'msg_1',type:'message',role:'assistant',content:[],model:'m',usage:{input_tokens:11,output_tokens:1}}});
  const text=sse({type:'content_block_start',index:0,content_block:{type:'text',text:''}})+sse({type:'content_block_delta',index:0,delta:{type:'text_delta',text:'ok'}});
  const end=sse({type:'message_delta',delta:{stop_reason:'end_turn'},usage:{output_tokens:7}})+sse({type:'message_stop'});
  const success={chunks:[start,text,end]};
  const httpError=(status:number)=>({status,headers:{'content-type':'application/json'},chunks:[JSON.stringify({type:'error',error:{type:status===429?'rate_limit_error':'api_error',message:status===429?'rate limit':'server fault'}})]});
  const specs:any[]=[
    {name:'usage-estimate-missing',scripts:[{events:[frame,{type:'text_delta',text:'hello '},{type:'thinking_delta',text:'world'},{type:'tool_call_delta',id:'t',delta:'{}'},{type:'message_end',finishReason:'stop'}]}]},
    ...[null,{}, {inputTokens:null,outputTokens:null,cacheReadTokens:null}, {inputTokens:0}].map((usage,index)=>({name:`usage-estimate-empty-${index}`,scripts:[{events:[frame,{type:'text_delta',text:'ok'},{type:'usage',usage},{type:'message_end',finishReason:'stop'}]}]})),
    ...[{inputTokens:3,outputTokens:null,cacheReadTokens:null},{outputTokens:2}].map((usage,index)=>({name:`usage-preserve-partial-${index}`,scripts:[{events:[frame,{type:'text_delta',text:'ok'},{type:'usage',usage},{type:'message_end',finishReason:'stop'}]}]})),
    // JSON encodes NaN as null. nanUsageFields lets Rust restore these
    // actual TS inputs before invoking the router, without changing expectations.
    ...[
      {name:'usage-estimate-nan',nanUsageFields:['inputTokens','outputTokens'],usage:{inputTokens:Number.NaN,outputTokens:Number.NaN}},
      {name:'usage-preserve-partial-nan',nanUsageFields:['outputTokens'],usage:{inputTokens:3,outputTokens:Number.NaN}},
    ].map(({usage,...spec})=>({...spec,scripts:[{events:[frame,{type:'text_delta',text:'ok'},{type:'usage',usage},{type:'message_end',finishReason:'stop'}]}]})),
    {name:'fallback-framing',scripts:[{events:[frame,failure()]},{events:ok}]},
    {name:'final-precontent-error',config:{fallback:{}},scripts:[{events:[frame,failure('invalid_request',false)]}]},
    ...['text_delta','thinking_delta','tool_call_start','tool_call_delta','tool_call_end'].map(type=>({name:`content-lock-${type}`,scripts:[{events:[frame,{type,text:'',delta:'',id:'t',name:'tool',input:{}},failure()]}]})),
    {name:'sdk-explicit-disabled',config:{enabled:false},decision:{scenarioType:'explicit'},fallbackModels:[ref('b')],scripts:[{events:[failure()]},{events:ok}]},
    {name:'explicit-no-project-fallback',decision:{scenarioType:'explicit'},scripts:[{events:[failure()]}]},
    {name:'disabled-passthrough',config:{enabled:false},scripts:[{events:[frame,failure()]}]},
    {name:'fallback-before-transient',config:{transientRetry:{enabled:true,maxAttempts:1,baseDelayMs:0,maxDelayMs:0}},scripts:[{events:[failure()]},{events:[failure()]},{events:ok}]},
    {name:'zero-usage-last-event',config:{zeroUsageRetry:{enabled:true,maxAttempts:2}},scripts:[{events:[frame,{type:'usage',usage:{inputTokens:7}},{type:'usage',usage:{inputTokens:0,outputTokens:0}},{type:'message_end'}]},{events:ok}]},
    {name:'health-open-middle-final',config:{fallback:{default:[ref('b'),ref('c'),ref('b')]}},seedOpen:['a','b','c'],scripts:[{events:[failure()]},{events:ok}]},
    {name:'media-native-first',request:{messages:[{role:'user',content:[{type:'image',mimeType:'image/png',bytes:1024,data:'AA==',source:'base64'}]}]},media:{a:['text'],b:['text','image']},scripts:[{events:[failure()]},{events:ok}]},
    {name:'deny-before-io',managedModelPolicy:{allow:[],deny:['a/*']},scripts:[]},
    {name:'filtered-sdk-project-fallback',fallbackModels:[ref('c')],managedModelPolicy:{allow:[],deny:['c/*']},scripts:[{events:[failure()]},{events:ok}]},
    {name:'cancel-before-attempt',abortBefore:true,scripts:[]},
    {name:'cancel-mid-stream',abortAfterText:true,scripts:[{events:ok}]},
    {name:'generation-captured',generation:1,changeGeneration:2,scripts:[{events:[failure()]},{events:ok}]},
    {name:'thrown-network',scripts:[{throw:{name:'TypeError',message:'fetch failed'}},{events:ok}]},
    // Thrown classification (timeout/aborted/network_error) and one case per
    // retryable needle. Fallback is disabled so the canonical error itself,
    // including code and retryable, is observable in the trace.
    {name:'thrown-timeout-named',config:{fallback:{}},scripts:[{throw:{name:'TimeoutError',message:'boom'}}]},
    {name:'thrown-timeout-message',config:{fallback:{}},scripts:[{throw:{name:'Error',message:'request timeout'}}]},
    {name:'thrown-aborted-named',config:{fallback:{}},scripts:[{throw:{name:'AbortError',message:'boom'}}]},
    {name:'thrown-aborted-message',config:{fallback:{}},scripts:[{throw:{name:'Error',message:'operation aborted'}}]},
    ...[['econnreset','read ECONNRESET'],['econnrefused','connect ECONNREFUSED'],['etimedout','connect ETIMEDOUT'],['epipe','write EPIPE'],['socket-hang-up','socket hang up'],['network','network down'],['dns','getaddrinfo ENOTFOUND dns']].map(([needle,message])=>({name:`thrown-needle-${needle}`,config:{fallback:{}},scripts:[{throw:{name:'Error',message}}]})),
    {name:'thrown-unmatched',config:{fallback:{}},scripts:[{throw:{name:'TypeError',message:'boom'}}]},
    {name:'request-error-details',scripts:[{throw:{name:'ModelRequestError',code:'too_many_images',message:'Image count exceeds model limits.',details:{imageCount:2,maxImagesPerRequest:1}}}]},
    {name:'request-error-no-details',scripts:[{throw:{name:'ModelRequestError',code:'invalid_speed',message:'speed must be between 0 and 1.'}}]},
    {name:'media-lone-surrogate',media:{a:['text'],b:['text']},request:{messages:[{role:'user',content:[{type:'image',mimeType:'image/\ud800',bytes:1536}]}]},scripts:[{events:ok}]},
    {name:'policy-becomes-empty',policyFlips:true,scripts:[]},
    {name:'policy-empty-lone-surrogate',policyFlips:true,decision:{provider:'a\ud800',model:'m\ud800'},scripts:[]},
    {name:'http-429-fallback',replies:[httpError(429),success]},
    {name:'http-503-fallback',replies:[httpError(503),success]},
    {name:'http-refused-fallback',replies:[{fault:'ECONNREFUSED'},success]},
    {name:'http-disconnect-after-content',replies:[{chunks:[start,text],end:'destroy'}]},
    {name:'http-timeout-fallback',streamTimeoutMs:80,replies:[{chunks:[start],end:'hold'},success]},
    {name:'http-cancel',abortAfterText:true,replies:[{chunks:[start,text],end:'hold'}]},
    {name:'http-final-503',replies:[httpError(503),httpError(503)]},
  ];
  const executeCases=[];
  const transportHeaders=['host','connection','content-length','user-agent','accept','accept-language','sec-fetch-mode','accept-encoding','transfer-encoding','keep-alive'];
  for(const spec of specs.filter(spec=>Boolean(spec.replies)===httpOnly)) {
    const trace:any[]=[]; const calls:any[]=[]; const requests:any[]=[]; const controller=new AbortController();
    let generation=spec.generation; let scriptIndex=0; let replyIndex=0; let sessionUsage:any;
    const health=createNativeRouterProviderHealthPort({now:()=>0});
    for(const providerId of spec.seedOpen??[])for(let i=0;i<5;i++)health.recordFailure({sessionId:'s',providerId});
    const healthPort=Object.fromEntries(['recordSuccess','recordFailure','shouldSkip'].map(action=>[action,(input:any)=>{
      const result=health[action](input);trace.push({kind:'health',action,input,...(action==='shouldSkip'?{result}:{})});return result;
    }]));
    const providers=Object.fromEntries(['a','b','c'].map(id=>[id,{id,protocol:'anthropic',url:`http://${id}.test/v1`,apiKey:'fixture-key',retry:{streamMaxRetries:0},models:{m:{id:'m',capabilities:{supportsStreaming:true,supportsToolUse:true,supportsSystemPrompt:true,maxOutputTokens:64},multimodal:{input:['text','image']}}}}]));
    let origin='';let server:any;let closedPort=0;
    if(spec.replies) {
      // The official HTTP recorder uses the same loopback strategy as model-stream.
      // All sockets are owned by this run; no external address is contacted.
      server=createServer(async(req,res)=>{
        let body='';for await(const chunk of req)body+=chunk;
        const reply=spec.replies[replyIndex++];assert.ok(reply,`unexpected HTTP request ${spec.name}`);
        const received:any={method:req.method,path:req.url,headers:Object.entries(req.headers).filter(([key])=>!transportHeaders.includes(key)).sort(([a],[b])=>a.localeCompare(b)),body};requests.push(received);
        if(reply.end==='hold'){received.clientClosed=false;res.once('close',()=>received.clientClosed=true);}
        res.writeHead(reply.status??200,{'content-type':'text/event-stream',...reply.headers});
        for(const chunk of reply.chunks??[]){res.write(chunk);await new Promise(resolve=>setImmediate(resolve));}
        if(reply.end==='destroy'){res.flushHeaders();setTimeout(()=>res.destroy(),10);}
        else if(reply.end!=='hold')res.end();
      });
      await new Promise<void>((resolve,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',resolve)});
      origin=`http://127.0.0.1:${server.address().port}`;
      for(const id of ['a','b','c'])providers[id].url=`${origin}/${id}/v1`;
      if(spec.streamTimeoutMs)providers.a.retry.streamIdleTimeoutMs=spec.streamTimeoutMs;
      if(spec.replies[0]?.fault){
        const closed=createNetServer();await new Promise<void>((resolve,reject)=>{closed.once('error',reject);closed.listen(0,'127.0.0.1',resolve)});closedPort=(closed.address() as any).port;
        await new Promise<void>((resolve,reject)=>closed.close(error=>error?reject(error):resolve()));
        providers.a.url=`http://127.0.0.1:${closedPort}/a/v1`;replyIndex++;
      }
    }
    const invoker={
      getCapabilities:()=>({maxOutputTokens:64}),getMultimodal:(p:string)=>({input:spec.media?.[p]??['text','image','pdf','audio']}),
      getProviderProtocol:()=> 'anthropic',getProviderBaseUrl:()=>undefined,getProviderGeneration:()=>generation,
      async *stream(req:any,options:any) {
        calls.push(req);
        if(spec.replies){yield* streamModel(req,{providers},options);return;}
        const script=spec.scripts[scriptIndex++];assert.ok(script,`unexpected invocation ${spec.name}`);
        if(spec.changeGeneration!==undefined)generation=spec.changeGeneration;
        for(const event of script.events??[])yield event;
        if(script.throw){if(script.throw.name==='ModelRequestError')throw new ModelRequestError(script.throw.code,script.throw.message,script.throw.details);throw Object.assign(new Error(script.throw.message),{name:script.throw.name});}
      },
    };
    let policyReads=0;
    const router=createRouterRuntime({...config,...spec.config},{modelInvoker:invoker,isModelAllowed:spec.policyFlips?()=>++policyReads===1:undefined,providerHealth:healthPort,now:()=>new Date(0),events:{emit:(value:any)=>trace.push({kind:'router',value})},usageObserver:{stats:{},getSessionUsage:()=>sessionUsage,observeSessionUsage:(_id:any,value:any)=>{sessionUsage=value},observeRequest:()=>{},dispose:()=>{}}});
    if(spec.abortBefore)controller.abort();
    try {
      for await(const value of router.execute({...decision,...spec.decision},{...request,...spec.request},{sessionId:'s',turnId:'t',abortSignal:controller.signal,fallbackModels:spec.fallbackModels,managedModelPolicy:spec.managedModelPolicy})) {
        trace.push({kind:'event',value});if(spec.abortAfterText&&value.type==='text_delta')controller.abort();
      }
    } catch(error:any) {trace.push({kind:'thrown',value:{name:error.name,message:error.message,...(error.code?{code:error.code}:{})}});}
    if(spec.replies)assert.equal(replyIndex,spec.replies.length,spec.name+JSON.stringify(trace));else assert.equal(scriptIndex,spec.scripts.length,spec.name);
    if(spec.replies){
      assert.equal(requests.length,spec.replies.filter((reply:any)=>!reply.fault).length);
      assert.equal(calls.length,spec.replies.length);
      const deadline=Date.now()+2000;
      while(requests.some(request=>request.clientClosed===false)&&Date.now()<deadline)await new Promise(resolve=>setTimeout(resolve,5));
      assert.ok(requests.every(request=>request.clientClosed!==false),'held response must close');
    }
    const serialized=JSON.stringify({spec,calls,requests,trace,sessionUsage});
    executeCases.push(JSON.parse(origin?serialized.replaceAll(origin,'$MOCK').replaceAll(`http://127.0.0.1:${closedPort}`,'$CLOSED'):serialized));
    if(server){server.closeAllConnections();await new Promise<void>((resolve,reject)=>server.close((error:any)=>error?reject(error):resolve()));}
  }
  // Real provider validation, before any transport can run. These are not
  // scripted ModelRequestErrors: details come from the fixed TS implementation.
  const image={type:'image',source:'base64',mimeType:'image/png',data:'AA==',bytes:2048};
  const validationSpecs=[
    {name:'content-unsupported-modality',multimodal:{input:['text']},content:[image]},
    {name:'unsupported-image-mime-type',multimodal:{input:['image'],supportedImageMimeTypes:['image/jpeg']},content:[image]},
    {name:'image-too-large',multimodal:{input:['image'],maxImageBytes:1024},content:[image]},
    {name:'pdf-too-large',multimodal:{input:['pdf'],maxPdfBytes:4096},content:[{type:'pdf',source:'base64',data:'AA==',bytes:8192}]},
    {name:'audio-too-long',multimodal:{input:['audio'],maxAudioSeconds:30},content:[{type:'audio',source:'base64',mimeType:'audio/wav',data:'AA==',durationSeconds:45}]},
    ...['anthropic','openai','openai-responses'].map(protocol=>({name:`unsupported-thinking-${protocol}`,protocol,thinking:{state:'enabled',efforts:['low']},requestThinking:{mode:'high'},content:[{type:'text',text:'hello'}]})),
    // Disabled Google thinking rejects even an unset request mode. The
    // enabled case rejects an unconfigured effort before client construction.
    {name:'unsupported-thinking-google-unset',protocol:'google',thinking:{state:'disabled',efforts:['low']},content:[{type:'text',text:'hello'}]},
    {name:'unsupported-thinking-google-set',protocol:'google',thinking:{state:'enabled',efforts:['low']},requestThinking:{mode:'high'},content:[{type:'text',text:'hello'}]},
  ];
  const validationCases=[];
  for(const spec of validationSpecs) {
    const model={id:'m',capabilities:{supportsStreaming:true,supportsToolUse:true,supportsSystemPrompt:true,maxOutputTokens:64},multimodal:spec.multimodal??{input:['text']},...(spec.thinking?{thinking:spec.thinking}:{})};
    const provider={id:'a',protocol:spec.protocol??'anthropic',url:'http://a.test/v1',apiKey:'fixture-key',models:{m:model}};
    const req={...request,messages:[{role:'user',content:spec.content}],...(spec.requestThinking?{thinking:spec.requestThinking}:{})};
    let error;const events=[];
    try {
      for await (const event of streamModel(req,{providers:{a:provider}},{fetch:async()=>{assert.fail('validation must precede transport')},googleClientFactory:()=>{assert.fail('validation must precede Google client')}}))events.push(event);
    } catch(thrown:any) {
      if(spec.protocol==='google') {
        // streamModel projects Google build errors to a canonical provider error.
        assert.equal(thrown.code,'unsupported_thinking',spec.name);
        error=thrown;
      } else {
        assert.ok(thrown instanceof ModelRequestError, spec.name);
        error={name:thrown.name,code:thrown.code,message:thrown.message,details:thrown.details};
      }
    }
    if(spec.protocol==='google'&&!error)error=events.find(event=>event.type==='error')?.error;
    assert.ok(error,spec.name);
    if(spec.protocol==='google')assert.equal(error.code,'unsupported_thinking',spec.name);
    validationCases.push({spec,request:req,config:{providers:{a:provider}},events,error});
  }
  Math.random=originalRandom;
  assert.equal(healthCases.length,4);assert.equal(portActions.length,17);assert.equal(eligibility.length,60);assert.equal(executeCases.length,httpOnly?7:46);
  return {healthCases,portActions,eligibility,executeCases,transportHeaders,validationCases};
}
