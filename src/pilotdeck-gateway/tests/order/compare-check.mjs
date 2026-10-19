// Pure adversarial controls: no baseline imports, process spawn or IO fixtures.
import assert from 'node:assert/strict';
import {matrix,frame,decode} from './protocol.mjs';
import {compareCases,splitFrames} from './compare.mjs';
const server=(opcode,payload='')=>{const bytes=Buffer.from(payload);assert.ok(bytes.length<126);return Buffer.concat([Buffer.from([128|opcode,bytes.length]),bytes]).toString('hex');};
const helloIn=frame(JSON.stringify({type:'hello'})).toString('hex'),helloOut=server(1,JSON.stringify({type:'hello_ok'}));
const close=server(8),closeIn=frame('',8).toString('hex');
const ts={results:[]},rust=[];
for(const spec of matrix){
  const req=spec.frames.map((method,i)=>frame(method==='ping'||method==='close'?'':JSON.stringify({type:'request',id:spec.name+'-'+i,method}),method==='ping'?9:method==='close'?8:1).toString('hex'));
  const allBody=spec.frames.map((m,i)=>m==='ping'?server(10):m==='close'?close:server(1,JSON.stringify({type:'response',id:spec.name+'-'+i,ok:true,result:{}})));
  const body=spec.frames.includes('close')?allBody.slice(-1):allBody;
  const inputEvents=[{event:'input_data',frames:[{wireHex:helloIn}]},...(spec.causal?req.map(wireHex=>({event:'input_data',frames:[{wireHex}]})):[{event:'input_data',frames:req.map(wireHex=>({wireHex}))}])];
  const outputEvents=[{event:'wire_write',...decode(Buffer.from(helloOut,'hex'))},...body.map(h=>({event:'wire_write',...decode(Buffer.from(h,'hex'))}))];
  ts.results.push({name:spec.name,events:[...inputEvents,...outputEvents]});
  const events=[],add=(event,extra)=>events.push({eventIndex:events.length+1,event,...extra});
  add('transport_read',{stream:'downstream',wireHex:helloIn});add('transport_write',{stream:'upstream',wireHex:helloIn});add('transport_write',{stream:'downstream',wireHex:helloOut});
  add('transport_read',{stream:'downstream',wireHex:spec.causal?req[0]:req.join('')});
  spec.frames.forEach((method,i)=>{if(spec.causal&&i===1){add('causal_first_response_observed',{wireHex:body[0]});add('transport_read',{stream:'downstream',wireHex:req[1]});}if(['ping','close','describe_server'].includes(method))add('transport_write',{stream:'upstream',wireHex:req[i]});if(!spec.frames.includes('close')||method==='close')add('transport_write',{stream:'downstream',wireHex:allBody[i]});});
  if(!spec.frames.includes('close')){add('transport_read',{stream:'downstream',wireHex:closeIn});add('transport_write',{stream:'upstream',wireHex:closeIn});add('transport_write',{stream:'downstream',wireHex:close});}
  rust.push({name:spec.name,wireHex:body,events,scope:{joinsetConsumable:'NOT_OBSERVED'}});
}
assert.equal(compareCases(ts,rust).filter(c=>c.orderMatches).length,6);
const controls=[
  r=>r.pop(),r=>r[0]=structuredClone(r[1]),r=>r[0].wireHex.pop(),
  r=>r[0].events[0].eventIndex=9,r=>r[0].scope.joinsetConsumable='COMPLETE',
  r=>{r[0].events=r[0].events.filter(e=>!(e.event==='transport_write'&&e.stream==='upstream'&&e.wireHex!==helloIn&&e.wireHex!==closeIn));r[0].events.forEach((e,i)=>e.eventIndex=i+1);},
  r=>{r[5].events=r[5].events.filter(e=>e.event!=='causal_first_response_observed');r[5].events.forEach((e,i)=>e.eventIndex=i+1);},
  r=>{r[1].events.push({eventIndex:r[1].events.length+1,event:'transport_write',stream:'upstream',wireHex:frame(JSON.stringify({type:'request',id:'leaked',method:'list_projects'})).toString('hex')});},
  r=>{const at=r[5].events.findIndex(e=>e.event==='causal_first_response_observed');r[5].events.push(...r[5].events.splice(at,1));r[5].events.forEach((e,i)=>e.eventIndex=i+1);},
];
for(const mutate of controls){const r=structuredClone(rust);mutate(r);assert.throws(()=>compareCases(ts,r));}
const mismatch=structuredClone(ts);const writes=mismatch.results[0].events.filter(e=>e.event==='wire_write');[writes[1].json,writes[2].json]=[writes[2].json,writes[1].json];
assert.equal(compareCases(mismatch,rust)[0].orderMatches,false,'order mismatch must be visible, not autoaccepted');
assert.throws(()=>splitFrames(['810461']));
console.log(JSON.stringify({status:'PREPARED',runtime:'NOT_RUN',positiveSix:6,rejectedMutations:controls.length+1,visibleOrderMismatch:true}));
