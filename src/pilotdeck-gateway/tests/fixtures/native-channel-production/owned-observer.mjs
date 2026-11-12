// Non-mutating observations of this fixture's own child and mock transports.
// No permission changes, signal injection, ptrace, or network probes.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import cp from 'node:child_process';
import http from 'node:http';
import net from 'node:net';
import {createRequire,syncBuiltinESMExports} from 'node:module';
const dir=process.env.PDRS_OWNED_PROCESS_DIR;
if(!dir||!path.isAbsolute(dir)||!fs.statSync(dir).isDirectory())throw Error('existing private observation directory required');
const bootId=fs.readFileSync('/proc/sys/kernel/random/boot_id','utf8').trim();
const records=[];let sequence=0;
const maxBytes=2*1024*1024;
const sha=filename=>crypto.createHash('sha256').update(fs.readFileSync(filename)).digest('hex');
const save=(name,value)=>{const tmp=path.join(dir,name+'.new');fs.writeFileSync(tmp,JSON.stringify(value,null,2)+'\n');fs.renameSync(tmp,path.join(dir,name));};
function append(kind,value){fs.appendFileSync(path.join(dir,'transport.jsonl'),JSON.stringify({sequence:sequence++,time:Date.now(),kind,...value})+'\n');}
function bytes(value,encoding){return Buffer.isBuffer(value)?value:typeof value==='string'?Buffer.from(value,typeof encoding==='string'?encoding:'utf8'):value instanceof Uint8Array?Buffer.from(value):Buffer.alloc(0);}
function body(value){const b=bytes(value);return {bytes:b.length,rawBytesHex:b.subarray(0,maxBytes).toString('hex'),truncated:b.length>maxBytes};}
function birth(pid){const raw=fs.readFileSync(`/proc/${pid}/stat`,'utf8');const fields=raw.slice(raw.lastIndexOf(')')+2).split(' ');return {bootId,startTimeTicks:fields[19]};}
function descendants(pid){const all=[];for(const name of fs.readdirSync('/proc')){if(!/^\d+$/.test(name))continue;try{const raw=fs.readFileSync(`/proc/${name}/stat`,'utf8');const fields=raw.slice(raw.lastIndexOf(')')+2).split(' ');all.push({pid:Number(name),parentPid:Number(fields[1]),birth:{bootId,startTimeTicks:fields[19]}});}catch{}}const owned=new Set([pid]);let changed=true;while(changed){changed=false;for(const row of all)if(owned.has(row.parentPid)&&!owned.has(row.pid)){owned.add(row.pid);changed=true;}}return all.filter(row=>owned.has(row.pid)&&row.pid!==pid);}
function isSame(pid,identity){try{return birth(pid).startTimeTicks===identity.startTimeTicks;}catch{return false;}}
function tables(){const found=[];for(const file of ['/proc/net/tcp','/proc/net/tcp6']){for(const line of fs.readFileSync(file,'utf8').trim().split('\n').slice(1)){const columns=line.trim().split(/\s+/);if(columns[3]!=='0A')continue;const [ip,port]=columns[1].split(':');const host=ip==='0100007F'?'127.0.0.1':/^0+$/.test(ip)?(ip.length===8?'0.0.0.0':'::'):ip;found.push({host,port:parseInt(port,16),inode:columns[9]});}}return found;}
function listeners(pid,identity){if(!isSame(pid,identity))return [];const owned=new Set();for(const name of fs.readdirSync(`/proc/${pid}/fd`)){try{const link=fs.readlinkSync(`/proc/${pid}/fd/${name}`);const m=/^socket:\[(\d+)\]$/.exec(link);if(m)owned.add(m[1]);}catch{}}return tables().filter(row=>owned.has(row.inode)).map(({inode,...row})=>({...row,ownerPid:pid}));}
const originalSpawn=cp.spawn;
cp.spawn=function(...args){const child=originalSpawn.apply(this,args);const expected=process.env.PDRS_EXPECTED_EXECUTABLE;
 if(!expected||path.resolve(String(args[0]))!==path.resolve(expected))return child;
 const record={schemaVersion:1,pid:child.pid,requestedArgv:[String(args[0]),...(Array.isArray(args[1])?args[1]:[])],observations:[],errors:[]};records.push(record);
 let timer;
 child.once('spawn',()=>{try{record.birth=birth(child.pid);record.pidNamespace=fs.readlinkSync('/proc/self/ns/pid');record.networkNamespace=fs.readlinkSync('/proc/self/ns/net');record.argv=fs.readFileSync(`/proc/${child.pid}/cmdline`).toString().split('\0').filter(Boolean);record.cwd=fs.readlinkSync(`/proc/${child.pid}/cwd`);const executable=fs.readlinkSync(`/proc/${child.pid}/exe`);record.executable={path:executable,sha256:sha(executable)};record.spawnObservedAt=Date.now();
 timer=setInterval(()=>{try{if(!isSame(child.pid,record.birth))return;const ready=listeners(child.pid,record.birth);if(ready.length)record.observations.push({time:Date.now(),readyListeners:ready});record.ownedDescendants??=[];for(const row of descendants(child.pid)){if(!record.ownedDescendants.some(x=>x.pid===row.pid&&x.birth.startTimeTicks===row.birth.startTimeTicks)){try{row.argv=fs.readFileSync(`/proc/${row.pid}/cmdline`).toString().split('\0').filter(Boolean);row.executable={path:fs.readlinkSync(`/proc/${row.pid}/exe`)};row.executable.sha256=sha(row.executable.path);row.observedAt=Date.now();record.ownedDescendants.push(row);}catch(e){if(!isSame(row.pid,row.birth)){row.retiredDuringCapture=true;record.ownedDescendants.push(row);}else record.errors.push('descendant '+row.pid+': '+String(e));}}}save('owned-processes.json',records);}catch(e){record.errors.push(String(e));}},75);timer.unref();save('owned-processes.json',records);
 }catch(e){record.errors.push(String(e));save('owned-processes.json',records);}});
 child.once('exit',(code,signal)=>{record.exit={code,signal};record.exitObservedAt=Date.now();save('owned-processes.json',records);});
 child.once('close',(code,signal)=>{if(timer)clearInterval(timer);record.close={code,signal};record.closeObservedAt=Date.now();record.identityStillPresent=record.birth?isSame(child.pid,record.birth):null;const active=tables();const seen=record.observations.flatMap(x=>x.readyListeners);record.listenTableAfterClose=active;record.remainingOwnedIdentities=(record.ownedDescendants??[]).filter(row=>isSame(row.pid,row.birth));record.portsStillListening=seen.filter(x=>active.some(y=>y.host===x.host&&y.port===x.port));save('owned-processes.json',records);});return child;};
