// Compare only the registered six-cell output order; never a stage acceptance.
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
import {matrix,decode} from './protocol.mjs';
import {sha} from './guard.mjs';
export function splitFrames(chunks,masked=false) {
  const data=Buffer.concat(chunks.map(hex=>{assert.match(hex,/^(?:[0-9a-f]{2})*$/);return Buffer.from(hex,'hex');}));
  const frames=[];let at=0;
  while(at<data.length){
    assert.ok(data.length-at>=2,'truncated frame header');let size=data[at+1]&127,offset=2;
    if(size===126){assert.ok(data.length-at>=4);size=data.readUInt16BE(at+2);offset=4;}
    if(size===127){assert.ok(data.length-at>=10);const n=data.readBigUInt64BE(at+2);assert.ok(n<=BigInt(Number.MAX_SAFE_INTEGER));size=Number(n);offset=10;}
    assert.equal(Boolean(data[at+1]&128),masked,'unexpected mask bit');
    const payloadAt=at+offset+(masked?4:0),end=payloadAt+size;
    assert.ok(end<=data.length,'truncated payload');const bytes=data.subarray(at,end);
    if(masked){const payload=Buffer.from(data.subarray(payloadAt,end));for(let i=0;i<payload.length;i++)payload[i]^=data[at+offset+i%4];frames.push({opcode:data[at]&15,json:(data[at]&15)===1?JSON.parse(payload.toString()):null,wireHex:bytes.toString('hex')});}
    else frames.push({...decode(bytes),wireHex:bytes.toString('hex')});at=end;
  }
  return frames;
}
const order=(frames,methods,name)=>frames.filter(f=>f.json?.type!=='hello_ok').map(f=>{
  if(f.opcode===10)return 'ping';if(f.opcode===8)return 'close';
  assert.equal(f.opcode,1,'unexpected output opcode');assert.equal(f.json?.type,'response');assert.equal(f.json.ok,true,'business response failed');
  const found=methods.findIndex((m,i)=>f.json.id===name+'-'+i);assert.ok(found>=0,'unknown response id');return methods[found];
});
export function compareCases(ts,rust) {
  assert.equal(ts.results.length,6);assert.equal(rust.length,6);
  const names=matrix.map(c=>c.name);
  assert.deepEqual(ts.results.map(c=>c.name).sort(),[...names].sort());
  assert.deepEqual(rust.map(c=>c.name).sort(),[...names].sort());
  return matrix.map(spec=>{
    const t=ts.results.find(c=>c.name===spec.name),r=rust.find(c=>c.name===spec.name);
    assert.equal(r.scope.joinsetConsumable,'NOT_OBSERVED','completion-domain scope drift');
    assert.ok(r.events.length>0);r.events.forEach((e,i)=>assert.equal(e.eventIndex,i+1,'noncontiguous local sequence'));
    const body=r.wireHex.map(hex=>decode(Buffer.from(hex,'hex')));
    if(spec.frames.includes('close')){assert.ok(body.length>=1&&body.length<=2,'close-case output cardinality');assert.equal(body.at(-1).opcode,8);assert.equal(body.filter(f=>f.opcode===8).length,1);}else assert.equal(body.length,2,'missing body output');
    const actual=order(body,spec.frames,spec.name);
    const expected=order(t.events.filter(e=>e.event==='wire_write'),spec.frames,spec.name);
    const writes=splitFrames(r.events.filter(e=>e.event==='transport_write'&&e.stream==='downstream').map(e=>e.wireHex));
    assert.equal(writes[0].json?.type,'hello_ok');
    const recordedBody=writes.slice(1,spec.frames.includes('close')?undefined:-1);
    assert.deepEqual(recordedBody.map(f=>f.wireHex),r.wireHex,'client observation differs from actual downstream writes');
    if(!spec.frames.includes('close'))assert.equal(writes.at(-1).opcode,8,'missing teardown close');
    const reads=r.events.filter(e=>e.event==='transport_read'&&e.stream==='downstream'&&e.wireHex);
    const inputs=splitFrames(reads.map(e=>e.wireHex),true);
    const expectedInputs=t.events.filter(e=>e.event==='input_data').flatMap(e=>e.frames.map(f=>f.wireHex));
    assert.equal(inputs.length,expectedInputs.length+(spec.frames.includes('close')?0:1),'extra or missing input frame');
    assert.deepEqual(inputs.slice(0,expectedInputs.length).map(f=>f.wireHex),expectedInputs,'input wire drift');
    if(!spec.frames.includes('close'))assert.equal(inputs.at(-1).opcode,8);
    const forwarded=splitFrames(r.events.filter(e=>e.event==='transport_write'&&e.stream==='upstream').map(e=>e.wireHex),true);
    const localRequests=forwarded.filter(f=>f.json?.type==='request'&&['list_projects','supported_agents'].includes(f.json.method));
    assert.equal(localRequests.length,0,'G7 local request reached upstream');
    const expectedForwarded=inputs.filter(f=>!(f.json?.type==='request'&&['list_projects','supported_agents'].includes(f.json.method)));
    assert.deepEqual(forwarded.map(f=>f.wireHex),expectedForwarded.map(f=>f.wireHex),'missing/extra upstream frame');
    const complete=r.events.filter(e=>e.event==='task_complete_sample'&&e.complete===true);
    const causal=r.events.filter(e=>e.event==='causal_first_response_observed');
    assert.equal(causal.length,spec.causal?1:0,'causal marker cardinality');
    if(spec.causal){assert.equal(causal[0].wireHex,r.wireHex[0]);const after=reads.filter(e=>e.eventIndex>causal[0].eventIndex);assert.equal(splitFrames(after.map(e=>e.wireHex),true)[0].opcode,9,'ping was not read after actual response observation');}
    return {name:spec.name,expected,actual,orderMatches:JSON.stringify(actual)===JSON.stringify(expected),rustReadHex:reads.map(e=>e.wireHex),tsReadHex:t.events.filter(e=>e.event==='input_data').map(e=>e.frames.map(f=>f.wireHex).join('')),taskCompleteSamples:complete.length,joinsetConsumable:'NOT_OBSERVED',scope:'six-cell order only; actual read batches preserved; no full payload/stage acceptance'};
  });
}
if(process.argv[1]&&import.meta.url===pathToFileURL(path.resolve(process.argv[1])).href){
  const [tsPath,rustPath,out]=process.argv.slice(2);assert.ok(tsPath&&rustPath&&out);
  const comparisonScripts=Object.fromEntries(['compare.mjs','protocol.mjs','guard.mjs'].map(name=>[name,sha(fs.readFileSync(new URL(name,import.meta.url)))]));
  const ts=JSON.parse(fs.readFileSync(tsPath)),index=JSON.parse(fs.readFileSync(rustPath));
  assert.equal(index.status,'RECORDED_NOT_ACCEPTED');assert.equal(index.cases.length,6);
  const root=fs.realpathSync(path.dirname(rustPath));
  const rust=index.cases.map(entry=>{
    const p=fs.realpathSync(path.join(root,entry.path));assert.ok(p.startsWith(root+path.sep));const bytes=fs.readFileSync(p);assert.equal(sha(bytes),entry.sha256);const row=JSON.parse(bytes);
    const hostPath=fs.realpathSync(path.join(path.dirname(p),row.host.path));assert.ok(hostPath.startsWith(root+path.sep));const hostBytes=fs.readFileSync(hostPath);assert.equal(sha(hostBytes),row.host.sha256);const host=JSON.parse(hostBytes);assert.equal(host.status,'RECORDED_NOT_ACCEPTED');assert.equal(host.cleanup.disposed,true);
    return row;
  });
  const cases=compareCases(ts,rust);
  for(const [name,digest]of Object.entries(comparisonScripts))assert.equal(sha(fs.readFileSync(new URL(name,import.meta.url))),digest,'comparison script postflight drift');
  const result={status:'RECORDED_NOT_ACCEPTED',node:{path:process.execPath,version:process.version,sha256:sha(fs.readFileSync(process.execPath))},comparisonScripts,tsSha256:sha(fs.readFileSync(tsPath)),rustIndexSha256:sha(fs.readFileSync(rustPath)),cases,allOrdersMatch:cases.every(c=>c.orderMatches),acceptance:'NOT_RUN'};
  fs.writeFileSync(out,JSON.stringify(result,null,2)+'\n',{flag:'wx'});
  console.log(JSON.stringify({output:out,orderMatches:cases.filter(c=>c.orderMatches).length,total:6,acceptance:'NOT_RUN'}));
}
