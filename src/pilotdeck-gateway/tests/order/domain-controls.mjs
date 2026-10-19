// Replays recorded oracle/candidate; never starts a host or changes raw evidence.
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {compare} from './domain-compare.mjs';
import {CommitWriter} from './domain-commit.mjs';
import {frame} from './protocol.mjs';
import {CommitStream,pack} from './domain-transport.mjs';
import {sha} from './guard.mjs';
export function checkControls(oracle,candidate) {
  const controls=[];const rejected=(id,fn)=>{assert.throws(fn,undefined,id+' must reject');controls.push({id,status:'REJECTED_AS_REQUIRED'});};
  const rec=(seq,kind,extra={})=>({connection:'control',generation:1,seq,kind,...extra});
  const sample=[rec(1,'HostFrame',{wireHex:'81027b7d'}),rec(2,'NativeSlot',{token:'native'})];
  const make=()=>new CommitWriter('control',1,new Map([['native','81025b5d']]));
  const reordered=make();reordered.accept(sample[1]);reordered.accept(sample[0]);assert.deepEqual(reordered.finalize(2),['81027b7d','81025b5d']);controls.push({id:'C1-out-of-order-transport',status:'RESTORED'});
  const encoded=Buffer.concat(sample.map(pack));
  for(let cut=1;cut<encoded.length;cut++){const stream=new CommitStream(make());stream.feed(encoded.subarray(0,cut));stream.feed(encoded.subarray(cut));assert.deepEqual(stream.finish(2),['81027b7d','81025b5d']);}
  controls.push({id:'C1-all-byte-splits',status:'RESTORED',cuts:encoded.length-1});
  rejected('C1-truncated-channel',()=>{const stream=new CommitStream(make());stream.feed(encoded.subarray(0,-1));stream.finish(2);});
  rejected('C1-duplicate-token',()=>{const w=make();w.accept({...sample[1],seq:1});w.accept(sample[1]);});
  rejected('C1-missing',()=>{const w=make();w.accept(sample[1]);w.finalize(2);});
  rejected('C1-duplicate-seq',()=>{const w=make();w.accept(sample[0]);w.accept(sample[0]);});
  rejected('C1-connection',()=>make().accept({...sample[0],connection:'foreign'}));
  rejected('C1-generation',()=>make().accept({...sample[0],generation:2}));
  rejected('C1-unknown-token',()=>make().accept(rec(1,'NativeSlot',{token:'unknown'})));
  rejected('C1-close-local',()=>{const w=make();w.accept(rec(1,'HostFrame',{wireHex:'880203e8'}));w.accept(sample[1]);});
  const mutate=(id,fn)=>rejected(id,()=>{const copy=structuredClone(candidate);fn(copy);compare(oracle,copy);});
  mutate('C2-empty',c=>c.results=[]);mutate('C2-missing-row',c=>c.results.pop());
  mutate('C2-missing-host-anchor',c=>c.results[0].events=c.results[0].events.filter(e=>e.event!=='host_method_call'));
  mutate('C2-host-local-leak',c=>c.results[0].events.push({event:'host_method_call',method:'supportedAgents'}));
  mutate('C2-hidden-control-frame',c=>c.results.find(r=>r.id==='D2-supported_agents-ping-same').wireHex.pop());
  mutate('C2-missing-async-gate',c=>c.missing.pop());
  mutate('C2-local-original-leak',c=>c.results[0].events.find(e=>e.event==='host_ingress_epoch').items.push({kind:'HostFrame',wireHex:frame(JSON.stringify({type:'request',id:'mutation',method:'supported_agents'})).toString('hex')}));
  mutate('D2-epoch-merge',c=>{const a=c.results.find(r=>r.id==='D2-supported_agents-ping-next-turn');const b=c.results.find(r=>r.id==='D2-supported_agents-ping-same');assert.notDeepEqual(a.wireHex.map(h=>parseInt(h.slice(0,2),16)&15),b.wireHex.map(h=>parseInt(h.slice(0,2),16)&15));a.wireHex.reverse();});
  mutate('D2-epoch-split',c=>{const a=c.results.find(r=>r.id==='D2-supported_agents-ping-same');const b=c.results.find(r=>r.id==='D2-supported_agents-ping-next-turn');assert.notDeepEqual(a.wireHex.map(h=>parseInt(h.slice(0,2),16)&15),b.wireHex.map(h=>parseInt(h.slice(0,2),16)&15));a.wireHex.reverse();});
  mutate('D3-immediate-error-as-promise',c=>{const row=c.results.find(r=>r.id==='D3-unknown_probe_method-supported_agents-reverse');row.wireHex.reverse();});
  const gate=candidate.results.find(r=>r.id==='P1-host-committed-channel-gate');assert.equal(gate.commits[0].kind,'HostFrame');assert.equal(gate.commits[1].kind,'NativeSlot');
  const arrivals=gate.events.filter(e=>['channel_arrival','channel_release'].includes(e.event)&&!(e.event==='channel_arrival'&&e.seq===1));assert.deepEqual(arrivals.map(e=>e.seq),[2,1]);
  mutate('P1-remove-seq-arrival-order',c=>{const row=c.results.find(r=>r.id===gate.id);row.wireHex.reverse();});
  const pending=candidate.results.find(r=>r.id==='P1-host-pending');assert.equal(pending.commits[0].kind,'NativeSlot');assert.equal(pending.commits[1].kind,'HostFrame');

  return controls;
}

