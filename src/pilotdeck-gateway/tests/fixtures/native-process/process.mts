// #1071 bounded executable probe. All ports are OS-assigned and all state is isolated.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer, request} from 'node:http';
import {mkdir, readFile, writeFile, readdir, appendFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {createHash} from 'node:crypto';
import {createRequire} from 'node:module';
import {once} from 'node:events';
const require = createRequire(join(process.env.PDRS_BASELINE!, 'package.json'));
const WebSocket = require('ws');
assert.equal(process.version,'v22.22.0','#1071 pinned Node');
const [binary,sidecar,out] = process.argv.slice(2).map(p=>resolve(p));
await mkdir(out,{recursive:true});
let assertionFailure:string|null=null;
const rows:any[]=[]; const children:any[]=[]; const sockets=new Set<any>();const clients:any[]=[];
let releaseHeld:(()=>void)|undefined;
let heldClosed:Promise<void>|undefined;
const overallDeadline=Date.now()+90000;
const sha=async(path:string)=>createHash('sha256').update(await readFile(path)).digest('hex');
const identities={binary:{path:binary,sha256:await sha(binary)},sidecar:{path:sidecar,sha256:await sha(sidecar)}};
const bounded = async <T>(promise:Promise<T>, label:string, ms=15000, cleanup=false):Promise<T> => {
  if(!cleanup){ms=Math.min(ms,overallDeadline-Date.now());if(ms<=0)throw Error('#1071 overall process deadline');}
  let timer:any;try{return await Promise.race([promise,new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(Error('#1071 '+label)),ms);})]);}finally{clearTimeout(timer);}
};
const server=createServer(async(req,res)=>{
  let bytes=Buffer.alloc(0);for await(const chunk of req)bytes=Buffer.concat([bytes,chunk]);
  if(req.url==='/bytes'){rows.push({a4Request:bytes.toString('hex')});res.writeHead(201,{'content-type':'application/octet-stream'});res.end(Buffer.from([0,255,1,128,13,10]));return;}
  const body=JSON.parse(bytes.toString());rows.push({provider:{stream:body.stream,model:body.model,messages:body.messages,tools:body.tools}});
  if(!body.stream){res.setHeader('content-type','application/json');res.end(JSON.stringify({id:'native-title',choices:[{message:{role:'assistant',content:'{"title":"Native process probe"}'},finish_reason:'stop'}]}));return;}
  res.setHeader('content-type','text/event-stream');
  const emit=(delta:any,finish_reason:any=null)=>res.write('data: '+JSON.stringify({id:'native-probe',model:'mock',choices:[{index:0,delta,finish_reason}]})+'\n\n');
  if(JSON.stringify(body.messages).includes('NATIVE_TOOL')&&!body.messages.some((m:any)=>m.role==='tool')){
    emit({tool_calls:[{index:0,id:'native-read',type:'function',function:{name:'read_file',arguments:JSON.stringify({file_path:'probe.txt'})}}]});emit({},'tool_calls');res.end('data: [DONE]\n\n');return;
  }
  emit({role:'assistant',content:'native process text'});
  if(JSON.stringify(body.messages).includes('NATIVE_HANG')){rows.push({held:true});releaseHeld=()=>{emit({},'stop');res.end('data: [DONE]\n\n');};heldClosed=new Promise<void>(resolve=>res.once('close',()=>{rows.push({heldClosed:true});resolve();}));return;}
  emit({},'stop');res.end('data: [DONE]\n\n');
});
server.on('connection',s=>{sockets.add(s);s.on('close',()=>sockets.delete(s));});
await new Promise<void>(r=>server.listen(0,'127.0.0.1',r));const port=(server.address() as any).port;
// Both entry paths publish their kernel-assigned listening address (#1131
// aligned the A4 proxy with the native process), so every launch asks for
// port 0 and reads the actual address back; no port is reserved and no
// bind-collision retry exists anymore.
async function launch(name:string,extra:string[]=[],native=true){
  const home=join(out,name,'home'),project=join(out,name,'project');await mkdir(home,{recursive:true});await mkdir(project,{recursive:true});
  await writeFile(join(home,'pilotdeck.yaml'),`schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:${port}/v1\n      apiKey: mock-only\n      models:\n        mock: {}\n`,{flag:'wx'});
  const token=join(home,'token');await writeFile(token,'native-process-fixture\n',{flag:'wx'});
  const args=native?['--native','--home',home,'--project',project,'--listen','127.0.0.1:0','--token-file',token,'--sidecar',sidecar,'--drain-ms','1500',...extra]:extra;
  const start=()=>{
    const child=spawn(binary,args,{cwd:project,env:{PATH:process.env.PATH,HOME:home,PILOT_HOME:home,LANG:'en_US.UTF-8',PILOTDECK_CONFIG_PATH:join(out,'must-not-read.yaml'),PILOTDECK_PROXY_LISTEN:'127.0.0.1:0',PILOTDECK_PROXY_UPSTREAM:`http://127.0.0.1:${port}`},stdio:['ignore','pipe','pipe']});children.push(child);
    let stderr='';let stdout=''; const exit=once(child,'exit');child.stderr.on('data',(b:any)=>stderr+=b.toString());child.stdout.on('data',(b:any)=>stdout+=b.toString());
    // The native process reports "native listening host:port" on stderr;
    // the A4 proxy reports its actual address on stdout (#1131). A child's
    // output arrives in arbitrarily split chunks (the binary writes its
    // readiness line through several write calls), so only a complete line
    // up to its newline can resolve readiness; a truncated chunk must keep
    // waiting instead of resolving a port-less address whose later URL
    // would fall through to http port 80.
    const readiness=native
      ?{stream:child.stderr,text:()=>stderr,pattern:/native listening (\S+:\d+)[^\n]*\n/}
      :{stream:child.stdout,text:()=>stdout,pattern:/PilotDeck server listening: http:\/\/(\S+:\d+)\n/};
    const ready=bounded(new Promise<string>((resolve,reject)=>{
      readiness.stream.on('data',()=>{const match=readiness.pattern.exec(readiness.text());if(match)resolve(match[1]);});
      child.once('exit',(code:any)=>reject(Error(`#1071 early exit ${code}: ${stderr}`)));child.once('error',reject);
    }),'readiness');
    return {child,exit,ready,stderr:()=>stderr};
  };
  const started=start();
  const address=await started.ready;
  // A resolved readiness value must be a complete host:port. A lax regex
  // or a truncated chunk can hand back a port-less fragment whose client
  // URL would silently degrade to the default HTTP port, so reject it
  // here as a classified assertion instead of relying on connect errors.
  assert.ok(/^\d+\.\d+\.\d+\.\d+:\d+$/.test(address),'#1071 readiness address carries a port');
  // Both entries were asked for port 0, so a zero port means the child
  // republished the requested address instead of the kernel-assigned one.
  assert.ok(Number(address.split(':')[1])>0,'#1131 readiness reports the kernel-assigned port');
  rows.push({name,pid:started.child.pid,argv:[binary,...args],cwd:project,address});
  return {child:started.child,exit:started.exit,address,project,home,stderr:started.stderr};
}
async function client(address:string){
  const ws=new WebSocket(`ws://${address}/ws`);
  clients.push(ws);let failure:Error|undefined;const queue:any[]=[];let wake:(()=>void)|undefined;ws.on('error',(e:Error)=>{failure=e;wake?.();});ws.on('close',()=>{failure??=Error('#1071 WS closed');wake?.();});ws.on('message',(b:any)=>{queue.push(JSON.parse(b));wake?.();wake=undefined;});
  // A refused or reset connection must reject the bounded open wait instead
  // of crashing the observer with an 'error' event that has no listener yet.
  await bounded(new Promise<void>((resolve,reject)=>{ws.once('open',()=>resolve());ws.once('error',reject);}),'WS open');
  const next=()=>bounded((async()=>{while(!queue.length){if(failure)throw failure;await new Promise<void>(r=>wake=r);}const value=queue.shift();rows.push({frame:value});return value;})(),'WS response');
  ws.send(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'ui',clientVersion:'native-probe',token:'native-process-fixture'}));assert.equal((await next()).type,'hello_ok','#1071 real authentication');
  return {ws,next};
}
try{
  await writeFile(join(out,'must-not-read.yaml'),'invalid: [',{flag:'wx'});
  for(const signal of ['SIGINT','SIGTERM'] as const){
    const process=await launch(signal);const {ws,next}=await client(process.address);
    if(signal==='SIGINT'){
      ws.send(JSON.stringify({type:'request',id:'catalog',method:'model_catalog_list',params:{projectKey:process.project}}));
      let catalog:any;do{catalog=await next();}while(catalog.id!=='catalog');assert.equal(catalog.ok,true,'#1071 isolated catalog configuration');
      ws.send(JSON.stringify({type:'request',id:'submit',method:'submit_turn',params:{sessionKey:'native-session',runId:'native-run',projectKey:process.project,channelKey:'probe',message:'hello native',allowedTools:[]}}));
      const events:any[]=[];
      while(true){const frame=await next();assert.equal(frame.type,'event','#1071 submit event envelope');assert.equal(frame.id,'submit','#1071 submit identity');assert.equal(frame.seq,events.length,'#1071 submit sequence');assert.notEqual(frame.event.type,'error','#1071 no submit error');
        if(frame.final){assert.deepEqual(frame.event,events.at(-1),'#1071 final repeats terminal');break;}events.push(frame.event);}
      assert.equal(events.at(-1).type,'turn_completed','#1071 successful terminal');assert.equal(events.at(-1).finishReason,'completed','#1071 successful completion reason');
      assert.ok(events.some(e=>e.type==='assistant_text_delta'&&e.text==='native process text'),'#1071 actual model text');
      const calls=rows.filter(r=>r.provider?.stream===true);assert.ok(calls.length>0,'#1071 actual stream provider called');assert.ok(calls.every(r=>!r.provider.tools?.length),'#1071 empty allowedTools enforced');
      ws.send(JSON.stringify({type:'request',id:'read',method:'run_get',params:{projectKey:process.project,sessionKey:'native-session',runId:'native-run'}}));let read:any;do{read=await next();}while(read.id!=='read');assert.equal(read.ok,true,'#1071 persisted run read');assert.equal(read.result.state,'completed','#1071 persisted run terminal');
      const files=(await readdir(process.home,{recursive:true})).filter(p=>p.endsWith('.jsonl'));assert.equal(files.length,1,'#1071 durable session file');const disk=await readFile(join(process.home,files[0]),'utf8');assert.ok(disk.split('\n').filter(Boolean).map(s=>JSON.parse(s)).some(e=>e.type==='assistant_message'&&e.message.content.some((b:any)=>b.text==='native process text')),'#1071 durable assistant text');rows.push({disk:{path:join(process.home,files[0]),sha256:await sha(join(process.home,files[0]))}});
      assert.ok(disk.split('\n').filter(Boolean).map(s=>JSON.parse(s)).some(e=>e.type==='session_metadata'&&e.metadata?.aiTitle==='Native process probe'),'#1071 production title persisted');
      await writeFile(join(process.project,'probe.txt'),'native tool fixture content\n',{flag:'wx'});
      ws.send(JSON.stringify({type:'request',id:'tool',method:'submit_turn',params:{sessionKey:'tool-session',runId:'tool-run',projectKey:process.project,channelKey:'probe',message:'NATIVE_TOOL',allowedTools:['read_file']}}));
      let toolFrame:any;do{toolFrame=await next();assert.notEqual(toolFrame.event?.type,'error','#1071 real tool turn has no host error');}while(!toolFrame.final);
      assert.equal(toolFrame.event.finishReason,'completed','#1071 real tool turn completes');
      assert.ok(rows.some(r=>r.provider?.messages.some((m:any)=>m.role==='tool'&&JSON.stringify(m.content).includes('native tool fixture content'))),'#1071 actual read_file result reaches model');
      const cleanConfig=await readFile(join(process.home,'pilotdeck.yaml'),'utf8');
      await appendFile(join(process.home,'pilotdeck.yaml'),'hooks:\n  PreToolUse:\n    - hooks: [{type: command, command: must-not-run}]\n');
      ws.send(JSON.stringify({type:'request',id:'late-hooks',method:'submit_turn',params:{sessionKey:'late-hooks-session',runId:'late-hooks-run',projectKey:process.project,channelKey:'probe',message:'late hooks'}}));
      let refused:any;do{refused=await next();}while(!refused.final);
      assert.equal(refused.event.type,'error','#1071 refreshed hooks refused');
      assert.equal(refused.event.code,'CAPABILITY_UNAVAILABLE','#1071 refreshed hooks error code');
      await writeFile(join(process.home,'pilotdeck.yaml'),cleanConfig);

    }
    if(signal==='SIGTERM'){
      ws.send(JSON.stringify({type:'request',id:'hang',method:'submit_turn',params:{sessionKey:'cancel-session',runId:'cancel-run',projectKey:process.project,channelKey:'probe',message:'NATIVE_HANG'}}));
      let frame:any;do{frame=await next();assert.notEqual(frame.event?.type,'error','#1071 pending submit enters provider');}while(frame.event?.type!=='assistant_text_delta');
      ws.send(JSON.stringify({type:'request',id:'cancel',method:'abort_turn',params:{sessionKey:'cancel-session',runId:'cancel-run',reason:'native_probe_cancel'}}));
      let final:any,aborted=false;while(!final){frame=await next();if(frame.id==='cancel')assert.equal(frame.ok,true,'#1071 cancellation RPC accepted');if(frame.id==='hang'&&frame.event?.type==='turn_completed'&&frame.event.finishReason==='aborted_streaming')aborted=true;if(frame.id==='hang')assert.notEqual(frame.event?.type,'error','#1071 cancellation has no host error');if(frame.id==='hang'&&frame.final)final=frame;}
      assert.ok(aborted,'#1071 pending model turn aborted');assert.ok(heldClosed,'#1071 held provider response exists');await bounded(heldClosed!,'cancelled provider closed');rows.push({cancelled:true});
    }
    ws.terminate();process.child.kill(signal);const status=await bounded(process.exit,'signal drain');assert.deepEqual(status,[0,null],'#1071 clean signal exit');rows.push({signal,exit:status,stderr:process.stderr()});
  }
  for(const predecessor of ['release','never']){
    releaseHeld=undefined;
    const running=await launch('pending-signals-'+predecessor);const {ws,next}=await client(running.address);
    ws.send(JSON.stringify({type:'request',id:'pending',method:'submit_turn',params:{sessionKey:'pending-session',runId:'pending-run',projectKey:running.project,channelKey:'probe',message:'NATIVE_HANG'}}));
    let frame:any;do{frame=await next();assert.notEqual(frame.event?.type,'error','#1071 pending signal turn starts');}while(frame.event?.type!=='assistant_text_delta');
    ws.pause();const stopping=bounded(new Promise<void>((resolve,reject)=>{running.child.stderr.on('data',()=>{if(running.stderr().includes('native stopping'))resolve();});running.child.once('exit',()=>reject(Error('#1071 exited before stopping observation')));}), 'stop observation');
    assert.ok(running.child.kill('SIGTERM'),'#1071 first signal accepted');await stopping;assert.ok(running.child.kill('SIGINT'),'#1071 repeated signal accepted during peer drain');
    if(predecessor==='release'){assert.equal(typeof releaseHeld,'function','#1071 held provider exists');releaseHeld!();}ws.resume();
    if(predecessor==='release'){do{frame=await next();assert.notEqual(frame.event?.type,'error','#1071 released predecessor succeeds');}while(!frame.final);ws.terminate();}
    const status=await bounded(running.exit,'pending signals drain');rows.push({predecessor,pendingSignals:running.stderr(),status});
    assert.deepEqual(status,[predecessor==='release'?0:1,null],'#1071 pending predecessor determines drain exit');
    if(predecessor==='never')assert.match(running.stderr(),/native WS drain deadline exceeded/,'#1071 never predecessor is an explicit drain error');
    assert.equal(running.stderr().split('native stopping').length-1,1,'#1071 one shutdown start');
  }
  const existingHome=join(out,'SIGINT','home'),existingProject=join(out,'SIGINT','project');
  const valid=['--native','--home',existingHome,'--project',existingProject,'--listen',`127.0.0.1:${port}`,'--token-file',join(existingHome,'token'),'--sidecar',sidecar,'--drain-ms','200'];
  for(const [name,args,extraEnv] of [['missing-options',['--native'],{}],['bind-failure',valid,{}],['ambient-managed',valid,{PILOTDECK_MANAGED_CONFIG:join(out,'must-not-read.yaml')}]] as const){
    const child=spawn(binary,[...args],{cwd:existingProject,env:{PATH:process.env.PATH,HOME:existingHome,...extraEnv},stdio:['ignore','pipe','pipe']});children.push(child);let error='';child.stderr.on('data',b=>error+=b);const status=await bounded(once(child,'exit'),'startup failure');assert.notEqual(status[0],0,'#1071 invalid startup exits nonzero');assert.equal(status[1],null,'#1071 failure does not hang');assert.ok(!error.includes('native listening'),'#1071 failed startup never advertises ready');if(name==='bind-failure')assert.match(error,/address already in use/i,'#1071 bind failure reaches occupied listener');rows.push({startupFailure:name,argv:[binary,...args],pid:child.pid,status,stderr:error});
  }
  const legacy=await launch('a4',[],false);const payload=Buffer.from([255,0,128,13,10,42]);
  const response=await bounded(new Promise<any>((resolve,reject)=>{const req=request(`http://${legacy.address}/bytes`,{method:'POST'},res=>{const chunks:Buffer[]=[];res.on('data',b=>chunks.push(b));res.on('end',()=>resolve({status:res.statusCode,body:Buffer.concat(chunks).toString('hex')}));});req.on('error',reject);req.end(payload);}), 'A4 byte proxy');
  assert.deepEqual(response,{status:201,body:'00ff01800d0a'},'#1071 A4 response bytes');assert.ok(rows.some(r=>r.a4Request===payload.toString('hex')),'#1071 A4 request bytes');legacy.child.kill('SIGINT');assert.deepEqual(await bounded(legacy.exit,'A4 exit'),[0,null],'#1071 A4 clean exit');rows.push({defaultA4:response});
 }catch(error:any){
  // Only an actual named assertion can become a target receipt. A network
  // error, process failure or deadline remains an unclassified INVALID.
  if(error?.code==='ERR_ASSERTION'){
    const first=String(error.message).split('\n')[0];
    if(['#1071 clean signal exit','#1071 refreshed hooks refused','#1071 readiness address carries a port','#1131 readiness reports the kernel-assigned port'].includes(first)) assertionFailure=first;
  }
  throw error;
}finally{
  for(const ws of clients)ws.terminate();
  for(const child of children)if(child.exitCode===null&&child.signalCode===null){child.kill('SIGKILL');await bounded(once(child,'exit'),'forced cleanup',5000,true).catch(()=>{});}
  for(const socket of sockets)socket.destroy();await new Promise<void>(r=>server.close(()=>r()));
  const after={binary:await sha(binary),sidecar:await sha(sidecar)};
  assert.equal(after.binary,identities.binary.sha256,'#1071 binary unchanged');assert.equal(after.sidecar,identities.sidecar.sha256,'#1071 sidecar unchanged');
  await writeFile(join(out,'process-record.json'),JSON.stringify({node:process.version,...identities,after,rows,assertionFailure},null,2)+'\n',{flag:'wx'});
}
console.log('#1071 native process signals, production submit, isolated catalog and default A4 bytes passed');
