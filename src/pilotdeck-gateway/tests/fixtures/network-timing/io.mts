// Real loopback I/O. The gate holds callbacks already produced by Undici's
// socket transport, never a timer pretending to be a ready exchange.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {pathToFileURL} from 'node:url';
export async function ioCell(spec:any, root:string) {
  const {Agent,fetch,setGlobalDispatcher,getGlobalDispatcher} = await import(pathToFileURL(`${root}/node_modules/undici/index.js`).href);
  const {networkFetch} = await import(pathToFileURL(`${root}/src/network/fetch.ts`).href);
  const {McpClient} = await import(pathToFileURL(`${root}/src/mcp/client/McpClient.ts`).href);
  const {HttpHookExecutor} = await import(pathToFileURL(`${root}/src/extension/hooks/execution/HttpHookExecutor.ts`).href);
  const {BackgroundSubagentRuntime} = await import(pathToFileURL(`${root}/src/agent/sub/BackgroundSubagentRuntime.ts`).href);
  const {getURLMarkdownContent} = await import(pathToFileURL(`${root}/src/tool/builtin/web/urlFetcher.ts`).href);
  const trace:string[] = [], requests:string[] = [], queue:(()=>any)[] = [];
  const parent = new AbortController(), agent = new Agent({connections:1});
  const previous = getGlobalDispatcher(), realTimer=globalThis.setTimeout;
  globalThis.setTimeout=((fn:any,ms:number,...args:any[])=>{if(ms===500)trace.push('retry-armed');return realTimer(fn,ms,...args);}) as any;
  let observed!:()=>void, held = true, targetSeen = false, background:any, task:any, stopped:boolean|null=null;
  const reached = new Promise<void>(resolve=>{observed=resolve;});
  const server = createServer((req,res)=>{
    trace.push('peer-request');
    if (spec.event === 'idle') {targetSeen=true;observed();return;}
    if (spec.event === 'rejected') {req.socket.end('not an HTTP response\r\n');return;}
    res.writeHead(200,{'content-type':'text/plain','content-length':'5'});res.end('FIRST');
  });
  await new Promise<void>((resolve,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',resolve);});
  const port = (server.address() as any).port;
  const origin = `http://127.0.0.1:${port}`;
  const dispatcher={dispatch(opts:any,handler:any){
    assert.equal(String(opts.origin),'https://fixture.test','loopback routing boundary');
    requests.push(opts.path);
    return agent.dispatch({...opts,origin},new Proxy(handler,{get(target,key){
      const fn=Reflect.get(target,key);
      if(typeof fn!=='function') return fn;
      return (...args:any[])=>{
        const callback=String(key);
        if(callback==='onRequestStart') return fn.apply(target,args);
        if(callback==='onResponseStart'||callback==='onResponseError') {
          trace.push(`wire:${callback}`);
          if(!targetSeen){targetSeen=true;observed();}
        }
        if(held){queue.push(()=>fn.apply(target,args));return;}
        return fn.apply(target,args);
      };
    }}));
  }};
  const release=()=>{trace.push('release');held=false;for(const f of queue.splice(0)) f();};
  const abort=()=>{trace.push('parent');if(background)stopped=background.stop(task.taskId);else parent.abort();};
  const bounded = async <T>(promise:Promise<T>) => {
    let timer:any;
    try {return await Promise.race([promise,new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(new Error(`I/O probe deadline: ${spec.id}`)),3000);})]);}
    finally {clearTimeout(timer);}
  };
  const plain=async(signal:AbortSignal)=>{
    if(spec.path==='web_fetch') return (await getURLMarkdownContent('https://fixture.test/start',signal,{cache:{get(){},set(){}}} as any)).content;
    if(spec.path.startsWith('mcp_')) {
      const client=new McpClient({id:'timing',transport:'streamable_http',url:'https://fixture.test/start'} as any);
      return (await (client as any).buildTransport()._fetch('https://fixture.test/start',{method:spec.path==='mcp_post'?'POST':'GET',signal})).text();
    }
    if(spec.path==='hooks') return new HttpHookExecutor(fetch as any).execute({hook:{type:'http',url:'https://fixture.test/start'},hookInput:{} as any,signal});
    const response=spec.path==='background'
      ? await networkFetch('https://fixture.test/start',{signal},{timeoutMs:60000,retry:{maxRetries:1,baseDelayMs:500,maxDelayMs:5000}})
      : await fetch('https://fixture.test/start',{signal});
    return response.text();
  };
  let result:any;
  try {
    setGlobalDispatcher(dispatcher);
    const child=async(signal:AbortSignal)=>{
      try {const content=await plain(signal); result=spec.path==='hooks'
        ? {name:null,message:content.stderr||null,delivery:'return',content:content.stdout,hook:content}
        : {name:null,message:null,delivery:'ok',content};}
      catch(error:any){result={name:error.name,message:error.message,delivery:'throw',content:null};if(background)throw error;}
      finally{trace.push('child-settled');}
    };
    let operation:Promise<any>;
    if(spec.path==='background') {
      background=new BackgroundSubagentRuntime();
      task=background.start({sessionId:'session',parentTurnId:'turn',subagentId:'child',subagentType:'test',run:child});
      operation=(background as any).entries.get(task.taskId).done;
    } else operation=child(parent.signal);
    await bounded(reached);
    assert.ok(targetSeen,'real I/O gate reached');
    trace.push('gate');
    if(spec.order==='continuation-first') {
      release();
      if(spec.event==='fulfilled') await bounded(operation);
      else await new Promise<void>(resolve=>setImmediate(resolve)); // drain the real rejection continuation into retry
      trace.push(spec.event==='idle'?'pending-observed':'continuations-drained');abort();
    } else if(spec.order==='tie-parent-last') {release();abort();}
    else {abort();if(spec.order==='parent-first'){await new Promise<void>(resolve=>setImmediate(resolve));trace.push('parent-continuations-drained');}release();}
    await bounded(operation);
    if(background) result.task={status:task.status,error:task.error??null,ended:!!task.endedAt,stopped};
    return {...spec,requests,observable:{...result,calls:requests.length,retries:requests.filter(p=>p==='/start').length-1},io:{transport:'loopback-undici',gate:spec.event==='idle'?'peer-request':spec.event==='fulfilled'?'onResponseStart':'onResponseError',order:spec.order},trace};
  } finally {
    globalThis.setTimeout=realTimer;setGlobalDispatcher(previous);held=false;
    await agent.destroy();server.closeAllConnections();await new Promise<void>(resolve=>server.close(()=>resolve()));
  }
}
