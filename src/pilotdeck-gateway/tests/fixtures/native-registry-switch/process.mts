// #1369 bounded executable probe for the controlled registry switch. Real
// binary, real WS client, loopback provider; every scenario owns an isolated
// home/project and all ports are OS-assigned.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {mkdir, readFile, writeFile, readdir, stat} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {createHash} from 'node:crypto';
import {createRequire} from 'node:module';
import {once} from 'node:events';
const require = createRequire(join(process.env.PDRS_BASELINE!, 'package.json'));
const WebSocket = require('ws');
assert.equal(process.version,'v22.22.0','#1369 pinned Node');
const [binary,sidecar,out] = process.argv.slice(2).map(p=>resolve(p));
await mkdir(out,{recursive:true});
let assertionFailure:string|null=null;
const rows:any[]=[]; const children:any[]=[]; const sockets=new Set<any>();const clients:any[]=[];
const overallDeadline=Date.now()+90000;
const sha=async(path:string)=>createHash('sha256').update(await readFile(path)).digest('hex');
const identities={binary:{path:binary,sha256:await sha(binary)},sidecar:{path:sidecar,sha256:await sha(sidecar)}};
const bounded = async <T>(promise:Promise<T>, label:string, ms=15000, cleanup=false):Promise<T> => {
  if(!cleanup){ms=Math.min(ms,overallDeadline-Date.now());if(ms<=0)throw Error('#1369 overall process deadline');}
  let timer:any;try{return await Promise.race([promise,new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(Error('#1369 '+label)),ms);})]);}finally{clearTimeout(timer);}
};
const server=createServer(async(req,res)=>{
  const chunks:Buffer[]=[];for await(const chunk of req)chunks.push(chunk as Buffer);
  const body=JSON.parse(Buffer.concat(chunks).toString());
  rows.push({provider:{stream:body.stream,model:body.model}});
  if(!body.stream){res.setHeader('content-type','application/json');res.end(JSON.stringify({id:'native-title',choices:[{message:{role:'assistant',content:'{"title":"Native registry switch probe"}'},finish_reason:'stop'}]}));return;}
  res.setHeader('content-type','text/event-stream');
  const emit=(delta:any,finish_reason:any=null)=>res.write('data: '+JSON.stringify({id:'native-probe',model:'mock',choices:[{index:0,delta,finish_reason}]})+'\n\n');
  emit({role:'assistant',content:'native registry switch text'});
  emit({},'stop');res.end('data: [DONE]\n\n');
});
server.on('connection',s=>{sockets.add(s);s.on('close',()=>sockets.delete(s));});
await new Promise<void>(r=>server.listen(0,'127.0.0.1',r));const port=(server.address() as any).port;
async function launch(name:string,registry:string){
  const home=join(out,name,'home'),project=join(out,name,'project');await mkdir(home,{recursive:true});await mkdir(project,{recursive:true});
  await writeFile(join(home,'pilotdeck.yaml'),`schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:${port}/v1\n      apiKey: mock-only\n      models:\n        mock: {}\n`,{flag:'wx'});
  const token=join(home,'token');await writeFile(token,'native-registry-switch-fixture\n',{flag:'wx'});
  const args=['--native','--home',home,'--project',project,'--listen','127.0.0.1:0','--token-file',token,'--sidecar',sidecar,'--drain-ms','1500'];
  const child=spawn(binary,args,{cwd:project,env:{PATH:process.env.PATH,HOME:home,PILOT_HOME:home,LANG:'en_US.UTF-8',PILOTDECK_NATIVE_RUN_REGISTRY:registry},stdio:['ignore','pipe','pipe']});children.push(child);
  let stderr='';const exit=once(child,'exit');child.stderr.on('data',(b:any)=>stderr+=b.toString());
  const ready=bounded(new Promise<string>((resolve,reject)=>{
    child.stderr.on('data',()=>{const match=/native listening (\S+:\d+)[^\n]*\n/.exec(stderr);if(match)resolve(match[1]);});
    child.once('exit',(code:any)=>reject(Error(`#1369 early exit ${code}: ${stderr}`)));child.once('error',reject);
  }),'readiness');
  const address=await ready;
  rows.push({name,pid:child.pid,argv:[binary,...args],address});
  return {child,exit,address,project,home,stderr:()=>stderr};
}
async function client(address:string){
  const ws=new WebSocket(`ws://${address}/ws`);
  clients.push(ws);let failure:Error|undefined;const queue:any[]=[];let wake:(()=>void)|undefined;ws.on('error',(e:Error)=>{failure=e;wake?.();});ws.on('close',()=>{failure??=Error('#1369 WS closed');wake?.();});ws.on('message',(b:any)=>{queue.push(JSON.parse(b));wake?.();wake=undefined;});
  await bounded(new Promise<void>((resolve,reject)=>{ws.once('open',()=>resolve());ws.once('error',reject);}),'WS open');
  const next=()=>bounded((async()=>{while(!queue.length){if(failure)throw failure;await new Promise<void>(r=>wake=r);}const value=queue.shift();rows.push({frame:value});return value;})(),'WS response');
  ws.send(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'ui',clientVersion:'native-registry-switch',token:'native-registry-switch-fixture'}));assert.equal((await next()).type,'hello_ok','#1369 real authentication');
  return {ws,next};
}
const request=async(ws:any,next:()=>Promise<any>,id:string,method:string,params:any)=>{
  ws.send(JSON.stringify({type:'request',id,method,params}));
  let frame:any;do{frame=await next();}while(frame.id!==id||frame.type!=='response');
  return frame;
};
const submit=async(ws:any,next:()=>Promise<any>,process:any,sessionKey:string,runId:string,message:string)=>{
  ws.send(JSON.stringify({type:'request',id:'submit',method:'submit_turn',params:{sessionKey,runId,projectKey:process.project,channelKey:'probe',message,allowedTools:[]}}));
  const events:any[]=[];
  while(true){const frame=await next();assert.equal(frame.type,'event','#1369 submit event envelope');assert.equal(frame.id,'submit','#1369 submit identity');assert.equal(frame.seq,events.length,'#1369 submit sequence');assert.notEqual(frame.event.type,'error','#1369 no submit error');
    if(frame.final){assert.deepEqual(frame.event,events.at(-1),'#1369 final repeats terminal');break;}events.push(frame.event);}
  return events;
};
try{
  // off: the registry is absent end to end, but the turn, transcript and
  // signal drain keep their real production behavior.
  {
    const process=await launch('registry-off','off');
    assert.ok(process.stderr().includes('native run registry off'),'#1369 off registry receipt on stderr');
    const {ws,next}=await client(process.address);
    const described=await request(ws,next,'describe','describe_server',{projectKey:process.project});
    assert.equal(described.ok,true,'#1369 off describe_server succeeds');
    for(const capability of ['run_get','run_events','run_reattach'])
      assert.ok(!described.result.capabilities.includes(capability),'#1369 describe_server gates run capabilities');
    const events=await submit(ws,next,process,'registry-off-session','registry-off-run','native registry off');
    assert.equal(events.at(-1).type,'turn_completed','#1369 off turn completes');
    assert.equal(events.at(-1).finishReason,'completed','#1369 off turn completion reason');
    assert.ok(events.some(e=>e.type==='assistant_text_delta'&&e.text==='native registry switch text'),'#1369 off real model text');
    const files=(await readdir(process.home,{recursive:true})).filter(p=>p.endsWith('.jsonl'));assert.equal(files.length,1,'#1369 off durable session file');
    const disk=await readFile(join(process.home,files[0]),'utf8');
    assert.ok(disk.split('\n').filter(Boolean).map(s=>JSON.parse(s)).some(e=>e.type==='assistant_message'&&e.message.content.some((b:any)=>b.text==='native registry switch text')),'#1369 off transcript persists');
    assert.equal(await stat(join(process.home,'runs.json')).then(()=>true,()=>false),false,'#1369 off leaves no runs.json');
    for(const method of ['run_get','run_events','run_reattach']){
      const refused=await request(ws,next,'refused-'+method,method,{projectKey:process.project,sessionKey:'registry-off-session',runId:'registry-off-run'});
      assert.equal(refused.ok,false,`#1369 off ${method} refuses`);
      assert.equal(refused.error.code,'CAPABILITY_UNAVAILABLE',`#1369 off ${method} unavailable code`);
      assert.equal(refused.error.message,'Persistent run registry is unavailable.',`#1369 off ${method} unavailable message`);
    }
    rows.push({scenario:'off',capabilities:described.result.capabilities});
    ws.terminate();process.child.kill('SIGTERM');const status=await bounded(process.exit,'off signal drain');
    assert.deepEqual(status,[0,null],'#1369 off clean signal exit');rows.push({scenario:'off',exit:status,stderr:process.stderr()});
  }
  // on: the switch states itself, the registry persists and run_get reads it.
  {
    const process=await launch('registry-on','on');
    assert.ok(process.stderr().includes('native run registry on'),'#1369 on registry receipt on stderr');
    const {ws,next}=await client(process.address);
    const described=await request(ws,next,'describe','describe_server',{projectKey:process.project});
    assert.equal(described.ok,true,'#1369 on describe_server succeeds');
    for(const capability of ['run_get','run_events','run_reattach'])
      assert.ok(described.result.capabilities.includes(capability),'#1369 describe_server gates run capabilities');
    const events=await submit(ws,next,process,'registry-on-session','registry-on-run','native registry on');
    assert.equal(events.at(-1).type,'turn_completed','#1369 on turn completes');
    assert.equal(await stat(join(process.home,'runs.json')).then(()=>true,()=>false),true,'#1369 on persists runs.json');
    const read=await request(ws,next,'read','run_get',{projectKey:process.project,sessionKey:'registry-on-session',runId:'registry-on-run'});
    assert.equal(read.ok,true,'#1369 on run_get reads the registry');
    assert.equal(read.result.state,'completed','#1369 on run_get terminal state');
    rows.push({scenario:'on',capabilities:described.result.capabilities});
    ws.terminate();process.child.kill('SIGTERM');const status=await bounded(process.exit,'on signal drain');
    assert.deepEqual(status,[0,null],'#1369 on clean signal exit');rows.push({scenario:'on',exit:status,stderr:process.stderr()});
  }
  // invalid: fail closed at startup, before any listening advertisement.
  {
    const home=join(out,'registry-invalid','home'),project=join(out,'registry-invalid','project');
    await mkdir(home,{recursive:true});await mkdir(project,{recursive:true});
    await writeFile(join(home,'pilotdeck.yaml'),`schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:${port}/v1\n      apiKey: mock-only\n      models:\n        mock: {}\n`,{flag:'wx'});
    const token=join(home,'token');await writeFile(token,'native-registry-switch-fixture\n',{flag:'wx'});
    const child=spawn(binary,['--native','--home',home,'--project',project,'--listen','127.0.0.1:0','--token-file',token,'--sidecar',sidecar,'--drain-ms','1500'],{cwd:project,env:{PATH:process.env.PATH,HOME:home,PILOT_HOME:home,LANG:'en_US.UTF-8',PILOTDECK_NATIVE_RUN_REGISTRY:'maybe'},stdio:['ignore','pipe','pipe']});children.push(child);
    let stderr='';child.stderr.on('data',b=>stderr+=b);
    const status=await bounded(once(child,'exit'),'invalid startup');
    assert.notEqual(status[0],0,'#1369 invalid registry switch rejected');
    assert.ok(stderr.includes('must be exactly on or off'),'#1369 invalid registry switch reports the gate');
    assert.ok(!stderr.includes('native listening'),'#1369 invalid registry switch never advertises ready');
    rows.push({scenario:'invalid',status,stderr});
  }
 }catch(error:any){
  // Only an actual named assertion can become a target receipt. A network
  // error, process failure or deadline remains an unclassified INVALID.
  if(error?.code==='ERR_ASSERTION'){
    const first=String(error.message).split('\n')[0];
    if(['#1369 off registry receipt on stderr','#1369 on registry receipt on stderr','#1369 describe_server gates run capabilities','#1369 off turn completes','#1369 off transcript persists','#1369 off leaves no runs.json','#1369 on run_get reads the registry','#1369 invalid registry switch rejected','#1369 off run_get unavailable code','#1369 on run_get terminal state','#1369 off clean signal exit','#1369 on clean signal exit','#1369 off durable session file'].includes(first)) assertionFailure=first;
  }
  throw error;
}finally{
  for(const ws of clients)ws.terminate();
  for(const child of children)if(child.exitCode===null&&child.signalCode===null){child.kill('SIGKILL');await bounded(once(child,'exit'),'forced cleanup',5000,true).catch(()=>{});}
  for(const socket of sockets)socket.destroy();await new Promise<void>(r=>server.close(()=>r()));
  const after={binary:await sha(binary),sidecar:await sha(sidecar)};
  assert.equal(after.binary,identities.binary.sha256,'#1369 binary unchanged');assert.equal(after.sidecar,identities.sidecar.sha256,'#1369 sidecar unchanged');
  await writeFile(join(out,'process-record.json'),JSON.stringify({node:process.version,...identities,after,rows,assertionFailure},null,2)+'\n',{flag:'wx'});
}
console.log('#1369 native registry switch off, on and invalid scenarios passed');
