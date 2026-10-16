// Single prepared cell. Root supplies the separate bounded execution window.
import {EventEmitter} from 'node:events';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
import {frame,decode} from './protocol.mjs';
import {bind,sha} from './guard.mjs';
import {prepareFixture} from './fixture.mjs';
import {listAcceptanceGate,assertAdmissionBeforeH1} from './acceptance-gate.mjs';

const [baseline,output,scratch,mode,git,prereg]=process.argv.slice(2);
assert.equal(process.env.PDRS_TS_ACCEPTANCE,'owned-five-r50');
assert.equal(process.version,'v22.22.0');
assert.ok(['T0','T1','T2','N1','N2'].includes(mode));
for(const p of [baseline,output,scratch,git,prereg])assert.ok(path.isAbsolute(p));
const directory=path.dirname(new URL(import.meta.url).pathname);
const scripts=['acceptance-probe.mjs','acceptance-gate.mjs','fixture.mjs','guard.mjs','protocol.mjs','offline.mjs'];
const scriptHashes=async()=>Object.fromEntries(await Promise.all(scripts.map(async n=>[n,sha(await fs.readFile(path.join(directory,n)))])));
const before=bind(baseline,git), scriptsBefore=await scriptHashes();
const taskBytes=await fs.readFile(path.join(prereg,'task.md')),matrixBytes=await fs.readFile(path.join(prereg,'matrix.json'));
assert.equal(sha(taskBytes),'d46d986d890601b1afd0fc4c0d9b60f3c1e66f1e1bb002da19167d83b8ddb32a');
assert.equal(sha(matrixBytes),'a60c7dda2f91039435e1a05fc945b5508f977504b9df5999ff60d1a6e4fedff0');
const fixture=await prepareFixture(scratch);
const load=p=>import(pathToFileURL(path.join(baseline,'src',p)).href);
const {createLocalGateway}=await load('cli/createLocalGateway.ts');
const {TextWebSocketConnection}=await load('gateway/server/websocket.ts');
const {GatewayWsConnection}=await load('gateway/server/GatewayWsConnection.ts');
let index=0,bytesRecorded=0,local,gate,socket,disposed=false,failure,negative=null,closed=false,mapping='UNMAPPED';
const events=[],pending=new Map(),responses=[];let inputLabel=null;
const connection=mode+'-generation-1',listId=mode+'-list',nextId=mode+'-supported';
const listFrame=frame(JSON.stringify({type:'request',id:listId,method:'list_projects'}));
const nextFrame=frame(JSON.stringify({type:'request',id:nextId,method:'supported_agents'}));
const identity={connection,requestId:listId,inputSha256:sha(listFrame)};
const mark=(event,extra={})=>{
  const row={event,eventIndex:++index,connection,...extra};
  bytesRecorded+=Buffer.byteLength(JSON.stringify(row));
  assert.ok(bytesRecorded<=512*1024,'trace byte bound');events.push(row);
};
const wait=id=>new Promise(resolve=>{assert.ok(!pending.has(id));pending.set(id,resolve);});
const one=event=>{const rows=events.filter(e=>e.event===event);assert.equal(rows.length,1,'exact one '+event);return rows[0];};
try {
  local=createLocalGateway();
  gate=listAcceptanceGate(local.gateway,{mode,identity,mark});
  class Socket extends EventEmitter {
    on(name,listener) {
      if(name!=='data')return super.on(name,listener);
      return super.on(name,function(...args) {
        const label=inputLabel;let completed=false;
        mark('DataCallbackEnter',{label});
        try{const result=Reflect.apply(listener,this,args);completed=true;return result;}
        finally{mark('DataCallbackReturn',{label,completed});}
      });
    }
    write(raw) {
      const bytes=Buffer.from(raw), decoded=decode(bytes);
      mark('SocketWrite',{wireHex:bytes.toString('hex'),frameSha256:sha(bytes),...decoded});
      const id=decoded.json?.type==='hello_ok'?'hello':decoded.json?.id;
      if(decoded.json?.type==='response')responses.push(decoded.json);
      if(id&&pending.has(id)){const resolve=pending.get(id);pending.delete(id);resolve(decoded.json);}
      if(decoded.opcode===8)closed=true;
      return true;
    }
    end(){mark('SocketEnd');this.emit('close');}
  }
  socket=new Socket();
  const ws=new TextWebSocketConnection(socket),originalSend=ws.sendText;
  ws.sendText=function(text) {
    const json=JSON.parse(text);
    if(closed){mark('SendTextClosedAttempt',{requestId:json.id??null,text});return Reflect.apply(originalSend,this,[text]);}
    mark('SendTextEnter',{requestId:json.id??null,text,framePayloadSha256:sha(text)});
    if(json.type==='response'&&json.id===listId&&json.ok===true)gate.observeH1(json.result);
    const result=Reflect.apply(originalSend,this,[text]);
    mark('SendTextReturn',{requestId:json.id??null});return result;
  };
  new GatewayWsConnection(ws,{gateway:gate.gateway,token:'fixture',serverVersion:'acceptance-mapping'});
  const input=(label,bytes)=>{
    mark('InputData',{label,wireHex:bytes.toString('hex'),inputSha256:sha(bytes)});
    assert.equal(inputLabel,null,'no nested fixture input');inputLabel=label;
    try{socket.emit('data',bytes);}finally{inputLabel=null;}
  };
  const hello=wait('hello');
  input('hello',frame(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'acceptance-mapping',token:'fixture'})));
  assert.equal((await hello).type,'hello_ok');
  const first=wait(listId),second=wait(nextId);
  if(mode==='T0')input('pair',Buffer.concat([listFrame,nextFrame]));
  else {
    input('list',listFrame);
    const token=await gate.owned;
    assert.ok(gate.snapshot().outcome.projects.some(p=>p.fullPath===fixture.project),'real registered disk project owned');
    if(mode==='T1'){gate.release(token);input('successor',nextFrame);}
    else {
      input('successor',nextFrame);
      if(mode==='N2') {
        const wrong={...token,connection:connection+'-wrong'};
        mark('mutant_hit',{mutation:'actual-wrong-token',expected:token,supplied:wrong});
        try{gate.release(wrong);assert.fail('wrong token accepted');}catch(error){assert.equal(error.code,'TOKEN_MISMATCH');negative={code:error.code};}
        assert.equal(gate.snapshot().settled,false,'wrong token must preserve pending gate');
        assert.equal(gate.snapshot().accepted,false);
        assert.equal(events.some(e=>e.event==='SendTextEnter'&&e.requestId===listId),false);
        assert.equal((await second).ok,true);
        mark('PendingAtCleanup',{...gate.snapshot(),requestStillPending:pending.has(listId)});
      }else gate.release(token);
      // N1 uses the identical T2 release driver. Its already-settled Promise
      // is not delayed, rejected, or given extra microtasks to force an H1 window.
    }
  }
  if(mode!=='N2') {
    const replies=await Promise.all([first,second]);
    for(const reply of replies)assert.equal(reply.ok,true,'real default method success');
    assert.ok(replies[0].result.projects.some(p=>p.fullPath===fixture.project),'real registered disk project response');
    for(const id of [listId,nextId])assert.equal(responses.filter(r=>r.id===id).length,1,'exact response cardinality');
    if(mode==='N1') {
      one('mutant_hit');one('PrematureResolved');
      assert.ok(events.some(e=>e.event==='SocketWrite'&&e.json?.id===listId));
      let causalRejection;
      try{assertAdmissionBeforeH1(events,listId);}catch(error){
        if(error.code!=='H1_BEFORE_ACCEPTANCE')throw error;causalRejection=error;
      }
      if(causalRejection) {
        assert.ok(gate.violations.includes(causalRejection.code),'independent H1 record agrees');
        negative={code:'H1_BEFORE_ACCEPTANCE'};
        mark('CausalRejected',{code:negative.code,violations:[...gate.violations]});
      }else {
        mapping='UNMAPPED_N1_H1_NOT_BEFORE_ADMISSION';
        mark('NegativeWindowUnmapped',{reason:mapping});
      }
    }else assert.deepEqual(gate.violations,[]);
  }
  if(mode==='T1'||mode==='T2') {
    const owned=one('OutcomeOwned'),sent=one('ReleaseSent'),received=one('ReleaseReceived'),resolved=one('GateResolved');
    const enter=events.find(e=>e.event==='DataCallbackEnter'&&e.label==='successor');
    const returned=events.find(e=>e.event==='DataCallbackReturn'&&e.label==='successor');
    assert.ok(owned.eventIndex<sent.eventIndex&&sent.eventIndex<received.eventIndex&&received.eventIndex<resolved.eventIndex);
    assert.ok(owned.eventIndex<enter.eventIndex,'outcome owned before successor callback');
    assert.equal(returned.completed,true,'actual parser callback returned');
    assert.ok(mode==='T1'?resolved.eventIndex<enter.eventIndex:returned.eventIndex<sent.eventIndex);
    assert.ok(events.filter(e=>e.event==='SendTextEnter'&&e.requestId===listId).every(e=>e.eventIndex>resolved.eventIndex));
    one('AcceptanceAtH1');assertAdmissionBeforeH1(events,listId);
  }
  if(mode==='N2'){one('ReleaseRejected');assert.equal(events.some(e=>e.event==='GateResolved'),false);}
  if(mode==='T0'){one('OriginalPromiseBypass');assert.equal(events.some(e=>e.event==='OutcomeOwned'),false);}
  assert.equal(gate.snapshot().calls,1);
  input('causal-close',frame('',8));
  assert.equal(closed,true,'actual close frame write required');
  mark('CloseObserved',{clientC0:'NOT_OBSERVED'});
}catch(error){failure={message:String(error),stack:error.stack};process.exitCode=1;}
finally {
  if(socket&&!closed){socket.emit('close');mark('FailureConnectionClose');}
  gate?.cleanup();
  try{if(local){await local.dispose();disposed=true;}}catch(error){failure??={message:'dispose: '+String(error)};process.exitCode=1;}
  let after,scriptsAfter;
  try{after=bind(baseline,git);scriptsAfter=await scriptHashes();assert.deepEqual(after,before);assert.deepEqual(scriptsAfter,scriptsBefore);}catch(error){failure??={message:'postflight: '+String(error)};process.exitCode=1;}
  const result={status:failure?'FAIL':negative?'NEGATIVE_REJECTED':mode==='N1'?'UNMAPPED':'RECORDED_NOT_ACCEPTED',mapping,mode,failure,negative,events,responses,fixture,sourceBefore:before,sourceAfter:after,scriptsBefore,scriptsAfter,gate:gate?.snapshot(),cleanup:{disposed,closed},observability:{outcomeOwned:mode==='T0'?'NOT_OBSERVED':'INTERVENED',naturalSettlement:'NOT_OBSERVED',rustR2:'UNMAPPED',clientC0:'NOT_OBSERVED'},rust:'NOT_RUN',scope:'one default real list/supported pair; no production commit protocol'};
  await fs.writeFile(output,JSON.stringify(result,null,2)+'\n',{flag:'wx'});
}
