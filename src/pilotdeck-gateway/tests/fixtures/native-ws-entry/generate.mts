import assert from 'node:assert/strict';
import {resourceObservations} from './resources.mts';
import {createRequire} from 'node:module';
import {mkdir, writeFile, readFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {connect} from 'node:net';
import {once, EventEmitter} from 'node:events';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
import {RFC6455_SAMPLE_NONCE} from './sample.mts';
assert.equal(process.version, 'v22.22.0', '#749 pinned Node');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, ['src/gateway/server/GatewayServer.ts', 'src/gateway/server/GatewayWsConnection.ts', 'src/gateway/server/websocket.ts']);
const deadline = setTimeout(() => { console.error('#749 probe deadline'); process.exit(1); }, 15000);
const req = createRequire(join(baseline, 'package.json'));
const {transformSync} = createRequire(req.resolve('tsx'))('esbuild');
async function load(source:string, imports:Record<string,any>={}) {
  const code=transformSync(await readFile(join(baseline,source),'utf8'),{loader:'ts',format:'cjs',target:'es2022',sourcefile:source}).code;
  const module={exports:{} as any};
  new Function('require','module','exports',code)((name:string)=>{
    if(name in imports)return imports[name];
    if(name.startsWith('node:'))return req(name);
    return new Proxy({}, {get(_t,p){if(p==='__esModule')return true;throw new Error(`unexercised import ${source} -> ${name}.${String(p)}`);}});
  },module,module.exports);return module.exports;
}
const version=await load('src/gateway/protocol/version.ts');
const errors=await load('src/gateway/dialog/errors.ts');
const skills=await load('src/extension/skills/SkillManager.ts');
const connection=await load('src/gateway/server/GatewayWsConnection.ts',{'../protocol/version.js':version,'../dialog/errors.js':errors,'../../extension/skills/index.js':skills});
const websocket=await load('src/gateway/server/websocket.ts');
const serverSockets:any[]=[];
const http=req('node:http');
const observedHttp={...http,createServer(...args:any[]){const server=http.createServer(...args);server.on('connection',(socket:any)=>serverSockets.push(socket));return server;}};
// Capture the actual transport constructed by GatewayServer; all methods are
// inherited unchanged from the fixed baseline, including close/data handling.
const observedWebsocket={...websocket,TextWebSocketConnection:class extends websocket.TextWebSocketConnection {
  constructor(socket:any){super(socket);socket.fixtureTransport=this;}
}};
const {startGatewayServer}=await load('src/gateway/server/GatewayServer.ts',{'node:http':observedHttp,'./websocket.js':observedWebsocket,'./GatewayWsConnection.js':connection});
const {WebSocket}=req('ws');
const deferred = () => {let resolve!: (v?: any)=>void;const promise = new Promise<any>(r=>resolve=r);return {promise,resolve};};
const hello = (token='fixture-token') => ({type:'hello',protocolVersion:'1.2',clientName:'ui',clientVersion:'probe',token});
let describeCalls=0;
let holdDescribe = false;
let heldDescriptionSettled = false;
let settled = deferred();
let entered = deferred(), release = deferred(), submitting = deferred(), submitRelease = deferred(), disconnectEntered = deferred(), disconnectRelease = deferred();
let holdDisconnect = false;
const lifecycle: string[] = [];
const gateway = {
  async describeServer() { describeCalls++;if (holdDescribe) {holdDescribe=false;entered.resolve();await release.promise;heldDescriptionSettled=true;} return {name:'native-ws-probe',capabilities:[]}; },
  async *submitTurn() {lifecycle.push('submit');submitting.resolve();await submitRelease.promise;yield {type:'turn_completed',result:{type:'aborted'}};lifecycle.push('settled');settled.resolve();},
  async disconnectInteraction() {lifecycle.push('disconnect');disconnectEntered.resolve();if(holdDisconnect) await disconnectRelease.promise;return {preserveTurn:false};},
  async abortTurn() {lifecycle.push('abort');submitRelease.resolve();return {ok:true};},
};
const server = await startGatewayServer({gateway,host:'127.0.0.1',port:0,token:'fixture-token',serverVersion:'probe'});
console.error('#749 stage listening');
const peers: any[]=[];
async function ws() {const peer=new WebSocket(server.wsUrl);peers.push(peer);await once(peer,'open');return peer;}
async function receive(peer:any, value:any) {const response=once(peer,'message');peer.send(JSON.stringify(value));return JSON.parse(String((await response)[0]));}
async function raw(path:string,key:boolean) {const socket=connect(Number(new URL(server.url).port),'127.0.0.1');await once(socket,'connect');let bytes='';socket.on('data',b=>bytes+=b.toString());socket.write(`GET ${path} HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n${key?'Sec-WebSocket-Key: '+RFC6455_SAMPLE_NONCE+'\r\n':''}\r\n`);await once(socket,'close');return bytes;}
const observations:any={};
try {
console.error('#749 stage invalid-upgrade');
  observations.invalidUpgrade=[await raw('/wrong',true),await raw('/ws',false)];
console.error('#749 stage hello');
  const a=await ws();observations.hello=(await receive(a,hello())).type;
  const bad=await ws();const badClose=once(bad,'close');bad.send(JSON.stringify(hello('wrong')));const bc=await badClose;observations.badToken=[bc[0],String(bc[1])];
  const invalid=await ws();const ic=once(invalid,'close');invalid.send('{');const ix=await ic;observations.invalidJson=[ix[0],String(ix[1])];
console.error('#749 stage concurrent');
  holdDescribe=true;const slow=await ws();const slowMessage=once(slow,'message');slow.send(JSON.stringify(hello()));await entered.promise;
  const fast=await ws();observations.concurrent=(await receive(fast,hello())).type;release.resolve();observations.predecessor=JSON.parse(String((await slowMessage)[0])).type;
  const submit={type:'request',id:'turn',method:'submit_turn',params:{sessionKey:'session',message:'probe'}};
console.error('#749 stage submit');
  a.send(JSON.stringify(submit));await submitting.promise;
  const abortReply=new Promise<any>(resolve=>a.on('message',(bytes:any)=>{const frame=JSON.parse(String(bytes));if(frame.type==='response'&&frame.id==='abort')resolve(frame);}));
  a.send(JSON.stringify({type:'request',id:'abort',method:'abort_turn',params:{sessionKey:'session'}}));
  observations.abortResponse=await abortReply;
  await settled.promise;
  observations.sameSocketAbort=lifecycle.splice(0);
console.error('#749 stage disconnect');
  submitting=deferred();submitRelease=deferred();settled=deferred();holdDisconnect=true;
  a.send(JSON.stringify({...submit,id:'close-turn'}));await submitting.promise;
  const normalClose=once(a,'close');a.close();await disconnectEntered.promise;
  observations.disconnectBeforeRelease=[...lifecycle];disconnectRelease.resolve();
  await settled.promise;
  observations.disconnectAfterRelease=[...lifecycle];
  const normal=await normalClose;observations.normalClose=[normal[0],String(normal[1])];
  holdDescribe=true;heldDescriptionSettled=false;entered=deferred();release=deferred();
  const held=await ws();held.send(JSON.stringify(hello()));await entered.promise;
  const heldClose=once(held,'close');held.close();await heldClose;
  observations.closedWhileDescriptionPending=!heldDescriptionSettled;
  const encode=(opcode:number,payload:string)=>{
    const bytes=Buffer.from(payload);assert.ok(bytes.length<126,'#749 small raw frame');
    const mask=Buffer.from([1,2,3,4]);return Buffer.concat([Buffer.from([0x80|opcode,0x80|bytes.length]),mask,Buffer.from(bytes.map((b,i)=>b^mask[i%4]))]);
  };
  // Exact data callback boundaries: a real ws client's automatic close reply
  // otherwise re-enters handleData and can revive the buffered tail.
  class ProbeSocket extends EventEmitter {
    write(_bytes:Buffer){return true;}
    end(){return this;}
  }
  async function deterministicText() {
    let calls=0;const socket=new ProbeSocket();
    const text=new websocket.TextWebSocketConnection(socket);
    new connection.GatewayWsConnection(text,{
      gateway:{async describeServer(){calls++;return {};}},
      token:'fixture-token',serverVersion:'probe',
    });
    socket.emit('data',encode(1,JSON.stringify(hello())));
    await Promise.resolve();const before=calls;
    return {socket,count:()=>calls-before};
  }
  const tail=encode(1,JSON.stringify({type:'request',id:'tail',method:'describe_server'}));
  const framed=await deterministicText();
  framed.socket.emit('data',Buffer.concat([encode(9,'x'),encode(8,''),tail]));
  observations.opcodeCloseTailCalls=framed.count();
  framed.socket.emit('data',encode(8,''));
  observations.opcodeCloseNextDataTailCalls=framed.count();
  const application=await deterministicText();
  application.socket.emit('data',Buffer.concat([encode(1,'{'),tail]));
  observations.applicationCloseTailCalls=application.count();
console.error('#749 stage stop');
  let closed=false;const closing=server.close().then(()=>closed=true);
  const pong=once(fast,'pong');fast.ping('alive');await pong;observations.existingAfterStop=!closed;
  const rejected=connect(Number(new URL(server.url).port),'127.0.0.1');observations.newAfterStop=String((await once(rejected,'error'))[0].code);rejected.destroy();
  for(const peer of peers) peer.close();await closing;observations.closedAfterPeers=closed;
  observations.serverCloseDoesNotAwaitDescription=!heldDescriptionSettled;release.resolve();
  observations.resources=await resourceObservations(websocket,startGatewayServer,serverSockets,connection);
  const output={sourceCommit,sourceSha256,node:process.version,observations};
  await mkdir(process.argv[2],{recursive:true});
  await writeFile(join(process.argv[2],'oracle.json'),JSON.stringify(output,null,2)+'\n',{flag:'wx'});
  console.log(JSON.stringify(output,null,2));
} finally {for(const peer of peers)peer.terminate();await server.close().catch(()=>{});clearTimeout(deadline);}
