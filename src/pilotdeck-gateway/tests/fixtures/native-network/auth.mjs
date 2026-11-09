import assert from 'node:assert/strict';import{writeFileSync}from'node:fs';
const[url,out]=process.argv.slice(2);const target=new URL(url);assert(target.hostname==='127.0.0.1'&&target.protocol==='ws:');
const socket=new WebSocket(url);const result={messages:[]};const timer=setTimeout(()=>{socket.close();throw Error('auth diagnostic deadline')},5000);
socket.addEventListener('open',()=>socket.send(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'ui',clientVersion:'probe',token:'owned-wrong-token'})));
socket.addEventListener('message',event=>result.messages.push(String(event.data)));
await new Promise((resolve,reject)=>{socket.addEventListener('close',event=>{result.code=event.code;result.reason=event.reason;resolve();});socket.addEventListener('error',reject);});clearTimeout(timer);assert.equal(result.code,4003);assert.equal(result.reason,'auth_failed');assert.equal(result.messages.length,0);writeFileSync(out+'/auth-wire.json',JSON.stringify(result,null,2)+'\n');
