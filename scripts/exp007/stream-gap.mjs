// D3 transport-only demonstrator: two successive connections with an offline gap.
import assert from 'node:assert/strict';
import {writeFile} from 'node:fs/promises';
import {resolve} from 'node:path';
import {startMock} from './mock.mjs';
import {origin,evidence,requireUI} from './env.mjs';
import {pause,waitFor} from './dom-env.mjs';
const {WebSocket}=requireUI('ws');
const old=[],fresh=[];const mock=await startMock();
async function connect(frames){const s=new WebSocket(origin.replace('http:','ws:')+'/ws?token=f1-probe-token');s.on('message',data=>frames.push(JSON.parse(data.toString()).seq));await new Promise((r,j)=>{s.once('open',r);s.once('error',j);});s.send(JSON.stringify({type:'watch-session',sessionId:'s'}));return s;}
let first,second;
try{
 first=await connect(old);await fetch(origin+'/__probe/stream?hz=25&count=75');await waitFor(()=>old.length>=20);
 first.close();await new Promise(r=>first.once('close',r));await pause(480);
 second=await connect(fresh);await waitFor(()=>fresh.at(-1)===75,5000);
 const delivered=new Set([...old,...fresh]);const missing=mock.stats.sent.map(f=>f.seq).filter(seq=>!delivered.has(seq));assert(missing.length>=10);assert(fresh[0]>old.at(-1)+1);
 const result={status:'PASS_TRANSPORT_ONLY',oldCount:old.length,newCount:fresh.length,missing,watchSessionMessages:mock.stats.outbound.filter(f=>f.type==='watch-session').length,limit:'mock does not replay stream deltas; real cross-document navigation and persisted completed history are unverified'};
 await writeFile(resolve(evidence,'stream-gap.json'),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify(result,null,2));
}finally{first?.terminate();second?.terminate();await mock.close();}
