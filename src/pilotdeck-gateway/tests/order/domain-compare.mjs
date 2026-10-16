import assert from 'node:assert/strict';
import {cases,missing,fixtureMethod} from './domain-matrix.mjs';
export function compare(oracle,candidate) {
  for(const report of [oracle,candidate]) {
    assert.equal(report.status,'RECORDED_NOT_ACCEPTED');assert.equal(report.cleanup.disposed,true);
    assert.equal(report.results.length,cases.length,'matrix count');assert.deepEqual(report.results.map(x=>x.id),cases.map(x=>x.id),'matrix identity');
    assert.deepEqual(report.missing,missing,'missing gates must remain visible');
    for(const [i,row]of report.results.entries()) {
      assert.equal(row.status,'RECORDED');assert.deepEqual(row.inputScript,cases[i],'fixed input script');
      const inputs=row.events.filter(e=>e.event==='input_data');assert.ok(inputs.length>0,'missing inputs');
      assert.equal(row.events.filter(e=>e.event==='parser_callback_return').length,inputs.length,'missing callback return');
      assert.ok(row.events.some(e=>e.event==='host_method_call'&&e.method==='describeServer'),'missing default host handshake anchor');
      for(const hex of row.wireHex)assert.match(hex,/^(?:[0-9a-f]{2})+$/);
      if(report.mode==='candidate') {
        assert.equal(row.events.some(e=>e.event==='host_method_call'&&e.method==='supportedAgents'),false,'local leaked to host');
        const envelopes=row.events.filter(e=>e.event==='host_ingress_epoch');assert.equal(envelopes.length,inputs.length,'missing ingress epoch');
        for(const event of envelopes)for(const item of event.items)if(item.kind==='NativeSlot')assert.deepEqual(Object.keys(item).sort(),['kind','readinessClass','token']);
        assert.equal(envelopes.flatMap(e=>e.items).some(e=>e.kind==='HostFrame'&&fixtureMethod(e.wireHex)==='supported_agents'),false,'local original leaked to host');
      }
    }
  }
  assert.equal(oracle.mode,'oracle');assert.equal(candidate.mode,'candidate');
  for(const [i,row]of candidate.results.entries())assert.deepEqual(row.wireHex,oracle.results[i].wireHex,'full wire mismatch '+row.id);
  const rejected=oracle.results.filter(x=>x.id.startsWith('D3-session_model_get'));
  assert.equal(rejected.length,4);
  for(const row of rejected)assert.ok(row.events.some(e=>e.event==='host_method_return'&&e.method==='sessionModelGet'&&e.promise),'real rejection path must return Promise');
  return {status:'NECESSARY_EVIDENCE_MISSING',immediateSubdomain:'SYNC_SUBDOMAIN_ONLY',matchedRows:cases.length,gates:['D1','D2','D3','D4','P1'],missing,productEnabled:false,issue65Closed:false};
}