if(process.argv[1]&&fileURLToPath(import.meta.url)===path.resolve(process.argv[1])) {
  const [directory,output]=process.argv.slice(2);assert.ok(directory&&output);assert.ok(!fs.existsSync(output),'preserve replay');
  const read=name=>JSON.parse(fs.readFileSync(path.join(directory,name)));
  const oracle=read('oracle.json'),candidate=read('candidate.json'),spec=read('candidate-spec.json'),runner=read('runner.json');
  assert.equal(sha(fs.readFileSync(spec.oracle.path)),spec.oracle.sha256,'fixed oracle drift');
  // Executed child, admission model, input scripts and comparator are unchanged.
  for(const name of ['domain-child.mjs','domain-matrix.mjs','domain-commit.mjs','domain-compare.mjs','domain-preregister.md','protocol.mjs','guard.mjs','fixture.mjs','offline.mjs'])assert.equal(sha(fs.readFileSync(path.join(spec.scriptRoot,name))),spec.scripts[name],'executed dependency drift '+name);
  assert.ok(runner.children.every(child=>child.childReaped&&!child.killed&&child.exit.code===0));
  assert.equal(sha(fs.readFileSync(spec.native.path)),spec.native.sha256);
  const summary=compare(oracle,candidate);summary.controls=checkControls(oracle,candidate);
  const report={status:'REPLAY_VALIDATED_NECESSARY_EVIDENCE_MISSING',summary,sourceChildExecuted:spec.scripts,recorderChangesAfterChildren:['extract controls','disable future tsx cache','shared budget and report bounds'],originalRecorderSha256:spec.scripts['domain-run.mjs'],currentRecorderSha256:sha(fs.readFileSync(path.join(spec.scriptRoot,'domain-run.mjs'))),checkerSha256:sha(fs.readFileSync(fileURLToPath(import.meta.url))),transportModelSha256:sha(fs.readFileSync(new URL('./domain-transport.mjs',import.meta.url))),raw:{oracle:{path:path.join(directory,'oracle.json'),sha256:sha(fs.readFileSync(path.join(directory,'oracle.json')))},candidate:{path:path.join(directory,'candidate.json'),sha256:sha(fs.readFileSync(path.join(directory,'candidate.json')))}},priorRunner:{path:path.join(directory,'runner.json'),sha256:sha(fs.readFileSync(path.join(directory,'runner.json')))},freshChildren:false};
  fs.writeFileSync(output,JSON.stringify(report,null,2)+'\n');process.stdout.write(JSON.stringify({status:report.status,rows:summary.matchedRows,controls:summary.controls.length})+'\n');
}
