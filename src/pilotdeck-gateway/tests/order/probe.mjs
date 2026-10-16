// Run only through run.mjs, after separate root authorization. No baseline runs in preparation.
import {EventEmitter} from 'node:events';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
import {matrix,frame,decode} from './protocol.mjs';
import {sha} from './guard.mjs';
import {prepareFixture} from './fixture.mjs';
const [baseline,output,scratch]=process.argv.slice(2);
assert.equal(process.env.PDRS_ORDER_PROBE,'owned-default-six');
assert.equal(process.version,'v22.22.0','pinned default oracle Node version');
const {project,pilot}=await prepareFixture(scratch);
const load=p=>import(pathToFileURL(path.join(baseline,'src',p)).href);
const {createLocalGateway}=await load('cli/createLocalGateway.ts');
const {TextWebSocketConnection}=await load('gateway/server/websocket.ts');
const {GatewayWsConnection}=await load('gateway/server/GatewayWsConnection.ts');
const results=[];let local;let failure;let disposed=false;
const timeout=(promise)=>{let timer;return Promise.race([promise,new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('watchdog: case incomplete')),5000)})]).finally(()=>clearTimeout(timer));};
try {
  local=createLocalGateway();
  for(const spec of matrix) {
    const events=[];let index=0;const pending=new Map();const ownership=new Map();const controlOwnership=new Map();let closeResolve;
    const closeObserved=new Promise(r=>closeResolve=r);
    const mark=(event,extra={})=>events.push({side:'ts',case:spec.name,connection:spec.name,eventIndex:++index,readBatch:null,frameIndex:null,op:null,method:null,requestId:null,actualResultHash:null,wireHex:null,event,...extra});
    class Socket extends EventEmitter {
      write(raw) { const bytes=Buffer.from(raw), decoded=decode(bytes),id=decoded.json?.id;mark('wire_write',{...(id?ownership.get(id):controlOwnership.get(decoded.opcode)),wireHex:bytes.toString('hex'),...decoded,op:decoded.opcode,requestId:id??null,actualResultHash:decoded.json?.result===undefined?null:sha(JSON.stringify(decoded.json.result))});
        const key=decoded.json?.type==='hello_ok'?'hello':decoded.json?.id;
        if(key&&pending.has(key)){pending.get(key)(decoded.json);pending.delete(key);}
        if(decoded.opcode===8)closeResolve();return true;
      }
      end(){mark('socket_end');this.emit('close');}
    }
    const socket=new Socket(), ws=new TextWebSocketConnection(socket);
    new GatewayWsConnection(ws,{gateway:local.gateway,token:'fixture',serverVersion:'owned-probe'});
    let batch=0;
    const input=parts=>{const readBatch=++batch;const frames=parts.map((p,i)=>{const meta={readBatch,frameIndex:i,op:p.op,method:p.method??null,requestId:p.id??null,wireHex:p.bytes.toString('hex')};if(p.id)ownership.set(p.id,meta);if(p.op===9||p.op===8)controlOwnership.set(p.op===9?10:8,meta);return meta;});mark('input_data',{readBatch,frames});socket.emit('data',Buffer.concat(parts.map(p=>p.bytes)));mark('parser_callback_return',{readBatch});};
    const wait=id=>new Promise(r=>pending.set(id,r));
    const hello=wait('hello');input([{op:1,bytes:frame(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'owned-probe',token:'fixture'}))}]);await timeout(hello);
    const parts=spec.frames.map((method,i)=>{
      if(method==='ping'||method==='close')return {op:method==='ping'?9:8,method,bytes:frame('',method==='ping'?9:8)};
      const id=spec.name+'-'+i;return {op:1,method,id,bytes:frame(JSON.stringify({type:'request',id,method}))};
    });
    const responses=parts.filter(p=>p.id).map(p=>wait(p.id));
    if(spec.causal){input([parts[0]]);const first=await timeout(responses[0]);assert.equal(first.ok,true);mark('causal_first_response_observed');input([parts[1]]);}
    else input(parts);
    if(spec.frames.includes('close'))await timeout(closeObserved);
    else {const replies=await timeout(Promise.all(responses));for(const reply of replies)assert.equal(reply.ok,true,'default method failed');}
    assert.ok(!spec.frames.includes('ping')||events.some(e=>e.event==='wire_write'&&e.opcode===10),'missing actual pong');
    assert.equal(events.filter(e=>e.event==='wire_write'&&e.json?.type==='hello_ok').length,1);
    assert.equal(events.filter(e=>e.event==='input_data').length,spec.causal?3:2);
    assert.equal(events.filter(e=>e.event==='parser_callback_return').length,spec.causal?3:2);
    if(!spec.frames.includes('close'))for(const part of parts.filter(p=>p.id))assert.equal(events.filter(e=>e.event==='wire_write'&&e.requestId===part.id).length,1,'exact response cardinality');
    if(spec.frames.includes('close'))assert.equal(events.filter(e=>e.event==='wire_write'&&e.opcode===8).length,1);
    results.push({name:spec.name,events,observability:{osCompletion:'NOT_OBSERVED',methodResultReady:'NOT_OBSERVED',taskConsumable:'NOT_OBSERVED',wireWrite:'OBSERVED'},status:'RECORDED'});
    // Close ends wire capture; it does not establish completion of an in-flight list_projects.
    socket.emit('close');
  }
  assert.equal(results.length,6,'six complete recording cases required');
}catch(error){failure={message:String(error),stack:error.stack};process.exitCode=1;}
finally {
  try{if(local){await local.dispose();disposed=true;}}catch(error){failure??={message:'dispose: '+String(error)};process.exitCode=1;}
  await fs.writeFile(output,JSON.stringify({status:failure?'FAIL':'RECORDED_NOT_ACCEPTED',failure,results,node:process.version,scriptSha256:sha(await fs.readFile(new URL(import.meta.url))),cleanup:{disposed},rust:'NOT_RUN',scope:'default createLocalGateway + real parser/RPC + owned disk; in-memory transport; no timing or task-completion claim'},null,2)+'\n');
}
