// Actual TS WebSocket parser + RPC consumer, on an EventEmitter socket (no listener).
import {EventEmitter} from 'node:events';
import {readFile,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const baseline=resolve(process.argv[2]);
const load=async(path:string)=>import(pathToFileURL(join(baseline,path)).href);
const {TextWebSocketConnection}=await load('src/gateway/server/websocket.ts');
const {GatewayWsConnection}=await load('src/gateway/server/GatewayWsConnection.ts');
const {InProcessGateway}=await load('src/gateway/client/InProcessGateway.ts');
class Socket extends EventEmitter {
 writes:Buffer[]=[];ended=false;
 write(bytes:Buffer){this.writes.push(Buffer.from(bytes));return true;}
 end(){this.ended=true;}
}
function client(opcode:number,payload:Buffer) {
 const header=Buffer.alloc(8);header[0]=0x80|opcode;header[1]=0x80|126;header.writeUInt16BE(payload.length,2);
 const mask=Buffer.from([0x11,0x22,0x33,0x44]);mask.copy(header,4);
 const encoded=Buffer.from(payload);for(let index=0;index<encoded.length;index++)encoded[index]^=mask[index%4];
 return Buffer.concat([header,encoded]);
}
const request='{"type":"request","id":"coalesced-native","method":"supported_agents","params":{}}';
const cases=[['ping',[client(1,Buffer.from(request)),client(9,Buffer.from('native-ping'))]],['close',[client(1,Buffer.from(request)),client(8,Buffer.alloc(0))]],['invalid',[client(1,Buffer.from(request)),client(1,Buffer.from('invalid json'))]]] as const;
const recorded=[];
for(const [name,frames] of cases) {
 const socket=new Socket();const ws=new TextWebSocketConnection(socket as any);
 new GatewayWsConnection(ws,{token:'offline',serverVersion:'offline',gateway:{describeServer:async()=>({}),supportedAgents:()=>InProcessGateway.prototype.supportedAgents.call({})} as any});
 socket.emit('data',client(1,Buffer.from(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'offline',token:'offline'}))));
 await new Promise(resolve=>setImmediate(resolve));socket.writes=[];
 const input=Buffer.concat(frames);socket.emit('data',input);await new Promise(resolve=>setImmediate(resolve));
 recorded.push({name,inputHex:input.toString('hex'),outputHex:Buffer.concat(socket.writes).toString('hex'),ended:socket.ended});
}
await writeFile(process.argv[3],JSON.stringify(recorded,null,2)+'\n');
