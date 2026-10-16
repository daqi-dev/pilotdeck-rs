// Private test transport: JSON pipe commands carry explicit callback envelopes.
import fs from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';
import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import {pathToFileURL} from 'node:url';
import {frame,decode} from './protocol.mjs';
import {prepareFixture} from './fixture.mjs';
import {bind,sha} from './guard.mjs';
import {ReadyOwner} from './ready-owner.mjs';
import {listAcceptanceGate} from './acceptance-gate.mjs';
const spec=JSON.parse(fs.readFileSync(process.argv[2],'utf8'));
assert.equal(process.env.PDRS_READY_PROBE,'owned-ready-admission');assert.equal(process.version,'v22.22.0');
assert.ok(['oracle','candidate'].includes(spec.mode));
for(const key of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR','PILOT_HOME'])assert.ok(process.env[key].startsWith(spec.scratch+path.sep),key);
const verify=()=>{for(const [name,digest]of Object.entries(spec.scripts))assert.equal(sha(fs.readFileSync(path.join(spec.scriptRoot,name))),digest,name);for(const value of Object.values(spec.tools))assert.equal(sha(fs.readFileSync(value.path)),value.sha256);};
verify();const before=bind(spec.baseline,spec.tools.git.path),fixture=await prepareFixture(spec.scratch);
const emit=value=>process.stdout.write(JSON.stringify(value)+'\n');
const events=[];let index=0,bytes=0,testing=false,disposed=false,closed=false;
const mark=(event,fields={})=>{const row={event,eventIndex:++index,...fields};bytes+=Buffer.byteLength(JSON.stringify(row));assert.ok(bytes<=512*1024,'host trace bound');events.push(row);};
const load=name=>import(pathToFileURL(path.join(spec.baseline,'src',name)).href);
const {createLocalGateway}=await load('cli/createLocalGateway.ts');
const {TextWebSocketConnection}=await load('gateway/server/websocket.ts');
const {GatewayWsConnection}=await load('gateway/server/GatewayWsConnection.ts');
const local=createLocalGateway();
const gateway=new Proxy(local.gateway,{get(target,key){const original=Reflect.get(target,key,target);if(typeof original!=='function')return original;return (...args)=>{mark('RealMethodCall',{method:key});return Reflect.apply(original,target,args);};}});
const owner=new ReadyOwner(spec.connection,1,record=>emit({event:'Commit',record}),mark);
const oracleList=spec.mode==='oracle'&&['A+','A-','X1'].includes(spec.connection);
const listGate=oracleList?listAcceptanceGate(gateway,{mode:'T1',identity:{connection:spec.connection,requestId:spec.connection+'-0',inputSha256:sha(frame(JSON.stringify({type:'request',id:spec.connection+'-0',method:'list_projects'})))},mark}):null;
if(listGate)listGate.owned.then(token=>emit({event:'Owned',token}));
let helloResolve;const hello=new Promise(resolve=>helloResolve=resolve);
class Socket extends EventEmitter {
  write(raw){const wireHex=Buffer.from(raw).toString('hex'),decoded=decode(Buffer.from(raw));mark('HostAdapterWrite',{wireHex,...decoded});if(decoded.json?.type==='hello_ok')helloResolve();else if(testing)owner.emit('HostFrame',{wireHex});if(decoded.opcode===8)closed=true;return true;}
  end(){closed=true;this.emit('close');}
}
const socket=new Socket(),ws=new TextWebSocketConnection(socket);
const originalSend=ws.sendText;
ws.sendText=function(text){const json=JSON.parse(text);mark('HostSendTextEnter',{requestId:json.id??null,text});if(listGate&&json.id===spec.connection+'-0'&&json.ok===true)listGate.observeH1(json.result);return Reflect.apply(originalSend,this,[text]);};
new GatewayWsConnection(ws,{gateway:listGate?.gateway??gateway,token:'fixture',serverVersion:'ready-admission'});
socket.emit('data',frame(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'ready-admission',token:'fixture'})));
await hello;testing=true;emit({event:'Boot',fixture});
const watchdog=setTimeout(()=>{process.stderr.write('ready host watchdog\n');process.exit(2);},15000);
function input(label,items){mark('CallbackEnter',{label,items});for(const item of items){if(item.kind==='HostFrame')socket.emit('data',Buffer.from(item.wireHex,'hex'));else if(item.readinessClass==='ReadyAtAdmission'){void owner.immediate(item);emit({event:'Accepted',token:item.token});}else owner.register(item);}mark('CallbackReturn',{label});emit({event:'CallbackReturned',label});}
async function finish(){socket.emit('close');closed=true;owner.cancel();listGate?.cleanup();await local.dispose();disposed=true;verify();assert.deepEqual(bind(spec.baseline,spec.tools.git.path),before);if(spec.mode==='candidate')assert.equal(events.filter(e=>e.event==='RealMethodCall'&&['supportedAgents','listProjects'].includes(e.method)).length,0,'host local business call');fs.writeFileSync(spec.report,JSON.stringify({events,cleanup:{closed,disposed},sourceBefore:before,fixture,mode:spec.mode,observability:{hostAdapterWrite:'MEMORY_ADAPTER_NOT_W0_C0'}},null,2)+'\n',{flag:'wx'});clearTimeout(watchdog);emit({event:'Finished',disposed,closed});process.stdout.end(()=>process.exit(0));}
readline.createInterface({input:process.stdin}).on('line',line=>{
  try{
    const command=JSON.parse(line);mark('CommandReceived',{kind:command.kind});
    if(command.kind==='Input'){
      input(command.label,command.items);
    }else if(command.kind==='OracleRelease'){
      assert.ok(listGate);listGate.release(command.token);emit({event:'Accepted',token:command.token});
      if(command.afterAccept)input('successor',command.afterAccept);
    }else if(command.kind==='Ready'){
      try{emit(owner.receive(command));if(command.afterAccept)input('successor',command.afterAccept);}catch(error){mark('ReadyRejected',{token:command.token,code:error.code??'INVALID_READY'});emit({event:'Rejected',token:command.token,code:error.code??'INVALID_READY'});}
    }else if(command.kind==='Finish')void finish().catch(error=>{process.stderr.write(String(error));process.exit(1);});
    else assert.fail('unknown command');
  }catch(error){process.stderr.write(String(error));process.exit(1);}
});