// Audit the actual mock HTTP parser input and the response chunks passed to Node.
const originalEmit=http.Server.prototype.emit;
http.Server.prototype.emit=function(event,...args){if(event==='request'){
 const [request,response]=args,id=sequence++;const incoming=[];const outgoing=[];let inBytes=0,outBytes=0;
 request.on('data',chunk=>{const b=bytes(chunk);inBytes+=b.length;if(inBytes<=maxBytes)incoming.push(b);});
 request.on('end',()=>append('mock-http-request',{id,socket:tcpSocket(request.socket),method:request.method,url:request.url,headers:request.headers,rawHeaders:request.rawHeaders,local:{host:request.socket.localAddress,port:request.socket.localPort},remote:{host:request.socket.remoteAddress,port:request.socket.remotePort},body:{bytes:inBytes,rawBytesHex:Buffer.concat(incoming).toString('hex'),truncated:inBytes>maxBytes}}));
 const write=response.write,end=response.end;
 const capture=(chunk,encoding)=>{const b=bytes(chunk,encoding);outBytes+=b.length;if(outBytes<=maxBytes)outgoing.push(b);};
 response.write=function(chunk,encoding,...rest){capture(chunk,encoding);return write.call(this,chunk,encoding,...rest);};
 response.end=function(chunk,encoding,...rest){capture(chunk,encoding);return end.call(this,chunk,encoding,...rest);};
 response.once('finish',()=>append('mock-http-response',{id,status:response.statusCode,headers:response.getHeaders(),actualHeaderText:response._header??null,finished:true,body:{bytes:outBytes,rawBytesHex:Buffer.concat(outgoing).toString('hex'),truncated:outBytes>maxBytes}}));
 response.once('error',error=>append('mock-http-response-error',{id,error:String(error)}));
 }return originalEmit.call(this,event,...args);};
