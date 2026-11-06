// #1370 bounded executable probe for the controlled storage stimulus. Real
// binary, real WS client, loopback provider; every scenario owns an isolated
// home/project and all ports are OS-assigned. The observer also acts as the
// collector: it arms stimulus.json, watches the witness receipts and writes
// release.json (atomically, tmp + rename) exactly like the #1368 collector.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {mkdir, readFile, writeFile, readdir, rename, stat} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {createHash} from 'node:crypto';
import {createRequire} from 'node:module';
import {once} from 'node:events';
const require = createRequire(join(process.env.PDRS_BASELINE!, 'package.json'));
const WebSocket = require('ws');
assert.equal(process.version,'v22.22.0','#1370 pinned Node');
const [binary,sidecar,out] = process.argv.slice(2).map(p=>resolve(p));
await mkdir(out,{recursive:true});
let assertionFailure:string|null=null;
const rows:any[]=[]; const children:any[]=[]; const sockets=new Set<any>();const clients:any[]=[];
const overallDeadline=Date.now()+120000;
const sha=async(path:string)=>createHash('sha256').update(await readFile(path)).digest('hex');
const identities={binary:{path:binary,sha256:await sha(binary)},sidecar:{path:sidecar,sha256:await sha(sidecar)}};
const bounded = async <T>(promise:Promise<T>, label:string, ms=15000, cleanup=false):Promise<T> => {
  if(!cleanup){ms=Math.min(ms,overallDeadline-Date.now());if(ms<=0)throw Error('#1370 overall process deadline');}
  let timer:any;try{return await Promise.race([promise,new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(Error('#1370 '+label)),ms);})]);}finally{clearTimeout(timer);}
};
const sleep=(ms:number)=>new Promise<void>(r=>setTimeout(r,ms));
// Slow-provider marker: a request carrying it never answers within the turn's
// 400ms timeout, so the gateway takes the real turn_timeout path. Writes after
// the aborted request are swallowed (the socket died with the abort).
const server=createServer(async(req,res)=>{
  const chunks:Buffer[]=[];for await(const chunk of req)chunks.push(chunk as Buffer);
  const body=JSON.parse(Buffer.concat(chunks).toString());
  rows.push({provider:{stream:body.stream,model:body.model}});
  if(!body.stream){res.setHeader('content-type','application/json');res.end(JSON.stringify({id:'native-title',choices:[{message:{role:'assistant',content:'{"title":"Native storage stimulus probe"}'},finish_reason:'stop'}]}));return;}
  res.setHeader('content-type','text/event-stream');
  const emit=(delta:any,finish_reason:any=null)=>res.write('data: '+JSON.stringify({id:'native-probe',model:'mock',choices:[{index:0,delta,finish_reason}]})+'\n\n');
  if(JSON.stringify(body).includes('stimulus-timeout-marker')){
    setTimeout(()=>{try{emit({role:'assistant',content:'never delivered'});emit({},'stop');res.end('data: [DONE]\n\n');}catch{}},6000);
    return;
  }
  emit({role:'assistant',content:'native storage stimulus text'});
  emit({},'stop');res.end('data: [DONE]\n\n');
});
server.on('connection',s=>{sockets.add(s);s.on('error',()=>{});s.on('close',()=>sockets.delete(s));});
server.on('clientError',(_error,socket)=>{socket.destroy?.();});
await new Promise<void>(r=>server.listen(0,'127.0.0.1',r));const port=(server.address() as any).port;
// #1370: a ready line is the product's `eprintln!("native listening {address}")`
// line and nothing else -- the phrase, exactly one space, the address, then the
// line end. This follows the Rust judge `advertises` in tests/native_process.rs
// on line start / one space / non-empty whitespace-free address / line end, and
// is deliberately stricter in two ways: the address must still carry `host:digits`
// (Rust accepts any whitespace-free token) and only a single CR before the LF is
// tolerated (Rust trims any run of CR/LF). `(?:^|\n)` instead of the `m` flag:
// with `m`, `^` also matches after a bare CR / U+2028 / U+2029, which `advertises`
// (it splits on LF) rejects. No `g` flag on purpose -- `exec` must stay stateless.
// The old `[^\n]*` also accepted `native listening 127.0.0.1:4321 extra`.
const LISTENING_NATIVE=/(?:^|\n)native listening (\S+:\d+)\r?\n/;
async function launch(name:string,stimulus?:string){
  const home=join(out,name,'home'),project=join(out,name,'project');await mkdir(home,{recursive:true});await mkdir(project,{recursive:true});
  await writeFile(join(home,'pilotdeck.yaml'),`schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:${port}/v1\n      apiKey: mock-only\n      models:\n        mock: {}\n`,{flag:'wx'});
  const token=join(home,'token');await writeFile(token,'native-storage-stimulus-fixture\n',{flag:'wx'});
  const args=['--native','--home',home,'--project',project,'--listen','127.0.0.1:0','--token-file',token,'--sidecar',sidecar,'--drain-ms','1500'];
  const env:any={PATH:process.env.PATH,HOME:home,PILOT_HOME:home,LANG:'en_US.UTF-8'};
  if(stimulus!==undefined)env.PILOTDECK_NATIVE_STORAGE_STIMULUS=stimulus;
  const child=spawn(binary,args,{cwd:project,env,stdio:['ignore','pipe','pipe']});children.push(child);
  let stderr='';const exit=once(child,'exit');child.stderr.on('data',(b:any)=>stderr+=b.toString());
  const ready=bounded(new Promise<string>((resolve,reject)=>{
    child.stderr.on('data',()=>{const match=LISTENING_NATIVE.exec(stderr);if(match)resolve(match[1]);});
    child.once('exit',(code:any)=>reject(Error(`#1370 early exit ${code}: ${stderr}`)));child.once('error',reject);
  }),'readiness');
  const address=await ready;
  rows.push({name,pid:child.pid,argv:[binary,...args],address});
  return {child,exit,address,project,home,stderr:()=>stderr};
}
async function client(address:string){
  const ws=new WebSocket(`ws://${address}/ws`);
  clients.push(ws);let failure:Error|undefined;const queue:any[]=[];let wake:(()=>void)|undefined;ws.on('error',(e:Error)=>{failure=e;wake?.();});ws.on('close',()=>{failure??=Error('#1370 WS closed');wake?.();});ws.on('message',(b:any)=>{queue.push(JSON.parse(b));wake?.();wake=undefined;});
  await bounded(new Promise<void>((resolve,reject)=>{ws.once('open',()=>resolve());ws.once('error',reject);}),'WS open');
  const next=()=>bounded((async()=>{while(!queue.length){if(failure)throw failure;await new Promise<void>(r=>wake=r);}const value=queue.shift();rows.push({frame:value});return value;})(),'WS response');
  ws.send(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'ui',clientVersion:'native-storage-stimulus',token:'native-storage-stimulus-fixture'}));assert.equal((await next()).type,'hello_ok','#1370 real authentication');
  return {ws,next};
}
const request=async(ws:any,next:()=>Promise<any>,id:string,method:string,params:any)=>{
  ws.send(JSON.stringify({type:'request',id,method,params}));
  let frame:any;do{frame=await next();}while(frame.id!==id||frame.type!=='response');
  return frame;
};
const transcriptOf=async(process:any)=>{
  const files=(await readdir(process.home,{recursive:true})).filter((p:any)=>p.endsWith('.jsonl'));
  return files.length===1?await readFile(join(process.home,files[0] as string),'utf8'):'';
};
// Collector-side control protocol: atomic stimulus.json / release.json, and a
// bounded poll over the product's witness directory (10ms, like the plan).
const writeAtomic=async(path:string,text:string)=>{const temporary=path+'.tmp';await writeFile(temporary,text);await rename(temporary,path);};
const arm=async(control:string,config:any)=>writeAtomic(join(control,'stimulus.json'),JSON.stringify(config));
const releaseBarrier=async(control:string,seq:number)=>writeAtomic(join(control,'release.json'),JSON.stringify({seq}));
// The product publishes each receipt atomically (`<seq>.json.tmp` + rename,
// stimulus.rs `publish`), so a `.tmp` name is never a finished receipt: a
// crash can leave one half written, and parsing it would fail an otherwise
// clean scenario on truncated JSON. Only the renamed `.json` is visible.
const finishedReceipts=(files:string[])=>files.filter(name=>name.endsWith('.json'));
const witness=async(control:string,site:string,phase:string,key:string)=>{
  const dir=join(control,'witness');
  for(;;){
    const files=finishedReceipts((await readdir(dir).catch(()=>[]as string[])).sort());
    for(const name of files){
      const receipt=JSON.parse(await readFile(join(dir,name),'utf8'));
      if(receipt.site===site&&receipt.phase===phase&&receipt.key===key)return receipt;
    }
    if(Date.now()>overallDeadline)assert.ok(false,`#1370 no ${site} ${phase} receipt for key ${key}`);
    await sleep(10);
  }
};
const witnessCount=async(control:string,site:string,phase:string,key:string)=>{
  const dir=join(control,'witness');
  const files=finishedReceipts((await readdir(dir).catch(()=>[]as string[])).sort());
  let count=0;for(const name of files){
    const receipt=JSON.parse(await readFile(join(dir,name),'utf8'));
    if(receipt.site===site&&receipt.phase===phase&&receipt.key===key)count++;
  }
  return count;
};
const exists=async(path:string)=>stat(path).then(()=>true,()=>false);
try{
  // #1370: the matcher accepts the two line endings the product can print and
  // keeps rejecting everything the Rust `advertises` judge rejects -- trailing
  // content, a missing address, a missing LF, a text prefix, a doubled space, a
  // colon glued to the phrase, a bare CR used as a line start (the `m`-flag trap),
  // and the empty string.
  for(const line of ['native listening 127.0.0.1:4321\n','native listening [::1]:4321\r\n'])assert.ok(LISTENING_NATIVE.exec(line),'#1370 listening line is an advertisement: '+JSON.stringify(line));
  for(const line of ['native listening 127.0.0.1:4321 extra\n','native listening 127.0.0.1:4321 some-diagnostic\n','native listening\n','native listening \n','native listening 127.0.0.1:4321','warn: native listening 127.0.0.1:4321\n','native listening  127.0.0.1:4321\n','native listening: 127.0.0.1:4321\n','warn: x\rnative listening 127.0.0.1:4321\n',''])assert.strictEqual(LISTENING_NATIVE.exec(line),null,'#1370 not an advertisement: '+JSON.stringify(line));
  // Unset: the production default. No receipt, no control reads, the turn and
  // its transcript keep their real behavior.
  {
    const process=await launch('stimulus-unset');
    assert.ok(!process.stderr().includes('native storage stimulus on'),'#1370 unset keeps no stimulus receipt');
    const {ws,next}=await client(process.address);
    ws.send(JSON.stringify({type:'request',id:'submit',method:'submit_turn',params:{sessionKey:'stimulus-unset-session',runId:'stimulus-unset-run',projectKey:process.project,channelKey:'probe',message:'native storage stimulus unset turn',allowedTools:[]}}));
    const events:any[]=[];
    while(true){const frame=await next();assert.equal(frame.type,'event','#1370 unset submit event envelope');assert.equal(frame.id,'submit','#1370 unset submit identity');assert.equal(frame.seq,events.length,'#1370 unset submit sequence');assert.notEqual(frame.event.type,'error','#1370 unset no submit error');
      if(frame.final){assert.deepEqual(frame.event,events.at(-1),'#1370 unset final repeats terminal');break;}events.push(frame.event);}
    assert.equal(events.at(-1).type,'turn_completed','#1370 unset turn completes');
    const disk=await transcriptOf(process);
    assert.ok(disk.split('\n').filter(Boolean).map(s=>JSON.parse(s)).some(e=>e.type==='assistant_message'&&e.message.content.some((b:any)=>b.text==='native storage stimulus text')),'#1370 unset transcript persists');
    rows.push({scenario:'unset'});
    ws.terminate();process.child.kill('SIGTERM');const status=await bounded(process.exit,'unset signal drain');
    assert.deepEqual(status,[0,null],'#1370 unset clean signal exit');rows.push({scenario:'unset',exit:status,stderr:process.stderr()});
  }
  // Armed with an existing control directory that carries no arms: the
  // construction receipt is on stderr and nothing else changes.
  {
    const control=join(out,'stimulus-armed-empty','control');await mkdir(control,{recursive:true});
    const process=await launch('stimulus-armed-empty',control);
    assert.ok(process.stderr().includes(`native storage stimulus on ${control}`),'#1370 stimulus receipt on stderr');
    const {ws,next}=await client(process.address);
    ws.send(JSON.stringify({type:'request',id:'submit',method:'submit_turn',params:{sessionKey:'stimulus-armed-session',runId:'stimulus-armed-run',projectKey:process.project,channelKey:'probe',message:'native storage stimulus armed turn',allowedTools:[]}}));
    const events:any[]=[];
    while(true){const frame=await next();assert.equal(frame.type,'event','#1370 armed submit event envelope');assert.equal(frame.id,'submit','#1370 armed submit identity');assert.equal(frame.seq,events.length,'#1370 armed submit sequence');assert.notEqual(frame.event.type,'error','#1370 armed no submit error');
      if(frame.final){break;}events.push(frame.event);}
    assert.equal(events.at(-1).type,'turn_completed','#1370 armed empty control turns normally');
    const disk=await transcriptOf(process);
    assert.ok(disk.split('\n').filter(Boolean).map(s=>JSON.parse(s)).some(e=>e.type==='assistant_message'&&e.message.content.some((b:any)=>b.text==='native storage stimulus text')),'#1370 armed transcript persists');
    assert.equal(await exists(join(control,'witness')),false,'#1370 armed empty control publishes no receipts');
    rows.push({scenario:'armed-empty'});
    ws.terminate();process.child.kill('SIGTERM');const status=await bounded(process.exit,'armed signal drain');
    assert.deepEqual(status,[0,null],'#1370 armed clean signal exit');rows.push({scenario:'armed-empty',exit:status,stderr:process.stderr()});
  }
  // Invalid: a value that does not name an existing directory fails closed at
  // startup, before any listening advertisement.
  {
    const home=join(out,'stimulus-invalid','home'),project=join(out,'stimulus-invalid','project');
    await mkdir(home,{recursive:true});await mkdir(project,{recursive:true});
    await writeFile(join(home,'pilotdeck.yaml'),`schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:${port}/v1\n      apiKey: mock-only\n      models:\n        mock: {}\n`,{flag:'wx'});
    const token=join(home,'token');await writeFile(token,'native-storage-stimulus-fixture\n',{flag:'wx'});
    const child=spawn(binary,['--native','--home',home,'--project',project,'--listen','127.0.0.1:0','--token-file',token,'--sidecar',sidecar,'--drain-ms','1500'],{cwd:project,env:{PATH:process.env.PATH,HOME:home,PILOT_HOME:home,LANG:'en_US.UTF-8',PILOTDECK_NATIVE_STORAGE_STIMULUS:join(out,'stimulus-invalid','missing-control')},stdio:['ignore','pipe','pipe']});children.push(child);
    let stderr='';child.stderr.on('data',b=>stderr+=b);
    // A mutated gate that lets the process start listening never exits on its
    // own: the bounded wait then returns null, and the classified assertion
    // (not a deadline error) is what the collector records.
    const status=await bounded(once(child,'exit'),'invalid startup').catch(()=>null);
    assert.ok(status!==null&&status[0]!==0,'#1370 invalid stimulus dir rejected');
    assert.ok(stderr.includes('existing directory'),'#1370 invalid stimulus dir reports the gate');
    assert.ok(!stderr.includes('native listening'),'#1370 invalid stimulus dir never advertises ready');
    rows.push({scenario:'invalid',status,stderr});
  }
  // Real replay: a 400ms timeout turn against a slow provider. The writer
  // barrier holds the turn_timeout status write, the registry barrier holds
  // the agent_status persist inside its lock, and run_get queues behind it.
  // With durable storage held, the visible stream closes before the held
  // events become durable, so the terminal frame is the real
  // "stream ended without a terminal result" error, not turn_completed.
  {
    const control=join(out,'stimulus-replay','control');await mkdir(control,{recursive:true});
    await arm(control,{writer:{barrier:{id:'w1',key:'turn_timeout',boundMs:8000}},registry:{barrier:{id:'r1',key:'agent_status:turn_timeout',boundMs:8000}}});
    const process=await launch('stimulus-replay',control);
    assert.ok(process.stderr().includes(`native storage stimulus on ${control}`),'#1370 replay stimulus receipt on stderr');
    const {ws,next}=await client(process.address);
    const frames:any[]=[];
    // Keep draining every frame for the whole scenario: the submit final frame
    // arrives while the barriers still hold, and the run_get response arrives
    // after the release — a collector that stops at final would miss it.
    const drain=(async()=>{for(;;){frames.push(await next());}})();
    drain.catch(()=>{});
    ws.send(JSON.stringify({type:'request',id:'submit',method:'submit_turn',params:{sessionKey:'stimulus-replay-session',runId:'stimulus-replay-run',projectKey:process.project,channelKey:'probe',message:'stimulus-timeout-marker native storage stimulus replay turn',allowedTools:[],timeoutMs:400}}));
    // Writer site: the underlying append of the turn_timeout status is held,
    // and the aborted turn's own result write queues behind it (FIFO witness).
    const writerBlock=await witness(control,'writer','block','turn_timeout');
    assert.equal(writerBlock.boundMs,8000,'#1370 writer block receipt states the bound');
    assert.ok(!(await transcriptOf(process)).includes('turn_timeout'),'#1370 transcript stays held before the release');
    await witness(control,'writer','queued','turn_result');
    await releaseBarrier(control,writerBlock.seq);
    await witness(control,'writer','complete','turn_timeout');
    assert.ok((await transcriptOf(process)).includes('turn_timeout'),'#1370 writer release lands the status write');
    // Registry site: the agent_status persist is held inside the lock.
    const registryBlock=await witness(control,'registry','block','agent_status:turn_timeout');
    const runsPath=join(process.home,'runs.json');
    assert.ok(!((await exists(runsPath))&&(await readFile(runsPath,'utf8')).includes('turn_timeout')),'#1370 runs.json stays held before the release');
    ws.send(JSON.stringify({type:'request',id:'runget',method:'run_get',params:{projectKey:process.project,sessionKey:'stimulus-replay-session',runId:'stimulus-replay-run'}}));
    await sleep(700);
    assert.ok(!frames.some(f=>f.id==='runget'&&f.type==='response'),'#1370 run_get held behind the registry barrier');
    await releaseBarrier(control,registryBlock.seq);
    await bounded((async()=>{while(!frames.some(f=>f.id==='runget'&&f.type==='response'))await sleep(10);})(),'run_get after release');
    await witness(control,'registry','complete','agent_status:turn_timeout');
    const registryRead=frames.find(f=>f.id==='runget'&&f.type==='response');
    assert.equal(registryRead.ok,true,'#1370 run_get answers after the release');
    assert.ok((await readFile(runsPath,'utf8')).includes('turn_timeout'),'#1370 runs.json lands the timed-out agent_status');
    await bounded((async()=>{while(!frames.some(f=>f.id==='submit'&&f.final))await sleep(10);})(),'replay submit final frame');
    const submitFrames=frames.filter(f=>f.id==='submit'&&f.type==='event');
    assert.deepEqual(submitFrames.map(f=>f.seq),submitFrames.map((_,i)=>i),'#1370 replay submit sequence');
    const terminal=frames.find(f=>f.id==='submit'&&f.final);
    assert.equal(terminal.event.type,'error','#1370 timeout turn final frame');
    assert.equal(await witnessCount(control,'writer','block','turn_timeout'),1,'#1370 single writer block for the replay');
    rows.push({scenario:'replay'});
    ws.terminate();process.child.kill('SIGTERM');const status=await bounded(process.exit,'replay signal drain');
    assert.deepEqual(status,[0,null],'#1370 replay clean signal exit');rows.push({scenario:'replay',exit:status,stderr:process.stderr()});
  }
 }catch(error:any){
  // Only an actual named assertion can become a target receipt. A network
  // error, process failure or deadline remains an unclassified INVALID.
  if(error?.code==='ERR_ASSERTION'){
    const first=String(error.message).split('\n')[0];
    if(['#1370 unset keeps no stimulus receipt','#1370 unset no submit error','#1370 unset turn completes','#1370 unset transcript persists','#1370 unset clean signal exit','#1370 stimulus receipt on stderr','#1370 armed empty control turns normally','#1370 armed transcript persists','#1370 armed empty control publishes no receipts','#1370 armed clean signal exit','#1370 invalid stimulus dir rejected','#1370 invalid stimulus dir reports the gate','#1370 invalid stimulus dir never advertises ready','#1370 replay stimulus receipt on stderr','#1370 writer block receipt states the bound','#1370 transcript stays held before the release','#1370 no writer block receipt for key turn_timeout','#1370 no writer queued receipt for key turn_result','#1370 no writer complete receipt for key turn_timeout','#1370 writer release lands the status write','#1370 no registry block receipt for key agent_status:turn_timeout','#1370 runs.json stays held before the release','#1370 run_get held behind the registry barrier','#1370 run_get answers after the release','#1370 no registry complete receipt for key agent_status:turn_timeout','#1370 runs.json lands the timed-out agent_status','#1370 timeout turn final frame','#1370 replay clean signal exit','#1370 single writer block for the replay'].includes(first)) assertionFailure=first;
  }
  throw error;
}finally{
  for(const ws of clients)ws.terminate();
  for(const child of children)if(child.exitCode===null&&child.signalCode===null){child.kill('SIGKILL');await bounded(once(child,'exit'),'forced cleanup',5000,true).catch(()=>{});}
  for(const socket of sockets)socket.destroy();await new Promise<void>(r=>server.close(()=>r()));
  const after={binary:await sha(binary),sidecar:await sha(sidecar)};
  assert.equal(after.binary,identities.binary.sha256,'#1370 binary unchanged');assert.equal(after.sidecar,identities.sidecar.sha256,'#1370 sidecar unchanged');
  await writeFile(join(out,'process-record.json'),JSON.stringify({node:process.version,...identities,after,rows,assertionFailure},null,2)+'\n',{flag:'wx'});
}
console.log('#1370 native storage stimulus unset, armed, invalid and replay scenarios passed');
