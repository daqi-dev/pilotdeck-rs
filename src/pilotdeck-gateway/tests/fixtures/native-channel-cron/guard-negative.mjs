import assert from 'node:assert/strict';
import net from 'node:net';
import {readFileSync,appendFileSync} from 'node:fs';
for(const host of ['ilinkai.weixin.qq.com','example.invalid','8.8.8.8']){
 assert.throws(()=>net.connect({host,port:443}),e=>e.code==='PDRS_NON_LOOPBACK_NETWORK');
}
assert.throws(()=>net.connect({path:'/tmp/not-owned-fixture.sock'}),e=>e.code==='PDRS_NON_LOOPBACK_NETWORK');
await assert.rejects(fetch('https://ilinkai.weixin.qq.com/never-send'),e=>e.code==='PDRS_NON_LOOPBACK_NETWORK');
const server=net.createServer(s=>s.end());await new Promise(r=>server.listen(0,'127.0.0.1',r));
await new Promise((done,reject)=>{const s=net.connect({host:'127.0.0.1',port:server.address().port});s.once('connect',()=>{s.destroy();done();});s.once('error',reject);});
await new Promise(r=>server.close(r));
readFileSync(process.env.PDRS_FIXTURE_CREDENTIALS_PATH,'utf8');
assert.equal(await (await fetch('data:text/plain,fixture-local')).text(),'fixture-local');
const ipc=net.createServer(s=>s.end()),ipcPath=process.env.TMPDIR+'/owned.sock';await new Promise(r=>ipc.listen(ipcPath,r));
await new Promise((done,reject)=>{const s=net.connect({path:ipcPath});s.once('connect',()=>{s.destroy();done();});s.once('error',reject);});
await new Promise(r=>ipc.close(r));
const rows=readFileSync(process.env.PDRS_LOOPBACK_GUARD_LOG,'utf8').trim().split('\n').map(l=>JSON.parse(l));
assert.equal(rows.filter(r=>r.type==='denied').length,5);
assert(rows.some(r=>r.type==='credentials-read'&&r.valid));
console.log(JSON.stringify({pass:true,deniedBeforeOriginalConnect:5,ownedIPCConnect:true,dataResource:true,numericLoopbackConnect:true,credentialReadObserved:true}));
