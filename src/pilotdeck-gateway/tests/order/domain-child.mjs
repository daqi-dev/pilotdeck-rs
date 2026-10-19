import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import {pathToFileURL} from 'node:url';
import {executionAsyncId} from 'node:async_hooks';
import {frame,decode} from './protocol.mjs';
import {prepareFixture} from './fixture.mjs';
import {sha,bind} from './guard.mjs';
import {cases,missing,fixtureMethod} from './domain-matrix.mjs';
import {CommitWriter,CommitOwner,serverText} from './domain-commit.mjs';
const spec=JSON.parse(fs.readFileSync(process.argv[2],'utf8'));
assert.equal(process.version,'v22.22.0');assert.ok(['oracle','candidate'].includes(spec.mode));
for(const [key,value]of Object.entries(spec.tools))assert.equal(sha(fs.readFileSync(value.path)),value.sha256,'tool drift '+key);
for(const [name,value]of Object.entries(spec.scripts))assert.equal(sha(fs.readFileSync(path.join(spec.scriptRoot,name))),value,'script drift '+name);
for(const key of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR','PILOT_HOME'])assert.ok(process.env[key].startsWith(spec.scratch+path.sep),key);
const before=bind(spec.baseline,spec.tools.git.path);
if(spec.mode==='candidate')assert.equal(sha(fs.readFileSync(spec.oracle.path)),spec.oracle.sha256,'oracle not fixed');
assert.equal(sha(fs.readFileSync(spec.native.path)),spec.native.sha256,'native original drift');
const nativeReport=JSON.parse(fs.readFileSync(spec.native.path));
const rawNative=nativeReport.wireHex.map(hex=>decode(Buffer.from(hex,'hex'))).find(x=>x.json?.id==='describe-supported-1');
assert.equal(rawNative.json.ok,true);assert.equal(nativeReport.events.find(x=>x.event==='route_result_ready').method,'supported_agents');
const {project,pilot}=await prepareFixture(spec.scratch);
const load=name=>import(pathToFileURL(path.join(spec.baseline,'src',name)).href);
const {createLocalGateway}=await load('cli/createLocalGateway.ts');
const {TextWebSocketConnection}=await load('gateway/server/websocket.ts');
const {GatewayWsConnection}=await load('gateway/server/GatewayWsConnection.ts');
const results=[];let local;let failure;let disposed=false;let current;
let watchdog=setTimeout(()=>{process.stderr.write('domain child watchdog\n');process.exit(2);},55000);
const waitTurn=()=>new Promise(resolve=>setImmediate(resolve));
const timeout=promise=>{let timer;return Promise.race([promise,new Promise((_,reject)=>timer=setTimeout(()=>reject(Error('case watchdog')),5000))]).finally(()=>clearTimeout(timer));};
try {
  local=createLocalGateway();
  // Proxy preserves identity of each returned Promise; no observing then/catch is attached.
  const gateway=new Proxy(local.gateway,{get(target,key){const value=Reflect.get(target,key,target);if(typeof value!=='function')return value;
    return (...args)=>{
      current?.mark('host_method_call',{method:key,args});
      try {
        const result=Reflect.apply(value,target,args);current?.mark('host_method_return',{method:key,promise:result instanceof Promise});
        if(current?.pendingGate&&key==='describeServer'&&current.testing) {current.mark('pending_gate_held');return current.pendingGate.promise.then(()=>result);}
        return result;
      }catch(error){current?.mark('host_method_throw',{method:key,error:String(error)});throw error;}
    };
  }});
  for(const plan of cases) {
    const events=[];const wire=[];const pending=new Map();let index=0;let epoch=0;let testing=false;let closed=false;let resolveClose;
    const closedPromise=new Promise(r=>resolveClose=r);
    const mark=(event,extra={})=>events.push({eventIndex:++index,event,epoch,asyncId:executionAsyncId(),...extra});
    current={mark,events,case:plan.id,testing:false};
    if(plan.intervention==='host-pending'){let release;const promise=new Promise(r=>release=r);current.pendingGate={promise,release};}
    const native=new Map();const writer=new CommitWriter(plan.id,1,native);const channel=[];let channelGate=false;
    const deliver=record=>{mark('channel_arrival',record);if(channelGate&&record.seq===1){channel.push(record);mark('channel_held',record);}else writer.accept(record);};
    const owner=new CommitOwner(plan.id,1,deliver,mark);
    const notify=(hex)=>{
      const decoded=decode(Buffer.from(hex,'hex'));mark('wire_write',{wireHex:hex,...decoded});wire.push(hex);
      const id=decoded.json?.type==='hello_ok'?'hello':decoded.json?.id;
      if(id&&pending.has(id)){pending.get(id)();pending.delete(id);}
      if(decoded.opcode===8)resolveClose();
    };
    class Socket extends EventEmitter {
      write(raw){const hex=Buffer.from(raw).toString('hex');mark('host_wire_write',{wireHex:hex,...decode(Buffer.from(raw))});if(testing&&spec.mode==='candidate')owner.emit('HostFrame',{wireHex:hex});notify(hex);return true;}
      end(){mark('socket_end');closed=true;this.emit('close');}
    }
    const socket=new Socket();const ws=new TextWebSocketConnection(socket);new GatewayWsConnection(ws,{gateway,token:'fixture',serverVersion:'owned-domain'});
    const wait=id=>new Promise(r=>pending.set(id,r));
    const hello=wait('hello');socket.emit('data',frame(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'owned-domain',token:'fixture'})));await timeout(hello);
    testing=true;current.testing=true;wire.length=0;
    const parts=plan.items.map((method,i)=>{
      if(['ping','close'].includes(method))return {kind:'HostFrame',method,bytes:frame('',method==='ping'?9:8)};
      const request={type:'request',id:plan.id+'-'+i,method,...(method==='session_model_get'?{params:{}}:{})};
      return {kind:'request',method,request,bytes:frame(JSON.stringify(request))};
    });
    const responses=new Map(parts.filter(p=>p.request).map(p=>[p.request.id,wait(p.request.id)]));
    const input=items=>{
      epoch++;mark('input_data',{wireHex:Buffer.concat(items.map(p=>p.bytes)).toString('hex'),items:items.map(p=>({method:p.method,wireHex:p.bytes.toString('hex')}))});
      if(spec.mode==='oracle')socket.emit('data',Buffer.concat(items.map(p=>p.bytes)));
      else {
        // The driver owns local request metadata. The host receives only this envelope.
        const envelope=items.map((p,i)=>{
          if(p.method!=='supported_agents')return {kind:'HostFrame',wireHex:p.bytes.toString('hex')};
          const token=`${plan.id}:${epoch}:${i}`;native.set(token,serverText({...rawNative.json,id:p.request.id}));return {kind:'NativeSlot',token,readinessClass:'ReadyAtAdmission'};
        });
        mark('host_ingress_epoch',{connection:plan.id,generation:1,items:envelope});
        for(const item of envelope){if(closed)break;if(item.kind==='HostFrame')socket.emit('data',Buffer.from(item.wireHex,'hex'));else {void owner.admit(item).catch(error=>{failure??={message:String(error),case:plan.id};});}}
      }
      mark('parser_callback_return');
    };
    if(plan.intervention==='host-committed-channel-gate')channelGate=true;
    if(plan.split==='same')input(parts);
    else if(plan.split==='sync-data'){input([parts[0]]);input([parts[1]]);}
    else if(plan.split==='next-turn'){input([parts[0]]);await waitTurn();mark('next_turn_callback');input([parts[1]]);}
    else {input([parts[0]]);await timeout(responses.get(parts[0].request.id));mark('causal_first_response_observed');input([parts[1]]);}
    if(plan.intervention==='host-pending'){
      const supported=parts.find(p=>p.method==='supported_agents');
      if(spec.mode==='candidate') {await waitTurn();assert.equal(owner.records[0]?.kind,'NativeSlot');}
      else await timeout(responses.get(supported.request.id));
      mark('pending_gate_released');current.pendingGate.release();
    }
    if(plan.items.includes('close'))await timeout(closedPromise);
    else if(spec.mode==='oracle')await timeout(Promise.all(responses.values()));
    else {
      // Wait for actual host replies; slot commits do not call host notify.
      const hostResponses=[...responses].filter(([id])=>parts.find(p=>p.request?.id===id).method!=='supported_agents').map(([,promise])=>promise);
      await timeout(Promise.all(hostResponses));
    }
    await waitTurn(); // capture already scheduled continuation/suppression; never selects their order
    if(spec.mode==='candidate'){
      for(const record of channel.reverse()){mark('channel_release',record);writer.accept(record);}
      writer.finalize(owner.seq);
      // No native slot original business request may have reached host.
      assert.equal(events.filter(e=>e.event==='host_method_call'&&e.method==='supportedAgents').length,0);
      assert.equal(events.filter(e=>e.event==='host_ingress_epoch').flatMap(e=>e.items).filter(e=>e.kind==='HostFrame').some(e=>fixtureMethod(e.wireHex)==='supported_agents'),false);
    }
    const committed=spec.mode==='candidate'?writer.wire:wire;
    if(!plan.items.includes('close'))assert.equal(committed.length,parts.length,'complete response/control cardinality');
    else assert.ok(committed.some(hex=>decode(Buffer.from(hex,'hex')).opcode===8),'missing close');
    results.push({id:plan.id,gate:plan.gate,inputScript:plan,events,wireHex:committed,commits:owner.records,transportIntervention:plan.intervention??null,localOriginalKeptAtWriter:spec.mode==='candidate',status:'RECORDED'});
    socket.emit('close');closed=true;
  }
}catch(error){failure={message:String(error),stack:error.stack};}
finally {
  try{if(local){await local.dispose();disposed=true;}}catch(error){failure??={message:String(error)};}
  let after;try{after=bind(spec.baseline,spec.tools.git.path);assert.deepEqual(after,before);for(const [key,value]of Object.entries(spec.tools))assert.equal(sha(fs.readFileSync(value.path)),value.sha256);for(const [name,value]of Object.entries(spec.scripts))assert.equal(sha(fs.readFileSync(path.join(spec.scriptRoot,name))),value);}catch(error){failure??={message:'postflight '+String(error)};}
  const report={partialCase:failure?{id:current?.case,events:current?.events}:null,mode:spec.mode,status:failure?'FAIL':'RECORDED_NOT_ACCEPTED',failure,results,missing,sourceBefore:before,sourceAfter:after,cleanup:{disposed,hooks:'proxy scoped to owned local gateway; no resource mutations'},project,pilot,native:spec.native,oracle:spec.oracle??null};
  fs.writeFileSync(spec.output,JSON.stringify(report,null,2)+'\n');clearTimeout(watchdog);process.exit(failure?1:0);
}