const originalFetch=globalThis.fetch;
globalThis.fetch=async function(input,init){const request=new Request(input,init);const url=new URL(request.url);if(!['127.0.0.1','localhost','[::1]'].includes(url.hostname))return originalFetch(input,init);
 const id=sequence++,payload=await request.clone().arrayBuffer();append('fixture-http-inbound-request',{id,url:request.url,method:request.method,headers:Object.fromEntries(request.headers),body:body(Buffer.from(payload))});
 const response=await originalFetch(request);const clone=response.clone();clone.arrayBuffer().then(raw=>append('fixture-http-inbound-response',{id,status:response.status,headers:Object.fromEntries(response.headers),body:body(Buffer.from(raw))})).catch(error=>append('fixture-http-inbound-error',{id,error:String(error)}));return response;};
if(process.env.PDRS_BASELINE){const require=createRequire(path.join(process.env.PDRS_BASELINE,'package.json'));const {WebSocket}=require('ws');const send=WebSocket.prototype.send,emit=WebSocket.prototype.emit;
 const socket=ws=>{const tcp=ws._socket?tcpSocket(ws._socket):null;return {url:ws.url??null,id:tcp?.id??null,localHost:tcp?.localHost??null,localPort:tcp?.localPort??null,remoteHost:tcp?.remoteHost??null,remotePort:tcp?.remotePort??null};};
 WebSocket.prototype.send=function(data,...args){append('fixture-ws-send',{socket:socket(this),payload:body(data)});return send.call(this,data,...args);};
 WebSocket.prototype.emit=function(event,...args){if(event==='message')append('fixture-ws-receive',{socket:socket(this),binary:args[1]===true,payload:body(args[0])});return emit.call(this,event,...args);};}
// Real bytes for protocol mocks that intentionally use net instead of HTTP
// (SMTP/IMAP and manual HTTP/WS peers). Never inspect a foreign socket or stdio.
const tcpIds=new WeakMap();let tcpSequence=0;
// A real TCP write may be queued before connect exposes either endpoint.
// Preserve those original bytes under the same object identity; the separate
// connect event supplies actual endpoints, never a URL-derived guess.
function tcpSocket(socket){if(!socket.localPort&&!socket.remotePort&&!socket.connecting)return null;if(!tcpIds.has(socket))tcpIds.set(socket,tcpSequence++);return {id:tcpIds.get(socket),localHost:socket.localAddress??null,localPort:socket.localPort??null,remoteHost:socket.remoteAddress??null,remotePort:socket.remotePort??null};}
const tcpWrite=net.Socket.prototype.write,tcpEmit=net.Socket.prototype.emit;
net.Socket.prototype.write=function(chunk,encoding,...args){const socket=tcpSocket(this);if(socket)append('fixture-tcp-write',{socket,payload:body(bytes(chunk,encoding))});return tcpWrite.call(this,chunk,encoding,...args);};
net.Socket.prototype.emit=function(event,...args){if(event==='data'||event==='connect'){const socket=tcpSocket(this);if(socket)append(event==='data'?'fixture-tcp-data':'fixture-tcp-connect',event==='data'?{socket,payload:body(args[0])}:{socket});}return tcpEmit.call(this,event,...args);};
syncBuiltinESMExports();
process.once('exit',()=>{try{save('observer-source.json',{path:new URL(import.meta.url).pathname,sha256:sha(new URL(import.meta.url)),ownerPid:process.pid,records:records.length});save('owned-processes.json',records);}catch{}});
